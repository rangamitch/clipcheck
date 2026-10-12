//! Tests for the `game` field of `Command::RegisterEditedRecording` (wire form and round trips).
//!
//! The field is optional on the wire: an old client that omits it still parses, with `game`
//! set to `None`. A value of `"Exports"` is carried through unchanged.

use clipcheck_ipc::proto::{Command, Request};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn register(game: Option<&str>) -> Command {
    Command::RegisterEditedRecording {
        source_recording_id: Some("rec-1".into()),
        path: r"C:\out\edit.mp4".into(),
        keep_source_time: false,
        game: game.map(str::to_owned),
    }
}

/// The `game` field of a parsed RegisterEditedRecording command.
fn game_of(c: &Command) -> Option<String> {
    match c {
        Command::RegisterEditedRecording { game, .. } => game.clone(),
        other => panic!("expected RegisterEditedRecording, got {other:?}"),
    }
}

fn parse_command(text: &str) -> Command {
    serde_json::from_str(text).expect("command parses")
}

/// Wire JSON of a command, as a generic value.
fn to_value(c: &Command) -> Value {
    serde_json::to_value(c).expect("command serializes")
}

// 1. JSON without a game key (an older client) still parses, with game None.
#[test]
fn json_without_game_parses_to_none() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":"r1","path":"C:\\out\\edit.mp4","keep_source_time":true}}"#,
    );
    assert_eq!(game_of(&c), None);
    assert_eq!(
        c,
        Command::RegisterEditedRecording {
            source_recording_id: Some("r1".into()),
            path: r"C:\out\edit.mp4".into(),
            keep_source_time: true,
            game: None,
        }
    );
}

// 2. "game":"Exports" parses to Some("Exports").
#[test]
fn json_with_exports_game_parses_to_some() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":null,"path":"C:\\out\\edit.mp4","keep_source_time":false,"game":"Exports"}}"#,
    );
    assert_eq!(game_of(&c), Some("Exports".to_owned()));
}

// 3. An explicit JSON null is the same as a missing key.
#[test]
fn json_null_game_parses_to_none() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":null,"path":"C:\\a.mp4","keep_source_time":false,"game":null}}"#,
    );
    assert_eq!(game_of(&c), None);
}

// 4. An empty string is kept as Some("") on the wire; the daemon decides it means "no override".
#[test]
fn json_empty_game_is_kept_as_some_empty() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":null,"path":"C:\\a.mp4","keep_source_time":false,"game":""}}"#,
    );
    assert_eq!(game_of(&c), Some(String::new()));
}

// 5. A game name with spaces and punctuation is carried unchanged.
#[test]
fn game_name_with_spaces_is_preserved() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":null,"path":"C:\\a.mp4","keep_source_time":false,"game":"Half-Life 2: Episode One"}}"#,
    );
    assert_eq!(game_of(&c), Some("Half-Life 2: Episode One".to_owned()));
}

// 6. The game value is not case-normalized by the wire layer.
#[test]
fn game_value_case_is_not_normalized() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":null,"path":"C:\\a.mp4","keep_source_time":false,"game":"exports"}}"#,
    );
    assert_eq!(game_of(&c), Some("exports".to_owned()));
}

// 7. Round trip Some("Exports"): Command -> JSON -> Command.
#[test]
fn round_trip_some_exports_preserves_game() {
    let c = register(Some("Exports"));
    let text = serde_json::to_string(&c).expect("serialize");
    let back: Command = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, c);
    assert_eq!(game_of(&back), Some("Exports".to_owned()));
}

// 8. Round trip None: Command -> JSON -> Command.
#[test]
fn round_trip_none_preserves_absence() {
    let c = register(None);
    let text = serde_json::to_string(&c).expect("serialize");
    let back: Command = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, c);
    assert_eq!(game_of(&back), None);
}

// 9. Round trip Some("") keeps the empty string distinct from None.
#[test]
fn round_trip_empty_game_is_not_none() {
    let c = register(Some(""));
    let text = serde_json::to_string(&c).expect("serialize");
    let back: Command = serde_json::from_str(&text).expect("parse");
    assert_eq!(game_of(&back), Some(String::new()));
    assert_ne!(back, register(None));
}

// 10. The serialized frame names the command and puts game under data.
#[test]
fn wire_form_has_type_and_game_under_data() {
    let v = to_value(&register(Some("Exports")));
    assert_eq!(v["type"], json!("register_edited_recording"));
    assert_eq!(v["data"]["game"], json!("Exports"));
    assert_eq!(v["data"]["path"], json!(r"C:\out\edit.mp4"));
}

// 11. A None game serializes as null, and the command still parses back to None.
#[test]
fn wire_form_for_none_parses_back_to_none() {
    let v = to_value(&register(None));
    assert!(v["data"]["game"].is_null(), "game should be null: {v}");
    let back: Command = serde_json::from_value(v).expect("parse");
    assert_eq!(game_of(&back), None);
}

// 12. Unknown extra fields inside data are ignored, so the game still parses.
#[test]
fn unknown_extra_fields_are_ignored() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","data":{"source_recording_id":null,"path":"C:\\a.mp4","keep_source_time":false,"game":"Exports","future_field":[1,2,3]}}"#,
    );
    assert_eq!(game_of(&c), Some("Exports".to_owned()));
}

// 13. Unknown fields at the command level are ignored.
#[test]
fn unknown_fields_on_command_are_ignored() {
    let c = parse_command(
        r#"{"type":"register_edited_recording","extra":true,"data":{"path":"C:\\a.mp4","keep_source_time":false}}"#,
    );
    assert_eq!(game_of(&c), None);
}

// 14. Omitting source_recording_id and game together still parses (both optional).
#[test]
fn only_path_is_enough_with_defaults() {
    let c = parse_command(r#"{"type":"register_edited_recording","data":{"path":"C:\\a.mp4"}}"#);
    assert_eq!(
        c,
        Command::RegisterEditedRecording {
            source_recording_id: None,
            path: r"C:\a.mp4".into(),
            keep_source_time: false,
            game: None,
        }
    );
}

// 15. A missing path is still an error: the game field did not make other fields optional.
#[test]
fn missing_path_is_still_an_error() {
    let r = serde_json::from_str::<Command>(
        r#"{"type":"register_edited_recording","data":{"keep_source_time":false,"game":"Exports"}}"#,
    );
    assert!(r.is_err(), "path must stay required");
}

// 16. A non-string game value is rejected.
#[test]
fn non_string_game_is_rejected() {
    let r = serde_json::from_str::<Command>(
        r#"{"type":"register_edited_recording","data":{"path":"C:\\a.mp4","game":5}}"#,
    );
    assert!(r.is_err(), "game must be a string or null");
}

// 17. The game field survives inside the full request envelope (id and v preserved).
#[test]
fn game_survives_request_envelope_round_trip() {
    let req = Request::request(7, register(Some("Exports")));
    let text = serde_json::to_string(&req).expect("serialize");
    let back: Request = serde_json::from_str(&text).expect("parse request");
    assert_eq!(back.id, Some(7));
    assert_eq!(game_of(&back.body), Some("Exports".to_owned()));
    assert_eq!(back, req);
}

// 18. A request envelope without game parses with game None.
#[test]
fn request_envelope_without_game_parses_to_none() {
    let text = r#"{"v":1,"id":3,"body":{"type":"register_edited_recording","data":{"path":"C:\\a.mp4","keep_source_time":false}}}"#;
    let req: Request = serde_json::from_str(text).expect("parse request");
    assert_eq!(req.id, Some(3));
    assert_eq!(game_of(&req.body), None);
}

// 19. keep_source_time and game are independent: both survive together.
#[test]
fn keep_source_time_and_game_round_trip_together() {
    let c = Command::RegisterEditedRecording {
        source_recording_id: None,
        path: r"D:\Exports\clip.mp4".into(),
        keep_source_time: true,
        game: Some("Exports".into()),
    };
    let text = serde_json::to_string(&c).expect("serialize");
    let back: Command = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, c);
}

// 20. Serializing a None game and parsing it back gives the same command as the original.
#[test]
fn none_game_round_trip_equals_original() {
    let c = register(None);
    let back: Command = serde_json::from_value(to_value(&c)).expect("parse");
    assert_eq!(back, c);
}
