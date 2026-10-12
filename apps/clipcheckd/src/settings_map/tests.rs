//! Unit tests for `settings_map`. Written by the tester agent (task D1a).

use super::*;
use clipcheck_core::config::schema::{AudioApp, ExeName, Fps, Modifiers, MuteMode};
use clipcheck_encode::probe::EncoderInfo;
use std::cell::RefCell;

/// Lookup that knows no variables.
fn no_env(_: &str) -> Option<String> {
    None
}

fn probe(encoders: &[(Backend, CodecKind, &str, bool)]) -> ProbeReport {
    ProbeReport {
        build: "test-build".to_owned(),
        encoders: encoders
            .iter()
            .map(|&(backend, codec, name, works)| EncoderInfo {
                backend,
                codec,
                encoder_name: name.to_owned(),
                works,
                error: if works {
                    None
                } else {
                    Some("failed".to_owned())
                },
                probe_ms: 1,
            })
            .collect(),
    }
}

/// Every H.264 and HEVC software encoder, and every NVENC encoder, working.
fn full_probe() -> ProbeReport {
    probe(&[
        (Backend::Software, CodecKind::H264, "libx264", true),
        (Backend::Software, CodecKind::H265, "libx265", true),
        (Backend::Nvenc, CodecKind::H264, "h264_nvenc", true),
        (Backend::Nvenc, CodecKind::H265, "hevc_nvenc", true),
        (Backend::Nvenc, CodecKind::Av1, "av1_nvenc", true),
    ])
}

fn data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn video_cfg(
    codec: Codec,
    preset: Preset,
    resolution: Resolution,
    encoder: EncoderChoice,
) -> Config {
    let mut cfg = Config::default();
    cfg.video.codec = codec;
    cfg.video.preset = preset;
    cfg.video.resolution = resolution;
    cfg.video.encoder = encoder;
    cfg
}

fn build(cfg: &Config, probe: &ProbeReport) -> (RecorderConfig, EncoderChoiceInfo) {
    build_recorder_config_with(cfg, probe, &data_dir(), no_env).expect("build should succeed")
}

// ---------------------------------------------------------------- preset table

#[test]
fn preset_codec_table_auto_native() {
    // (preset, canvas, fps, bitrates for [H264, HEVC, AV1])
    type Row = (Preset, (u32, u32), u32, [u32; 3]);
    const PRESETS: [Row; 4] = [
        (Preset::Low, (854, 480), 30, [4000, 2600, 2000]),
        (Preset::Medium, (1280, 720), 60, [8000, 5200, 4000]),
        (Preset::High, (1920, 1080), 60, [14000, 9100, 7000]),
        (Preset::Ultra, (2560, 1440), 60, [30000, 19500, 15000]),
    ];
    // (codec, expected encoder name when NVENC is working, index of the bitrate column).
    // An AV1 request uses HEVC (no native AV1), so it reads the HEVC column.
    const CODECS: [(Codec, &str, usize); 3] = [
        (Codec::H264, "h264_nvenc", 0),
        (Codec::Hevc, "hevc_nvenc", 1),
        (Codec::Av1, "hevc_nvenc", 1),
    ];

    let probe = full_probe();
    for (preset, canvas, fps, bitrates) in PRESETS {
        for (codec, enc_name, column) in CODECS {
            let cfg = video_cfg(codec, preset, Resolution::Native, EncoderChoice::Auto);
            let (rc, info) = build(&cfg, &probe);
            let label = format!("{preset:?}/{codec:?}");
            assert_eq!(info.encoder_name, enc_name, "encoder for {label}");
            assert_eq!(info.fell_back_from, None, "fallback for {label}");
            assert_eq!(rc.encoder_name, enc_name, "recorder encoder for {label}");
            assert_eq!(rc.canvas, canvas, "canvas for {label}");
            assert_eq!(rc.fps, fps, "fps for {label}");
            assert_eq!(rc.bitrate_kbps, bitrates[column], "bitrate for {label}");
        }
    }
}

#[test]
fn auto_prefers_nvenc_for_h264_when_software_also_works() {
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    let (_, info) = build(&cfg, &full_probe());
    assert_eq!(info.encoder_name, "h264_nvenc");
}

// ------------------------------------------------------------------ custom

#[test]
fn custom_uses_explicit_fps_and_bitrate() {
    let mut cfg = video_cfg(
        Codec::Hevc,
        Preset::Custom,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.video.fps = Fps::F120;
    cfg.video.bitrate_kbps = 15000;
    let (rc, info) = build(&cfg, &full_probe());
    assert_eq!(rc.fps, 120);
    assert_eq!(rc.bitrate_kbps, 15000, "custom bitrate is not codec-scaled");
    assert_eq!(rc.canvas, (3840, 2160), "native under custom caps at 2160");
    assert_eq!(info.encoder_name, "hevc_nvenc");
}

#[test]
fn custom_fps_30_and_60_pass_through() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::Custom,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.video.bitrate_kbps = 8000;
    cfg.video.fps = Fps::F30;
    assert_eq!(build(&cfg, &full_probe()).0.fps, 30);
    cfg.video.fps = Fps::F60;
    assert_eq!(build(&cfg, &full_probe()).0.fps, 60);
}

#[test]
fn custom_with_720p_resolution() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::Custom,
        Resolution::P720,
        EncoderChoice::Auto,
    );
    cfg.video.bitrate_kbps = 5000;
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.canvas, (1280, 720));
}

// ------------------------------------------------------- resolution override

#[test]
fn high_preset_with_720p_keeps_preset_fps() {
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::P720,
        EncoderChoice::Auto,
    );
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.canvas, (1920, 1080), "preset ignores resolution 720p");
    assert_eq!(rc.fps, 60);
}

#[test]
fn low_preset_with_1080p_keeps_preset_fps() {
    let cfg = video_cfg(
        Codec::H264,
        Preset::Low,
        Resolution::P1080,
        EncoderChoice::Auto,
    );
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.canvas, (854, 480), "preset ignores resolution 1080p");
    assert_eq!(rc.fps, 30);
}

#[test]
fn ultra_preset_with_1080p_ignores_resolution() {
    let cfg = video_cfg(
        Codec::Av1,
        Preset::Ultra,
        Resolution::P1080,
        EncoderChoice::Auto,
    );
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.canvas, (2560, 1440), "preset ignores resolution 1080p");
    assert_eq!(
        rc.bitrate_kbps, 19500,
        "ultra AV1 request uses HEVC: 30000 * 0.65, not resolution dependent"
    );
}

// ------------------------------------------------------ forced encoder choice

#[test]
fn forced_software_available_uses_libx264() {
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Software,
    );
    let (rc, info) = build(&cfg, &full_probe());
    assert_eq!(info.encoder_name, "libx264");
    assert_eq!(info.fell_back_from, None);
    assert_eq!(rc.encoder_name, "libx264");
}

#[test]
fn forced_nvenc_available_uses_h264_nvenc() {
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Nvenc,
    );
    let (_, info) = build(&cfg, &full_probe());
    assert_eq!(info.encoder_name, "h264_nvenc");
    assert_eq!(info.fell_back_from, None);
}

#[test]
fn forced_amf_unavailable_falls_back_to_nvenc() {
    let p = probe(&[
        (Backend::Amf, CodecKind::H264, "h264_amf", false),
        (Backend::Nvenc, CodecKind::H264, "h264_nvenc", true),
    ]);
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Amf,
    );
    let (_, info) = build(&cfg, &p);
    assert_eq!(info.encoder_name, "h264_nvenc");
    assert_eq!(info.fell_back_from, Some("amf".to_owned()));
}

#[test]
fn forced_software_unavailable_falls_back_to_nvenc() {
    let p = probe(&[
        (Backend::Software, CodecKind::H264, "libx264", false),
        (Backend::Nvenc, CodecKind::H264, "h264_nvenc", true),
    ]);
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Software,
    );
    let (_, info) = build(&cfg, &p);
    assert_eq!(info.encoder_name, "h264_nvenc");
    assert_eq!(info.fell_back_from, Some("software".to_owned()));
}

// ------------------------------------------------------------ AV1 rules

#[test]
fn av1_software_only_is_no_encoder_for_auto_and_software() {
    let p = probe(&[(Backend::Software, CodecKind::Av1, "libsvtav1", true)]);

    let auto = video_cfg(
        Codec::Av1,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    assert_eq!(
        build_recorder_config_with(&auto, &p, &data_dir(), no_env).err(),
        Some(MapError::NoEncoder {
            codec: Codec::Av1,
            encoder: EncoderChoice::Auto,
        })
    );

    let sw = video_cfg(
        Codec::Av1,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Software,
    );
    assert_eq!(
        build_recorder_config_with(&sw, &p, &data_dir(), no_env).err(),
        Some(MapError::NoEncoder {
            codec: Codec::Av1,
            encoder: EncoderChoice::Software,
        })
    );
}

#[test]
fn av1_request_uses_hevc_encoder_and_never_av1() {
    // An AV1 encoder that works is ignored: the request resolves to HEVC.
    let p = probe(&[
        (Backend::Nvenc, CodecKind::Av1, "av1_nvenc", true),
        (Backend::Qsv, CodecKind::Av1, "av1_qsv", true),
        (Backend::Qsv, CodecKind::H265, "hevc_qsv", true),
    ]);
    let cfg = video_cfg(
        Codec::Av1,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    let (rc, info) = build(&cfg, &p);
    assert_eq!(info.encoder_name, "hevc_qsv");
    assert_eq!(rc.encoder_name, "hevc_qsv");
    assert_eq!(info.fell_back_from, None);
}

#[test]
fn av1_request_falls_back_to_h264_without_hevc() {
    let p = probe(&[(Backend::Nvenc, CodecKind::H264, "h264_nvenc", true)]);
    let cfg = video_cfg(
        Codec::Av1,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    let (rc, info) = build(&cfg, &p);
    assert_eq!(info.encoder_name, "h264_nvenc");
    // The bitrate follows the codec used (H.264, no scale), not the request.
    assert_eq!(rc.bitrate_kbps, 14000);
}

// ------------------------------------------------------- errors

#[test]
fn empty_probe_is_no_encoder() {
    let p = probe(&[]);
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    assert_eq!(
        build_recorder_config_with(&cfg, &p, &data_dir(), no_env).err(),
        Some(MapError::NoEncoder {
            codec: Codec::H264,
            encoder: EncoderChoice::Auto,
        })
    );
}

#[test]
fn builds_recorder_config() {
    let cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    let (_rc, info) = build_recorder_config_with(&cfg, &full_probe(), &data_dir(), no_env)
        .expect("builds a recorder config");
    assert_eq!(info.encoder_name, "h264_nvenc");
}

#[test]
fn map_error_display_strings() {
    let no_enc = MapError::NoEncoder {
        codec: Codec::Av1,
        encoder: EncoderChoice::Qsv,
    }
    .to_string();
    assert!(!no_enc.is_empty());
    assert!(no_enc.contains("no working encoder"), "got: {no_enc}");
}

// ------------------------------------------------------- env expansion

#[test]
fn expand_known_variable() {
    let out = expand_vars_with(r"%USERPROFILE%\Videos", |n| {
        (n == "USERPROFILE").then(|| r"C:\Users\x".to_owned())
    });
    assert_eq!(out, r"C:\Users\x\Videos");
}

#[test]
fn expand_unknown_variable_is_kept() {
    let out = expand_vars_with(r"%NOPE%\a", no_env);
    assert_eq!(out, r"%NOPE%\a");
}

#[test]
fn expand_double_percent_is_kept() {
    assert_eq!(expand_vars_with("%%", no_env), "%%");
}

#[test]
fn expand_unclosed_percent_is_kept() {
    assert_eq!(expand_vars_with("abc%def", no_env), "abc%def");
}

#[test]
fn expand_two_variables() {
    let out = expand_vars_with("%A%-%B%", |n| match n {
        "A" => Some("1".to_owned()),
        "B" => Some("2".to_owned()),
        _ => None,
    });
    assert_eq!(out, "1-2");
}

#[test]
fn expand_empty_input() {
    assert_eq!(expand_vars_with("", no_env), "");
}

#[test]
fn expand_without_percent_is_unchanged() {
    assert_eq!(expand_vars_with(r"C:\plain\path", no_env), r"C:\plain\path");
}

#[test]
fn expand_never_calls_lookup_with_empty_name() {
    let seen = RefCell::new(Vec::<String>::new());
    let out = expand_vars_with("%%a%%", |n| {
        seen.borrow_mut().push(n.to_owned());
        None
    });
    assert_eq!(out, "%%a%%");
    assert!(
        seen.borrow().is_empty(),
        "lookup called with: {:?}",
        seen.borrow()
    );
}

#[test]
fn expand_calls_lookup_with_name() {
    let seen = RefCell::new(Vec::<String>::new());
    let _ = expand_vars_with("x%ABC%y", |n| {
        seen.borrow_mut().push(n.to_owned());
        None
    });
    assert_eq!(*seen.borrow(), vec!["ABC".to_owned()]);
}

// ------------------------------------------- paths through build_recorder_config_with

#[test]
fn storage_root_is_expanded_and_joined() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.storage.root = PathBuf::from("%APPDATA%\\CC");
    let lookup = |n: &str| (n == "APPDATA").then(|| "D:\\AD".to_owned());
    let (rc, _) = build_recorder_config_with(&cfg, &full_probe(), &data_dir(), lookup)
        .expect("build should succeed");
    let root = PathBuf::from(r"D:\AD\CC");
    assert_eq!(rc.clips_dir, root.join("Clips"));
    assert_eq!(rc.sessions_dir, root.join("Sessions"));
}

#[test]
fn per_game_folders_flag_is_passed_through() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.storage.per_game_folders = true;
    let (rc, _) = build(&cfg, &full_probe());
    assert!(rc.per_game_folders);
    cfg.storage.per_game_folders = false;
    let (rc, _) = build(&cfg, &full_probe());
    assert!(!rc.per_game_folders);
}

#[test]
fn replay_location_is_expanded_to_work_dir() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.replay.location = PathBuf::from(r"%LOCALAPPDATA%\ClipCheck\buffer");
    let lookup = |n: &str| (n == "LOCALAPPDATA").then(|| r"E:\L".to_owned());
    let (rc, _) = build_recorder_config_with(&cfg, &full_probe(), &data_dir(), lookup)
        .expect("build should succeed");
    assert_eq!(rc.work_dir, PathBuf::from(r"E:\L\ClipCheck\buffer"));
}

#[test]
fn empty_replay_location_uses_data_dir_buffer() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.replay.location = PathBuf::new();
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.work_dir, data_dir().join("buffer"));
}

#[test]
fn empty_storage_root_uses_data_dir_recordings() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.storage.root = PathBuf::new();
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.clips_dir, data_dir().join("recordings").join("Clips"));
    assert_eq!(
        rc.sessions_dir,
        data_dir().join("recordings").join("Sessions")
    );
}

// ------------------------------------------- buffer and session fields

#[test]
fn replay_enabled_and_max_seconds_are_mapped() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.replay.enabled = false;
    cfg.replay.max_seconds = 90;
    let (rc, _) = build(&cfg, &full_probe());
    assert!(!rc.buffer_enabled);
    assert_eq!(rc.buffer_max_secs, 90.0);

    cfg.replay.enabled = true;
    let (rc, _) = build(&cfg, &full_probe());
    assert!(rc.buffer_enabled);
}

#[test]
fn auto_record_maps_to_session_auto_start() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.detection.auto_record = true;
    assert!(build(&cfg, &full_probe()).0.session_auto_start);
    cfg.detection.auto_record = false;
    assert!(!build(&cfg, &full_probe()).0.session_auto_start);
}

#[test]
fn record_sessions_off_disables_session_auto_start_but_keeps_buffer() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.detection.auto_record = true;
    cfg.replay.enabled = true;
    cfg.detection.record_sessions = false;
    let rc = build(&cfg, &full_probe()).0;
    assert!(!rc.session_auto_start);
    assert!(rc.buffer_enabled);
    cfg.detection.record_sessions = true;
    assert!(build(&cfg, &full_probe()).0.session_auto_start);
}

// ------------------------------------------------------- rounding

#[test]
fn canvas_for_height_is_even_in_both_dimensions() {
    for h in [720u32, 1080, 1440, 2160, 719, 1081] {
        let (w, hh) = canvas_for_height(h);
        assert_eq!(w % 2, 0, "width for height {h} is odd: {w}");
        assert_eq!(hh % 2, 0, "height {h} is odd: {hh}");
    }
    assert_eq!(canvas_for_height(1080), (1920, 1080));
}

#[test]
fn round_100_rounds_to_nearest_hundred() {
    assert_eq!(round_100(3899), 3900);
    assert_eq!(round_100(3850), 3900);
    assert_eq!(round_100(3849), 3800);
}

// ------------------------------------------------------- hotkeys

fn is_clip(b: &Binding, name: &str, secs: u32) -> bool {
    matches!(&b.action, HotkeyAction::ClipLast { name: n, seconds } if n == name && *seconds == secs)
}

#[test]
fn default_hotkeys_convert_in_order() {
    let (bindings, warnings) = clip_hotkey_bindings(&Config::default());
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    assert_eq!(bindings.len(), 6);

    assert!(is_clip(&bindings[0], "Clip 30s", 30));
    assert!(is_clip(&bindings[1], "Clip 60s", 60));
    assert_eq!(bindings[0].chord.to_string(), "Alt+F9");
    assert_eq!(bindings[1].chord.to_string(), "Alt+F10");

    assert!(matches!(bindings[2].action, HotkeyAction::SessionSplit));
    assert_eq!(bindings[2].chord.to_string(), "Alt+F11");
    assert!(matches!(bindings[3].action, HotkeyAction::SessionStopStart));
    assert_eq!(bindings[3].chord.to_string(), "Alt+F12");
    assert!(matches!(bindings[4].action, HotkeyAction::ToggleBuffer));
    assert_eq!(bindings[4].chord.to_string(), "Alt+F8");
    assert!(matches!(bindings[5].action, HotkeyAction::MuteMic));
    assert_eq!(bindings[5].chord.to_string(), "Alt+F7");

    assert!(bindings.iter().all(|b| b.enabled));
    assert!(bindings.iter().all(|b| b.chord.alt));
}

#[test]
fn unbound_session_key_is_skipped_silently() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.split = None;
    cfg.hotkeys.session.mute_mic = None;
    let (bindings, warnings) = clip_hotkey_bindings(&cfg);
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    assert_eq!(bindings.len(), 4);
    assert!(!bindings
        .iter()
        .any(|b| matches!(b.action, HotkeyAction::SessionSplit)));
    assert!(!bindings
        .iter()
        .any(|b| matches!(b.action, HotkeyAction::MuteMic)));
}

#[test]
fn clip_without_printable_key_is_skipped_with_one_warning() {
    let mut cfg = Config::default();
    cfg.hotkeys.clips[0].name = "Bad".to_owned();
    cfg.hotkeys.clips[0].keys = KeyChord {
        mods: Modifiers::default(),
        vk: 0x00,
    };
    let (bindings, warnings) = clip_hotkey_bindings(&cfg);
    assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    assert!(warnings[0].contains("Bad"), "warning: {}", warnings[0]);
    assert_eq!(bindings.len(), 5);
    assert!(!bindings.iter().any(|b| is_clip(b, "Bad", 30)));
}

#[test]
fn session_key_without_printable_name_gives_one_warning() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.toggle_buffer = Some(KeyChord {
        mods: Modifiers::default(),
        vk: 0x00,
    });
    let (bindings, warnings) = clip_hotkey_bindings(&cfg);
    assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    assert_eq!(bindings.len(), 5);
    assert!(!bindings
        .iter()
        .any(|b| matches!(b.action, HotkeyAction::ToggleBuffer)));
}

#[test]
fn ctrl_shift_modifiers_are_carried_over() {
    let mut cfg = Config::default();
    cfg.hotkeys.clips[0].keys = KeyChord {
        mods: Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
            win: false,
        },
        vk: 0x78,
    };
    let (bindings, warnings) = clip_hotkey_bindings(&cfg);
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    let chord = &bindings[0].chord;
    assert!(chord.ctrl && chord.shift && !chord.alt);
    assert_eq!(chord.to_string(), "Ctrl+Shift+F9");
}

#[test]
fn clip_hotkeys_come_before_session_hotkeys() {
    let mut cfg = Config::default();
    cfg.hotkeys.clips.truncate(1);
    let (bindings, _) = clip_hotkey_bindings(&cfg);
    assert_eq!(bindings.len(), 5);
    assert!(is_clip(&bindings[0], "Clip 30s", 30));
    assert!(matches!(bindings[1].action, HotkeyAction::SessionSplit));
}

// ------------------------------------------------- preset table (all rows)

/// (preset, canvas, fps, bitrates for [H264, HEVC, AV1]) for the four fixed presets.
type PresetRow = (Preset, (u32, u32), u32, [u32; 3]);
const PRESET_ROWS: [PresetRow; 4] = [
    (Preset::Low, (854, 480), 30, [4000, 2600, 2000]),
    (Preset::Medium, (1280, 720), 60, [8000, 5200, 4000]),
    (Preset::High, (1920, 1080), 60, [14000, 9100, 7000]),
    (Preset::Ultra, (2560, 1440), 60, [30000, 19500, 15000]),
];
const ALL_CODECS: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];
const ALL_RESOLUTIONS: [Resolution; 7] = [
    Resolution::Native,
    Resolution::P360,
    Resolution::P480,
    Resolution::P720,
    Resolution::P1080,
    Resolution::P1440,
    Resolution::P2160,
];
const ALL_RATE_CONTROLS: [RateControl; 3] =
    [RateControl::Quality, RateControl::Cbr, RateControl::Vbr];

/// Expected preset quality per codec, in `ALL_CODECS` order: the H.264 basis
/// (low 32, medium 29, high 26, ultra 23) plus the section 6.3 offset (H.265 +2
/// on NVENC). The AV1 column is the HEVC column: an AV1 request uses HEVC.
/// The auto probe picks NVENC for these codecs.
const PRESET_CQ: [(Preset, [u8; 3]); 4] = [
    (Preset::Low, [32, 34, 34]),
    (Preset::Medium, [29, 31, 31]),
    (Preset::High, [26, 28, 28]),
    (Preset::Ultra, [23, 25, 25]),
];

#[test]
fn every_preset_codec_resolution_and_rate_control_gives_fixed_values_and_quality_peak() {
    let probe = full_probe();
    for (preset, canvas, fps, bitrates) in PRESET_ROWS {
        let cqs = PRESET_CQ
            .iter()
            .find(|(p, _)| *p == preset)
            .map(|(_, c)| *c)
            .expect("every preset row has a cq");
        for (ci, codec) in ALL_CODECS.iter().enumerate() {
            // An AV1 request resolves to HEVC, so it reads the HEVC column.
            let column = if *codec == Codec::Av1 { 1 } else { ci };
            for resolution in ALL_RESOLUTIONS {
                for rate_control in ALL_RATE_CONTROLS {
                    let mut cfg = video_cfg(*codec, preset, resolution, EncoderChoice::Auto);
                    cfg.video.rate_control = rate_control;
                    cfg.video.quality_cq = 35;
                    cfg.video.bitrate_kbps = 1234;
                    cfg.video.max_bitrate_kbps = 9999;
                    let label = format!("{preset:?}/{codec:?}/{resolution:?}/{rate_control:?}");
                    let (rc, _) = build(&cfg, &probe);
                    assert_eq!(rc.canvas, canvas, "canvas for {label}");
                    assert_eq!(rc.fps, fps, "fps for {label}");
                    assert_eq!(rc.bitrate_kbps, bitrates[column], "bitrate for {label}");
                    assert_eq!(
                        rc.tuning.rate,
                        RateMode::QualityPeak {
                            cq: cqs[column],
                            max_kbps: bitrates[column],
                        },
                        "rate for {label}"
                    );
                    assert_eq!(rc.tuning.gpu_index, None, "gpu index for {label}");
                }
            }
        }
    }
}

// ------------------------------------------------- custom mode tables

#[test]
fn custom_resolution_maps_to_canvas_for_every_value() {
    const ROWS: [(Resolution, (u32, u32)); 7] = [
        (Resolution::Native, (3840, 2160)),
        (Resolution::P360, (640, 360)),
        (Resolution::P480, (854, 480)),
        (Resolution::P720, (1280, 720)),
        (Resolution::P1080, (1920, 1080)),
        (Resolution::P1440, (2560, 1440)),
        (Resolution::P2160, (3840, 2160)),
    ];
    for codec in ALL_CODECS {
        for (resolution, canvas) in ROWS {
            let mut cfg = video_cfg(codec, Preset::Custom, resolution, EncoderChoice::Auto);
            cfg.video.bitrate_kbps = 5000;
            let (rc, _) = build(&cfg, &full_probe());
            assert_eq!(rc.canvas, canvas, "canvas for {codec:?}/{resolution:?}");
        }
    }
}

#[test]
fn custom_native_sets_canvas_height_zero() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::Custom,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.video.bitrate_kbps = 5000;
    let (rc, _) = build(&cfg, &full_probe());
    assert_eq!(rc.canvas_height, 0);
    assert_eq!(rc.canvas, (3840, 2160));
}

#[test]
fn custom_fps_maps_to_integer_for_every_value() {
    const ROWS: [(Fps, u32); 6] = [
        (Fps::F24, 24),
        (Fps::F30, 30),
        (Fps::F60, 60),
        (Fps::F90, 90),
        (Fps::F120, 120),
        (Fps::F144, 144),
    ];
    for (fps, expected) in ROWS {
        let mut cfg = video_cfg(
            Codec::H264,
            Preset::Custom,
            Resolution::P720,
            EncoderChoice::Auto,
        );
        cfg.video.fps = fps;
        cfg.video.bitrate_kbps = 5000;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.fps, expected, "fps for {fps:?}");
    }
}

#[test]
fn custom_rate_control_maps_to_rate_mode() {
    for codec in ALL_CODECS {
        let mut cfg = video_cfg(
            codec,
            Preset::Custom,
            Resolution::P1080,
            EncoderChoice::Auto,
        );
        cfg.video.bitrate_kbps = 12000;
        cfg.video.max_bitrate_kbps = 16000;
        cfg.video.quality_cq = 18;

        cfg.video.rate_control = RateControl::Quality;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(
            rc.tuning.rate,
            RateMode::Quality { cq: 18 },
            "{codec:?} quality"
        );

        cfg.video.rate_control = RateControl::Cbr;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.tuning.rate, RateMode::Cbr, "{codec:?} cbr");
        assert_eq!(rc.bitrate_kbps, 12000, "{codec:?} cbr bitrate");

        cfg.video.rate_control = RateControl::Vbr;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(
            rc.tuning.rate,
            RateMode::Vbr { max_kbps: 16000 },
            "{codec:?} vbr"
        );
        assert_eq!(rc.bitrate_kbps, 12000, "{codec:?} vbr average bitrate");
    }
}

#[test]
fn rate_mode_of_quality_uses_quality_cq_only() {
    let mut v = Video {
        preset: Preset::Custom,
        rate_control: RateControl::Quality,
        quality_cq: 40,
        ..Video::default()
    };
    assert_eq!(
        rate_mode_of(&v, Codec::H264, 14_000, "h264_nvenc"),
        RateMode::Quality { cq: 40 }
    );
    v.preset = Preset::Ultra;
    assert_eq!(
        rate_mode_of(&v, Codec::H264, 30_000, "h264_nvenc"),
        RateMode::QualityPeak {
            cq: 23,
            max_kbps: 30_000,
        },
        "preset uses quality with the preset bitrate as the peak"
    );
}

#[test]
fn preset_cq_offsets_follow_codec_and_encoder() {
    assert_eq!(cq_offset(Codec::H264, "libx264"), 0);
    assert_eq!(cq_offset(Codec::Hevc, "hevc_nvenc"), 2);
    assert_eq!(cq_offset(Codec::Hevc, "libx265"), 5);
    assert_eq!(cq_offset(Codec::Av1, "av1_nvenc"), 3);
    // High on x265 is 26 + 5. High on AV1 is 26 + 3.
    assert_eq!(
        rate_mode_of(
            &Video {
                preset: Preset::High,
                ..Video::default()
            },
            Codec::Hevc,
            1,
            "libx265"
        ),
        RateMode::QualityPeak {
            cq: 31,
            max_kbps: 1
        }
    );
    assert_eq!(
        rate_mode_of(
            &Video {
                preset: Preset::High,
                ..Video::default()
            },
            Codec::Av1,
            1,
            "av1_nvenc"
        ),
        RateMode::QualityPeak {
            cq: 29,
            max_kbps: 1
        }
    );
}

#[test]
fn speed_mapping_for_every_encoder_speed() {
    const ROWS: [(EncoderSpeed, Speed); 3] = [
        (EncoderSpeed::Fastest, Speed::Fastest),
        (EncoderSpeed::Balanced, Speed::Balanced),
        (EncoderSpeed::Quality, Speed::Quality),
    ];
    for (input, expected) in ROWS {
        assert_eq!(speed_of(input), expected, "speed_of({input:?})");
        let mut cfg = video_cfg(
            Codec::H264,
            Preset::High,
            Resolution::Native,
            EncoderChoice::Auto,
        );
        cfg.video.encoder_speed = input;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.tuning.speed, expected, "tuning speed for {input:?}");
    }
}

#[test]
fn b_frames_are_passed_through_in_preset_and_custom() {
    for b in [0u8, 2, 4] {
        for preset in [Preset::High, Preset::Custom] {
            let mut cfg = video_cfg(Codec::H264, preset, Resolution::Native, EncoderChoice::Auto);
            cfg.video.b_frames = b;
            let (rc, _) = build(&cfg, &full_probe());
            assert_eq!(rc.tuning.b_frames, b, "b_frames for {preset:?}");
        }
    }
}

// ------------------------------------------------- GPU vendor mapping

#[test]
fn backend_for_gpu_name_vendor_table() {
    const ROWS: [(&str, Option<Backend>); 13] = [
        ("NVIDIA GeForce RTX 5080", Some(Backend::Nvenc)),
        ("nvidia geforce gtx 1060", Some(Backend::Nvenc)),
        ("GeForce RTX 4090", Some(Backend::Nvenc)),
        ("AMD Radeon(TM) Graphics", Some(Backend::Amf)),
        ("AMD Ryzen 7 AI Radeon", Some(Backend::Amf)),
        ("Radeon RX 7900 XTX", Some(Backend::Amf)),
        ("Intel(R) Arc(TM) A770", Some(Backend::Qsv)),
        ("Intel(R) UHD Graphics 770", Some(Backend::Qsv)),
        ("Microsoft Basic Render Driver", None),
        ("auto", None),
        ("", None),
        ("Some Unknown Adapter", None),
        ("Virtual Display", None),
    ];
    for (name, expected) in ROWS {
        assert_eq!(backend_for_gpu_name(name), expected, "gpu name {name:?}");
    }
}

#[test]
fn preference_for_auto_encoder_uses_gpu_vendor() {
    const ROWS: [(&str, Preference); 8] = [
        ("auto", Preference::Auto),
        ("  AUTO ", Preference::Auto),
        ("", Preference::Auto),
        ("   ", Preference::Auto),
        ("Microsoft Basic Render Driver", Preference::Auto),
        ("NVIDIA GeForce RTX 5080", Preference::Force(Backend::Nvenc)),
        (
            "  AMD Radeon(TM) Graphics  ",
            Preference::Force(Backend::Amf),
        ),
        ("Intel(R) Arc(TM) A770", Preference::Force(Backend::Qsv)),
    ];
    for (gpu, expected) in ROWS {
        let v = Video {
            encoder: EncoderChoice::Auto,
            gpu: gpu.to_owned(),
            ..Video::default()
        };
        assert_eq!(preference_for(&v), expected, "gpu {gpu:?}");
    }
}

#[test]
fn preference_for_explicit_encoder_ignores_gpu() {
    const ROWS: [(EncoderChoice, Preference); 4] = [
        (EncoderChoice::Nvenc, Preference::Force(Backend::Nvenc)),
        (EncoderChoice::Amf, Preference::Force(Backend::Amf)),
        (EncoderChoice::Qsv, Preference::Force(Backend::Qsv)),
        (
            EncoderChoice::Software,
            Preference::Force(Backend::Software),
        ),
    ];
    for gpu in [
        "AMD Radeon(TM) Graphics",
        "NVIDIA GeForce RTX 5080",
        "auto",
        "",
    ] {
        for (encoder, expected) in ROWS {
            let v = Video {
                encoder,
                gpu: gpu.to_owned(),
                ..Video::default()
            };
            assert_eq!(preference_for(&v), expected, "{encoder:?} with gpu {gpu:?}");
        }
    }
}

#[test]
fn gpu_vendor_selects_encoder_end_to_end() {
    let both = probe(&[
        (Backend::Nvenc, CodecKind::H264, "h264_nvenc", true),
        (Backend::Amf, CodecKind::H264, "h264_amf", true),
        (Backend::Qsv, CodecKind::H264, "h264_qsv", true),
    ]);
    let amf_broken = probe(&[
        (Backend::Nvenc, CodecKind::H264, "h264_nvenc", true),
        (Backend::Amf, CodecKind::H264, "h264_amf", false),
    ]);
    let qsv_broken = probe(&[
        (Backend::Nvenc, CodecKind::H264, "h264_nvenc", true),
        (Backend::Qsv, CodecKind::H264, "h264_qsv", false),
    ]);
    let auto = |gpu: &str| {
        let mut cfg = video_cfg(
            Codec::H264,
            Preset::High,
            Resolution::Native,
            EncoderChoice::Auto,
        );
        cfg.video.gpu = gpu.to_owned();
        cfg
    };

    let (rc, info) = build(&auto("AMD Radeon(TM) Graphics"), &both);
    assert_eq!(info.encoder_name, "h264_amf");
    assert_eq!(info.fell_back_from, None);
    assert_eq!(rc.encoder_name, "h264_amf");

    let (_, info) = build(&auto("Intel(R) Arc(TM) A770"), &both);
    assert_eq!(info.encoder_name, "h264_qsv");
    assert_eq!(info.fell_back_from, None);

    let (_, info) = build(&auto("NVIDIA GeForce RTX 5080"), &both);
    assert_eq!(info.encoder_name, "h264_nvenc");
    assert_eq!(info.fell_back_from, None);

    let (_, info) = build(&auto("Microsoft Basic Render Driver"), &both);
    assert_eq!(
        info.encoder_name, "h264_nvenc",
        "auto order picks nvenc first"
    );
    assert_eq!(info.fell_back_from, None);

    let (_, info) = build(&auto("AMD Radeon(TM) Graphics"), &amf_broken);
    assert_eq!(info.encoder_name, "h264_nvenc");
    assert_eq!(info.fell_back_from, Some("amf".to_owned()));

    let (_, info) = build(&auto("Intel(R) Arc(TM) A770"), &qsv_broken);
    assert_eq!(info.encoder_name, "h264_nvenc");
    assert_eq!(info.fell_back_from, Some("qsv".to_owned()));

    let mut forced = auto("AMD Radeon(TM) Graphics");
    forced.video.encoder = EncoderChoice::Nvenc;
    let (_, info) = build(&forced, &both);
    assert_eq!(
        info.encoder_name, "h264_nvenc",
        "explicit encoder wins over gpu"
    );
    assert_eq!(info.fell_back_from, None);
}

// ------------------------------------------------- canvas and mic tables

#[test]
fn canvas_for_height_table() {
    const ROWS: [(u32, (u32, u32)); 8] = [
        (360, (640, 360)),
        (480, (854, 480)),
        (720, (1280, 720)),
        (1080, (1920, 1080)),
        (1440, (2560, 1440)),
        (2160, (3840, 2160)),
        (719, (1278, 718)),
        (1081, (1922, 1080)),
    ];
    for (height, expected) in ROWS {
        assert_eq!(canvas_for_height(height), expected, "height {height}");
    }
}

#[test]
fn mic_device_of_table() {
    const ROWS: [(&str, Option<&str>); 7] = [
        ("", None),
        ("   ", None),
        ("default", None),
        ("DEFAULT", None),
        (" Default ", None),
        (
            "{0.0.1.00000000}.{abc-123}",
            Some("{0.0.1.00000000}.{abc-123}"),
        ),
        ("  Mic 1 ", Some("  Mic 1 ")),
    ];
    for (device, expected) in ROWS {
        assert_eq!(
            mic_device_of(device),
            expected.map(str::to_owned),
            "device {device:?}"
        );
    }
}

// ------------------------------------------------- audio tracks

fn exe(name: &str) -> ExeName {
    name.parse().expect("valid exe name")
}

fn src(kind: AudioSourceKind, gain_db: f32, gate_db: Option<f32>) -> TrackSource {
    TrackSource {
        kind,
        gain_db,
        gate_db,
    }
}

fn mic_kind(device: Option<&str>) -> AudioSourceKind {
    AudioSourceKind::Microphone {
        device: device.map(str::to_owned),
    }
}

fn app_kind(name: &str) -> AudioSourceKind {
    AudioSourceKind::App {
        exe: name.to_owned(),
    }
}

fn track(title: &str, sources: Vec<TrackSource>, bitrate_kbps: u32) -> AudioTrackConfig {
    AudioTrackConfig {
        title: title.to_owned(),
        sources,
        bitrate_kbps,
    }
}

#[test]
fn default_audio_gives_four_tracks() {
    let tracks = audio_tracks_of(&Audio::default());
    let expected = vec![
        track(
            "Mix",
            vec![
                src(AudioSourceKind::GameProcess, 0.0, None),
                src(mic_kind(None), 0.0, None),
                src(app_kind("discord.exe"), 0.0, None),
            ],
            128,
        ),
        track(
            "Game",
            vec![src(AudioSourceKind::GameProcess, 0.0, None)],
            128,
        ),
        track("Microphone", vec![src(mic_kind(None), 0.0, None)], 128),
        track(
            "Discord",
            vec![src(app_kind("discord.exe"), 0.0, None)],
            128,
        ),
    ];
    assert_eq!(tracks, expected);
}

#[test]
fn separate_tracks_off_gives_single_mix() {
    let mut audio = Audio::default();
    audio.separate_tracks = false;
    let tracks = audio_tracks_of(&audio);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Mix");
    assert_eq!(tracks[0].sources.len(), 3);
}

#[test]
fn all_sources_disabled_gives_no_tracks() {
    for separate in [true, false] {
        let mut audio = Audio::default();
        audio.separate_tracks = separate;
        audio.game.enabled = false;
        audio.mic.enabled = false;
        for app in &mut audio.apps {
            app.enabled = false;
        }
        audio.other_apps.enabled = false;
        assert!(
            audio_tracks_of(&audio).is_empty(),
            "separate_tracks={separate} should give no tracks"
        );
    }
}

#[test]
fn mic_gate_applies_only_to_mic_sources_when_enabled() {
    let mut audio = Audio::default();
    audio.mic.gate_db = -42.0;

    audio.mic.gate_enabled = false;
    for t in audio_tracks_of(&audio) {
        for s in t.sources {
            assert_eq!(s.gate_db, None, "gate off, track {}", t.title);
        }
    }

    audio.mic.gate_enabled = true;
    let tracks = audio_tracks_of(&audio);
    let mix = &tracks[0];
    assert_eq!(mix.sources[0].gate_db, None, "game in Mix");
    assert_eq!(mix.sources[1].kind, mic_kind(None));
    assert_eq!(mix.sources[1].gate_db, Some(-42.0), "mic in Mix");
    assert_eq!(mix.sources[2].gate_db, None, "app in Mix");
    let mic_track = tracks
        .iter()
        .find(|t| t.title == "Microphone")
        .expect("mic track");
    assert_eq!(mic_track.sources[0].gate_db, Some(-42.0), "mic track");
    let game_track = tracks
        .iter()
        .find(|t| t.title == "Game")
        .expect("game track");
    assert_eq!(game_track.sources[0].gate_db, None, "game track");
}

#[test]
fn game_gain_is_applied_to_game_sources() {
    let mut audio = Audio::default();
    audio.game.gain_db = -6.0;
    let tracks = audio_tracks_of(&audio);
    let game_track = tracks
        .iter()
        .find(|t| t.title == "Game")
        .expect("game track");
    assert_eq!(game_track.sources[0].gain_db, -6.0);
    assert_eq!(tracks[0].sources[0].gain_db, -6.0, "game in Mix");
    assert_eq!(tracks[0].sources[2].gain_db, 0.0, "app keeps its own gain");
}

#[test]
fn app_titles_gains_order_and_disabled_apps() {
    let mut audio = Audio::default();
    audio.other_apps.enabled = true;
    audio.other_apps.gain_db = -3.0;
    audio.apps = vec![
        AudioApp {
            exe: exe("discord.exe"),
            name: "Voice".to_owned(),
            enabled: true,
            gain_db: -9.0,
        },
        AudioApp {
            exe: exe("chrome.exe"),
            name: "Browser".to_owned(),
            enabled: true,
            gain_db: 0.0,
        },
        AudioApp {
            exe: exe("spotify.exe"),
            name: "Music".to_owned(),
            enabled: false,
            gain_db: 0.0,
        },
    ];
    let tracks = audio_tracks_of(&audio);
    let titles: Vec<&str> = tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Mix",
            "Game",
            "Microphone",
            "Voice",
            "Browser",
            "Other apps"
        ]
    );

    let mix = &tracks[0];
    assert_eq!(
        mix.sources,
        vec![
            src(AudioSourceKind::GameProcess, 0.0, None),
            src(mic_kind(None), 0.0, None),
            src(app_kind("discord.exe"), -9.0, None),
            src(app_kind("chrome.exe"), 0.0, None),
            src(AudioSourceKind::OtherApps, -3.0, None),
        ]
    );
    assert_eq!(
        tracks[5].sources,
        vec![src(AudioSourceKind::OtherApps, -3.0, None)]
    );
    assert!(tracks.iter().all(|t| !t.sources.is_empty()));
    assert!(!tracks
        .iter()
        .any(|t| t.sources.iter().any(|s| s.kind == app_kind("spotify.exe"))));
}

#[test]
fn mic_device_is_passed_to_microphone_sources() {
    let mut audio = Audio::default();
    audio.mic.device = "{0.0.1.00000000}.{abc-123}".to_owned();
    let tracks = audio_tracks_of(&audio);
    let want = mic_kind(Some("{0.0.1.00000000}.{abc-123}"));
    assert_eq!(tracks[0].sources[1].kind, want, "mic in Mix");
    let mic_track = tracks
        .iter()
        .find(|t| t.title == "Microphone")
        .expect("mic track");
    assert_eq!(mic_track.sources[0].kind, want, "mic track");

    audio.mic.device = "DEFAULT".to_owned();
    let tracks = audio_tracks_of(&audio);
    assert_eq!(tracks[0].sources[1].kind, mic_kind(None));
}

#[test]
fn audio_bitrate_applies_to_every_track() {
    let mut audio = Audio::default();
    audio.bitrate_kbps = 128;
    let tracks = audio_tracks_of(&audio);
    assert_eq!(tracks.len(), 4);
    assert!(tracks.iter().all(|t| t.bitrate_kbps == 128));
}

#[test]
fn build_places_audio_tracks_in_recorder_config() {
    let mut cfg = video_cfg(
        Codec::H264,
        Preset::High,
        Resolution::Native,
        EncoderChoice::Auto,
    );
    cfg.audio.game.gain_db = -6.0;
    cfg.audio.mic.gate_enabled = true;
    let (rc, _) = build(&cfg, &full_probe());
    // The default config binds the toggle mute key, so the mic edges are gated.
    assert_eq!(rc.audio_tracks.len(), 4);
    assert_eq!(
        rc.audio_tracks,
        audio_tracks_gated(&cfg.audio, gating(true, false))
    );
}

// ------------------------------------------------- canvas aspect (tester)

mod canvas_aspect_tester {
    use super::*;

    #[test]
    fn preset_ultra_canvas_height_is_1440() {
        let cfg = video_cfg(
            Codec::H264,
            Preset::Ultra,
            Resolution::Native,
            EncoderChoice::Auto,
        );
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.canvas_height, 1440);
        assert_eq!(rc.canvas, (2560, 1440));
    }

    #[test]
    fn preset_high_canvas_height_is_1080() {
        let cfg = video_cfg(
            Codec::H264,
            Preset::High,
            Resolution::Native,
            EncoderChoice::Auto,
        );
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.canvas_height, 1080);
        assert_eq!(rc.canvas, (1920, 1080));
    }

    #[test]
    fn preset_medium_canvas_height_is_720() {
        let cfg = video_cfg(
            Codec::H264,
            Preset::Medium,
            Resolution::Native,
            EncoderChoice::Auto,
        );
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.canvas_height, 720);
        assert_eq!(rc.canvas, (1280, 720));
    }

    #[test]
    fn custom_1080_not_native() {
        let mut cfg = video_cfg(
            Codec::H264,
            Preset::Custom,
            Resolution::P1080,
            EncoderChoice::Auto,
        );
        cfg.video.bitrate_kbps = 5000;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.canvas_height, 1080);
        assert_ne!(rc.canvas_height, 0);
    }

    #[test]
    fn custom_native_zero_height() {
        let mut cfg = video_cfg(
            Codec::H264,
            Preset::Custom,
            Resolution::Native,
            EncoderChoice::Auto,
        );
        cfg.video.bitrate_kbps = 5000;
        let (rc, _) = build(&cfg, &full_probe());
        assert_eq!(rc.canvas_height, 0);
        assert_eq!(rc.canvas, (3840, 2160));
    }
}

// ------------------------------------------- no_border_capture pass-through

#[test]
fn no_border_capture_defaults_false_in_recorder_config() {
    let cfg = Config::default();
    assert!(!cfg.video.no_border_capture);
    let (rc, _) = build(&cfg, &full_probe());
    assert!(!rc.no_border_capture);
}

#[test]
fn no_border_capture_is_passed_through_for_all_presets() {
    for preset in [Preset::Low, Preset::Medium, Preset::High, Preset::Custom] {
        for on in [false, true] {
            let mut cfg = video_cfg(Codec::H264, preset, Resolution::Native, EncoderChoice::Auto);
            cfg.video.no_border_capture = on;
            let (rc, _) = build(&cfg, &full_probe());
            assert_eq!(
                rc.no_border_capture, on,
                "no_border_capture for {preset:?} set to {on}"
            );
        }
    }
}

// ------------------------------------------------- push to talk gating (D65)

fn gated_mic_kind(device: Option<&str>) -> AudioSourceKind {
    AudioSourceKind::MicrophoneGated {
        device: device.map(str::to_owned),
    }
}

/// Only the microphone is enabled. `separate` turns on `ptt_separate_track`, which adds
/// the "Push to talk" source (MicPtt) to the plan.
fn mic_only_audio(separate: bool) -> Audio {
    let mut audio = Audio::default();
    audio.game.enabled = false;
    audio.other_apps.enabled = false;
    audio.apps.clear();
    audio.mic.enabled = true;
    audio.mic.ptt_separate_track = separate;
    audio
}

fn gating(mute_key: bool, ptt: bool) -> MicGating {
    MicGating { mute_key, ptt }
}

#[test]
fn gating_default_leaves_mic_tracks_plain() {
    let audio = mic_only_audio(false);
    let tracks = audio_tracks_gated(&audio, MicGating::default());
    assert_eq!(tracks, audio_tracks_of(&audio));
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].title, "Mix");
    assert_eq!(tracks[0].sources[0].kind, mic_kind(None));
    assert_eq!(tracks[1].title, "Microphone");
    assert_eq!(tracks[1].sources[0].kind, mic_kind(None));
}

#[test]
fn gating_mute_only_gates_every_mic_edge() {
    let tracks = audio_tracks_gated(&mic_only_audio(false), gating(true, false));
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].sources[0].kind, gated_mic_kind(None));
    assert_eq!(tracks[1].sources[0].kind, gated_mic_kind(None));
}

#[test]
fn gating_ptt_only_gates_mix_and_microphone() {
    let tracks = audio_tracks_gated(&mic_only_audio(false), gating(false, true));
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].sources[0].kind, gated_mic_kind(None));
    assert_eq!(tracks[1].sources[0].kind, gated_mic_kind(None));
}

#[test]
fn gating_ptt_with_separate_track_keeps_microphone_open() {
    let tracks = audio_tracks_gated(&mic_only_audio(true), gating(false, true));
    let titles: Vec<&str> = tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Mix", "Microphone", "Push to talk"]);
    assert_eq!(tracks[0].sources[0].kind, gated_mic_kind(None));
    assert_eq!(tracks[1].sources[0].kind, mic_kind(None));
    assert_eq!(tracks[2].sources[0].kind, gated_mic_kind(None));
}

#[test]
fn gating_mute_and_ptt_with_separate_track_makes_microphone_mute_only() {
    // A toggle mute still closes the standalone Microphone track, but push to talk does not.
    let tracks = audio_tracks_gated(&mic_only_audio(true), gating(true, true));
    let titles: Vec<&str> = tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Mix", "Microphone", "Push to talk"]);
    assert_eq!(tracks[0].sources[0].kind, gated_mic_kind(None));
    assert_eq!(
        tracks[1].sources[0].kind,
        AudioSourceKind::MicrophoneMuted { device: None }
    );
    assert_eq!(tracks[2].sources[0].kind, gated_mic_kind(None));
}

#[test]
fn gating_mute_only_with_separate_track_gates_push_to_talk_track() {
    // Toggle mode has one gate for every mic edge, so the push to talk track follows the mute.
    let tracks = audio_tracks_gated(&mic_only_audio(true), gating(true, false));
    assert_eq!(tracks.len(), 3);
    for track in &tracks {
        assert_eq!(
            track.sources[0].kind,
            gated_mic_kind(None),
            "{}",
            track.title
        );
    }
}

#[test]
fn gating_without_any_key_leaves_push_to_talk_track_plain() {
    let tracks = audio_tracks_gated(&mic_only_audio(true), MicGating::default());
    assert_eq!(tracks.len(), 3);
    assert_eq!(tracks[2].title, "Push to talk");
    assert_eq!(tracks[2].sources[0].kind, mic_kind(None));
}

#[test]
fn gating_applies_noise_gate_to_push_to_talk_track() {
    let mut audio = mic_only_audio(true);
    audio.mic.gate_enabled = true;
    audio.mic.gate_db = -40.0;
    let tracks = audio_tracks_gated(&audio, gating(false, true));
    assert_eq!(tracks[2].title, "Push to talk");
    assert_eq!(tracks[2].sources[0].gate_db, Some(-40.0));
    assert_eq!(tracks[1].sources[0].gate_db, Some(-40.0));
}

#[test]
fn gating_uses_mic_device_on_gated_sources() {
    let mut audio = mic_only_audio(true);
    audio.mic.device = "Headset".to_owned();
    let tracks = audio_tracks_gated(&audio, gating(false, true));
    assert_eq!(tracks[2].sources[0].kind, gated_mic_kind(Some("Headset")));
}

#[test]
fn mic_gating_default_is_toggle_mute_only() {
    assert_eq!(mic_gating(&Config::default()), gating(true, false));
}

#[test]
fn mic_gating_legacy_push_to_talk_mode_is_hold_key() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.mute_mic_mode = MuteMode::PushToTalk;
    assert_eq!(mic_gating(&cfg), gating(false, true));
}

#[test]
fn mic_gating_legacy_mode_without_key_gates_nothing() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.mute_mic = None;
    cfg.hotkeys.session.mute_mic_mode = MuteMode::PushToTalk;
    assert_eq!(mic_gating(&cfg), gating(false, false));
}

#[test]
fn mic_gating_push_to_talk_list_sets_ptt() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.mute_mic = None;
    cfg.hotkeys.session.push_to_talk = vec!["Mouse4".parse().expect("chord")];
    assert_eq!(mic_gating(&cfg), gating(false, true));
}

#[test]
fn mic_gating_list_and_toggle_key_both_apply() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.push_to_talk = vec!["Mouse4".parse().expect("chord")];
    assert_eq!(mic_gating(&cfg), gating(true, true));
}

#[test]
fn push_to_talk_list_gives_one_binding_per_chord() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.push_to_talk = vec![
        "Mouse4".parse().expect("chord"),
        "Alt+F6".parse().expect("chord"),
    ];
    let (bindings, warnings) = clip_hotkey_bindings(&cfg);
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    // The default six, then the two push to talk chords after the mute key.
    assert_eq!(bindings.len(), 8);
    assert!(matches!(bindings[5].action, HotkeyAction::MuteMic));
    assert!(matches!(bindings[6].action, HotkeyAction::PushToTalk));
    assert_eq!(bindings[6].chord.to_string(), "Mouse4");
    assert!(matches!(bindings[7].action, HotkeyAction::PushToTalk));
    assert_eq!(bindings[7].chord.to_string(), "Alt+F6");
    assert!(bindings[6..]
        .iter()
        .all(|b| b.enabled && b.press == PressKind::Tap));
}

#[test]
fn legacy_push_to_talk_mode_maps_mute_key_to_push_to_talk() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.mute_mic_mode = MuteMode::PushToTalk;
    let (bindings, warnings) = clip_hotkey_bindings(&cfg);
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    assert_eq!(bindings.len(), 6);
    assert!(matches!(bindings[5].action, HotkeyAction::PushToTalk));
    assert_eq!(bindings[5].chord.to_string(), "Alt+F7");
}

#[test]
fn toggle_mute_key_stays_mute_mic_with_push_to_talk_list() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.push_to_talk = vec!["Mouse5".parse().expect("chord")];
    let (bindings, _) = clip_hotkey_bindings(&cfg);
    assert!(matches!(bindings[5].action, HotkeyAction::MuteMic));
    assert!(matches!(bindings[6].action, HotkeyAction::PushToTalk));
}

#[test]
fn recorder_config_gates_mic_when_push_to_talk_key_is_bound() {
    let mut cfg = Config::default();
    cfg.audio.mic.ptt_separate_track = true;
    cfg.hotkeys.session.mute_mic = None;
    cfg.hotkeys.session.push_to_talk = vec!["Mouse4".parse().expect("chord")];
    let (rc, _) = build(&cfg, &full_probe());
    let kinds: Vec<AudioSourceKind> = rc
        .audio_tracks
        .iter()
        .flat_map(|t| t.sources.iter().map(|s| s.kind.clone()))
        .collect();
    assert!(kinds.contains(&gated_mic_kind(None)));
    assert!(
        kinds.contains(&mic_kind(None)),
        "Microphone track stays open"
    );
}
