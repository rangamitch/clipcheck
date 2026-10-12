//! Maps `config.toml` settings onto the recorder and hotkey configuration
//! (task D1a).
//!
//! Everything here is pure given its inputs. The only environment access is
//! `%VAR%` expansion, which [`build_recorder_config`] performs through
//! `std::env::var`. [`build_recorder_config_with`] takes the lookup as an
//! argument so tests can supply a fixed environment. No directories are
//! created here.
//!
//! The daemon's service layer calls into this module when it builds the recorder
//! and hotkey configuration from `config.toml`.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use clipcheck_core::config::schema::Press;
use clipcheck_core::config::schema::{
    Audio, Codec, Config, EncoderChoice, EncoderSpeed, KeyChord, MuteMode, Preset, RateControl,
    Resolution, SourceRef, Video,
};
use clipcheck_encode::probe::{Backend, CodecKind, ProbeReport};
use clipcheck_encode::select::{self, Preference};
use clipcheck_encode::tuning::{EncoderTuning, RateMode, Speed};
use clipcheck_engine::canvas::canvas_for_source;
use clipcheck_engine::recorder::{AudioSourceKind, AudioTrackConfig, RecorderConfig, TrackSource};
use clipcheck_hotkeys::{Binding, Chord, HotkeyAction, PressKind};

/// Height used for `Native` resolution in Custom mode (the largest supported
/// canvas height, 2160p). The width of a Native canvas is decided per recording
/// from the captured window (see `canvas_height` in [`build_recorder_config_with`]);
/// this height only sets the fallback 16:9 canvas.
const NATIVE_CAP_HEIGHT: u32 = 2160;

/// Errors from [`build_recorder_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    /// No working encoder exists for the requested codec, even after fallback.
    NoEncoder {
        /// The codec from `[video] codec`.
        codec: Codec,
        /// The backend setting from `[video] encoder`.
        encoder: EncoderChoice,
    },
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::NoEncoder { codec, encoder } => write!(
                f,
                "no working encoder for codec {codec:?} with encoder setting {encoder:?}"
            ),
        }
    }
}

impl std::error::Error for MapError {}

/// Which encoder was chosen, and whether a forced backend was unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderChoiceInfo {
    /// Encoder id, for example `h264_nvenc`.
    pub encoder_name: String,
    /// Backend name (`nvenc`, `amf`, `qsv`, `software`) that was requested
    /// but not working, when Auto had to pick another one.
    pub fell_back_from: Option<String>,
}

/// Bitrate scale per codec, in permille of the H.264 value.
fn codec_scale_permille(codec: Codec) -> u32 {
    match codec {
        Codec::H264 => 1000,
        Codec::Hevc => 650,
        Codec::Av1 => 500,
    }
}

/// Maps the config codec to the probe codec family.
fn codec_kind(codec: Codec) -> CodecKind {
    match codec {
        Codec::H264 => CodecKind::H264,
        Codec::Hevc => CodecKind::H265,
        Codec::Av1 => CodecKind::Av1,
    }
}

/// Picks the encoder for the requested codec and returns it with the codec
/// actually used. The native encoder has no AV1 yet, so an AV1 request uses
/// HEVC, then H.264. The returned codec drives the bitrate scale and quality
/// offset, so they match the encoder that was picked.
///
/// `Selection::fell_back_from` names a backend, not a codec, so a codec
/// fallback is not reported through it. The fallback is only visible in the
/// encoder name.
fn select_encoder(
    report: &ProbeReport,
    codec: Codec,
    pref: &Preference,
) -> Option<(select::Selection, Codec)> {
    let order: &[Codec] = match codec {
        Codec::H264 => &[Codec::H264],
        Codec::Hevc => &[Codec::Hevc],
        Codec::Av1 => &[Codec::Hevc, Codec::H264],
    };
    order
        .iter()
        .find_map(|&c| select::pick(report, &codec_kind(c), pref).map(|selection| (selection, c)))
}

/// Maps the config encoder setting to a selection preference.
fn preference(choice: EncoderChoice) -> Preference {
    match choice {
        EncoderChoice::Auto => Preference::Auto,
        EncoderChoice::Nvenc => Preference::Force(Backend::Nvenc),
        EncoderChoice::Amf => Preference::Force(Backend::Amf),
        EncoderChoice::Qsv => Preference::Force(Backend::Qsv),
        EncoderChoice::Software => Preference::Force(Backend::Software),
    }
}

/// Lowercase name of a backend, as used in [`EncoderChoiceInfo`].
fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Nvenc => "nvenc",
        Backend::Amf => "amf",
        Backend::Qsv => "qsv",
        Backend::Software => "software",
    }
}

/// Output height for a resolution. `Native` keeps `cap_height`; the others are
/// their nominal heights.
fn resolve_height(resolution: Resolution, cap_height: u32) -> u32 {
    match resolution {
        Resolution::Native => cap_height,
        Resolution::P360 => 360,
        Resolution::P480 => 480,
        Resolution::P720 => 720,
        Resolution::P1080 => 1080,
        Resolution::P1440 => 1440,
        Resolution::P2160 => 2160,
    }
}

/// Fallback 16:9 canvas for a height. The settings map does not know the
/// captured window size, so this is only the canvas used when no source size
/// is known. The real canvas width follows the window per recording (D54), see
/// `clipcheck_engine::canvas::canvas_for_source`. 480p gives 854x480.
fn canvas_for_height(height: u32) -> (u32, u32) {
    canvas_for_source(16, 9, height)
}
/// Maps `video.encoder_speed` to the encoder speed preset.
fn speed_of(speed: EncoderSpeed) -> Speed {
    match speed {
        EncoderSpeed::Fastest => Speed::Fastest,
        EncoderSpeed::Balanced => Speed::Balanced,
        EncoderSpeed::Quality => Speed::Quality,
    }
}

/// Constant-quality value for a non-custom preset on the H.264 basis
/// (`docs/design/03-encoding.md` section 5.2). Lower is higher quality. `None`
/// for [`Preset::Custom`].
fn preset_base_cq(preset: Preset) -> Option<u8> {
    match preset {
        Preset::Low => Some(32),
        Preset::Medium => Some(29),
        Preset::High => Some(26),
        Preset::Ultra => Some(23),
        Preset::Custom => None,
    }
}

/// Constant-quality offset for a codec and encoder, added to the H.264 basis
/// (`docs/design/03-encoding.md` section 6.3): x265 +5, other HEVC +2, AV1 +3.
fn cq_offset(codec: Codec, encoder_name: &str) -> u8 {
    match codec {
        Codec::H264 => 0,
        Codec::Hevc if encoder_name == "libx265" => 5,
        Codec::Hevc => 2,
        Codec::Av1 => 3,
    }
}

/// Rate control for the recorder. Non-custom presets use quality-based VBR:
/// the preset's quality value with the scaled preset bitrate as the peak cap
/// (`cap_kbps`), so static scenes spend few bits and busy scenes stay under the
/// cap. Custom maps `video.rate_control`: quality uses `quality_cq`, CBR uses
/// `bitrate_kbps`, and VBR uses `bitrate_kbps` as the average with
/// `max_bitrate_kbps` as the peak. `cap_kbps` and `encoder_name` are only used
/// for presets.
fn rate_mode_of(video: &Video, codec: Codec, cap_kbps: u32, encoder_name: &str) -> RateMode {
    if let Some(base) = preset_base_cq(video.preset) {
        let cq = (base + cq_offset(codec, encoder_name)).min(51);
        return RateMode::QualityPeak {
            cq,
            max_kbps: cap_kbps,
        };
    }
    match video.rate_control {
        RateControl::Quality => RateMode::Quality {
            cq: video.quality_cq,
        },
        RateControl::Cbr => RateMode::Cbr,
        RateControl::Vbr => RateMode::Vbr {
            max_kbps: video.max_bitrate_kbps,
        },
    }
}

/// Encoder backend implied by a GPU adapter name, from vendor words in the
/// name (case-insensitive). `None` for unknown names, including the Microsoft
/// Basic Render Driver.
fn backend_for_gpu_name(name: &str) -> Option<Backend> {
    let lower = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has(&["nvidia", "geforce", "rtx", "gtx"]) {
        Some(Backend::Nvenc)
    } else if has(&["amd", "radeon"]) {
        Some(Backend::Amf)
    } else if has(&["intel"]) {
        Some(Backend::Qsv)
    } else {
        None
    }
}

/// Encoder preference from `video.encoder` and `video.gpu`.
///
/// An explicit `video.encoder` always wins. With `Auto`, a GPU name that
/// names a vendor selects that vendor's backend. A missing or `auto` GPU
/// leaves the choice to the selector. A forced backend that does not work
/// still falls back in `select::pick`.
fn preference_for(video: &Video) -> Preference {
    if video.encoder != EncoderChoice::Auto {
        return preference(video.encoder);
    }
    let gpu = video.gpu.trim();
    if gpu.is_empty() || gpu.eq_ignore_ascii_case("auto") {
        return Preference::Auto;
    }
    match backend_for_gpu_name(gpu) {
        Some(backend) => Preference::Force(backend),
        None => Preference::Auto,
    }
}

/// Mic endpoint for capture. `None` means the default capture device, which
/// is used when the setting is empty or `default`.
fn mic_device_of(device: &str) -> Option<String> {
    let trimmed = device.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        None
    } else {
        Some(device.to_owned())
    }
}

/// How the shared mic gate affects one microphone edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeGate {
    /// Always recorded.
    Open,
    /// Silenced by the toggle mute key only.
    MuteOnly,
    /// Silenced by the mute key, and open only while a push-to-talk key is held.
    PttAndMute,
}

/// Capture source for one track source. `gate` selects the gated microphone kinds for `Mic`
/// and `MicPtt`; other sources ignore it.
fn source_kind_of(source: &SourceRef, mic_device: Option<&str>, gate: EdgeGate) -> AudioSourceKind {
    match source {
        SourceRef::Game => AudioSourceKind::GameProcess,
        SourceRef::Mic | SourceRef::MicPtt if gate == EdgeGate::PttAndMute => {
            AudioSourceKind::MicrophoneGated {
                device: mic_device.map(str::to_owned),
            }
        }
        SourceRef::Mic | SourceRef::MicPtt if gate == EdgeGate::MuteOnly => {
            AudioSourceKind::MicrophoneMuted {
                device: mic_device.map(str::to_owned),
            }
        }
        SourceRef::Mic | SourceRef::MicPtt => AudioSourceKind::Microphone {
            device: mic_device.map(str::to_owned),
        },
        SourceRef::OtherApps => AudioSourceKind::OtherApps,
        SourceRef::App(exe) => AudioSourceKind::App {
            exe: exe.to_string(),
        },
    }
}

/// Which mic gates are in effect, from `hotkeys.session` (see [`mic_gating`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MicGating {
    /// The toggle mute key is bound (`mute_mic` with `mute_mic_mode = toggle`).
    pub mute_key: bool,
    /// At least one push-to-talk key is bound (`push_to_talk`, or `mute_mic` with
    /// `mute_mic_mode = push_to_talk`).
    pub ptt: bool,
}

/// Mic gating from `hotkeys.session`. The toggle mute key is `mute_mic` in toggle mode.
/// Push-to-talk keys are the `push_to_talk` list plus `mute_mic` in push-to-talk mode
/// (the legacy setting).
pub fn mic_gating(cfg: &Config) -> MicGating {
    let session = &cfg.hotkeys.session;
    let legacy_ptt = session.mute_mic.is_some() && session.mute_mic_mode == MuteMode::PushToTalk;
    MicGating {
        mute_key: session.mute_mic.is_some() && session.mute_mic_mode == MuteMode::Toggle,
        ptt: legacy_ptt || !session.push_to_talk.is_empty(),
    }
}

/// How the mic gate affects the microphone edge of `source` in track `track_index` (see
/// [`audio_tracks_gated`]).
fn mic_gate_of(
    source: &SourceRef,
    track_index: usize,
    audio: &Audio,
    gating: MicGating,
) -> EdgeGate {
    let any = gating.ptt || gating.mute_key;
    match source {
        // With a separate push-to-talk track, the standalone Microphone track ignores push to
        // talk (always open) but a toggle mute still silences it.
        SourceRef::Mic
            if gating.ptt
                && ((audio.mic.ptt_separate_track && track_index > 0)
                    || (track_index == 0 && !audio.mic.ptt_mix_gated)) =>
        {
            if gating.mute_key {
                EdgeGate::MuteOnly
            } else {
                EdgeGate::Open
            }
        }
        SourceRef::Mic | SourceRef::MicPtt if any => EdgeGate::PttAndMute,
        _ => EdgeGate::Open,
    }
}

/// Recorded audio tracks from `[audio]`, in the order of
/// [`Audio::track_plan`]. Each source gets its gain. The microphone also gets
/// the noise gate when `audio.mic.gate_enabled` is set. Each track uses
/// `audio.bitrate_kbps`. Tracks with no sources (a silent Mix when nothing is
/// enabled) are dropped, so nothing is recorded in that case.
pub fn audio_tracks_of(audio: &Audio) -> Vec<AudioTrackConfig> {
    audio_tracks_gated(audio, MicGating::default())
}

/// [`audio_tracks_of`] with the mic gates in effect (see [`mic_gating`]). A gated mic edge
/// is `AudioSourceKind::MicrophoneGated`, which the shared `MicGate` opens and closes.
/// With no gate every mic edge is `Microphone`.
///
/// Gating rules. With a push-to-talk key the mic is gated in the Mix track and the
/// `Microphone` track. The exception is `audio.mic.ptt_separate_track`: the separate
/// `Microphone` track then stays open, unless a toggle mute key is also bound. `Push to
/// talk` (`SourceRef::MicPtt`) is gated whenever any gate is set. The track indexes come
/// from the full [`Audio::track_plan`], before empty tracks are dropped.
pub fn audio_tracks_gated(audio: &Audio, gating: MicGating) -> Vec<AudioTrackConfig> {
    let mic_device = mic_device_of(&audio.mic.device);
    audio
        .track_plan()
        .into_iter()
        .enumerate()
        .filter(|(_, track)| !track.sources.is_empty())
        .map(|(index, track)| AudioTrackConfig {
            title: track.title,
            sources: track
                .sources
                .iter()
                .map(|source| TrackSource {
                    kind: source_kind_of(
                        source,
                        mic_device.as_deref(),
                        mic_gate_of(source, index, audio, gating),
                    ),
                    gain_db: audio.gain_for(source),
                    gate_db: match source {
                        SourceRef::Mic | SourceRef::MicPtt if audio.mic.gate_enabled => {
                            Some(audio.mic.gate_db)
                        }
                        _ => None,
                    },
                })
                .collect(),
            bitrate_kbps: audio.bitrate_kbps,
        })
        .collect()
}

/// Rounds kbps to the nearest 100.
fn round_100(kbps: u32) -> u32 {
    (kbps + 50) / 100 * 100
}

/// Expands `%NAME%` references using `lookup`.
///
/// Unknown names, empty names (`%%`) and an unclosed `%` are kept verbatim.
/// The lookup is called only with non-empty names.
pub fn expand_vars_with<F>(input: &str, lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find('%') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('%') else {
            break;
        };
        let name = &after_open[..close];
        out.push_str(&rest[..open]);
        let value = if name.is_empty() { None } else { lookup(name) };
        match value {
            Some(v) => out.push_str(&v),
            None => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after_open[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Expands `%VARS%` in a path using `lookup`.
fn expand_path<F>(path: &Path, lookup: &F) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    PathBuf::from(expand_vars_with(&path.to_string_lossy(), lookup))
}

/// Builds the recorder configuration from the settings, reading the process
/// environment for `%VAR%` expansion.
///
/// `data_dir` is the daemon data directory. It is the parent of the buffer
/// folder when `replay.location` is empty, and of the recordings folder when
/// `storage.root` is empty. Nothing is created on disk.
pub fn build_recorder_config(
    cfg: &Config,
    probe: &ProbeReport,
    data_dir: &Path,
) -> Result<(RecorderConfig, EncoderChoiceInfo), MapError> {
    build_recorder_config_with(cfg, probe, data_dir, |name| env::var(name).ok())
}

/// [`build_recorder_config`] with an explicit environment lookup.
///
/// Mapping rules:
/// - Encoder: `[video] encoder` as the backend, or, when it is `auto`, the
///   vendor in `[video] gpu` (see [`backend_for_gpu_name`]). `[video] codec` is
///   the family.
/// - Preset (`low`, `medium`, `high`, `ultra`): resolution, fps and bitrate
///   come from [`Preset::values`]. `[video] resolution` is ignored.
/// - Custom: `[video] resolution` (`native` is 2160p), `[video] fps`, and
///   `[video] bitrate_kbps` as is.
/// - Canvas: height from the rule above. The width is not known here: the 16:9
///   canvas is only the fallback. `canvas_height` is 0 for a Custom `native`
///   resolution, so the recorder takes the width from the window; otherwise it is
///   the height.
/// - Bitrate: the preset target scaled by codec (HEVC x0.65, AV1 x0.5) and
///   rounded to 100 kbps. Custom uses `[video] bitrate_kbps` without scaling.
/// - Rate control: presets use constant quality (`low` 32, `medium` 29, `high`
///   26, `ultra` 23 on H.264, plus the codec offset) with the scaled preset
///   bitrate as the peak cap. Custom uses `[video] rate_control` (see
///   [`rate_mode_of`]).
/// - Speed and B-frames: `[video] encoder_speed` and `[video] b_frames`.
/// - GPU index: always `None`. The probe report has no adapter list, so the
///   NVENC `-gpu` index cannot be worked out here. The driver chooses.
/// - Audio: [`audio_tracks_gated`] from `[audio]`, with [`mic_gating`] from
///   `hotkeys.session` so the mic is gated when a push-to-talk or mute key is bound.
/// - Buffer: `replay.enabled` and `replay.max_seconds`. The folder is
///   `replay.location`, or `<data_dir>/buffer` when that is empty.
/// - Clips and sessions: `storage.root\Clips` and `storage.root\Sessions` base
///   folders. An empty root becomes `<data_dir>\recordings`. With
///   `storage.per_game_folders` the recorder adds one `<Game>` folder below each.
/// - Session auto start: `detection.auto_record` and `detection.record_sessions`.
pub fn build_recorder_config_with<F>(
    cfg: &Config,
    probe: &ProbeReport,
    data_dir: &Path,
    lookup: F,
) -> Result<(RecorderConfig, EncoderChoiceInfo), MapError>
where
    F: Fn(&str) -> Option<String>,
{
    let video = &cfg.video;
    let (selection, codec) =
        select_encoder(probe, video.codec, &preference_for(video)).ok_or(MapError::NoEncoder {
            codec: video.codec,
            encoder: video.encoder,
        })?;

    // `canvas_height` is 0 only for Custom `native`: the width then follows the
    // window. Presets always pass their nominal height.
    let (height, canvas_height, fps, bitrate_kbps) = match video.preset.values() {
        Some(values) => (
            resolve_height(values.resolution, NATIVE_CAP_HEIGHT),
            resolve_height(values.resolution, NATIVE_CAP_HEIGHT),
            u32::from(u16::from(values.fps)),
            round_100(values.bitrate_kbps * codec_scale_permille(codec) / 1000),
        ),
        None => {
            let height = resolve_height(video.resolution, NATIVE_CAP_HEIGHT);
            let canvas_height = if video.resolution == Resolution::Native {
                0
            } else {
                height
            };
            (
                height,
                canvas_height,
                u32::from(u16::from(video.fps)),
                video.bitrate_kbps,
            )
        }
    };
    let canvas = canvas_for_height(height);
    let tuning = EncoderTuning {
        rate: rate_mode_of(video, codec, bitrate_kbps, &selection.encoder.encoder_name),
        speed: speed_of(video.encoder_speed),
        b_frames: video.b_frames,
        gpu_index: None,
    };

    let replay = &cfg.replay;
    let work_dir = if replay.location.as_os_str().is_empty() {
        data_dir.join("buffer")
    } else {
        expand_path(&replay.location, &lookup)
    };
    let root = if cfg.storage.root.as_os_str().is_empty() {
        data_dir.join("recordings")
    } else {
        expand_path(&cfg.storage.root, &lookup)
    };

    let recorder = RecorderConfig {
        encoder_name: selection.encoder.encoder_name.clone(),
        fps,
        canvas,
        canvas_height,
        bitrate_kbps,
        buffer_enabled: replay.enabled,
        buffer_max_secs: f64::from(replay.max_seconds),
        work_dir,
        clips_dir: root.join("Clips"),
        sessions_dir: root.join("Sessions"),
        per_game_folders: cfg.storage.per_game_folders,
        session_auto_start: cfg.detection.auto_record && cfg.detection.record_sessions,
        audio_tracks: audio_tracks_gated(&cfg.audio, mic_gating(cfg)),
        tuning,
        no_border_capture: video.no_border_capture,
    };
    let info = EncoderChoiceInfo {
        encoder_name: selection.encoder.encoder_name,
        fell_back_from: selection.fell_back_from.map(|b| backend_name(b).to_owned()),
    };
    Ok((recorder, info))
}

/// Converts clip and session hotkeys into bindings.
///
/// Clips come first in config order, then session split, start/stop, buffer
/// toggle and mic mute, then each `push_to_talk` chord. Unbound session keys are
/// skipped silently. A chord that cannot be turned into a [`Chord`] is skipped and a
/// message is added to the returned warnings. Duplicate chords and clip limits are
/// left to `HotkeyRegistry::build`.
///
/// `mute_mic_mode = push_to_talk` (legacy) maps the mute key to
/// [`HotkeyAction::PushToTalk`], which the hotkey service reports on press and on
/// release. `toggle` (the default) maps it to [`HotkeyAction::MuteMic`]. Each
/// `push_to_talk` chord is a [`HotkeyAction::PushToTalk`] binding.
///
/// Each clip maps its `press` (`tap` or `long`) to the binding's [`PressKind`]. Session
/// bindings are always `Tap`. A clip chord shared by a tap clip and a long clip is kept
/// (both bindings are returned); other duplicates are rejected by `HotkeyRegistry::build`.
pub fn clip_hotkey_bindings(cfg: &Config) -> (Vec<Binding>, Vec<String>) {
    let mut bindings = Vec::new();
    let mut warnings = Vec::new();
    let hotkeys = &cfg.hotkeys;

    for clip in &hotkeys.clips {
        let label = format!("clip hotkey '{}' ({})", clip.name, clip.press);
        if let Some(chord) = to_chord(&clip.keys, &label, &mut warnings) {
            let press = match clip.press {
                Press::Tap => PressKind::Tap,
                Press::Long => PressKind::Long,
            };
            bindings.push(Binding {
                chord,
                action: HotkeyAction::ClipLast {
                    name: clip.name.clone(),
                    seconds: clip.seconds,
                },
                press,
                enabled: true,
            });
        }
    }

    let session = &hotkeys.session;
    let (mute_action, mute_label) = match session.mute_mic_mode {
        MuteMode::Toggle => (HotkeyAction::MuteMic, "mute microphone"),
        MuteMode::PushToTalk => (HotkeyAction::PushToTalk, "push to talk"),
    };
    let session_keys = [
        (session.split, HotkeyAction::SessionSplit, "session split"),
        (
            session.stop_start,
            HotkeyAction::SessionStopStart,
            "session start/stop",
        ),
        (
            session.toggle_buffer,
            HotkeyAction::ToggleBuffer,
            "toggle buffer",
        ),
        (session.mute_mic, mute_action, mute_label),
    ];
    for (keys, action, label) in session_keys {
        let Some(keys) = keys else {
            continue;
        };
        if let Some(chord) = to_chord(&keys, label, &mut warnings) {
            bindings.push(Binding {
                chord,
                action,
                press: PressKind::Tap,
                enabled: true,
            });
        }
    }

    for keys in &session.push_to_talk {
        if let Some(chord) = to_chord(keys, "push to talk", &mut warnings) {
            bindings.push(Binding {
                chord,
                action: HotkeyAction::PushToTalk,
                press: PressKind::Tap,
                enabled: true,
            });
        }
    }

    (bindings, warnings)
}

/// Converts a config chord to a hotkeys chord via its canonical text form.
fn to_chord(keys: &KeyChord, label: &str, warnings: &mut Vec<String>) -> Option<Chord> {
    // `KeyChord`'s Display never fails (it writes a `VK 0x..` fallback for unnamed keys,
    // which the parser then rejects below).
    let text = keys.to_string();
    match text.parse::<Chord>() {
        Ok(chord) => Some(chord),
        Err(err) => {
            warnings.push(format!("{label}: invalid hotkey '{text}' skipped: {err}"));
            None
        }
    }
}

#[cfg(test)]
mod tests;
