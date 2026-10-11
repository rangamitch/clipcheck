//! Split / keyframe / session-id behavior of the reducer. Black-box: public API only.

use clipcheck_session::{
    reduce, Effect, EndReason, Event, ExitReason, Mode, Notice, SessionConfig, SessionState,
    SplitReason, State, WriteError,
};
const MAX_MS: u64 = 60_000;
const MAX_BYTES: u64 = 1_000_000;
const FREE_OK: u64 = 10_000_000;

const ULID_1: &str = "01J00000000000000000000001";
const ULID_2: &str = "01J00000000000000000000002";
const ULID_3: &str = "01J00000000000000000000003";

fn cfg() -> SessionConfig {
    SessionConfig {
        max_part_ms: MAX_MS,
        max_part_bytes: MAX_BYTES,
        min_free_bytes: 1_000,
        join_relaunch_after_crash: false,
        join_window_ms: 60_000,
    }
}

fn cfg_join() -> SessionConfig {
    SessionConfig {
        join_relaunch_after_crash: true,
        ..cfg()
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

fn enable(mode: Mode, id: &str) -> Event {
    Event::SetEnabled {
        enabled: true,
        mode,
        new_session_id: id.to_string(),
    }
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

fn start_enc(session_id: &str, part_index: u32, game: &str) -> Effect {
    Effect::StartEncoder {
        session_id: session_id.to_string(),
        part_index,
        game: game.to_string(),
    }
}

fn has_start_encoder(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::StartEncoder { .. }))
}

fn has_open_part(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::OpenPart { .. }))
}

fn has_finalize(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::FinalizePart { .. }))
}

/// Auto mode, game "A" recording as ULID_1, part 1, free disk OK.
fn recording(c: SessionConfig) -> SessionState {
    let (s, _) = feed(
        &SessionState::new(c),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Auto, ULID_1),
            game_start_id("A", 0, ULID_1),
        ],
    );
    assert_eq!(s.state, State::Recording, "setup: expected Recording");
    assert_eq!(s.part_index, 1, "setup: expected part 1");
    s
}

/// Recording part 1 with a split requested and its keyframe pending.
fn splitting(c: SessionConfig) -> SessionState {
    let (s, _) = feed(&recording(c), vec![Event::UserSplit]);
    assert_eq!(s.state, State::Splitting, "setup: expected Splitting");
    s
}

// ---------- 1-2: split request and keyframe apply ----------

#[test]
fn t01_user_split_requests_keyframe_only_and_keeps_part_index() {
    let (s, fx) = reduce(&recording(cfg()), Event::UserSplit);
    assert_eq!(s.state, State::Splitting);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe,
            Effect::Reply(Ok(())),
        ]
    );
    assert_eq!(s.part_index, 1);
}

#[test]
fn t02_keyframe_arrived_applies_split_with_exact_effects() {
    let (s, _) = feed(&splitting(cfg()), vec![]);
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
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
    assert_eq!(s.part_index, 2);
    assert_eq!(s.state, State::Recording);
}

// ---------- 3: successive splits ----------

#[test]
fn t03_two_successive_splits_bump_index_1_to_2_to_3() {
    let s = recording(cfg());

    let (s, fx1) = feed(&s, vec![Event::UserSplit]);
    assert_eq!(
        fx1,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe,
            Effect::Reply(Ok(())),
        ]
    );
    assert_eq!(s.part_index, 1);

    let (s, fx2) = feed(&s, vec![Event::KeyframeArrived]);
    assert_eq!(s.part_index, 2);
    assert!(fx2.contains(&Effect::OpenPart { index: 2 }), "fx: {fx2:?}");
    assert!(!has_start_encoder(&fx2));

    let (s, fx3) = feed(&s, vec![Event::UserSplit]);
    assert_eq!(
        fx3,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe,
            Effect::Reply(Ok(())),
        ]
    );
    assert_eq!(s.part_index, 2);
    assert!(!has_open_part(&fx3) && !has_finalize(&fx3));

    let (s, fx4) = feed(&s, vec![Event::KeyframeArrived]);
    assert_eq!(s.part_index, 3);
    assert_eq!(
        fx4,
        vec![
            Effect::FinalizePart {
                reason: EndReason::Split(SplitReason::UserRequested)
            },
            Effect::OpenPart { index: 3 },
            Effect::EmitStatus(State::Recording),
        ]
    );
    assert!(!has_start_encoder(&fx4));
}

// ---------- 4: every split reason ----------

#[test]
fn t04a_split_reason_user_requested() {
    let (s, _) = feed(&splitting(cfg()), vec![]);
    let (_, fx) = reduce(&s, Event::KeyframeArrived);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::Split(SplitReason::UserRequested)
        }),
        "fx: {fx:?}"
    );
}

#[test]
fn t04b_split_reason_max_duration_via_tick() {
    let (s, fx) = reduce(&recording(cfg()), tick(1, MAX_MS + 1, 0, FREE_OK));
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe
        ]
    );
    assert_eq!(s.state, State::Splitting);
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
    assert_eq!(s.part_index, 2);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::Split(SplitReason::MaxDuration)
        }),
        "fx: {fx:?}"
    );
}

#[test]
fn t04c_split_reason_max_size_via_tick() {
    let (s, fx) = reduce(&recording(cfg()), tick(1, 0, MAX_BYTES + 1, FREE_OK));
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe
        ]
    );
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
    assert_eq!(s.part_index, 2);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::Split(SplitReason::MaxSize)
        }),
        "fx: {fx:?}"
    );
}

#[test]
fn t04d_split_reason_stream_params_changed() {
    let (s, fx) = reduce(&recording(cfg()), Event::StreamParamsChanged);
    assert_eq!(s.state, State::Splitting);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe
        ]
    );
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
    assert_eq!(s.part_index, 2);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::Split(SplitReason::StreamParamsChanged)
        }),
        "fx: {fx:?}"
    );
}

#[test]
fn t04e_split_reason_layout_changed() {
    let (s, fx) = reduce(&recording(cfg()), Event::LayoutChanged);
    assert_eq!(s.state, State::Splitting);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe
        ]
    );
    let (s, fx) = reduce(&s, Event::KeyframeArrived);
    assert_eq!(s.part_index, 2);
    assert!(
        fx.iter().any(|e| matches!(
            e,
            Effect::FinalizePart {
                reason: EndReason::Split(_)
            }
        )),
        "expected a Split finalize; fx: {fx:?}"
    );
}

// ---------- 5: keyframe timeout ----------

#[test]
fn t05a_keyframe_timeout_attempts_1_and_2_re_request() {
    let s = splitting(cfg());
    for attempts in [1u32, 2] {
        let (s2, fx) = reduce(&s, Event::KeyframeTimeout { attempts });
        assert_eq!(fx, vec![Effect::RequestKeyframe], "attempts {attempts}");
        assert_eq!(s2.state, State::Splitting, "attempts {attempts}");
        assert_eq!(s2.part_index, 1, "attempts {attempts}");
    }
}

#[test]
fn t05b_keyframe_timeout_attempts_3_and_4_cancel_without_part_change() {
    let s = splitting(cfg());
    for attempts in [3u32, 4] {
        let (s2, fx) = reduce(&s, Event::KeyframeTimeout { attempts });
        assert_eq!(
            fx,
            vec![
                Effect::CancelSplit,
                Effect::Notify(Notice::KeyframeTimeout),
                Effect::EmitStatus(State::Recording),
            ],
            "attempts {attempts}"
        );
        assert_eq!(s2.state, State::Recording, "attempts {attempts}");
        assert_eq!(s2.part_index, 1, "attempts {attempts}");
        assert!(
            !has_finalize(&fx) && !has_open_part(&fx),
            "attempts {attempts}"
        );
    }
}

#[test]
fn t05c_after_cancel_new_split_applies_at_index_2() {
    let (s, _) = feed(
        &splitting(cfg()),
        vec![Event::KeyframeTimeout { attempts: 3 }],
    );
    assert_eq!(s.state, State::Recording);
    let (s, fx) = feed(&s, vec![Event::UserSplit]);
    assert_eq!(
        fx,
        vec![
            Effect::EmitStatus(State::Splitting),
            Effect::RequestKeyframe,
            Effect::Reply(Ok(())),
        ]
    );
    assert_eq!(s.state, State::Splitting);
    let (s, fx) = feed(&s, vec![Event::KeyframeArrived]);
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

// ---------- 6: tick during Splitting ----------

#[test]
fn t06_tick_during_splitting_changes_nothing() {
    let s = splitting(cfg());
    let (s2, fx) = reduce(&s, tick(5, MAX_MS + 1, MAX_BYTES + 1, FREE_OK));
    assert_eq!(s2.state, State::Splitting);
    assert_eq!(s2.part_index, 1);
    assert!(
        !fx.contains(&Effect::RequestKeyframe),
        "re-emitted split: {fx:?}"
    );
    assert!(!has_finalize(&fx) && !has_open_part(&fx), "fx: {fx:?}");
}

// ---------- 7-9: terminal events during Splitting ----------

#[test]
fn t07_user_stop_during_splitting_finalizes_without_open() {
    let (s, fx) = reduce(&splitting(cfg()), Event::UserStop);
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::UserStop
        }),
        "fx: {fx:?}"
    );
    assert!(!has_open_part(&fx), "fx: {fx:?}");
    assert_eq!(s.part_index, 1);
}

#[test]
fn t08_game_exit_during_splitting_finalizes_without_open() {
    let (s, fx) = reduce(&splitting(cfg()), game_end("A", ExitReason::Normal, 10));
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::GameExited
        }),
        "fx: {fx:?}"
    );
    assert!(!has_open_part(&fx), "fx: {fx:?}");
    assert_eq!(s.part_index, 1);
}

#[test]
fn t09_disk_full_during_splitting_finalizes_without_open() {
    let (s, fx) = reduce(&splitting(cfg()), Event::WriteFailed(WriteError::DiskFull));
    assert!(
        fx.contains(&Effect::FinalizePart {
            reason: EndReason::DiskFull
        }),
        "fx: {fx:?}"
    );
    assert!(!has_open_part(&fx), "fx: {fx:?}");
    assert_eq!(s.part_index, 1);
}

// ---------- 10: session id propagation ----------

#[test]
fn t10a_set_enabled_from_idle_uses_caller_id() {
    let (s, fx) = feed(
        &SessionState::new(cfg()),
        vec![tick(0, 0, 0, FREE_OK), game_start_id("A", 0, ULID_3)],
    );
    assert_eq!(s.state, State::Idle, "setup: game alone must not start");
    assert!(!has_start_encoder(&fx), "fx: {fx:?}");
    let (s, fx) = reduce(&s, enable(Mode::Auto, ULID_2));
    assert_eq!(s.state, State::Recording);
    assert!(fx.contains(&start_enc(ULID_2, 1, "A")), "fx: {fx:?}");
}

#[test]
fn t10b_game_started_in_armed_uses_caller_id() {
    let (s, _) = feed(
        &SessionState::new(cfg()),
        vec![tick(0, 0, 0, FREE_OK), enable(Mode::Auto, ULID_1)],
    );
    assert_eq!(s.state, State::Armed);
    let (s, fx) = reduce(&s, game_start_id("A", 0, ULID_2));
    assert_eq!(s.state, State::Recording);
    assert!(fx.contains(&start_enc(ULID_2, 1, "A")), "fx: {fx:?}");
}

#[test]
fn t10c_user_start_in_armed_with_game_uses_caller_id() {
    let (s, _) = feed(
        &SessionState::new(cfg()),
        vec![
            tick(0, 0, 0, FREE_OK),
            enable(Mode::Manual, ULID_1),
            game_start_id("A", 0, ULID_1),
        ],
    );
    assert_eq!(s.state, State::Armed, "setup: expected Armed");
    let (s, fx) = reduce(
        &s,
        Event::UserStart {
            new_session_id: ULID_2.to_string(),
        },
    );
    assert_eq!(s.state, State::Recording);
    assert!(fx.contains(&start_enc(ULID_2, 1, "A")), "fx: {fx:?}");
}

#[test]
fn t10d_user_start_from_stopped_by_user_uses_caller_id() {
    let (s, _) = feed(
        &recording(cfg()),
        vec![
            Event::UserStop,
            Event::FinalizeDone {
                ok: true,
                new_session_id: ULID_3.to_string(),
            },
        ],
    );
    assert_eq!(s.state, State::StoppedByUser, "setup");
    let (s, fx) = reduce(
        &s,
        Event::UserStart {
            new_session_id: ULID_2.to_string(),
        },
    );
    assert_eq!(s.state, State::Recording);
    assert!(
        fx.iter().any(|e| matches!(
            e,
            Effect::StartEncoder { session_id, .. } if session_id == ULID_2
        )),
        "fx: {fx:?}"
    );
}

#[test]
fn t10e_finalize_done_completing_game_switch_uses_caller_id() {
    let (s, fx) = reduce(&recording(cfg()), game_start_id("B", 100, ULID_3));
    assert!(
        !has_start_encoder(&fx),
        "switch must wait for finalize: {fx:?}"
    );
    let (s, fx) = reduce(
        &s,
        Event::FinalizeDone {
            ok: true,
            new_session_id: ULID_2.to_string(),
        },
    );
    assert_eq!(s.state, State::Recording);
    assert!(fx.contains(&start_enc(ULID_2, 1, "B")), "fx: {fx:?}");
}

// ---------- 11: join after crash ----------

#[test]
fn t11_join_after_crash_keeps_previous_id_and_increments_part() {
    let (s, _) = feed(
        &recording(cfg_join()),
        vec![game_end("A", ExitReason::Crashed { code: 1 }, 1_000)],
    );
    // Crash may pass through Stopping; complete finalize if pending.
    let s = if s.state == State::Stopping {
        let (s2, _) = feed(
            &s,
            vec![Event::FinalizeDone {
                ok: true,
                new_session_id: ULID_3.to_string(),
            }],
        );
        s2
    } else {
        s
    };
    let (s, fx) = reduce(&s, game_start_id("A", 2_000, ULID_2));
    assert_eq!(s.state, State::Recording, "fx: {fx:?}");
    assert!(
        fx.contains(&start_enc(ULID_1, 2, "A")),
        "expected previous id ULID_1 with part 2; fx: {fx:?}"
    );
    assert!(
        !fx.iter().any(|e| matches!(
            e,
            Effect::StartEncoder { session_id, .. } if session_id == ULID_2
        )),
        "new id must be ignored on join; fx: {fx:?}"
    );
    assert_eq!(s.part_index, 2);
}

// ---------- 12: new id ignored on non-starting event ----------

#[test]
fn t12_new_id_ignored_when_game_already_recording() {
    let s = recording(cfg());
    let (s2, fx) = reduce(&s, game_start_id("A", 1_000, ULID_2));
    assert!(!has_start_encoder(&fx), "fx: {fx:?}");
    assert_eq!(s2.state, State::Recording);
    assert_eq!(s2.part_index, 1);
}
