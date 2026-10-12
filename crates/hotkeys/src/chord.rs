//! Key chords: parsing ("Ctrl+Shift+F9"), canonical display, and Win32 mapping.

use std::fmt;
use std::str::FromStr;

use clipcheck_core::config::vk::{
    VK_APPS, VK_DELETE, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F2, VK_F3, VK_F4, VK_F5,
    VK_F6, VK_F7, VK_F8, VK_F9, VK_L, VK_PRINT_SCREEN, VK_SPACE, VK_TAB,
};
use thiserror::Error;

/// `RegisterHotKey` modifier bit: Alt.
pub const MOD_ALT: u32 = 0x0001;
/// `RegisterHotKey` modifier bit: Ctrl.
pub const MOD_CONTROL: u32 = 0x0002;
/// `RegisterHotKey` modifier bit: Shift.
pub const MOD_SHIFT: u32 = 0x0004;
/// `RegisterHotKey` modifier bit: Windows key.
pub const MOD_WIN: u32 = 0x0008;

macro_rules! define_keys {
    ($( $variant:ident = $name:literal => $vk:expr, )*) => {
        /// A non-modifier key that can appear in a chord.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Key {
            $(
                #[doc = concat!("The `", $name, "` key.")]
                $variant,
            )*
        }

        impl Key {
            /// Every supported key, in declaration order.
            pub const ALL: &'static [Key] = &[ $( Key::$variant, )* ];

            /// Canonical display name (e.g. `F9`, `A`, `Left`, `Numpad5`).
            pub const fn name(self) -> &'static str {
                match self {
                    $( Key::$variant => $name, )*
                }
            }

            /// Win32 virtual-key code (`VK_*`).
            pub const fn vk_code(self) -> u32 {
                match self {
                    $( Key::$variant => $vk, )*
                }
            }
        }
    };
}

define_keys! {
    A = "A" => 0x41,
    B = "B" => 0x42,
    C = "C" => 0x43,
    D = "D" => 0x44,
    E = "E" => 0x45,
    F = "F" => 0x46,
    G = "G" => 0x47,
    H = "H" => 0x48,
    I = "I" => 0x49,
    J = "J" => 0x4A,
    K = "K" => 0x4B,
    L = "L" => VK_L as u32,
    M = "M" => 0x4D,
    N = "N" => 0x4E,
    O = "O" => 0x4F,
    P = "P" => 0x50,
    Q = "Q" => 0x51,
    R = "R" => 0x52,
    S = "S" => 0x53,
    T = "T" => 0x54,
    U = "U" => 0x55,
    V = "V" => 0x56,
    W = "W" => 0x57,
    X = "X" => 0x58,
    Y = "Y" => 0x59,
    Z = "Z" => 0x5A,
    Digit0 = "0" => 0x30,
    Digit1 = "1" => 0x31,
    Digit2 = "2" => 0x32,
    Digit3 = "3" => 0x33,
    Digit4 = "4" => 0x34,
    Digit5 = "5" => 0x35,
    Digit6 = "6" => 0x36,
    Digit7 = "7" => 0x37,
    Digit8 = "8" => 0x38,
    Digit9 = "9" => 0x39,
    F1 = "F1" => VK_F1 as u32,
    F2 = "F2" => VK_F2 as u32,
    F3 = "F3" => VK_F3 as u32,
    F4 = "F4" => VK_F4 as u32,
    F5 = "F5" => VK_F5 as u32,
    F6 = "F6" => VK_F6 as u32,
    F7 = "F7" => VK_F7 as u32,
    F8 = "F8" => VK_F8 as u32,
    F9 = "F9" => VK_F9 as u32,
    F10 = "F10" => VK_F10 as u32,
    F11 = "F11" => VK_F11 as u32,
    F12 = "F12" => VK_F12 as u32,
    F13 = "F13" => 0x7C,
    F14 = "F14" => 0x7D,
    F15 = "F15" => 0x7E,
    F16 = "F16" => 0x7F,
    F17 = "F17" => 0x80,
    F18 = "F18" => 0x81,
    F19 = "F19" => 0x82,
    F20 = "F20" => 0x83,
    F21 = "F21" => 0x84,
    F22 = "F22" => 0x85,
    F23 = "F23" => 0x86,
    F24 = "F24" => 0x87,
    Left = "Left" => 0x25,
    Up = "Up" => 0x26,
    Right = "Right" => 0x27,
    Down = "Down" => 0x28,
    Insert = "Insert" => 0x2D,
    Delete = "Delete" => VK_DELETE as u32,
    Home = "Home" => 0x24,
    End = "End" => 0x23,
    PageUp = "PageUp" => 0x21,
    PageDown = "PageDown" => 0x22,
    Numpad0 = "Numpad0" => 0x60,
    Numpad1 = "Numpad1" => 0x61,
    Numpad2 = "Numpad2" => 0x62,
    Numpad3 = "Numpad3" => 0x63,
    Numpad4 = "Numpad4" => 0x64,
    Numpad5 = "Numpad5" => 0x65,
    Numpad6 = "Numpad6" => 0x66,
    Numpad7 = "Numpad7" => 0x67,
    Numpad8 = "Numpad8" => 0x68,
    Numpad9 = "Numpad9" => 0x69,
    NumpadMultiply = "NumpadMultiply" => 0x6A,
    NumpadAdd = "NumpadAdd" => 0x6B,
    NumpadSeparator = "NumpadSeparator" => 0x6C,
    NumpadSubtract = "NumpadSubtract" => 0x6D,
    NumpadDecimal = "NumpadDecimal" => 0x6E,
    NumpadDivide = "NumpadDivide" => 0x6F,
    Semicolon = "Semicolon" => 0xBA,
    Equals = "Equals" => 0xBB,
    Comma = "Comma" => 0xBC,
    Minus = "Minus" => 0xBD,
    Period = "Period" => 0xBE,
    Slash = "Slash" => 0xBF,
    Backquote = "Backquote" => 0xC0,
    LeftBracket = "LeftBracket" => 0xDB,
    Backslash = "Backslash" => 0xDC,
    RightBracket = "RightBracket" => 0xDD,
    Quote = "Quote" => 0xDE,
    Space = "Space" => VK_SPACE as u32,
    Enter = "Enter" => 0x0D,
    Tab = "Tab" => VK_TAB as u32,
    Escape = "Escape" => VK_ESCAPE as u32,
    Backspace = "Backspace" => 0x08,
    PrintScreen = "PrintScreen" => VK_PRINT_SCREEN as u32,
    Pause = "Pause" => 0x13,
    ScrollLock = "ScrollLock" => 0x91,
    NumLock = "NumLock" => 0x90,
    CapsLock = "CapsLock" => 0x14,
    AppsKey = "AppsKey" => VK_APPS as u32,
    VolumeMute = "VolumeMute" => 0xAD,
    VolumeDown = "VolumeDown" => 0xAE,
    VolumeUp = "VolumeUp" => 0xAF,
    MediaNext = "MediaNext" => 0xB0,
    MediaPrev = "MediaPrev" => 0xB1,
    MediaStop = "MediaStop" => 0xB2,
    MediaPlayPause = "MediaPlayPause" => 0xB3,
    LaunchMail = "LaunchMail" => 0xB4,
    LaunchMediaSelect = "LaunchMediaSelect" => 0xB5,
    LaunchApp1 = "LaunchApp1" => 0xB6,
    LaunchApp2 = "LaunchApp2" => 0xB7,
    BrowserBack = "BrowserBack" => 0xA6,
    BrowserForward = "BrowserForward" => 0xA7,
    BrowserRefresh = "BrowserRefresh" => 0xA8,
    BrowserStop = "BrowserStop" => 0xA9,
    BrowserSearch = "BrowserSearch" => 0xAA,
    BrowserFavorites = "BrowserFavorites" => 0xAB,
    BrowserHome = "BrowserHome" => 0xAC,
    Mouse3 = "Mouse3" => 0x04,
    Mouse4 = "Mouse4" => 0x05,
    Mouse5 = "Mouse5" => 0x06,
}

impl Key {
    /// Parses a single key token (case-insensitive). Accepts canonical names
    /// plus common aliases such as `Esc`, `Del`, `PgUp`, `PrtSc` and symbols
    /// like `;` or `[`.
    pub fn parse(token: &str) -> Option<Key> {
        let upper = token.to_ascii_uppercase();
        let alias = match upper.as_str() {
            "ESC" => Some(Key::Escape),
            "DEL" => Some(Key::Delete),
            "INS" => Some(Key::Insert),
            "PGUP" => Some(Key::PageUp),
            "PGDN" | "PGDOWN" => Some(Key::PageDown),
            "PRTSC" | "PRINT" => Some(Key::PrintScreen),
            "BKSP" | "BACK" => Some(Key::Backspace),
            "RETURN" => Some(Key::Enter),
            "BREAK" => Some(Key::Pause),
            "SCROLL" => Some(Key::ScrollLock),
            "MENU" => Some(Key::AppsKey),
            ";" => Some(Key::Semicolon),
            "=" => Some(Key::Equals),
            "," => Some(Key::Comma),
            "-" => Some(Key::Minus),
            "." => Some(Key::Period),
            "/" => Some(Key::Slash),
            "`" => Some(Key::Backquote),
            "[" => Some(Key::LeftBracket),
            "\\" => Some(Key::Backslash),
            "]" => Some(Key::RightBracket),
            "'" => Some(Key::Quote),
            _ => None,
        };
        alias.or_else(|| {
            Key::ALL
                .iter()
                .copied()
                .find(|k| k.name().eq_ignore_ascii_case(token))
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Key {
    type Err = ChordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.is_empty() {
            return Err(ChordError::Empty);
        }
        Key::parse(t).ok_or_else(|| ChordError::UnknownKey(t.to_string()))
    }
}

/// Errors produced while parsing a chord string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChordError {
    /// The string was empty or whitespace only.
    #[error("hotkey is empty")]
    Empty,
    /// A `+`-separated segment was empty (e.g. `Ctrl++F9` or a trailing `+`).
    #[error("empty segment in hotkey '{0}'")]
    EmptySegment(String),
    /// The chord has modifiers but no key, or no key at all.
    #[error("hotkey '{0}' has no key (modifiers alone are not a hotkey)")]
    NoKey(String),
    /// A segment is not a known modifier or key name.
    #[error("unknown key '{0}'")]
    UnknownKey(String),
    /// The same modifier appears twice.
    #[error("modifier '{0}' is given more than once")]
    DuplicateModifier(String),
    /// More than one non-modifier key is present.
    #[error("hotkey '{0}' has more than one key")]
    MultipleKeys(String),
    /// The chord can never be a hotkey: Escape, or Alt+Space (the Windows window menu).
    #[error("hotkey '{chord}' is reserved and cannot be used: {reason}")]
    Reserved {
        /// The chord text as written.
        chord: String,
        /// Why the chord is refused.
        reason: &'static str,
    },
}

/// A keyboard chord: a set of modifiers plus exactly one non-modifier key.
///
/// Parse with `"Ctrl+Shift+F9".parse::<Chord>()`. Display produces the
/// canonical order `Ctrl+Shift+Alt+Win+Key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Control modifier.
    pub ctrl: bool,
    /// Shift modifier.
    pub shift: bool,
    /// Alt modifier.
    pub alt: bool,
    /// Windows-key modifier.
    pub win: bool,
    /// The non-modifier key.
    pub key: Key,
}

impl Chord {
    /// Creates a chord with no modifiers for the given key.
    pub fn bare(key: Key) -> Self {
        Chord {
            ctrl: false,
            shift: false,
            alt: false,
            win: false,
            key,
        }
    }

    /// True when at least one modifier is set.
    pub fn has_modifiers(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.win
    }

    /// `RegisterHotKey` modifier bits (`MOD_*`), without `MOD_NOREPEAT`.
    pub fn modifier_flags(&self) -> u32 {
        let mut flags = 0;
        if self.alt {
            flags |= MOD_ALT;
        }
        if self.ctrl {
            flags |= MOD_CONTROL;
        }
        if self.shift {
            flags |= MOD_SHIFT;
        }
        if self.win {
            flags |= MOD_WIN;
        }
        flags
    }

    /// Win32 virtual-key code of the non-modifier key.
    pub fn vk_code(&self) -> u32 {
        self.key.vk_code()
    }
}

impl FromStr for Chord {
    type Err = ChordError;

    /// Parses chords such as `Ctrl+Shift+F9`. Matching is case-insensitive and
    /// whitespace around segments is ignored. Modifier names: `Ctrl`/`Control`,
    /// `Shift`, `Alt`, `Win`/`Windows`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err(ChordError::Empty);
        }

        let mut chord = Chord {
            ctrl: false,
            shift: false,
            alt: false,
            win: false,
            key: Key::A,
        };
        let mut key: Option<Key> = None;

        for raw in s.split('+') {
            let seg = raw.trim();
            if seg.is_empty() {
                return Err(ChordError::EmptySegment(s.trim().to_string()));
            }
            let flag = match seg.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Some(&mut chord.ctrl),
                "shift" => Some(&mut chord.shift),
                "alt" => Some(&mut chord.alt),
                "win" | "windows" => Some(&mut chord.win),
                _ => None,
            };
            match flag {
                Some(slot) => {
                    if *slot {
                        return Err(ChordError::DuplicateModifier(seg.to_string()));
                    }
                    *slot = true;
                }
                None => {
                    let k =
                        Key::parse(seg).ok_or_else(|| ChordError::UnknownKey(seg.to_string()))?;
                    if key.is_some() {
                        return Err(ChordError::MultipleKeys(s.trim().to_string()));
                    }
                    key = Some(k);
                }
            }
        }

        match key {
            Some(k) => {
                if k == Key::Escape {
                    return Err(ChordError::Reserved {
                        chord: s.trim().to_string(),
                        reason: "Escape is reserved",
                    });
                }
                let alt_only = chord.alt && !chord.ctrl && !chord.shift && !chord.win;
                if k == Key::Space && alt_only {
                    return Err(ChordError::Reserved {
                        chord: s.trim().to_string(),
                        reason: "Alt+Space opens the Windows window menu",
                    });
                }
                chord.key = k;
                Ok(chord)
            }
            None => Err(ChordError::NoKey(s.trim().to_string())),
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Written part by part so no intermediate Vec or String is allocated.
        let mut first = true;
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
            (self.win, "Win"),
        ] {
            if on {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if !first {
            f.write_str("+")?;
        }
        f.write_str(self.key.name())
    }
}

#[cfg(test)]
mod vk_tests {
    use super::*;

    #[test]
    fn key_table_uses_the_shared_core_vk_codes() {
        assert_eq!("F4".parse::<Chord>().unwrap().vk_code(), VK_F4 as u32);
        assert_eq!(Key::parse("Escape").unwrap().vk_code(), VK_ESCAPE as u32);
    }

    #[test]
    fn mouse_buttons_map_to_their_virtual_key_codes() {
        assert_eq!(Key::Mouse3.vk_code(), 0x04);
        assert_eq!(Key::Mouse4.vk_code(), 0x05);
        assert_eq!(Key::Mouse5.vk_code(), 0x06);
    }

    #[test]
    fn mouse_button_names_parse_case_insensitively() {
        assert_eq!(Key::parse("Mouse4"), Some(Key::Mouse4));
        assert_eq!(Key::parse("mouse5"), Some(Key::Mouse5));
        assert_eq!("MOUSE3".parse::<Key>().unwrap(), Key::Mouse3);
    }

    #[test]
    fn left_and_right_mouse_buttons_are_not_keys() {
        assert_eq!(Key::parse("Mouse1"), None);
        assert_eq!(Key::parse("Mouse2"), None);
        assert!("Ctrl+Mouse1".parse::<Chord>().is_err());
    }

    #[test]
    fn mouse_button_chord_parses_with_modifiers() {
        let chord: Chord = "Ctrl+Mouse5".parse().unwrap();
        assert!(chord.ctrl);
        assert_eq!(chord.key, Key::Mouse5);
        assert_eq!(chord.vk_code(), 0x06);

        let bare: Chord = "Mouse4".parse().unwrap();
        assert_eq!(bare, Chord::bare(Key::Mouse4));
        assert_eq!(bare.vk_code(), 0x05);
    }

    #[test]
    fn mouse_button_chord_display_round_trips() {
        for text in ["Mouse3", "Mouse4", "Alt+Mouse4", "Ctrl+Shift+Mouse5"] {
            let chord: Chord = text.parse().unwrap();
            assert_eq!(chord.to_string(), text);
            assert_eq!(chord.to_string().parse::<Chord>().unwrap(), chord);
        }
        let chord: Chord = "ctrl+mouse5".parse().unwrap();
        assert_eq!(chord.to_string(), "Ctrl+Mouse5");
    }
}
