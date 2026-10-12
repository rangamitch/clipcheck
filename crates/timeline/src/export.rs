//! Export settings, presets and pure export maths (docs/design/13-video-editor.md section 8).
//!
//! The types are final (contract). The functions below them are the pure export maths.

use serde::{Deserialize, Serialize};

use crate::time::{Rate, Tick};

/// Output video codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1 (hardware only).
    Av1,
}

/// How the quality of the video is controlled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum QualityMode {
    /// Constant-quality style slider, 1..=100 (higher is better).
    Quality {
        /// Slider value.
        value: u32,
    },
    /// Variable bitrate with this average.
    Bitrate {
        /// Average bitrate in kbps.
        kbps: u32,
    },
    /// Constant bitrate.
    Cbr {
        /// Bitrate in kbps.
        kbps: u32,
    },
    /// Solve the bitrate so the file lands near this size.
    TargetSize {
        /// Target size in megabytes (1 MB = 1,000,000 bytes).
        mb: f64,
    },
}

/// Which part of the timeline to export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportRange {
    /// Everything from 0 to the end of the last clip.
    Entire,
    /// The project's work area (falls back to Entire when none is set).
    WorkArea,
    /// The span covered by these clips.
    SelectedClips {
        /// Clip ids.
        ids: Vec<String>,
    },
}

/// Audio output layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioLayout {
    /// All audio mixed into one stereo track.
    Mix,
    /// Each timeline audio track becomes its own AAC track.
    Separate,
}

/// Loudness normalisation target.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Loudness {
    /// No normalisation.
    Off,
    /// Normalise integrated loudness to this LUFS value (for example -14 or -16).
    Lufs {
        /// Target in LUFS.
        target: f64,
    },
}

/// Audio export settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioExport {
    /// Layout.
    pub layout: AudioLayout,
    /// AAC bitrate per track in kbps (96, 128, 192, 256, 320).
    pub bitrate_kbps: u32,
    /// Sample rate (44100 or 48000).
    pub sample_rate: u32,
    /// Loudness normalisation.
    pub loudness: Loudness,
    /// Peak limiter at -1 dBFS.
    pub limiter: bool,
}

impl Default for AudioExport {
    fn default() -> Self {
        AudioExport {
            layout: AudioLayout::Mix,
            bitrate_kbps: 192,
            sample_rate: 48_000,
            loudness: Loudness::Off,
            limiter: true,
        }
    }
}

/// What kind of file to export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportKind {
    /// MP4 video.
    Video,
    /// One still frame.
    Still {
        /// "png" or "jpg".
        format: String,
        /// JPEG quality 1..=100.
        quality: u32,
        /// Frame time on the timeline.
        at: Tick,
    },
    /// M4A audio only.
    AudioOnly,
}

/// Everything the export dialog chooses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    /// Preset id the settings started from ("balanced_1080p30", "custom", ...).
    pub preset: String,
    /// File name template: tokens `{project} {date} {time} {yyyy-MM-dd HH-mm-ss} {res} {preset} {n}`.
    pub name_template: String,
    /// Output directory override. None = the configured export folder.
    pub dir: Option<String>,
    /// Range.
    pub range: ExportRange,
    /// Output width (even).
    pub width: u32,
    /// Output height (even).
    pub height: u32,
    /// Frame rate override. None = project fps.
    pub fps: Option<Rate>,
    /// Video codec.
    pub codec: VideoCodec,
    /// Encoder id: "auto", "software", or an encoder name from the daemon's encoder list.
    pub encoder: String,
    /// Quality control.
    pub quality: QualityMode,
    /// Audio.
    pub audio: AudioExport,
    /// Kind of output.
    pub kind: ExportKind,
    /// Use the smart lossless path (stream copy) when the timeline is eligible.
    pub lossless: bool,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            preset: "balanced_1080p30".to_string(),
            name_template: "{project} {yyyy-MM-dd HH-mm-ss}".to_string(),
            dir: None,
            range: ExportRange::Entire,
            width: 1920,
            height: 1080,
            fps: Some(Rate::whole(30)),
            codec: VideoCodec::H264,
            encoder: "auto".to_string(),
            quality: QualityMode::Quality { value: 70 },
            audio: AudioExport::default(),
            kind: ExportKind::Video,
            lossless: false,
        }
    }
}

/// A built-in or user preset: a name plus the settings it applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Stable id ("discord_10mb").
    pub id: String,
    /// Label shown in the UI.
    pub label: String,
    /// Settings applied. When `fixed_size` is true, width x height is a bounding box the canvas
    /// is fitted into and fps is applied; otherwise the project canvas is used.
    pub settings: ExportSettings,
    /// Whether width/height are fixed by the preset.
    pub fixed_size: bool,
    /// Maximum output size in MB when the preset exists to satisfy a size limit.
    pub max_mb: Option<f64>,
    /// Shipped with the app (false = user preset).
    pub builtin: bool,
}

/// Size estimate shown in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SizeEstimate {
    /// Estimated bytes.
    pub bytes: u64,
    /// Video bitrate used for the estimate, kbps.
    pub video_kbps: u32,
    /// Audio bitrate total, kbps.
    pub audio_kbps: u32,
}

/// Audio bitrates (kbps) the AAC encoder accepts.
const AUDIO_BITRATES: [u32; 7] = [64, 96, 128, 160, 192, 256, 320];

/// Largest even value at or below `v`.
fn even_down(v: u32) -> u32 {
    v - (v % 2)
}

/// Converts a float to an integer clamped into `lo..=hi`. NaN maps to `lo`.
fn clamp_to_u32(v: f64, lo: u32, hi: u32) -> u32 {
    if v.is_nan() {
        return lo;
    }
    v.max(f64::from(lo)).min(f64::from(hi)) as u32
}

/// Builds the video side of a built-in preset (audio layout Mix, sample rate 48 kHz).
fn video_settings(
    width: u32,
    height: u32,
    fps: Option<Rate>,
    codec: VideoCodec,
    quality: QualityMode,
    audio_kbps: u32,
) -> ExportSettings {
    ExportSettings {
        width,
        height,
        fps,
        codec,
        quality,
        audio: AudioExport {
            bitrate_kbps: audio_kbps,
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    }
}

/// Wraps settings into a built-in preset whose settings carry the preset's own id.
fn builtin(
    id: &str,
    label: &str,
    fixed_size: bool,
    max_mb: Option<f64>,
    settings: ExportSettings,
) -> Preset {
    Preset {
        id: id.to_string(),
        label: label.to_string(),
        settings: ExportSettings {
            preset: id.to_string(),
            ..settings
        },
        fixed_size,
        max_mb,
        builtin: true,
    }
}

/// Built-in presets (docs/design/13-video-editor.md section 8.3).
pub fn builtin_presets() -> Vec<Preset> {
    vec![
        builtin(
            "original_quality",
            "Original quality",
            false,
            None,
            video_settings(
                1920,
                1080,
                None,
                VideoCodec::H264,
                QualityMode::Quality { value: 90 },
                256,
            ),
        ),
        builtin(
            "high_1080p60",
            "High 1080p60",
            true,
            None,
            video_settings(
                1920,
                1080,
                Some(Rate::whole(60)),
                VideoCodec::H264,
                QualityMode::Quality { value: 80 },
                192,
            ),
        ),
        builtin(
            "balanced_1080p30",
            "Balanced 1080p30",
            true,
            None,
            video_settings(
                1920,
                1080,
                Some(Rate::whole(30)),
                VideoCodec::H264,
                QualityMode::Quality { value: 70 },
                192,
            ),
        ),
        builtin(
            "res_1440p",
            "1440p",
            true,
            None,
            video_settings(
                2560,
                1440,
                None,
                VideoCodec::H264,
                QualityMode::Quality { value: 75 },
                256,
            ),
        ),
        builtin(
            "res_4k",
            "4K",
            true,
            None,
            video_settings(
                3840,
                2160,
                None,
                VideoCodec::H264,
                QualityMode::Quality { value: 75 },
                256,
            ),
        ),
        builtin(
            "small_720p30",
            "Small file 720p30",
            true,
            None,
            video_settings(
                1280,
                720,
                Some(Rate::whole(30)),
                VideoCodec::H264,
                QualityMode::Bitrate { kbps: 2500 },
                128,
            ),
        ),
        builtin(
            "discord_10mb",
            "Discord 10 MB",
            true,
            Some(10.0),
            video_settings(
                1280,
                720,
                Some(Rate::whole(30)),
                VideoCodec::H264,
                QualityMode::TargetSize { mb: 9.5 },
                96,
            ),
        ),
        builtin(
            "discord_50mb",
            "Discord 50 MB",
            true,
            Some(50.0),
            video_settings(
                1920,
                1080,
                Some(Rate::whole(30)),
                VideoCodec::H264,
                QualityMode::TargetSize { mb: 48.0 },
                128,
            ),
        ),
        builtin(
            "discord_500mb",
            "Discord 500 MB",
            true,
            Some(500.0),
            video_settings(
                1920,
                1080,
                None,
                VideoCodec::H264,
                QualityMode::TargetSize { mb: 480.0 },
                192,
            ),
        ),
        builtin(
            "youtube",
            "YouTube",
            false,
            None,
            video_settings(
                1920,
                1080,
                None,
                VideoCodec::H264,
                QualityMode::Quality { value: 75 },
                256,
            ),
        ),
        builtin(
            "vertical_1080x1920",
            "Vertical 1080x1920",
            true,
            None,
            video_settings(
                1080,
                1920,
                Some(Rate::whole(30)),
                VideoCodec::H264,
                QualityMode::Bitrate { kbps: 10_000 },
                192,
            ),
        ),
        builtin(
            "twitter_x",
            "X / Twitter",
            true,
            None,
            video_settings(
                1280,
                720,
                Some(Rate::whole(30)),
                VideoCodec::H264,
                QualityMode::Bitrate { kbps: 6000 },
                128,
            ),
        ),
        builtin(
            "audio_only",
            "Audio only (M4A)",
            false,
            None,
            ExportSettings {
                kind: ExportKind::AudioOnly,
                audio: AudioExport {
                    bitrate_kbps: 192,
                    ..AudioExport::default()
                },
                ..ExportSettings::default()
            },
        ),
        builtin(
            "still_png",
            "Still frame (PNG)",
            false,
            None,
            ExportSettings {
                kind: ExportKind::Still {
                    format: "png".to_string(),
                    quality: 90,
                    at: Tick::ZERO,
                },
                ..ExportSettings::default()
            },
        ),
    ]
}

/// Looks a preset up by id among built-ins and the given user presets. User presets win.
pub fn find_preset(id: &str, user: &[Preset]) -> Option<Preset> {
    user.iter()
        .find(|p| p.id == id)
        .cloned()
        .or_else(|| builtin_presets().into_iter().find(|p| p.id == id))
}

/// Fits a canvas inside a bounding box, keeping its aspect ratio. The canvas is never scaled up,
/// so a canvas smaller than the box keeps its size. Sides are rounded to the nearest pixel, then
/// to even (rounded down), and kept at least 128.
fn fit_into_box(canvas_w: u32, canvas_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let (cw, ch) = (f64::from(canvas_w), f64::from(canvas_h));
    let scale = (f64::from(box_w) / cw).min(f64::from(box_h) / ch).min(1.0);
    let w = clamp_to_u32((cw * scale).round(), 128, u32::MAX);
    let h = clamp_to_u32((ch * scale).round(), 128, u32::MAX);
    (even_down(w), even_down(h))
}

/// Applies a preset to settings for a given canvas (keeps name template, dir, range, encoder).
/// When the preset is `fixed_size`, its width x height is a bounding box: the canvas is scaled
/// to fit inside it keeping the canvas aspect ratio (see [`fit_into_box`]), and the preset fps
/// is used. Otherwise the canvas size and the settings' own fps are kept. Sides are even.
pub fn apply_preset(
    settings: &ExportSettings,
    preset: &Preset,
    canvas_w: u32,
    canvas_h: u32,
) -> ExportSettings {
    let ps = &preset.settings;
    let (width, height, fps) = if preset.fixed_size {
        let (w, h) = fit_into_box(canvas_w, canvas_h, ps.width, ps.height);
        (w, h, ps.fps)
    } else {
        (even_down(canvas_w), even_down(canvas_h), settings.fps)
    };
    ExportSettings {
        preset: preset.id.clone(),
        width,
        height,
        fps,
        codec: ps.codec,
        quality: ps.quality,
        audio: ps.audio.clone(),
        kind: ps.kind.clone(),
        lossless: ps.lossless,
        ..settings.clone()
    }
}

/// Video bitrate (kbps) that makes a `duration` clip land near `target_mb`, after subtracting
/// audio and 2 % container overhead. Never below 100 kbps.
pub fn solve_target_bitrate(target_mb: f64, duration: Tick, audio_kbps_total: u32) -> u32 {
    let secs = duration.as_secs_f64();
    if !target_mb.is_finite() || !secs.is_finite() || secs <= 0.0 {
        return 100;
    }
    let total_bits = target_mb * 1_000_000.0 * 8.0 * 0.98;
    let video_kbps = total_bits / secs / 1000.0 - f64::from(audio_kbps_total);
    clamp_to_u32(video_kbps, 100, u32::MAX)
}

/// Maps the 1..=100 quality slider to a video bitrate (kbps) for a resolution, fps and codec.
/// Bits per pixel rise from 0.03 (value 1) to 0.25 (value 100) with a 1.5 power curve. HEVC and
/// AV1 use 0.65 of the H.264 rate. The result is clamped to 100..=200_000 kbps.
pub fn quality_to_kbps(value: u32, width: u32, height: u32, fps: f64, codec: VideoCodec) -> u32 {
    let v = value.clamp(1, 100);
    let t = f64::from(v - 1) / 99.0;
    let bits_per_pixel = 0.03 + 0.22 * t.powf(1.5);
    let codec_factor = match codec {
        VideoCodec::H264 => 1.0,
        VideoCodec::Hevc | VideoCodec::Av1 => 0.65,
    };
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        30.0
    };
    let kbps = f64::from(width) * f64::from(height) * fps * bits_per_pixel * codec_factor / 1000.0;
    clamp_to_u32(kbps, 100, 200_000)
}

/// Bytes for `kbps` sustained over `secs` seconds, with 2 % container overhead.
fn bytes_for(kbps: f64, secs: f64) -> u64 {
    // Saturating float-to-int cast; NaN becomes 0.
    (kbps * 125.0 * secs * 1.02) as u64
}

/// Estimates the output size. Video kinds count video plus audio; audio-only counts audio;
/// still frames use a rough raw-size figure (width * height * 1.5 bytes).
pub fn estimate_size(s: &ExportSettings, duration: Tick, audio_track_count: u32) -> SizeEstimate {
    let tracks = match s.audio.layout {
        AudioLayout::Mix => 1,
        AudioLayout::Separate => audio_track_count.max(1),
    };
    let audio_kbps = s.audio.bitrate_kbps.saturating_mul(tracks);
    let fps = s.fps.map_or(30.0, |r| r.as_f64());
    let secs = duration.as_secs_f64().max(0.0);
    match s.kind {
        ExportKind::Video => {
            let video_kbps = match s.quality {
                QualityMode::Quality { value } => {
                    quality_to_kbps(value, s.width, s.height, fps, s.codec)
                }
                QualityMode::Bitrate { kbps } | QualityMode::Cbr { kbps } => kbps,
                QualityMode::TargetSize { mb } => solve_target_bitrate(mb, duration, audio_kbps),
            };
            let total_kbps = f64::from(video_kbps) + f64::from(audio_kbps);
            SizeEstimate {
                bytes: bytes_for(total_kbps, secs),
                video_kbps,
                audio_kbps,
            }
        }
        ExportKind::AudioOnly => SizeEstimate {
            bytes: bytes_for(f64::from(audio_kbps), secs),
            video_kbps: 0,
            audio_kbps,
        },
        ExportKind::Still { .. } => SizeEstimate {
            bytes: u64::from(s.width) * u64::from(s.height) * 3 / 2,
            video_kbps: 0,
            audio_kbps: 0,
        },
    }
}

/// Expands a token of a name template. Unknown tokens expand to nothing.
fn expand_token(
    token: &str,
    project: &str,
    stamp: &(String, String, String),
    res: &str,
    preset: &str,
    n: u32,
) -> String {
    let (date, time, both) = stamp;
    match token {
        "project" => project.to_string(),
        "date" => date.clone(),
        "time" => time.clone(),
        "yyyy-MM-dd HH-mm-ss" => both.clone(),
        "res" => res.to_string(),
        "preset" => preset.to_string(),
        "n" if n > 0 => n.to_string(),
        _ => String::new(),
    }
}

/// True for names Windows reserves as device names (CON, COM1..COM9, ...), case-insensitively.
fn is_reserved_name(stem: &str) -> bool {
    let base = stem
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered = |prefix: &str| {
        base.strip_prefix(prefix)
            .is_some_and(|d| d.len() == 1 && d.chars().all(|c| ('1'..='9').contains(&c)))
    };
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
}

/// Makes arbitrary text a safe Windows file stem: drops reserved characters and control
/// characters, collapses spaces, trims spaces and dots, prefixes reserved device names,
/// limits the length to 120 characters and never returns an empty string.
fn sanitize_stem(raw: &str) -> String {
    let mut cleaned = String::new();
    let mut last_space = false;
    for c in raw.chars() {
        if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            continue;
        }
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        cleaned.push(c);
    }
    let trimmed = cleaned.trim_matches(|c: char| c == ' ' || c == '.');
    let prefixed = if is_reserved_name(trimmed) {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    };
    let limited: String = prefixed.chars().take(120).collect();
    let stem = limited.trim_matches(|c: char| c == ' ' || c == '.');
    if stem.is_empty() {
        "Export".to_string()
    } else {
        stem.to_string()
    }
}

/// Expands a name template and makes it a safe Windows file stem (no extension).
/// `now` is local time as (year, month, day, hour, minute, second). `n` is the collision counter
/// (0 = none). If `n` is above 0 and the template has no `{n}`, " (n)" is appended.
pub fn expand_name(
    template: &str,
    project: &str,
    now: (i32, u32, u32, u32, u32, u32),
    width: u32,
    height: u32,
    preset: &str,
    n: u32,
) -> String {
    let (y, mo, d, h, mi, s) = now;
    let date = format!("{y:04}-{mo:02}-{d:02}");
    let time = format!("{h:02}-{mi:02}-{s:02}");
    let both = format!("{date} {time}");
    let stamp = (date, time, both);
    let res = format!("{width}x{height}");

    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or(""));
        let after = rest.get(open + 1..).unwrap_or("");
        let Some(close) = after.find('}') else {
            // Unclosed brace: keep the remainder as typed.
            rest = rest.get(open..).unwrap_or("");
            break;
        };
        let token = after.get(..close).unwrap_or("");
        out.push_str(&expand_token(token, project, &stamp, &res, preset, n));
        rest = after.get(close + 1..).unwrap_or("");
    }
    out.push_str(rest);

    if n > 0 && !template.contains("{n}") {
        out.push_str(&format!(" ({n})"));
    }
    sanitize_stem(&out)
}

/// Checks export settings against the limits of section 8.8. Returns a list of human-readable
/// problems (empty = valid).
pub fn validate_settings(s: &ExportSettings) -> Vec<String> {
    let mut problems = Vec::new();

    if matches!(s.kind, ExportKind::AudioOnly) {
        problems.push("Audio-only export is not available yet".to_string());
    }
    for (name, v) in [("Width", s.width), ("Height", s.height)] {
        if v % 2 == 1 || !(128..=8192).contains(&v) {
            problems.push(format!(
                "{name} must be an even number from 128 to 8192 (got {v})"
            ));
        }
    }
    if let Some(fps) = s.fps {
        let f = fps.as_f64();
        if !(1.0..=120.0).contains(&f) {
            problems.push(format!("Frame rate must be from 1 to 120 fps (got {f})"));
        }
    }
    match s.quality {
        QualityMode::Quality { value } => {
            if !(1..=100).contains(&value) {
                problems.push(format!("Quality must be from 1 to 100 (got {value})"));
            }
        }
        QualityMode::Bitrate { kbps } | QualityMode::Cbr { kbps } => {
            if !(100..=200_000).contains(&kbps) {
                problems.push(format!(
                    "Bitrate must be from 100 to 200000 kbps (0.1 to 200 Mbps, got {kbps})"
                ));
            }
        }
        QualityMode::TargetSize { mb } => {
            if !mb.is_finite() || !(1.0..=65_536.0).contains(&mb) {
                problems.push(format!("Target size must be from 1 to 65536 MB (got {mb})"));
            }
        }
    }
    if !AUDIO_BITRATES.contains(&s.audio.bitrate_kbps) {
        problems.push(format!(
            "Audio bitrate must be one of 64, 96, 128, 160, 192, 256 or 320 kbps (got {})",
            s.audio.bitrate_kbps
        ));
    }
    if !matches!(s.audio.sample_rate, 44_100 | 48_000) {
        problems.push(format!(
            "Sample rate must be 44100 or 48000 Hz (got {})",
            s.audio.sample_rate
        ));
    }
    match s.audio.loudness {
        Loudness::Off => {}
        Loudness::Lufs { target } => {
            if !(-40.0..=-5.0).contains(&target) {
                problems.push(format!(
                    "Loudness target must be from -40 to -5 LUFS (got {target})"
                ));
            }
        }
    }
    if let ExportKind::Still {
        format, quality, ..
    } = &s.kind
    {
        if !(1..=100).contains(quality) {
            problems.push(format!(
                "Still image quality must be from 1 to 100 (got {quality})"
            ));
        }
        if !matches!(format.as_str(), "png" | "jpg") {
            problems.push(format!(
                "Still image format must be png or jpg (got {format})"
            ));
        }
    }
    problems
}
