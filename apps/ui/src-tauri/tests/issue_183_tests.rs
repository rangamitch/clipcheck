//! Issue #183 / #77: close-button decision table and `get_status` reply mapping.
//!
//! Pure tests only (no daemon, no processes). Spec:
//! - `minimize_to_tray` always ends only the UI, whatever the session state.
//! - `exit` with an unknown state (link down, error, timeout, malformed reply) always
//!   asks for confirmation, whatever `confirm_enabled` says.
//! - `exit` with an active session asks only when `confirm_enabled`, else stops the daemon.
//! - `exit` with no session stops the daemon without asking.
//! - A status reply with a session is Active, a status reply with `session: null` is
//!   Inactive, anything else is Unknown (fail closed).

use clipcheck_ui_lib::closing::{
    decide_close, decide_close_state, session_state_from_reply, CloseAction, CloseBehavior,
    SessionState,
};
use clipcheck_ui_lib::ipc::ClientError;
use serde_json::{json, Value};

const ALL_BEHAVIORS: [CloseBehavior; 2] = [CloseBehavior::MinimizeToTray, CloseBehavior::Exit];
const ALL_STATES: [SessionState; 3] = [
    SessionState::Active,
    SessionState::Inactive,
    SessionState::Unknown,
];

/// Expected action per the spec, written independently of the implementation.
fn expected(behavior: CloseBehavior, confirm: bool, state: SessionState) -> CloseAction {
    use CloseAction::{AskConfirmThenExit, ExitUiOnly, StopDaemonAndExit};
    match (behavior, state) {
        (CloseBehavior::MinimizeToTray, _) => ExitUiOnly,
        (CloseBehavior::Exit, SessionState::Unknown) => AskConfirmThenExit,
        (CloseBehavior::Exit, SessionState::Active) => {
            if confirm {
                AskConfirmThenExit
            } else {
                StopDaemonAndExit
            }
        }
        (CloseBehavior::Exit, SessionState::Inactive) => StopDaemonAndExit,
    }
}

/// A `get_status` reply body with an active session, shaped like `Reply::Status`
/// (serde `tag = "type", content = "data"`, `StatusDto` fields).
fn status_with_session() -> Value {
    json!({
        "type": "status",
        "data": {
            "state": "idle",
            "game": null,
            "session": {
                "session_id": "s-1",
                "part": 1,
                "started_unix_ms": 1_700_000_000_000_i64,
                "path": "C:\\Videos\\clip.mp4",
                "parts_saved": 0
            },
            "buffer_seconds": 30,
            "buffer_bytes": 0,
            "encoder": null,
            "codec": null,
            "fps_target": 60,
            "bitrate_kbps": null,
            "mic_muted": false,
            "detection_paused": false,
            "autostart": "off",
            "worker_pid": null,
            "worker_restarts_10min": 0,
            "uptime_s": 10,
            "degraded_reason": null,
            "exe": "",
            "name": "",
            "icon_png_b64": null,
            "active": false,
            "peak": 0.0,
            "last_active_unix_ms": 0
        }
    })
}

/// A `get_status` reply body with no session (`session: null`).
fn status_without_session() -> Value {
    let mut body = status_with_session();
    body["data"]["session"] = Value::Null;
    body
}

// ---------------------------------------------------------------------------
// Truth table: decide_close_state over behaviours x confirm x session states
// ---------------------------------------------------------------------------

#[test]
fn decide_close_state_full_truth_table() {
    let mut cases = 0;
    for behavior in ALL_BEHAVIORS {
        for confirm in [true, false] {
            for state in ALL_STATES {
                let got = decide_close_state(behavior, confirm, state);
                let want = expected(behavior, confirm, state);
                assert_eq!(
                    got, want,
                    "decide_close_state({behavior:?}, confirm={confirm}, {state:?})"
                );
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 12);
}

#[test]
fn minimize_to_tray_ignores_state_and_confirm() {
    for confirm in [true, false] {
        for state in ALL_STATES {
            assert_eq!(
                decide_close_state(CloseBehavior::MinimizeToTray, confirm, state),
                CloseAction::ExitUiOnly,
                "confirm={confirm}, state={state:?}"
            );
        }
    }
}

#[test]
fn exit_unknown_always_asks_regardless_of_confirm() {
    for confirm in [true, false] {
        assert_eq!(
            decide_close_state(CloseBehavior::Exit, confirm, SessionState::Unknown),
            CloseAction::AskConfirmThenExit,
            "confirm={confirm}"
        );
    }
}

#[test]
fn exit_active_confirm_matrix() {
    assert_eq!(
        decide_close_state(CloseBehavior::Exit, true, SessionState::Active),
        CloseAction::AskConfirmThenExit
    );
    assert_eq!(
        decide_close_state(CloseBehavior::Exit, false, SessionState::Active),
        CloseAction::StopDaemonAndExit
    );
}

#[test]
fn exit_inactive_never_asks() {
    for confirm in [true, false] {
        assert_eq!(
            decide_close_state(CloseBehavior::Exit, confirm, SessionState::Inactive),
            CloseAction::StopDaemonAndExit,
            "confirm={confirm}"
        );
    }
}

// ---------------------------------------------------------------------------
// decide_close (bool form): connected=false must map to Unknown
// ---------------------------------------------------------------------------

#[test]
fn decide_close_disconnected_maps_to_unknown() {
    for behavior in ALL_BEHAVIORS {
        for confirm in [true, false] {
            for session_active in [true, false] {
                let got = decide_close(behavior, confirm, session_active, false);
                let want = expected(behavior, confirm, SessionState::Unknown);
                assert_eq!(
                    got, want,
                    "decide_close({behavior:?}, confirm={confirm}, session={session_active}, connected=false)"
                );
            }
        }
    }
}

#[test]
fn decide_close_connected_maps_session_flag_to_state() {
    for behavior in ALL_BEHAVIORS {
        for confirm in [true, false] {
            for session_active in [true, false] {
                let state = if session_active {
                    SessionState::Active
                } else {
                    SessionState::Inactive
                };
                assert_eq!(
                    decide_close(behavior, confirm, session_active, true),
                    expected(behavior, confirm, state),
                    "decide_close({behavior:?}, confirm={confirm}, session={session_active}, connected=true)"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// session_state_from_reply: well-formed replies
// ---------------------------------------------------------------------------

#[test]
fn status_reply_with_session_is_active() {
    assert_eq!(
        session_state_from_reply(Ok(status_with_session())),
        SessionState::Active
    );
}

#[test]
fn status_reply_without_session_is_inactive() {
    assert_eq!(
        session_state_from_reply(Ok(status_without_session())),
        SessionState::Inactive
    );
}

#[test]
fn transport_errors_are_unknown() {
    assert_eq!(
        session_state_from_reply(Err(ClientError::Timeout)),
        SessionState::Unknown
    );
    assert_eq!(
        session_state_from_reply(Err(ClientError::Disconnected)),
        SessionState::Unknown
    );
    assert_eq!(
        session_state_from_reply(Err(ClientError::InvalidCommand("x".into()))),
        SessionState::Unknown
    );
}

#[test]
fn daemon_error_reply_is_unknown() {
    assert_eq!(
        session_state_from_reply(Ok(json!({ "type": "error", "data": { "code": "busy" } }))),
        SessionState::Unknown
    );
}

// ---------------------------------------------------------------------------
// session_state_from_reply: malformed replies must fail closed (Unknown)
// ---------------------------------------------------------------------------

#[test]
fn null_and_non_object_replies_are_unknown() {
    let bodies = [
        json!(null),
        json!("status"),
        json!(42),
        json!(true),
        json!([]),
        json!(["status"]),
    ];
    for body in bodies {
        assert_eq!(
            session_state_from_reply(Ok(body.clone())),
            SessionState::Unknown,
            "body={body}"
        );
    }
}

#[test]
fn empty_object_reply_is_unknown() {
    assert_eq!(
        session_state_from_reply(Ok(json!({}))),
        SessionState::Unknown
    );
}

#[test]
fn wrong_type_tag_is_unknown() {
    let bodies = [
        json!({ "type": 5, "data": { "session": null } }),
        json!({ "type": null, "data": { "session": null } }),
        json!({ "type": "Status", "data": { "session": null } }),
        json!({ "type": ["status"], "data": { "session": null } }),
        json!({ "data": { "session": null } }),
    ];
    for body in bodies {
        assert_eq!(
            session_state_from_reply(Ok(body.clone())),
            SessionState::Unknown,
            "body={body}"
        );
    }
}

#[test]
fn status_missing_data_is_unknown() {
    assert_eq!(
        session_state_from_reply(Ok(json!({ "type": "status" }))),
        SessionState::Unknown
    );
}

#[test]
fn status_with_null_data_is_unknown() {
    assert_eq!(
        session_state_from_reply(Ok(json!({ "type": "status", "data": null }))),
        SessionState::Unknown
    );
}

#[test]
fn status_with_non_object_data_is_unknown() {
    let bodies = [
        json!({ "type": "status", "data": "session" }),
        json!({ "type": "status", "data": 1 }),
        json!({ "type": "status", "data": [] }),
    ];
    for body in bodies {
        assert_eq!(
            session_state_from_reply(Ok(body.clone())),
            SessionState::Unknown,
            "body={body}"
        );
    }
}

#[test]
fn status_missing_session_field_is_unknown() {
    // A real StatusDto always serialises `session` (null or object), so a missing key
    // means the reply is not a status snapshot.
    assert_eq!(
        session_state_from_reply(Ok(json!({ "type": "status", "data": {} }))),
        SessionState::Unknown
    );
    assert_eq!(
        session_state_from_reply(Ok(json!({ "type": "status", "data": { "state": "idle" } }))),
        SessionState::Unknown
    );
}

#[test]
fn status_session_wrong_type_is_unknown() {
    let sessions = [
        json!("s-1"),
        json!(42),
        json!(true),
        json!([]),
        json!(["s-1"]),
    ];
    for session in sessions {
        let body = json!({ "type": "status", "data": { "session": session.clone() } });
        assert_eq!(
            session_state_from_reply(Ok(body.clone())),
            SessionState::Unknown,
            "session={session}"
        );
    }
}

#[test]
fn malformed_reply_with_exit_always_asks() {
    let garbage = [
        Err(ClientError::Timeout),
        Ok(json!(null)),
        Ok(json!("status")),
        Ok(json!({})),
        Ok(json!({ "type": "status" })),
        Ok(json!({ "type": "status", "data": null })),
        Ok(json!({ "type": "status", "data": { "session": "s-1" } })),
    ];
    for reply in garbage {
        let state = session_state_from_reply(reply);
        for confirm in [true, false] {
            assert_eq!(
                decide_close_state(CloseBehavior::Exit, confirm, state),
                CloseAction::AskConfirmThenExit,
                "state={state:?}, confirm={confirm}"
            );
        }
    }
}
