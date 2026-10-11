//! Wire-format tests for `Event::SessionPartFinished::duration_unknown`.
//!
//! The field is additive: omitted from JSON when false, and a missing key
//! must deserialize as false so older daemons stay compatible.

use clipcheck_ipc::proto::Event;
use serde_json::{json, Value};

fn finished(duration_unknown: bool) -> Event {
    Event::SessionPartFinished {
        session_id: "s".into(),
        part: 1,
        path: "p".into(),
        duration_ms: 0,
        duration_unknown,
        bytes: 10,
        recovered: false,
        started_unix_ms: 1_791_510_843_000,
        parts_saved: 1,
    }
}

fn data_of(event: &Event) -> Value {
    let v = serde_json::to_value(event).expect("serialize event");
    assert_eq!(v["type"], "session_part_finished", "tag: {v}");
    v["data"].clone()
}

#[test]
fn false_is_omitted_from_json_text() {
    let text = serde_json::to_string(&finished(false)).expect("to_string");
    assert!(
        !text.contains("duration_unknown"),
        "key must be omitted when false: {text}"
    );
    assert!(data_of(&finished(false)).get("duration_unknown").is_none());
}

#[test]
fn true_is_serialized_as_true() {
    let text = serde_json::to_string(&finished(true)).expect("to_string");
    assert!(
        text.contains("\"duration_unknown\":true"),
        "key must be present and true: {text}"
    );
    assert_eq!(
        data_of(&finished(true))["duration_unknown"],
        Value::Bool(true)
    );
}

#[test]
fn missing_key_deserializes_as_false() {
    let v = json!({
        "type": "session_part_finished",
        "data": {
            "session_id": "s",
            "part": 1,
            "path": "p",
            "duration_ms": 1000,
            "bytes": 2000,
            "recovered": true,
            "started_unix_ms": 1791510843000i64,
            "parts_saved": 1
        }
    });
    let ev: Event = serde_json::from_value(v).expect("parse without key");
    match ev {
        Event::SessionPartFinished {
            duration_unknown,
            duration_ms,
            ..
        } => {
            assert!(!duration_unknown, "missing key must default to false");
            assert_eq!(duration_ms, 1000);
        }
        other => panic!("expected SessionPartFinished, got {other:?}"),
    }
}

#[test]
fn true_survives_json_round_trip() {
    let original = finished(true);
    let text = serde_json::to_string(&original).expect("to_string");
    let back: Event = serde_json::from_str(&text).expect("from_str");
    assert_eq!(back, original);
    match back {
        Event::SessionPartFinished {
            duration_unknown, ..
        } => assert!(duration_unknown),
        other => panic!("expected SessionPartFinished, got {other:?}"),
    }
}

#[test]
fn false_survives_json_round_trip() {
    let original = finished(false);
    let text = serde_json::to_string(&original).expect("to_string");
    let back: Event = serde_json::from_str(&text).expect("from_str");
    assert_eq!(back, original);
}
