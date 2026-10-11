//! Trim execution for a [`TrimPlan`] (decision D26), fully native (decision D58).
//!
//! The planner in [`crate::plan`] decides what to cut. This module executes it:
//!
//! - [`TrimMode::Lossless`]: MP4 stream copy through [`clipcheck_mp4::remux`].
//! - [`TrimMode::Precise`] (Windows only): decode the source with Media
//!   Foundation, re-encode video with a Media Foundation H.264 encoder, copy
//!   AAC audio packets, and mux the result with [`clipcheck_mp4::segment`].
//!
//! The result is written to `<dst>.part` and renamed to `dst` only when it is
//! complete. `src` is never modified.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::time::{Duration, Instant};

use clipcheck_mp4::remux::{concat_remux, RemuxOptions, TrackSelect};
use clipcheck_mp4::Mp4Error;

use crate::plan::{TrimMode, TrimPlan};

/// Minimum spacing between two progress callbacks (the final 100% is not throttled).
#[cfg(windows)]
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Inputs for one trim run.
#[derive(Debug, Clone, Copy)]
pub struct TrimRequest<'a> {
    /// Source recording. Never written.
    pub src: &'a Path,
    /// Final output path. Must not exist when the run starts.
    pub dst: &'a Path,
    /// Plan from [`crate::plan_trim`].
    pub plan: &'a TrimPlan,
}

/// One progress report from a running trim.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrimProgress {
    /// Output time reached so far, in ms, on the output timeline.
    pub out_time_ms: i64,
    /// `out_time_ms / output_duration_ms`, clamped to `0.0..=100.0`.
    pub percent: f32,
}

/// Errors from [`run_trim`].
#[derive(Debug, Clone, PartialEq)]
pub enum TrimRunError {
    /// Filesystem, decode, encode or mux I/O failed. The message says what failed.
    Io(String),
    /// The cancel flag was set. The `.part` file and temp files were removed.
    Cancelled,
    /// The plan cannot be executed (for example a zero-length output).
    BadPlan(String),
}

impl fmt::Display for TrimRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrimRunError::Io(msg) => write!(f, "trim I/O error: {msg}"),
            TrimRunError::Cancelled => write!(f, "trim cancelled"),
            TrimRunError::BadPlan(msg) => write!(f, "bad trim plan: {msg}"),
        }
    }
}

impl std::error::Error for TrimRunError {}

fn mp4_err(e: Mp4Error) -> TrimRunError {
    TrimRunError::Io(format!("mp4: {e}"))
}

/// Progress percent for `out_ms` of `total_ms`, clamped to `0.0..=100.0`.
#[cfg_attr(not(windows), allow(dead_code))]
fn percent_of(out_ms: i64, total_ms: i64) -> f32 {
    if total_ms <= 0 {
        return 0.0;
    }
    let p = out_ms as f64 / total_ms as f64 * 100.0;
    p.clamp(0.0, 100.0) as f32
}

/// `<dst>.part`, next to the destination.
fn part_path(dst: &Path) -> PathBuf {
    let mut name = dst.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Runs the trim described by `req`.
///
/// Refuses plans with a non-positive output duration and an existing `dst`.
/// Writes `<dst>.part`, then renames it to `dst` on success. On failure or
/// cancellation the `.part` file and any temp files are deleted. `src` is
/// never modified. `cancel` is checked while work runs; a set flag aborts the
/// run. Progress is reported at most every 200 ms, plus a final 100% on success.
pub fn run_trim(
    req: &TrimRequest<'_>,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(TrimProgress),
) -> Result<(), TrimRunError> {
    let total = req.plan.output_duration_ms();
    if total <= 0 {
        return Err(TrimRunError::BadPlan(format!(
            "output duration {total} ms is not positive"
        )));
    }
    if req.dst.exists() {
        return Err(TrimRunError::Io("destination exists".to_string()));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(TrimRunError::Cancelled);
    }

    let tmp = part_path(req.dst);
    let _ = fs::remove_file(&tmp);

    let result = match req.plan.mode {
        TrimMode::Lossless => run_lossless(req.src, req.plan, &tmp),
        TrimMode::Precise => run_precise(req, &tmp, cancel, on_progress),
    };
    let result = result.and_then(|()| {
        if cancel.load(Ordering::SeqCst) {
            Err(TrimRunError::Cancelled)
        } else {
            Ok(())
        }
    });
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if req.dst.exists() {
        let _ = fs::remove_file(&tmp);
        return Err(TrimRunError::Io("destination exists".to_string()));
    }
    if let Err(e) = fs::rename(&tmp, req.dst) {
        let _ = fs::remove_file(&tmp);
        return Err(TrimRunError::Io(format!("rename output: {e}")));
    }
    on_progress(TrimProgress {
        out_time_ms: total,
        percent: 100.0,
    });
    Ok(())
}

/// Stream-copy trim: keyframe-snapped start, packet-accurate end, written to `tmp`.
fn run_lossless(src: &Path, plan: &TrimPlan, tmp: &Path) -> Result<(), TrimRunError> {
    let opts = RemuxOptions {
        start_secs: plan.actual_start_ms as f64 / 1000.0,
        end_secs: Some(plan.actual_end_ms as f64 / 1000.0),
        tracks: TrackSelect::All,
        faststart: true,
    };
    concat_remux(&[src.to_path_buf()], tmp, &opts).map_err(mp4_err)?;
    Ok(())
}

#[cfg(not(windows))]
fn run_precise(
    _req: &TrimRequest<'_>,
    _tmp: &Path,
    _cancel: &AtomicBool,
    _on_progress: &mut dyn FnMut(TrimProgress),
) -> Result<(), TrimRunError> {
    Err(TrimRunError::Io("precise trim needs Windows".to_string()))
}

/// Precise trim: decode, re-encode video from the keyframe, discard the head,
/// copy AAC audio in range, and mux. Writes `tmp` as a flat faststart MP4.
#[cfg(windows)]
fn run_precise(
    req: &TrimRequest<'_>,
    tmp: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(TrimProgress),
) -> Result<(), TrimRunError> {
    use clipcheck_mp4::reader::read_movie;

    let movie = read_movie(req.src)
        .map_err(|e| TrimRunError::Io(format!("precise trim needs an MP4 source: {e}")))?;
    let work_dir = work_dir_for(req.dst);
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir).map_err(|e| TrimRunError::Io(format!("create temp dir: {e}")))?;
    let result = precise_pipeline(req, &movie, tmp, &work_dir, cancel, on_progress);
    let _ = fs::remove_dir_all(&work_dir);
    result
}

/// `.<dst name>.trimtmp` next to the destination. Holds the one-segment mux output.
#[cfg(windows)]
fn work_dir_for(dst: &Path) -> PathBuf {
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".to_string());
    let parent = dst
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    parent.join(format!(".{name}.trimtmp"))
}

/// Progress reporter that emits at most once per [`PROGRESS_INTERVAL`].
#[cfg(windows)]
#[derive(Default)]
struct Throttle {
    last: Option<Instant>,
}

#[cfg(windows)]
impl Throttle {
    fn emit(&mut self, on_progress: &mut dyn FnMut(TrimProgress), out_ms: i64, total: i64) {
        let now = Instant::now();
        if let Some(t) = self.last {
            if now.duration_since(t) < PROGRESS_INTERVAL {
                return;
            }
        }
        self.last = Some(now);
        let out_time_ms = out_ms.max(0);
        on_progress(TrimProgress {
            out_time_ms,
            percent: percent_of(out_time_ms, total),
        });
    }
}

#[cfg(windows)]
fn dec_err(e: clipcheck_mfdec::DecError) -> TrimRunError {
    TrimRunError::Io(format!("decode: {e}"))
}

#[cfg(windows)]
fn enc_err(e: clipcheck_hwenc::EncError) -> TrimRunError {
    TrimRunError::Io(format!("encode: {e}"))
}

/// Reads the AAC samples of `track` whose decode time lies in `[start100, end100)`
/// (100 ns units), rebased so the first kept sample starts at 0.
#[cfg(windows)]
fn audio_packets(
    file: &mut std::fs::File,
    track: &clipcheck_mp4::reader::Track,
    start100: i64,
    end100: i64,
) -> Result<std::collections::VecDeque<clipcheck_mp4::segment::AudioPacket>, TrimRunError> {
    use std::io::{Read, Seek, SeekFrom};

    let mut out = std::collections::VecDeque::new();
    let ts = i64::from(track.timescale);
    if ts <= 0 {
        return Ok(out);
    }
    let rate = match track.kind {
        clipcheck_mp4::reader::TrackKind::Audio { sample_rate, .. } => sample_rate,
        _ => 0,
    };
    for s in &track.samples {
        let t = s.dts.saturating_mul(10_000_000) / ts;
        if !(start100..end100).contains(&t) {
            continue;
        }
        let mut data = vec![0u8; s.size as usize];
        file.seek(SeekFrom::Start(s.offset))
            .map_err(|e| TrimRunError::Io(format!("seek audio: {e}")))?;
        file.read_exact(&mut data)
            .map_err(|e| TrimRunError::Io(format!("read audio: {e}")))?;
        let samples = if i64::from(rate) == ts {
            s.duration
        } else {
            1024
        };
        out.push_back(clipcheck_mp4::segment::AudioPacket {
            data,
            pts: t - start100,
            samples,
        });
    }
    Ok(out)
}

/// Writes every audio packet with `pts <= until` to the muxer, per track.
#[cfg(windows)]
fn drain_audio(
    muxer: &mut clipcheck_mp4::segment::SegmentMuxer,
    queues: &mut [std::collections::VecDeque<clipcheck_mp4::segment::AudioPacket>],
    until: i64,
) -> Result<(), TrimRunError> {
    for (k, q) in queues.iter_mut().enumerate() {
        while let Some(front) = q.front() {
            if front.pts > until {
                break;
            }
            if let Some(p) = q.pop_front() {
                muxer.push_audio(k, p).map_err(mp4_err)?;
            }
        }
    }
    Ok(())
}

/// Muxes encoder output, interleaving audio that is due before each video packet.
#[cfg(windows)]
fn mux_video(
    muxer: &mut clipcheck_mp4::segment::SegmentMuxer,
    queues: &mut [std::collections::VecDeque<clipcheck_mp4::segment::AudioPacket>],
    packets: Vec<clipcheck_hwenc::EncodedPacket>,
) -> Result<(), TrimRunError> {
    use clipcheck_mp4::segment::VideoPacket;

    for p in packets {
        drain_audio(muxer, queues, p.pts)?;
        muxer
            .push_video(VideoPacket {
                data: p.data,
                pts: p.pts,
                dts: p.dts,
                keyframe: p.keyframe,
            })
            .map_err(mp4_err)?;
    }
    Ok(())
}

#[cfg(windows)]
fn precise_pipeline(
    req: &TrimRequest<'_>,
    movie: &clipcheck_mp4::reader::Movie,
    tmp: &Path,
    work_dir: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(TrimProgress),
) -> Result<(), TrimRunError> {
    use clipcheck_hwenc::{Codec, RateControl, VideoEncoder, VideoEncoderConfig};
    use clipcheck_mfdec::{OutFormat, VideoDecoder};
    use clipcheck_mp4::aac::aac_lc_asc;
    use clipcheck_mp4::annexb::VideoCodec;
    use clipcheck_mp4::fmp4::AudioTrackSpec;
    use clipcheck_mp4::reader::{Track, TrackKind};
    use clipcheck_mp4::segment::{SegmentConfig, SegmentMuxer};
    use std::collections::VecDeque;

    let plan = req.plan;

    // Video track: codec, size, frame rate and a bitrate estimate from the source.
    let video: &Track = movie
        .tracks
        .iter()
        .find(|t| matches!(t.kind, TrackKind::Video { .. }))
        .ok_or_else(|| TrimRunError::Io("source has no video track".to_string()))?;
    let TrackKind::Video {
        codec,
        width,
        height,
    } = &video.kind
    else {
        return Err(TrimRunError::Io("source has no video track".to_string()));
    };
    if codec != "h264" && codec != "hevc" {
        return Err(TrimRunError::Io(format!(
            "precise trim does not support video codec {codec}"
        )));
    }
    let (width, height) = (*width, *height);
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err(TrimRunError::Io(format!(
            "precise trim needs even frame size, source is {width}x{height}"
        )));
    }
    if video.timescale == 0 || video.samples.is_empty() {
        return Err(TrimRunError::Io("source video has no samples".to_string()));
    }
    let ticks: i64 = video.samples.iter().map(|s| i64::from(s.duration)).sum();
    let video_secs = ticks as f64 / f64::from(video.timescale);
    if video_secs <= 0.0 {
        return Err(TrimRunError::Io("source video has no duration".to_string()));
    }
    let fps = (video.samples.len() as f64 / video_secs).round().max(1.0) as u32;
    let bytes: u64 = video.samples.iter().map(|s| u64::from(s.size)).sum();
    let kbps = ((bytes as f64 * 8.0 / video_secs / 1000.0).round() as u32).clamp(2_000, 50_000);

    // AAC audio tracks are copied; other audio codecs are dropped (see report).
    let mut specs: Vec<AudioTrackSpec> = Vec::new();
    let mut audio_tracks: Vec<&Track> = Vec::new();
    for t in &movie.tracks {
        let TrackKind::Audio {
            codec,
            channels,
            sample_rate,
        } = &t.kind
        else {
            continue;
        };
        if codec != "aac" {
            continue;
        }
        let asc = u8::try_from(*channels)
            .ok()
            .and_then(|c| aac_lc_asc(*sample_rate, c))
            .ok_or_else(|| {
                TrimRunError::Io(format!(
                    "unsupported AAC layout: {sample_rate} Hz, {channels} channels"
                ))
            })?;
        specs.push(AudioTrackSpec {
            sample_rate: *sample_rate,
            channels: *channels,
            asc,
            bitrate: 160_000,
            title: t.title.clone().unwrap_or_default(),
        });
        audio_tracks.push(t);
    }

    // Software H.264 encoder: needs no GPU device.
    clipcheck_hwenc::mf_startup().map_err(enc_err)?;
    let list = clipcheck_hwenc::enumerate(Codec::H264).map_err(enc_err)?;
    let desc = list
        .iter()
        .find(|d| !d.hardware)
        .ok_or_else(|| TrimRunError::Io("no software H.264 encoder found".to_string()))?;
    let cfg = VideoEncoderConfig {
        codec: Codec::H264,
        width,
        height,
        fps,
        rate: RateControl::Vbr {
            kbps,
            peak_kbps: kbps * 3 / 2,
        },
        gop_frames: fps * 2,
        quality_vs_speed: 70,
        low_latency: false,
    };
    let mut enc: Box<dyn VideoEncoder> =
        clipcheck_hwenc::open(desc, None, &cfg).map_err(enc_err)?;

    let mut muxer = SegmentMuxer::new(SegmentConfig {
        out_dir: work_dir.to_path_buf(),
        segment_secs: 1e9,
        fps,
        codec: VideoCodec::H264,
        width,
        height,
        audio: specs,
    })
    .map_err(mp4_err)?;

    // Audio in the requested range, rebased to the output timeline.
    let start100 = plan.requested_start_ms * 10_000;
    let end100 = plan.actual_end_ms * 10_000;
    let mut src_file =
        std::fs::File::open(req.src).map_err(|e| TrimRunError::Io(format!("open source: {e}")))?;
    let mut queues: Vec<VecDeque<_>> = Vec::with_capacity(audio_tracks.len());
    for t in &audio_tracks {
        queues.push(audio_packets(&mut src_file, t, start100, end100)?);
    }

    // Decode from the re-encode keyframe; encode frames in [requested start, actual end).
    let mut dec = VideoDecoder::open(req.src, OutFormat::Nv12).map_err(dec_err)?;
    let from_ms = plan.reencode_from_keyframe_ms.unwrap_or(0);
    dec.seek(from_ms as f64 / 1000.0).map_err(dec_err)?;

    let total = plan.output_duration_ms();
    let mut throttle = Throttle::default();
    let mut frames_in: u64 = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(TrimRunError::Cancelled);
        }
        let Some(frame) = dec.next_frame().map_err(dec_err)? else {
            break;
        };
        let t_ms = frame.pts / 10_000;
        if t_ms < plan.requested_start_ms - 1 {
            continue;
        }
        if t_ms >= plan.actual_end_ms {
            break;
        }
        let out_pts = (frame.pts - start100).max(0);
        let packets = enc
            .encode(
                clipcheck_hwenc::InputFrame::Nv12 {
                    data: &frame.data,
                    width: frame.width,
                    height: frame.height,
                },
                out_pts,
                frames_in == 0,
            )
            .map_err(enc_err)?;
        frames_in += 1;
        mux_video(&mut muxer, &mut queues, packets)?;
        throttle.emit(on_progress, out_pts / 10_000, total);
    }
    if frames_in == 0 {
        return Err(TrimRunError::Io(
            "no video frames in the trim range".to_string(),
        ));
    }

    let rest = enc.flush().map_err(enc_err)?;
    mux_video(&mut muxer, &mut queues, rest)?;
    drain_audio(&mut muxer, &mut queues, i64::MAX)?;
    let segments = muxer.finish().map_err(mp4_err)?;
    if segments.is_empty() {
        return Err(TrimRunError::Io("muxer produced no segment".to_string()));
    }

    // One segment -> flat faststart MP4 at `tmp`.
    concat_remux(&segments, tmp, &RemuxOptions::default()).map_err(mp4_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lossless_plan() -> TrimPlan {
        TrimPlan {
            mode: TrimMode::Lossless,
            requested_start_ms: 12_000,
            requested_end_ms: 17_000,
            actual_start_ms: 10_000,
            actual_end_ms: 17_000,
            snap_delta_ms: 2_000,
            reencode_from_keyframe_ms: None,
            discard_head_ms: 0,
        }
    }

    fn req<'a>(plan: &'a TrimPlan) -> TrimRequest<'a> {
        TrimRequest {
            src: Path::new("in.mp4"),
            dst: Path::new("out.mp4"),
            plan,
        }
    }

    #[test]
    fn percent_is_clamped() {
        assert_eq!(percent_of(-5, 1_000), 0.0);
        assert_eq!(percent_of(0, 1_000), 0.0);
        assert_eq!(percent_of(250, 1_000), 25.0);
        assert_eq!(percent_of(1_000, 1_000), 100.0);
        assert_eq!(percent_of(2_000, 1_000), 100.0);
        assert_eq!(percent_of(5, 0), 0.0);
    }

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("a/b.mp4")),
            PathBuf::from("a/b.mp4.part")
        );
    }

    #[test]
    fn stderr_tail_keeps_last_bytes() {
        let big = vec![b'x'; 2148];
        assert_eq!(clipcheck_util::proc::stderr_tail(&big, 2048).len(), 2048);
        assert_eq!(
            clipcheck_util::proc::stderr_tail(b"  oops \n", 2048),
            "oops"
        );
    }

    #[test]
    fn run_rejects_zero_duration_plan() {
        let mut plan = lossless_plan();
        plan.actual_start_ms = plan.actual_end_ms;
        let r = req(&plan);
        let cancel = AtomicBool::new(false);
        let res = run_trim(&r, &cancel, &mut |_| {});
        assert!(matches!(res, Err(TrimRunError::BadPlan(_))));
    }

    #[test]
    fn run_refuses_existing_destination() {
        let dir = std::env::temp_dir().join(format!("clipedit-run-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("exists.mp4");
        fs::write(&dst, b"x").unwrap();
        let plan = lossless_plan();
        let r = TrimRequest {
            src: Path::new("in.mp4"),
            dst: &dst,
            plan: &plan,
        };
        let cancel = AtomicBool::new(false);
        let res = run_trim(&r, &cancel, &mut |_| {});
        assert_eq!(res, Err(TrimRunError::Io("destination exists".to_string())));
        assert!(!part_path(&dst).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_cancelled_before_start() {
        let dir = std::env::temp_dir().join(format!("clipedit-cancel-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("never.mp4");
        let plan = lossless_plan();
        let r = TrimRequest {
            src: Path::new("in.mp4"),
            dst: &dst,
            plan: &plan,
        };
        let cancel = AtomicBool::new(true);
        let res = run_trim(&r, &cancel, &mut |_| {});
        assert_eq!(res, Err(TrimRunError::Cancelled));
        assert!(!part_path(&dst).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Video duration of a movie in seconds (first video track).
    #[cfg(windows)]
    fn video_secs(path: &Path) -> f64 {
        use clipcheck_mp4::reader::{read_movie, TrackKind};
        let m = read_movie(path).unwrap();
        let t = m
            .tracks
            .iter()
            .find(|t| matches!(t.kind, TrackKind::Video { .. }))
            .unwrap();
        let ticks: i64 = t.samples.iter().map(|s| i64::from(s.duration)).sum();
        ticks as f64 / f64::from(t.timescale)
    }

    /// Audio duration of the first AAC track in seconds.
    #[cfg(windows)]
    fn audio_secs(path: &Path) -> f64 {
        use clipcheck_mp4::reader::{read_movie, TrackKind};
        let m = read_movie(path).unwrap();
        let t = m
            .tracks
            .iter()
            .find(|t| matches!(t.kind, TrackKind::Audio { .. }))
            .unwrap();
        let ticks: i64 = t.samples.iter().map(|s| i64::from(s.duration)).sum();
        ticks as f64 / f64::from(t.timescale)
    }

    /// Smoke test on a 6 s 640x360 30 fps H.264 + AAC fixture generated by clipcheck-testkit.
    #[cfg(windows)]
    #[test]
    fn smoke_lossless_and_precise_on_fixture() {
        use crate::plan::{plan_trim, TrimInput};

        if !clipcheck_testkit::available() {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        }
        let dir = std::env::temp_dir().join(format!("clipedit-smoke-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut spec = clipcheck_testkit::MediaSpec::h264_aac(6.0).with_video(640, 360, 30);
        if let Some(v) = spec.video.as_mut() {
            v.gop_frames = 60; // keyframe every 2 s
        }
        // One segment, so the source audio has no segment-seam timing gaps.
        let src = clipcheck_testkit::write_segments(&dir, &spec, 6.0)
            .unwrap()
            .remove(0);

        let movie = clipcheck_mp4::reader::read_movie(&src).unwrap();
        let vt = movie
            .tracks
            .iter()
            .find(|t| matches!(t.kind, clipcheck_mp4::reader::TrackKind::Video { .. }))
            .unwrap();
        let keyframes: Vec<i64> = vt
            .samples
            .iter()
            .filter(|s| s.keyframe)
            .map(|s| s.dts * 1000 / i64::from(vt.timescale))
            .collect();
        let duration_ms = (movie.duration_secs * 1000.0).round() as i64;

        let cancel = AtomicBool::new(false);
        for (mode, name) in [
            (TrimMode::Lossless, "lossless"),
            (TrimMode::Precise, "precise"),
        ] {
            let plan = plan_trim(&TrimInput {
                keyframes_ms: &keyframes,
                duration_ms,
                start_ms: 1_500,
                end_ms: 4_000,
                mode,
            })
            .unwrap();
            let dst = dir.join(format!("{name}.mp4"));
            let r = TrimRequest {
                src: &src,
                dst: &dst,
                plan: &plan,
            };
            let mut last = None;
            run_trim(&r, &cancel, &mut |p| last = Some(p)).unwrap();
            assert_eq!(last.map(|p| p.percent), Some(100.0));
            assert!(!part_path(&dst).exists());
            let vs = video_secs(&dst);
            let asx = audio_secs(&dst);
            eprintln!(
                "SMOKE {name}: plan {}..{} ms, output video {vs:.3} s, audio {asx:.3} s",
                plan.actual_start_ms, plan.actual_end_ms
            );
            let expected = plan.output_duration_ms() as f64 / 1000.0;
            if mode == TrimMode::Precise {
                assert!((vs - 2.5).abs() < 0.1, "precise video {vs}");
            } else {
                assert!((vs - expected).abs() < 0.3, "lossless video {vs}");
            }
            assert!(
                (asx - 2.5).abs() < 0.3 || mode == TrimMode::Lossless,
                "audio {asx}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
