//! Integration tests for `clipcheck_core::config::schema`.

use std::path::PathBuf;

use clipcheck_core::config::schema::*;

fn parse(text: &str) -> Config {
    Config::from_toml_str(text).expect("config should parse")
}

// 1. The commented template parses to exactly the defaults.
#[test]
fn default_toml_parses_to_default() {
    let parsed = parse(Config::default_toml());
    assert_eq!(parsed, Config::default());
}

// 2. Serializing the defaults and parsing the result gives the defaults back.
#[test]
fn default_serializes_and_round_trips() {
    let text = Config::default()
        .to_toml_string()
        .expect("default config should serialize");
    let back = Config::from_toml_str(&text).expect("serialized default should parse");
    assert_eq!(back, Config::default());
}

// 3. Empty input and partial input keep defaults for every omitted key.
#[test]
fn empty_string_is_all_defaults() {
    assert_eq!(parse(""), Config::default());
}

#[test]
fn partial_file_keeps_other_defaults() {
    let cfg = parse("[video]\nfps = 120\n");
    assert_eq!(cfg.video.fps, Fps::F120);
    let mut expected = Config::default();
    expected.video.fps = Fps::F120;
    assert_eq!(cfg, expected);
}

// 4. Enum string forms.
#[test]
fn enum_string_forms_parse() {
    let text = r#"
[video]
encoder = "nvenc"
codec = "hevc"
container = "mp4"
preset = "custom"
resolution = "1440p"
fps = 30
rate_control = "vbr"

[audio]
codec = "opus"

[replay]
storage = "ram"

[detection]
mode = "allowlist"
after_manual_stop = "until_reenabled"

[storage]
quota_action = "delete_oldest_clips"

[hotkeys.session]
mute_mic_mode = "push_to_talk"

[advanced]
log_level = "trace"
"#;
    let cfg = parse(text);
    assert_eq!(cfg.video.encoder, EncoderChoice::Nvenc);
    assert_eq!(cfg.video.codec, Codec::Hevc);
    assert_eq!(cfg.video.container, Container::Mp4);
    assert_eq!(cfg.video.preset, Preset::Custom);
    assert_eq!(cfg.video.resolution, Resolution::P1440);
    assert_eq!(cfg.video.fps, Fps::F30);
    assert_eq!(cfg.video.rate_control, RateControl::Vbr);
    assert_eq!(cfg.audio.codec, AudioCodec::Opus);
    assert_eq!(cfg.replay.storage, ReplayStorage::Ram);
    assert_eq!(cfg.detection.mode, DetectionMode::Allowlist);
    assert_eq!(
        cfg.detection.after_manual_stop,
        AfterManualStop::UntilReenabled
    );
    assert_eq!(cfg.storage.quota_action, QuotaAction::DeleteOldestClips);
    assert_eq!(cfg.hotkeys.session.mute_mic_mode, MuteMode::PushToTalk);
    assert_eq!(cfg.advanced.log_level, LogLevel::Trace);
}

#[test]
fn remaining_enum_variants_parse() {
    let text = r#"
[video]
encoder = "amf"
codec = "av1"
preset = "low"
resolution = "720p"
rate_control = "cbr"

[advanced]
log_level = "error"
"#;
    let cfg = parse(text);
    assert_eq!(cfg.video.encoder, EncoderChoice::Amf);
    assert_eq!(cfg.video.codec, Codec::Av1);
    assert_eq!(cfg.video.preset, Preset::Low);
    assert_eq!(cfg.video.resolution, Resolution::P720);
    assert_eq!(cfg.video.rate_control, RateControl::Cbr);
    assert_eq!(cfg.advanced.log_level, LogLevel::Error);

    let other = parse("[video]\nencoder = \"qsv\"\nresolution = \"1080p\"\npreset = \"ultra\"\n[advanced]\nlog_level = \"debug\"\n[storage]\nquota_action = \"stop_recording\"\n");
    assert_eq!(other.video.encoder, EncoderChoice::Qsv);
    assert_eq!(other.video.resolution, Resolution::P1080);
    assert_eq!(other.video.preset, Preset::Ultra);
    assert_eq!(other.advanced.log_level, LogLevel::Debug);
    assert_eq!(other.storage.quota_action, QuotaAction::StopRecording);
}

// 5. Container: only "mp4" is valid in v1.
#[test]
fn container_mkv_is_rejected() {
    assert!(Config::from_toml_str("[video]\ncontainer = \"mkv\"\n").is_err());
}

#[test]
fn container_mp4_is_accepted() {
    let cfg = parse("[video]\ncontainer = \"mp4\"\n");
    assert_eq!(cfg.video.container, Container::Mp4);
}

// 6. fps must be the integer 30, 60 or 120.
#[test]
fn fps_45_is_rejected() {
    assert!(Config::from_toml_str("[video]\nfps = 45\n").is_err());
}

#[test]
fn fps_string_is_rejected() {
    assert!(Config::from_toml_str("[video]\nfps = \"60\"\n").is_err());
}

// 7. Unknown keys outside game overrides are accepted (warnings are handled elsewhere).
#[test]
fn unknown_keys_are_accepted() {
    let cfg = parse("top_level_junk = 1\n[general]\nfoo = 1\n[video]\nbar = \"x\"\n");
    assert_eq!(cfg, Config::default());
}

// 8. Unknown keys inside a game override are accepted and reported as CFG-P004 warnings.
#[test]
fn unknown_key_in_game_override_is_accepted_with_warning() {
    let text = "[games.\"cs2.exe\"]\nbogus = 1\n";
    assert!(Config::from_toml_str(text).is_ok());
    let (_, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "CFG-P004");
}

#[test]
fn unknown_key_in_game_video_override_is_accepted_with_warning() {
    let text = "[games.\"cs2.exe\".video]\nbogus = 1\n";
    assert!(Config::from_toml_str(text).is_ok());
    let (_, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "CFG-P004");
}

// 9. Game overrides parse with the expected fields.
#[test]
fn game_overrides_parse() {
    let text = r#"
[games."cs2.exe@steam:730"]
display_name = "CS2"
replay_max_seconds = 120

[games."cs2.exe".video]
preset = "ultra"
fps = 120
"#;
    let cfg = parse(text);
    assert_eq!(cfg.games.len(), 2);

    let store = cfg
        .games
        .get("cs2.exe@steam:730")
        .expect("store-specific key present");
    assert_eq!(store.display_name.as_deref(), Some("CS2"));
    assert_eq!(store.replay_max_seconds, Some(120));
    assert_eq!(store.folder, None);
    assert_eq!(store.auto_record, None);
    assert_eq!(store.replay_enabled, None);
    assert_eq!(store.video, None);

    let plain = cfg.games.get("cs2.exe").expect("plain key present");
    assert_eq!(plain.display_name, None);
    assert_eq!(plain.replay_max_seconds, None);
    let video = plain.video.as_ref().expect("video override present");
    assert_eq!(video.preset, Some(Preset::Ultra));
    assert_eq!(video.fps, Some(Fps::F120));
    assert_eq!(video.resolution, None);
    assert_eq!(video.rate_control, None);
    assert_eq!(video.quality_cq, None);
    assert_eq!(video.bitrate_kbps, None);
    assert_eq!(video.max_bitrate_kbps, None);
}

// 10. Empty-string session hotkeys mean unbound.
#[test]
fn empty_session_hotkey_is_none() {
    let cfg = parse("[hotkeys.session]\nsplit = \"\"\n");
    assert_eq!(cfg.hotkeys.session.split, None);
    let defaults = SessionHotkeys::default();
    assert_eq!(cfg.hotkeys.session.stop_start, defaults.stop_start);
    assert_eq!(cfg.hotkeys.session.toggle_buffer, defaults.toggle_buffer);
    assert_eq!(cfg.hotkeys.session.mute_mic, defaults.mute_mic);
    assert_eq!(cfg.hotkeys.session.mute_mic_mode, defaults.mute_mic_mode);
}

#[test]
fn none_session_hotkey_round_trips_as_empty_string() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.split = None;
    let text = cfg.to_toml_string().expect("serializes");
    assert!(
        text.contains("split = \"\""),
        "expected split = \"\" in output, got:\n{text}"
    );
    let back = parse(&text);
    assert_eq!(back.hotkeys.session.split, None);
    assert_eq!(back, cfg);
}

// 11. Key chord parsing and display.
#[test]
fn key_chord_valid_forms() {
    let alt_f9: KeyChord = "Alt+F9".parse().expect("Alt+F9 parses");
    assert_eq!(alt_f9.to_string(), "Alt+F9");

    let lower: KeyChord = "alt+f9".parse().expect("alt+f9 parses");
    assert_eq!(lower, alt_f9);

    let ctrl_alt: KeyChord = "Ctrl+Alt+F9".parse().expect("parses");
    assert_eq!(ctrl_alt.to_string(), "Ctrl+Alt+F9");

    let shifted: KeyChord = "Shift+Ctrl+A".parse().expect("parses");
    assert_eq!(shifted.to_string(), "Ctrl+Shift+A");

    let bare: KeyChord = "F9".parse().expect("bare F9 parses");
    assert_eq!(bare.to_string(), "F9");
    assert_eq!(bare.mods, Modifiers::default());
}

#[test]
fn key_chord_win_and_digits_display() {
    let c: KeyChord = "win+shift+7".parse().expect("parses");
    assert_eq!(c.to_string(), "Shift+Win+7");
    let f24: KeyChord = "F24".parse().expect("F24 parses");
    assert_eq!(f24.to_string(), "F24");
}

#[test]
fn key_chord_invalid_forms() {
    for bad in [
        "Alt+Alt+F9",
        "Alt+F25",
        "Alt+",
        "Alt+Space",
        "Foo+F9",
        "",
        "F0",
    ] {
        assert!(
            bad.parse::<KeyChord>().is_err(),
            "expected `{bad}` to be rejected"
        );
    }
}

#[test]
fn invalid_chord_in_toml_fails_parse() {
    let text = "[[hotkeys.clips]]\nname = \"x\"\nkeys = \"Alt+Space\"\nseconds = 30\n";
    assert!(Config::from_toml_str(text).is_err());
}

// Three clip hotkeys parse with no count enforcement at schema level.
#[test]
fn three_clip_hotkeys_parse() {
    let text = r#"
[[hotkeys.clips]]
name = "a"
keys = "Alt+F9"
seconds = 30

[[hotkeys.clips]]
name = "b"
keys = "Ctrl+F10"
seconds = 60

[[hotkeys.clips]]
name = "c"
keys = "Shift+F11"
seconds = 90
"#;
    let cfg = parse(text);
    assert_eq!(cfg.hotkeys.clips.len(), 3);
    assert_eq!(cfg.hotkeys.clips[1].keys.to_string(), "Ctrl+F10");
    assert_eq!(cfg.hotkeys.clips[2].seconds, 90);
}

// 17. Malformed TOML is a Parse error.
#[test]
fn malformed_toml_is_parse_error() {
    match Config::from_toml_str("[general") {
        Err(ConfigError::Parse(_)) => {}
        other => panic!("expected ConfigError::Parse, got {other:?}"),
    }
}

// 18. Wrong value type is rejected.
#[test]
fn wrong_type_is_rejected() {
    assert!(Config::from_toml_str("[general]\nstart_minimized = \"yes\"\n").is_err());
}

// 19. Literal-string Windows path round-trips.
#[test]
fn storage_root_literal_path_round_trips() {
    let cfg = parse(
        r"[storage]
root = 'D:\Clips'
",
    );
    assert_eq!(cfg.storage.root, PathBuf::from(r"D:\Clips"));
    let text = cfg.to_toml_string().expect("serializes");
    let back = parse(&text);
    assert_eq!(back, cfg);
    assert_eq!(back.storage.root, PathBuf::from(r"D:\Clips"));
}

// 20. Game override folder round-trips through serialization.
#[test]
fn game_folder_override_round_trips() {
    let mut cfg = Config::default();
    cfg.games.insert(
        "cs2.exe".to_owned(),
        GameOverride {
            folder: Some(PathBuf::from("D:\\Clips\\CS2")),
            ..GameOverride::default()
        },
    );
    let text = cfg.to_toml_string().expect("serializes");
    let back = Config::from_toml_str(&text).expect("re-parses");
    assert_eq!(back, cfg);
    assert_eq!(
        back.games["cs2.exe"].folder,
        Some(PathBuf::from("D:\\Clips\\CS2"))
    );
}

// 21. general.close_behavior and general.confirm_exit_while_recording defaults.
#[test]
fn general_new_keys_have_defaults() {
    let cfg = Config::default();
    assert_eq!(cfg.general.close_behavior, CloseBehavior::MinimizeToTray);
    assert!(cfg.general.confirm_exit_while_recording);
    assert_eq!(CloseBehavior::default(), CloseBehavior::MinimizeToTray);
}

// 22. close_behavior string forms parse.
#[test]
fn close_behavior_string_forms_parse() {
    let exit = parse("[general]\nclose_behavior = \"exit\"\n");
    assert_eq!(exit.general.close_behavior, CloseBehavior::Exit);

    let tray = parse("[general]\nclose_behavior = \"minimize_to_tray\"\n");
    assert_eq!(tray.general.close_behavior, CloseBehavior::MinimizeToTray);
}

// 23. close_behavior = Exit serializes as a snake_case string and round-trips.
#[test]
fn close_behavior_exit_round_trips() {
    let mut cfg = Config::default();
    cfg.general.close_behavior = CloseBehavior::Exit;
    let text = cfg.to_toml_string().expect("serializes");
    assert!(
        text.contains("close_behavior = \"exit\""),
        "expected close_behavior = \"exit\" in output, got:\n{text}"
    );
    let back = parse(&text);
    assert_eq!(back.general.close_behavior, CloseBehavior::Exit);
    assert_eq!(back, cfg);
}

// 24. confirm_exit_while_recording = false parses and round-trips.
#[test]
fn confirm_exit_false_round_trips() {
    let cfg = parse("[general]\nconfirm_exit_while_recording = false\n");
    assert!(!cfg.general.confirm_exit_while_recording);

    let text = cfg.to_toml_string().expect("serializes");
    assert!(
        text.contains("confirm_exit_while_recording = false"),
        "expected confirm_exit_while_recording = false in output, got:\n{text}"
    );
    let back = parse(&text);
    assert!(!back.general.confirm_exit_while_recording);
    assert_eq!(back, cfg);
}

// 25. Unknown enum strings for close_behavior are rejected.
#[test]
fn invalid_close_behavior_is_rejected() {
    assert!(Config::from_toml_str("[general]\nclose_behavior = \"close\"\n").is_err());
    assert!(Config::from_toml_str("[general]\nclose_behavior = \"minimize\"\n").is_err());
}

// 26. Partial [general] sections keep defaults for the omitted new keys.
#[test]
fn partial_general_close_behavior_keeps_other_defaults() {
    let cfg = parse("[general]\nclose_behavior = \"exit\"\n");
    assert_eq!(cfg.general.close_behavior, CloseBehavior::Exit);
    assert!(cfg.general.confirm_exit_while_recording);
    assert!(!cfg.general.autostart);
    assert!(cfg.general.start_minimized);
    assert!(cfg.general.tray_badge_recording);
    assert_eq!(cfg.general.locale, "system");

    let mut expected = Config::default();
    expected.general.close_behavior = CloseBehavior::Exit;
    assert_eq!(cfg, expected);
}

// 27. An empty [general] section, or an empty file, yields both defaults.
#[test]
fn empty_general_section_yields_new_defaults() {
    for text in ["", "[general]\n"] {
        let cfg = parse(text);
        assert_eq!(cfg.general.close_behavior, CloseBehavior::MinimizeToTray);
        assert!(cfg.general.confirm_exit_while_recording);
    }
}

// 28. The commented template documents both new keys.
#[test]
fn default_toml_mentions_new_general_keys() {
    let text = Config::default_toml();
    assert!(text.contains("close_behavior"), "missing close_behavior");
    assert!(
        text.contains("confirm_exit_while_recording"),
        "missing confirm_exit_while_recording"
    );
}
