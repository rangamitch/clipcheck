//! clipcheck-session: pure session-recording state machine.
//!
//! See `docs/design/06-session-recording.md` sections 4.6, 5.2 (transition
//! table T1-T46) and 5.4 (zero-gap split). `reduce` performs no I/O, spawns no
//! threads and reads no clock.
//!
//! Reply contract (issue #146): every `UserStart`, `UserStop` and `UserSplit`
//! yields exactly one `Effect::Reply`, in every state. Accepted commands reply
//! `Ok(())`; rejected ones reply the matching `CommandError`. Other unlisted
//! (state, event) pairs return the state unchanged with no effects.

mod state;

pub use state::{
    CommandError, Effect, EndReason, Event, ExitReason, Mode, Notice, Resume, SessionConfig,
    SessionState, SplitReason, State, WriteError,
};

/// Number of keyframe timeouts after which a pending split is cancelled.
const KEYFRAME_RETRY_LIMIT: u32 = 3;

/// Advance the machine by one event, from a borrowed state.
///
/// Clones `state` once, then delegates to [`reduce_owned`]. A shell that owns
/// its state should call [`reduce_owned`] or [`reduce_in_place`] instead.
/// Behaviour is identical to [`reduce_owned`].
pub fn reduce(state: &SessionState, ev: Event) -> (SessionState, Vec<Effect>) {
    reduce_owned(state.clone(), ev)
}

/// Advance the machine by one event, consuming the state (no clone).
///
/// Returns the next state and the ordered list of effects for the shell.
/// Same rules as [`reduce_in_place`]. Issue #41.
///
/// Reply contract (issue #146): every `UserStart`, `UserStop` and `UserSplit`
/// produces exactly one `Effect::Reply`, whatever the state. Accepted commands
/// reply `Ok(())`. Rejected commands reply the matching `CommandError`
/// (`NotEnabled`, `NoTargetGame`, `NotRecording`, `AlreadyRecording`,
/// `AlreadyPending` or `Busy`). Any other unlisted (state, event) pair returns
/// the state unchanged with no effects.
pub fn reduce_owned(mut state: SessionState, ev: Event) -> (SessionState, Vec<Effect>) {
    let fx = reduce_in_place(&mut state, ev);
    (state, fx)
}

/// Advance `s` by one event in place and return the effects. No clone is made.
///
/// Same rules as [`reduce_owned`]. A tick that changes nothing allocates nothing.
pub fn reduce_in_place(s: &mut SessionState, ev: Event) -> Vec<Effect> {
    let mut fx = Vec::new();
    if let Some(now) = ev.now_ms() {
        s.now_ms = now;
    }
    match ev {
        Event::Tick {
            part_elapsed_ms,
            part_bytes,
            free_bytes,
            ..
        } => {
            s.free_bytes = Some(free_bytes);
            expire_resume(s);
            if s.state == State::Recording {
                on_tick_recording(s, &mut fx, part_elapsed_ms, part_bytes);
            }
        }
        Event::Config(cfg) => s.config = cfg,
        // T46: already enabled, so only the mode changes.
        Event::SetEnabled {
            enabled: true,
            mode,
            new_session_id,
        } if s.enabled => {
            s.mode = mode;
            if s.state == State::Armed && mode == Mode::Auto {
                try_auto_start(s, &mut fx, new_session_id);
            }
        }
        other => match s.state {
            State::Idle => idle(s, &mut fx, other),
            State::Armed => armed(s, &mut fx, other),
            State::Recording => recording(s, &mut fx, other),
            State::Splitting => splitting(s, &mut fx, other),
            State::Stopping => stopping(s, &mut fx, other),
            State::StoppedByUser => stopped_by_user(s, &mut fx, other),
        },
    }
    fx
}

fn idle(s: &mut SessionState, fx: &mut Vec<Effect>, ev: Event) {
    match ev {
        // T1
        Event::SetEnabled {
            enabled: true,
            mode,
            new_session_id,
        } => {
            s.enabled = true;
            s.mode = mode;
            let auto_game = if mode == Mode::Auto {
                s.current_game.clone()
            } else {
                None
            };
            match auto_game {
                Some(game) if disk_ok(s) => start_part(s, fx, game, new_session_id),
                Some(_) => {
                    fx.push(Effect::Notify(Notice::DiskLowBlocked));
                    set_state(s, fx, State::Armed);
                }
                None => set_state(s, fx, State::Armed),
            }
        }
        // T2
        Event::GameStarted { game, .. } => s.current_game = Some(game),
        Event::GameEnded { game, .. } => clear_game(s, &game),
        // Idle means the feature is disabled, so every user command is refused.
        Event::UserStart { .. } => fx.push(Effect::Reply(Err(CommandError::NotEnabled))),
        Event::UserStop | Event::UserSplit => {
            fx.push(Effect::Reply(Err(CommandError::NotRecording)));
        }
        _ => {}
    }
}

fn armed(s: &mut SessionState, fx: &mut Vec<Effect>, ev: Event) {
    match ev {
        // T3, T4
        Event::GameStarted {
            game,
            new_session_id,
            ..
        } => {
            s.current_game = Some(game);
            if s.mode == Mode::Auto {
                try_auto_start(s, fx, new_session_id);
            }
        }
        // T5, T6
        Event::UserStart { new_session_id } => match s.current_game.clone() {
            Some(game) => {
                start_part(s, fx, game, new_session_id);
                fx.push(Effect::Reply(Ok(())));
            }
            None => fx.push(Effect::Reply(Err(CommandError::NoTargetGame))),
        },
        // T7
        Event::SetEnabled { enabled: false, .. } => {
            s.enabled = false;
            s.resume = None;
            s.session_id = None;
            set_state(s, fx, State::Idle);
        }
        // T8
        Event::GameEnded { game, .. } => clear_game(s, &game),
        // Not recording, so there is nothing to stop or split.
        Event::UserStop | Event::UserSplit => {
            fx.push(Effect::Reply(Err(CommandError::NotRecording)));
        }
        _ => {}
    }
}

fn recording(s: &mut SessionState, fx: &mut Vec<Effect>, ev: Event) {
    match ev {
        // T9
        Event::UserSplit => {
            split(s, fx, SplitReason::UserRequested);
            fx.push(Effect::Reply(Ok(())));
        }
        // Already recording, so a start is refused.
        Event::UserStart { .. } => fx.push(Effect::Reply(Err(CommandError::AlreadyRecording))),
        // T12
        Event::StreamParamsChanged => split(s, fx, SplitReason::StreamParamsChanged),
        // T13
        Event::LayoutChanged => split(s, fx, SplitReason::ConfigChanged),
        // T14
        Event::UserStop => {
            finalize(s, fx, EndReason::UserStop);
            fx.push(Effect::Reply(Ok(())));
        }
        // T15, T16
        Event::GameEnded { game, reason, .. } => on_game_ended(s, fx, &game, &reason),
        // T17: game switch detected via a start for a different game.
        Event::GameStarted { game, .. } => match s.current_game.clone() {
            Some(current) if current != game => {
                finalize(s, fx, EndReason::GameSwitched);
                s.pending_start = Some(game.clone());
                s.current_game = Some(game);
            }
            Some(_) => {}
            None => s.current_game = Some(game),
        },
        // T18
        Event::Suspend { .. } => {
            remember_resume(s);
            finalize(s, fx, EndReason::Suspend);
        }
        // T19, T21, T22
        Event::WriteFailed(err) => {
            finalize(s, fx, err.end_reason());
            fx.push(Effect::Notify(Notice::WriteFailed(err)));
        }
        // T23, T45
        Event::SetEnabled { enabled: false, .. } => {
            s.enabled = false;
            s.disable_pending = true;
            finalize(s, fx, EndReason::UserStop);
        }
        // T24
        Event::Shutdown => {
            s.shutdown_pending = true;
            finalize(s, fx, EndReason::ServiceShutdown);
        }
        _ => {}
    }
}

fn splitting(s: &mut SessionState, fx: &mut Vec<Effect>, ev: Event) {
    match ev {
        // T25: the keyframe is the boundary. Close the old part and open the
        // next one in the same batch, and only now bump the part index.
        Event::KeyframeArrived => {
            if let Some(reason) = s.split_pending.take() {
                s.part_index += 1;
                fx.push(Effect::FinalizePart {
                    reason: EndReason::Split(reason),
                });
                fx.push(Effect::OpenPart {
                    index: s.part_index,
                });
            }
            set_state(s, fx, State::Recording);
        }
        // T26, T27: the split is cancelled without touching the part index
        // or the current part.
        Event::KeyframeTimeout { attempts } => {
            if attempts < KEYFRAME_RETRY_LIMIT {
                fx.push(Effect::RequestKeyframe);
            } else {
                fx.push(Effect::CancelSplit);
                fx.push(Effect::Notify(Notice::KeyframeTimeout));
                s.split_pending = None;
                set_state(s, fx, State::Recording);
            }
        }
        // T28
        Event::UserSplit => fx.push(Effect::Reply(Err(CommandError::AlreadyPending))),
        // A split is still a recording in progress, so a start is refused.
        Event::UserStart { .. } => fx.push(Effect::Reply(Err(CommandError::AlreadyRecording))),
        // T29
        Event::UserStop => {
            finalize(s, fx, EndReason::UserStop);
            fx.push(Effect::Reply(Ok(())));
        }
        // T30
        Event::GameEnded { game, reason, .. } => on_game_ended(s, fx, &game, &reason),
        // T31
        Event::WriteFailed(err) => {
            finalize(s, fx, err.end_reason());
            fx.push(Effect::Notify(Notice::WriteFailed(err)));
        }
        // T32
        Event::SetEnabled { enabled: false, .. } => {
            s.enabled = false;
            s.disable_pending = true;
            finalize(s, fx, EndReason::UserStop);
        }
        Event::Shutdown => {
            s.shutdown_pending = true;
            finalize(s, fx, EndReason::ServiceShutdown);
        }
        _ => {}
    }
}

fn stopping(s: &mut SessionState, fx: &mut Vec<Effect>, ev: Event) {
    match ev {
        // T33 to T38
        Event::FinalizeDone { ok, new_session_id } => finish(s, fx, ok, new_session_id),
        // T39
        Event::UserStart { .. } | Event::UserStop | Event::UserSplit => {
            fx.push(Effect::Reply(Err(CommandError::Busy)));
        }
        // T40
        Event::GameStarted { game, .. } => {
            s.current_game = Some(game.clone());
            // A user stop stays sticky: a game detected during finalize must
            // not auto-start (StoppedByUser ignores detection until user start).
            if s.mode == Mode::Auto && s.end_reason != Some(EndReason::UserStop) {
                s.pending_start = Some(game);
            }
        }
        // Tracking only: keeps current_game accurate for the next decision.
        Event::GameEnded { game, .. } => clear_game(s, &game),
        // T45 (already finalizing, so only the flag changes)
        Event::SetEnabled { enabled: false, .. } => {
            s.enabled = false;
            s.disable_pending = true;
        }
        // T24 analogue (already finalizing): the flag makes finish() skip any
        // pending start, so no new part opens while the service exits.
        Event::Shutdown => s.shutdown_pending = true,
        _ => {}
    }
}

fn stopped_by_user(s: &mut SessionState, fx: &mut Vec<Effect>, ev: Event) {
    match ev {
        // T41
        Event::GameEnded { game, .. } => {
            if s.current_game.as_deref() == Some(game.as_str()) {
                s.current_game = None;
                // T41 lists "auto & enabled" but the T34 note says manual
                // waits in Armed too. Enabled always returns to Armed so an
                // enabled feature never sits in Idle.
                let next = if s.enabled { State::Armed } else { State::Idle };
                set_state(s, fx, next);
            }
        }
        // T42
        Event::UserStart { new_session_id } => match s.current_game.clone() {
            Some(game) => {
                start_part(s, fx, game, new_session_id);
                fx.push(Effect::Reply(Ok(())));
            }
            None => fx.push(Effect::Reply(Err(CommandError::NoTargetGame))),
        },
        // Stopped by the user, so there is no recording to stop or split.
        Event::UserStop | Event::UserSplit => {
            fx.push(Effect::Reply(Err(CommandError::NotRecording)));
        }
        // T43
        Event::SetEnabled { enabled: false, .. } => {
            s.enabled = false;
            set_state(s, fx, State::Idle);
        }
        // T44: GameStarted is suppressed until the user starts again.
        _ => {}
    }
}

/// Completion of the current part's finalize (T33 to T38).
fn finish(s: &mut SessionState, fx: &mut Vec<Effect>, ok: bool, new_session_id: String) {
    s.split_pending = None;
    if !ok {
        fx.push(Effect::Notify(Notice::PartialKept));
    }
    fx.push(Effect::StopEncoder);

    if s.disable_pending || s.shutdown_pending {
        s.disable_pending = false;
        s.shutdown_pending = false;
        s.pending_start = None;
        s.session_id = None;
        set_state(s, fx, State::Idle);
        return;
    }

    let had_pending = s.pending_start.is_some();
    if let Some(game) = s.pending_start.take() {
        if s.mode == Mode::Auto {
            if disk_ok(s) {
                start_part(s, fx, game, new_session_id);
                return;
            }
            fx.push(Effect::Notify(Notice::DiskLowBlocked));
        }
    }

    s.session_id = None;
    let reason = s.end_reason.clone();

    // T33
    if !had_pending && reason == Some(EndReason::UserStop) {
        set_state(s, fx, State::StoppedByUser);
        return;
    }
    // T36: crashed game may relaunch into the same session.
    let join_open = s.resume.as_ref().is_some_and(|r| r.deadline_ms >= s.now_ms);
    if reason == Some(EndReason::GameCrashed) && join_open && s.enabled {
        set_state(s, fx, State::Armed);
        return;
    }
    // T34, T37, T38
    let next = if s.enabled { State::Armed } else { State::Idle };
    set_state(s, fx, next);
}

/// Tick rules for Recording (T10, T11, T20).
fn on_tick_recording(s: &mut SessionState, fx: &mut Vec<Effect>, elapsed_ms: u64, bytes: u64) {
    if !disk_ok(s) {
        finalize(s, fx, EndReason::DiskLow);
        fx.push(Effect::Notify(Notice::DiskLow));
    } else if s.config.max_part_ms > 0 && elapsed_ms >= s.config.max_part_ms {
        split(s, fx, SplitReason::MaxDuration);
    } else if s.config.max_part_bytes > 0 && bytes >= s.config.max_part_bytes {
        split(s, fx, SplitReason::MaxSize);
    }
}

/// Game ended while recording or splitting (T15, T16, T30).
fn on_game_ended(s: &mut SessionState, fx: &mut Vec<Effect>, game: &str, reason: &ExitReason) {
    if s.current_game.as_deref() != Some(game) {
        return;
    }
    let end = match reason {
        ExitReason::Crashed { .. } => {
            if s.config.join_relaunch_after_crash {
                remember_resume(s);
            }
            EndReason::GameCrashed
        }
        ExitReason::Normal | ExitReason::Unknown => EndReason::GameExited,
    };
    s.current_game = None;
    finalize(s, fx, end);
}

/// Request a split at the next keyframe (T9, T10, T11, T12, T13).
///
/// Only enters `Splitting` and asks the encoder for a keyframe. The file and
/// the part index are not touched until `KeyframeArrived` (see `splitting`),
/// so the close and the open are applied at the same keyframe with no gap.
fn split(s: &mut SessionState, fx: &mut Vec<Effect>, reason: SplitReason) {
    s.split_pending = Some(reason);
    // Goes through set_state so subscribers see Splitting (#23).
    set_state(s, fx, State::Splitting);
    fx.push(Effect::RequestKeyframe);
}

/// Begin finalizing the current part and enter Stopping.
fn finalize(s: &mut SessionState, fx: &mut Vec<Effect>, reason: EndReason) {
    s.split_pending = None;
    s.end_reason = Some(reason.clone());
    fx.push(Effect::FinalizePart { reason });
    set_state(s, fx, State::Stopping);
}

/// Start a session for `game`, or join the previous session on relaunch.
///
/// `new_session_id` is used only when this is not a join.
fn start_part(s: &mut SessionState, fx: &mut Vec<Effect>, game: String, new_session_id: String) {
    let joined = s
        .resume
        .as_ref()
        .filter(|r| r.game == game && s.now_ms <= r.deadline_ms)
        .map(|r| r.session_id.clone());
    s.resume = None;
    let session_id = match joined {
        Some(id) => {
            s.part_index += 1;
            id
        }
        None => {
            s.part_index = 1;
            new_session_id
        }
    };
    s.session_id = Some(session_id.clone());
    s.current_game = Some(game.clone());
    s.pending_start = None;
    s.end_reason = None;
    fx.push(Effect::StartEncoder {
        session_id,
        part_index: s.part_index,
        game,
    });
    set_state(s, fx, State::Recording);
}

/// Auto start for a detected game, guarded by free disk space (T3, T4, T46).
fn try_auto_start(s: &mut SessionState, fx: &mut Vec<Effect>, new_session_id: String) {
    let Some(game) = s.current_game.clone() else {
        return;
    };
    if disk_ok(s) {
        start_part(s, fx, game, new_session_id);
    } else {
        fx.push(Effect::Notify(Notice::DiskLowBlocked));
    }
}

/// Remember the session so a relaunch of the same game can join it.
fn remember_resume(s: &mut SessionState) {
    if let (Some(session_id), Some(game)) = (s.session_id.clone(), s.current_game.clone()) {
        s.resume = Some(Resume {
            session_id,
            game,
            deadline_ms: s.now_ms + s.config.join_window_ms,
        });
    }
}

fn set_state(s: &mut SessionState, fx: &mut Vec<Effect>, next: State) {
    s.state = next;
    fx.push(Effect::EmitStatus(next));
}

fn clear_game(s: &mut SessionState, game: &str) {
    if s.current_game.as_deref() == Some(game) {
        s.current_game = None;
    }
}

fn expire_resume(s: &mut SessionState) {
    let expired = s.resume.as_ref().is_some_and(|r| s.now_ms > r.deadline_ms);
    if expired {
        s.resume = None;
    }
}

fn disk_ok(s: &SessionState) -> bool {
    s.free_bytes
        .is_none_or(|free| free >= s.config.min_free_bytes)
}

#[cfg(test)]
mod shutdown_in_stopping_tests {
    use super::*;

    fn config() -> SessionConfig {
        SessionConfig {
            min_free_bytes: 1,
            ..SessionConfig::default()
        }
    }

    /// Auto mode, enabled, recording game "a" with session "s1".
    fn recording_state() -> SessionState {
        let mut s = SessionState::new(config());
        s.state = State::Recording;
        s.enabled = true;
        s.mode = Mode::Auto;
        s.session_id = Some("s1".into());
        s.part_index = 1;
        s.current_game = Some("a".into());
        s
    }

    /// Stopping state with a pending auto-start for game "b".
    fn stopping_with_pending() -> SessionState {
        let (s, _) = reduce(
            &recording_state(),
            Event::GameStarted {
                game: "b".into(),
                now_ms: 0,
                new_session_id: "s2".into(),
            },
        );
        assert_eq!(s.state, State::Stopping);
        assert_eq!(s.pending_start.as_deref(), Some("b"));
        s
    }

    fn starts(fx: &[Effect]) -> usize {
        fx.iter()
            .filter(|e| matches!(e, Effect::StartEncoder { .. }))
            .count()
    }

    #[test]
    fn shutdown_in_stopping_suppresses_pending_start() {
        let (s, fx) = reduce(&stopping_with_pending(), Event::Shutdown);
        assert_eq!(s.state, State::Stopping);
        assert!(s.shutdown_pending);
        assert!(fx.is_empty(), "Shutdown alone must not emit effects");

        let (s, fx) = reduce(
            &s,
            Event::FinalizeDone {
                ok: true,
                new_session_id: "s3".into(),
            },
        );
        assert_eq!(starts(&fx), 0, "no new part may start during shutdown");
        assert!(!fx.iter().any(|e| matches!(e, Effect::OpenPart { .. })));
        assert_eq!(s.state, State::Idle);
        assert!(!s.shutdown_pending);
        assert_eq!(s.pending_start, None);
        assert_eq!(s.session_id, None);
        assert_eq!(fx.last(), Some(&Effect::EmitStatus(State::Idle)));
    }

    #[test]
    fn shutdown_in_stopping_without_pending_ends_cleanly() {
        let mut base = recording_state();
        base.current_game = None;
        let (s, _) = reduce(&base, Event::UserStop);
        assert_eq!(s.state, State::Stopping);
        assert_eq!(s.pending_start, None);

        let (s, _) = reduce(&s, Event::Shutdown);
        let (s, fx) = reduce(
            &s,
            Event::FinalizeDone {
                ok: true,
                new_session_id: "s3".into(),
            },
        );
        assert_eq!(starts(&fx), 0);
        assert_eq!(s.state, State::Idle);
        assert!(!s.shutdown_pending);
        assert!(fx.iter().any(|e| matches!(e, Effect::StopEncoder)));
    }

    #[test]
    fn stopping_without_shutdown_still_starts_pending_part() {
        let (s, fx) = reduce(
            &stopping_with_pending(),
            Event::FinalizeDone {
                ok: true,
                new_session_id: "s3".into(),
            },
        );
        assert_eq!(
            starts(&fx),
            1,
            "ordinary completion starts the pending part"
        );
        assert_eq!(s.state, State::Recording);
        assert_eq!(s.pending_start, None);
        assert!(!s.shutdown_pending);
    }
}
