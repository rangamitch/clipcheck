//! Tests for issue #173: `StatusDto.hotkeys_down`, an additive field that defaults to false
//! when an older daemon omits it. Black-box: public API only.

use clipcheck_ipc::proto::{Reply, Response, StatusDto};
use serde_json::{json, Value};

/// The `data` object of a complete status frame, without `hotkeys_down`.
fn status_data() -> Value {
    json!({
        "state": "recording",
        "game": {
            "game_id": "valorant.exe",
            "exe": "VALORANT-Win64-Shipping.exe",
            "pid": 18844,
            "title": null
        },
        "session": {
            "session_id": "01JDK7W2Q9Z6H4T1V8B3N5M7KC",
            "part": 2,
            "started_unix_ms": 1791510843000i64,
            "path": "C:\\Videos\\session-part2.mkv",
            "parts_saved": 1
        },
        "buffer_seconds": 300,
        "buffer_bytes": 412876800u64,
        "encoder": "nvenc_h264",
        "codec": "h264",
        "fps_target": 60,
        "bitrate_kbps": 12000,
        "mic_muted": false,
        "detection_paused": false,
        "autostart": "enabled",
        "worker_pid": 20312,
        "worker_restarts_10min": 0,
        "uptime_s": 5421,
        "degraded_reason": null,
        "game_info": null,
        "buffer_secs": 300.0,
        "settings_pending_restart": false,
        "server_unix_ms": 1791510843500i64
    })
}

/// `status_data()` with `"hotkeys_down"` set to `value`.
fn status_with_hotkeys_down(value: bool) -> Value {
    let mut v = status_data();
    v.as_object_mut()
        .expect("object")
        .insert("hotkeys_down".to_string(), json!(value));
    v
}

#[test]
fn issue_173_status_without_hotkeys_down_defaults_to_false() {
    let dto: StatusDto =
        serde_json::from_value(status_data()).expect("status without hotkeys_down must parse");
    assert!(
        !dto.hotkeys_down,
        "a missing hotkeys_down must default to false"
    );
}

#[test]
fn issue_173_status_without_hotkeys_down_parses_in_a_response_frame() {
    let v = json!({"v": 1, "id": 3, "body": {"type": "status", "data": status_data()}});
    let r: Response = serde_json::from_value(v).expect("older-daemon status frame must parse");
    match r.body {
        Reply::Status(st) => assert!(!st.hotkeys_down),
        other => panic!("expected status, got {other:?}"),
    }
}

#[test]
fn issue_173_status_with_hotkeys_down_true_deserializes_true() {
    let dto: StatusDto = serde_json::from_value(status_with_hotkeys_down(true))
        .expect("status with hotkeys_down must parse");
    assert!(dto.hotkeys_down);
}

#[test]
fn issue_173_status_with_hotkeys_down_false_deserializes_false() {
    let dto: StatusDto = serde_json::from_value(status_with_hotkeys_down(false))
        .expect("status with hotkeys_down must parse");
    assert!(!dto.hotkeys_down);
}

#[test]
fn issue_173_hotkeys_down_serializes_and_round_trips() {
    let dto: StatusDto = serde_json::from_value(status_with_hotkeys_down(true))
        .expect("status with hotkeys_down must parse");
    let text = serde_json::to_string(&dto).expect("serialize status");
    assert!(
        text.contains("\"hotkeys_down\":true"),
        "serialized status must carry hotkeys_down:true, got {text}"
    );
    let back: StatusDto = serde_json::from_str(&text).expect("round-trip parse");
    assert_eq!(back, dto, "a status must round-trip unchanged");
    assert!(back.hotkeys_down);
}
