//! Win32 virtual-key codes shared by the config rules and the hotkeys crate.
//!
//! One definition per key: `config::validate`, `config::schema` and
//! `clipcheck-hotkeys` all use these constants. Values are the Win32 `VK_*` codes.

/// Backspace (`VK_BACK`).
pub const VK_BACK: u16 = 0x08;
/// Tab (`VK_TAB`).
pub const VK_TAB: u16 = 0x09;
/// Enter (`VK_RETURN`).
pub const VK_RETURN: u16 = 0x0D;
/// Escape (`VK_ESCAPE`). Reserved, never a valid hotkey.
pub const VK_ESCAPE: u16 = 0x1B;
/// Space (`VK_SPACE`).
pub const VK_SPACE: u16 = 0x20;
/// PrintScreen (`VK_SNAPSHOT`).
pub const VK_PRINT_SCREEN: u16 = 0x2C;
/// Delete (`VK_DELETE`).
pub const VK_DELETE: u16 = 0x2E;
/// The letter L (`VK_L`).
pub const VK_L: u16 = 0x4C;
/// Apps / context-menu key (`VK_APPS`), also written as `Menu`.
pub const VK_APPS: u16 = 0x5D;
/// F1 (`VK_F1`).
pub const VK_F1: u16 = 0x70;
/// F2 (`VK_F2`).
pub const VK_F2: u16 = 0x71;
/// F3 (`VK_F3`).
pub const VK_F3: u16 = 0x72;
/// F4 (`VK_F4`).
pub const VK_F4: u16 = 0x73;
/// F5 (`VK_F5`).
pub const VK_F5: u16 = 0x74;
/// F6 (`VK_F6`).
pub const VK_F6: u16 = 0x75;
/// F7 (`VK_F7`).
pub const VK_F7: u16 = 0x76;
/// F8 (`VK_F8`).
pub const VK_F8: u16 = 0x77;
/// F9 (`VK_F9`).
pub const VK_F9: u16 = 0x78;
/// F10 (`VK_F10`).
pub const VK_F10: u16 = 0x79;
/// F11 (`VK_F11`).
pub const VK_F11: u16 = 0x7A;
/// F12 (`VK_F12`).
pub const VK_F12: u16 = 0x7B;
