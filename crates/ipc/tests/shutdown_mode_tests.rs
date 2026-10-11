//! Wire-format and decision tests for the D47 shutdown mode (`Command::Shutdown`).
//!
//! Pure serde and `ShutdownMode::effective` tests; no daemon, no pipe.

use clipcheck_ipc::proto::{Command, ShutdownMode};
use serde_json::{json, Value};

fn shutdown(reason: Option<&str>, mode: Option<ShutdownMode>) -> Command {
    Command::Shutdown {
        reason: reason.map(str::to_owned),
        mode,
    }
}

/// All eight combinations of (mode, reason) with their expected wire value.
fn wire_cases() -> Vec<(Option<ShutdownMode>, Option<&'static str>, Value)> {
    use ShutdownMode::*;
    vec![
        (None, None, json!({"type": "shutdown"})),
        (
            None,
            Some("update"),
            json!({"type": "shutdown", "data": {"reason": "update"}}),
        ),
        (
            None,
            Some("x"),
            json!({"type": "shutdown", "data": {"reason": "x"}}),
        ),
        (
            Some(Graceful),
            None,
            json!({"type": "shutdown", "data": {"mode": "graceful"}}),
        ),
        (
            Some(Update),
            None,
            json!({"type": "shutdown", "data": {"mode": "update"}}),
        ),
        (
            Some(Force),
            None,
            json!({"type": "shutdown", "data": {"mode": "force"}}),
        ),
        (
            Some(Force),
            Some("update"),
            json!({"type": "shutdown", "data": {"reason": "update", "mode": "force"}}),
        ),
        (
            Some(Graceful),
            Some("installer"),
            json!({"type": "shutdown", "data": {"reason": "installer", "mode": "graceful"}}),
        ),
    ]
}

#[test]
fn mode_serializes_as_snake_case_strings() {
    let cases = [
        (ShutdownMode::Graceful, "graceful"),
        (ShutdownMode::Update, "update"),
        (ShutdownMode::Force, "force"),
    ];
    for (mode, name) in cases {
        assert_eq!(
            serde_json::to_value(mode).unwrap(),
            json!(name),
            "mode {name}"
        );
        let back: ShutdownMode = serde_json::from_value(json!(name)).unwrap();
        assert_eq!(back, mode, "mode {name} parses back");
    }
}

#[test]
fn unknown_mode_string_is_an_error() {
    let r = serde_json::from_str::<Command>(r#"{"type":"shutdown","data":{"mode":"later"}}"#);
    assert!(r.is_err(), "unknown mode must fail, got {r:?}");
}

#[test]
fn mode_with_wrong_json_type_is_an_error() {
    let r = serde_json::from_str::<Command>(r#"{"type":"shutdown","data":{"mode":5}}"#);
    assert!(r.is_err(), "numeric mode must fail, got {r:?}");
}

#[test]
fn serialize_matches_expected_wire_value() {
    for (mode, reason, expected) in wire_cases() {
        let cmd = shutdown(reason, mode);
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v, expected, "serialize {mode:?} / {reason:?}");
        if mode.is_none() && reason.is_none() {
            assert!(
                v.get("data").is_none(),
                "bare shutdown must have no data key"
            );
        }
    }
}

#[test]
fn deserialize_expected_forms() {
    let cases: Vec<(&str, Option<ShutdownMode>, Option<&str>)> = vec![
        (r#"{"type":"shutdown"}"#, None, None),
        (r#"{"type":"shutdown","data":null}"#, None, None),
        (r#"{"type":"shutdown","data":{}}"#, None, None),
        (
            r#"{"type":"shutdown","data":{"reason":"update"}}"#,
            None,
            Some("update"),
        ),
        (
            r#"{"type":"shutdown","data":{"mode":"force"}}"#,
            Some(ShutdownMode::Force),
            None,
        ),
        (
            r#"{"type":"shutdown","data":{"reason":"x","mode":"graceful"}}"#,
            Some(ShutdownMode::Graceful),
            Some("x"),
        ),
        (
            r#"{"type":"shutdown","data":{"mode":"update","bogus":true,"extra":[1,2]}}"#,
            Some(ShutdownMode::Update),
            None,
        ),
    ];
    for (text, mode, reason) in cases {
        let cmd: Command =
            serde_json::from_str(text).unwrap_or_else(|e| panic!("{text} must parse: {e}"));
        assert_eq!(cmd, shutdown(reason, mode), "parse {text}");
    }
}

#[test]
fn every_combination_round_trips() {
    for (mode, reason, _) in wire_cases() {
        let cmd = shutdown(reason, mode);
        let text = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&text).unwrap();
        assert_eq!(back, cmd, "round trip of {text}");
    }
}

#[test]
fn effective_mode_table() {
    use ShutdownMode::*;
    let table: Vec<(Option<ShutdownMode>, Option<&str>, ShutdownMode)> = vec![
        (None, None, Graceful),
        (None, Some("update"), Update),
        (None, Some("installer"), Graceful),
        (None, Some(""), Graceful),
        (Some(Graceful), None, Graceful),
        (Some(Graceful), Some("update"), Graceful),
        (Some(Update), None, Update),
        (Some(Update), Some("installer"), Update),
        (Some(Force), None, Force),
        (Some(Force), Some("update"), Force),
    ];
    for (mode, reason, expected) in table {
        assert_eq!(
            ShutdownMode::effective(mode, reason),
            expected,
            "effective({mode:?}, {reason:?})"
        );
    }
}
