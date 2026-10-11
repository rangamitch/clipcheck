//! Pure key-event state machine behind the Windows low-level keyboard hook.
//!
//! No Win32 calls and no clock: the caller passes the raw virtual-key code, the
//! direction (down or up), the `LLKHF_*` flags and a millisecond timestamp.
//!
//! Modifier state is built from the events themselves. Left and right modifier
//! keys are tracked separately, and Alt is re-synchronised from `LLKHF_ALTDOWN`
//! on every non-modifier event, so a modifier that was already held when the
//! hook started is picked up on the next key.
//!
//! The hook never swallows a key. `on_key` only reports which chords fired.

use std::collections::BTreeSet;

use crate::chord::Chord;
use crate::gesture::{ChordId, GestureTracker};
use crate::registry::PressKind;

/// Suspension requested by the UI expires this long after the request, so a
/// lost "resume" call cannot leave hotkeys disabled for good.
pub(crate) const SUSPEND_TIMEOUT_MS: u64 = 60_000;

/// `LLKHF_ALTDOWN`: set on a key event while Alt is held.
const LLKHF_ALTDOWN: u32 = 0x20;

const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

/// One bit per physical modifier key.
const L_SHIFT: u8 = 1 << 0;
const R_SHIFT: u8 = 1 << 1;
const L_CTRL: u8 = 1 << 2;
const R_CTRL: u8 = 1 << 3;
const L_ALT: u8 = 1 << 4;
const R_ALT: u8 = 1 << 5;
const L_WIN: u8 = 1 << 6;
const R_WIN: u8 = 1 << 7;

const SHIFT_BITS: u8 = L_SHIFT | R_SHIFT;
const CTRL_BITS: u8 = L_CTRL | R_CTRL;
const ALT_BITS: u8 = L_ALT | R_ALT;
const WIN_BITS: u8 = L_WIN | R_WIN;

/// Bits for a modifier virtual-key code, or `None` for any other key. The
/// generic codes (`VK_SHIFT` and friends) cover both sides.
fn modifier_bits(vk: u32) -> Option<u8> {
    Some(match vk {
        VK_LSHIFT => L_SHIFT,
        VK_RSHIFT => R_SHIFT,
        VK_SHIFT => SHIFT_BITS,
        VK_LCONTROL => L_CTRL,
        VK_RCONTROL => R_CTRL,
        VK_CONTROL => CTRL_BITS,
        VK_LMENU => L_ALT,
        VK_RMENU => R_ALT,
        VK_MENU => ALT_BITS,
        VK_LWIN => L_WIN,
        VK_RWIN => R_WIN,
        _ => return None,
    })
}

/// The modifiers held right now, as the chord syntax sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mods {
    ctrl: bool,
    shift: bool,
    alt: bool,
    win: bool,
}

/// A chord that fired, with the press kind to deliver for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fired {
    /// The chord id the state machine was built with.
    pub(crate) id: ChordId,
    /// Tap on release, or long once the hold reaches the threshold.
    pub(crate) press: PressKind,
}

/// Chord matching and tap/long gestures for the keyboard hook.
#[derive(Debug, Clone)]
pub(crate) struct HookState {
    long_press_ms: u64,
    /// `(id, chord)` for every chord with at least one binding.
    chords: Vec<(ChordId, Chord)>,
    /// `(id, has_tap, has_long)` used to rebuild `gestures` when holds are cleared.
    triples: Vec<(ChordId, bool, bool)>,
    gestures: GestureTracker,
    /// Modifier keys currently held, one bit each.
    mods: u8,
    /// Non-modifier virtual-key codes currently held.
    down: BTreeSet<u32>,
    /// Start time of an active UI suspension, if any.
    suspended_since: Option<u64>,
}

impl HookState {
    /// Builds the matcher. Each slot is `(id, chord, has_tap, has_long)`; a slot
    /// with neither flag is ignored.
    pub(crate) fn new(long_press_ms: u64, slots: &[(ChordId, Chord, bool, bool)]) -> HookState {
        let chords = slots.iter().map(|&(id, chord, _, _)| (id, chord)).collect();
        let triples: Vec<(ChordId, bool, bool)> = slots
            .iter()
            .filter(|(_, _, tap, long)| *tap || *long)
            .map(|&(id, _, tap, long)| (id, tap, long))
            .collect();
        let gestures = GestureTracker::new(long_press_ms, &triples);
        HookState {
            long_press_ms,
            chords,
            triples,
            gestures,
            mods: 0,
            down: BTreeSet::new(),
            suspended_since: None,
        }
    }

    /// Sets the suspension start (`Some(ms)`) or clears it (`None`). While a
    /// suspension is active (for less than [`SUSPEND_TIMEOUT_MS`]) no chord fires.
    pub(crate) fn set_suspension(&mut self, since: Option<u64>) {
        self.suspended_since = since;
    }

    /// True while a suspension is in force at `now_ms`.
    fn is_suspended(&self, now_ms: u64) -> bool {
        self.suspended_since
            .is_some_and(|since| now_ms.saturating_sub(since) < SUSPEND_TIMEOUT_MS)
    }

    /// True while a long-press hold is pending and the caller should tick.
    pub(crate) fn is_pending(&self) -> bool {
        self.gestures.is_pending()
    }

    /// A key event from the hook. `down` is true for key-down (including
    /// auto-repeat), false for key-up. `flags` is the raw `KBDLLHOOKSTRUCT.flags`.
    /// Returns the chords that fired on this event.
    pub(crate) fn on_key(&mut self, vk: u32, down: bool, flags: u32, now_ms: u64) -> Vec<Fired> {
        if let Some(bits) = modifier_bits(vk) {
            if down {
                self.mods |= bits;
            } else {
                self.mods &= !bits;
            }
            return Vec::new();
        }

        if down {
            // Auto-repeat: the key is already down, so it is not a new press.
            if !self.down.insert(vk) {
                return Vec::new();
            }
            self.sync_alt(flags);
            if self.is_suspended(now_ms) {
                self.clear_holds();
                return Vec::new();
            }
            let mods = self.current_mods();
            let Some(id) = self.find(vk, mods) else {
                return Vec::new();
            };
            return self
                .gestures
                .on_press(id, now_ms)
                .map(|press| Fired { id, press })
                .into_iter()
                .collect();
        }

        // Key-up for a key that was never seen down (it was held before the
        // hook started, or the press was ignored): nothing to do.
        if !self.down.remove(&vk) {
            return Vec::new();
        }
        self.sync_alt(flags);
        self.poll(now_ms)
    }

    /// Called periodically while [`is_pending`](Self::is_pending) is true.
    /// Fires the long press for a hold that reached the threshold while the key
    /// is still down.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Vec<Fired> {
        if !self.gestures.is_pending() {
            return Vec::new();
        }
        self.poll(now_ms)
    }

    /// Advances the gesture tracker using the current set of held keys.
    fn poll(&mut self, now_ms: u64) -> Vec<Fired> {
        if self.is_suspended(now_ms) {
            self.clear_holds();
            return Vec::new();
        }
        let chords = &self.chords;
        let down = &self.down;
        let is_down = |id: ChordId| {
            chords
                .iter()
                .find(|(cid, _)| *cid == id)
                .is_some_and(|(_, chord)| down.contains(&chord.vk_code()))
        };
        self.gestures
            .on_poll(now_ms, is_down)
            .into_iter()
            .map(|(id, press)| Fired { id, press })
            .collect()
    }

    /// Drops every pending hold, so a press made before a suspension cannot
    /// fire after it.
    fn clear_holds(&mut self) {
        self.gestures = GestureTracker::new(self.long_press_ms, &self.triples);
    }

    /// Corrects Alt from `LLKHF_ALTDOWN` on a non-modifier event.
    fn sync_alt(&mut self, flags: u32) {
        if flags & LLKHF_ALTDOWN != 0 {
            if self.mods & ALT_BITS == 0 {
                self.mods |= L_ALT;
            }
        } else {
            self.mods &= !ALT_BITS;
        }
    }

    fn current_mods(&self) -> Mods {
        Mods {
            ctrl: self.mods & CTRL_BITS != 0,
            shift: self.mods & SHIFT_BITS != 0,
            alt: self.mods & ALT_BITS != 0,
            win: self.mods & WIN_BITS != 0,
        }
    }

    /// The chord whose key is `vk` and whose modifiers equal `mods` exactly.
    fn find(&self, vk: u32, mods: Mods) -> Option<ChordId> {
        self.chords
            .iter()
            .find(|(_, c)| {
                c.vk_code() == vk
                    && c.ctrl == mods.ctrl
                    && c.shift == mods.shift
                    && c.alt == mods.alt
                    && c.win == mods.win
            })
            .map(|(id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::Key;

    const MS: u64 = 400;
    const NO_FLAGS: u32 = 0;
    const ALT_FLAG: u32 = LLKHF_ALTDOWN;
    const F9: u32 = 0x78;
    const A: u32 = 0x41;

    fn chord(s: &str) -> Chord {
        s.parse::<Chord>().unwrap()
    }

    fn bare(key: Key) -> Chord {
        Chord::bare(key)
    }

    /// One chord with a tap binding only.
    fn tap(c: Chord) -> HookState {
        HookState::new(MS, &[(1, c, true, false)])
    }

    /// One chord with a long binding only.
    fn long(c: Chord) -> HookState {
        HookState::new(MS, &[(1, c, false, true)])
    }

    /// One chord with both tap and long bindings (a shared clip chord).
    fn both(c: Chord) -> HookState {
        HookState::new(MS, &[(1, c, true, true)])
    }

    fn ids(fired: &[Fired]) -> Vec<(ChordId, PressKind)> {
        fired.iter().map(|f| (f.id, f.press)).collect()
    }

    #[test]
    fn bare_key_fires_with_no_modifiers() {
        let mut s = tap(bare(Key::F9));
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, 0)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn bare_key_does_not_fire_while_a_modifier_is_held() {
        let mut s = tap(bare(Key::F9));
        s.on_key(VK_LCONTROL, true, NO_FLAGS, 0);
        assert!(s.on_key(F9, true, NO_FLAGS, 10).is_empty());
    }

    #[test]
    fn modifier_chord_is_exact_about_extra_modifiers() {
        let mut s = tap(chord("Ctrl+Shift+F9"));
        s.on_key(VK_LCONTROL, true, NO_FLAGS, 0);
        assert!(s.on_key(F9, true, NO_FLAGS, 5).is_empty());
        let mut s = tap(chord("Ctrl+Shift+F9"));
        s.on_key(VK_LCONTROL, true, NO_FLAGS, 0);
        s.on_key(VK_LSHIFT, true, NO_FLAGS, 1);
        s.on_key(VK_LMENU, true, NO_FLAGS, 2);
        assert!(s.on_key(F9, true, ALT_FLAG, 5).is_empty());
    }

    #[test]
    fn alt_f9_does_not_fire_for_bare_f9() {
        let mut s = tap(bare(Key::F9));
        s.on_key(VK_LMENU, true, NO_FLAGS, 0);
        assert!(s.on_key(F9, true, ALT_FLAG, 5).is_empty());
    }

    #[test]
    fn alt_f9_fires_for_alt_chord_when_alt_is_held() {
        let mut s = tap(chord("Alt+F9"));
        s.on_key(VK_LMENU, true, NO_FLAGS, 0);
        assert_eq!(
            ids(&s.on_key(F9, true, ALT_FLAG, 5)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn alt_is_resynced_from_altdown_flag_when_alt_was_never_seen() {
        // Alt was already held when the hook started, so no Alt event arrived.
        let mut s = tap(chord("Alt+F9"));
        assert_eq!(
            ids(&s.on_key(F9, true, ALT_FLAG, 5)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn stale_alt_is_cleared_when_altdown_flag_is_clear() {
        let mut s = tap(bare(Key::F9));
        s.on_key(VK_LMENU, true, NO_FLAGS, 0);
        // Alt was released with no event seen; the flag tells us so.
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, 5)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn right_and_generic_modifier_codes_count_as_the_modifier() {
        let mut s = tap(chord("Ctrl+F9"));
        s.on_key(VK_RCONTROL, true, NO_FLAGS, 0);
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, 5)),
            vec![(1, PressKind::Tap)]
        );
        let mut s = tap(chord("Shift+F9"));
        s.on_key(VK_SHIFT, true, NO_FLAGS, 0);
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, 5)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn win_key_combinations_match_on_either_side() {
        for vk in [VK_LWIN, VK_RWIN] {
            let mut s = tap(chord("Win+F9"));
            s.on_key(vk, true, NO_FLAGS, 0);
            assert_eq!(
                ids(&s.on_key(F9, true, NO_FLAGS, 5)),
                vec![(1, PressKind::Tap)]
            );
        }
        let mut s = tap(bare(Key::F9));
        s.on_key(VK_LWIN, true, NO_FLAGS, 0);
        assert!(s.on_key(F9, true, NO_FLAGS, 5).is_empty());
    }

    #[test]
    fn auto_repeat_is_not_a_new_press() {
        let mut s = tap(bare(Key::F9));
        assert_eq!(s.on_key(F9, true, NO_FLAGS, 0).len(), 1);
        assert!(s.on_key(F9, true, NO_FLAGS, 30).is_empty());
        assert!(s.on_key(F9, true, NO_FLAGS, 60).is_empty());
        s.on_key(F9, false, NO_FLAGS, 90);
        assert_eq!(s.on_key(F9, true, NO_FLAGS, 120).len(), 1);
    }

    #[test]
    fn tap_only_chord_fires_on_key_down_not_on_release() {
        let mut s = tap(bare(Key::F9));
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, 0)),
            vec![(1, PressKind::Tap)]
        );
        assert!(s.on_key(F9, false, NO_FLAGS, 50).is_empty());
        assert!(!s.is_pending());
    }

    #[test]
    fn long_only_chord_fires_at_threshold_while_held() {
        let mut s = long(bare(Key::F9));
        assert!(s.on_key(F9, true, NO_FLAGS, 0).is_empty());
        assert!(s.is_pending());
        assert!(s.tick(399).is_empty());
        assert_eq!(ids(&s.tick(400)), vec![(1, PressKind::Long)]);
        assert!(s.tick(500).is_empty());
        assert!(s.on_key(F9, false, NO_FLAGS, 600).is_empty());
        assert!(!s.is_pending());
    }

    #[test]
    fn long_only_chord_short_press_does_nothing() {
        let mut s = long(bare(Key::F9));
        assert!(s.on_key(F9, true, NO_FLAGS, 0).is_empty());
        assert!(s.on_key(F9, false, NO_FLAGS, 100).is_empty());
        assert!(!s.is_pending());
    }

    #[test]
    fn shared_chord_quick_release_is_tap() {
        let mut s = both(bare(Key::F9));
        assert!(s.on_key(F9, true, NO_FLAGS, 0).is_empty());
        assert!(s.tick(100).is_empty());
        assert_eq!(
            ids(&s.on_key(F9, false, NO_FLAGS, 150)),
            vec![(1, PressKind::Tap)]
        );
        assert!(!s.is_pending());
    }

    #[test]
    fn shared_chord_hold_is_long_once_and_not_again_on_release() {
        let mut s = both(bare(Key::F9));
        assert!(s.on_key(F9, true, NO_FLAGS, 0).is_empty());
        assert!(s.tick(200).is_empty());
        assert_eq!(ids(&s.tick(400)), vec![(1, PressKind::Long)]);
        assert!(s.tick(450).is_empty());
        assert!(s.on_key(F9, false, NO_FLAGS, 500).is_empty());
    }

    #[test]
    fn shared_chord_release_at_threshold_without_tick_is_long() {
        let mut s = both(bare(Key::F9));
        s.on_key(F9, true, NO_FLAGS, 0);
        assert_eq!(
            ids(&s.on_key(F9, false, NO_FLAGS, 500)),
            vec![(1, PressKind::Long)]
        );
    }

    #[test]
    fn shared_chord_release_just_under_threshold_is_tap() {
        let mut s = both(bare(Key::F9));
        s.on_key(F9, true, NO_FLAGS, 0);
        assert_eq!(
            ids(&s.on_key(F9, false, NO_FLAGS, 399)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn two_chords_track_holds_independently() {
        let mut s = HookState::new(
            MS,
            &[
                (1, bare(Key::F9), true, true),
                (2, bare(Key::F10), true, true),
            ],
        );
        s.on_key(F9, true, NO_FLAGS, 0);
        s.on_key(0x79, true, NO_FLAGS, 50); // F10
        assert_eq!(
            ids(&s.on_key(F9, false, NO_FLAGS, 100)),
            vec![(1, PressKind::Tap)]
        );
        assert_eq!(ids(&s.tick(450)), vec![(2, PressKind::Long)]);
    }

    #[test]
    fn modifier_released_before_key_does_not_cancel_or_double_fire() {
        let mut s = HookState::new(MS, &[(1, chord("Ctrl+F9"), true, true)]);
        s.on_key(VK_LCONTROL, true, NO_FLAGS, 0);
        assert!(s.on_key(F9, true, NO_FLAGS, 10).is_empty());
        s.on_key(VK_LCONTROL, false, NO_FLAGS, 20);
        // Ctrl is up but F9 is still held: the hold continues and fires long once.
        assert_eq!(ids(&s.tick(410)), vec![(1, PressKind::Long)]);
        assert!(s.on_key(F9, false, NO_FLAGS, 500).is_empty());
    }

    #[test]
    fn modifier_released_before_tap_key_fires_tap_once() {
        let mut s = tap(chord("Ctrl+F9"));
        s.on_key(VK_LCONTROL, true, NO_FLAGS, 0);
        assert_eq!(s.on_key(F9, true, NO_FLAGS, 5).len(), 1);
        s.on_key(VK_LCONTROL, false, NO_FLAGS, 10);
        assert!(s.on_key(F9, false, NO_FLAGS, 20).is_empty());
        // A fresh press with no modifier must not match the Ctrl chord.
        assert!(s.on_key(F9, true, NO_FLAGS, 30).is_empty());
    }

    #[test]
    fn key_up_without_prior_down_is_ignored() {
        let mut s = both(bare(Key::F9));
        assert!(s.on_key(F9, false, NO_FLAGS, 100).is_empty());
        assert!(!s.is_pending());
        // An unrelated key up is also ignored.
        assert!(s.on_key(A, false, NO_FLAGS, 120).is_empty());
    }

    #[test]
    fn modifier_key_up_without_down_is_harmless() {
        let mut s = tap(bare(Key::F9));
        s.on_key(VK_LCONTROL, false, NO_FLAGS, 0);
        assert_eq!(s.on_key(F9, true, NO_FLAGS, 5).len(), 1);
    }

    #[test]
    fn suspended_state_fires_nothing_but_still_tracks_modifiers() {
        let mut s = tap(chord("Ctrl+F9"));
        s.set_suspension(Some(1_000));
        s.on_key(VK_LCONTROL, true, NO_FLAGS, 1_010);
        assert!(s.on_key(F9, true, NO_FLAGS, 1_020).is_empty());
        s.on_key(F9, false, NO_FLAGS, 1_030);
        s.set_suspension(None);
        // Ctrl was tracked during the suspension, so the chord fires now.
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, 2_000)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn suspension_clears_a_pending_hold() {
        let mut s = long(bare(Key::F9));
        s.on_key(F9, true, NO_FLAGS, 0);
        s.set_suspension(Some(100));
        assert!(s.tick(400).is_empty());
        assert!(!s.is_pending());
    }

    #[test]
    fn suspension_expires_after_sixty_seconds() {
        let mut s = tap(bare(Key::F9));
        s.set_suspension(Some(0));
        assert!(s
            .on_key(F9, true, NO_FLAGS, SUSPEND_TIMEOUT_MS - 1)
            .is_empty());
        s.on_key(F9, false, NO_FLAGS, SUSPEND_TIMEOUT_MS);
        assert_eq!(
            ids(&s.on_key(F9, true, NO_FLAGS, SUSPEND_TIMEOUT_MS)),
            vec![(1, PressKind::Tap)]
        );
    }

    #[test]
    fn unbound_keys_and_empty_slots_are_ignored() {
        let mut s = HookState::new(MS, &[(1, bare(Key::F9), false, false)]);
        assert!(s.on_key(F9, true, NO_FLAGS, 0).is_empty());
        assert!(!s.is_pending());
        assert!(s.on_key(A, true, NO_FLAGS, 5).is_empty());
    }
}
