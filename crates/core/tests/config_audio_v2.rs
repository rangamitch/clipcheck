//! Tests for the audio/video model v2: `[audio]` schema, `track_plan`, legacy keys,
//! audio validation (CFG-A*), video additions (CFG-V012/V013), R002 with derived tracks,
//! and apply-class rules for audio.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn parse(text: &str) -> Config {
    Config::from_toml_str(text).expect("config should parse")
}

fn exe(s: &str) -> ExeName {
    s.parse().expect("test exe should parse")
}

fn app(exe_name: &str, name: &str) -> AudioApp {
    AudioApp {
        exe: exe(exe_name),
        name: name.to_owned(),
        enabled: true,
        gain_db: 0.0,
    }
}

fn find<'a>(d: &'a [Diagnostic], code: &str) -> &'a Diagnostic {
    d.iter()
        .find(|x| x.code == code)
        .unwrap_or_else(|| panic!("expected {code} in {d:#?}"))
}

fn assert_error(c: &Config, code: &str, path: &str) {
    let d = validate(c);
    let x = find(&d, code);
    assert_eq!(
        x.severity,
        Severity::Error,
        "{code} should be an error: {x:?}"
    );
    assert_eq!(x.path, path, "{code} path: {x:?}");
}

fn assert_no(c: &Config, code: &str) {
    let d = validate(c);
    assert!(
        !d.iter().any(|x| x.code == code),
        "{code} should not fire: {d:#?}"
    );
}

fn titles(plan: &[TrackPlan]) -> Vec<&str> {
    plan.iter().map(|t| t.title.as_str()).collect()
}

// ---------------------------------------------------------------------------
// 1. round trip and defaults
// ---------------------------------------------------------------------------

#[test]
fn default_toml_round_trips_to_default() {
    let parsed = parse(Config::default_toml());
    assert_eq!(parsed, Config::default());
}

#[test]
fn serialized_default_reparses_equal() {
    let text = Config::default().to_toml_string().expect("serialize");
    assert_eq!(parse(&text), Config::default());
}

#[test]
fn default_validate_has_no_error_diagnostics() {
    let errors: Vec<_> = validate(&Config::default())
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
}

// ---------------------------------------------------------------------------
// 2. old v1 file with [[audio.tracks]]
// ---------------------------------------------------------------------------

#[test]
fn v1_audio_tracks_file_loads_with_new_defaults() {
    let text = r#"
[[audio.tracks]]
name = "Game"
enabled = true
sources = ["game", "mic"]
gain_db = -3.0

[[audio.tracks]]
name = "Chat"
enabled = true
sources = ["process:Discord.exe"]
gain_db = 0.0
"#;
    let cfg = parse(text);
    assert_eq!(cfg.audio, Audio::default());
}

// ---------------------------------------------------------------------------
// 3. legacy mic_device key
// ---------------------------------------------------------------------------

#[test]
fn legacy_mic_device_moves_into_mic_device() {
    let cfg = parse("[audio]\nmic_device = \"USB Mic\"\n");
    assert_eq!(cfg.audio.mic.device, "USB Mic");
}

#[test]
fn explicit_mic_device_key_is_read() {
    let cfg = parse("[audio.mic]\ndevice = \"X\"\n");
    assert_eq!(cfg.audio.mic.device, "X");
}

#[test]
fn legacy_key_is_never_written_back() {
    let cfg = parse("[audio]\nmic_device = \"USB Mic\"\n");
    let text = cfg.to_toml_string().expect("serialize");
    assert!(!text.contains("mic_device"), "output: {text}");
    assert_eq!(parse(&text).audio.mic.device, "USB Mic");
}

// ---------------------------------------------------------------------------
// 4. partial file
// ---------------------------------------------------------------------------

#[test]
fn partial_audio_mic_keeps_other_defaults() {
    let cfg = parse("[audio.mic]\ngain_db = -3.0\n");
    let mut expected = Config::default();
    expected.audio.mic.gain_db = -3.0;
    assert_eq!(cfg, expected);
    assert_eq!(cfg.audio.mic.device, "default");
    assert!(cfg.audio.mic.enabled);
}

// ---------------------------------------------------------------------------
// 5. enum and value string forms
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ResWrap {
    resolution: Resolution,
}

#[test]
fn every_resolution_string_round_trips() {
    let cases = [
        ("native", Resolution::Native),
        ("360p", Resolution::P360),
        ("480p", Resolution::P480),
        ("720p", Resolution::P720),
        ("1080p", Resolution::P1080),
        ("1440p", Resolution::P1440),
        ("2160p", Resolution::P2160),
    ];
    for (text, value) in cases {
        let cfg = parse(&format!("[video]\nresolution = \"{text}\"\n"));
        assert_eq!(cfg.video.resolution, value, "parse {text}");
        let out = cfg.to_toml_string().expect("serialize");
        assert!(
            out.contains(&format!("resolution = \"{text}\"")),
            "serialized form of {text}: {out}"
        );
        assert_eq!(parse(&out).video.resolution, value);
    }
}

#[test]
fn unknown_resolution_is_rejected() {
    assert!(Config::from_toml_str("[video]\nresolution = \"4320p\"\n").is_err());
}

#[test]
fn every_fps_integer_round_trips() {
    let cases = [
        (24, Fps::F24),
        (30, Fps::F30),
        (60, Fps::F60),
        (90, Fps::F90),
        (120, Fps::F120),
        (144, Fps::F144),
    ];
    for (n, value) in cases {
        let cfg = parse(&format!("[video]\nfps = {n}\n"));
        assert_eq!(cfg.video.fps, value, "parse {n}");
        let out = cfg.to_toml_string().expect("serialize");
        assert!(out.contains(&format!("fps = {n}")), "serialized: {out}");
        assert_eq!(parse(&out).video.fps, value);
    }
}

#[test]
fn fps_45_is_rejected() {
    assert!(Config::from_toml_str("[video]\nfps = 45\n").is_err());
}

#[test]
fn fps_try_from_error_text() {
    let err = Fps::try_from(45u16).expect_err("45 is not a valid fps");
    assert!(
        err.contains("fps must be 24, 30, 60, 90, 120 or 144 (CFG-V007)"),
        "error text: {err}"
    );
}

#[test]
fn every_encoder_speed_string_round_trips() {
    let cases = [
        ("fastest", EncoderSpeed::Fastest),
        ("balanced", EncoderSpeed::Balanced),
        ("quality", EncoderSpeed::Quality),
    ];
    for (text, value) in cases {
        let cfg = parse(&format!("[video]\nencoder_speed = \"{text}\"\n"));
        assert_eq!(cfg.video.encoder_speed, value, "parse {text}");
        let out = cfg.to_toml_string().expect("serialize");
        assert!(out.contains(&format!("encoder_speed = \"{text}\"")));
        assert_eq!(parse(&out).video.encoder_speed, value);
    }
}

#[test]
fn every_preset_string_round_trips() {
    let cases = [
        ("low", Preset::Low),
        ("medium", Preset::Medium),
        ("high", Preset::High),
        ("ultra", Preset::Ultra),
        ("custom", Preset::Custom),
    ];
    for (text, value) in cases {
        let cfg = parse(&format!("[video]\npreset = \"{text}\"\n"));
        assert_eq!(cfg.video.preset, value, "parse {text}");
        let out = cfg.to_toml_string().expect("serialize");
        assert!(out.contains(&format!("preset = \"{text}\"")));
        assert_eq!(parse(&out).video.preset, value);
    }
}

#[test]
fn resolution_wrapper_struct_round_trips() {
    // Direct serde round trip for Resolution, independent of the Config wrapper.
    let w = ResWrap {
        resolution: Resolution::P1440,
    };
    let text = toml::to_string(&w).expect("serialize wrapper");
    assert_eq!(text.trim(), "resolution = \"1440p\"");
    let back: ResWrap = toml::from_str(&text).expect("parse wrapper");
    assert_eq!(back, w);
}

// ---------------------------------------------------------------------------
// 6. track_plan and gain_for
// ---------------------------------------------------------------------------

#[test]
fn track_plan_all_enabled_separate_lists_every_track() {
    let mut cfg = Config::default();
    cfg.audio.other_apps.enabled = true;
    let plan = cfg.audio.track_plan();
    assert_eq!(
        titles(&plan),
        ["Mix", "Game", "Microphone", "Discord", "Other apps"]
    );
    // The Mix holds the four enabled sources (game, mic, Discord, other apps), in order.
    assert_eq!(
        plan[0].sources,
        vec![
            SourceRef::Game,
            SourceRef::Mic,
            SourceRef::App(exe("discord.exe")),
            SourceRef::OtherApps,
        ]
    );
    assert_eq!(plan[1].sources, vec![SourceRef::Game]);
    assert_eq!(plan[4].sources, vec![SourceRef::OtherApps]);
    assert_no(&cfg, "CFG-A001");
}

#[test]
fn track_plan_not_separate_is_one_mix() {
    let mut cfg = Config::default();
    cfg.audio.other_apps.enabled = true;
    cfg.audio.separate_tracks = false;
    let plan = cfg.audio.track_plan();
    assert_eq!(titles(&plan), ["Mix"]);
    assert_eq!(plan[0].sources.len(), 4);
}

#[test]
fn track_plan_no_source_is_silent_mix_regardless_of_separate() {
    for separate in [true, false] {
        let mut cfg = Config::default();
        cfg.audio.separate_tracks = separate;
        cfg.audio.game.enabled = false;
        cfg.audio.mic.enabled = false;
        cfg.audio.other_apps.enabled = false;
        cfg.audio.apps[0].enabled = false;
        let plan = cfg.audio.track_plan();
        assert_eq!(plan.len(), 1, "separate = {separate}");
        assert_eq!(plan[0].title, "Mix");
        assert!(plan[0].sources.is_empty());
    }
}

#[test]
fn seven_enabled_sources_separate_is_eight_tracks_and_valid() {
    let mut cfg = Config::default();
    cfg.audio.other_apps.enabled = true;
    cfg.audio.apps = vec![
        app("a1.exe", "App1"),
        app("a2.exe", "App2"),
        app("a3.exe", "App3"),
        app("a4.exe", "App4"),
    ];
    // game, mic, other, plus 4 apps = 7 sources
    assert_eq!(cfg.audio.enabled_sources().len(), 7);
    assert_eq!(cfg.audio.track_plan().len(), 8);
    assert_no(&cfg, "CFG-A001");
}

#[test]
fn eight_enabled_sources_separate_is_nine_tracks_and_a001() {
    let mut cfg = Config::default();
    cfg.audio.other_apps.enabled = true;
    cfg.audio.apps = vec![
        app("a1.exe", "App1"),
        app("a2.exe", "App2"),
        app("a3.exe", "App3"),
        app("a4.exe", "App4"),
        app("a5.exe", "App5"),
    ];
    // game, mic, other, plus 5 apps = 8 sources
    assert_eq!(cfg.audio.enabled_sources().len(), 8);
    assert_eq!(cfg.audio.track_plan().len(), 9);
    assert_error(&cfg, "CFG-A001", "audio.separate_tracks");
}

#[test]
fn sixteen_apps_not_separate_has_no_a001() {
    let mut cfg = Config::default();
    cfg.audio.separate_tracks = false;
    cfg.audio.apps = (1..=16)
        .map(|i| app(&format!("app{i}.exe"), &format!("App {i}")))
        .collect();
    assert_eq!(cfg.audio.apps.len(), 16);
    assert_no(&cfg, "CFG-A001");
}

#[test]
fn seventeen_apps_is_a001_on_audio_apps() {
    let mut cfg = Config::default();
    cfg.audio.separate_tracks = false;
    cfg.audio.apps = (1..=17)
        .map(|i| app(&format!("app{i}.exe"), &format!("App {i}")))
        .collect();
    assert_error(&cfg, "CFG-A001", "audio.apps");
}

#[test]
fn app_track_uses_its_name_and_disabled_apps_are_skipped() {
    let mut cfg = Config::default();
    cfg.audio.game.enabled = false;
    cfg.audio.mic.enabled = false;
    cfg.audio.other_apps.enabled = false;
    cfg.audio.apps = vec![
        app("first.exe", "First Chat"),
        AudioApp {
            enabled: false,
            ..app("skip.exe", "Skipped")
        },
        app("third.exe", "Third"),
    ];
    let plan = cfg.audio.track_plan();
    assert_eq!(titles(&plan), ["Mix", "First Chat", "Third"]);
    assert_eq!(
        plan[0].sources,
        vec![
            SourceRef::App(exe("first.exe")),
            SourceRef::App(exe("third.exe")),
        ]
    );
    assert_eq!(plan[2].sources, vec![SourceRef::App(exe("third.exe"))]);
}

#[test]
fn gain_for_returns_each_configured_gain() {
    let mut cfg = Config::default();
    cfg.audio.game.gain_db = -6.0;
    cfg.audio.mic.gain_db = 3.0;
    cfg.audio.other_apps.gain_db = -10.0;
    cfg.audio.apps[0].gain_db = 5.0;
    assert_eq!(cfg.audio.gain_for(&SourceRef::Game), -6.0);
    assert_eq!(cfg.audio.gain_for(&SourceRef::Mic), 3.0);
    assert_eq!(cfg.audio.gain_for(&SourceRef::OtherApps), -10.0);
    assert_eq!(cfg.audio.gain_for(&SourceRef::App(exe("discord.exe"))), 5.0);
    assert_eq!(cfg.audio.gain_for(&SourceRef::App(exe("spotify.exe"))), 0.0);
}

#[test]
fn exe_name_normalizes_and_rejects_bad_input() {
    assert_eq!(exe("Discord.exe").as_str(), "discord.exe");
    assert!("".parse::<ExeName>().is_err());
    assert!("discord".parse::<ExeName>().is_err());
    assert!("C:\\x\\discord.exe".parse::<ExeName>().is_err());
}

// ---------------------------------------------------------------------------
// 7. Preset table
// ---------------------------------------------------------------------------

#[test]
fn preset_values_table() {
    let v = |p: Preset| p.values().expect("non-custom preset has values");
    assert_eq!(
        v(Preset::Low),
        PresetValues {
            resolution: Resolution::P480,
            fps: Fps::F30,
            bitrate_kbps: 4000,
        }
    );
    assert_eq!(
        v(Preset::Medium),
        PresetValues {
            resolution: Resolution::P720,
            fps: Fps::F60,
            bitrate_kbps: 8000,
        }
    );
    assert_eq!(
        v(Preset::High),
        PresetValues {
            resolution: Resolution::P1080,
            fps: Fps::F60,
            bitrate_kbps: 14000,
        }
    );
    assert_eq!(
        v(Preset::Ultra),
        PresetValues {
            resolution: Resolution::P1440,
            fps: Fps::F60,
            bitrate_kbps: 30000,
        }
    );
    assert_eq!(Preset::Custom.values(), None);
}

// ---------------------------------------------------------------------------
// 8. validation codes
// ---------------------------------------------------------------------------

#[test]
fn a002_app_name_length() {
    let mut c = Config::default();
    c.audio.apps[0].name = String::new();
    assert_error(&c, "CFG-A002", "audio.apps[0].name");

    c.audio.apps[0].name = "   ".to_owned();
    assert_error(&c, "CFG-A002", "audio.apps[0].name");

    c.audio.apps[0].name = "n".repeat(33);
    assert_error(&c, "CFG-A002", "audio.apps[0].name");

    c.audio.apps[0].name = "n".repeat(32);
    assert_no(&c, "CFG-A002");
}

#[test]
fn a003_duplicate_app_name_case_insensitive() {
    let mut c = Config::default();
    c.audio.apps = vec![
        app("discord.exe", "Discord"),
        app("discord2.exe", "discord"),
    ];
    assert_error(&c, "CFG-A003", "audio.apps[1].name");
}

#[test]
fn a006_gain_boundaries_for_every_gain_field() {
    let mut c = Config::default();

    c.audio.game.gain_db = -60.0;
    assert_no(&c, "CFG-A006");
    c.audio.game.gain_db = 12.0;
    assert_no(&c, "CFG-A006");
    c.audio.game.gain_db = -60.1;
    assert_error(&c, "CFG-A006", "audio.game.gain_db");
    c.audio.game.gain_db = 12.1;
    assert_error(&c, "CFG-A006", "audio.game.gain_db");
    c.audio.game.gain_db = 0.0;

    c.audio.mic.gain_db = -60.1;
    assert_error(&c, "CFG-A006", "audio.mic.gain_db");
    c.audio.mic.gain_db = 0.0;

    c.audio.other_apps.gain_db = 12.1;
    assert_error(&c, "CFG-A006", "audio.other_apps.gain_db");
    c.audio.other_apps.gain_db = -60.0;
    assert_no(&c, "CFG-A006");
    c.audio.other_apps.gain_db = 0.0;

    c.audio.apps[0].gain_db = -60.1;
    assert_error(&c, "CFG-A006", "audio.apps[0].gain_db");
    c.audio.apps[0].gain_db = 12.0;
    assert_no(&c, "CFG-A006");
}

#[test]
fn a013_gate_db_boundaries() {
    let mut c = Config::default();
    c.audio.mic.gate_db = -100.1;
    assert_error(&c, "CFG-A013", "audio.mic.gate_db");
    c.audio.mic.gate_db = -19.9;
    assert_error(&c, "CFG-A013", "audio.mic.gate_db");
    c.audio.mic.gate_db = -100.0;
    assert_no(&c, "CFG-A013");
    c.audio.mic.gate_db = -20.0;
    assert_no(&c, "CFG-A013");
}

#[test]
fn a012_duplicate_exe_through_toml_is_case_normalized() {
    let text = r#"
[[audio.apps]]
exe = "Discord.exe"
name = "Chat A"

[[audio.apps]]
exe = "discord.exe"
name = "Chat B"
"#;
    let cfg = parse(text);
    assert_eq!(cfg.audio.apps[0].exe.as_str(), "discord.exe");
    assert_eq!(cfg.audio.apps[1].exe.as_str(), "discord.exe");
    assert_error(&cfg, "CFG-A012", "audio.apps[1].exe");
}

#[test]
fn empty_exe_fails_to_parse() {
    let text = "[[audio.apps]]\nexe = \"\"\nname = \"X\"\n";
    assert!(Config::from_toml_str(text).is_err());
}

#[test]
fn v012_gpu_must_not_be_blank() {
    let mut c = Config::default();
    c.video.gpu = String::new();
    assert_error(&c, "CFG-V012", "video.gpu");
    c.video.gpu = "   ".to_owned();
    assert_error(&c, "CFG-V012", "video.gpu");
    c.video.gpu = "auto".to_owned();
    assert_no(&c, "CFG-V012");
    c.video.gpu = "NVIDIA GeForce RTX 4070".to_owned();
    assert_no(&c, "CFG-V012");
}

#[test]
fn v013_b_frames_boundaries() {
    let mut c = Config::default();
    c.video.b_frames = 5;
    assert_error(&c, "CFG-V013", "video.b_frames");
    c.video.b_frames = 0;
    assert_no(&c, "CFG-V013");
    c.video.b_frames = 4;
    assert_no(&c, "CFG-V013");
}

#[test]
fn r002_ram_budget_counts_derived_tracks() {
    // Default video max 15000 kbps, 300 s.
    // separate = true: 4 tracks * 128 + 15000 = 15512 kbps -> about 569 MB.
    // separate = false: 1 track * 128 + 15000 = 15128 kbps -> about 555 MB.
    let mut c = Config::default();
    c.replay.storage = ReplayStorage::Ram;
    c.replay.ram_max_mb = 565;
    c.audio.separate_tracks = false;
    assert_no(&c, "CFG-R002");

    c.audio.separate_tracks = true;
    assert_error(&c, "CFG-R002", "replay.ram_max_mb");
}

#[test]
fn r002_ram_with_generous_budget_is_clean() {
    let mut c = Config::default();
    c.replay.storage = ReplayStorage::Ram;
    c.replay.ram_max_mb = 4096;
    assert_no(&c, "CFG-R002");
}

#[test]
fn r002_disk_default_config_is_clean_with_tiny_ram_budget() {
    // Disk storage ignores the RAM budget, so the default plan cannot trip R002.
    let mut c = Config::default();
    c.replay.ram_max_mb = 64;
    assert_no(&c, "CFG-R002");
}

// ---------------------------------------------------------------------------
// apply_class for audio and video keys
// ---------------------------------------------------------------------------

fn class_of(mutate: impl FnOnce(&mut Config)) -> ApplyClass {
    let old = Config::default();
    let mut new = old.clone();
    mutate(&mut new);
    apply_class(&old, &new)
}

#[test]
fn audio_changes_are_audio_reinit() {
    assert_eq!(
        class_of(|c| c.audio.separate_tracks = false),
        ApplyClass::AudioReinit
    );
    assert_eq!(
        class_of(|c| c.audio.mic.gain_db = -3.0),
        ApplyClass::AudioReinit
    );
    assert_eq!(
        class_of(|c| c.audio.mic.device = "USB".to_owned()),
        ApplyClass::AudioReinit
    );
    assert_eq!(
        class_of(|c| c.audio.apps[0].enabled = false),
        ApplyClass::AudioReinit
    );
    assert_eq!(
        class_of(|c| c.audio.apps.push(app("x.exe", "X"))),
        ApplyClass::AudioReinit
    );
    assert_eq!(
        class_of(|c| c.audio.bitrate_kbps = 256),
        ApplyClass::AudioReinit
    );
}

#[test]
fn video_changes_are_pipeline_reinit() {
    assert_eq!(
        class_of(|c| c.video.gpu = "auto2".to_owned()),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.encoder_speed = EncoderSpeed::Quality),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.b_frames = 0),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.preset = Preset::Low),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.resolution = Resolution::P720),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.fps = Fps::F30),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.bitrate_kbps = 12000),
        ApplyClass::PipelineReinit
    );
    assert_eq!(
        class_of(|c| c.video.cursor = false),
        ApplyClass::PipelineReinit
    );
}

#[test]
fn audio_change_with_video_change_is_audio_reinit() {
    assert_eq!(
        class_of(|c| {
            c.audio.mic.gain_db = -3.0;
            c.video.fps = Fps::F30;
        }),
        ApplyClass::AudioReinit
    );
}

#[test]
fn log_dir_is_restart_alone_and_with_audio() {
    assert_eq!(
        class_of(|c| c.advanced.log_dir = std::path::PathBuf::from(r"D:\logs")),
        ApplyClass::Restart
    );
    assert_eq!(
        class_of(|c| {
            c.audio.separate_tracks = false;
            c.advanced.log_dir = std::path::PathBuf::from(r"D:\logs");
        }),
        ApplyClass::Restart
    );
}
