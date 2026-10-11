//! Tests for constants shared between the config, hotkey and media modules.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;
use clipcheck_core::config::vk::{VK_ESCAPE, VK_F1, VK_F12, VK_F4, VK_L, VK_SPACE};
use clipcheck_core::config::MAX_CLIP_HOTKEYS;
use clipcheck_core::media::TICKS_PER_SEC;

fn chord(s: &str) -> KeyChord {
    s.parse().expect("test chord should parse")
}

fn clip_hotkeys(n: usize) -> Config {
    let names = [
        "Ctrl+F1", "Ctrl+F2", "Ctrl+F3", "Ctrl+F4", "Ctrl+F5", "Ctrl+F6", "Ctrl+F7", "Ctrl+F8",
        "Ctrl+F9", "Ctrl+F10", "Ctrl+F11", "Ctrl+F12",
    ];
    assert!(n <= names.len(), "need more chord names for n={n}");
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
}

#[test]
fn max_clip_hotkeys_is_eight() {
    assert_eq!(MAX_CLIP_HOTKEYS, 8);
}

#[test]
fn vk_constants_match_windows_codes() {
    assert_eq!(VK_F4, 0x73);
    assert_eq!(VK_L, 0x4C);
    assert_eq!(VK_ESCAPE, 0x1B);
    assert_eq!(VK_SPACE, 0x20);
    assert_eq!(VK_F1, 0x70);
    assert_eq!(VK_F12, 0x7B);
}

#[test]
fn validation_rejects_one_over_max_clip_hotkeys() {
    let d = validate(&clip_hotkeys(MAX_CLIP_HOTKEYS + 1));
    let x = d
        .iter()
        .find(|x| x.code == "CFG-H005")
        .unwrap_or_else(|| panic!("expected CFG-H005 in {d:#?}"));
    assert_eq!(x.severity, Severity::Error);
    assert_eq!(x.path, "hotkeys.clips");
}

#[test]
fn validation_accepts_exactly_max_clip_hotkeys() {
    let d = validate(&clip_hotkeys(MAX_CLIP_HOTKEYS));
    assert!(
        !d.iter().any(|x| x.code == "CFG-H005"),
        "unexpected CFG-H005 in {d:#?}"
    );
}

#[test]
fn ticks_per_sec_is_100ns_units() {
    assert_eq!(TICKS_PER_SEC, 10_000_000);
}
