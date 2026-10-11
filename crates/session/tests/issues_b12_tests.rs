//! Regression tests for session issues #23, #41 and #146. Black-box: public API only.

use clipcheck_session::{
    reduce, reduce_in_place, CommandError, Effect, Event, ExitReason, Mode, SessionConfig,
    SessionState, State,
};

const ULID_1: &str = "01J00000000000000000000001";
const ULID_2: &str = "01J00000000000000000000002";
const MAX_MS: u64 = 60_000;
const FREE_OK: u64 = 10_000_000;

fn cfg() -> SessionConfig {
    SessionConfig {
        max_part_ms: MAX_MS,
        max_part_bytes: 1_000_000,
        min_free_bytes: 1_000,
        join_relaunch_after_crash: false,
        join_window_ms: 60_000,
    }
}

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

fn tick(now_ms: i64, elapsed: u64, bytes: u64, free: u64) -> Event {
    Event::Tick {
        now_ms,
        part_elapsed_ms: elapsed,
        part_bytes: bytes,
        free_bytes: free,
    }
}

fn enable(mode: Mode) -> Event {
    Event::SetEnabled {
        enabled: true,
        mode,
        new_session_id: ULID_1.to_string(),
    }
}

fn game_start(game: &str, now_ms: i64) -> Event {
    Event::GameStarted {
        game: game.to_string(),
        now_ms,
        new_session_id: ULID_1.to_string(),
    }
}

fn user_start(id: &str) -> Event {
    Event::UserStart {
        new_session_id: id.to_string(),
    }
}

/// Auto mode, game "A" recording as ULID_1, part 1.
fn recording() -> SessionState {
    let (s, _) = feed(
        &SessionState::new(cfg()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Auto),
            game_start("A", 0),
        ],
    );
    assert_eq!(s.state, State::Recording, "setup: expected Recording");
    s
}

/// Manual mode, enabled, no game: Armed.
fn armed_manual() -> SessionState {
    let (s, _) = feed(&SessionState::new(cfg()), vec![enable(Mode::Manual)]);
    assert_eq!(s.state, State::Armed, "setup: expected Armed");
    s
}

fn replies(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| matches!(e, Effect::Reply(_)))
        .collect()
}

// ---------- #23: split announces Splitting ----------

#[test]
fn issue_23_user_split_emits_splitting_status_before_keyframe_request() {
    let (s, fx) = reduce(&recording(), Event::UserSplit);
    assert_eq!(s.state, State::Splitting);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe,
            Effect::Reply(Ok(())),
        ]
    );
}

#[test]
fn issue_23_tick_max_duration_split_emits_splitting_status() {
    let (s, fx) = reduce(&recording(), tick(1, MAX_MS, 0, FREE_OK));
    assert_eq!(s.state, State::Splitting);
    assert!(
        fx.contains(&Effect::EmitStatus(State::Splitting)),
        "fx: {fx:?}"
    );
    assert!(fx.contains(&Effect::RequestKeyframe), "fx: {fx:?}");
}

#[test]
fn issue_23_keyframe_apply_does_not_reannounce_splitting() {
    let (s, _) = feed(&recording(), vec![Event::UserSplit]);
    let (_, fx) = reduce(&s, Event::KeyframeArrived);
    let count = fx
        .iter()
        .filter(|e| **e == Effect::EmitStatus(State::Splitting))
        .count();
    assert_eq!(count, 0, "keyframe apply must not re-announce: {fx:?}");
}

// ---------- #146: explicit replies for user commands ----------

#[test]
fn issue_146_idle_user_start_replies_not_enabled() {
    let (s, fx) = reduce(&SessionState::new(cfg()), user_start(ULID_1));
    assert_eq!(s.state, State::Idle);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::NotEnabled))]);
}

#[test]
fn issue_146_idle_user_split_replies_not_recording() {
    let (s, fx) = reduce(&SessionState::new(cfg()), Event::UserSplit);
    assert_eq!(s.state, State::Idle);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::NotRecording))]);
}

#[test]
fn issue_146_armed_user_stop_replies_not_recording() {
    let (s, fx) = reduce(&armed_manual(), Event::UserStop);
    assert_eq!(s.state, State::Armed);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::NotRecording))]);
}

#[test]
fn issue_146_armed_user_split_replies_not_recording() {
    let (s, fx) = reduce(&armed_manual(), Event::UserSplit);
    assert_eq!(s.state, State::Armed);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::NotRecording))]);
}

#[test]
fn issue_146_recording_user_start_replies_already_recording() {
    let s = recording();
    let (s2, fx) = reduce(&s, user_start(ULID_2));
    assert_eq!(s2.state, State::Recording);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::AlreadyRecording))]);
}

#[test]
fn issue_146_splitting_user_start_replies_already_recording() {
    let (s, _) = feed(&recording(), vec![Event::UserSplit]);
    let (s2, fx) = reduce(&s, user_start(ULID_2));
    assert_eq!(s2.state, State::Splitting);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::AlreadyRecording))]);
}

#[test]
fn issue_146_idle_user_stop_replies_not_recording() {
    // Every user command gets exactly one reply, even in Idle (#146).
    let (s, fx) = reduce(&SessionState::new(cfg()), Event::UserStop);
    assert_eq!(s.state, State::Idle);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::NotRecording))]);
    assert_eq!(replies(&fx).len(), 1, "fx: {fx:?}");
}

#[test]
fn issue_146_each_listed_command_gets_exactly_one_reply() {
    let cases: Vec<(&str, SessionState, Event)> = vec![
        (
            "Idle+UserStart",
            SessionState::new(cfg()),
            user_start(ULID_1),
        ),
        ("Idle+UserSplit", SessionState::new(cfg()), Event::UserSplit),
        ("Armed+UserStop", armed_manual(), Event::UserStop),
        ("Armed+UserSplit", armed_manual(), Event::UserSplit),
        ("Recording+UserStart", recording(), user_start(ULID_2)),
    ];
    for (label, start, ev) in cases {
        let (_, fx) = reduce(&start, ev);
        assert_eq!(replies(&fx).len(), 1, "{label}: fx {fx:?}");
    }
}

#[test]
fn issue_146_refused_start_in_recording_does_not_change_state_or_open_part() {
    let s = recording();
    let (s2, fx) = reduce(&s, user_start(ULID_2));
    assert_eq!(s2, s, "a refused start must not change the state");
    assert!(
        !fx.iter().any(|e| matches!(
            e,
            Effect::StartEncoder { .. } | Effect::OpenPart { .. } | Effect::FinalizePart { .. }
        )),
        "fx: {fx:?}"
    );
}

#[test]
fn issue_146_game_exit_while_recording_has_no_reply() {
    // Sanity: a normal game exit from Recording still finalizes, with no reply.
    let s = recording();
    let (_, fx) = reduce(
        &s,
        Event::GameEnded {
            game: "A".to_string(),
            reason: ExitReason::Normal,
            now_ms: 10,
        },
    );
    assert!(replies(&fx).is_empty(), "fx: {fx:?}");
}

// ---------- #41: reduce_in_place matches reduce ----------

#[test]
fn issue_41_reduce_in_place_matches_reduce_over_a_sequence() {
    let events = vec![
        tick(0, 0, 0, FREE_OK),
        enable(Mode::Auto),
        game_start("A", 0),
        tick(1_000, 1_000, 1_000, FREE_OK),
        Event::UserSplit,
        Event::KeyframeArrived,
        tick(2_000, MAX_MS, 0, FREE_OK),
        Event::KeyframeArrived,
        Event::UserStop,
    ];

    let mut by_value = SessionState::new(cfg());
    let mut in_place = SessionState::new(cfg());
    for ev in events {
        let (next, fx_a) = reduce(&by_value, ev.clone());
        let fx_b = reduce_in_place(&mut in_place, ev.clone());
        assert_eq!(in_place, next, "state after {ev:?}");
        assert_eq!(fx_b, fx_a, "effects after {ev:?}");
        by_value = next;
    }
}

#[test]
fn issue_41_reduce_in_place_mutates_the_given_state() {
    let mut s = SessionState::new(cfg());
    let before = s.clone();
    let fx = reduce_in_place(&mut s, enable(Mode::Manual));
    assert_ne!(s, before, "state must change in place");
    assert_eq!(s.state, State::Armed);
    assert_eq!(fx, vec![Effect::EmitStatus(State::Armed)]);
}

#[test]
fn issue_41_reduce_leaves_its_input_untouched() {
    let s = recording();
    let snapshot = s.clone();
    let (_, _) = reduce(&s, Event::UserSplit);
    assert_eq!(s, snapshot, "reduce must not mutate its input");
}
