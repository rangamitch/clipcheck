//! Windows export engine: Media Foundation decode, CPU reference compositor, software H.264 or
//! HEVC encode, AAC audio, MP4 mux. Owner: worker `render-engine`.
//!
//! Adapted from `crates/clipedit/src/run.rs` (`precise_pipeline`): encoder open, frame push,
//! packet drain, audio interleave, segment mux and flat faststart output.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use clipcheck_aacenc::AacEncoder;
use clipcheck_hwenc::{
    Codec, EncodedPacket, EncoderDescriptor, InputFrame, RateControl, VideoEncoder,
    VideoEncoderConfig,
};
use clipcheck_mp4::annexb::VideoCodec as Mp4VideoCodec;
use clipcheck_mp4::fmp4::AudioTrackSpec;
use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
use clipcheck_mp4::segment::{AudioPacket, SegmentConfig, SegmentMuxer, VideoPacket};
use clipcheck_timeline::export::{
    quality_to_kbps, solve_target_bitrate, validate_settings, AudioLayout, ExportKind, ExportRange,
    ExportSettings, Loudness, QualityMode, VideoCodec,
};
use clipcheck_timeline::model::{Project, TrackKind};
use clipcheck_timeline::plan::{derive_plan, RenderPlan};
use clipcheck_timeline::time::{Rate, Tick};

use crate::dsp::normalize_gain_db;
use crate::error::RenderError;
use crate::frame::Frame;
use crate::gpuenc::GpuUploader;
use crate::job::{JobControl, Progress};
use crate::nv12;
use crate::pipeline::{self, PipelineConfig, Sink};
use crate::reference;
use crate::smart;
use crate::sources::{MfAudioProvider, MfVideoProvider};
use crate::still;
use crate::titles::GdiTitleProvider;

/// Inputs for one export.
pub struct ExportRequest<'a> {
    /// Project snapshot (not modified).
    pub project: &'a Project,
    /// Settings chosen in the dialog.
    pub settings: &'a ExportSettings,
    /// Final output path (a `.mp4`, `.m4a`, `.png` or `.jpg` that does not exist yet).
    pub out_path: &'a Path,
    /// Scratch directory for temporary files (created and cleaned by the engine).
    pub work_dir: &'a Path,
    /// Cancel / pause flags.
    pub ctl: &'a JobControl,
}

/// What a finished export produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportReport {
    /// Final file.
    pub out_path: PathBuf,
    /// Size in bytes.
    pub bytes: u64,
    /// Output duration in seconds.
    pub duration_secs: f64,
    /// Encoder actually used ("h264_nvenc", "Microsoft H.264 (software)", "stream copy", ...).
    pub encoder: String,
    /// Whether the smart lossless path was used.
    pub smart: bool,
    /// Warnings (missing media substituted, encoder fallback, size missed the target, ...).
    pub warnings: Vec<String>,
}

/// Runs an export end to end. Writes `<out_path>.part` and renames on success; removes the `.part`
/// on cancel or error. Dispatches on `settings.kind`: Video (smart path when `settings.lossless`
/// and eligible, else the full pipeline), Still (see `still`), AudioOnly (M4A, not supported yet:
/// returns `RenderError::Unsupported`).
/// For `QualityMode::TargetSize` it solves a bitrate, encodes, and when the result exceeds the
/// target by more than 3 % re-encodes at a lower bitrate (at most 2 retries), then warns if still
/// over. Encoder choice: `settings.encoder == "software"` = Microsoft software H.264; "auto" =
/// first hardware encoder for the codec, falling back to software with a warning; otherwise the
/// named encoder. Output frame size is `settings.width` x `settings.height`.
pub fn run_export(
    req: &ExportRequest<'_>,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<ExportReport, RenderError> {
    if req.out_path.exists() {
        return Err(RenderError::Output(format!(
            "destination exists: {}",
            req.out_path.display()
        )));
    }
    let part = part_path(req.out_path);
    let _ = fs::remove_file(&part);
    let created_work = !req.work_dir.exists();
    fs::create_dir_all(req.work_dir).map_err(|e| io_out("create work dir", e))?;

    let mut warnings = Vec::new();
    let result = match &req.settings.kind {
        ExportKind::Video => export_video(req, on_progress, &mut warnings),
        ExportKind::Still {
            format,
            quality,
            at,
        } => export_still(req, format, *quality, *at),
        ExportKind::AudioOnly => Err(RenderError::Unsupported("audio-only export")),
    };

    // Clean up what this export created: the pass directories, and the work dir when we made it.
    for n in 0..=MAX_RETRIES {
        let _ = fs::remove_dir_all(req.work_dir.join(format!("pass-{n}")));
    }
    if created_work {
        let _ = fs::remove_dir_all(req.work_dir);
    }
    match result {
        Ok(mut report) => {
            report.warnings = warnings;
            Ok(report)
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Retries after a TargetSize pass that came out too large.
const MAX_RETRIES: u32 = 2;
/// Safety factor on the bitrate scaled by the measured overshoot, so a retry aims slightly under
/// the target (0.97 = 3 % margin).
const RETRY_SAFETY: f64 = 0.97;
/// Lowest bitrate a retry will try, in kbps.
const MIN_RETRY_KBPS: f64 = 100.0;
/// Share of the overall progress bar given to the loudness pre-pass when normalisation is on.
const LOUDNESS_SHARE: f32 = 10.0;
/// Allowed overshoot of a target size before a retry (3 %).
const TARGET_TOLERANCE: f64 = 1.03;
/// Frame rate cap for exports.
const MAX_FPS: u32 = 120;
/// Quality-vs-speed hint for the software encoder (0 fastest .. 100 best); speed-leaning.
const QUALITY_VS_SPEED_SW: u32 = 50;
/// Quality-vs-speed hint for hardware encoders.
const QUALITY_VS_SPEED_HW: u32 = 70;

/// Video export: the smart path when it applies, else decode, composite, encode and mux.
fn export_video(
    req: &ExportRequest<'_>,
    on_progress: &mut dyn FnMut(Progress),
    warnings: &mut Vec<String>,
) -> Result<ExportReport, RenderError> {
    let s = req.settings;
    let project = req.project;
    let problems = validate_settings(s);
    if !problems.is_empty() {
        return Err(RenderError::Invalid(problems.join("; ")));
    }

    let range = resolve_range(project, &s.range)?;
    let plan = derive_plan(project, range);
    if plan.start >= plan.end {
        return Err(RenderError::Invalid(
            "the export range is empty".to_string(),
        ));
    }
    let duration = plan.end.saturating_sub(plan.start);
    let duration_secs = duration.as_secs_f64();

    if s.lossless {
        let eligibility = smart::check_eligibility(project, s);
        if eligibility.eligible {
            let bytes = smart::run_lossless(project, s, req.out_path, req.ctl, on_progress)?;
            warnings.extend(eligibility.snap_notes);
            return Ok(ExportReport {
                out_path: req.out_path.to_path_buf(),
                bytes,
                duration_secs,
                encoder: "stream copy".to_string(),
                smart: true,
                warnings: Vec::new(),
            });
        }
        let reason = eligibility
            .reason
            .unwrap_or_else(|| "the timeline is not eligible".to_string());
        warnings.push(format!("Smart lossless export not used: {reason}"));
    }

    let width = s.width;
    let height = s.height;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(RenderError::Invalid(format!(
            "output size {width}x{height} must be even and non-zero"
        )));
    }
    let fps = s.fps.unwrap_or(project.canvas.fps);
    let fps = if fps.num == 0 || fps.den == 0 {
        return Err(RenderError::Invalid(
            "frame rate must be positive".to_string(),
        ));
    } else if fps.as_f64() > f64::from(MAX_FPS) {
        Rate::whole(MAX_FPS)
    } else {
        fps
    };
    let fps_int = (fps.as_f64().round() as u32).clamp(1, MAX_FPS);

    let want = codec_for(s.codec)?;
    let chosen = choose_encoder(want, &s.encoder, warnings)?;
    let codec = chosen.codec;

    let audio_tracks = match s.audio.layout {
        AudioLayout::Mix => 1,
        AudioLayout::Separate => project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .count()
            .max(1),
    };
    let audio_tracks_u32 = u32::try_from(audio_tracks).unwrap_or(1);
    let audio_kbps_total = s.audio.bitrate_kbps.saturating_mul(audio_tracks_u32);

    // The codec actually encoded with (HEVC may have fallen back to H.264), so the Quality mode
    // bitrate uses that codec's efficiency factor.
    let used_codec = match codec {
        Codec::H264 => VideoCodec::H264,
        Codec::Hevc => VideoCodec::Hevc,
    };
    let cbr = matches!(s.quality, QualityMode::Cbr { .. });
    let (mut kbps, target_bytes) = match s.quality {
        QualityMode::Quality { value } => (
            quality_to_kbps(value, width, height, fps.as_f64(), used_codec),
            None,
        ),
        QualityMode::Bitrate { kbps } | QualityMode::Cbr { kbps } => (kbps, None),
        QualityMode::TargetSize { mb } => (
            solve_target_bitrate(mb, duration, audio_kbps_total),
            Some(mb * 1_000_000.0),
        ),
    };

    let mut cfg = PipelineConfig {
        out_w: width,
        out_h: height,
        fps,
        sample_rate: s.audio.sample_rate,
        layout: s.audio.layout,
        audio_tracks,
        limiter: s.audio.limiter,
        loudness_gain_db: 0.0,
        video: true,
        audio: true,
    };
    let normalise = matches!(s.audio.loudness, Loudness::Lufs { .. });
    if let Loudness::Lufs { target } = s.audio.loudness {
        wait_while_paused(req.ctl, duration_secs, on_progress)?;
        if req.ctl.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        report_progress(on_progress, 0.0, duration_secs, 0.0);
        let mut audio = MfAudioProvider::new(project);
        // measure_loudness has no progress callback, so the pre-pass reports only its start and
        // end, as the first LOUDNESS_SHARE percent of the job.
        let measured = pipeline::measure_loudness(&plan, &cfg, &mut audio, req.ctl)?;
        report_progress(on_progress, 0.0, duration_secs, LOUDNESS_SHARE);
        match measured {
            Some(measured) => cfg.loudness_gain_db = normalize_gain_db(measured, target),
            None => {
                warnings.push("Loudness normalisation skipped: the audio is silent".to_string())
            }
        }
    }
    let (offset, span) = if normalise {
        (LOUDNESS_SHARE, 100.0 - LOUDNESS_SHARE)
    } else {
        (0.0, 100.0)
    };
    // Progress budget: a target-size job may run up to MAX_RETRIES + 1 passes (the first pass
    // gets most of the bar, see `pass_span`), a single pass otherwise. The budget is fixed so the
    // bar never moves backwards when a retry is added.
    let budget: u32 = if target_bytes.is_some() {
        MAX_RETRIES + 1
    } else {
        1
    };

    let job = Job {
        project,
        plan,
        cfg,
        ctl: req.ctl,
        desc: chosen.desc,
        fallback: chosen.fallback,
        codec,
        width,
        height,
        fps,
        fps_int,
        audio_tracks,
        sample_rate: s.audio.sample_rate,
        audio_kbps: s.audio.bitrate_kbps,
    };

    let part = part_path(req.out_path);
    let mut attempt: u32 = 0;
    let mut allow_hw = true;
    let mut first_miss = 1.0_f64;
    let mut used: String;
    let mut used_hw: bool;
    // The highest percent reported so far. A pass that is restarted in software resumes from the
    // same point, so the reported percent never goes back.
    let mut high = 0.0_f32;
    let mut monotone = |p: Progress| {
        high = high.max(p.percent);
        on_progress(Progress { percent: high, ..p });
    };
    let bytes = loop {
        if req.ctl.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
        let rate = rate_control(cbr, kbps);
        let pass_dir = req.work_dir.join(format!("pass-{attempt}"));
        let map = ProgressMap {
            offset,
            span,
            attempt,
            budget,
        };
        let pass = encode_pass(
            &job,
            rate,
            &pass_dir,
            &part,
            map,
            &mut allow_hw,
            &mut monotone,
            warnings,
        )?;
        if pass.restart {
            // The hardware encoder failed early and allow_hw is now false: redo this pass in software.
            continue;
        }
        let bytes = pass.bytes;
        used = pass.encoder;
        used_hw = pass.hardware;
        match target_bytes {
            Some(target) if (bytes as f64) > target * TARGET_TOLERANCE => {
                let ratio = bytes as f64 / target;
                if attempt == 0 {
                    first_miss = ratio;
                }
                // A second retry only when the first pass missed by more than 10 %.
                let may_retry = attempt == 0 || (attempt == 1 && first_miss > 1.10);
                if !may_retry || attempt >= MAX_RETRIES {
                    break bytes;
                }
                attempt += 1;
                // Scale the video bitrate so one retry can converge (audio share removed).
                kbps = retry_video_kbps(kbps, bytes, target, duration_secs, audio_kbps_total);
            }
            _ => break bytes,
        }
    };
    if let Some(target) = target_bytes {
        if (bytes as f64) > target * TARGET_TOLERANCE {
            warnings.push(format!(
                "The file is {:.1} MB, over the {:.1} MB target after {} passes",
                bytes as f64 / 1_000_000.0,
                target / 1_000_000.0,
                attempt + 1
            ));
        }
    }
    warnings.push(format!(
        "Encoder: {used} ({})",
        if used_hw { "hardware" } else { "software" }
    ));

    fs::rename(&part, req.out_path).map_err(|e| {
        let _ = fs::remove_file(&part);
        io_out("rename output", e)
    })?;
    report_progress(&mut monotone, duration_secs, duration_secs, 100.0);

    Ok(ExportReport {
        out_path: req.out_path.to_path_buf(),
        bytes,
        duration_secs,
        encoder: used,
        smart: false,
        warnings: Vec::new(),
    })
}

/// Still-frame export: one composed frame at the settings' size, then WIC.
fn export_still(
    req: &ExportRequest<'_>,
    format: &str,
    quality: u32,
    at: Tick,
) -> Result<ExportReport, RenderError> {
    if req.ctl.is_cancelled() {
        return Err(RenderError::Cancelled);
    }
    let s = req.settings;
    let problems = validate_settings(s);
    if !problems.is_empty() {
        return Err(RenderError::Invalid(problems.join("; ")));
    }
    let frame = still::render_frame_with(req.project, at, req.ctl)?;
    let frame = if frame.width == s.width && frame.height == s.height {
        frame
    } else {
        reference::resize(&frame, s.width, s.height)
    };
    still::write_image(&frame, req.out_path, format, quality)?;
    let bytes = fs::metadata(req.out_path)
        .map_err(|e| io_out("stat output", e))?
        .len();
    Ok(ExportReport {
        out_path: req.out_path.to_path_buf(),
        bytes,
        duration_secs: 0.0,
        encoder: format!("WIC {format}"),
        smart: false,
        warnings: Vec::new(),
    })
}

/// Export range in timeline ticks, or None for the whole timeline.
fn resolve_range(
    project: &Project,
    range: &ExportRange,
) -> Result<Option<(Tick, Tick)>, RenderError> {
    match range {
        ExportRange::Entire => Ok(None),
        ExportRange::WorkArea => Ok(project.work_area.map(|w| (w.start, w.end))),
        ExportRange::SelectedClips { ids } => {
            let clips: Vec<_> = ids.iter().filter_map(|id| project.clip(id)).collect();
            let start = clips.iter().map(|c| c.start).reduce(Tick::min);
            let end = clips.iter().map(|c| c.end()).reduce(Tick::max);
            match (start, end) {
                (Some(a), Some(b)) => Ok(Some((a, b))),
                _ => Err(RenderError::Invalid(
                    "none of the selected clips exist".to_string(),
                )),
            }
        }
    }
}

/// Maps the settings codec to a hardware-encoder codec. AV1 has no encoder path here.
fn codec_for(codec: VideoCodec) -> Result<Codec, RenderError> {
    match codec {
        VideoCodec::H264 => Ok(Codec::H264),
        VideoCodec::Hevc => Ok(Codec::Hevc),
        VideoCodec::Av1 => Err(RenderError::Invalid(
            "AV1 export is not supported by the export engine yet".to_string(),
        )),
    }
}

/// Software encoder for `codec`. HEVC has no software encoder here, so it falls back to H.264
/// with a warning.
fn software_encoder(
    codec: Codec,
    warnings: &mut Vec<String>,
) -> Result<(EncoderDescriptor, Codec), RenderError> {
    let list = clipcheck_hwenc::enumerate(codec).map_err(enc_err)?;
    if let Some(d) = list.into_iter().find(|d| !d.hardware) {
        return Ok((d, codec));
    }
    if codec == Codec::Hevc {
        warnings.push("No software HEVC encoder is available; exporting H.264 instead".to_string());
        let list = clipcheck_hwenc::enumerate(Codec::H264).map_err(enc_err)?;
        if let Some(d) = list.into_iter().find(|d| !d.hardware) {
            return Ok((d, Codec::H264));
        }
    }
    Err(RenderError::Encode(
        "no software H.264 encoder is available".to_string(),
    ))
}

/// The encoder to try first and the software encoder to fall back to if the first one fails.
struct Chosen {
    desc: EncoderDescriptor,
    codec: Codec,
    /// Software encoder for the same codec, used when a hardware encoder cannot be opened or
    /// fails on its first frame. None for software choices.
    fallback: Option<EncoderDescriptor>,
}

/// Chooses the encoder for `requested` ("software", "auto" or an encoder name). Hardware encoders
/// are fed D3D11 NV12 textures (see `gpuenc`); `fallback` carries the software encoder to use if
/// the hardware path fails at run time.
fn choose_encoder(
    codec: Codec,
    requested: &str,
    warnings: &mut Vec<String>,
) -> Result<Chosen, RenderError> {
    clipcheck_hwenc::mf_startup().map_err(enc_err)?;
    let software_only = |warnings: &mut Vec<String>| {
        software_encoder(codec, warnings).map(|(desc, codec)| Chosen {
            desc,
            codec,
            fallback: None,
        })
    };
    let hardware = |hw: EncoderDescriptor, list: &[EncoderDescriptor]| Chosen {
        fallback: list.iter().find(|d| !d.hardware).cloned(),
        desc: hw,
        codec,
    };
    match requested {
        "software" => software_only(warnings),
        "auto" => {
            let list = clipcheck_hwenc::enumerate(codec).map_err(enc_err)?;
            match list.iter().find(|d| d.hardware).cloned() {
                Some(hw) => Ok(hardware(hw, &list)),
                None => software_only(warnings),
            }
        }
        name => {
            let list = clipcheck_hwenc::enumerate(codec).map_err(enc_err)?;
            match list.iter().find(|d| d.name == name).cloned() {
                Some(d) if !d.hardware => Ok(Chosen {
                    desc: d,
                    codec,
                    fallback: None,
                }),
                Some(d) => Ok(hardware(d, &list)),
                None => {
                    let chosen = software_only(warnings)?;
                    warnings.push(format!(
                        "Encoder {name} was not found; exporting with {}",
                        chosen.desc.name
                    ));
                    Ok(chosen)
                }
            }
        }
    }
}

/// Rate control for a bitrate: CBR when the quality mode asked for it, else VBR with a 1.5x peak.
fn rate_control(cbr: bool, kbps: u32) -> RateControl {
    if cbr {
        RateControl::Cbr { kbps }
    } else {
        RateControl::Vbr {
            kbps,
            peak_kbps: kbps.saturating_mul(3) / 2,
        }
    }
}

/// Everything one encode pass needs; built once per export.
struct Job<'a> {
    project: &'a Project,
    plan: RenderPlan,
    cfg: PipelineConfig,
    ctl: &'a JobControl,
    desc: EncoderDescriptor,
    /// Software encoder for the same codec, tried if `desc` is hardware and fails.
    fallback: Option<EncoderDescriptor>,
    codec: Codec,
    width: u32,
    height: u32,
    fps: Rate,
    fps_int: u32,
    audio_tracks: usize,
    sample_rate: u32,
    audio_kbps: u32,
}

/// Appends `incoming` to `dst`, skipping any message `dst` already holds. A target-size export
/// runs several passes that each report the same warnings, so plain `extend` would repeat them.
fn extend_unique(dst: &mut Vec<String>, incoming: impl IntoIterator<Item = String>) {
    for w in incoming {
        if !dst.contains(&w) {
            dst.push(w);
        }
    }
}

/// Video bitrate (kbps) for a target-size retry. The measured overshoot covers the whole file,
/// audio included, so the audio share (`audio_kbps_total` over `secs`) is removed from both the
/// actual and the target size before scaling the video rate. Falls back to scaling by the plain
/// ratio when the actual video size is not positive, and never goes below `MIN_RETRY_KBPS`.
fn retry_video_kbps(kbps: u32, bytes: u64, target: f64, secs: f64, audio_kbps_total: u32) -> u32 {
    let audio_bytes = f64::from(audio_kbps_total) * secs / 8.0 * 1000.0;
    let actual_video = bytes as f64 - audio_bytes;
    let target_video = target - audio_bytes;
    let scaled = if actual_video <= 0.0 {
        f64::from(kbps) * (target / bytes as f64) * RETRY_SAFETY
    } else if target_video <= 0.0 {
        MIN_RETRY_KBPS
    } else {
        f64::from(kbps) * (target_video / actual_video) * RETRY_SAFETY
    };
    scaled.max(MIN_RETRY_KBPS) as u32
}

/// Result of one encode pass.
struct PassResult {
    bytes: u64,
    /// Name of the encoder that produced the video.
    encoder: String,
    hardware: bool,
    /// True when the hardware encoder failed within its first frames: nothing from this pass is
    /// usable, and the caller must run the same pass again (now in software).
    restart: bool,
}

impl PassResult {
    /// The result of an abandoned pass that must be redone.
    fn restart() -> PassResult {
        PassResult {
            bytes: 0,
            encoder: String::new(),
            hardware: false,
            restart: true,
        }
    }
}

/// Runs one full encode into `part` (a flat faststart MP4). `allow_hw` is cleared when the
/// hardware encoder failed so later passes go straight to the software encoder.
#[allow(clippy::too_many_arguments)]
fn encode_pass(
    job: &Job<'_>,
    rate: RateControl,
    pass_dir: &Path,
    part: &Path,
    map: ProgressMap,
    allow_hw: &mut bool,
    on_progress: &mut dyn FnMut(Progress),
    warnings: &mut Vec<String>,
) -> Result<PassResult, RenderError> {
    let _ = fs::remove_dir_all(pass_dir);
    fs::create_dir_all(pass_dir).map_err(|e| io_out("create pass dir", e))?;
    let _ = fs::remove_file(part);

    let base_cfg = VideoEncoderConfig {
        codec: job.codec,
        width: job.width,
        height: job.height,
        fps: job.fps_int,
        rate,
        gop_frames: job.fps_int.saturating_mul(2).max(1),
        quality_vs_speed: QUALITY_VS_SPEED_SW,
        low_latency: false,
    };
    let (primary, fallback) = match (&job.fallback, *allow_hw) {
        (Some(sw), false) => (sw.clone(), None),
        (fb, _) => (job.desc.clone(), fb.clone()),
    };

    let mut aacs = Vec::with_capacity(job.audio_tracks);
    let mut specs = Vec::with_capacity(job.audio_tracks);
    for _ in 0..job.audio_tracks {
        let aac = AacEncoder::new(job.sample_rate, 2, job.audio_kbps).map_err(aac_err)?;
        specs.push(AudioTrackSpec {
            sample_rate: job.sample_rate,
            channels: 2,
            asc: aac.asc().to_vec(),
            bitrate: job.audio_kbps.saturating_mul(1000),
            title: String::new(),
        });
        aacs.push(aac);
    }

    let mp4_codec = match job.codec {
        Codec::H264 => Mp4VideoCodec::H264,
        Codec::Hevc => Mp4VideoCodec::Hevc,
    };
    let muxer = SegmentMuxer::new(SegmentConfig {
        out_dir: pass_dir.to_path_buf(),
        segment_secs: 1e9,
        fps: job.fps_int,
        codec: mp4_codec,
        width: job.width,
        height: job.height,
        audio: specs,
    })
    .map_err(mp4_err)?;

    // Set by the encoder thread when the hardware encoder fails within its first frames.
    let early_hw_failure = Arc::new(AtomicBool::new(false));
    let mut sink = EncodeSink::start(
        ThreadSetup {
            primary,
            fallback,
            base_cfg,
            muxer,
            tracks: job.audio_tracks,
            fps: job.fps,
            early_hw_failure: Arc::clone(&early_hw_failure),
        },
        aacs,
        job.sample_rate,
        (job.width, job.height),
    )?;
    let mut video = MfVideoProvider::new(job.project);
    let mut audio = MfAudioProvider::new(job.project);
    let mut titles = GdiTitleProvider;
    let mut scaled = |p: Progress| {
        on_progress(Progress {
            percent: map.apply(p.percent),
            ..p
        });
    };
    let run = pipeline::run(
        &job.plan,
        &job.cfg,
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        job.ctl,
        &mut scaled,
    );
    let report = match run {
        Ok(r) => r,
        Err(e) => {
            if early_hw_failure.load(Ordering::SeqCst) && !job.ctl.is_cancelled() {
                drop(sink);
                *allow_hw = false;
                return Ok(PassResult::restart());
            }
            return Err(e);
        }
    };
    extend_unique(warnings, report.warnings);

    let done = match sink.finish() {
        Ok(d) => d,
        Err(e) => {
            if early_hw_failure.load(Ordering::SeqCst) && !job.ctl.is_cancelled() {
                *allow_hw = false;
                return Ok(PassResult::restart());
            }
            return Err(e);
        }
    };
    extend_unique(warnings, done.warnings);
    if done.fell_back {
        *allow_hw = false;
    }
    if done.segments.is_empty() {
        return Err(RenderError::Encode(
            "the encoder produced no video".to_string(),
        ));
    }
    if job.ctl.is_cancelled() {
        return Err(RenderError::Cancelled);
    }
    concat_remux(&done.segments, part, &RemuxOptions::default()).map_err(mp4_err)?;
    let _ = fs::remove_dir_all(pass_dir);
    let bytes = fs::metadata(part)
        .map_err(|e| io_out("stat output", e))?
        .len();
    Ok(PassResult {
        bytes,
        encoder: done.encoder,
        hardware: done.hardware,
        restart: false,
    })
}

/// Maps one pass's own progress (0..=100) into the overall job percent: the job is
/// `offset..offset+span` percent, divided among the passes by `pass_span`.
#[derive(Debug, Clone, Copy)]
struct ProgressMap {
    offset: f32,
    span: f32,
    /// Zero-based pass number.
    attempt: u32,
    /// Passes the progress bar is divided into (fixed for the whole job).
    budget: u32,
}

/// Share of the job, in percent (0..=100), that pass `attempt` covers. A single-pass job uses the
/// whole bar. A target-size job gives its first pass 0..70 (most jobs finish in one pass) and the
/// retries 70..90 and 90..100. The spans are contiguous, so the bar never moves backwards.
fn pass_span(attempt: u32, budget: u32) -> (f32, f32) {
    if budget <= 1 {
        return (0.0, 100.0);
    }
    match attempt {
        0 => (0.0, 70.0),
        1 => (70.0, 90.0),
        _ => (90.0, 100.0),
    }
}

impl ProgressMap {
    /// Overall percent (0..=100) for a pass percent `pass` (0..=100). Non-decreasing in both
    /// `attempt` and `pass`, because each pass span starts where the previous one ended.
    fn apply(&self, pass: f32) -> f32 {
        let (lo, hi) = pass_span(self.attempt, self.budget);
        let t = pass.clamp(0.0, 100.0);
        let job = lo + (hi - lo) * t / 100.0;
        (self.offset + self.span * job / 100.0).clamp(0.0, 100.0)
    }
}

/// Reports a progress point for the loudness pre-pass. `percent` is the overall job percent.
fn report_progress(
    on_progress: &mut dyn FnMut(Progress),
    out_secs: f64,
    total_secs: f64,
    percent: f32,
) {
    on_progress(Progress {
        out_secs,
        total_secs,
        percent,
        frames: 0,
        paused: false,
    });
}

/// Waits while the job is paused (reporting paused progress) and returns `Err(Cancelled)` if the
/// job is cancelled while waiting.
fn wait_while_paused(
    ctl: &JobControl,
    total_secs: f64,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), RenderError> {
    while ctl.is_paused() {
        on_progress(Progress {
            out_secs: 0.0,
            total_secs,
            percent: 0.0,
            frames: 0,
            paused: true,
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        if ctl.is_cancelled() {
            return Err(RenderError::Cancelled);
        }
    }
    Ok(())
}

/// Messages from the pipeline thread to the encoder thread.
enum Msg {
    /// A composed frame, with the audio packets (per track) that became ready since the previous
    /// frame. Audio rides along with video so it never takes a channel slot of its own.
    Video {
        index: u64,
        frame: Frame,
        audio: Vec<(usize, Vec<AudioPacket>)>,
    },
    /// Encoded audio packets of one track, sent after the last frame.
    Audio {
        track: usize,
        packets: Vec<AudioPacket>,
    },
    /// No more input: flush and finish.
    End,
}

/// Frames in flight between the pipeline and the encoder thread (each holds a full frame).
const CHANNEL_DEPTH: usize = 6;
/// A hardware encoder failure before this many frames restarts the pass in software.
const EARLY_HW_FRAMES: u64 = 60;

/// What the encoder thread reports when it finishes.
struct ThreadDone {
    segments: Vec<PathBuf>,
    /// Name of the encoder that produced the video.
    encoder: String,
    hardware: bool,
    /// True when the hardware encoder failed and the software fallback took over.
    fell_back: bool,
    warnings: Vec<String>,
}

/// Everything the encoder thread needs.
struct ThreadSetup {
    primary: EncoderDescriptor,
    fallback: Option<EncoderDescriptor>,
    base_cfg: VideoEncoderConfig,
    muxer: SegmentMuxer,
    tracks: usize,
    fps: Rate,
    /// Set when the hardware encoder fails within `EARLY_HW_FRAMES` frames.
    early_hw_failure: Arc<AtomicBool>,
}

/// Video encoder plus its input path.
struct VideoPath {
    enc: Box<dyn VideoEncoder>,
    gpu: Option<GpuUploader>,
    desc: EncoderDescriptor,
}

/// Opens `desc` for `base_cfg` with the quality/speed hint suited to its kind.
fn open_video(
    desc: &EncoderDescriptor,
    base_cfg: &VideoEncoderConfig,
) -> Result<VideoPath, RenderError> {
    let cfg = VideoEncoderConfig {
        quality_vs_speed: if desc.hardware {
            QUALITY_VS_SPEED_HW
        } else {
            QUALITY_VS_SPEED_SW
        },
        ..*base_cfg
    };
    if desc.hardware {
        let gpu = GpuUploader::new(desc.vendor, cfg.width, cfg.height)?;
        let enc = clipcheck_hwenc::open(desc, Some(gpu.device()), &cfg).map_err(enc_err)?;
        Ok(VideoPath {
            enc,
            gpu: Some(gpu),
            desc: desc.clone(),
        })
    } else {
        let enc = clipcheck_hwenc::open(desc, None, &cfg).map_err(enc_err)?;
        Ok(VideoPath {
            enc,
            gpu: None,
            desc: desc.clone(),
        })
    }
}

/// Runs on the encoder thread: encodes frames, interleaves audio and muxes.
struct Worker {
    path: VideoPath,
    fallback: Option<EncoderDescriptor>,
    base_cfg: VideoEncoderConfig,
    muxer: SegmentMuxer,
    /// Encoded audio waiting to be interleaved, per track.
    queues: Vec<VecDeque<AudioPacket>>,
    fps: Rate,
    nv12: Vec<u8>,
    frames: u64,
    fell_back: bool,
    warnings: Vec<String>,
    early_hw_failure: Arc<AtomicBool>,
}

impl Worker {
    /// Switches to the software fallback after a hardware failure, or returns `err` when there is
    /// none (or the software encoder is already in use).
    fn switch_to_fallback(&mut self, err: RenderError) -> Result<(), RenderError> {
        let Some(sw) = self.fallback.take() else {
            return Err(err);
        };
        if !self.path.desc.hardware {
            return Err(err);
        }
        self.warnings.push(format!(
            "Hardware encoder {} failed ({err}); exporting with {}",
            self.path.desc.name, sw.name
        ));
        self.path = open_video(&sw, &self.base_cfg)?;
        self.fell_back = true;
        Ok(())
    }

    /// Encodes one frame with the current encoder.
    fn encode_one(&mut self, frame: &Frame, index: u64) -> Result<Vec<EncodedPacket>, RenderError> {
        let pts = frame_pts(index, self.fps);
        let force = index == 0;
        match &mut self.path.gpu {
            Some(gpu) => {
                let tex = gpu.upload(&frame.bgra)?;
                self.path
                    .enc
                    .encode(InputFrame::Texture(tex), pts, force)
                    .map_err(enc_err)
            }
            None => {
                nv12::bgra_to_nv12(frame, &mut self.nv12);
                self.path
                    .enc
                    .encode(
                        InputFrame::Nv12 {
                            data: &self.nv12,
                            width: frame.width,
                            height: frame.height,
                        },
                        pts,
                        force,
                    )
                    .map_err(enc_err)
            }
        }
    }

    fn video(&mut self, frame: &Frame, index: u64) -> Result<(), RenderError> {
        let result = self.encode_one(frame, index);
        let packets = match result {
            Ok(p) => p,
            // The first frame may switch encoders in place: nothing has been written yet.
            Err(e) if self.frames == 0 => {
                self.switch_to_fallback(e)?;
                self.encode_one(frame, index)?
            }
            Err(e) => {
                // An early hardware failure abandons the pass; the caller redoes it in software.
                if self.frames < EARLY_HW_FRAMES
                    && self.path.desc.hardware
                    && self.fallback.is_some()
                {
                    self.early_hw_failure.store(true, Ordering::SeqCst);
                }
                return Err(e);
            }
        };
        self.frames += 1;
        self.mux_video(packets)
    }

    /// Muxes encoder packets, writing audio that is due before each video packet first.
    fn mux_video(&mut self, packets: Vec<EncodedPacket>) -> Result<(), RenderError> {
        for p in packets {
            self.drain_audio(p.pts)?;
            self.muxer
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

    /// Writes every queued audio packet with `pts <= until`, per track.
    fn drain_audio(&mut self, until: i64) -> Result<(), RenderError> {
        for (k, q) in self.queues.iter_mut().enumerate() {
            while let Some(front) = q.front() {
                if front.pts > until {
                    break;
                }
                if let Some(p) = q.pop_front() {
                    self.muxer.push_audio(k, p).map_err(mp4_err)?;
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<ThreadDone, RenderError> {
        let rest = self.path.enc.flush().map_err(enc_err)?;
        self.mux_video(rest)?;
        self.drain_audio(i64::MAX)?;
        let segments = self.muxer.finish().map_err(mp4_err)?;
        Ok(ThreadDone {
            segments,
            encoder: self.path.desc.name.clone(),
            hardware: self.path.desc.hardware,
            fell_back: self.fell_back,
            warnings: self.warnings,
        })
    }
}

/// The encoder thread body. Returns Err(Cancelled) when the sender went away without `End`.
fn encoder_thread(
    setup: ThreadSetup,
    rx: Receiver<Msg>,
    free_tx: Sender<Vec<u8>>,
) -> Result<ThreadDone, RenderError> {
    let _com = crate::sources::ComGuard::enter();
    let ThreadSetup {
        primary,
        fallback,
        base_cfg,
        muxer,
        tracks,
        fps,
        early_hw_failure,
    } = setup;
    let mut warnings = Vec::new();
    let mut fell_back = false;
    let mut fallback = fallback;
    let path = match open_video(&primary, &base_cfg) {
        Ok(p) => p,
        Err(e) => match fallback.take() {
            Some(sw) if primary.hardware => {
                warnings.push(format!(
                    "Hardware encoder {} could not be opened ({e}); exporting with {}",
                    primary.name, sw.name
                ));
                fell_back = true;
                open_video(&sw, &base_cfg)?
            }
            _ => return Err(e),
        },
    };
    let mut worker = Worker {
        path,
        fallback,
        base_cfg,
        muxer,
        queues: (0..tracks).map(|_| VecDeque::new()).collect(),
        fps,
        nv12: Vec::new(),
        frames: 0,
        fell_back,
        warnings,
        early_hw_failure,
    };
    loop {
        match rx.recv() {
            Ok(Msg::Video {
                index,
                frame,
                audio,
            }) => {
                for (track, packets) in audio {
                    if let Some(q) = worker.queues.get_mut(track) {
                        q.extend(packets);
                    }
                }
                worker.video(&frame, index)?;
                let _ = free_tx.send(frame.bgra);
            }
            Ok(Msg::Audio { track, packets }) => {
                if let Some(q) = worker.queues.get_mut(track) {
                    q.extend(packets);
                }
            }
            Ok(Msg::End) => return worker.finish(),
            Err(_) => return Err(RenderError::Cancelled),
        }
    }
}

/// Receives composed frames and mixed audio on the pipeline thread. Video is handed to a
/// dedicated encoder thread (which also muxes) so compose and encode overlap; AAC runs here.
struct EncodeSink {
    tx: Option<SyncSender<Msg>>,
    handle: Option<JoinHandle<Result<ThreadDone, RenderError>>>,
    free_rx: Receiver<Vec<u8>>,
    aacs: Vec<AacEncoder>,
    /// Samples per channel pushed so far, per track.
    audio_frames: Vec<u64>,
    /// Encoded audio not yet sent, per track; it is sent with the next video frame.
    pending_audio: Vec<Vec<AudioPacket>>,
    sample_rate: u32,
    width: u32,
    height: u32,
}

impl EncodeSink {
    /// Spawns the encoder thread. `frame_size` is the (width, height) every video frame must have.
    fn start(
        setup: ThreadSetup,
        aacs: Vec<AacEncoder>,
        sample_rate: u32,
        frame_size: (u32, u32),
    ) -> Result<Self, RenderError> {
        let (tx, rx) = sync_channel(CHANNEL_DEPTH);
        let (free_tx, free_rx) = std::sync::mpsc::channel();
        let (width, height) = frame_size;
        let tracks = aacs.len();
        let handle = std::thread::Builder::new()
            .name("export-encoder".to_string())
            .spawn(move || encoder_thread(setup, rx, free_tx))
            .map_err(|e| io_out("spawn encoder thread", e))?;
        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
            free_rx,
            aacs,
            audio_frames: vec![0; tracks],
            pending_audio: (0..tracks).map(|_| Vec::new()).collect(),
            sample_rate,
            width,
            height,
        })
    }

    /// Takes the audio packets waiting to be sent, as (track, packets) pairs.
    fn take_pending_audio(&mut self) -> Vec<(usize, Vec<AudioPacket>)> {
        self.pending_audio
            .iter_mut()
            .enumerate()
            .filter(|(_, p)| !p.is_empty())
            .map(|(k, p)| (k, std::mem::take(p)))
            .collect()
    }

    /// Sends a message; when the encoder thread is gone, returns the error it ended with.
    fn send(&mut self, msg: Msg) -> Result<(), RenderError> {
        let sent = self.tx.as_ref().is_some_and(|tx| tx.send(msg).is_ok());
        if sent {
            Ok(())
        } else {
            Err(self.thread_error())
        }
    }

    /// Joins the encoder thread and returns why it stopped.
    fn thread_error(&mut self) -> RenderError {
        self.tx = None;
        match self.handle.take() {
            Some(h) => match h.join() {
                Ok(Err(e)) => e,
                Ok(Ok(_)) => RenderError::Encode("the encoder thread ended early".to_string()),
                Err(_) => RenderError::Encode("the encoder thread panicked".to_string()),
            },
            None => RenderError::Encode("the encoder thread is gone".to_string()),
        }
    }

    /// Flushes AAC, tells the encoder thread to finish and returns its result.
    fn finish(mut self) -> Result<ThreadDone, RenderError> {
        for k in 0..self.aacs.len() {
            let flushed = match self.aacs.get_mut(k) {
                Some(aac) => aac.flush().map_err(aac_err)?,
                None => continue,
            };
            if let Some(q) = self.pending_audio.get_mut(k) {
                q.extend(flushed.into_iter().map(to_audio_packet));
            }
        }
        for (track, packets) in self.take_pending_audio() {
            self.send(Msg::Audio { track, packets })?;
        }
        self.send(Msg::End)?;
        self.tx = None;
        match self.handle.take() {
            Some(h) => h
                .join()
                .map_err(|_| RenderError::Encode("the encoder thread panicked".to_string()))?,
            None => Err(RenderError::Encode(
                "the encoder thread is gone".to_string(),
            )),
        }
    }
}

impl Drop for EncodeSink {
    fn drop(&mut self) {
        // Closing the channel ends the thread; wait for it so its files are released.
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Sink for EncodeSink {
    fn video(&mut self, frame: &Frame, index: u64) -> Result<(), RenderError> {
        if frame.width != self.width || frame.height != self.height {
            return Err(RenderError::Encode(format!(
                "frame is {}x{}, expected {}x{}",
                frame.width, frame.height, self.width, self.height
            )));
        }
        let mut bgra = self.free_rx.try_recv().unwrap_or_default();
        bgra.clear();
        bgra.extend_from_slice(&frame.bgra);
        let audio = self.take_pending_audio();
        self.send(Msg::Video {
            index,
            frame: Frame {
                width: frame.width,
                height: frame.height,
                bgra,
            },
            audio,
        })
    }

    fn audio(&mut self, track: usize, samples: &[f32]) -> Result<(), RenderError> {
        let frames_done = self.audio_frames.get(track).copied().unwrap_or(0);
        let packets = {
            let Some(aac) = self.aacs.get_mut(track) else {
                return Ok(());
            };
            let pts = audio_pts(frames_done, self.sample_rate);
            aac.push(samples, pts).map_err(aac_err)?
        };
        if let Some(n) = self.audio_frames.get_mut(track) {
            *n = n.saturating_add((samples.len() / 2) as u64);
        }
        if let Some(q) = self.pending_audio.get_mut(track) {
            q.extend(packets.into_iter().map(to_audio_packet));
        }
        Ok(())
    }
}

/// Presentation time (100 ns) of output frame `index` at `fps`.
fn frame_pts(index: u64, fps: Rate) -> i64 {
    let num = i128::from(fps.num.max(1));
    let v = i128::from(index) * i128::from(fps.den) * 10_000_000 / num;
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Presentation time (100 ns) of sample frame `frames` at `sample_rate`.
fn audio_pts(frames: u64, sample_rate: u32) -> i64 {
    let v = i128::from(frames) * 10_000_000 / i128::from(sample_rate.max(1));
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Converts an AAC packet to a muxer audio packet.
fn to_audio_packet(p: clipcheck_aacenc::AacPacket) -> AudioPacket {
    AudioPacket {
        data: p.data,
        pts: p.pts,
        samples: p.samples,
    }
}

/// `<out>.part`, the temporary file written before the rename.
fn part_path(dst: &Path) -> PathBuf {
    let mut s = dst.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Maps an encoder error.
fn enc_err(e: clipcheck_hwenc::EncError) -> RenderError {
    RenderError::Encode(e.to_string())
}

/// Maps an AAC encoder error.
fn aac_err(e: clipcheck_aacenc::AacError) -> RenderError {
    RenderError::Encode(e.to_string())
}

/// Maps a muxer error.
fn mp4_err(e: clipcheck_mp4::Mp4Error) -> RenderError {
    RenderError::Output(format!("mux: {e}"))
}

/// Maps a file-system error.
fn io_out(context: &str, e: std::io::Error) -> RenderError {
    RenderError::Output(format!("{context}: {e}"))
}

#[cfg(test)]
mod progress_map_tests {
    use super::*;

    fn map(offset: f32, span: f32, attempt: u32, budget: u32) -> ProgressMap {
        ProgressMap {
            offset,
            span,
            attempt,
            budget,
        }
    }

    #[test]
    fn single_pass_job_spans_whole_bar() {
        let m = map(0.0, 100.0, 0, 1);
        assert_eq!(m.apply(0.0), 0.0);
        assert_eq!(m.apply(50.0), 50.0);
        assert_eq!(m.apply(100.0), 100.0);
    }

    #[test]
    fn single_pass_job_respects_loudness_offset() {
        // With loudness normalisation the encode gets `offset..100`.
        let m = map(20.0, 80.0, 0, 1);
        assert_eq!(m.apply(0.0), 20.0);
        assert_eq!(m.apply(100.0), 100.0);
    }

    #[test]
    fn first_pass_of_target_job_gets_most_of_the_bar() {
        let m = map(0.0, 100.0, 0, 3);
        assert_eq!(m.apply(50.0), 35.0);
        assert_eq!(m.apply(100.0), 70.0);
    }

    #[test]
    fn retries_split_the_remainder() {
        assert_eq!(map(0.0, 100.0, 1, 3).apply(0.0), 70.0);
        assert_eq!(map(0.0, 100.0, 1, 3).apply(100.0), 90.0);
        assert_eq!(map(0.0, 100.0, 2, 3).apply(0.0), 90.0);
        assert_eq!(map(0.0, 100.0, 2, 3).apply(100.0), 100.0);
    }

    #[test]
    fn progress_is_monotone_across_passes_and_ends_at_100() {
        let mut last = -1.0_f32;
        for attempt in 0..3 {
            for step in 0..=100 {
                let p = map(0.0, 100.0, attempt, 3).apply(step as f32);
                assert!(p >= last, "went back: {last} -> {p} at {attempt}/{step}");
                last = p;
            }
        }
        assert_eq!(last, 100.0);
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn warnings_are_not_repeated_across_passes() {
        let mut warnings = vec!["Encoder: x".to_string()];
        extend_unique(
            &mut warnings,
            vec!["Encoder: x".to_string(), "new".to_string()],
        );
        extend_unique(&mut warnings, vec!["new".to_string()]);
        assert_eq!(warnings, vec!["Encoder: x".to_string(), "new".to_string()]);
    }

    #[test]
    fn retry_removes_audio_share_before_scaling() {
        // 4 MB target, 60 s, 128 kbps audio = 960 kB of audio. First pass 4.8 MB.
        // Video: actual 3.84 MB, target 3.04 MB -> 1000 * 0.791666 * 0.97 = 767.9 kbps.
        let v = retry_video_kbps(1000, 4_800_000, 4_000_000.0, 60.0, 128);
        assert_eq!(v, 767);
        // The plain ratio (4.0 / 4.8) would have given 808, under-correcting the overshoot.
        let plain = (1000.0 * (4.0 / 4.8) * RETRY_SAFETY) as u32;
        assert!(v < plain, "{v} vs {plain}");
    }

    #[test]
    fn retry_without_audio_matches_plain_ratio() {
        let v = retry_video_kbps(1000, 4_800_000, 4_000_000.0, 60.0, 0);
        assert_eq!(v, (1000.0 * (4.0 / 4.8) * RETRY_SAFETY) as u32);
    }

    #[test]
    fn retry_falls_back_when_audio_exceeds_the_file() {
        // Audio alone is larger than the actual file: the plain ratio is used, never zero.
        let v = retry_video_kbps(1000, 100_000, 50_000.0, 60.0, 1000);
        assert!(v >= MIN_RETRY_KBPS as u32);
        assert_eq!(v, (1000.0 * (50_000.0 / 100_000.0) * RETRY_SAFETY) as u32);
    }

    #[test]
    fn retry_is_floored_when_target_is_inside_audio() {
        // Target smaller than the audio share: nothing video-wise fits, use the floor.
        let v = retry_video_kbps(1000, 2_000_000, 500_000.0, 60.0, 128);
        assert_eq!(v, MIN_RETRY_KBPS as u32);
    }
}
