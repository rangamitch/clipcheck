use clipcheck_hotkeys::{
    reserved, Binding, Chord, Conflict, ConflictKind, HotkeyAction, HotkeyRegistry, PressKind,
};

fn chord(s: &str) -> Chord {
    s.parse::<Chord>().unwrap()
}

fn clip_bind(c: &str, name: &str, seconds: u32, press: PressKind, enabled: bool) -> Binding {
    Binding {
        chord: chord(c),
        action: HotkeyAction::ClipLast {
            name: name.to_string(),
            seconds,
        },
        press,
        enabled,
    }
}

fn clip(c: &str, name: &str, press: PressKind) -> Binding {
    clip_bind(c, name, 30, press, true)
}

fn session_split(c: &str, press: PressKind) -> Binding {
    Binding {
        chord: chord(c),
        action: HotkeyAction::SessionSplit,
        press,
        enabled: true,
    }
}

fn conflicts(bindings: &[Binding], max: u32) -> Vec<Conflict> {
    match HotkeyRegistry::build(bindings, max) {
        Ok(_) => panic!("expected conflicts, got Ok"),
        Err(c) => c,
    }
}

fn has_duplicate(list: &[Conflict]) -> bool {
    list.iter()
        .any(|c| matches!(c.kind, ConflictKind::DuplicateChord { .. }))
}

#[test]
fn tap_and_long_clip_on_same_chord_is_valid() {
    let set = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        clip("Alt+F9", "B", PressKind::Long),
    ];
    let reg = HotkeyRegistry::build(&set, 300).expect("tap+long clips on one chord are valid");

    let tap = reg
        .lookup_press(&chord("Alt+F9"), PressKind::Tap)
        .expect("tap binding");
    assert_eq!(
        tap.action,
        HotkeyAction::ClipLast {
            name: "A".to_string(),
            seconds: 30
        }
    );

    let long = reg
        .lookup_press(&chord("Alt+F9"), PressKind::Long)
        .expect("long binding");
    assert_eq!(
        long.action,
        HotkeyAction::ClipLast {
            name: "B".to_string(),
            seconds: 30
        }
    );
}

#[test]
fn same_press_clips_conflict() {
    let long_pair = vec![
        clip("Alt+F9", "A", PressKind::Long),
        clip("Alt+F9", "B", PressKind::Long),
    ];
    let errs = conflicts(&long_pair, 300);
    assert!(has_duplicate(&errs), "two Long clips: {errs:?}");
    let dup = errs
        .iter()
        .find(|c| matches!(c.kind, ConflictKind::DuplicateChord { .. }))
        .unwrap();
    assert!(
        dup.message.contains("Alt+F9"),
        "message missing chord: {}",
        dup.message
    );
    assert!(
        dup.message.contains("already used"),
        "message missing 'already used': {}",
        dup.message
    );

    let tap_pair = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        clip("Alt+F9", "B", PressKind::Tap),
    ];
    let errs = conflicts(&tap_pair, 300);
    assert!(has_duplicate(&errs), "two Tap clips: {errs:?}");
    let dup = errs
        .iter()
        .find(|c| matches!(c.kind, ConflictKind::DuplicateChord { .. }))
        .unwrap();
    assert!(
        dup.message.contains("Alt+F9"),
        "message missing chord: {}",
        dup.message
    );
    assert!(
        dup.message.contains("already used"),
        "message missing 'already used': {}",
        dup.message
    );
}

#[test]
fn clip_and_session_on_same_chord_conflict_for_both_presses() {
    let long_clip_and_split = vec![
        clip("Alt+F9", "A", PressKind::Long),
        session_split("Alt+F9", PressKind::Tap),
    ];
    let errs = conflicts(&long_clip_and_split, 300);
    assert!(has_duplicate(&errs), "Long clip + SessionSplit: {errs:?}");

    let tap_clip_and_split = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        session_split("Alt+F9", PressKind::Tap),
    ];
    let errs = conflicts(&tap_clip_and_split, 300);
    assert!(has_duplicate(&errs), "Tap clip + SessionSplit: {errs:?}");
}

#[test]
fn disabled_binding_does_not_conflict() {
    let set = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        clip_bind("Alt+F9", "B", 30, PressKind::Tap, false),
    ];
    assert!(
        HotkeyRegistry::build(&set, 300).is_ok(),
        "disabled same-press clip should not conflict"
    );
}

#[test]
fn three_clips_on_one_chord_conflict() {
    let set = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        clip("Alt+F9", "B", PressKind::Long),
        clip("Alt+F9", "C", PressKind::Long),
    ];
    let errs = conflicts(&set, 300);
    assert!(
        errs.iter()
            .any(|c| c.binding == 2 && matches!(c.kind, ConflictKind::DuplicateChord { other: 1 })),
        "second Long should conflict with first Long (index 1): {errs:?}"
    );
}

#[test]
fn different_chords_never_conflict() {
    let set = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        clip("Alt+F10", "B", PressKind::Long),
    ];
    assert!(
        HotkeyRegistry::build(&set, 300).is_ok(),
        "different chords should not conflict"
    );
}

#[test]
fn lookup_press_distinguishes() {
    let set = vec![
        clip("Alt+F9", "A", PressKind::Tap),
        clip("Alt+F9", "B", PressKind::Long),
        clip("Alt+F10", "C", PressKind::Tap),
    ];
    let reg = HotkeyRegistry::build(&set, 300).unwrap();

    let long = reg
        .lookup_press(&chord("Alt+F9"), PressKind::Long)
        .expect("long clip");
    assert!(matches!(
        &long.action,
        HotkeyAction::ClipLast { name, .. } if name == "B"
    ));
    assert_eq!(long.press, PressKind::Long);

    let tap = reg
        .lookup_press(&chord("Alt+F9"), PressKind::Tap)
        .expect("tap clip");
    assert!(matches!(
        &tap.action,
        HotkeyAction::ClipLast { name, .. } if name == "A"
    ));
    assert_eq!(tap.press, PressKind::Tap);

    // A chord with only a Tap binding has no Long binding.
    assert!(reg
        .lookup_press(&chord("Alt+F10"), PressKind::Long)
        .is_none());
    // A chord with no binding at all returns None for both presses.
    assert!(reg
        .lookup_press(&chord("Alt+F11"), PressKind::Tap)
        .is_none());
    assert!(reg
        .lookup_press(&chord("Alt+F11"), PressKind::Long)
        .is_none());
}

#[test]
fn existing_reserved_chord_still_blocked_for_long_press() {
    assert!(reserved(&chord("Alt+F4")).is_some(), "Alt+F4 is reserved");
    let set = vec![clip("Alt+F4", "A", PressKind::Long)];
    let errs = conflicts(&set, 300);
    assert!(
        errs.iter()
            .any(|c| c.binding == 0 && matches!(c.kind, ConflictKind::ReservedByWindows { .. })),
        "expected ReservedByWindows: {errs:?}"
    );
}
