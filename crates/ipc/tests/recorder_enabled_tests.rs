//! Wire-format tests for the recorder on/off switch: `Command::SetRecorderEnabled` and
//! `StatusDto::recorder_enabled`. Pure serde tests; no daemon, no pipe.

use clipcheck_ipc::proto::{AutostartState, Command, RecState, Reply, StatusDto};
use serde_json::{json, Value};

/// A minimal status snapshot. Only `recorder_enabled` varies between tests.
fn status(recorder_enabled: bool) -> StatusDto {
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
        hotkeys_suspended: false,
        finalizing_parts: 0,
        uptime_s: 10,
        degraded_reason: None,
        game_info: None,
        buffer_secs: 0.0,
        settings_pending_restart: false,
        recorder_enabled,
        server_unix_ms: 1791510843500,
        exe_path: None,
    }
}

#[test]
fn set_recorder_enabled_wire_form_for_true_and_false() {
    let on = serde_json::to_value(Command::SetRecorderEnabled { enabled: true }).unwrap();
    assert_eq!(
        on,
        json!({"type": "set_recorder_enabled", "data": {"enabled": true}})
    );
    let off = serde_json::to_value(Command::SetRecorderEnabled { enabled: false }).unwrap();
    assert_eq!(
        off,
        json!({"type": "set_recorder_enabled", "data": {"enabled": false}})
    );
}

#[test]
fn set_recorder_enabled_roundtrips_both_values() {
    for enabled in [true, false] {
        let cmd = Command::SetRecorderEnabled { enabled };
        let text = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&text).unwrap();
        assert_eq!(back, cmd);
    }
}

#[test]
fn set_recorder_enabled_parses_from_literal_wire_text() {
    let cmd: Command =
        serde_json::from_str(r#"{"type":"set_recorder_enabled","data":{"enabled":false}}"#)
            .unwrap();
    assert_eq!(cmd, Command::SetRecorderEnabled { enabled: false });
}

#[test]
fn set_recorder_enabled_without_enabled_field_is_rejected() {
    let bad: Result<Command, _> =
        serde_json::from_str(r#"{"type":"set_recorder_enabled","data":{}}"#);
    assert!(bad.is_err(), "a missing enabled field must not decode");
    let no_data: Result<Command, _> = serde_json::from_str(r#"{"type":"set_recorder_enabled"}"#);
    assert!(no_data.is_err(), "a missing data object must not decode");
}

#[test]
fn set_recorder_enabled_with_non_bool_is_rejected() {
    let bad: Result<Command, _> =
        serde_json::from_str(r#"{"type":"set_recorder_enabled","data":{"enabled":"yes"}}"#);
    assert!(bad.is_err());
}

#[test]
fn ok_reply_is_the_generic_success_reply() {
    assert_eq!(
        serde_json::to_value(Reply::Ok).unwrap(),
        json!({"type": "ok"})
    );
}

#[test]
fn status_always_serializes_recorder_enabled() {
    let on: Value = serde_json::to_value(status(true)).unwrap();
    assert_eq!(on["recorder_enabled"], json!(true));
    let off: Value = serde_json::to_value(status(false)).unwrap();
    assert_eq!(off["recorder_enabled"], json!(false));
}

#[test]
fn status_missing_recorder_enabled_defaults_to_true() {
    let mut v: Value = serde_json::to_value(status(false)).unwrap();
    v.as_object_mut().unwrap().remove("recorder_enabled");
    let back: StatusDto = serde_json::from_value(v).unwrap();
    assert!(
        back.recorder_enabled,
        "an older daemon omits the field; the UI treats it as on"
    );
}

#[test]
fn status_recorder_enabled_roundtrips() {
    for enabled in [true, false] {
        let s = status(enabled);
        let text = serde_json::to_string(&s).unwrap();
        let back: StatusDto = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.recorder_enabled, enabled);
    }
}
