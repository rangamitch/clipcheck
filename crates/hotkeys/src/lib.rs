//! clipcheck-hotkeys: pure hotkey chord parsing and binding validation.
//!
//! This crate makes no Win32 calls. `vk_code()` and `modifier_flags()` return
//! the raw Windows values for the platform layer. The Windows backend (`win`)
//! observes keys with a low-level keyboard hook and never swallows them.

mod chord;
pub mod gesture;
#[cfg(windows)]
mod hook_state;
mod registry;

pub use chord::{Chord, ChordError, Key, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
pub use gesture::{ChordId, GestureTracker};
pub use registry::{
    reserved, Binding, Conflict, ConflictKind, HotkeyAction, HotkeyRegistry, PressKind, Registry,
};

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{ApplyError, FailureReason, HotkeyEvent, HotkeyService, RegisterFailure, StartError};
