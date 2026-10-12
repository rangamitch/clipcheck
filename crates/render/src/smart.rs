//! Smart lossless export (no re-encode). Owner: worker `render-smart`.
//!
//! The eligibility check and the stream-copy path both read the same analysis of the render plan
//! ([`analyse`]), so the reasons shown to the user and the work done at export time cannot drift.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clipcheck_mp4::reader::{read_movie, Track as SourceTrack, TrackKind as SourceKind};
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
use clipcheck_mp4::Mp4Error;
use clipcheck_timeline::export::{
    AudioLayout, ExportKind, ExportRange, ExportSettings, Loudness, VideoCodec,
};
use clipcheck_timeline::model::{ClipKind, Project, TrackKind, Transform};
use clipcheck_timeline::plan::{derive_plan, RenderPlan};
use clipcheck_timeline::time::Tick;

use crate::error::RenderError;
use crate::job::{JobControl, Progress};

/// Result of the eligibility check (design doc section 8.6).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Eligibility {
    /// Whether the lossless path can be used.
    pub eligible: bool,
    /// First blocking reason when not eligible ("Clip 2 has a different resolution").
    pub reason: Option<String>,
    /// Notes about keyframe snapping ("Start of clip 2 snaps back by 1.3 s").
    pub snap_notes: Vec<String>,
}

/// Reason used when the export range holds nothing.
const NOTHING: &str = "Nothing to export";
/// Frame-rate tolerance, frames per second.
const FPS_TOLERANCE: f64 = 0.01;
/// Times closer than this to a keyframe count as on it, seconds.
const TIME_EPS: f64 = 1e-6;
/// Snap-back shorter than this is not reported, seconds.
const SNAP_EPS: f64 = 1e-3;
/// File extensions the stream-copy path accepts (lower case).
const MP4_EXTS: [&str; 4] = ["mp4", "m4v", "mov", "m4a"];

/// Header facts of the video track of one source file.
#[derive(Debug, Clone, PartialEq)]
struct VideoInfo {
    /// Reader codec name ("h264", "hevc", "av1" or a fourcc).
    codec: String,
    /// Width in pixels.
    width: u32,
    /// Height in pixels.
    height: u32,
    /// Frames per second (sample count over duration), 0 when unknown.
    fps: f64,
    /// Presentation times of sync samples in seconds, ascending.
    keyframes: Vec<f64>,
}

/// Header facts of one source file, read once per media id.
#[derive(Debug, Clone, PartialEq)]
struct Probe {
    /// The video track, when the file has one.
    video: Option<VideoInfo>,
    /// Codec of each audio track, in file order.
    audio: Vec<String>,
}

/// The render plan over the export range plus the probed sources it uses.
struct Analysis {
    plan: RenderPlan,
    probes: HashMap<String, Probe>,
}

/// One run of consecutive plan segments that show the same video clip; one source cut.
#[derive(Debug, Clone, PartialEq)]
struct Piece {
    /// Clip id.
    clip_id: String,
    /// Media id.
    media_id: String,
    /// Source file path.
    path: PathBuf,
    /// Source start (presentation time in the file).
    src_start: Tick,
    /// Length of the cut (equal to the timeline length, speed is 1.0).
    src_len: Tick,
}

/// Checks whether `project` over the export range can be stitched without re-encoding: every
/// video segment is a plain cut (speed 1.0, no reverse, no transform change, opacity 1, no
/// transition, no title/image/solid layers, single video layer at a time), all used media share
/// codec, resolution and fps, canvas equals the source size, output size equals the canvas, all
/// audio clips have 0 dB gain, no fades/envelope/pan, no muting except whole-track selection, and
/// the sources are MP4/MOV files readable by `clipcheck_mp4::reader`.
pub fn check_eligibility(project: &Project, settings: &ExportSettings) -> Eligibility {
    match analyse(project, settings) {
        Err(reason) => Eligibility {
            eligible: false,
            reason: Some(reason),
            snap_notes: Vec::new(),
        },
        Ok(analysis) => {
            let mut snap_notes = Vec::new();
            for (i, piece) in build_pieces(project, &analysis.plan).iter().enumerate() {
                let Some(info) = analysis
                    .probes
                    .get(&piece.media_id)
                    .and_then(|p| p.video.as_ref())
                else {
                    continue;
                };
                let start = piece.src_start.as_secs_f64();
                let snap = start - keyframe_at_or_before(&info.keyframes, start);
                if snap > SNAP_EPS {
                    snap_notes.push(format!(
                        "Start of clip {} snaps back by {:.1} s",
                        i + 1,
                        snap
                    ));
                }
            }
            Eligibility {
                eligible: true,
                reason: None,
                snap_notes,
            }
        }
    }
}

/// Stitches the ranges with `clipcheck_mp4::remux::concat_remux` into `<dst>.part`, renames to
/// `dst`. Video starts snap back to the previous keyframe. Returns bytes written.
///
/// One source cut is remuxed per clip to a `<dst>.pieceN.part` temp file (only when there is
/// more than one cut), then the temp files are concatenated. Every temp file is removed on exit.
pub fn run_lossless(
    project: &Project,
    settings: &ExportSettings,
    dst: &Path,
    ctl: &JobControl,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<u64, RenderError> {
    let analysis = analyse(project, settings).map_err(RenderError::Invalid)?;
    let pieces = build_pieces(project, &analysis.plan);
    if pieces.is_empty() {
        return Err(RenderError::Invalid(NOTHING.to_string()));
    }
    let total = analysis
        .plan
        .end
        .saturating_sub(analysis.plan.start)
        .as_secs_f64();
    let temps: Vec<PathBuf> = if pieces.len() > 1 {
        (1..=pieces.len())
            .map(|i| suffixed(dst, &format!(".piece{i}.part")))
            .collect()
    } else {
        Vec::new()
    };
    // Nothing appears under `dst` until the whole output is written: every remux writes to `part`
    // (concat_remux itself goes through `<part>.part` and renames), then `part` is renamed to `dst`.
    let part = suffixed(dst, ".part");

    let result = stitch(&pieces, &temps, &part, total, ctl, on_progress).and_then(|()| {
        check_cancel(ctl)?;
        std::fs::rename(&part, dst).map_err(|e| RenderError::Output(e.to_string()))
    });
    for t in &temps {
        let _ = std::fs::remove_file(t);
    }
    match result {
        Ok(()) => std::fs::metadata(dst)
            .map(|m| m.len())
            .map_err(|e| RenderError::Output(e.to_string())),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Runs every eligibility rule in the documented order and returns the first reason that blocks
/// the lossless path. On success, returns the plan and the probed sources.
fn analyse(project: &Project, settings: &ExportSettings) -> Result<Analysis, String> {
    let range = export_range(project, settings).ok_or_else(|| NOTHING.to_string())?;
    let plan = derive_plan(project, Some(range));
    // derive_plan skips disabled, muted and missing clips but still emits gap segments, so a range
    // with no usable layers at all is nothing to export rather than a gap.
    if plan.segments.is_empty()
        || plan
            .segments
            .iter()
            .all(|s| s.video.is_empty() && s.audio.is_empty())
    {
        return Err(NOTHING.to_string());
    }
    if !matches!(settings.kind, ExportKind::Video) {
        return Err("Only video exports can be lossless".to_string());
    }
    let segs = &plan.segments;

    if segs.iter().any(|s| s.video.len() > 1) {
        return Err("Overlapping video layers".to_string());
    }
    if segs
        .iter()
        .flat_map(|s| s.video.iter())
        .any(|v| v.kind != ClipKind::Video)
    {
        return Err("Titles, images and solids need re-encoding".to_string());
    }
    if segs.iter().any(|s| {
        s.video.iter().any(|v| v.speed != 1.0 || v.reverse)
            || s.audio.iter().any(|a| a.speed != 1.0 || a.reverse)
    }) {
        return Err("Speed changes need re-encoding".to_string());
    }
    if segs
        .iter()
        .flat_map(|s| s.video.iter())
        .any(|v| v.opacity != 1.0 || v.transition.is_some() || v.transform != Transform::default())
    {
        return Err("Clip transforms need re-encoding".to_string());
    }
    if segs.iter().any(|s| s.video.is_empty()) {
        return Err("Gaps need re-encoding".to_string());
    }

    // Probe every used file (headers only), in plan order. A file that cannot be read stops here.
    let mut probes: HashMap<String, Probe> = HashMap::new();
    for seg in segs {
        for v in &seg.video {
            let Some(mid) = v.media_id.as_deref() else {
                return Err("Gaps need re-encoding".to_string());
            };
            load_probe(project, &mut probes, mid, true)?;
        }
        for a in &seg.audio {
            load_probe(project, &mut probes, &a.media_id, false)?;
        }
    }

    // Clips in order of first use, with their media. "Clip n" messages use this numbering.
    let clips = clip_order(&plan);
    let (_, first_mid) = clips
        .first()
        .ok_or_else(|| "Gaps need re-encoding".to_string())?;
    let first = video_of(&probes, first_mid)?;

    for (i, (_, mid)) in clips.iter().enumerate() {
        let n = i + 1;
        let info = video_of(&probes, mid)?;
        if info.width != first.width || info.height != first.height {
            return Err(format!("Clip {n} has a different resolution"));
        }
        if !info.codec.eq_ignore_ascii_case(&first.codec) {
            return Err(format!("Clip {n} has a different codec"));
        }
        if (info.fps - first.fps).abs() > FPS_TOLERANCE {
            return Err(format!("Clip {n} has a different frame rate"));
        }
    }

    let canvas = &project.canvas;
    if canvas.width != first.width
        || canvas.height != first.height
        || settings.width != first.width
        || settings.height != first.height
    {
        return Err("Output size differs from the source".to_string());
    }
    if !codec_matches(settings.codec, &first.codec) {
        return Err("Output codec differs from the source".to_string());
    }
    let want_fps = settings.fps.unwrap_or(canvas.fps).as_f64();
    if (want_fps - first.fps).abs() > FPS_TOLERANCE {
        return Err("Frame rate differs from the source".to_string());
    }

    // Audio: no gain, fades, envelope, pan, mono or track fader; audio must line up with the video
    // of the same segment.
    for seg in segs {
        for a in &seg.audio {
            let c = &a.audio;
            if c.gain_db != 0.0
                || c.pan != 0.0
                || c.fade_in != Tick::ZERO
                || c.fade_out != Tick::ZERO
                || !c.envelope.is_empty()
                || c.mono
                || a.track_db != 0.0
            {
                return Err("Audio changes need re-encoding".to_string());
            }
            let aligned = seg.video.first().is_some_and(|v| {
                v.media_id.as_deref() == Some(a.media_id.as_str())
                    && v.clip_start == a.clip_start
                    && v.src_time == a.src_time
            });
            if !aligned {
                return Err("Audio changes need re-encoding".to_string());
            }
        }
    }

    // Stream copy keeps every audio stream of a source, so each stream of the shown clip must be
    // audible in every segment (deleted, detached, muted or disabled audio needs re-encoding).
    for seg in segs {
        let Some(mid) = seg.video.first().and_then(|v| v.media_id.as_deref()) else {
            return Err("Gaps need re-encoding".to_string());
        };
        let streams = probes.get(mid).map_or(0, |p| p.audio.len());
        let audible: Vec<u32> = seg
            .audio
            .iter()
            .filter(|a| a.media_id == mid)
            .map(|a| a.source_track)
            .collect();
        let all_audible = (0..streams).all(|k| {
            let k = u32::try_from(k).unwrap_or(u32::MAX);
            audible.contains(&k)
        });
        if !all_audible {
            return Err("Audio changes need re-encoding".to_string());
        }
    }
    // Mix sums the streams into one track, which stream copy cannot do.
    if matches!(settings.audio.layout, AudioLayout::Mix)
        && clips
            .iter()
            .any(|(_, mid)| probes.get(mid).is_some_and(|p| p.audio.len() > 1))
    {
        return Err("Mixing several audio tracks needs re-encoding".to_string());
    }

    if matches!(settings.audio.layout, AudioLayout::Separate) {
        let timeline_audio = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .count();
        let source_audio = probes.get(first_mid).map_or(0, |p| p.audio.len());
        if timeline_audio != source_audio {
            return Err("Separate audio tracks need re-encoding".to_string());
        }
    }
    if !matches!(settings.audio.loudness, Loudness::Off) {
        return Err("Loudness normalisation needs re-encoding".to_string());
    }
    let first_audio = probes
        .get(first_mid)
        .map(|p| p.audio.clone())
        .unwrap_or_default();
    for (i, (_, mid)) in clips.iter().enumerate() {
        let audio = probes.get(mid).map(|p| &p.audio);
        if audio != Some(&first_audio) {
            return Err(format!("Clip {} has a different audio track layout", i + 1));
        }
    }

    Ok(Analysis { plan, probes })
}

/// The export range on the timeline, or None when it is empty.
fn export_range(project: &Project, settings: &ExportSettings) -> Option<(Tick, Tick)> {
    let whole = (Tick::ZERO, project.timeline_end());
    let range = match &settings.range {
        ExportRange::Entire => whole,
        ExportRange::WorkArea => match &project.work_area {
            Some(w) => (w.start, w.end),
            None => whole,
        },
        ExportRange::SelectedClips { ids } => {
            let mut span: Option<(Tick, Tick)> = None;
            for c in project
                .clips
                .iter()
                .filter(|c| ids.iter().any(|id| id == &c.id))
            {
                span = Some(match span {
                    None => (c.start, c.end()),
                    Some((a, b)) => (a.min(c.start), b.max(c.end())),
                });
            }
            span?
        }
    };
    (range.0 < range.1).then_some(range)
}

/// Distinct video clips in plan order, each with its media id.
fn clip_order(plan: &RenderPlan) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for seg in &plan.segments {
        for v in &seg.video {
            if out.iter().any(|(id, _)| id == &v.clip_id) {
                continue;
            }
            let mid = v.media_id.clone().unwrap_or_default();
            out.push((v.clip_id.clone(), mid));
        }
    }
    out
}

/// Probes `media_id` into `probes` when it is not there yet. Fails with "<name> is not an MP4
/// file" when the file cannot be used (or has no video track when `need_video`).
fn load_probe(
    project: &Project,
    probes: &mut HashMap<String, Probe>,
    media_id: &str,
    need_video: bool,
) -> Result<(), String> {
    if !probes.contains_key(media_id) {
        let media = project.media_ref(media_id);
        let name = media.map_or_else(|| media_id.to_string(), |m| m.name.clone());
        let path = media.map_or_else(String::new, |m| m.path.clone());
        let probe = probe_file(&path).ok_or_else(|| format!("{name} is not an MP4 file"))?;
        probes.insert(media_id.to_string(), probe);
    }
    let has_video = probes.get(media_id).is_some_and(|p| p.video.is_some());
    if need_video && !has_video {
        let name = project
            .media_ref(media_id)
            .map_or_else(|| media_id.to_string(), |m| m.name.clone());
        return Err(format!("{name} is not an MP4 file"));
    }
    Ok(())
}

/// Reads the header facts of one file. None when the extension is not accepted or the file
/// cannot be parsed. Never panics on bad input.
fn probe_file(path: &str) -> Option<Probe> {
    let p = Path::new(path);
    let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
    if !MP4_EXTS.contains(&ext.as_str()) {
        return None;
    }
    let movie = read_movie(p).ok()?;
    let mut video = None;
    let mut audio = Vec::new();
    for t in &movie.tracks {
        match &t.kind {
            SourceKind::Video {
                codec,
                width,
                height,
            } if video.is_none() => {
                video = Some(video_info(t, codec, *width, *height)?);
            }
            SourceKind::Audio { codec, .. } => audio.push(codec.clone()),
            _ => {}
        }
    }
    Some(Probe { video, audio })
}

/// Video facts of one track; None when the track has no usable timescale.
fn video_info(t: &SourceTrack, codec: &str, width: u32, height: u32) -> Option<VideoInfo> {
    if t.timescale == 0 {
        return None;
    }
    let ts = f64::from(t.timescale);
    let secs = t.duration_secs();
    let fps = if secs > 0.0 {
        t.samples.len() as f64 / secs
    } else {
        0.0
    };
    let mut keyframes: Vec<f64> = t
        .samples
        .iter()
        .filter(|s| s.keyframe)
        .map(|s| {
            let dts = s.dts.saturating_sub(t.edit_media_time) as f64 / ts;
            dts + f64::from(s.cts_offset) / ts
        })
        .collect();
    keyframes.sort_by(f64::total_cmp);
    Some(VideoInfo {
        codec: codec.to_string(),
        width,
        height,
        fps,
        keyframes,
    })
}

/// The video facts of a probed media id.
fn video_of<'a>(
    probes: &'a HashMap<String, Probe>,
    media_id: &str,
) -> Result<&'a VideoInfo, String> {
    probes
        .get(media_id)
        .and_then(|p| p.video.as_ref())
        .ok_or_else(|| "Source is not readable".to_string())
}

/// Whether the reader codec name is the codec the settings ask for.
fn codec_matches(codec: VideoCodec, source: &str) -> bool {
    let names: &[&str] = match codec {
        VideoCodec::H264 => &["h264", "avc1"],
        VideoCodec::Hevc => &["hevc", "hvc1", "hev1"],
        VideoCodec::Av1 => &["av1", "av01"],
    };
    names.iter().any(|n| n.eq_ignore_ascii_case(source))
}

/// The last keyframe at or before `t`, or the first keyframe when none is before `t`, or `t`.
fn keyframe_at_or_before(keyframes: &[f64], t: f64) -> f64 {
    let n = keyframes.partition_point(|&k| k <= t + TIME_EPS);
    match n.checked_sub(1).and_then(|i| keyframes.get(i)) {
        Some(&k) => k,
        None => keyframes.first().copied().unwrap_or(t),
    }
}

/// Source cuts in timeline order, one per clip occurrence.
fn build_pieces(project: &Project, plan: &RenderPlan) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    for seg in &plan.segments {
        let Some(v) = seg.video.first() else {
            continue;
        };
        let len = seg.end.saturating_sub(seg.start);
        if let Some(last) = pieces.last_mut() {
            if last.clip_id == v.clip_id {
                last.src_len = last.src_len.saturating_add(len);
                continue;
            }
        }
        let Some(mid) = v.media_id.clone() else {
            continue;
        };
        let path = project
            .media_ref(&mid)
            .map_or_else(PathBuf::new, |m| PathBuf::from(&m.path));
        pieces.push(Piece {
            clip_id: v.clip_id.clone(),
            media_id: mid,
            path,
            src_start: v.src_time,
            src_len: len,
        });
    }
    pieces
}

/// Remux options for one source cut.
fn piece_opts(p: &Piece) -> RemuxOptions {
    RemuxOptions {
        start_secs: p.src_start.as_secs_f64(),
        end_secs: Some(p.src_start.saturating_add(p.src_len).as_secs_f64()),
        ..RemuxOptions::default()
    }
}

/// Writes the cuts. With one cut, remuxes straight to `out`; with several, remuxes each to its
/// temp file (`temps`, one per cut) and concatenates them into `out`. `out` is a `.part` path, so
/// the caller renames it to the final name.
fn stitch(
    pieces: &[Piece],
    temps: &[PathBuf],
    out: &Path,
    total: f64,
    ctl: &JobControl,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), RenderError> {
    check_cancel(ctl)?;
    report(on_progress, ctl, 0.0, total);
    if let [only] = pieces {
        let opts = piece_opts(only);
        concat_remux(std::slice::from_ref(&only.path), out, &opts).map_err(map_mp4)?;
        report(on_progress, ctl, total, total);
        return Ok(());
    }
    let mut done = 0.0;
    for (piece, tmp) in pieces.iter().zip(temps) {
        check_cancel(ctl)?;
        let opts = piece_opts(piece);
        concat_remux(std::slice::from_ref(&piece.path), tmp, &opts).map_err(map_mp4)?;
        done += piece.src_len.as_secs_f64();
        report(on_progress, ctl, done, total);
    }
    check_cancel(ctl)?;
    concat_remux(temps, out, &RemuxOptions::default()).map_err(map_mp4)?;
    report(on_progress, ctl, total, total);
    Ok(())
}

/// Fails with [`RenderError::Cancelled`] when cancellation was requested.
fn check_cancel(ctl: &JobControl) -> Result<(), RenderError> {
    if ctl.is_cancelled() {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

/// Sends one progress report.
fn report(on_progress: &mut dyn FnMut(Progress), ctl: &JobControl, done: f64, total: f64) {
    let percent = if total > 0.0 {
        (done / total * 100.0).clamp(0.0, 100.0) as f32
    } else {
        100.0
    };
    on_progress(Progress {
        out_secs: done,
        total_secs: total,
        percent,
        frames: 0,
        paused: ctl.is_paused(),
    });
}

/// Maps a remux error: I/O is an output failure, anything else a source failure.
fn map_mp4(e: Mp4Error) -> RenderError {
    match e {
        Mp4Error::Io(io) => RenderError::Output(io.to_string()),
        other => RenderError::Source(other.to_string()),
    }
}

/// `path` with `suffix` appended to its file name (for example `.part`).
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}
