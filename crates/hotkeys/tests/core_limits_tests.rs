//! Hotkeys crate agreement with clipcheck-core: the clip hotkey limit and the VK table.

use clipcheck_core::config::vk::{VK_DELETE, VK_F1, VK_F12, VK_F4, VK_L, VK_SPACE, VK_TAB};
use clipcheck_core::config::MAX_CLIP_HOTKEYS as CORE_MAX;
use clipcheck_hotkeys::{Chord, HotkeyRegistry, Key};

#[test]
fn max_clip_hotkeys_agrees_across_core_and_hotkeys() {
    assert_eq!(HotkeyRegistry::MAX_CLIP_HOTKEYS, CORE_MAX);
    assert_eq!(CORE_MAX, 8);
}

fn chord_vk(text: &str) -> u32 {
    let chord: Chord = text
        .parse()
        .unwrap_or_else(|e| panic!("{text:?} should parse: {e:?}"));
    assert_eq!(
        chord.vk_code(),
        chord.key.vk_code(),
        "chord and key agree for {text:?}"
    );
    chord.vk_code()
}

#[test]
fn chord_vk_codes_match_core_vk_consts() {
    assert_eq!(chord_vk("Ctrl+F1"), u32::from(VK_F1));
    assert_eq!(chord_vk("Ctrl+F4"), u32::from(VK_F4));
    assert_eq!(chord_vk("Alt+F4"), u32::from(VK_F4));
    assert_eq!(chord_vk("Ctrl+F12"), u32::from(VK_F12));
    assert_eq!(chord_vk("Ctrl+L"), u32::from(VK_L));
    assert_eq!(chord_vk("Ctrl+Delete"), u32::from(VK_DELETE));
    assert_eq!(chord_vk("Ctrl+Tab"), u32::from(VK_TAB));
    assert_eq!(chord_vk("Ctrl+Space"), u32::from(VK_SPACE));
}

#[test]
fn bare_key_parse_vk_matches_core_consts() {
    assert_eq!(Key::parse("F1").map(Key::vk_code), Some(u32::from(VK_F1)));
    assert_eq!(Key::parse("f12").map(Key::vk_code), Some(u32::from(VK_F12)));
    assert_eq!(Key::parse("L").map(Key::vk_code), Some(u32::from(VK_L)));
    assert_eq!(
        Key::parse("Del").map(Key::vk_code),
        Some(u32::from(VK_DELETE))
    );
    assert_eq!(Key::parse("Tab").map(Key::vk_code), Some(u32::from(VK_TAB)));
    assert_eq!(
        Key::parse("Space").map(Key::vk_code),
        Some(u32::from(VK_SPACE))
    );
}

#[test]
fn chord_key_vk_is_consistent_with_key_table_for_listed_keys() {
    // Every key in the table resolves to a distinct, non-zero VK.
    let mut seen = std::collections::HashSet::new();
    for key in Key::ALL {
        let vk = key.vk_code();
        assert!(vk != 0, "{key} has vk 0");
        assert!(seen.insert(vk), "{key} duplicates vk {vk:#x}");
    }
}
