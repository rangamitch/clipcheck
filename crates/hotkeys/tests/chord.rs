use clipcheck_hotkeys::{Chord, ChordError, Key, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use std::collections::HashSet;
use std::str::FromStr;

fn chord(s: &str) -> Chord {
    s.parse::<Chord>()
        .unwrap_or_else(|e| panic!("expected '{s}' to parse, got {e:?}"))
}

fn err(s: &str) -> ChordError {
    match s.parse::<Chord>() {
        Ok(c) => panic!("expected '{s}' to fail, got {c:?}"),
        Err(e) => e,
    }
}

#[test]
fn ctrl_shift_f9_fields_and_codes() {
    let c = chord("Ctrl+Shift+F9");
    assert!(c.ctrl && c.shift);
    assert!(!c.alt && !c.win);
    assert_eq!(c.key, Key::F9);
    assert_eq!(c.vk_code(), 0x78);
    assert_eq!(c.modifier_flags(), MOD_CONTROL | MOD_SHIFT);
    assert_eq!(c.modifier_flags(), 0x6);
}

#[test]
fn case_and_whitespace_insensitive() {
    let canon = chord("Ctrl+Shift+F9");
    assert_eq!(chord("ctrl+shift+f9"), canon);
    assert_eq!(chord("CTRL+SHIFT+F9"), canon);
    assert_eq!(chord("  Ctrl +  Shift + F9 "), canon);
}

#[test]
fn aliases() {
    assert_eq!(chord("Control+F9"), chord("Ctrl+F9"));
    assert_eq!(chord("Windows+R"), chord("Win+R"));
    assert_eq!(chord("Ctrl+Del").key, Key::Delete);
    assert_eq!(chord("Ctrl+PgUp").key, Key::PageUp);
    assert_eq!(chord("Ctrl+PrtSc").key, Key::PrintScreen);
    assert_eq!(chord("Ctrl+;").key, Key::Semicolon);
    assert_eq!(chord("Ctrl+[").key, Key::LeftBracket);
    assert_eq!(chord("Menu").key, Key::AppsKey);
}

#[test]
fn escape_is_refused_in_every_form() {
    // Escape is reserved: bare, aliased, and with modifiers.
    for s in ["Escape", "Esc", "Ctrl+Esc", "Ctrl+Shift+Esc", "Win+Escape"] {
        match err(s) {
            ChordError::Reserved { reason, .. } => {
                assert!(reason.contains("Escape"), "{s}: reason {reason:?}")
            }
            other => panic!("{s}: expected Reserved, got {other:?}"),
        }
    }
}

#[test]
fn alt_space_is_refused_other_space_chords_parse() {
    match err("Alt+Space") {
        ChordError::Reserved { reason, .. } => {
            assert!(reason.contains("Alt+Space"), "reason {reason:?}")
        }
        other => panic!("expected Reserved for Alt+Space, got {other:?}"),
    }
    assert!(matches!(err("alt + SPACE"), ChordError::Reserved { .. }));

    assert_eq!(chord("Space"), Chord::bare(Key::Space));
    assert_eq!(chord("Ctrl+Space").key, Key::Space);
    assert!(chord("Ctrl+Space").ctrl);
    chord("Ctrl+Alt+Space");
    chord("Shift+Alt+Space");
    chord("Win+Space");
}

#[test]
fn bare_print_screen_parses_and_win_print_screen_parses() {
    assert_eq!(chord("PrintScreen"), Chord::bare(Key::PrintScreen));
    assert_eq!(chord("Win+PrintScreen").key, Key::PrintScreen);
}

#[test]
fn display_is_canonical_order() {
    assert_eq!(
        chord("Shift+Alt+Win+Ctrl+F9").to_string(),
        "Ctrl+Shift+Alt+Win+F9"
    );
    assert_eq!(chord("Win+Alt+R").to_string(), "Alt+Win+R");
    assert_eq!(chord("ctrl+shift+f9").to_string(), "Ctrl+Shift+F9");
}

#[test]
fn display_round_trips_for_many_chords() {
    let keys = [
        Key::A,
        Key::Z,
        Key::Digit0,
        Key::F1,
        Key::F24,
        Key::Left,
        Key::Numpad5,
        Key::Semicolon,
        Key::PrintScreen,
        Key::Space,
    ];
    for mask in 0u32..16 {
        for key in keys {
            let c = Chord {
                ctrl: mask & 1 != 0,
                shift: mask & 2 != 0,
                alt: mask & 4 != 0,
                win: mask & 8 != 0,
                key,
            };
            let text = c.to_string();
            let alt_space_only = key == Key::Space && c.alt && !c.ctrl && !c.shift && !c.win;
            if alt_space_only {
                // Alt+Space is reserved: it must display, then be refused on parse.
                assert!(
                    matches!(text.parse::<Chord>(), Err(ChordError::Reserved { .. })),
                    "'{text}' must be refused"
                );
                continue;
            }
            let back: Chord = text
                .parse()
                .unwrap_or_else(|e| panic!("'{text}' failed: {e:?}"));
            assert_eq!(back, c, "round trip of '{text}'");
        }
    }
}

#[test]
fn modifier_flags_combinations() {
    assert_eq!(chord("Alt+A").modifier_flags(), MOD_ALT);
    assert_eq!(chord("Ctrl+A").modifier_flags(), MOD_CONTROL);
    assert_eq!(chord("Shift+A").modifier_flags(), MOD_SHIFT);
    assert_eq!(chord("Win+A").modifier_flags(), MOD_WIN);
    assert_eq!(chord("Ctrl+Shift+Alt+Win+A").modifier_flags(), 0xF);
    assert_eq!(chord("A").modifier_flags(), 0);
    for s in ["Ctrl+Shift+Alt+Win+F24", "Alt+Win+R"] {
        assert_eq!(
            chord(s).modifier_flags() & 0x4000,
            0,
            "MOD_NOREPEAT must not be set"
        );
    }
}

#[test]
fn has_modifiers_and_bare() {
    assert!(!chord("F9").has_modifiers());
    assert_eq!(chord("F9"), Chord::bare(Key::F9));
    assert!(chord("Win+F9").has_modifiers());
}

#[test]
fn vk_codes() {
    let cases: &[(&str, u32)] = &[
        ("A", 0x41),
        ("Z", 0x5A),
        ("0", 0x30),
        ("9", 0x39),
        ("F1", 0x70),
        ("F12", 0x7B),
        ("F24", 0x87),
        ("Left", 0x25),
        ("Insert", 0x2D),
        ("PageDown", 0x22),
        ("Numpad0", 0x60),
        ("NumpadDivide", 0x6F),
        ("Semicolon", 0xBA),
        ("Quote", 0xDE),
        ("Space", 0x20),
        ("PrintScreen", 0x2C),
    ];
    for (name, vk) in cases {
        assert_eq!(Key::parse(name).unwrap().vk_code(), *vk, "vk for {name}");
    }
    assert_eq!(chord("Ctrl+Numpad0").vk_code(), 0x60);
}

#[test]
fn empty_inputs_error() {
    assert_eq!(err(""), ChordError::Empty);
    assert_eq!(err("   "), ChordError::Empty);
}

#[test]
fn modifiers_only_is_no_key() {
    assert!(matches!(err("Ctrl+Shift"), ChordError::NoKey(_)));
    assert!(matches!(err("Ctrl"), ChordError::NoKey(_)));
}

#[test]
fn empty_segments() {
    assert!(matches!(err("Ctrl++F9"), ChordError::EmptySegment(_)));
    assert!(matches!(err("Ctrl+"), ChordError::EmptySegment(_)));
    assert!(matches!(err("+F9"), ChordError::EmptySegment(_)));
}

#[test]
fn duplicate_modifier() {
    assert!(matches!(
        err("Ctrl+Ctrl+F9"),
        ChordError::DuplicateModifier(_)
    ));
}

#[test]
fn unknown_key() {
    assert!(matches!(err("Ctrl+Foo"), ChordError::UnknownKey(_)));
}

#[test]
fn multiple_keys() {
    assert!(matches!(err("F9+F10"), ChordError::MultipleKeys(_)));
}

#[test]
fn bare_key_parses() {
    assert_eq!(chord("F9"), Chord::bare(Key::F9));
}

#[test]
fn key_parse_and_from_str() {
    assert_eq!(Key::parse("f9"), Some(Key::F9));
    assert_eq!(Key::parse("nonsense"), None);
    assert_eq!(Key::from_str(" F9 "), Ok(Key::F9));
    assert_eq!(
        Key::from_str("nonsense"),
        Err(ChordError::UnknownKey("nonsense".into()))
    );
    assert_eq!(Key::from_str("   "), Err(ChordError::Empty));
}

#[test]
fn key_names_round_trip_and_are_unique() {
    let mut seen = HashSet::new();
    for &k in Key::ALL {
        assert_eq!(Key::parse(k.name()), Some(k), "parse(name) for {:?}", k);
        assert!(
            seen.insert(k.name().to_ascii_lowercase()),
            "duplicate name {}",
            k.name()
        );
    }
    assert_eq!(seen.len(), Key::ALL.len());
}

#[test]
fn key_display_matches_name() {
    for &k in Key::ALL {
        assert_eq!(k.to_string(), k.name());
    }
}
