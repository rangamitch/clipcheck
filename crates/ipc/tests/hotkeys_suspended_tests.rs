//! Wire-format tests for `Command::SetHotkeysSuspended` and `StatusDto::hotkeys_suspended`.
//! Pure serde tests; no daemon, no pipe. Black-box: public API only.

use clipcheck_ipc::proto::{AutostartState, Command, RecState, Reply, StatusDto};
use serde_json::{json, Value};

/// A minimal status snapshot. Only `hotkeys_suspended` varies between tests.
fn status(hotkeys_suspended: bool) -> StatusDto {
    StatusDto {
        state: RecState::Armed,
        game: None,
        session: None,
        buffer_seconds: 0,
        buffer_bytes: 0,
        encoder: None,
        codec: None,
        fps_target: 60,
        bitrate_kbps: None,
        mic_muted: false,
        detection_paused: false,
        autostart: AutostartState::Enabled,
        worker_pid: None,
        worker_restarts_10min: 0,
        hotkeys_down: false,
        hotkeys_suspended,
        finalizing_parts: 0,
        uptime_s: 10,
        degraded_reason: None,
        game_info: None,
        buffer_secs: 0.0,
        settings_pending_restart: false,
        recorder_enabled: true,
        server_unix_ms: 1791510843500,
        exe_path: None,
    }
}

#[test]
fn set_hotkeys_suspended_wire_form_for_true_and_false() {
    let on = serde_json::to_value(Command::SetHotkeysSuspended { suspended: true }).unwrap();
    assert_eq!(
        on,
        json!({"type": "set_hotkeys_suspended", "data": {"suspended": true}})
    );
    let off = serde_json::to_value(Command::SetHotkeysSuspended { suspended: false }).unwrap();
    assert_eq!(
        off,
        json!({"type": "set_hotkeys_suspended", "data": {"suspended": false}})
    );
}

#[test]
fn set_hotkeys_suspended_roundtrips_both_values() {
    for suspended in [true, false] {
        let cmd = Command::SetHotkeysSuspended { suspended };
        let text = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&text).unwrap();
        assert_eq!(back, cmd);
    }
}

#[test]
fn set_hotkeys_suspended_parses_from_literal_wire_text() {
    let cmd: Command =
        serde_json::from_str(r#"{"type":"set_hotkeys_suspended","data":{"suspended":true}}"#)
            .unwrap();
    assert_eq!(cmd, Command::SetHotkeysSuspended { suspended: true });
}

#[test]
fn set_hotkeys_suspended_without_suspended_field_is_rejected() {
    let bad: Result<Command, _> =
        serde_json::from_str(r#"{"type":"set_hotkeys_suspended","data":{}}"#);
    assert!(bad.is_err(), "a missing suspended field must not decode");
    let no_data: Result<Command, _> = serde_json::from_str(r#"{"type":"set_hotkeys_suspended"}"#);
    assert!(no_data.is_err(), "a missing data object must not decode");
}

#[test]
fn set_hotkeys_suspended_with_non_bool_is_rejected() {
    let bad: Result<Command, _> =
        serde_json::from_str(r#"{"type":"set_hotkeys_suspended","data":{"suspended":"yes"}}"#);
    assert!(bad.is_err());
}

#[test]
fn set_hotkeys_suspended_is_answered_with_ok() {
    assert_eq!(
        serde_json::to_value(Reply::Ok).unwrap(),
        json!({"type": "ok"})
    );
}

#[test]
fn status_omits_hotkeys_suspended_when_false() {
    let v: Value = serde_json::to_value(status(false)).unwrap();
    assert!(
        v.get("hotkeys_suspended").is_none(),
        "false is omitted, got {v}"
    );
}

#[test]
fn status_serializes_hotkeys_suspended_when_true() {
    let v: Value = serde_json::to_value(status(true)).unwrap();
    assert_eq!(v["hotkeys_suspended"], json!(true));
}

#[test]
fn status_missing_hotkeys_suspended_defaults_to_false() {
    let mut v: Value = serde_json::to_value(status(true)).unwrap();
    v.as_object_mut().unwrap().remove("hotkeys_suspended");
    let back: StatusDto = serde_json::from_value(v).unwrap();
    assert!(
        !back.hotkeys_suspended,
        "an older daemon omits the field; it must read as not suspended"
    );
}

#[test]
fn status_hotkeys_suspended_roundtrips() {
    for suspended in [true, false] {
        let s = status(suspended);
        let text = serde_json::to_string(&s).unwrap();
        let back: StatusDto = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.hotkeys_suspended, suspended);
    }
}
