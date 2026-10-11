//! Integration tests for `clipcheck_core::config::validate`.

use std::path::PathBuf;

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn chord(s: &str) -> KeyChord {
    s.parse().expect("test chord should parse")
}

fn with_template(t: &str) -> Config {
    let mut c = Config::default();
    c.storage.filename_template = t.to_owned();
    c
}

fn find<'a>(d: &'a [Diagnostic], code: &str) -> &'a Diagnostic {
    d.iter()
        .find(|x| x.code == code)
        .unwrap_or_else(|| panic!("expected {code} in {d:#?}"))
}

fn assert_clean(c: &Config) {
    let d = validate(c);
    assert!(d.is_empty(), "expected no diagnostics, got {d:#?}");
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

fn assert_warning(c: &Config, code: &str, path: &str) {
    let d = validate(c);
    let x = find(&d, code);
    assert_eq!(
        x.severity,
        Severity::Warning,
        "{code} should be a warning: {x:?}"
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

// ---------------------------------------------------------------------------
// 1. defaults
// ---------------------------------------------------------------------------

#[test]
fn default_config_has_zero_diagnostics() {
    assert_clean(&Config::default());
}

#[test]
fn default_template_and_root_are_accepted() {
    let c = Config::default();
    assert_eq!(
        c.storage.filename_template,
        "{game} {yyyy-MM-dd HH-mm-ss} {kind}"
    );
    assert!(c.storage.root.to_string_lossy().contains('%'));
    assert_no(&c, "CFG-S001");
    assert_no(&c, "CFG-S008");
}

// ---------------------------------------------------------------------------
// 2. config version and storage
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::field_reassign_with_default)]
fn config_version_newer_than_supported_is_p003() {
    let mut c = Config::default();
    c.config_version = 2;
    assert_error(&c, "CFG-P003", "config_version");
    c.config_version = 1;
    assert_no(&c, "CFG-P003");
}

#[test]
fn storage_root_must_be_absolute() {
    let mut c = Config::default();
    c.storage.root = PathBuf::from(r"relative\clips");
    assert_error(&c, "CFG-S001", "storage.root");

    c.storage.root = PathBuf::from("");
    assert_error(&c, "CFG-S001", "storage.root");

    c.storage.root = PathBuf::from("   ");
    assert_error(&c, "CFG-S001", "storage.root");

    c.storage.root = PathBuf::from(r"C:\ClipCheck");
    assert_no(&c, "CFG-S001");

    c.storage.root = PathBuf::from(r"%APPDATA%\ClipCheck");
    assert_no(&c, "CFG-S001");
}

#[test]
fn quota_boundaries() {
    let mut c = Config::default();
    for ok in [0u32, 1, 100_000] {
        c.storage.quota_gb = ok;
        assert_no(&c, "CFG-S006");
    }
    c.storage.quota_gb = 100_001;
    assert_error(&c, "CFG-S006", "storage.quota_gb");
}

#[test]
fn template_without_date_or_id_is_s008() {
    assert_error(
        &with_template("{game} {kind}"),
        "CFG-S008",
        "storage.filename_template",
    );
    assert_error(&with_template(""), "CFG-S008", "storage.filename_template");
}

#[test]
fn template_with_id_has_no_s008() {
    assert_no(&with_template("{game} {id}"), "CFG-S008");
}

#[test]
fn template_without_kind_is_s007_warning_only() {
    let c = with_template("{game}{yyyy}");
    let d = validate(&c);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].code, "CFG-S007");
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(d[0].path, "storage.filename_template");
}

#[test]
fn template_unknown_token_is_s005() {
    assert_error(
        &with_template("{game} {bogus}"),
        "CFG-S005",
        "storage.filename_template",
    );
}

#[test]
fn template_unclosed_brace_is_s005() {
    assert_error(
        &with_template("{game} {kind"),
        "CFG-S005",
        "storage.filename_template",
    );
}

#[test]
fn template_illegal_char_is_s003() {
    let c = with_template("a:b {id} {kind}");
    let d = validate(&c);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_error(&c, "CFG-S003", "storage.filename_template");
}

#[test]
fn template_rendered_length_boundary() {
    // "x" * n + "{id}" + "{kind}" renders to n + 8 + 7 characters; the limit is 180.
    let at_limit = format!("{}{{id}}{{kind}}", "x".repeat(165));
    assert_no(&with_template(&at_limit), "CFG-S004");

    let over = format!("{}{{id}}{{kind}}", "x".repeat(166));
    assert_error(
        &with_template(&over),
        "CFG-S004",
        "storage.filename_template",
    );
}

// ---------------------------------------------------------------------------
// detection
// ---------------------------------------------------------------------------

#[test]
fn exe_in_allow_and_block_is_d002_case_insensitive() {
    let mut c = Config::default();
    c.detection.allow = vec!["Discord.exe".parse().unwrap()];
    c.detection.block = vec!["discord.exe".parse().unwrap()];
    assert_error(&c, "CFG-D002", "detection.allow");
}

#[test]
fn allowlist_mode_with_empty_allow_is_d003_warning() {
    let mut c = Config::default();
    c.detection.mode = DetectionMode::Allowlist;
    assert_warning(&c, "CFG-D003", "detection.mode");
    c.detection.allow = vec!["cs2.exe".parse().unwrap()];
    assert_no(&c, "CFG-D003");
}

#[test]
fn stop_grace_boundary_is_d004() {
    let mut c = Config::default();
    c.detection.stop_grace_s = 300;
    assert_no(&c, "CFG-D004");
    c.detection.stop_grace_s = 301;
    assert_error(&c, "CFG-D004", "detection.stop_grace_s");
}

// ---------------------------------------------------------------------------
// video
// ---------------------------------------------------------------------------

#[test]
fn av1_with_software_encoder_is_v001() {
    let mut c = Config::default();
    c.video.codec = Codec::Av1;
    c.video.encoder = EncoderChoice::Software;
    assert_error(&c, "CFG-V001", "video.codec");

    c.video.encoder = EncoderChoice::Nvenc;
    assert_no(&c, "CFG-V001");
}

#[test]
fn quality_cq_boundaries_per_codec() {
    let mut c = Config::default();
    c.video.codec = Codec::H264;
    c.video.quality_cq = 51;
    assert_no(&c, "CFG-V004");
    c.video.quality_cq = 52;
    assert_error(&c, "CFG-V004", "video.quality_cq");

    c.video.codec = Codec::Hevc;
    c.video.quality_cq = 52;
    assert_error(&c, "CFG-V004", "video.quality_cq");

    c.video.codec = Codec::Av1;
    c.video.quality_cq = 63;
    assert_no(&c, "CFG-V004");
    c.video.quality_cq = 64;
    assert_error(&c, "CFG-V004", "video.quality_cq");
}

#[test]
fn bitrate_boundaries_are_v005() {
    let mut c = Config::default();
    c.video.bitrate_kbps = 1000;
    c.video.max_bitrate_kbps = 1000;
    assert_clean(&c);

    c.video.bitrate_kbps = 100_000;
    c.video.max_bitrate_kbps = 100_000;
    assert_clean(&c);

    c.video.bitrate_kbps = 999;
    c.video.max_bitrate_kbps = 999;
    assert_error(&c, "CFG-V005", "video.bitrate_kbps");

    c.video.bitrate_kbps = 100_001;
    c.video.max_bitrate_kbps = 100_001;
    assert_error(&c, "CFG-V005", "video.bitrate_kbps");
}

#[test]
fn max_bitrate_below_bitrate_is_v006() {
    let mut c = Config::default();
    c.video.preset = Preset::Custom;
    c.video.rate_control = RateControl::Vbr;
    c.video.bitrate_kbps = 12_000;
    c.video.max_bitrate_kbps = 11_999;
    assert_error(&c, "CFG-V006", "video.max_bitrate_kbps");
}

#[test]
fn ultra_preset_with_default_max_bitrate_validates() {
    let mut c = Config::default();
    c.video.preset = Preset::Ultra;
    // max_bitrate_kbps keeps its default (15000); the check must not apply outside custom + vbr.
    assert_no(&c, "CFG-V006");
}

#[test]
fn keyframe_interval_boundaries_are_v008() {
    let mut c = Config::default();
    c.video.keyframe_interval_s = 1;
    assert_no(&c, "CFG-V008");
    c.video.keyframe_interval_s = 4;
    assert_no(&c, "CFG-V008");
    c.video.keyframe_interval_s = 0;
    assert_error(&c, "CFG-V008", "video.keyframe_interval_s");
    c.video.keyframe_interval_s = 5;
    assert_error(&c, "CFG-V008", "video.keyframe_interval_s");
}

#[test]
fn software_120fps_native_is_v010_warning() {
    let mut c = Config::default();
    c.video.encoder = EncoderChoice::Software;
    c.video.fps = Fps::F120;
    c.video.resolution = Resolution::Native;
    assert_warning(&c, "CFG-V010", "video.fps");

    c.video.resolution = Resolution::P1080;
    assert_no(&c, "CFG-V010");
}

// ---------------------------------------------------------------------------
// audio
// ---------------------------------------------------------------------------

#[test]
fn opus_is_a011() {
    let mut c = Config::default();
    c.audio.codec = AudioCodec::Opus;
    assert_error(&c, "CFG-A011", "audio.codec");
}

#[test]
fn audio_bitrate_boundaries_are_a008() {
    let mut c = Config::default();
    c.audio.bitrate_kbps = 128;
    assert_no(&c, "CFG-A008");
    c.audio.bitrate_kbps = 320;
    assert_no(&c, "CFG-A008");
    c.audio.bitrate_kbps = 127;
    assert_error(&c, "CFG-A008", "audio.bitrate_kbps");
    c.audio.bitrate_kbps = 321;
    assert_error(&c, "CFG-A008", "audio.bitrate_kbps");
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

#[test]
fn replay_max_seconds_boundaries_are_r001() {
    let mut c = Config::default();
    c.replay.max_seconds = 30;
    assert_no(&c, "CFG-R001");
    c.replay.max_seconds = 3600;
    assert_no(&c, "CFG-R001");
    c.replay.max_seconds = 29;
    assert_error(&c, "CFG-R001", "replay.max_seconds");
    c.replay.max_seconds = 3601;
    assert_error(&c, "CFG-R001", "replay.max_seconds");
}

#[test]
fn ram_max_mb_boundaries_are_r003() {
    let mut c = Config::default();
    c.replay.ram_max_mb = 64;
    assert_no(&c, "CFG-R003");
    c.replay.ram_max_mb = 4096;
    assert_no(&c, "CFG-R003");
    c.replay.ram_max_mb = 63;
    assert_error(&c, "CFG-R003", "replay.ram_max_mb");
    c.replay.ram_max_mb = 4097;
    assert_error(&c, "CFG-R003", "replay.ram_max_mb");
}

#[test]
fn ram_storage_over_budget_is_r002() {
    // Default: video max 15000 kbps + 4 tracks at 128 kbps = 15512 kbps, 300 s, about 569 MB.
    let mut c = Config::default();
    c.replay.storage = ReplayStorage::Ram;
    c.replay.ram_max_mb = 512;
    assert_error(&c, "CFG-R002", "replay.ram_max_mb");

    c.replay.ram_max_mb = 4096;
    assert_no(&c, "CFG-R002");
}

#[test]
fn ram_storage_fits_small_buffer() {
    let mut c = Config::default();
    c.replay.storage = ReplayStorage::Ram;
    c.replay.max_seconds = 30;
    c.replay.ram_max_mb = 64;
    c.hotkeys.clips.clear();
    assert_clean(&c);
}

#[test]
fn disk_storage_ignores_ram_budget() {
    let mut c = Config::default();
    c.replay.storage = ReplayStorage::Disk;
    c.replay.ram_max_mb = 64;
    assert_no(&c, "CFG-R002");
}

// ---------------------------------------------------------------------------
// hotkeys
// ---------------------------------------------------------------------------

#[test]
fn clip_seconds_boundary_is_h004() {
    let mut c = Config::default();
    c.replay.max_seconds = 30;
    c.hotkeys.clips[0].seconds = 30;
    c.hotkeys.clips[1].seconds = 30;
    assert_no(&c, "CFG-H004");
    c.hotkeys.clips[0].seconds = 31;
    assert_error(&c, "CFG-H004", "hotkeys.clips[0].seconds");
}

#[test]
fn clip_seconds_below_five_is_h004() {
    let mut c = Config::default();
    c.hotkeys.clips[0].seconds = 5;
    assert_no(&c, "CFG-H004");
    c.hotkeys.clips[0].seconds = 4;
    assert_error(&c, "CFG-H004", "hotkeys.clips[0].seconds");
}

#[test]
fn clip_count_boundary_is_h005() {
    let names = [
        "Ctrl+F1", "Ctrl+F2", "Ctrl+F3", "Ctrl+F4", "Ctrl+F5", "Ctrl+F6", "Ctrl+F7", "Ctrl+F8",
        "Ctrl+F9",
    ];
    let make = |n: usize| -> Config {
        let mut c = Config::default();
        c.hotkeys.clips = names[..n]
            .iter()
            .enumerate()
            .map(|(i, k)| ClipHotkey {
                name: format!("Clip {i}"),
                keys: chord(k),
                seconds: 30,
                press: Press::Tap,
            })
            .collect();
        c
    };
    assert_clean(&make(8));
    assert_error(&make(9), "CFG-H005", "hotkeys.clips");
}

#[test]
fn clip_name_length_is_h007() {
    let mut c = Config::default();
    c.hotkeys.clips[0].name = "n".repeat(32);
    assert_no(&c, "CFG-H007");
    c.hotkeys.clips[0].name = "n".repeat(33);
    assert_error(&c, "CFG-H007", "hotkeys.clips[0].name");
    c.hotkeys.clips[0].name = String::new();
    assert_error(&c, "CFG-H007", "hotkeys.clips[0].name");
}

#[test]
fn duplicate_chord_clip_and_session_is_h002_once() {
    let mut c = Config::default();
    // Default clip 0 is Alt+F9.
    c.hotkeys.session.split = Some(chord("Alt+F9"));
    let d = validate(&c);
    let h2: Vec<_> = d.iter().filter(|x| x.code == "CFG-H002").collect();
    assert_eq!(h2.len(), 1, "{d:#?}");
    assert_eq!(h2[0].severity, Severity::Error);
    assert_eq!(h2[0].path, "hotkeys.session.split");

    c.hotkeys.session.split = None;
    assert_no(&c, "CFG-H002");
    assert_clean(&c);
}

#[test]
fn reserved_chords_are_h003() {
    let mut c = Config::default();
    c.hotkeys.clips[0].keys = chord("Alt+F4");
    assert_error(&c, "CFG-H003", "hotkeys.clips[0].keys");

    let mut c = Config::default();
    c.hotkeys.session.stop_start = Some(chord("Win+L"));
    assert_error(&c, "CFG-H003", "hotkeys.session.stop_start");

    let mut c = Config::default();
    c.hotkeys.session.split = Some(chord("Ctrl+Alt+F4"));
    assert_no(&c, "CFG-H003");
}

#[test]
fn push_to_talk_without_mute_key_is_h008() {
    let mut c = Config::default();
    c.hotkeys.session.mute_mic_mode = MuteMode::PushToTalk;
    c.hotkeys.session.mute_mic = None;
    assert_error(&c, "CFG-H008", "hotkeys.session.mute_mic_mode");

    c.hotkeys.session.mute_mic = Some(chord("Alt+F7"));
    assert_no(&c, "CFG-H008");
}

// ---------------------------------------------------------------------------
// notifications and advanced
// ---------------------------------------------------------------------------

#[test]
fn low_disk_boundaries_are_n001() {
    let mut c = Config::default();
    c.notifications.low_disk_gb = 1;
    assert_no(&c, "CFG-N001");
    c.notifications.low_disk_gb = 100;
    assert_no(&c, "CFG-N001");
    c.notifications.low_disk_gb = 0;
    assert_error(&c, "CFG-N001", "notifications.low_disk_gb");
    c.notifications.low_disk_gb = 101;
    assert_error(&c, "CFG-N001", "notifications.low_disk_gb");
}

#[test]
fn detect_poll_boundaries_are_x001() {
    let mut c = Config::default();
    c.advanced.detect_poll_ms = 250;
    assert_no(&c, "CFG-X001");
    c.advanced.detect_poll_ms = 5000;
    assert_no(&c, "CFG-X001");
    c.advanced.detect_poll_ms = 249;
    assert_error(&c, "CFG-X001", "advanced.detect_poll_ms");
    c.advanced.detect_poll_ms = 5001;
    assert_error(&c, "CFG-X001", "advanced.detect_poll_ms");
}

#[test]
fn ipc_timeout_boundaries_are_x005() {
    let mut c = Config::default();
    c.advanced.ipc_timeout_ms = 500;
    assert_no(&c, "CFG-X005");
    c.advanced.ipc_timeout_ms = 10_000;
    assert_no(&c, "CFG-X005");
    c.advanced.ipc_timeout_ms = 499;
    assert_error(&c, "CFG-X005", "advanced.ipc_timeout_ms");
    c.advanced.ipc_timeout_ms = 10_001;
    assert_error(&c, "CFG-X005", "advanced.ipc_timeout_ms");
}

// ---------------------------------------------------------------------------
// games
// ---------------------------------------------------------------------------

fn with_game(id: &str, g: GameOverride) -> Config {
    let mut c = Config::default();
    c.games.insert(id.to_owned(), g);
    c
}

#[test]
fn game_id_validity_is_g001() {
    assert_clean(&with_game("cs2.exe@steam:730", GameOverride::default()));
    assert_clean(&with_game("cs2.exe", GameOverride::default()));

    let bad = with_game("cs2", GameOverride::default());
    assert_error(&bad, "CFG-G001", r#"games."cs2""#);

    let no_id = with_game("cs2.exe@steam", GameOverride::default());
    assert_error(&no_id, "CFG-G001", r#"games."cs2.exe@steam""#);
}

#[test]
fn game_display_name_length_is_g002() {
    let ok = GameOverride {
        display_name: Some("n".repeat(64)),
        ..Default::default()
    };
    assert_clean(&with_game("cs2.exe", ok));

    let long = GameOverride {
        display_name: Some("n".repeat(65)),
        ..Default::default()
    };
    assert_error(
        &with_game("cs2.exe", long),
        "CFG-G002",
        r#"games."cs2.exe".display_name"#,
    );

    let empty = GameOverride {
        display_name: Some(String::new()),
        ..Default::default()
    };
    assert_error(
        &with_game("cs2.exe", empty),
        "CFG-G002",
        r#"games."cs2.exe".display_name"#,
    );
}

#[test]
fn game_replay_max_seconds_boundaries_are_g002() {
    let g = |s| GameOverride {
        replay_max_seconds: Some(s),
        ..Default::default()
    };
    assert_clean(&with_game("cs2.exe", g(30)));
    assert_clean(&with_game("cs2.exe", g(3600)));
    assert_error(
        &with_game("cs2.exe", g(29)),
        "CFG-G002",
        r#"games."cs2.exe".replay_max_seconds"#,
    );
    assert_error(
        &with_game("cs2.exe", g(3601)),
        "CFG-G002",
        r#"games."cs2.exe".replay_max_seconds"#,
    );
}

#[test]
fn game_folder_must_be_absolute_unless_it_has_a_variable() {
    let f = |p: &str| GameOverride {
        folder: Some(PathBuf::from(p)),
        ..Default::default()
    };
    assert_clean(&with_game("cs2.exe", f(r"D:\Clips\CS2")));
    assert_clean(&with_game("cs2.exe", f(r"%USERPROFILE%\Clips\CS2")));
    assert_error(
        &with_game("cs2.exe", f(r"games\cs2")),
        "CFG-G002",
        r#"games."cs2.exe".folder"#,
    );
}

// ---------------------------------------------------------------------------
// apply_class
// ---------------------------------------------------------------------------

#[test]
fn identical_configs_are_live() {
    let c = Config::default();
    assert_eq!(apply_class(&c, &c.clone()), ApplyClass::Live);
}

#[test]
fn storage_root_is_next_part() {
    let old = Config::default();
    let mut new = old.clone();
    new.storage.root = PathBuf::from(r"D:\Clips");
    assert_eq!(apply_class(&old, &new), ApplyClass::NextPart);
}

#[test]
fn video_fps_is_pipeline_reinit() {
    let old = Config::default();
    let mut new = old.clone();
    new.video.fps = Fps::F30;
    assert_eq!(apply_class(&old, &new), ApplyClass::PipelineReinit);
}

#[test]
fn replay_storage_is_replay_flush() {
    let old = Config::default();
    let mut new = old.clone();
    new.replay.storage = ReplayStorage::Ram;
    assert_eq!(apply_class(&old, &new), ApplyClass::ReplayFlush);
}

#[test]
fn advanced_log_dir_is_restart() {
    let old = Config::default();
    let mut new = old.clone();
    new.advanced.log_dir = PathBuf::from(r"D:\logs");
    assert_eq!(apply_class(&old, &new), ApplyClass::Restart);
}

#[test]
fn live_keys_stay_live() {
    let old = Config::default();

    let mut new = old.clone();
    new.hotkeys.clips[0].seconds = 45;
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);

    let mut new = old.clone();
    new.detection.stop_grace_s = 60;
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);

    let mut new = old.clone();
    new.storage.quota_gb = 50;
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn several_changes_give_the_strongest_class() {
    let old = Config::default();

    // AudioReinit is stronger than PipelineReinit in the declared order, so this is AudioReinit.
    let mut new = old.clone();
    new.audio.apps[0].name = "Renamed".to_owned();
    new.video.fps = Fps::F30;
    assert_eq!(apply_class(&old, &new), ApplyClass::AudioReinit);

    let mut new = old.clone();
    new.video.fps = Fps::F30;
    new.advanced.log_dir = PathBuf::from(r"D:\logs");
    assert_eq!(apply_class(&old, &new), ApplyClass::Restart);

    let mut new = old.clone();
    new.storage.root = PathBuf::from(r"D:\Clips");
    new.video.fps = Fps::F30;
    assert_eq!(apply_class(&old, &new), ApplyClass::PipelineReinit);
}

#[test]
fn apply_class_ordering() {
    use ApplyClass::*;
    assert!(Live < NextPart);
    assert!(NextPart < PipelineReinit);
    assert!(PipelineReinit < AudioReinit);
    assert!(AudioReinit < ReplayFlush);
    assert!(ReplayFlush < Restart);
    assert_eq!(ApplyClass::default(), Live);
}

#[test]
fn game_folder_is_next_part_and_new_entry_counts() {
    let old = Config::default();

    let mut new = old.clone();
    new.games.insert(
        "cs2.exe@steam:730".to_owned(),
        GameOverride {
            folder: Some(PathBuf::from(r"D:\Clips\CS2")),
            ..Default::default()
        },
    );
    assert_eq!(apply_class(&old, &new), ApplyClass::NextPart);

    let mut new = old.clone();
    new.games.insert(
        "cs2.exe".to_owned(),
        GameOverride {
            display_name: Some("CS2".to_owned()),
            ..Default::default()
        },
    );
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn game_video_override_is_pipeline_reinit() {
    let old = Config::default();
    let mut new = old.clone();
    new.games.insert(
        "cs2.exe".to_owned(),
        GameOverride {
            video: Some(VideoOverride {
                fps: Some(Fps::F30),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    assert_eq!(apply_class(&old, &new), ApplyClass::PipelineReinit);
}

// ---------------------------------------------------------------------------
// general: close_behavior and confirm_exit_while_recording
// ---------------------------------------------------------------------------

#[test]
fn general_close_behavior_and_confirm_exit_are_always_valid() {
    for behavior in [CloseBehavior::MinimizeToTray, CloseBehavior::Exit] {
        for confirm in [true, false] {
            let mut c = Config::default();
            c.general.close_behavior = behavior;
            c.general.confirm_exit_while_recording = confirm;
            assert_clean(&c);
            let d = validate(&c);
            assert!(
                !d.iter()
                    .any(|x| x.path.starts_with("general.close_behavior")
                        || x.path.starts_with("general.confirm_exit_while_recording")),
                "new general keys should not produce diagnostics: {d:#?}"
            );
        }
    }
}

#[test]
fn close_behavior_exit_alone_has_no_diagnostics() {
    let mut c = Config::default();
    c.general.close_behavior = CloseBehavior::Exit;
    c.general.confirm_exit_while_recording = false;
    let d = validate(&c);
    assert!(d.is_empty(), "expected no diagnostics, got {d:#?}");
    assert_eq!(d.len(), validate(&Config::default()).len());
}

#[test]
fn close_behavior_only_change_is_live() {
    let old = Config::default();
    let mut new = old.clone();
    new.general.close_behavior = CloseBehavior::Exit;
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn confirm_exit_only_change_is_live() {
    let old = Config::default();
    let mut new = old.clone();
    new.general.confirm_exit_while_recording = false;
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn general_new_keys_do_not_raise_or_lower_other_classes() {
    let old = Config::default();

    // NextPart from storage.per_game_folders, with both new keys changed.
    let mut new = old.clone();
    new.general.close_behavior = CloseBehavior::Exit;
    new.general.confirm_exit_while_recording = false;
    new.storage.per_game_folders = !old.storage.per_game_folders;
    assert_eq!(apply_class(&old, &new), ApplyClass::NextPart);

    // ReplayFlush from replay.storage, with close_behavior also changed.
    let mut new = old.clone();
    new.general.close_behavior = CloseBehavior::Exit;
    new.replay.storage = ReplayStorage::Ram;
    assert_eq!(apply_class(&old, &new), ApplyClass::ReplayFlush);

    // PipelineReinit from video.fps, with confirm_exit also changed.
    let mut new = old.clone();
    new.general.confirm_exit_while_recording = false;
    new.video.fps = Fps::F30;
    assert_eq!(apply_class(&old, &new), ApplyClass::PipelineReinit);
}
