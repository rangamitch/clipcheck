//! Hotkey bindings and their validation (duplicates, reserved chords, limits).

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use crate::chord::{Chord, Key};

/// Maximum number of enabled clip hotkeys. The single definition lives in
/// `clipcheck_core::config`; `HotkeyRegistry::MAX_CLIP_HOTKEYS` re-exports it.
pub use clipcheck_core::config::MAX_CLIP_HOTKEYS;

/// How a clip hotkey is pressed: released quickly, or held past the long-press
/// threshold. Two bindings may share a chord only when one is `Tap` and the
/// other `Long`, and both are clip hotkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressKind {
    /// Released before the long-press threshold.
    Tap,
    /// Held for at least the long-press threshold.
    Long,
}

impl PressKind {
    /// Lowercase word used in conflict messages.
    fn word(self) -> &'static str {
        match self {
            PressKind::Tap => "tap",
            PressKind::Long => "long press",
        }
    }
}

/// What a hotkey does when pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyAction {
    /// Save the last `seconds` of the replay buffer as a clip named `name`.
    ClipLast {
        /// Clip label; must be non-empty and unique among clip hotkeys.
        name: String,
        /// Clip length in seconds.
        seconds: u32,
    },
    /// Split the current recording session.
    SessionSplit,
    /// Start the session if stopped, stop it if running.
    SessionStopStart,
    /// Toggle the replay buffer on or off.
    ToggleBuffer,
    /// Toggle microphone mute.
    MuteMic,
    /// Push to talk: the mic is open while the chord is held. Delivers a press event on key
    /// down and a release event when the key (or a required modifier) goes up.
    PushToTalk,
}

impl HotkeyAction {
    /// Short human description used in conflict messages.
    fn describe(&self) -> Cow<'static, str> {
        match self {
            HotkeyAction::ClipLast { name, seconds } => {
                Cow::Owned(format!("clip '{}' ({}s)", name, seconds))
            }
            HotkeyAction::SessionSplit => Cow::Borrowed("session split"),
            HotkeyAction::SessionStopStart => Cow::Borrowed("session start/stop"),
            HotkeyAction::ToggleBuffer => Cow::Borrowed("toggle buffer"),
            HotkeyAction::MuteMic => Cow::Borrowed("mute microphone"),
            HotkeyAction::PushToTalk => Cow::Borrowed("push to talk"),
        }
    }
}

/// One configured hotkey: a chord bound to an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The key combination.
    pub chord: Chord,
    /// The action fired by the chord.
    pub action: HotkeyAction,
    /// Tap or long press. Only clip bindings are Long or Tap; every other binding is Tap.
    pub press: PressKind,
    /// Disabled bindings are validated but do not take part in duplicate
    /// detection and are not registered.
    pub enabled: bool,
}

/// The kind of problem found in a binding set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    /// The chord is already used by the enabled binding at index `other`.
    DuplicateChord {
        /// Index of the first binding that uses this chord.
        other: usize,
    },
    /// The chord is reserved by Windows (see [`reserved`]).
    ReservedByWindows {
        /// Human name of the reserved chord, e.g. `Win+L (lock workstation)`.
        name: &'static str,
    },
    /// Clip length is outside `1..=buffer_max_seconds`.
    DurationOutOfRange {
        /// The requested length.
        seconds: u32,
        /// The buffer maximum that was passed to `build`.
        max_seconds: u32,
    },
    /// The clip name is empty or only whitespace.
    EmptyClipName,
    /// Two enabled clip hotkeys share a name.
    DuplicateClipName {
        /// Index of the first binding with this name.
        other: usize,
    },
    /// More than [`MAX_CLIP_HOTKEYS`] clip hotkeys are enabled.
    TooManyClipHotkeys {
        /// The limit (8).
        max: usize,
    },
}

/// A problem with one binding, with a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// Index into the slice passed to [`HotkeyRegistry::build`].
    pub binding: usize,
    /// What went wrong.
    pub kind: ConflictKind,
    /// Human-readable description suitable for the UI.
    pub message: String,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Conflict {}

/// A validated set of bindings. Produced by [`HotkeyRegistry::build`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    bindings: Vec<Binding>,
}

impl Registry {
    /// All bindings, in the order they were supplied (enabled and disabled).
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The enabled binding for `chord`, if any.
    pub fn lookup(&self, chord: &Chord) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.enabled && b.chord == *chord)
    }

    /// The first enabled binding for `chord` with the given press kind, if any.
    pub fn lookup_press(&self, chord: &Chord, press: PressKind) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.enabled && b.chord == *chord && b.press == press)
    }

    /// Iterator over enabled bindings.
    pub fn enabled(&self) -> impl Iterator<Item = &Binding> {
        self.bindings.iter().filter(|b| b.enabled)
    }
}

/// Builder entry point: validates a set of bindings.
#[derive(Debug, Clone, Copy)]
pub struct HotkeyRegistry;

impl HotkeyRegistry {
    /// Maximum number of enabled clip hotkeys (8).
    pub const MAX_CLIP_HOTKEYS: usize = MAX_CLIP_HOTKEYS;

    /// Validates `bindings` against `buffer_max_seconds`.
    ///
    /// Checks, in order per binding: Windows-reserved chord, and clip
    /// length `1..=buffer_max_seconds` and clip name. Across enabled bindings it
    /// checks duplicate chords, duplicate clip names, and the clip-hotkey limit.
    /// Bare keys (no modifier) are allowed. Returns every problem found, or the validated
    /// [`Registry`].
    pub fn build(bindings: &[Binding], buffer_max_seconds: u32) -> Result<Registry, Vec<Conflict>> {
        let mut conflicts = Vec::new();
        let mut enabled_by_chord: HashMap<Chord, Vec<usize>> = HashMap::new();
        let mut first_name: HashMap<&str, usize> = HashMap::new();
        let mut clip_count = 0usize;

        for (i, b) in bindings.iter().enumerate() {
            if let Some(name) = reserved(&b.chord) {
                conflicts.push(conflict(
                    i,
                    ConflictKind::ReservedByWindows { name },
                    format!("{} is reserved by Windows: {}", b.chord, name),
                ));
            }

            if let HotkeyAction::ClipLast { name, seconds } = &b.action {
                if *seconds < 1 || *seconds > buffer_max_seconds {
                    conflicts.push(conflict(
                        i,
                        ConflictKind::DurationOutOfRange {
                            seconds: *seconds,
                            max_seconds: buffer_max_seconds,
                        },
                        format!(
                            "clip length {}s is out of range; use 1 to {} seconds",
                            seconds, buffer_max_seconds
                        ),
                    ));
                }

                if name.trim().is_empty() {
                    conflicts.push(conflict(
                        i,
                        ConflictKind::EmptyClipName,
                        format!("clip hotkey {} needs a name", b.chord),
                    ));
                }
            }

            if !b.enabled {
                continue;
            }

            let earlier = enabled_by_chord.entry(b.chord).or_default();
            let both_clips = |other: &Binding| {
                matches!(b.action, HotkeyAction::ClipLast { .. })
                    && matches!(other.action, HotkeyAction::ClipLast { .. })
            };
            // Two enabled bindings clash unless both are clip hotkeys with
            // different press kinds (one tap, one long).
            let clash = earlier.iter().copied().find(|&j| {
                let other = &bindings[j];
                !both_clips(other) || other.press == b.press
            });
            if let Some(other) = clash {
                let message = if both_clips(&bindings[other]) {
                    format!(
                        "{} ({}) is already used by {}",
                        b.chord,
                        b.press.word(),
                        bindings[other].action.describe()
                    )
                } else {
                    format!(
                        "{} is already used by {}",
                        b.chord,
                        bindings[other].action.describe()
                    )
                };
                conflicts.push(conflict(i, ConflictKind::DuplicateChord { other }, message));
            }
            earlier.push(i);

            if let HotkeyAction::ClipLast { name, .. } = &b.action {
                clip_count += 1;
                if clip_count > MAX_CLIP_HOTKEYS {
                    conflicts.push(conflict(
                        i,
                        ConflictKind::TooManyClipHotkeys {
                            max: MAX_CLIP_HOTKEYS,
                        },
                        format!("at most {} clip hotkeys can be enabled", MAX_CLIP_HOTKEYS),
                    ));
                }

                let key_name = name.trim();
                if !key_name.is_empty() {
                    match first_name.get(key_name) {
                        Some(&other) => conflicts.push(conflict(
                            i,
                            ConflictKind::DuplicateClipName { other },
                            format!("clip name '{}' is already used", key_name),
                        )),
                        None => {
                            first_name.insert(key_name, i);
                        }
                    }
                }
            }
        }

        if conflicts.is_empty() {
            Ok(Registry {
                bindings: bindings.to_vec(),
            })
        } else {
            Err(conflicts)
        }
    }
}

fn conflict(binding: usize, kind: ConflictKind, message: String) -> Conflict {
    Conflict {
        binding,
        kind,
        message,
    }
}

/// Chords that Windows reserves or that are unsafe to bind.
///
/// Each row is `(ctrl, shift, alt, win, key, name)`; a chord matches only when
/// its modifiers are exactly those listed. Returns the reason when blocked.
/// `Ctrl+Alt+F12` (Intel graphics hotkey) is intentionally not listed: the
/// design treats it as a warning, not a block.
pub fn reserved(c: &Chord) -> Option<&'static str> {
    const TABLE: &[(bool, bool, bool, bool, Key, &str)] = &[
        (
            true,
            false,
            true,
            false,
            Key::Delete,
            "Ctrl+Alt+Delete (secure attention sequence)",
        ),
        (
            false,
            false,
            false,
            true,
            Key::L,
            "Win+L (lock workstation)",
        ),
        (false, false, true, false, Key::F4, "Alt+F4 (close window)"),
        (
            false,
            false,
            true,
            false,
            Key::Tab,
            "Alt+Tab (task switcher)",
        ),
        (
            true,
            true,
            false,
            false,
            Key::Escape,
            "Ctrl+Shift+Esc (Task Manager)",
        ),
        (false, false, false, true, Key::G, "Win+G (Xbox Game Bar)"),
        (false, false, true, true, Key::R, "Win+Alt+R (Game DVR)"),
        (false, false, true, true, Key::G, "Win+Alt+G (Game DVR)"),
        (false, true, false, true, Key::S, "Win+Shift+S (screenshot)"),
        (
            false,
            false,
            true,
            false,
            Key::Space,
            "Alt+Space (window menu)",
        ),
        (
            false,
            false,
            false,
            true,
            Key::PrintScreen,
            "Win+PrintScreen (screenshot)",
        ),
    ];

    TABLE
        .iter()
        .find(|(ctrl, shift, alt, win, key, _)| {
            c.ctrl == *ctrl && c.shift == *shift && c.alt == *alt && c.win == *win && c.key == *key
        })
        .map(|(_, _, _, _, _, name)| *name)
}
