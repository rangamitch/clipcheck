//! Regression tests for hotkeys issues #43 (clip-hotkey limit exposed once and
//! used in the error) and #46 (Chord Display output is unchanged after the
//! allocation-free rewrite). Public API only; no Win32 calls.
//!
//! Not covered here: #43's "core and hotkeys limits are equal" check needs
//! `clipcheck-core`, which this crate does not depend on, and core's copy is
//! private to that crate. That check belongs in a core test.

use clipcheck_hotkeys::{Binding, Chord, ConflictKind, HotkeyAction, HotkeyRegistry, PressKind};

fn chord(s: &str) -> Chord {
    s.parse::<Chord>().unwrap()
}

fn on(c: &str, action: HotkeyAction) -> Binding {
    Binding {
        chord: chord(c),
        action,
        press: PressKind::Tap,
        enabled: true,
    }
}

fn clip(name: &str) -> HotkeyAction {
    HotkeyAction::ClipLast {
        name: name.to_string(),
        seconds: 10,
    }
}

// ---------------------------------------------------------------------------
// #43
// ---------------------------------------------------------------------------

#[test]
fn issue_43_clip_limit_is_eight_and_reported_in_the_error() {
    assert_eq!(HotkeyRegistry::MAX_CLIP_HOTKEYS, 8);

    let chords: Vec<String> = (1..=9).map(|i| format!("Ctrl+Alt+F{i}")).collect();
    let eight: Vec<Binding> = (0..8)
        .map(|i| on(&chords[i], clip(&format!("c{i}"))))
        .collect();
    assert!(HotkeyRegistry::build(&eight, 60).is_ok());

    let nine: Vec<Binding> = (0..9)
        .map(|i| on(&chords[i], clip(&format!("c{i}"))))
        .collect();
    let errs = HotkeyRegistry::build(&nine, 60).expect_err("nine clip hotkeys must fail");
    let limit_err = errs
        .iter()
        .find(|c| matches!(c.kind, ConflictKind::TooManyClipHotkeys { .. }))
        .expect("TooManyClipHotkeys expected");
    assert_eq!(
        limit_err.kind,
        ConflictKind::TooManyClipHotkeys {
            max: HotkeyRegistry::MAX_CLIP_HOTKEYS
        },
        "the reported max must be the public constant"
    );
    assert!(
        limit_err.message.contains('8'),
        "message should name the limit: {}",
        limit_err.message
    );
}

// ---------------------------------------------------------------------------
// #46
// ---------------------------------------------------------------------------

#[test]
fn issue_46_display_all_modifier_combinations_in_canonical_order() {
    // Canonical order is Ctrl, Shift, Alt, Win, then the key.
    let names = ["Ctrl", "Shift", "Alt", "Win"];
    for mask in 0u32..16 {
        let mods: Vec<&str> = names
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << *i) != 0)
            .map(|(_, n)| *n)
            .collect();
        let mut expected = mods.join("+");
        if !expected.is_empty() {
            expected.push('+');
        }
        expected.push_str("F9");

        // Feed the parts in a scrambled order too, to prove output is canonical.
        let mut input = mods.clone();
        input.reverse();
        input.push("F9");
        let text = input.join("+");

        let c = chord(&text);
        assert_eq!(c.to_string(), expected, "input {text:?}, mask {mask:#06b}");
    }
}

#[test]
fn issue_46_reserved_conflict_message_uses_chord_display() {
    // Bare keys are now allowed, so the message check uses a reserved chord.
    // Input order is scrambled; the message must show the canonical form.
    let errs = HotkeyRegistry::build(&[on("win+alt+r", HotkeyAction::SessionSplit)], 60)
        .expect_err("Win+Alt+R is reserved");
    let reserved = errs
        .iter()
        .find(|c| matches!(c.kind, ConflictKind::ReservedByWindows { .. }))
        .expect("ReservedByWindows expected");
    assert!(
        reserved
            .message
            .starts_with("Alt+Win+R is reserved by Windows"),
        "message: {}",
        reserved.message
    );
}

#[test]
fn issue_46_duplicate_message_uses_chord_display_for_both_modifier_order_inputs() {
    let set = vec![
        on("win+shift+f10", HotkeyAction::SessionSplit),
        on("Shift+Win+F10", HotkeyAction::MuteMic),
    ];
    let errs = HotkeyRegistry::build(&set, 60).expect_err("duplicate chord");
    let dup = errs
        .iter()
        .find(|c| matches!(c.kind, ConflictKind::DuplicateChord { .. }))
        .expect("DuplicateChord expected");
    assert!(
        dup.message.starts_with("Shift+Win+F10 is already used by"),
        "message: {}",
        dup.message
    );
}
