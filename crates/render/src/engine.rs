//! Windows export engine: Media Foundation decode, CPU reference compositor, software H.264 or
//! HEVC encode, AAC audio, MP4 mux. Owner: worker `render-engine`.
//!
//! Adapted from `crates/clipedit/src/run.rs` (`precise_pipeline`): encoder open, frame push,
//! packet drain, audio interleave, segment mux and flat faststart output.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

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
use crate::job::{JobControl, Progress};
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
/// and eligible, else the full pipeline), Still (see `still`), AudioOnly (M4A).
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
/// Quality-vs-speed hint passed to the encoder (0 fastest .. 100 best).
const QUALITY_VS_SPEED: u32 = 70;

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
    let (desc, codec) = choose_encoder(want, &s.encoder, warnings)?;

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
    let attempts: u32 = if target_bytes.is_some() {
        MAX_RETRIES + 1
    } else {
        1
    };

    let job = Job {
        project,
        plan,
        cfg,
        ctl: req.ctl,
        desc,
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
            attempts,
        };
        let bytes = encode_pass(&job, rate, &pass_dir, &part, map, on_progress, warnings)?;
        match target_bytes {
            Some(target) if (bytes as f64) > target * TARGET_TOLERANCE && attempt < MAX_RETRIES => {
                attempt += 1;
                // Scale the bitrate by the measured overshoot so one retry can converge.
                let scaled = f64::from(kbps) * (target / bytes as f64) * RETRY_SAFETY;
                kbps = scaled.max(MIN_RETRY_KBPS) as u32;
            }
            _ => break bytes,
        }
    };
    if let Some(target) = target_bytes {
        if (bytes as f64) > target * TARGET_TOLERANCE {
            warnings.push(format!(
                "The file is {:.1} MB, over the {:.1} MB target after {} retries",
                bytes as f64 / 1_000_000.0,
                target / 1_000_000.0,
                MAX_RETRIES
            ));
        }
    }

    fs::rename(&part, req.out_path).map_err(|e| {
        let _ = fs::remove_file(&part);
        io_out("rename output", e)
    })?;

    Ok(ExportReport {
        out_path: req.out_path.to_path_buf(),
        bytes,
        duration_secs,
        encoder: job.desc.name.clone(),
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

/// Chooses the encoder for `requested` ("software", "auto" or an encoder name). Hardware
/// encoders need a D3D11 device and NV12 textures, which the export engine does not create yet,
/// so hardware choices fall back to the software encoder with a warning.
fn choose_encoder(
    codec: Codec,
    requested: &str,
    warnings: &mut Vec<String>,
) -> Result<(EncoderDescriptor, Codec), RenderError> {
    clipcheck_hwenc::mf_startup().map_err(enc_err)?;
    match requested {
        "software" => software_encoder(codec, warnings),
        "auto" => {
            let list = clipcheck_hwenc::enumerate(codec).map_err(enc_err)?;
            let software = software_encoder(codec, warnings)?;
            if let Some(hw) = list.iter().find(|d| d.hardware) {
                warnings.push(format!(
                    "Hardware encoder {} is not wired into the export engine yet; exporting with {}",
                    hw.name, software.0.name
                ));
            }
            Ok(software)
        }
        name => {
            let list = clipcheck_hwenc::enumerate(codec).map_err(enc_err)?;
            match list.into_iter().find(|d| d.name == name) {
                Some(d) if !d.hardware => Ok((d, codec)),
                Some(d) => {
                    let software = software_encoder(codec, warnings)?;
                    warnings.push(format!(
                        "Hardware encoder {} is not wired into the export engine yet; exporting with {}",
                        d.name, software.0.name
                    ));
                    Ok(software)
                }
                None => {
                    let software = software_encoder(codec, warnings)?;
                    warnings.push(format!(
                        "Encoder {name} was not found; exporting with {}",
                        software.0.name
                    ));
                    Ok(software)
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
    codec: Codec,
    width: u32,
    height: u32,
    fps: Rate,
    fps_int: u32,
    audio_tracks: usize,
    sample_rate: u32,
    audio_kbps: u32,
}

/// Runs one full encode into `part` (a flat faststart MP4) and returns its size.
fn encode_pass(
    job: &Job<'_>,
    rate: RateControl,
    pass_dir: &Path,
    part: &Path,
    map: ProgressMap,
    on_progress: &mut dyn FnMut(Progress),
    warnings: &mut Vec<String>,
) -> Result<u64, RenderError> {
    let _ = fs::remove_dir_all(pass_dir);
    fs::create_dir_all(pass_dir).map_err(|e| io_out("create pass dir", e))?;
    let _ = fs::remove_file(part);

    let enc_cfg = VideoEncoderConfig {
        codec: job.codec,
        width: job.width,
        height: job.height,
        fps: job.fps_int,
        rate,
        gop_frames: job.fps_int.saturating_mul(2).max(1),
        quality_vs_speed: QUALITY_VS_SPEED,
        low_latency: false,
    };
    let enc = clipcheck_hwenc::open(&job.desc, None, &enc_cfg).map_err(enc_err)?;

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

    let tracks = job.audio_tracks;
    let mut sink = EncodeSink {
        enc,
        muxer,
        aacs,
        queues: (0..tracks).map(|_| VecDeque::new()).collect(),
        audio_frames: vec![0; tracks],
        fps: job.fps,
        sample_rate: job.sample_rate,
        width: job.width,
        height: job.height,
        nv12: Vec::new(),
    };
    let mut video = MfVideoProvider::new(job.project);
    let mut audio = MfAudioProvider::new(job.project);
    let mut titles = GdiTitleProvider;
    let mut scaled = |p: Progress| {
        on_progress(Progress {
            percent: map.apply(p.percent),
            ..p
        });
    };
    let report = pipeline::run(
        &job.plan,
        &job.cfg,
        &mut video,
        &mut audio,
        &mut titles,
        &mut sink,
        job.ctl,
        &mut scaled,
    )?;
    warnings.extend(report.warnings);

    let segments = sink.finish()?;
    if segments.is_empty() {
        return Err(RenderError::Encode(
            "the encoder produced no video".to_string(),
        ));
    }
    if job.ctl.is_cancelled() {
        return Err(RenderError::Cancelled);
    }
    concat_remux(&segments, part, &RemuxOptions::default()).map_err(mp4_err)?;
    let _ = fs::remove_dir_all(pass_dir);
    let bytes = fs::metadata(part)
        .map_err(|e| io_out("stat output", e))?
        .len();
    Ok(bytes)
}

/// Maps one pass's own progress (0..=100) into the overall job percent: the job is
/// `offset..offset+span` percent, split evenly across `attempts` passes.
#[derive(Debug, Clone, Copy)]
struct ProgressMap {
    offset: f32,
    span: f32,
    attempt: u32,
    attempts: u32,
}

impl ProgressMap {
    /// Overall percent (0..=100) for a pass percent `pass` (0..=100).
    fn apply(&self, pass: f32) -> f32 {
        let frac = (f64::from(self.attempt) + f64::from(pass.clamp(0.0, 100.0)) / 100.0)
            / f64::from(self.attempts.max(1));
        (self.offset + self.span * frac as f32).clamp(0.0, 100.0)
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

/// Receives composed frames and mixed audio; encodes, muxes and flushes at the end.
struct EncodeSink {
    enc: Box<dyn VideoEncoder>,
    muxer: SegmentMuxer,
    aacs: Vec<AacEncoder>,
    /// Encoded audio waiting to be interleaved, per track.
    queues: Vec<VecDeque<AudioPacket>>,
    /// Samples per channel pushed so far, per track.
    audio_frames: Vec<u64>,
    fps: Rate,
    sample_rate: u32,
    width: u32,
    height: u32,
    /// Reused NV12 buffer.
    nv12: Vec<u8>,
}

impl EncodeSink {
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

    /// Flushes encoders, writes the remaining audio and finishes the segments.
    fn finish(mut self) -> Result<Vec<PathBuf>, RenderError> {
        let rest = self.enc.flush().map_err(enc_err)?;
        self.mux_video(rest)?;
        for (k, aac) in self.aacs.iter_mut().enumerate() {
            let packets = aac.flush().map_err(aac_err)?;
            if let Some(q) = self.queues.get_mut(k) {
                q.extend(packets.into_iter().map(to_audio_packet));
            }
        }
        self.drain_audio(i64::MAX)?;
        self.muxer.finish().map_err(mp4_err)
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
        bgra_to_nv12(frame, &mut self.nv12);
        let pts = frame_pts(index, self.fps);
        let packets = self
            .enc
            .encode(
                InputFrame::Nv12 {
                    data: &self.nv12,
                    width: self.width,
                    height: self.height,
                },
                pts,
                index == 0,
            )
            .map_err(enc_err)?;
        self.mux_video(packets)
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
        if let Some(q) = self.queues.get_mut(track) {
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

/// Converts a BGRA frame to NV12 (BT.601, limited range) in `out`. Width and height are even.
fn bgra_to_nv12(frame: &Frame, out: &mut Vec<u8>) {
    let w = frame.width as usize;
    let h = frame.height as usize;
    out.clear();
    out.resize(w * h + w * h / 2, 0);
    let (luma, chroma) = out.split_at_mut(w * h);
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let mut sum_r = 0i32;
            let mut sum_g = 0i32;
            let mut sum_b = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let x = bx * 2 + dx;
                    let y = by * 2 + dy;
                    let Some([b, g, r, _]) = frame.pixel(x as u32, y as u32) else {
                        continue;
                    };
                    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
                    sum_r += r;
                    sum_g += g;
                    sum_b += b;
                    if let Some(dst) = luma.get_mut(y * w + x) {
                        *dst = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
                    }
                }
            }
            let (r, g, b) = (sum_r / 4, sum_g / 4, sum_b / 4);
            let u = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            let v = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            let at = by * w + bx * 2;
            if let Some(dst) = chroma.get_mut(at) {
                *dst = u;
            }
            if let Some(dst) = chroma.get_mut(at + 1) {
                *dst = v;
            }
        }
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
