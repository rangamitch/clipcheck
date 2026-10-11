//! Regression tests for issues #104, #118, #134, #138, #87 and #99 (core config and ids).
//!
//! Public API only. Config-file reads use a per-test directory under the OS temp dir, which is
//! removed when the test ends. Nothing here touches the user's config or recordings.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process;

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;
use clipcheck_core::ids::{normalize_exe, GameId};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A directory under the OS temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("clipcheck-core-b12-{}-{tag}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Named keys that the config layer must read and write, with their Win32 virtual-key codes.
/// This is the table the hotkeys crate registers, minus Escape (always refused) and Space (tested on its own).
const NAMED_KEYS: &[(&str, u16)] = &[
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Numpad0", 0x60),
    ("Numpad1", 0x61),
    ("Numpad2", 0x62),
    ("Numpad3", 0x63),
    ("Numpad4", 0x64),
    ("Numpad5", 0x65),
    ("Numpad6", 0x66),
    ("Numpad7", 0x67),
    ("Numpad8", 0x68),
    ("Numpad9", 0x69),
    ("NumpadMultiply", 0x6A),
    ("NumpadAdd", 0x6B),
    ("NumpadSeparator", 0x6C),
    ("NumpadSubtract", 0x6D),
    ("NumpadDecimal", 0x6E),
    ("NumpadDivide", 0x6F),
    ("Semicolon", 0xBA),
    ("Equals", 0xBB),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backquote", 0xC0),
    ("LeftBracket", 0xDB),
    ("Backslash", 0xDC),
    ("RightBracket", 0xDD),
    ("Quote", 0xDE),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("PrintScreen", 0x2C),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
];

// ---------------------------------------------------------------------------
// #104: config_version doc and the comment rewrite
// ---------------------------------------------------------------------------

// The schema doc now says a missing key reads as CONFIG_VERSION and lower values are accepted.
#[test]
fn issue_104_missing_config_version_reads_as_current_version() {
    let cfg = Config::from_toml_str("").expect("empty config parses");
    assert_eq!(cfg.config_version, CONFIG_VERSION);
}

#[test]
fn issue_104_config_version_zero_is_accepted_by_validate() {
    let cfg = Config::from_toml_str("config_version = 0\n").expect("version 0 parses");
    assert_eq!(cfg.config_version, 0);
    let diags = validate(&cfg);
    assert!(
        diags.iter().all(|d| d.code != "CFG-P003"),
        "version 0 must not raise CFG-P003: {diags:#?}"
    );
}

// ---------------------------------------------------------------------------
// #118: the 1 MiB size limit is enforced
// ---------------------------------------------------------------------------

#[test]
fn issue_118_text_one_byte_over_limit_is_refused_with_cfg_p002() {
    // "#\n" repeated gives exactly MAX_FILE_BYTES bytes of valid TOML (only comments).
    let at_limit = "#\n".repeat((MAX_FILE_BYTES / 2) as usize);
    assert_eq!(at_limit.len() as u64, MAX_FILE_BYTES);
    assert!(
        Config::from_toml_str(&at_limit).is_ok(),
        "at the limit must parse"
    );

    let over = format!("{at_limit}#");
    let err = Config::from_toml_str(&over).expect_err("one byte over must be refused");
    assert!(
        matches!(err, ConfigError::TooLarge { size, limit } if size == MAX_FILE_BYTES + 1 && limit == MAX_FILE_BYTES),
        "{err:?}"
    );
    assert_eq!(err.diagnostic_code(), Some("CFG-P002"));
}

#[test]
fn issue_118_read_config_file_refuses_oversized_file() {
    let dir = TempDir::new("118-over");
    let path = dir.file("config.toml");
    fs::write(&path, vec![b'#'; (MAX_FILE_BYTES + 1) as usize]).expect("write temp file");

    let err = read_config_file(&path).expect_err("oversized file must be refused");
    assert!(
        matches!(err, ConfigError::TooLarge { limit, .. } if limit == MAX_FILE_BYTES),
        "{err:?}"
    );
    assert_eq!(err.diagnostic_code(), Some("CFG-P002"));
}

#[test]
fn issue_118_read_config_file_accepts_file_at_limit() {
    let dir = TempDir::new("118-at");
    let path = dir.file("config.toml");
    let text = "#\n".repeat((MAX_FILE_BYTES / 2) as usize);
    fs::write(&path, text.as_bytes()).expect("write temp file");

    let back = read_config_file(&path).expect("file at the limit is read");
    assert_eq!(back.len() as u64, MAX_FILE_BYTES);
}

#[test]
fn issue_118_read_config_file_rejects_non_utf8_without_a_cfg_code() {
    let dir = TempDir::new("118-utf8");
    let path = dir.file("config.toml");
    fs::write(&path, [0xFF, 0xFE, 0xFD]).expect("write temp file");

    let err = read_config_file(&path).expect_err("non-UTF-8 must be refused");
    assert!(matches!(err, ConfigError::Io(_)), "{err:?}");
    assert_eq!(err.diagnostic_code(), None);
}

#[test]
fn issue_118_syntax_error_maps_to_cfg_p001() {
    let err = Config::from_toml_str("x = = 1\n").expect_err("bad TOML");
    assert_eq!(err.diagnostic_code(), Some("CFG-P001"));
}

// ---------------------------------------------------------------------------
// #134: named hotkey keys are accepted (Escape and Alt+Space stay refused)
// ---------------------------------------------------------------------------

#[test]
fn issue_134_every_named_key_parses_to_its_vk_in_any_case() {
    for &(name, vk) in NAMED_KEYS {
        let chord: KeyChord = format!("Alt+{name}")
            .parse()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(chord, KeyChord::alt(vk), "{name}");
        assert_eq!(chord.to_string(), format!("Alt+{name}"), "{name} display");

        let lower: KeyChord = format!("alt+{}", name.to_ascii_lowercase())
            .parse()
            .unwrap_or_else(|e| panic!("{name} lowercase: {e}"));
        assert_eq!(lower, chord, "{name} lowercase");
    }
}

#[test]
fn issue_134_every_named_key_round_trips_through_config_toml() {
    for &(name, vk) in NAMED_KEYS {
        let mut cfg = Config::default();
        cfg.hotkeys.session.split = Some(KeyChord::alt(vk));
        let text = cfg
            .to_toml_string()
            .unwrap_or_else(|e| panic!("{name} should serialize: {e}"));
        let back = Config::from_toml_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(back, cfg, "{name} round trip");
    }
}

#[test]
fn issue_134_named_key_loads_from_hotkey_toml() {
    let cfg = Config::from_toml_str("[hotkeys.session]\nsplit = \"Alt+Insert\"\n")
        .expect("Insert is a known key");
    assert_eq!(cfg.hotkeys.session.split, Some(KeyChord::alt(0x2D)));
}

#[test]
fn issue_134_space_is_still_refused() {
    assert!("Alt+Space".parse::<KeyChord>().is_err());
    assert!("alt+space".parse::<KeyChord>().is_err());
    assert!(Config::from_toml_str("[hotkeys.session]\nsplit = \"Alt+Space\"\n").is_err());
}

// ---------------------------------------------------------------------------
// #138: one normalization of the exe part, shared by game keys, ExeName and GameId
// ---------------------------------------------------------------------------

#[test]
fn issue_138_normalize_exe_strips_directories_and_one_exe_suffix() {
    assert_eq!(normalize_exe("C:\\Games\\Hades.EXE"), "hades");
    assert_eq!(normalize_exe("/usr/games/Hades.exe"), "hades");
    assert_eq!(normalize_exe("  Hades.exe "), "hades");
    assert_eq!(normalize_exe("a.exe.exe"), "a.exe");
    assert_eq!(normalize_exe(".exe"), "");
}

#[test]
fn issue_138_game_key_and_game_id_share_one_exe_part() {
    for raw in ["Hades.exe", "  HADES.EXE ", "Game.v2.exe", "a.exe.exe"] {
        let id = GameId::from_exe(raw);
        let key = normalize_game_key(raw).expect("valid game key");
        assert_eq!(key, format!("{}.exe", id.exe()), "{raw}");
    }
}

#[test]
fn issue_138_exe_name_matches_game_key() {
    let exe: ExeName = "  Hades.EXE ".parse().expect("valid exe name");
    assert_eq!(exe.as_str(), "hades.exe");
    assert_eq!(
        normalize_game_key("Hades.EXE").as_deref(),
        Some(exe.as_str())
    );
    assert!(".exe".parse::<ExeName>().is_err());
}

// ---------------------------------------------------------------------------
// #87: store ids are lowercased, and deserialization normalizes
// ---------------------------------------------------------------------------

#[test]
fn issue_87_with_store_lowercases_store_id() {
    let upper = GameId::with_store("Hades.exe", "steam", "ABC");
    let lower = GameId::with_store("hades.exe", "STEAM", "abc");
    assert_eq!(upper, lower);
    assert_eq!(upper.as_str(), "hades@steam:abc");
}

#[test]
fn issue_87_new_normalizes_the_whole_text_form() {
    assert_eq!(GameId::new("Hades@Steam:ABC").as_str(), "hades@steam:abc");
    assert_eq!(GameId::new("C:\\Games\\Hades.EXE").as_str(), "hades");
    assert_eq!(
        GameId::new("Hades@Steam:ABC"),
        GameId::with_store("hades.exe", "steam", "abc")
    );
}

#[test]
fn issue_87_deserialize_normalizes_plain_exe() {
    let g: GameId = serde_json::from_str("\"Hades.EXE\"").expect("deserializes");
    assert_eq!(g.as_str(), "hades");
    assert_eq!(g, GameId::from_exe("Hades.exe"));
}

#[test]
fn issue_87_deserialize_normalizes_store_form() {
    let g: GameId = serde_json::from_str("\"  Hades.EXE@Steam:ABC \"").expect("deserializes");
    assert_eq!(g.as_str(), "hades@steam:abc");
    assert_eq!(g, GameId::with_store("Hades.exe", "steam", "abc"));
}

#[test]
fn issue_87_deserialized_id_finds_constructed_id_in_a_set() {
    let mut set = BTreeSet::new();
    set.insert(GameId::with_store("Hades.exe", "steam", "1145360"));
    let parsed: GameId = serde_json::from_str("\"HADES.EXE@STEAM:1145360\"").expect("deserializes");
    assert!(set.contains(&parsed));
}

// ---------------------------------------------------------------------------
// #99: unknown keys are dropped and the file still loads; every unknown key warns with CFG-P004
// ---------------------------------------------------------------------------

#[test]
fn issue_99_unknown_key_in_game_override_loads_with_one_warning() {
    let text = "[games.\"cs2.exe\"]\ndisplay_nme = \"CS2\"\n";
    assert!(
        Config::from_toml_str(text).is_ok(),
        "the file must still load"
    );

    let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
    let game = cfg.games.get("cs2.exe").expect("game override kept");
    assert_eq!(game.display_name, None, "the unknown key is dropped");

    assert_eq!(diags.len(), 1, "{diags:#?}");
    let d = &diags[0];
    assert_eq!(d.code, "CFG-P004");
    assert_eq!(d.severity, Severity::Warning);
    assert_eq!(d.path, "games.\"cs2.exe\".display_nme");
}

#[test]
fn issue_99_unknown_key_in_game_video_table_loads_with_warning() {
    let text = "[games.\"cs2.exe\".video]\nbogus = 1\nfps = 60\n";
    let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
    let video = cfg.games["cs2.exe"]
        .video
        .as_ref()
        .expect("video override kept");
    assert!(video.fps.is_some(), "known keys still apply");

    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "CFG-P004");
    assert_eq!(diags[0].path, "games.\"cs2.exe\".video.bogus");
}

#[test]
fn issue_99_each_unknown_game_key_gets_its_own_warning() {
    let text = "[games.\"cs2.exe\"]\nbogus_a = 1\nbogus_b = 2\n";
    let (_, diags) = Config::from_toml_str_checked(text).expect("file loads");
    let mut paths: Vec<&str> = diags.iter().map(|d| d.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        ["games.\"cs2.exe\".bogus_a", "games.\"cs2.exe\".bogus_b"]
    );
}

// The policy changed: unknown keys outside game overrides now warn too (the old version of this
// test asserted no warnings, which contradicted the policy). Known values still apply.
#[test]
fn issue_99_top_level_unknown_keys_load_with_warning() {
    let (cfg, diags) = Config::from_toml_str_checked(
        "top_level_junk = 1\n[general]\nfoo = 1\n[video]\nbar = \"x\"\n",
    )
    .expect("file loads");
    assert_eq!(cfg, Config::default());
    let mut paths: Vec<&str> = diags.iter().map(|d| d.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(diags.len(), 3, "{diags:#?}");
    assert!(diags.iter().all(|d| d.code == "CFG-P004"), "{diags:#?}");
    assert!(
        paths.iter().any(|p| p.contains("top_level_junk")),
        "{paths:?}"
    );
    assert!(paths.iter().any(|p| p.contains("foo")), "{paths:?}");
    assert!(paths.iter().any(|p| p.contains("bar")), "{paths:?}");
}

#[test]
fn issue99_top_level_unknown_key_is_dropped_with_one_warning() {
    let text = "bogus = 1\n";
    assert!(
        Config::from_toml_str(text).is_ok(),
        "the file must still load"
    );
    let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(cfg, Config::default(), "the unknown key changes nothing");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "CFG-P004");
    assert!(diags[0].path.contains("bogus"), "{}", diags[0].path);
}

#[test]
fn issue99_unknown_key_in_audio_section_warns_and_keeps_known_values() {
    let text = "[audio]\nbitrate_typo = 1\n[audio.mic]\ngain_db = -6.0\n";
    assert!(
        Config::from_toml_str(text).is_ok(),
        "the file must still load"
    );
    let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(cfg.audio.mic.gain_db, -6.0, "known key still applies");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "CFG-P004");
    assert!(diags[0].path.contains("bitrate_typo"), "{}", diags[0].path);
}

#[test]
fn issue99_unknown_key_in_video_section_warns_and_keeps_known_values() {
    let text = "[video]\nfps_typo = 1\nfps = 60\n";
    assert!(
        Config::from_toml_str(text).is_ok(),
        "the file must still load"
    );
    let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(cfg.video.fps, Fps::F60, "known key still applies");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "CFG-P004");
    assert!(diags[0].path.contains("fps_typo"), "{}", diags[0].path);
}

#[test]
fn issue99_unknown_keys_across_sections_give_one_warning_each() {
    let text = "bogus = 1\n[audio]\nbogus_audio = 2\n[video]\nbogus_video = 3\n\
                [games.\"cs2.exe\"]\nbogus_game = 4\n";
    let (_, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(diags.len(), 4, "{diags:#?}");
    assert!(diags.iter().all(|d| d.code == "CFG-P004"), "{diags:#?}");
    for key in ["bogus", "bogus_audio", "bogus_video", "bogus_game"] {
        let hits = diags
            .iter()
            .filter(|d| d.path == key || d.path.ends_with(&format!(".{key}")))
            .count();
        assert_eq!(hits, 1, "expected one warning naming {key}: {diags:#?}");
    }
}

#[test]
fn issue99_legacy_audio_mic_device_is_read_without_warning() {
    let text = "[audio]\nmic_device = \"Legacy Mic\"\n";
    let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert_eq!(cfg.audio.mic.device, "Legacy Mic", "legacy key still read");
    assert!(diags.is_empty(), "legacy key must not warn: {diags:#?}");
}

#[test]
fn issue99_known_keys_only_give_zero_warnings() {
    let default_text = Config::default().to_toml_string().expect("serializes");
    for text in [default_text.as_str(), ""] {
        let (cfg, diags) = Config::from_toml_str_checked(text).expect("file loads");
        assert_eq!(cfg, Config::default(), "text: {text:?}");
        assert!(diags.is_empty(), "text: {text:?}: {diags:#?}");
    }
}

#[test]
fn issue99_checked_and_plain_parse_agree_on_config_with_unknown_keys() {
    let text =
        "bogus = 1\n[video]\nfps = 30\nfps_typo = 1\n[games.\"cs2.exe\"]\ndisplay_nme = \"x\"\n";
    let plain = Config::from_toml_str(text).expect("plain parse loads");
    let (checked, diags) = Config::from_toml_str_checked(text).expect("checked parse loads");
    assert_eq!(
        plain, checked,
        "unknown keys must not change the loaded config"
    );
    assert_eq!(checked.video.fps, Fps::F30);
    assert_eq!(diags.len(), 3, "{diags:#?}");
}

#[test]
fn issue_99_valid_game_override_gives_no_warning() {
    let text = "[games.\"cs2.exe\"]\ndisplay_name = \"CS2\"\n[games.\"cs2.exe\".video]\npreset = \"ultra\"\n";
    let (_, diags) = Config::from_toml_str_checked(text).expect("file loads");
    assert!(diags.is_empty(), "{diags:#?}");
}

#[test]
fn issue_99_checked_parse_still_fails_on_bad_toml() {
    assert!(Config::from_toml_str_checked("[games.\"a.exe\"]\nbogus = 1\n= oops\n").is_err());
}

// Space is a named key: accepted bare and with Ctrl, Shift or Win, refused only as Alt+Space.
#[test]
fn issue_134_space_parses_except_as_alt_space() {
    assert_eq!(
        "Space".parse::<KeyChord>().expect("bare Space"),
        KeyChord {
            mods: Modifiers::default(),
            vk: 0x20,
        }
    );
    assert_eq!(
        "Ctrl+Space".parse::<KeyChord>().expect("Ctrl+Space").vk,
        0x20
    );
    assert_eq!(
        "shift+space".parse::<KeyChord>().expect("Shift+Space").vk,
        0x20
    );
    assert!("Alt+Space".parse::<KeyChord>().is_err());
}
