//! Behavior tests for the clipcheck-session reducer. Black-box: public API only.

use clipcheck_session::{
    reduce, CommandError, Effect, EndReason, Event, ExitReason, Mode, Notice, SessionConfig,
    SessionState, SplitReason, State, WriteError,
};

const MIN_FREE: u64 = 1_000;
const MAX_MS: u64 = 60_000;
const MAX_BYTES: u64 = 1_000_000;
const JOIN_MS: i64 = 60_000;
const FREE_OK: u64 = 10_000_000;
const FREE_LOW: u64 = 10;

/// Caller-generated session ids (ULID strings in production).
const ULID_1: &str = "01J00000000000000000000001";
const ULID_2: &str = "01J00000000000000000000002";

fn cfg() -> SessionConfig {
    SessionConfig {
        max_part_ms: MAX_MS,
        max_part_bytes: MAX_BYTES,
        min_free_bytes: MIN_FREE,
        join_relaunch_after_crash: false,
        join_window_ms: JOIN_MS,
    }
}

fn cfg_join() -> SessionConfig {
    SessionConfig {
        join_relaunch_after_crash: true,
        ..cfg()
    }
}

fn fresh(c: SessionConfig) -> SessionState {
    SessionState::new(c)
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

fn tick(now_ms: i64, elapsed: u64, bytes: u64, free: u64) -> Event {
    Event::Tick {
        now_ms,
        part_elapsed_ms: elapsed,
        part_bytes: bytes,
        free_bytes: free,
    }
}

/// Enable the feature with `mode`; a session it starts gets `ULID_1`.
fn enable(mode: Mode) -> Event {
    Event::SetEnabled {
        enabled: true,
        mode,
        new_session_id: ULID_1.to_string(),
    }
}

/// Disable the feature. No session id is needed.
fn disable() -> Event {
    Event::SetEnabled {
        enabled: false,
        mode: Mode::Auto,
        new_session_id: String::new(),
    }
}

fn game_start(game: &str, now_ms: i64) -> Event {
    game_start_id(game, now_ms, ULID_1)
}

fn game_start_id(game: &str, now_ms: i64, id: &str) -> Event {
    Event::GameStarted {
        game: game.to_string(),
        now_ms,
        new_session_id: id.to_string(),
    }
}

fn game_end(game: &str, reason: ExitReason, now_ms: i64) -> Event {
    Event::GameEnded {
        game: game.to_string(),
        reason,
        now_ms,
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

fn start_enc(session_id: &str, part_index: u32, game: &str) -> Effect {
    Effect::StartEncoder {
        session_id: session_id.to_string(),
        part_index,
        game: game.to_string(),
    }
}

/// Auto mode, game "A" recording as ULID_1, part 1.
fn recording_auto(c: SessionConfig) -> SessionState {
    let (s, _) = feed(
        &fresh(c),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Auto),
            game_start("A", 0),
        ],
    );
    assert_eq!(s.state, State::Recording, "setup: expected Recording");
    s
}

/// Auto mode, recorded "A", user stopped, finalize ok -> StoppedByUser.
fn stopped_by_user_auto(c: SessionConfig) -> SessionState {
    let (s, _) = feed(
        &recording_auto(c),
        vec![Event::UserStop, finalize_done(true, ULID_2)],
    );
    assert_eq!(
        s.state,
        State::StoppedByUser,
        "setup: expected StoppedByUser"
    );
    s
}

// ---------- 1-4: enable and start ----------

#[test]
fn t01_idle_enable_manual_arms() {
    let (s, fx) = reduce(&fresh(cfg()), enable(Mode::Manual));
    assert_eq!(s.state, State::Armed);
    assert!(s.enabled);
    assert_eq!(fx, vec![Effect::EmitStatus(State::Armed)]);
}

#[test]
fn t02_idle_enable_auto_with_game_records_part_1() {
    let (s, _) = feed(
        &fresh(cfg()),
        vec![game_start("A", 0), tick(0, 0, 0, FREE_OK)],
    );
    let (s, fx) = feed(&s, vec![enable(Mode::Auto)]);
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 1);
    assert_eq!(
        fx,
        vec![
            start_enc(ULID_1, 1, "A"),
            Effect::EmitStatus(State::Recording)
        ]
    );
}

#[test]
fn t03a_armed_auto_game_start_records() {
    let (s, _) = feed(
        &fresh(cfg()),
        vec![tick(0, 0, 0, FREE_OK), enable(Mode::Auto)],
    );
    assert_eq!(s.state, State::Armed);
    let (s, fx) = feed(&s, vec![game_start("A", 0)]);
    assert_eq!(s.state, State::Recording, "effects: {fx:?}");
    assert!(fx.contains(&start_enc(ULID_1, 1, "A")), "effects: {fx:?}");
    assert!(
        fx.contains(&Effect::EmitStatus(State::Recording)),
        "effects: {fx:?}"
    );
}

#[test]
fn t03b_armed_auto_game_start_low_disk_stays_armed() {
    let (s, _) = feed(
        &fresh(cfg()),
        vec![tick(0, 0, 0, FREE_LOW), enable(Mode::Auto)],
    );
    let (s, fx) = feed(&s, vec![game_start("A", 0)]);
    assert_eq!(s.state, State::Armed);
    assert_eq!(fx, vec![Effect::Notify(Notice::DiskLowBlocked)]);
    assert!(!fx.iter().any(|e| matches!(e, Effect::StartEncoder { .. })));
}

#[test]
fn t04a_armed_user_start_without_game_replies_no_target() {
    let (s, _) = feed(&fresh(cfg()), vec![enable(Mode::Manual)]);
    let (s, fx) = reduce(&s, user_start(ULID_1));
    assert_eq!(s.state, State::Armed);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::NoTargetGame))]);
}

#[test]
fn t04b_armed_user_start_with_game_records() {
    let (s, _) = feed(
        &fresh(cfg()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Manual),
            game_start("A", 0),
        ],
    );
    assert_eq!(s.state, State::Armed, "manual mode must not auto-start");
    let (s, fx) = reduce(&s, user_start(ULID_1));
    assert_eq!(s.state, State::Recording);
    assert_eq!(
        fx,
        vec![
            start_enc(ULID_1, 1, "A"),
            Effect::EmitStatus(State::Recording),
            Effect::Reply(Ok(())),
        ]
    );
}

// ---------- 5-7: split ----------

#[test]
fn t05_recording_user_split_requests_keyframe_only() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::UserSplit);
    assert_eq!(s.state, State::Splitting);
    assert_eq!(s.part_index, 1, "part index moves only at the keyframe");
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
fn t06a_splitting_keyframe_arrived_finalizes_and_opens_next_part() {
    let s = recording_auto(cfg());
    let (s, _) = reduce(&s, Event::UserSplit);
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 2);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::Split(SplitReason::UserRequested)
            },
            Effect::OpenPart { index: 2 },
            Effect::EmitStatus(State::Recording),
        ]
    );
}

#[test]
fn t06b_splitting_keyframe_timeout_once_requests_keyframe() {
    let s = recording_auto(cfg());
    let (s, _) = reduce(&s, Event::UserSplit);
    let (s, fx) = reduce(&s, Event::KeyframeTimeout { attempts: 1 });
    assert_eq!(s.state, State::Splitting);
    assert_eq!(fx, vec![Effect::RequestKeyframe]);
}

#[test]
fn t06c_splitting_keyframe_timeout_three_cancels_split_without_part_change() {
    let s = recording_auto(cfg());
    let (s, _) = reduce(&s, Event::UserSplit);
    let (s, fx) = reduce(&s, Event::KeyframeTimeout { attempts: 3 });
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 1, "cancelled split must not bump part_index");
    assert_eq!(s.split_pending, None);
    assert_eq!(
        fx,
        vec![
            Effect::CancelSplit,
            Effect::Notify(Notice::KeyframeTimeout),
            Effect::EmitStatus(State::Recording),
        ]
    );
}

#[test]
fn t07_splitting_user_split_already_pending() {
    let s = recording_auto(cfg());
    let (s, _) = reduce(&s, Event::UserSplit);
    let (s2, fx) = reduce(&s, Event::UserSplit);
    assert_eq!(s2.state, State::Splitting);
    assert_eq!(fx, vec![Effect::Reply(Err(CommandError::AlreadyPending))]);
}

// ---------- 8: ticks ----------

#[test]
fn t08a_tick_max_duration_splits() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, tick(60_000, MAX_MS, 0, FREE_OK));
    assert_eq!(s.state, State::Splitting);
    assert_eq!(s.part_index, 1);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe
        ]
    );
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
    assert_eq!(s.part_index, 2);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::Split(SplitReason::MaxDuration)
            },
            Effect::OpenPart { index: 2 },
            Effect::EmitStatus(State::Recording),
        ]
    );
}

#[test]
fn t08b_tick_max_bytes_splits() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, tick(1, 0, MAX_BYTES, FREE_OK));
    assert_eq!(s.state, State::Splitting);
    assert_eq!(s.part_index, 1);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe
        ]
    );
    let (_, fx) = reduce(&s, Event::KeyframeArrived);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::Split(SplitReason::MaxSize)
        }),
        "effects: {fx:?}"
    );
}

#[test]
fn t08c_tick_low_disk_stops_recording() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, tick(1, 0, 0, FREE_LOW));
    assert_eq!(s.state, State::Stopping);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::DiskLow
            },
            Effect::EmitStatus(State::Stopping),
            Effect::Notify(Notice::DiskLow),
        ]
    );
}

// ---------- 9: part_index ----------

#[test]
fn t09_part_index_changes_only_on_keyframe_join_or_start() {
    let s = recording_auto(cfg());
    assert_eq!(s.part_index, 1, "plain start is part 1");
    let (s, _) = feed(
        &s,
        vec![
            tick(1, 1_000, 1_000, FREE_OK),
            tick(2, 2_000, 2_000, FREE_OK),
            tick(3, 3_000, 3_000, FREE_OK),
        ],
    );
    assert_eq!(s.part_index, 1, "ticks must not change part_index");
    let (s, _) = feed(&s, vec![Event::UserSplit]);
    assert_eq!(s.part_index, 1, "split request must not change part_index");
    let (s, _) = feed(&s, vec![Event::KeyframeArrived]);
    assert_eq!(s.part_index, 2, "keyframe applies the split");
    let (s, _) = feed(&s, vec![tick(4, MAX_MS, 0, FREE_OK)]);
    assert_eq!(s.state, State::Splitting);
    assert_eq!(s.part_index, 2, "pending max-duration split waits");
    let (s, _) = feed(&s, vec![Event::KeyframeArrived]);
    assert_eq!(s.part_index, 3, "max-duration split increments part_index");
}

// ---------- 10-11: user stop and StoppedByUser ----------

#[test]
fn t10_user_stop_then_finalize_ok_is_stopped_by_user() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::UserStop);
    assert_eq!(s.state, State::Stopping);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::UserStop
            },
            Effect::EmitStatus(State::Stopping),
            Effect::Reply(Ok(())),
        ]
    );
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::StoppedByUser);
    assert_eq!(
        fx,
        vec![
            Effect::StopEncoder,
            Effect::EmitStatus(State::StoppedByUser)
        ]
    );
}

#[test]
fn t11a_stopped_by_user_ignores_game_start() {
    let s = stopped_by_user_auto(cfg());
    let (s2, fx) = reduce(&s, game_start("B", 5));
    assert_eq!(s2.state, State::StoppedByUser);
    assert!(fx.is_empty(), "effects: {fx:?}");
}

#[test]
fn t11b_stopped_by_user_user_start_records_tracked_game() {
    let s = stopped_by_user_auto(cfg());
    assert_eq!(s.current_game.as_deref(), Some("A"));
    let (s, fx) = reduce(&s, user_start(ULID_2));
    assert_eq!(s.state, State::Recording);
    assert!(fx.contains(&start_enc(ULID_2, 1, "A")), "effects: {fx:?}");
}

#[test]
fn t12_stopped_by_user_game_end_arms_auto_then_game_start_records() {
    let s = stopped_by_user_auto(cfg());
    let (s, fx) = reduce(&s, game_end("A", ExitReason::Normal, 10));
    assert_eq!(s.state, State::Armed);
    assert_eq!(fx, vec![Effect::EmitStatus(State::Armed)]);
    let (s, fx) = reduce(&s, game_start_id("A", 20, ULID_2));
    assert_eq!(s.state, State::Recording);
    assert!(fx.contains(&start_enc(ULID_2, 1, "A")), "effects: {fx:?}");
}

// ---------- 13: game exit while recording ----------

#[test]
fn t13a_recording_game_exit_finalizes_then_arms() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, game_end("A", ExitReason::Normal, 10));
    assert_eq!(s.state, State::Stopping);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::GameExited
            },
            Effect::EmitStatus(State::Stopping),
        ]
    );
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::Armed);
    assert!(s.enabled);
    assert_eq!(
        fx,
        vec![Effect::StopEncoder, Effect::EmitStatus(State::Armed)]
    );
}

#[test]
fn t13b_recording_other_game_end_is_noop() {
    let s = recording_auto(cfg());
    let (s2, fx) = reduce(&s, game_end("B", ExitReason::Normal, 10));
    assert_eq!(s2.state, State::Recording);
    assert_eq!(s2.current_game.as_deref(), Some("A"));
    assert!(fx.is_empty(), "effects: {fx:?}");
}

// ---------- 14-15: crash join ----------

#[test]
fn t14a_crash_then_relaunch_within_window_joins_session() {
    let (s, _) = feed(
        &fresh(cfg_join()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Auto),
            game_start("A", 1_000),
        ],
    );
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 1);

    let (s, fx) = feed(
        &s,
        vec![
            game_end("A", ExitReason::Crashed { code: 1 }, 2_000),
            finalize_done(true, ULID_2),
        ],
    );
    assert_eq!(s.state, State::Armed, "effects: {fx:?}");
    assert_eq!(
        s.resume.as_ref().map(|r| r.deadline_ms),
        Some(2_000 + JOIN_MS),
        "resume deadline"
    );

    // The relaunch passes ULID_1 again, but a join must keep the old session id.
    let (s, fx) = reduce(&s, game_start("A", 10_000));
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 2);
    assert_eq!(
        fx,
        vec![
            start_enc(ULID_1, 2, "A"),
            Effect::EmitStatus(State::Recording)
        ]
    );
}

#[test]
fn t14b_crash_relaunch_after_window_starts_new_session() {
    let (s, _) = feed(
        &fresh(cfg_join()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Auto),
            game_start("A", 1_000),
            game_end("A", ExitReason::Crashed { code: 1 }, 2_000),
            finalize_done(true, ULID_2),
        ],
    );
    assert_eq!(s.state, State::Armed);
    let (s, fx) = reduce(&s, game_start_id("A", 2_000 + JOIN_MS + 1, ULID_2));
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 1);
    assert_eq!(
        fx,
        vec![
            start_enc(ULID_2, 1, "A"),
            Effect::EmitStatus(State::Recording)
        ]
    );
}

#[test]
fn t15_crash_without_join_flag_starts_new_session() {
    let (s, _) = feed(
        &fresh(cfg()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Auto),
            game_start("A", 1_000),
            game_end("A", ExitReason::Crashed { code: 1 }, 2_000),
            finalize_done(true, ULID_2),
        ],
    );
    assert_eq!(s.state, State::Armed);
    assert!(
        s.resume.is_none(),
        "no resume should be recorded without the flag"
    );
    let (s, fx) = reduce(&s, game_start_id("A", 3_000, ULID_2));
    assert_eq!(s.state, State::Recording);
    assert_eq!(s.part_index, 1);
    assert!(fx.contains(&start_enc(ULID_2, 1, "A")), "effects: {fx:?}");
}

// ---------- 16: game switch ----------

#[test]
fn t16_game_switch_finalizes_then_starts_new_session_for_new_game() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, game_start("B", 5));
    assert_eq!(s.state, State::Stopping);
    assert_eq!(s.current_game.as_deref(), Some("B"));
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::GameSwitched
            },
            Effect::EmitStatus(State::Stopping),
        ]
    );
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::Recording);
    assert_eq!(
        fx,
        vec![
            Effect::StopEncoder,
            start_enc(ULID_2, 1, "B"),
            Effect::EmitStatus(State::Recording),
        ]
    );
}

// ---------- 17: write failures ----------

#[test]
fn t17a_write_failed_disk_full() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::WriteFailed(WriteError::DiskFull));
    assert_eq!(s.state, State::Stopping);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::DiskFull
            },
            Effect::EmitStatus(State::Stopping),
            Effect::Notify(Notice::WriteFailed(WriteError::DiskFull)),
        ]
    );
}

#[test]
fn t17b_write_failed_volume_gone() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::WriteFailed(WriteError::VolumeGone));
    assert_eq!(s.state, State::Stopping);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::VolumeGone
        }),
        "effects: {fx:?}"
    );
    assert!(fx.contains(&Effect::Notify(Notice::WriteFailed(WriteError::VolumeGone))));
}

#[test]
fn t17c_write_failed_io_is_writer_error() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::WriteFailed(WriteError::Io));
    assert_eq!(s.state, State::Stopping);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::WriterError
        }),
        "effects: {fx:?}"
    );
    assert!(fx.contains(&Effect::Notify(Notice::WriteFailed(WriteError::Io))));
}

// ---------- 18-19: disable and shutdown ----------

#[test]
fn t18_disable_while_recording_goes_idle_after_finalize() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, disable());
    assert_eq!(s.state, State::Stopping);
    assert!(!s.enabled);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::UserStop
            },
            Effect::EmitStatus(State::Stopping),
        ]
    );
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::Idle);
    assert!(!s.enabled);
    assert_eq!(
        fx,
        vec![Effect::StopEncoder, Effect::EmitStatus(State::Idle)]
    );
}

#[test]
fn t19_shutdown_while_recording_goes_idle_after_finalize() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::Shutdown);
    assert_eq!(s.state, State::Stopping);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::ServiceShutdown
            },
            Effect::EmitStatus(State::Stopping),
        ]
    );
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::Idle);
    assert_eq!(
        fx,
        vec![Effect::StopEncoder, Effect::EmitStatus(State::Idle)]
    );
}

// ---------- 20-22: Stopping rules, failed finalize, suspend ----------

#[test]
fn t20_stopping_rejects_commands_with_busy() {
    let base = recording_auto(cfg());
    let (stopping, _) = reduce(&base, Event::UserStop);
    assert_eq!(stopping.state, State::Stopping);
    for ev in [user_start(ULID_2), Event::UserStop, Event::UserSplit] {
        let label = format!("{ev:?}");
        let (s, fx) = reduce(&stopping, ev);
        assert_eq!(s.state, State::Stopping, "state after {label}");
        assert_eq!(
            fx,
            vec![Effect::Reply(Err(CommandError::Busy))],
            "effects after {label}"
        );
    }
}

#[test]
fn t21_finalize_failed_emits_partial_kept_then_next_state() {
    let s = recording_auto(cfg());
    let (s, _) = reduce(&s, Event::UserStop);
    let (s, fx) = reduce(&s, finalize_done(false, ULID_2));
    assert_eq!(s.state, State::StoppedByUser);
    assert_eq!(
        fx,
        vec![
            Effect::Notify(Notice::PartialKept),
            Effect::StopEncoder,
            Effect::EmitStatus(State::StoppedByUser),
        ]
    );
}

#[test]
fn t22_suspend_while_recording_finalizes() {
    let s = recording_auto(cfg());
    let (s, fx) = reduce(&s, Event::Suspend { now_ms: 5 });
    assert_eq!(s.state, State::Stopping);
    assert_eq!(
        fx,
        vec![
            Effect::FinalizePart {
                reason: EndReason::Suspend
            },
            Effect::EmitStatus(State::Stopping),
        ]
    );
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::Armed);
    assert_eq!(
        fx,
        vec![Effect::StopEncoder, Effect::EmitStatus(State::Armed)]
    );
}

// ---------- 23: unlisted pairs are no-ops ----------

#[test]
fn t23_unlisted_pairs_are_noops() {
    let idle = fresh(cfg());
    let (s, fx) = reduce(&idle, Event::UserStop);
    assert_eq!(s, idle);
    assert_eq!(
        fx,
        vec![Effect::Reply(Err(CommandError::NotRecording))],
        "Idle + UserStop replies NotRecording (#146): {fx:?}"
    );

    let (s, fx) = reduce(&idle, Event::KeyframeArrived);
    assert_eq!(s, idle);
    assert!(fx.is_empty(), "Idle + KeyframeArrived: {fx:?}");

    let (armed, _) = feed(&idle, vec![enable(Mode::Manual)]);
    assert_eq!(armed.state, State::Armed);
    let (s, fx) = reduce(&armed, Event::WriteFailed(WriteError::DiskFull));
    assert_eq!(s, armed);
    assert!(fx.is_empty(), "Armed + WriteFailed: {fx:?}");

    let sbu = stopped_by_user_auto(cfg());
    let (s, fx) = reduce(&sbu, tick(99, 0, 0, 1 << 40));
    assert_eq!(s.state, State::StoppedByUser);
    assert_eq!(s.free_bytes, Some(1 << 40));
    assert_eq!(s.now_ms, 99);
    assert!(fx.is_empty(), "StoppedByUser + Tick: {fx:?}");
}

// ---------- 25: split invariant ----------

#[test]
fn t25_every_split_requests_keyframe_then_finalize_and_open_on_keyframe() {
    let cases: Vec<(&str, Event, SplitReason)> = vec![
        ("UserSplit", Event::UserSplit, SplitReason::UserRequested),
        (
            "Tick max duration",
            tick(1, MAX_MS, 0, FREE_OK),
            SplitReason::MaxDuration,
        ),
        (
            "Tick max size",
            tick(1, 0, MAX_BYTES, FREE_OK),
            SplitReason::MaxSize,
        ),
        (
            "StreamParamsChanged",
            Event::StreamParamsChanged,
            SplitReason::StreamParamsChanged,
        ),
        (
            "LayoutChanged",
            Event::LayoutChanged,
            SplitReason::ConfigChanged,
        ),
    ];
    for (label, ev, reason) in cases {
        let base = recording_auto(cfg());
        let prev = base.part_index;
        // A user split also replies Ok(()) after the request (#146).
        let is_user = matches!(ev, Event::UserSplit);
        let (s, fx) = reduce(&base, ev);
        assert_eq!(s.state, State::Splitting, "{label}: state");
        let mut want = vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe,
        ];
        if is_user {
            want.push(Effect::Reply(Ok(())));
        }
        assert_eq!(fx, want, "{label}: request");
        assert_eq!(
            s.part_index, prev,
            "{label}: no index change before keyframe"
        );

        let (s, fx) = reduce(&s, Event::KeyframeArrived);
        assert_eq!(fx.len(), 3, "{label}: effects {fx:?}");
        assert_eq!(
            fx[0],
            Effect::FinalizePart {
                reason: EndReason::Split(reason)
            },
            "{label}: first effect must finalize with the split reason"
        );
        match fx[1] {
            Effect::OpenPart { index } => {
                assert_eq!(index, prev + 1, "{label}: OpenPart index");
                assert_eq!(s.part_index, index, "{label}: part_index");
            }
            ref other => panic!("{label}: second effect must be OpenPart, got {other:?}"),
        }
    }
}

// ---------- Suspected code bugs (expected to FAIL if the code is as read) ----------

/// Manual + enabled: after a finished stop and the game exits, the feature is
/// still enabled, so the machine should be Armed (as T34/T37/T38 do in finish()).
/// The T41 path in stopped_by_user() sends it to Idle instead.
#[test]
fn stopped_by_user_manual_game_end_goes_idle_not_armed() {
    let (s, _) = feed(
        &fresh(cfg()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Manual),
            game_start("A", 0),
            user_start(ULID_1),
            Event::UserStop,
            finalize_done(true, ULID_2),
        ],
    );
    assert_eq!(s.state, State::StoppedByUser);
    let (s, fx) = reduce(&s, game_end("A", ExitReason::Normal, 10));
    assert!(s.enabled);
    assert_eq!(s.state, State::Armed, "effects: {fx:?}");
}

/// User stops, then the game is re-detected in Stopping (Auto). finish() treats
/// the pending start as a new auto-start, so recording resumes even though the
/// user stopped. Invariant 11 says detection is suppressed until a user start.
#[test]
fn game_start_during_user_stop_finalize_autostarts() {
    let s = recording_auto(cfg());
    let (s, _) = reduce(&s, Event::UserStop);
    assert_eq!(s.state, State::Stopping);
    let (s, _) = reduce(&s, game_start("A", 5));
    let (s, fx) = reduce(&s, finalize_done(true, ULID_2));
    assert_eq!(s.state, State::StoppedByUser, "effects: {fx:?}");
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::StartEncoder { .. })),
        "effects: {fx:?}"
    );
}
