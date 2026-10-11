//! Pure start/stop debouncer (5.6, 5.12, 5.15).
//!
//! The caller runs candidate evaluation each tick, picks the best eligible
//! candidate (as a [`Pick`]), reports the liveness of the currently bound
//! target, and passes both to [`Debouncer::update`] together with `now_ms`.
//! The debouncer answers with [`DetectEvent`]s. It never reads a clock.

use crate::timing::WINDOW_GONE_GRACE_MS;
use crate::{CaptureTarget, DetectConfig};

/// Pinned (allowlisted / manual) games skip the start debounce once their
/// process has been running for longer than this.
const PINNED_SKIP_CONFIRM_AFTER_MS: u64 = 5_000;
/// A retarget to a different window of the same game must be stable this long.
const RETARGET_CONFIRM_MS: i64 = 2_000;

/// Why a game started recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartReason {
    /// Heuristic eligibility.
    Detected,
    /// Matched an allowlist rule.
    Allowlisted,
    /// Added manually by the user.
    Manual,
    /// Already running when the service started.
    AlreadyRunning,
}

/// Why a game stopped recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The target process exited.
    ProcessExited,
    /// The target window was destroyed (process still alive).
    WindowDestroyed,
    /// The allow rule for the game was removed.
    RuleRemoved,
    /// Detection mode was switched off.
    ModeOff,
    /// Service shutdown.
    Shutdown,
    /// The stop grace period expired.
    GraceExpired,
}

/// Liveness of the currently bound target, as observed by the caller this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Process and window are still present.
    Alive,
    /// The target window is gone.
    WindowDestroyed,
    /// The target process is gone.
    ProcessExited,
    /// Allowlist-only mode released an unpinned recording. The game still runs,
    /// so it ends after the normal stop grace, not at once.
    Released,
}

/// The best eligible candidate for this tick, as seen by the debouncer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// Stable game identifier (same value for relaunches of the same game).
    pub game: String,
    /// Name shown in the UI.
    pub display_name: String,
    /// Window to capture.
    pub target: CaptureTarget,
    /// Reason to report if this pick starts a recording.
    pub reason: StartReason,
    /// True for allowlisted or manually added games.
    pub pinned: bool,
    /// Milliseconds since the owning process started.
    pub process_age_ms: u64,
}

/// Events emitted by the debouncer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectEvent {
    /// A game started (after the start debounce).
    GameStarted {
        /// Game identifier.
        game: String,
        /// Display name.
        display_name: String,
        /// Bound window.
        target: CaptureTarget,
        /// Why it started.
        reason: StartReason,
        /// Event time in ms.
        at_ms: i64,
    },
    /// The bound window changed within the same game (no stop).
    TargetChanged {
        /// Game identifier.
        game: String,
        /// Previous window.
        old: CaptureTarget,
        /// New window.
        new: CaptureTarget,
        /// Event time in ms.
        at_ms: i64,
    },
    /// The same game came back within the stop grace period.
    GameRelaunched {
        /// Game identifier.
        game: String,
        /// Name shown in the UI (same source as [`DetectEvent::GameStarted`]).
        display_name: String,
        /// Newly bound window.
        target: CaptureTarget,
        /// Time between the loss and the relaunch, in ms.
        gap_ms: u32,
        /// Event time in ms.
        at_ms: i64,
    },
    /// The game stopped (after the grace period, or immediately via [`Debouncer::stop`]).
    GameStopped {
        /// Game identifier.
        game: String,
        /// Why it stopped.
        reason: StopReason,
        /// Event time in ms.
        at_ms: i64,
    },
}

/// Stop reason for a target that is no longer alive; `None` while it is alive.
fn stop_reason(liveness: Liveness) -> Option<StopReason> {
    match liveness {
        Liveness::Alive => None,
        Liveness::WindowDestroyed => Some(StopReason::WindowDestroyed),
        Liveness::ProcessExited => Some(StopReason::ProcessExited),
        Liveness::Released => Some(StopReason::RuleRemoved),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Idle,
    Pending {
        pick: Pick,
        since: i64,
    },
    Active {
        pick: Pick,
        /// Candidate new target and the time it was first seen.
        retarget: Option<(CaptureTarget, i64)>,
    },
    Ending {
        pick: Pick,
        since: i64,
        reason: StopReason,
    },
}

/// Debounces start and stop decisions. Pure: all time is passed in as `now_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Debouncer {
    start_confirm_ms: u32,
    stop_grace_ms: u32,
    state: State,
}

impl Debouncer {
    /// Creates an idle debouncer.
    pub fn new(start_confirm_ms: u32, stop_grace_ms: u32) -> Self {
        Self {
            start_confirm_ms,
            stop_grace_ms,
            state: State::Idle,
        }
    }

    /// Creates an idle debouncer from the detection configuration.
    pub fn from_config(cfg: &DetectConfig) -> Self {
        Self::new(cfg.start_confirm_ms, cfg.stop_grace_ms)
    }

    /// Advances the state machine by one tick.
    ///
    /// `best` is the best eligible candidate this tick, or `None`.
    /// `liveness` describes the currently bound target; it is only consulted
    /// while a game is active or ending.
    pub fn update(
        &mut self,
        now_ms: i64,
        best: Option<Pick>,
        liveness: Liveness,
    ) -> Vec<DetectEvent> {
        let mut out = Vec::new();
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match state {
            State::Idle => self.idle(now_ms, best, &mut out),
            State::Pending { pick, since } => self.pending(now_ms, pick, since, best, &mut out),
            State::Active { pick, retarget } => {
                self.active(now_ms, pick, retarget, best, liveness, &mut out)
            }
            State::Ending {
                pick,
                since,
                reason,
            } => self.ending(now_ms, pick, (since, reason), best, liveness, &mut out),
        };
        out
    }

    /// Stops the current game immediately (mode off, rule removed, shutdown).
    /// Emits `GameStopped` if a game was active or ending; otherwise nothing.
    pub fn stop(&mut self, now_ms: i64, reason: StopReason) -> Vec<DetectEvent> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        match state {
            State::Active { pick, .. } | State::Ending { pick, .. } => {
                vec![DetectEvent::GameStopped {
                    game: pick.game,
                    reason,
                    at_ms: now_ms,
                }]
            }
            State::Idle | State::Pending { .. } => Vec::new(),
        }
    }

    /// True while a game is recording (Active state).
    pub fn is_active(&self) -> bool {
        matches!(self.state, State::Active { .. })
    }

    /// The window currently bound while a game is active.
    pub fn active_target(&self) -> Option<CaptureTarget> {
        match &self.state {
            State::Active { pick, .. } => Some(pick.target),
            _ => None,
        }
    }

    /// The pick currently recording while the state is Active.
    pub(crate) fn active_pick(&self) -> Option<&Pick> {
        match &self.state {
            State::Active { pick, .. } => Some(pick),
            _ => None,
        }
    }

    fn start_confirm_for(&self, pick: &Pick) -> i64 {
        if pick.pinned && pick.process_age_ms > PINNED_SKIP_CONFIRM_AFTER_MS {
            0
        } else {
            i64::from(self.start_confirm_ms)
        }
    }

    fn idle(&self, now: i64, best: Option<Pick>, out: &mut Vec<DetectEvent>) -> State {
        match best {
            None => State::Idle,
            Some(pick) => self.begin_pending(now, pick, out),
        }
    }

    fn begin_pending(&self, now: i64, pick: Pick, out: &mut Vec<DetectEvent>) -> State {
        if self.start_confirm_for(&pick) == 0 {
            self.start(now, pick, out)
        } else {
            State::Pending { pick, since: now }
        }
    }

    fn start(&self, now: i64, pick: Pick, out: &mut Vec<DetectEvent>) -> State {
        out.push(DetectEvent::GameStarted {
            game: pick.game.clone(),
            display_name: pick.display_name.clone(),
            target: pick.target,
            reason: pick.reason,
            at_ms: now,
        });
        State::Active {
            pick,
            retarget: None,
        }
    }

    fn pending(
        &self,
        now: i64,
        pick: Pick,
        since: i64,
        best: Option<Pick>,
        out: &mut Vec<DetectEvent>,
    ) -> State {
        match best {
            None => State::Idle,
            // The best candidate changed to another game: restart the debounce.
            Some(p) if p.game != pick.game => self.begin_pending(now, p, out),
            Some(p) => {
                if now.saturating_sub(since) >= self.start_confirm_for(&p) {
                    self.start(now, p, out)
                } else {
                    State::Pending { pick: p, since }
                }
            }
        }
    }

    fn active(
        &self,
        now: i64,
        pick: Pick,
        retarget: Option<(CaptureTarget, i64)>,
        best: Option<Pick>,
        liveness: Liveness,
        out: &mut Vec<DetectEvent>,
    ) -> State {
        let stop = stop_reason(liveness);
        match best {
            Some(p) if p.game == pick.game && p.target != pick.target => {
                if stop.is_some() {
                    // Target gone but the same game has another window right now.
                    out.push(DetectEvent::TargetChanged {
                        game: pick.game,
                        old: pick.target,
                        new: p.target,
                        at_ms: now,
                    });
                    return State::Active {
                        pick: p,
                        retarget: None,
                    };
                }
                let since = match retarget {
                    Some((t, since)) if t == p.target => since,
                    _ => now,
                };
                if now.saturating_sub(since) >= RETARGET_CONFIRM_MS {
                    out.push(DetectEvent::TargetChanged {
                        game: pick.game,
                        old: pick.target,
                        new: p.target,
                        at_ms: now,
                    });
                    State::Active {
                        pick: p,
                        retarget: None,
                    }
                } else {
                    let target = p.target;
                    State::Active {
                        pick,
                        retarget: Some((target, since)),
                    }
                }
            }
            // Same window still listed: liveness decides.
            Some(p) if p.game == pick.game => match stop {
                None => State::Active {
                    pick: p,
                    retarget: None,
                },
                Some(reason) => self.lost(now, pick, reason, None, out),
            },
            // No candidate for this game: alt-tab and gate failures do not stop
            // a game whose target is still alive.
            other => match stop {
                None => State::Active {
                    pick,
                    retarget: None,
                },
                Some(reason) => self.lost(now, pick, reason, other, out),
            },
        }
    }

    /// The bound target is lost and no window of the same game replaced it in
    /// this tick. A process exit ends the game at once (no grace: the process is
    /// gone for good). A destroyed window enters `Ending` and waits its grace.
    /// `other` is the best candidate for a different game, if any; it may start
    /// its own pending debounce.
    fn lost(
        &self,
        now: i64,
        pick: Pick,
        reason: StopReason,
        other: Option<Pick>,
        out: &mut Vec<DetectEvent>,
    ) -> State {
        if reason == StopReason::ProcessExited {
            out.push(DetectEvent::GameStopped {
                game: pick.game,
                reason,
                at_ms: now,
            });
            return self.idle(now, other, out);
        }
        State::Ending {
            pick,
            since: now,
            reason,
        }
    }

    /// Stop grace for a lost target, in ms. A process exit has none. A destroyed
    /// window gets `WINDOW_GONE_GRACE_MS`, capped by `stop_grace_ms` when smaller.
    fn grace_ms(&self, reason: StopReason) -> i64 {
        match reason {
            StopReason::ProcessExited => 0,
            StopReason::WindowDestroyed => i64::from(WINDOW_GONE_GRACE_MS.min(self.stop_grace_ms)),
            _ => i64::from(self.stop_grace_ms),
        }
    }

    /// `(since, reason)` is the `Ending` state being advanced.
    fn ending(
        &self,
        now: i64,
        pick: Pick,
        (since, reason): (i64, StopReason),
        best: Option<Pick>,
        liveness: Liveness,
        out: &mut Vec<DetectEvent>,
    ) -> State {
        let reason = if liveness == Liveness::ProcessExited {
            StopReason::ProcessExited
        } else {
            reason
        };
        match best {
            Some(p) if p.game == pick.game && p.target != pick.target => {
                let gap = now.saturating_sub(since).clamp(0, i64::from(u32::MAX)) as u32;
                out.push(DetectEvent::GameRelaunched {
                    game: pick.game,
                    display_name: p.display_name.clone(),
                    target: p.target,
                    gap_ms: gap,
                    at_ms: now,
                });
                State::Active {
                    pick: p,
                    retarget: None,
                }
            }
            other => {
                if liveness == Liveness::Alive {
                    // The bound window and its process answer again (a failed probe
                    // or a flicker): the same game continues, no stop, no relaunch.
                    return State::Active {
                        pick,
                        retarget: None,
                    };
                }
                // The lost window itself is not a replacement.
                let other = other.filter(|p| p.target != pick.target);
                if reason == StopReason::ProcessExited
                    || now.saturating_sub(since) >= self.grace_ms(reason)
                {
                    out.push(DetectEvent::GameStopped {
                        game: pick.game,
                        reason,
                        at_ms: now,
                    });
                    return self.idle(now, other, out);
                }
                State::Ending {
                    pick,
                    since,
                    reason,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: CaptureTarget = CaptureTarget { pid: 10, hwnd: 100 };
    const T2: CaptureTarget = CaptureTarget { pid: 10, hwnd: 200 };

    fn pick(game: &str, target: CaptureTarget) -> Pick {
        Pick {
            game: game.into(),
            display_name: game.to_uppercase(),
            target,
            reason: StartReason::Detected,
            pinned: false,
            process_age_ms: 60_000,
        }
    }

    #[test]
    fn starts_after_three_seconds_stable() {
        let mut d = Debouncer::new(3000, 10_000);
        assert!(d.update(0, Some(pick("g", T1)), Liveness::Alive).is_empty());
        assert!(d
            .update(2999, Some(pick("g", T1)), Liveness::Alive)
            .is_empty());
        let ev = d.update(3000, Some(pick("g", T1)), Liveness::Alive);
        assert_eq!(
            ev,
            vec![DetectEvent::GameStarted {
                game: "g".into(),
                display_name: "G".into(),
                target: T1,
                reason: StartReason::Detected,
                at_ms: 3000,
            }]
        );
        assert!(d.is_active());
    }

    #[test]
    fn pending_is_aborted_when_best_disappears_or_changes() {
        let mut d = Debouncer::new(3000, 10_000);
        d.update(0, Some(pick("a", T1)), Liveness::Alive);
        d.update(1000, None, Liveness::Alive);
        // New candidate restarts the timer from 2000.
        d.update(2000, Some(pick("b", T1)), Liveness::Alive);
        assert!(d
            .update(4999, Some(pick("b", T1)), Liveness::Alive)
            .is_empty());
        assert_eq!(
            d.update(5000, Some(pick("b", T1)), Liveness::Alive).len(),
            1
        );
    }

    #[test]
    fn pinned_old_process_starts_immediately() {
        let mut d = Debouncer::new(3000, 10_000);
        let mut p = pick("pinned", T1);
        p.pinned = true;
        p.reason = StartReason::Allowlisted;
        p.process_age_ms = 6_000;
        assert_eq!(d.update(0, Some(p), Liveness::Alive).len(), 1);
    }

    #[test]
    fn pinned_young_process_uses_normal_confirm() {
        let mut d = Debouncer::new(3000, 10_000);
        let mut p = pick("pinned", T1);
        p.pinned = true;
        p.process_age_ms = 1_000;
        assert!(d.update(0, Some(p.clone()), Liveness::Alive).is_empty());
        assert_eq!(d.update(3000, Some(p), Liveness::Alive).len(), 1);
    }

    #[test]
    fn relaunch_within_window_grace_continues_session() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        assert!(d.update(1000, None, Liveness::WindowDestroyed).is_empty());
        let ev = d.update(1500, Some(pick("g", T2)), Liveness::WindowDestroyed);
        assert_eq!(
            ev,
            vec![DetectEvent::GameRelaunched {
                game: "g".into(),
                display_name: "G".into(),
                target: T2,
                gap_ms: 500,
                at_ms: 1500,
            }]
        );
        assert!(d.is_active());
    }

    #[test]
    fn process_exit_stops_at_once_without_grace() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        let ev = d.update(1000, None, Liveness::ProcessExited);
        assert_eq!(
            ev,
            vec![DetectEvent::GameStopped {
                game: "g".into(),
                reason: StopReason::ProcessExited,
                at_ms: 1000,
            }]
        );
        assert!(!d.is_active());
    }

    #[test]
    fn window_gone_grace_is_one_second_and_capped_by_stop_grace() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        d.update(1000, None, Liveness::WindowDestroyed);
        assert!(d.update(1999, None, Liveness::WindowDestroyed).is_empty());
        let ev = d.update(2000, None, Liveness::WindowDestroyed);
        assert_eq!(
            ev,
            vec![DetectEvent::GameStopped {
                game: "g".into(),
                reason: StopReason::WindowDestroyed,
                at_ms: 2000,
            }]
        );
        assert!(!d.is_active());

        // A shorter configured stop grace caps the window-gone grace.
        let mut short = Debouncer::new(0, 300);
        short.update(0, Some(pick("g", T1)), Liveness::Alive);
        short.update(1000, None, Liveness::WindowDestroyed);
        assert!(short
            .update(1299, None, Liveness::WindowDestroyed)
            .is_empty());
        assert_eq!(short.update(1300, None, Liveness::WindowDestroyed).len(), 1);
    }

    #[test]
    fn window_gone_then_process_exit_stops_immediately() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        d.update(1000, None, Liveness::WindowDestroyed);
        let ev = d.update(1200, None, Liveness::ProcessExited);
        assert_eq!(
            ev,
            vec![DetectEvent::GameStopped {
                game: "g".into(),
                reason: StopReason::ProcessExited,
                at_ms: 1200,
            }]
        );
    }

    #[test]
    fn transient_failed_probe_while_ending_resumes_silently() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        d.update(1000, None, Liveness::WindowDestroyed);
        // The window answers again inside the grace: no event, no stop.
        assert!(d.update(1250, None, Liveness::Alive).is_empty());
        assert!(d.is_active());
        assert!(d.update(5000, None, Liveness::Alive).is_empty());
        assert!(d.is_active());
    }

    #[test]
    fn relaunch_after_grace_is_a_new_start() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        d.update(1000, None, Liveness::ProcessExited);
        d.update(11_000, None, Liveness::ProcessExited);
        let ev = d.update(12_000, Some(pick("g", T2)), Liveness::ProcessExited);
        assert!(matches!(ev.as_slice(), [DetectEvent::GameStarted { .. }]));
    }

    #[test]
    fn retarget_needs_two_seconds_stable() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        assert!(d
            .update(1000, Some(pick("g", T2)), Liveness::Alive)
            .is_empty());
        assert!(d
            .update(2999, Some(pick("g", T2)), Liveness::Alive)
            .is_empty());
        let ev = d.update(3000, Some(pick("g", T2)), Liveness::Alive);
        assert_eq!(
            ev,
            vec![DetectEvent::TargetChanged {
                game: "g".into(),
                old: T1,
                new: T2,
                at_ms: 3000,
            }]
        );
    }

    #[test]
    fn retarget_resets_if_candidate_flips_back() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        d.update(1000, Some(pick("g", T2)), Liveness::Alive);
        d.update(2000, Some(pick("g", T1)), Liveness::Alive);
        assert!(d
            .update(3000, Some(pick("g", T2)), Liveness::Alive)
            .is_empty());
    }

    #[test]
    fn target_replaced_while_dead_switches_immediately() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        let ev = d.update(500, Some(pick("g", T2)), Liveness::WindowDestroyed);
        assert!(matches!(ev.as_slice(), [DetectEvent::TargetChanged { .. }]));
        assert!(d.is_active());
    }

    #[test]
    fn alt_tab_does_not_stop_alive_game() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        assert!(d.update(60_000, None, Liveness::Alive).is_empty());
        assert!(d.is_active());
    }

    #[test]
    fn window_destroyed_gives_window_destroyed_reason() {
        let mut d = Debouncer::new(0, 1_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        d.update(100, None, Liveness::WindowDestroyed);
        let ev = d.update(1_100, None, Liveness::WindowDestroyed);
        assert!(matches!(
            ev.as_slice(),
            [DetectEvent::GameStopped {
                reason: StopReason::WindowDestroyed,
                ..
            }]
        ));
    }

    #[test]
    fn immediate_stop_for_mode_off() {
        let mut d = Debouncer::new(0, 10_000);
        d.update(0, Some(pick("g", T1)), Liveness::Alive);
        let ev = d.stop(500, StopReason::ModeOff);
        assert_eq!(
            ev,
            vec![DetectEvent::GameStopped {
                game: "g".into(),
                reason: StopReason::ModeOff,
                at_ms: 500,
            }]
        );
        assert!(d.stop(600, StopReason::Shutdown).is_empty());
    }
}
