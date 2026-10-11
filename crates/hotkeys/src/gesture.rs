//! Tap versus long-press decisions for chords shared by a tap and a long clip
//! binding. Pure: no Win32 calls and no clock. Every time is a `u64`
//! millisecond value supplied by the caller, and the caller polls while
//! [`GestureTracker::is_pending`] is true.

use std::collections::BTreeMap;

use crate::registry::PressKind;

/// Identifier the caller uses for a chord (for example an index into its own
/// table of registered chords).
pub type ChordId = u32;

/// Whether a chord is waiting for its key to be released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Held { since: u64, long_fired: bool },
}

/// Per-chord bindings and current hold state.
#[derive(Debug, Clone, Copy)]
struct Entry {
    has_tap: bool,
    has_long: bool,
    state: State,
}

/// Decides when a chord press is a tap and when it is a long press.
#[derive(Debug, Clone)]
pub struct GestureTracker {
    long_press_ms: u64,
    chords: BTreeMap<ChordId, Entry>,
}

impl GestureTracker {
    /// `chords` lists `(id, has_tap, has_long)` for every chord that has at
    /// least one enabled binding. `long_press_ms` is the hold threshold.
    pub fn new(long_press_ms: u64, chords: &[(ChordId, bool, bool)]) -> GestureTracker {
        let chords = chords
            .iter()
            .filter(|(_, has_tap, has_long)| *has_tap || *has_long)
            .map(|&(id, has_tap, has_long)| {
                (
                    id,
                    Entry {
                        has_tap,
                        has_long,
                        state: State::Idle,
                    },
                )
            })
            .collect();
        GestureTracker {
            long_press_ms,
            chords,
        }
    }

    /// The chord's WM_HOTKEY arrived at `now_ms`.
    ///
    /// Returns `Some(PressKind::Tap)` when the chord has only a tap binding
    /// (fire now, no state kept). Returns `None` otherwise: a hold starts, or
    /// the press is ignored because the chord is already held.
    pub fn on_press(&mut self, id: ChordId, now_ms: u64) -> Option<PressKind> {
        let entry = self.chords.get_mut(&id)?;
        if entry.has_tap && !entry.has_long {
            return Some(PressKind::Tap);
        }
        if entry.has_long && entry.state == State::Idle {
            entry.state = State::Held {
                since: now_ms,
                long_fired: false,
            };
        }
        None
    }

    /// Called periodically while [`is_pending`](Self::is_pending) is true.
    /// `is_down(id)` reports whether that chord's key is physically held now.
    /// Returns the `(chord, press)` events that fire at this poll, in
    /// ascending chord id order.
    pub fn on_poll(
        &mut self,
        now_ms: u64,
        is_down: impl Fn(ChordId) -> bool,
    ) -> Vec<(ChordId, PressKind)> {
        let threshold = self.long_press_ms;
        let mut events = Vec::new();
        for (&id, entry) in self.chords.iter_mut() {
            let (since, long_fired) = match entry.state {
                State::Idle => continue,
                State::Held { since, long_fired } => (since, long_fired),
            };
            let elapsed = now_ms.saturating_sub(since);
            if !is_down(id) {
                entry.state = State::Idle;
                if !long_fired {
                    if elapsed >= threshold {
                        if entry.has_long {
                            events.push((id, PressKind::Long));
                        }
                    } else if entry.has_tap {
                        events.push((id, PressKind::Tap));
                    }
                }
            } else if !long_fired && elapsed >= threshold {
                events.push((id, PressKind::Long));
                entry.state = State::Held {
                    since,
                    long_fired: true,
                };
            }
        }
        events
    }

    /// True while at least one chord is held, so the caller keeps polling.
    pub fn is_pending(&self) -> bool {
        self.chords
            .values()
            .any(|e| matches!(e.state, State::Held { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 400;

    fn tap_only() -> GestureTracker {
        GestureTracker::new(MS, &[(1, true, false)])
    }

    fn long_only() -> GestureTracker {
        GestureTracker::new(MS, &[(1, false, true)])
    }

    fn both() -> GestureTracker {
        GestureTracker::new(MS, &[(1, true, true)])
    }

    #[test]
    fn tap_only_fires_immediately() {
        let mut g = tap_only();
        assert_eq!(g.on_press(1, 0), Some(PressKind::Tap));
        assert!(g.on_poll(50, |_| true).is_empty());
        assert!(!g.is_pending());
    }

    #[test]
    fn long_only_fires_at_threshold() {
        let mut g = long_only();
        assert_eq!(g.on_press(1, 0), None);
        assert!(g.on_poll(399, |_| true).is_empty());
        assert_eq!(g.on_poll(400, |_| true), vec![(1, PressKind::Long)]);
        assert!(g.on_poll(500, |_| true).is_empty());
        assert!(g.on_poll(600, |_| false).is_empty());
        assert!(!g.is_pending());
    }

    #[test]
    fn long_only_short_press_does_nothing() {
        let mut g = long_only();
        assert_eq!(g.on_press(1, 0), None);
        assert!(g.on_poll(100, |_| false).is_empty());
        assert!(!g.is_pending());
    }

    #[test]
    fn both_quick_tap_fires_tap_on_release() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert!(g.on_poll(100, |_| true).is_empty());
        assert_eq!(g.on_poll(150, |_| false), vec![(1, PressKind::Tap)]);
        assert!(!g.is_pending());
    }

    #[test]
    fn both_hold_fires_long_once_at_threshold() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert!(g.on_poll(200, |_| true).is_empty());
        assert_eq!(g.on_poll(400, |_| true), vec![(1, PressKind::Long)]);
        assert!(g.on_poll(401, |_| true).is_empty());
        assert!(g.on_poll(450, |_| false).is_empty());
        assert!(!g.is_pending());
    }

    #[test]
    fn both_released_late_without_seeing_threshold() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_poll(500, |_| false), vec![(1, PressKind::Long)]);
        assert!(!g.is_pending());
    }

    #[test]
    fn release_at_exactly_threshold_is_long() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_poll(MS, |_| false), vec![(1, PressKind::Long)]);
    }

    #[test]
    fn release_just_under_threshold_is_tap() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_poll(MS - 1, |_| false), vec![(1, PressKind::Tap)]);
    }

    #[test]
    fn two_chords_are_independent() {
        let mut g = GestureTracker::new(MS, &[(1, true, true), (2, true, true)]);
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_press(2, 50), None);
        assert_eq!(g.on_poll(100, |id| id == 2), vec![(1, PressKind::Tap)]);
        assert_eq!(g.on_poll(450, |_| false), vec![(2, PressKind::Long)]);
        assert!(!g.is_pending());
    }

    #[test]
    fn repress_during_hold_is_ignored() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_poll(400, |_| true), vec![(1, PressKind::Long)]);
        assert_eq!(g.on_press(1, 420), None);
        assert!(g.on_poll(500, |_| true).is_empty());
        assert!(g.on_poll(600, |_| false).is_empty());
    }

    #[test]
    fn press_during_pending_hold_does_not_restart_timer() {
        let mut g = both();
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_press(1, 100), None);
        assert_eq!(g.on_poll(400, |_| true), vec![(1, PressKind::Long)]);
    }

    #[test]
    fn unknown_or_empty_chord_is_ignored() {
        let mut g = GestureTracker::new(MS, &[(1, false, false)]);
        assert_eq!(g.on_press(1, 0), None);
        assert_eq!(g.on_press(9, 0), None);
        assert!(!g.is_pending());
    }
}
