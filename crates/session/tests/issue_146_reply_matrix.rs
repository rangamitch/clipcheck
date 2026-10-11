//! Issue #146: every user command yields exactly one `Effect::Reply`, in every
//! state, and `reduce_owned` matches `reduce`. Black-box: public API only.

use clipcheck_session::{
    reduce, reduce_owned, CommandError, Effect, Event, ExitReason, Mode, SessionConfig,
    SessionState, State, WriteError,
};

const MAX_MS: u64 = 60_000;
const FREE_OK: u64 = 10_000_000;
const ULID_1: &str = "01J00000000000000000000001";
const ULID_2: &str = "01J00000000000000000000002";

fn cfg() -> SessionConfig {
    SessionConfig {
        max_part_ms: MAX_MS,
        max_part_bytes: 1_000_000,
        min_free_bytes: 1_000,
        join_relaunch_after_crash: false,
        join_window_ms: 60_000,
    }
}

fn feed(start: &SessionState, evs: Vec<Event>) -> SessionState {
    let mut s = start.clone();
    for ev in evs {
        s = reduce(&s, ev).0;
    }
    s
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

fn game_start(game: &str) -> Event {
    Event::GameStarted {
        game: game.to_string(),
        now_ms: 0,
        new_session_id: ULID_1.to_string(),
    }
}

fn game_end(game: &str) -> Event {
    Event::GameEnded {
        game: game.to_string(),
        reason: ExitReason::Normal,
        now_ms: 0,
    }
}

fn user_start(id: &str) -> Event {
    Event::UserStart {
        new_session_id: id.to_string(),
    }
}

fn finalize_done(ok: bool, id: &str) -> Event {
    Event::FinalizeDone {
        ok,
        new_session_id: id.to_string(),
    }
}

// ---------- states ----------

/// Feature disabled.
fn st_idle() -> SessionState {
    SessionState::new(cfg())
}

/// Enabled in manual mode with no game detected.
fn st_armed_no_game() -> SessionState {
    let s = feed(&st_idle(), vec![enable(Mode::Manual)]);
    assert_eq!(s.state, State::Armed, "setup: Armed");
    s
}

/// Enabled in manual mode with game "A" detected, not recording.
fn st_armed_with_game() -> SessionState {
    let s = feed(&st_armed_no_game(), vec![game_start("A")]);
    assert_eq!(s.state, State::Armed, "setup: Armed with game");
    s
}

/// Auto mode, game "A" recording as part 1.
fn st_recording() -> SessionState {
    let s = feed(
        &st_idle(),
        vec![tick(0, 0, 0, FREE_OK), enable(Mode::Auto), game_start("A")],
    );
    assert_eq!(s.state, State::Recording, "setup: Recording");
    s
}

/// Recording with a user split pending its keyframe.
fn st_splitting() -> SessionState {
    let s = feed(&st_recording(), vec![Event::UserSplit]);
    assert_eq!(s.state, State::Splitting, "setup: Splitting");
    s
}

/// Recording with a user stop pending its finalize.
fn st_stopping() -> SessionState {
    let s = feed(&st_recording(), vec![Event::UserStop]);
    assert_eq!(s.state, State::Stopping, "setup: Stopping");
    s
}

/// Stopped by the user, game "A" still current.
fn st_stopped_by_user_with_game() -> SessionState {
    let s = feed(&st_stopping(), vec![finalize_done(true, ULID_2)]);
    assert_eq!(s.state, State::StoppedByUser, "setup: StoppedByUser");
    assert!(s.current_game.is_some(), "setup: game still current");
    s
}

/// Stopped by the user, no game current.
fn st_stopped_by_user_no_game() -> SessionState {
    let s = feed(
        &st_stopping(),
        vec![game_end("A"), finalize_done(true, ULID_2)],
    );
    assert_eq!(s.state, State::StoppedByUser, "setup: StoppedByUser");
    assert!(s.current_game.is_none(), "setup: no game");
    s
}

// ---------- one reply per (state, command) ----------

fn replies(fx: &[Effect]) -> Vec<Result<(), CommandError>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Reply(r) => Some(*r),
            _ => None,
        })
        .collect()
}

#[test]
fn every_user_command_in_every_state_yields_exactly_one_reply() {
    // (label, starting state, command, expected reply)
    let cases: Vec<(&str, SessionState, Event, Result<(), CommandError>)> = vec![
        // Idle
        (
            "Idle+UserStart",
            st_idle(),
            user_start(ULID_1),
            Err(CommandError::NotEnabled),
        ),
        (
            "Idle+UserStop",
            st_idle(),
            Event::UserStop,
            Err(CommandError::NotRecording),
        ),
        (
            "Idle+UserSplit",
            st_idle(),
            Event::UserSplit,
            Err(CommandError::NotRecording),
        ),
        // Armed without a game
        (
            "Armed-nogame+UserStart",
            st_armed_no_game(),
            user_start(ULID_1),
            Err(CommandError::NoTargetGame),
        ),
        (
            "Armed-nogame+UserStop",
            st_armed_no_game(),
            Event::UserStop,
            Err(CommandError::NotRecording),
        ),
        (
            "Armed-nogame+UserSplit",
            st_armed_no_game(),
            Event::UserSplit,
            Err(CommandError::NotRecording),
        ),
        // Armed with a game
        (
            "Armed+UserStart",
            st_armed_with_game(),
            user_start(ULID_1),
            Ok(()),
        ),
        (
            "Armed+UserStop",
            st_armed_with_game(),
            Event::UserStop,
            Err(CommandError::NotRecording),
        ),
        (
            "Armed+UserSplit",
            st_armed_with_game(),
            Event::UserSplit,
            Err(CommandError::NotRecording),
        ),
        // Recording
        (
            "Recording+UserStart",
            st_recording(),
            user_start(ULID_2),
            Err(CommandError::AlreadyRecording),
        ),
        (
            "Recording+UserSplit",
            st_recording(),
            Event::UserSplit,
            Ok(()),
        ),
        (
            "Recording+UserStop",
            st_recording(),
            Event::UserStop,
            Ok(()),
        ),
        // Splitting
        (
            "Splitting+UserStart",
            st_splitting(),
            user_start(ULID_2),
            Err(CommandError::AlreadyRecording),
        ),
        (
            "Splitting+UserSplit",
            st_splitting(),
            Event::UserSplit,
            Err(CommandError::AlreadyPending),
        ),
        (
            "Splitting+UserStop",
            st_splitting(),
            Event::UserStop,
            Ok(()),
        ),
        // Stopping
        (
            "Stopping+UserStart",
            st_stopping(),
            user_start(ULID_2),
            Err(CommandError::Busy),
        ),
        (
            "Stopping+UserStop",
            st_stopping(),
            Event::UserStop,
            Err(CommandError::Busy),
        ),
        (
            "Stopping+UserSplit",
            st_stopping(),
            Event::UserSplit,
            Err(CommandError::Busy),
        ),
        // StoppedByUser
        (
            "StoppedByUser+UserStart (game)",
            st_stopped_by_user_with_game(),
            user_start(ULID_2),
            Ok(()),
        ),
        (
            "StoppedByUser+UserStart (no game)",
            st_stopped_by_user_no_game(),
            user_start(ULID_2),
            Err(CommandError::NoTargetGame),
        ),
        (
            "StoppedByUser+UserStop",
            st_stopped_by_user_with_game(),
            Event::UserStop,
            Err(CommandError::NotRecording),
        ),
        (
            "StoppedByUser+UserSplit",
            st_stopped_by_user_with_game(),
            Event::UserSplit,
            Err(CommandError::NotRecording),
        ),
    ];

    for (label, start, ev, want) in cases {
        let (_, fx) = reduce(&start, ev);
        let got = replies(&fx);
        assert_eq!(
            got.len(),
            1,
            "{label}: expected exactly one Reply, fx {fx:?}"
        );
        assert_eq!(got[0], want, "{label}: reply value, fx {fx:?}");
    }
}

// ---------- reduce_owned equals reduce ----------

fn sample_events() -> Vec<Event> {
    vec![
        tick(1, 0, 0, FREE_OK),
        tick(2, MAX_MS, 0, FREE_OK),
        tick(3, 0, 0, 0),
        enable(Mode::Auto),
        enable(Mode::Manual),
        Event::SetEnabled {
            enabled: false,
            mode: Mode::Auto,
            new_session_id: String::new(),
        },
        Event::Config(cfg()),
        game_start("A"),
        game_start("B"),
        game_end("A"),
        user_start(ULID_2),
        Event::UserStop,
        Event::UserSplit,
        Event::KeyframeArrived,
        Event::KeyframeTimeout { attempts: 1 },
        Event::Shutdown,
        Event::Suspend { now_ms: 5 },
        Event::StreamParamsChanged,
        Event::LayoutChanged,
        Event::WriteFailed(WriteError::DiskFull),
        finalize_done(true, ULID_2),
        finalize_done(false, ULID_2),
    ]
}

fn sample_states() -> Vec<SessionState> {
    vec![
        st_idle(),
        st_armed_no_game(),
        st_armed_with_game(),
        st_recording(),
        st_splitting(),
        st_stopping(),
        st_stopped_by_user_with_game(),
        st_stopped_by_user_no_game(),
    ]
}

#[test]
fn reduce_owned_equals_reduce_for_sample_events() {
    for start in sample_states() {
        for ev in sample_events() {
            let borrowed = reduce(&start, ev.clone());
            let owned = reduce_owned(start.clone(), ev.clone());
            assert_eq!(borrowed, owned, "state {:?} event {:?}", start.state, ev);
        }
    }
}

#[test]
fn reduce_owned_equals_reduce_across_a_sequence_including_tick() {
    let script = vec![
        enable(Mode::Auto),
        tick(10, 0, 0, FREE_OK),
        game_start("A"),
        tick(20, MAX_MS, 0, FREE_OK),
        Event::KeyframeArrived,
        Event::UserStop,
        finalize_done(true, ULID_2),
        user_start(ULID_2),
    ];
    let mut by_ref = st_idle();
    let mut owned = st_idle();
    for ev in script {
        let (next_ref, fx_ref) = reduce(&by_ref, ev.clone());
        let (next_own, fx_own) = reduce_owned(owned, ev.clone());
        assert_eq!(next_ref, next_own, "state after {ev:?}");
        assert_eq!(fx_ref, fx_own, "effects after {ev:?}");
        by_ref = next_ref;
        owned = next_own;
    }
}
