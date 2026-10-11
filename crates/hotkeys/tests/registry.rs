use clipcheck_hotkeys::{
    reserved, Binding, Chord, Conflict, ConflictKind, HotkeyAction, HotkeyRegistry, Key, PressKind,
};

fn chord(s: &str) -> Chord {
    s.parse::<Chord>().unwrap()
}

fn bind(c: &str, action: HotkeyAction, enabled: bool) -> Binding {
    Binding {
        chord: chord(c),
        action,
        press: PressKind::Tap,
        enabled,
    }
}

fn clip(name: &str, seconds: u32) -> HotkeyAction {
    HotkeyAction::ClipLast {
        name: name.to_string(),
        seconds,
    }
}

fn on(c: &str, action: HotkeyAction) -> Binding {
    bind(c, action, true)
}

fn conflicts(bindings: &[Binding], max: u32) -> Vec<Conflict> {
    match HotkeyRegistry::build(bindings, max) {
        Ok(_) => panic!("expected conflicts, got Ok"),
        Err(c) => c,
    }
}

fn has_kind(list: &[Conflict], idx: usize, pred: impl Fn(&ConflictKind) -> bool) -> bool {
    list.iter().any(|c| c.binding == idx && pred(&c.kind))
}

/// Compile-time check only: `Conflict` must implement `std::error::Error`.
/// Never called; the coercion stops compiling if the impl is removed.
#[allow(dead_code)]
fn conflict_is_std_error(c: &Conflict) -> &dyn std::error::Error {
    c
}

#[test]
fn valid_set_builds_and_lookup_works() {
    let set = vec![
        on("Ctrl+Shift+F9", clip("30s", 30)),
        on("Ctrl+Shift+F10", clip("2m", 120)),
        on("Ctrl+Shift+F11", HotkeyAction::SessionSplit),
        bind("Ctrl+Shift+F12", HotkeyAction::MuteMic, false),
    ];
    let reg = HotkeyRegistry::build(&set, 300).expect("valid set");
    assert_eq!(reg.bindings().len(), 4);
    assert_eq!(
        reg.lookup(&chord("Ctrl+Shift+F9")).unwrap().action,
        clip("30s", 30)
    );
    assert_eq!(
        reg.lookup(&chord("ctrl+shift+f10")).unwrap().action,
        clip("2m", 120)
    );
    assert!(
        reg.lookup(&chord("Ctrl+Shift+F12")).is_none(),
        "disabled binding not looked up"
    );
    assert!(reg.lookup(&chord("Ctrl+Shift+F1")).is_none());
}

#[test]
fn enabled_yields_only_enabled() {
    let set = vec![
        on("Ctrl+Shift+F9", clip("a", 5)),
        bind("Ctrl+Shift+F10", HotkeyAction::ToggleBuffer, false),
        on("Ctrl+Shift+F11", HotkeyAction::SessionSplit),
    ];
    let reg = HotkeyRegistry::build(&set, 60).unwrap();
    let got: Vec<_> = reg.enabled().map(|b| b.chord).collect();
    assert_eq!(got, vec![chord("Ctrl+Shift+F9"), chord("Ctrl+Shift+F11")]);
}

#[test]
fn duplicate_enabled_chord_reports_first_index() {
    let set = vec![
        on("Ctrl+Shift+F9", clip("a", 5)),
        on("Shift+Ctrl+F9", HotkeyAction::SessionSplit),
    ];
    let errs = conflicts(&set, 60);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].binding, 1);
    assert_eq!(errs[0].kind, ConflictKind::DuplicateChord { other: 0 });
    assert!(!errs[0].message.is_empty());
    assert!(
        errs[0].message.contains("Ctrl+Shift+F9"),
        "message: {}",
        errs[0].message
    );
}

#[test]
fn duplicate_with_disabled_is_ok() {
    let set = vec![
        bind("Ctrl+Shift+F9", clip("a", 5), false),
        on("Ctrl+Shift+F9", HotkeyAction::SessionSplit),
    ];
    assert!(HotkeyRegistry::build(&set, 60).is_ok());
}

#[test]
fn reserved_chords_are_blocked() {
    let cases = [
        "Ctrl+Alt+Delete",
        "Win+L",
        "Alt+F4",
        "Alt+Tab",
        "Win+G",
        "Win+Alt+R",
        "Win+Alt+G",
        "Win+Shift+S",
        "Win+PrintScreen",
    ];
    for s in cases {
        let c = chord(s);
        let name = reserved(&c).unwrap_or_else(|| panic!("{s} should be reserved"));
        let errs = conflicts(&[on(s, HotkeyAction::SessionSplit)], 60);
        assert!(
            has_kind(
                &errs,
                0,
                |k| matches!(k, ConflictKind::ReservedByWindows { name: n } if *n == name)
            ),
            "{s}: expected ReservedByWindows, got {errs:?}"
        );
    }
}

#[test]
fn reserved_none_for_unblocked_chords() {
    for s in [
        "Ctrl+Shift+F9",
        "Ctrl+Alt+F12",
        "Win+Shift+R",
        "Ctrl+Space",
        "PrintScreen",
    ] {
        assert!(reserved(&chord(s)).is_none(), "{s} should not be reserved");
    }
}

#[test]
fn alt_space_is_reserved_when_built_directly() {
    // The parser refuses "Alt+Space" (see tests/chord.rs), so build the
    // chord from its public fields to check the registry rule as well.
    let alt_space = Chord {
        alt: true,
        ..Chord::bare(Key::Space)
    };
    assert_eq!(reserved(&alt_space), Some("Alt+Space (window menu)"));

    let errs = conflicts(
        &[Binding {
            chord: alt_space,
            action: HotkeyAction::SessionSplit,
            press: PressKind::Tap,
            enabled: true,
        }],
        60,
    );
    assert!(
        has_kind(&errs, 0, |k| matches!(
            k,
            ConflictKind::ReservedByWindows { name } if *name == "Alt+Space (window menu)"
        )),
        "{errs:?}"
    );
}

#[test]
fn space_chords_other_than_alt_space_are_allowed() {
    for s in [
        "Space",
        "Ctrl+Space",
        "Shift+Space",
        "Ctrl+Alt+Space",
        "Win+Space",
    ] {
        assert!(
            HotkeyRegistry::build(&[on(s, HotkeyAction::SessionSplit)], 60).is_ok(),
            "{s} should be accepted"
        );
    }
}

#[test]
fn bare_keys_are_accepted() {
    for s in ["F9", "PrintScreen", "Pause", "Space", "Delete"] {
        assert!(
            HotkeyRegistry::build(&[on(s, HotkeyAction::SessionSplit)], 60).is_ok(),
            "bare {s} should be accepted"
        );
    }
}

#[test]
fn bare_f13_to_f24_allowed() {
    let keys = [
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];
    let set: Vec<Binding> = keys
        .iter()
        .map(|&k| Binding {
            chord: Chord::bare(k),
            action: HotkeyAction::MuteMic,
            press: PressKind::Tap,
            enabled: true,
        })
        .collect();
    // Same action on different chords is fine; chords are distinct.
    assert!(HotkeyRegistry::build(&set, 60).is_ok());
}

#[test]
fn clip_seconds_bounds_default_max() {
    let ok = |s| HotkeyRegistry::build(&[on("Ctrl+Shift+F9", clip("a", s))], 300).is_ok();
    assert!(!ok(0));
    assert!(ok(1));
    assert!(ok(300));
    let errs = conflicts(&[on("Ctrl+Shift+F9", clip("a", 301))], 300);
    assert!(has_kind(&errs, 0, |k| *k
        == ConflictKind::DurationOutOfRange {
            seconds: 301,
            max_seconds: 300
        }));
}

#[test]
fn clip_seconds_bounds_max_60() {
    let ok = |s| HotkeyRegistry::build(&[on("Ctrl+Shift+F9", clip("a", s))], 60).is_ok();
    assert!(ok(1));
    assert!(ok(60));
    assert!(!ok(61));
    let errs = conflicts(&[on("Ctrl+Shift+F9", clip("a", 61))], 60);
    assert!(has_kind(&errs, 0, |k| *k
        == ConflictKind::DurationOutOfRange {
            seconds: 61,
            max_seconds: 60
        }));
}

#[test]
fn empty_clip_name() {
    for name in ["", "   "] {
        let errs = conflicts(&[on("Ctrl+Shift+F9", clip(name, 10))], 60);
        assert!(
            has_kind(&errs, 0, |k| *k == ConflictKind::EmptyClipName),
            "name {name:?}: {errs:?}"
        );
    }
}

#[test]
fn duplicate_clip_name() {
    let set = vec![
        on("Ctrl+Shift+F9", clip("same", 10)),
        on("Ctrl+Shift+F10", clip("same", 20)),
    ];
    let errs = conflicts(&set, 60);
    assert!(
        has_kind(&errs, 1, |k| *k
            == ConflictKind::DuplicateClipName { other: 0 }),
        "{errs:?}"
    );
}

#[test]
fn eight_clip_hotkeys_ok_nine_rejected() {
    let chords: Vec<String> = (1..=9).map(|i| format!("Ctrl+Shift+F{i}")).collect();
    let eight: Vec<Binding> = (0..8)
        .map(|i| on(&chords[i], clip(&format!("c{i}"), 10)))
        .collect();
    assert!(HotkeyRegistry::build(&eight, 60).is_ok());

    let nine: Vec<Binding> = (0..9)
        .map(|i| on(&chords[i], clip(&format!("c{i}"), 10)))
        .collect();
    let errs = conflicts(&nine, 60);
    assert!(
        has_kind(&errs, 8, |k| *k
            == ConflictKind::TooManyClipHotkeys { max: 8 }),
        "{errs:?}"
    );
}

#[test]
fn nine_clip_hotkeys_with_one_disabled_ok() {
    let chords: Vec<String> = (1..=9).map(|i| format!("Ctrl+Shift+F{i}")).collect();
    let mut set: Vec<Binding> = (0..9)
        .map(|i| on(&chords[i], clip(&format!("c{i}"), 10)))
        .collect();
    set[0].enabled = false;
    assert!(HotkeyRegistry::build(&set, 60).is_ok());
}

#[test]
fn multiple_conflicts_reported_exactly_once_each() {
    let set = vec![
        on("Alt+F4", clip("a", 500)),      // reserved + out of range
        on("Ctrl+Shift+F9", clip("", 10)), // empty clip name
    ];
    let errs = conflicts(&set, 300);
    let got: Vec<(usize, ConflictKind)> =
        errs.iter().map(|c| (c.binding, c.kind.clone())).collect();
    assert_eq!(
        got,
        vec![
            (
                0,
                ConflictKind::ReservedByWindows {
                    name: "Alt+F4 (close window)"
                }
            ),
            (
                0,
                ConflictKind::DurationOutOfRange {
                    seconds: 500,
                    max_seconds: 300
                }
            ),
            (1, ConflictKind::EmptyClipName),
        ]
    );
}

#[test]
fn conflict_display_is_message() {
    let errs = conflicts(&[on("Alt+F4", clip("a", 10))], 60);
    assert!(!errs.is_empty());
    for c in &errs {
        assert_eq!(c.to_string(), c.message);
        assert!(!c.message.is_empty());
    }
}
