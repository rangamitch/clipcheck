//! Issue #21: `Event::Shutdown` while Stopping must behave like it does in
//! Recording and Splitting. A pending start is dropped, no part opens, and the
//! machine ends Idle. Black-box: public API only.

use clipcheck_session::{
    reduce, CommandError, Effect, EndReason, Event, Mode, Notice, SessionConfig, SessionState,
    State,
};

const ULID_1: &str = "01J00000000000000000000001";
const ULID_2: &str = "01J00000000000000000000002";
const ULID_3: &str = "01J00000000000000000000003";

fn cfg() -> SessionConfig {
    SessionConfig {
        max_part_ms: 60_000,
        max_part_bytes: 1_000_000,
        min_free_bytes: 1_000,
        join_relaunch_after_crash: false,
        join_window_ms: 60_000,
    }
}

/// Feed events in order, returning the final state and all effects in order.
fn feed(start: &SessionState, evs: Vec<Event>) -> (SessionState, Vec<Effect>) {
    let mut s = start.clone();
    let mut all = Vec::new();
    for ev in evs {
        let (next, fx) = reduce(&s, ev);
        all.extend(fx);
        s = next;
    }
    (s, all)
}

fn enable_auto() -> Event {
    Event::SetEnabled {
        enabled: true,
        mode: Mode::Auto,
        new_session_id: ULID_1.into(),
    }
}

fn game(name: &str, id: &str) -> Event {
    Event::GameStarted {
        game: name.into(),
        now_ms: 0,
        new_session_id: id.into(),
    }
}

fn finalize_done(ok: bool, id: &str) -> Event {
    Event::FinalizeDone {
        ok,
        new_session_id: id.into(),
    }
}

fn starts(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| matches!(e, Effect::StartEncoder { .. }))
        .count()
}

fn opens(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| matches!(e, Effect::OpenPart { .. }))
        .count()
}

/// Auto mode, enabled, recording game "a" (session ULID_1).
fn recording() -> SessionState {
    let (s, fx) = feed(
        &SessionState::new(cfg()),
        vec![enable_auto(), game("a", ULID_1)],
    );
    assert_eq!(s.state, State::Recording);
    assert_eq!(starts(&fx), 1);
    s
}

/// Recording "a", then a switch to "b": Stopping with pending_start "b".
fn stopping_with_pending() -> SessionState {
    let (s, fx) = feed(&recording(), vec![game("b", ULID_2)]);
    assert_eq!(s.state, State::Stopping);
    assert_eq!(s.pending_start.as_deref(), Some("b"));
    assert_eq!(starts(&fx), 0);
    s
}

/// Recording "a", then a user stop: Stopping with no pending start.
fn stopping_user_stop() -> SessionState {
    let (s, _) = feed(&recording(), vec![Event::UserStop]);
    assert_eq!(s.state, State::Stopping);
    assert_eq!(s.pending_start, None);
    s
}

// (1) Recording -> game switch -> Stopping -> Shutdown -> FinalizeDone ok.
#[test]
fn switch_then_shutdown_then_ok_finalize_starts_nothing_and_ends_idle() {
    let (s, fx) = feed(
        &stopping_with_pending(),
        vec![Event::Shutdown, finalize_done(true, ULID_3)],
    );
    assert_eq!(
        starts(&fx),
        0,
        "pending part must not start during shutdown"
    );
    assert_eq!(opens(&fx), 0, "no OpenPart after shutdown");
    assert_eq!(s.state, State::Idle);
    assert_eq!(s.pending_start, None);
    assert!(!s.shutdown_pending, "shutdown flag is consumed by finish");
    assert_eq!(s.session_id, None);
    assert!(fx.contains(&Effect::StopEncoder));
    assert_eq!(fx.last(), Some(&Effect::EmitStatus(State::Idle)));
}

// (2) Same as (1) but the part closed with an error.
#[test]
fn switch_then_shutdown_then_failed_finalize_starts_nothing_and_ends_idle() {
    let (s, fx) = feed(
        &stopping_with_pending(),
        vec![Event::Shutdown, finalize_done(false, ULID_3)],
    );
    assert_eq!(starts(&fx), 0);
    assert_eq!(opens(&fx), 0);
    assert_eq!(s.state, State::Idle);
    assert_eq!(s.pending_start, None);
    assert!(!s.shutdown_pending);
    assert!(fx.contains(&Effect::Notify(Notice::PartialKept)));
    assert!(fx.contains(&Effect::StopEncoder));
}

// (3) Stopping with nothing pending + Shutdown: clean end.
#[test]
fn stopping_without_pending_plus_shutdown_ends_cleanly() {
    let (s, fx0) = feed(&stopping_user_stop(), vec![Event::Shutdown]);
    assert!(fx0.is_empty(), "Shutdown alone emits no effects");
    assert_eq!(s.state, State::Stopping);
    assert!(s.shutdown_pending);

    let (s, fx) = feed(&s, vec![finalize_done(true, ULID_2)]);
    assert_eq!(starts(&fx), 0);
    assert_eq!(s.state, State::Idle);
    assert!(!s.shutdown_pending);
    assert!(fx.contains(&Effect::StopEncoder));
}

// (4) Regression guard: without Shutdown the pending part still starts.
#[test]
fn switch_without_shutdown_still_starts_pending_part() {
    let (s, fx) = feed(&stopping_with_pending(), vec![finalize_done(true, ULID_3)]);
    assert_eq!(starts(&fx), 1);
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.pending_start, None);
    assert_eq!(s.current_game.as_deref(), Some("b"));
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::StartEncoder { game, .. } if game == "b"
    )));
}

// (5) Shutdown twice, then more events: nothing starts.
#[test]
fn shutdown_twice_is_idempotent_and_still_ends_idle() {
    let (s, fx1) = feed(&stopping_with_pending(), vec![Event::Shutdown]);
    assert!(fx1.is_empty());
    let (s, fx2) = feed(&s, vec![Event::Shutdown]);
    assert!(fx2.is_empty(), "second Shutdown emits no effects");
    assert_eq!(s.state, State::Stopping);
    assert!(s.shutdown_pending);

    let (s, fx) = feed(&s, vec![finalize_done(true, ULID_3)]);
    assert_eq!(starts(&fx), 0);
    assert_eq!(s.state, State::Idle);
}

#[test]
fn user_start_after_shutdown_in_stopping_is_refused_and_starts_nothing() {
    let (s, fx) = feed(
        &stopping_with_pending(),
        vec![
            Event::Shutdown,
            Event::UserStart {
                new_session_id: ULID_3.into(),
            },
        ],
    );
    assert_eq!(starts(&fx), 0);
    assert!(fx.contains(&Effect::Reply(Err(CommandError::Busy))));
    assert_eq!(s.state, State::Stopping);

    let (s, fx) = feed(&s, vec![finalize_done(true, ULID_3)]);
    assert_eq!(starts(&fx), 0);
    assert_eq!(s.state, State::Idle);
}

#[test]
fn game_started_after_shutdown_in_stopping_starts_nothing() {
    let (s, fx) = feed(
        &stopping_with_pending(),
        vec![Event::Shutdown, game("c", ULID_3)],
    );
    assert_eq!(starts(&fx), 0);
    assert_eq!(s.state, State::Stopping);

    let (s, fx) = feed(&s, vec![finalize_done(true, ULID_3)]);
    assert_eq!(starts(&fx), 0, "a game seen during shutdown must not start");
    assert_eq!(opens(&fx), 0);
    assert_eq!(s.state, State::Idle);
}

#[test]
fn events_after_idle_from_shutdown_do_not_start_anything() {
    let (s, _) = feed(
        &stopping_with_pending(),
        vec![Event::Shutdown, finalize_done(true, ULID_3)],
    );
    assert_eq!(s.state, State::Idle);

    let (s, fx) = feed(
        &s,
        vec![
            Event::UserStart {
                new_session_id: ULID_3.into(),
            },
            game("c", ULID_3),
            Event::UserStart {
                new_session_id: ULID_3.into(),
            },
        ],
    );
    assert_eq!(starts(&fx), 0, "no start from Idle after shutdown");
    assert_eq!(s.state, State::Idle);
}

// (6) Splitting + Shutdown and Recording + Shutdown keep their T24/T32 behaviour.
#[test]
fn recording_plus_shutdown_finalizes_with_service_shutdown_then_idles() {
    let (s, fx) = feed(&recording(), vec![Event::Shutdown]);
    assert_eq!(s.state, State::Stopping);
    assert!(s.shutdown_pending);
    assert!(fx.contains(&Effect::FinalizePart {
        reason: EndReason::ServiceShutdown
    }));
    assert!(fx.contains(&Effect::EmitStatus(State::Stopping)));

    let (s, fx) = feed(&s, vec![finalize_done(true, ULID_2)]);
    assert_eq!(starts(&fx), 0);
    assert_eq!(s.state, State::Idle);
    assert!(!s.shutdown_pending);
}

#[test]
fn splitting_plus_shutdown_finalizes_with_service_shutdown_then_idles() {
    let (s, _) = feed(&recording(), vec![Event::UserSplit]);
    assert_eq!(s.state, State::Splitting);

    let (s, fx) = feed(&s, vec![Event::Shutdown]);
    assert_eq!(s.state, State::Stopping);
    assert!(s.shutdown_pending);
    assert!(s.split_pending.is_none());
    assert!(fx.contains(&Effect::FinalizePart {
        reason: EndReason::ServiceShutdown
    }));
    assert_eq!(opens(&fx), 0);

    let (s, fx) = feed(&s, vec![finalize_done(true, ULID_2)]);
    assert_eq!(starts(&fx), 0);
    assert_eq!(opens(&fx), 0);
    assert_eq!(s.state, State::Idle);
    assert!(!s.shutdown_pending);
}

// (7) Table over all states x Shutdown: no panic, no start effect.
#[test]
fn every_state_handles_shutdown_without_starting_a_part() {
    let idle = SessionState::new(cfg());
    let (armed, _) = feed(
        &idle,
        vec![Event::SetEnabled {
            enabled: true,
            mode: Mode::Manual,
            new_session_id: ULID_1.into(),
        }],
    );
    let (splitting, _) = feed(&recording(), vec![Event::UserSplit]);
    let stopping_plain = stopping_user_stop();
    let stopping_pend = stopping_with_pending();
    let (stopped_by_user, _) = feed(&stopping_user_stop(), vec![finalize_done(true, ULID_2)]);

    let table = [
        ("Idle", idle),
        ("Armed", armed),
        ("Recording", recording()),
        ("Splitting", splitting),
        ("Stopping(plain)", stopping_plain),
        ("Stopping(pending)", stopping_pend),
        ("StoppedByUser", stopped_by_user),
    ];

    for (name, st) in table {
        let (after, fx) = reduce(&st, Event::Shutdown);
        assert_eq!(starts(&fx), 0, "{name}: Shutdown emitted StartEncoder");
        assert_eq!(opens(&fx), 0, "{name}: Shutdown emitted OpenPart");

        if after.state == State::Stopping {
            let (end, fx) = reduce(&after, finalize_done(true, ULID_3));
            assert_eq!(
                starts(&fx),
                0,
                "{name}: finish after shutdown started a part"
            );
            assert_eq!(opens(&fx), 0, "{name}: finish after shutdown opened a part");
            assert_eq!(end.state, State::Idle, "{name}: did not end Idle");
        }
    }
}
