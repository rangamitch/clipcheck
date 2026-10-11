//! Tests for issues #71, #145 (and #141, which is covered by nothing at runtime).
//!
//! #71: unnamed virtual-key codes must fail serialization with a clear error (no panic),
//!      save must return Err, and validation must report CFG-H001 at the chord's path.
//! #145: Display text is canonical (Ctrl, Alt, Shift, Win, then key), and hotkey duplicate
//!      diagnostics (CFG-H002) name both sites with correct paths.

use std::fmt::Write as _;

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

const MODIFIER_NAMES: [&str; 4] = ["Ctrl", "Alt", "Shift", "Win"];

fn all_mods(mask: u8) -> Modifiers {
    Modifiers {
        ctrl: mask & 1 != 0,
        alt: mask & 2 != 0,
        shift: mask & 4 != 0,
        win: mask & 8 != 0,
    }
}

/// Canonical text built independently of the implementation under test.
fn expected_text(mask: u8, key: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for (bit, name) in MODIFIER_NAMES.iter().enumerate() {
        if mask & (1 << bit) != 0 {
            parts.push(name);
        }
    }
    parts.push(key);
    parts.join("+")
}

/// Every named key the writer accepts, with its expected display name.
fn named_keys() -> Vec<(u16, String)> {
    let mut keys = Vec::new();
    for vk in 0x30..=0x39u16 {
        keys.push((vk, char::from(vk as u8).to_string()));
    }
    for vk in 0x41..=0x5Au16 {
        keys.push((vk, char::from(vk as u8).to_string()));
    }
    for vk in 0x70..=0x87u16 {
        keys.push((vk, format!("F{}", vk - 0x6F)));
    }
    keys
}

fn diags_with<'a>(d: &'a [Diagnostic], code: &str) -> Vec<&'a Diagnostic> {
    d.iter().filter(|x| x.code == code).collect()
}

fn chord(s: &str) -> KeyChord {
    s.parse().expect("test chord should parse")
}

// ---------------------------------------------------------------------------
// #145: canonical Display text
// ---------------------------------------------------------------------------

#[test]
fn display_is_canonical_modifier_order_for_all_masks() {
    for mask in 0..16u8 {
        for (vk, key) in named_keys() {
            let c = KeyChord {
                mods: all_mods(mask),
                vk,
            };
            assert_eq!(
                c.to_string(),
                expected_text(mask, &key),
                "mask {mask} vk {vk:#X}"
            );
        }
    }
}

#[test]
fn display_known_strings_are_unchanged() {
    assert_eq!(chord("ctrl+alt+f9").to_string(), "Ctrl+Alt+F9");
    assert_eq!(chord("ctrl+shift+f9").to_string(), "Ctrl+Shift+F9");
    assert_eq!(
        chord("win+shift+alt+ctrl+l").to_string(),
        "Ctrl+Alt+Shift+Win+L"
    );
    assert_eq!(KeyChord::alt(0x87).to_string(), "Alt+F24");
    assert_eq!(KeyChord::alt(0x30).to_string(), "Alt+0");
}

#[test]
fn named_keys_round_trip_display_parse_for_all_masks() {
    for mask in 0..16u8 {
        for (vk, _) in named_keys() {
            let c = KeyChord {
                mods: all_mods(mask),
                vk,
            };
            let text = c.to_string();
            let back: KeyChord = text.parse().expect("display text parses");
            assert_eq!(back, c, "{text}");
        }
    }
}

#[test]
fn named_keys_round_trip_serde_json_for_all_masks() {
    for mask in 0..16u8 {
        for (vk, _) in named_keys() {
            let c = KeyChord {
                mods: all_mods(mask),
                vk,
            };
            let json = serde_json::to_string(&c).expect("serializes");
            assert_eq!(json, format!("\"{}\"", c), "{json}");
            let back: KeyChord = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(back, c, "{json}");
        }
    }
}

#[test]
fn named_keys_round_trip_toml_inside_config_for_all_masks() {
    for mask in 0..16u8 {
        for (vk, _) in named_keys() {
            let c = KeyChord {
                mods: all_mods(mask),
                vk,
            };
            let mut config = Config::default();
            config.hotkeys.session.split = Some(c);
            let text = config.to_toml_string().expect("saves");
            let back = Config::from_toml_str(&text).expect("loads");
            assert_eq!(back.hotkeys.session.split, Some(c), "{text}");
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing: case, whitespace, duplicates, unknown names, empty input
// ---------------------------------------------------------------------------

#[test]
fn parse_is_case_insensitive() {
    let c = chord("ctrl+shift+f9");
    assert_eq!(
        c,
        KeyChord {
            mods: Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
            vk: 0x78,
        }
    );
    assert_eq!(chord("WIN+alt+a").vk, 0x41);
    assert!(chord("WIN+alt+a").mods.win);
}

#[test]
fn parse_tolerates_surrounding_whitespace() {
    // Whitespace around '+' and around the whole string is trimmed by the parser.
    assert_eq!(chord("  Ctrl +  Shift + f9 "), chord("Ctrl+Shift+F9"));
}

#[test]
fn parse_rejects_duplicate_modifiers_in_any_case() {
    for s in ["Alt+Alt+F9", "ctrl+Ctrl+F9", "Win+win+L", "Shift+SHIFT+A"] {
        assert!(s.parse::<KeyChord>().is_err(), "{s} should fail");
    }
}

#[test]
fn parse_rejects_unknown_keys_and_modifiers() {
    for s in [
        "Ctrl+Foo",
        "Ctrl+F0",
        "Ctrl+F25",
        "Ctrl+F99999",
        "Ctrl+,",
        "Ctrl+\u{00E9}",
        "Meta+F9",
        "F9+Ctrl",
        "Ctrl",
        "Shift",
        "+1",
        "Ctrl+",
        "Ctrl+F9+",
    ] {
        assert!(s.parse::<KeyChord>().is_err(), "{s:?} should fail");
    }
}

#[test]
fn parse_rejects_empty_and_blank_strings() {
    for s in ["", "   ", "+", " + "] {
        assert!(s.parse::<KeyChord>().is_err(), "{s:?} should fail");
    }
}

#[test]
fn parse_rejects_bad_text_through_serde_and_config() {
    assert!(serde_json::from_str::<KeyChord>("\"\"").is_err());
    assert!(serde_json::from_str::<KeyChord>("\"Alt+Alt+F9\"").is_err());
    assert!(serde_json::from_str::<KeyChord>("\"Ctrl+Foo\"").is_err());
    let toml_text = "[hotkeys.session]\nsplit = \"Alt+Alt+F9\"\n";
    assert!(Config::from_toml_str(toml_text).is_err());
}

#[test]
fn parse_never_panics_on_odd_inputs() {
    let odd = [
        "\0",
        "+++",
        "Ctrl++F9",
        "\u{FEFF}Ctrl+F9",
        "Ctrl+F9\n",
        "é",
        "💥",
        "F\u{0660}",
        "Ctrl+F\u{0660}",
        "99999999999999999999999999",
        "Alt+F-1",
        "Alt+F+1",
    ];
    for s in odd {
        let _ = s.parse::<KeyChord>();
    }
}

#[test]
fn every_named_key_candidate_parses_to_its_vk() {
    for (vk, name) in named_keys() {
        let c: KeyChord = name.parse().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(c.vk, vk, "{name}");
        assert_eq!(c.mods, Modifiers::default(), "{name}");
        let lower: KeyChord = name.to_ascii_lowercase().parse().expect("lowercase parses");
        assert_eq!(lower.vk, vk, "{name} lowercase");
    }
}

// ---------------------------------------------------------------------------
// #71: unnamed key codes
// ---------------------------------------------------------------------------

#[test]
fn unnamed_key_serde_error_has_clear_message() {
    let err = serde_json::to_string(&KeyChord::alt(0x1B)).expect_err("must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("key code 0x1B has no name ClipCheck can write. Use A-Z, 0-9 or F1-F24"),
        "{msg}"
    );
}

#[test]
fn unnamed_key_display_via_write_does_not_error_out_of_band() {
    // write! must return Err (not panic) for an unnamed code.
    let mut s = String::new();
    let _ = write!(s, "{}", KeyChord::alt(0x1B));
}

#[test]
fn unnamed_key_to_string_does_not_panic() {
    // Expected by the spec: constructing and Display-ing an unnamed chord never panics.
    let result = std::panic::catch_unwind(|| KeyChord::alt(0x1B).to_string());
    assert!(result.is_ok(), "KeyChord::to_string panicked for vk 0x1B");
}

#[test]
fn unnamed_key_save_returns_err_not_panic() {
    let mut config = Config::default();
    config.hotkeys.session.split = Some(KeyChord::alt(0x1B));
    let err = config.to_toml_string().expect_err("save must fail");
    assert!(
        err.to_string().contains("has no name ClipCheck can write"),
        "{err}"
    );
}

#[test]
fn unnamed_key_in_clip_hotkey_save_returns_err() {
    let mut config = Config::default();
    config.hotkeys.clips[0].keys = KeyChord::alt(0xE8);
    assert!(config.to_toml_string().is_err());
}

#[test]
fn every_u16_vk_serializes_or_errors_and_round_trips() {
    let mut failures: Vec<String> = Vec::new();
    for vk in 0..=u16::MAX {
        let c = KeyChord::alt(vk);
        // Display is infallible (unnamed keys show as `VK 0x..`); serialization is the authority on
        // whether a key has a writable name.
        let shown = c.to_string();
        assert!(!shown.is_empty(), "vk {vk:#X}: empty Display");
        let named = serde_json::to_string(&c).is_ok();
        match serde_json::to_string(&c) {
            Ok(json) => {
                let inner = json.trim_matches('"');
                match inner.parse::<KeyChord>() {
                    Ok(back) if back == c => {}
                    other => failures.push(format!("vk {vk:#X}: {json} parsed as {other:?}")),
                }
            }
            Err(e) => {
                if named {
                    failures.push(format!("vk {vk:#X}: named but serialize failed: {e}"));
                } else if !e.to_string().contains("has no name ClipCheck can write") {
                    failures.push(format!("vk {vk:#X}: unclear error: {e}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first: {:?}",
        failures.len(),
        failures.iter().take(5).collect::<Vec<_>>()
    );
}

#[test]
fn unnamed_key_validation_reports_h001_at_session_path() {
    let mut config = Config::default();
    config.hotkeys.session.split = Some(KeyChord::alt(0x07));
    let d = validate(&config);
    let h1 = diags_with(&d, "CFG-H001");
    assert_eq!(h1.len(), 1, "{d:#?}");
    assert_eq!(h1[0].severity, Severity::Error);
    assert_eq!(h1[0].path, "hotkeys.session.split");
    assert!(h1[0].message.contains("0x07"), "{}", h1[0].message);
}

#[test]
fn unnamed_key_validation_reports_h001_at_clip_path() {
    let mut config = Config::default();
    config.hotkeys.clips[1].keys = KeyChord::alt(0x07);
    let d = validate(&config);
    let h1 = diags_with(&d, "CFG-H001");
    assert_eq!(h1.len(), 1, "{d:#?}");
    assert_eq!(h1[0].path, "hotkeys.clips[1].keys");
    assert_eq!(h1[0].severity, Severity::Error);
}

#[test]
fn named_key_does_not_report_h001() {
    let mut config = Config::default();
    config.hotkeys.session.split = Some(chord("Alt+F24"));
    assert!(diags_with(&validate(&config), "CFG-H001").is_empty());
}

#[test]
fn validate_never_panics_for_any_unnamed_session_code() {
    // #134 made the named-key table larger (Backspace, arrows, numpad, punctuation, ...), and
    // that table is private. A code counts as named when its chord text parses back through the
    // public FromStr, which is the same contract the config file relies on.
    for vk in 0..=u16::MAX {
        let chord = KeyChord::alt(vk);
        let named = chord
            .to_string()
            .parse::<KeyChord>()
            .is_ok_and(|parsed| parsed == chord);
        let mut config = Config::default();
        config.hotkeys.session.split = Some(chord);
        let d = validate(&config);
        let expected = usize::from(!named);
        assert_eq!(diags_with(&d, "CFG-H001").len(), expected, "vk {vk:#X}");
    }
}

// ---------------------------------------------------------------------------
// #145: hotkey duplicate diagnostics name both sites
// ---------------------------------------------------------------------------

#[test]
fn duplicate_clip_chords_report_h002_naming_both_clips() {
    let mut config = Config::default();
    config.hotkeys.clips = vec![
        ClipHotkey {
            name: "Alpha".to_owned(),
            keys: chord("Alt+F9"),
            seconds: 30,
            press: Press::Tap,
        },
        ClipHotkey {
            name: "Beta".to_owned(),
            keys: chord("Alt+F9"),
            seconds: 30,
            press: Press::Tap,
        },
    ];
    let d = validate(&config);
    let h2 = diags_with(&d, "CFG-H002");
    assert_eq!(h2.len(), 1, "{d:#?}");
    assert_eq!(h2[0].path, "hotkeys.clips[1].keys");
    assert_eq!(h2[0].severity, Severity::Error);
    assert!(h2[0].message.contains("Alt+F9"), "{}", h2[0].message);
    assert!(
        h2[0].message.contains("the Alpha hotkey"),
        "{}",
        h2[0].message
    );
    assert!(
        h2[0].message.contains("the Beta hotkey"),
        "{}",
        h2[0].message
    );
}

#[test]
fn clip_equal_to_session_mute_mic_reports_h002_at_session_path() {
    let mut config = Config::default();
    config.hotkeys.clips = vec![ClipHotkey {
        name: "Quick".to_owned(),
        keys: chord("Alt+F7"),
        seconds: 30,
        press: Press::Tap,
    }];
    assert_eq!(config.hotkeys.session.mute_mic, Some(chord("Alt+F7")));
    let d = validate(&config);
    let h2 = diags_with(&d, "CFG-H002");
    assert_eq!(h2.len(), 1, "{d:#?}");
    assert_eq!(h2[0].path, "hotkeys.session.mute_mic");
    assert!(
        h2[0].message.contains("the Quick hotkey"),
        "{}",
        h2[0].message
    );
    assert!(
        h2[0].message.contains("the session mute_mic hotkey"),
        "{}",
        h2[0].message
    );
}

#[test]
fn duplicate_detection_ignores_modifier_order_and_case() {
    let mut config = Config::default();
    config.hotkeys.clips = vec![
        ClipHotkey {
            name: "One".to_owned(),
            keys: chord("ctrl+shift+f9"),
            seconds: 30,
            press: Press::Tap,
        },
        ClipHotkey {
            name: "Two".to_owned(),
            keys: chord("Shift+Ctrl+F9"),
            seconds: 30,
            press: Press::Tap,
        },
    ];
    let d = validate(&config);
    assert_eq!(diags_with(&d, "CFG-H002").len(), 1, "{d:#?}");
}

#[test]
fn default_config_has_no_hotkey_duplicates_or_unnamed_keys() {
    let d = validate(&Config::default());
    assert!(diags_with(&d, "CFG-H002").is_empty(), "{d:#?}");
    assert!(diags_with(&d, "CFG-H001").is_empty(), "{d:#?}");
}

#[test]
fn reserved_chord_reports_h003_at_clip_path() {
    // The other chord problem the code defines besides duplicates.
    let mut config = Config::default();
    config.hotkeys.clips[0].keys = chord("Alt+F4");
    let d = validate(&config);
    let h3 = diags_with(&d, "CFG-H003");
    assert_eq!(h3.len(), 1, "{d:#?}");
    assert_eq!(h3[0].path, "hotkeys.clips[0].keys");
}
