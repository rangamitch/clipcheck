//! Behavioural tests for clipcheck-ipc: framing, wire format, protocol types.

use std::io::{self, Cursor, ErrorKind, Write};
use std::time::Duration;

use clipcheck_ipc::frame::{read_frame, write_frame, FrameError, MAX_FRAME_BYTES};
use clipcheck_ipc::proto::{
    negotiate, AudioAppDto, AutostartState, Command, ConfigIssue, DeviceDto, DiagSeverity,
    DiagnosticDto, EncoderDto, Envelope, ErrorCode, Event, GameRef, GpuDto, Push, RecState, Reply,
    Request, Response, RunningAppDto, SessionInfo, StatusDto, Topic, TrackLevel,
    PROTOCOL_MIN_SUPPORTED, PROTOCOL_VERSION,
};
use clipcheck_ipc::{HANDSHAKE_TIMEOUT, MAX_CLIENTS, PIPE_NAME};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn sample_game() -> GameRef {
    GameRef {
        game_id: "valorant.exe".into(),
        exe: "VALORANT-Win64-Shipping.exe".into(),
        pid: 18844,
        title: None,
    }
}

fn sample_status() -> StatusDto {
    StatusDto {
        state: RecState::Recording,
        game: Some(sample_game()),
        session: Some(SessionInfo {
            session_id: "01JDK7W2Q9Z6H4T1V8B3N5M7KC".into(),
            part: 2,
            started_unix_ms: 1791510843000,
            path: r"C:\Users\someone\Videos\ClipCheck\Valorant\part2.mkv".into(),
            parts_saved: 1,
        }),
        buffer_seconds: 300,
        buffer_bytes: 412876800,
        encoder: Some("nvenc_h264".into()),
        codec: Some("h264".into()),
        fps_target: 60,
        bitrate_kbps: Some(12000),
        mic_muted: false,
        detection_paused: false,
        autostart: AutostartState::Enabled,
        worker_pid: Some(20312),
        worker_restarts_10min: 0,
        hotkeys_down: false,
        hotkeys_suspended: false,
        finalizing_parts: 0,
        uptime_s: 5421,
        degraded_reason: None,
        game_info: None,
        buffer_secs: 300.0,
        settings_pending_restart: false,
        recorder_enabled: true,
        server_unix_ms: 1791510843500,
        exe_path: None,
    }
}

/// A writer whose every write fails.
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("boom"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn header(len: u32) -> [u8; 4] {
    len.to_le_bytes()
}

// ---------------------------------------------------------------------------
// framing
// ---------------------------------------------------------------------------

#[test]
fn frame_roundtrips_request_through_cursor() {
    let req: Request = Envelope::request(
        42,
        Command::RenameRecording {
            recording_id: "rec-1".into(),
            name: "Ace round".into(),
        },
    );
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).expect("write_frame");
    let back: Request = read_frame(&mut Cursor::new(buf)).expect("read_frame");
    assert_eq!(back, req);
}

#[test]
fn frame_roundtrips_push_event_through_cursor() {
    let push: Push = Envelope::event(
        1042,
        Event::ClipSaved {
            clip_id: "01JDK9B7KCV4Y1N8T6P2Q9R3WE".into(),
            path: "clip.mp4".into(),
            duration_ms: 30012,
            bytes: 48211234,
        },
    );
    let mut buf = Vec::new();
    write_frame(&mut buf, &push).expect("write_frame");
    let back: Push = read_frame(&mut Cursor::new(buf)).expect("read_frame");
    assert_eq!(back, push);
}

#[test]
fn frame_wire_layout_is_little_endian_length_then_json_body() {
    let req: Request = Envelope::request(7, Command::Ping);
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).expect("write_frame");

    let declared = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    assert_eq!(
        declared,
        buf.len() - 4,
        "header must equal the remaining byte count"
    );

    let body: Value = serde_json::from_slice(&buf[4..]).expect("body is JSON");
    assert_eq!(body, json!({"v": 1, "id": 7, "body": {"type": "ping"}}));
}

#[test]
fn frame_multiple_frames_back_to_back_read_in_order() {
    let a: Request = Envelope::request(1, Command::Ping);
    let b: Request = Envelope::request(2, Command::GetStatus);
    let c: Request = Envelope::request(3, Command::ClipNow { seconds: 30 });

    let mut buf = Vec::new();
    write_frame(&mut buf, &a).unwrap();
    write_frame(&mut buf, &b).unwrap();
    write_frame(&mut buf, &c).unwrap();

    let mut cur = Cursor::new(buf);
    assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), a);
    assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), b);
    assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), c);
    assert!(matches!(
        read_frame::<_, Request>(&mut cur),
        Err(FrameError::Eof)
    ));
}

#[test]
fn frame_read_on_empty_stream_is_clean_eof() {
    let res: Result<Value, _> = read_frame(&mut Cursor::new(Vec::<u8>::new()));
    assert!(matches!(res, Err(FrameError::Eof)), "got {res:?}");
}

#[test]
fn frame_partial_header_of_1_to_3_bytes_is_truncated() {
    for n in 1..=3usize {
        let bytes = vec![0x05u8; n];
        let res: Result<Value, _> = read_frame(&mut Cursor::new(bytes));
        assert!(
            matches!(res, Err(FrameError::Truncated)),
            "{n}-byte header: got {res:?}"
        );
    }
}

#[test]
fn frame_body_shorter_than_header_is_truncated() {
    let mut bytes = header(100).to_vec();
    bytes.extend_from_slice(b"{\"a\":");
    let res: Result<Value, _> = read_frame(&mut Cursor::new(bytes));
    assert!(matches!(res, Err(FrameError::Truncated)), "got {res:?}");
}

#[test]
fn frame_header_over_max_is_too_large_without_body() {
    let bytes = header((MAX_FRAME_BYTES + 1) as u32).to_vec();
    let res: Result<Value, _> = read_frame(&mut Cursor::new(bytes));
    match res {
        Err(FrameError::TooLarge(n)) => assert_eq!(n, MAX_FRAME_BYTES + 1),
        other => panic!("expected TooLarge({}), got {other:?}", MAX_FRAME_BYTES + 1),
    }
}

#[test]
fn frame_header_exactly_max_with_incomplete_body_is_truncated_not_too_large() {
    let mut bytes = header(MAX_FRAME_BYTES as u32).to_vec();
    bytes.extend_from_slice(b"{\"v\":1");
    let res: Result<Value, _> = read_frame(&mut Cursor::new(bytes));
    assert!(matches!(res, Err(FrameError::Truncated)), "got {res:?}");
}

#[test]
fn frame_write_oversized_body_is_too_large_and_writes_nothing() {
    let cmd: Request = Envelope::request(
        9,
        Command::PatchConfig {
            patch: Value::String("x".repeat(1_100_000)),
            dry_run: false,
        },
    );
    let mut buf: Vec<u8> = Vec::new();
    let res = write_frame(&mut buf, &cmd);
    assert!(
        matches!(res, Err(FrameError::TooLarge(n)) if n > MAX_FRAME_BYTES),
        "got {res:?}"
    );
    assert!(buf.is_empty(), "writer received {} bytes", buf.len());
}

#[test]
fn frame_body_not_valid_json_for_target_type_is_json_error() {
    let body = b"abc";
    let mut bytes = header(body.len() as u32).to_vec();
    bytes.extend_from_slice(body);
    let res: Result<Request, _> = read_frame(&mut Cursor::new(bytes));
    assert!(matches!(res, Err(FrameError::Json(_))), "got {res:?}");
}

#[test]
fn frame_valid_json_of_wrong_shape_is_json_error() {
    // Valid JSON, but not a Request.
    let mut w = Vec::new();
    write_frame(&mut w, &json!({"nope": true})).unwrap();
    let res: Result<Request, _> = read_frame(&mut Cursor::new(w));
    assert!(matches!(res, Err(FrameError::Json(_))), "got {res:?}");
}

#[test]
fn frame_write_on_failing_writer_is_io_error() {
    let req: Request = Envelope::request(1, Command::Ping);
    let res = write_frame(&mut FailingWriter, &req);
    assert!(matches!(res, Err(FrameError::Io(_))), "got {res:?}");
}

#[test]
fn frame_read_io_error_from_underlying_reader_is_io_error() {
    struct FailingReader;
    impl io::Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::PermissionDenied, "no"))
        }
    }
    let res: Result<Value, _> = read_frame(&mut FailingReader);
    assert!(matches!(res, Err(FrameError::Io(_))), "got {res:?}");
}

// ---------------------------------------------------------------------------
// serde examples from the design doc (ids are u64)
// ---------------------------------------------------------------------------

fn assert_json_roundtrip<T>(input: &str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let original: Value = serde_json::from_str(input).expect("input is JSON");
    let typed: T = serde_json::from_str(input).expect("input parses into typed struct");
    let again = serde_json::to_value(&typed).expect("re-serialize");
    assert_eq!(again, original, "re-serialized JSON differs");
}

#[test]
fn example_a_hello_request_parses() {
    let s = r#"{"v":1,"id":1,"body":{"type":"hello","data":{"proto_min":1,"proto_max":1,"client":"clipcheck-ui","client_version":"0.1.0"}}}"#;
    assert_json_roundtrip::<Request>(s);
    let req: Request = serde_json::from_str(s).unwrap();
    assert!(matches!(
        req.body,
        Command::Hello {
            proto_min: 1,
            proto_max: 1,
            ..
        }
    ));
}

#[test]
fn example_b_hello_ack_response_parses() {
    let s = r#"{"v":1,"id":1,"body":{"type":"hello_ack","data":{"proto":1,"daemon_version":"0.1.0","daemon_pid":4120,"features":["clip_save","split","levels","diagnostics"]}}}"#;
    assert_json_roundtrip::<Response>(s);
}

#[test]
fn example_c_error_response_parses() {
    let s = r#"{"v":1,"id":2,"body":{"type":"error","data":{"code":"proto_mismatch","message":"server supports 1..=1","retryable":false}}}"#;
    assert_json_roundtrip::<Response>(s);
    let r: Response = serde_json::from_str(s).unwrap();
    assert!(matches!(
        r.body,
        Reply::Error {
            code: ErrorCode::ProtoMismatch,
            retryable: false,
            ..
        }
    ));
}

#[test]
fn example_d_get_status_request_parses() {
    let s = r#"{"v":1,"id":3,"body":{"type":"get_status"}}"#;
    assert_json_roundtrip::<Request>(s);
    let r: Request = serde_json::from_str(s).unwrap();
    assert_eq!(r.body, Command::GetStatus);
}

#[test]
fn example_e_status_reply_parses() {
    let s = r#"{"v":1,"id":3,"body":{"type":"status","data":{"state":"recording","game":{"game_id":"valorant.exe","exe":"VALORANT-Win64-Shipping.exe","pid":18844,"title":null},"session":{"session_id":"01JDK7W2Q9Z6H4T1V8B3N5M7KC","part":2,"started_unix_ms":1791510843000,"path":"C:\\Users\\someone\\Videos\\ClipCheck\\Valorant\\Valorant 2026-10-09 21-14-03 session-part2.mkv","parts_saved":1},"buffer_seconds":300,"buffer_bytes":412876800,"encoder":"nvenc_h264","codec":"h264","fps_target":60,"bitrate_kbps":12000,"mic_muted":false,"detection_paused":false,"autostart":"enabled","worker_pid":20312,"worker_restarts_10min":0,"uptime_s":5421,"degraded_reason":null,"game_info":null,"buffer_secs":300.0,"settings_pending_restart":false,"recorder_enabled":true,"server_unix_ms":1791510843500}}}"#;
    assert_json_roundtrip::<Response>(s);
    let r: Response = serde_json::from_str(s).unwrap();
    match r.body {
        Reply::Status(st) => {
            assert_eq!(st.state, RecState::Recording);
            assert_eq!(st.autostart, AutostartState::Enabled);
            assert_eq!(st.worker_pid, Some(20312));
        }
        other => panic!("expected Reply::Status, got {other:?}"),
    }
}

#[test]
fn example_f_subscribe_request_parses() {
    let s = r#"{"v":1,"id":4,"body":{"type":"subscribe","data":{"topics":["state","clips","health","warnings"]}}}"#;
    assert_json_roundtrip::<Request>(s);
    let r: Request = serde_json::from_str(s).unwrap();
    assert_eq!(
        r.body,
        Command::Subscribe {
            topics: vec![Topic::State, Topic::Clips, Topic::Health, Topic::Warnings]
        }
    );
}

#[test]
fn example_g_clip_now_request_parses() {
    let s = r#"{"v":1,"id":5,"body":{"type":"clip_now","data":{"seconds":30}}}"#;
    assert_json_roundtrip::<Request>(s);
    let r: Request = serde_json::from_str(s).unwrap();
    assert_eq!(r.body, Command::ClipNow { seconds: 30 });
}

#[test]
fn example_h_clip_queued_response_parses() {
    let s = r#"{"v":1,"id":5,"body":{"type":"clip_queued","data":{"clip_id":"01JDK9B7KCV4Y1N8T6P2Q9R3WE"}}}"#;
    assert_json_roundtrip::<Response>(s);
}

#[test]
fn example_i_clip_saved_push_parses_with_seq_and_no_id() {
    let s = r#"{"v":1,"seq":1042,"body":{"type":"clip_saved","data":{"clip_id":"01JDK9B7KCV4Y1N8T6P2Q9R3WE","path":"C:\\Users\\someone\\Videos\\ClipCheck\\Valorant\\Valorant 2026-10-09 21-14-03 clip-30s.mp4","duration_ms":30012,"bytes":48211234}}}"#;
    assert_json_roundtrip::<Push>(s);
    let p: Push = serde_json::from_str(s).unwrap();
    assert_eq!(p.seq, Some(1042));
    assert_eq!(p.id, None);
}

#[test]
fn example_j_state_changed_push_parses() {
    let s = r#"{"v":1,"seq":1043,"body":{"type":"state_changed","data":{"state":"recording","game":{"game_id":"valorant.exe","exe":"VALORANT-Win64-Shipping.exe","pid":18844,"title":null}}}}"#;
    assert_json_roundtrip::<Push>(s);
}

#[test]
fn example_k_worker_restarted_push_parses_negative_exit_code() {
    let s = r#"{"v":1,"seq":1047,"body":{"type":"worker_restarted","data":{"attempt":1,"reason":"access violation in encode","exit_code":-1073741819}}}"#;
    assert_json_roundtrip::<Push>(s);
    let p: Push = serde_json::from_str(s).unwrap();
    match p.body {
        Event::WorkerRestarted { exit_code, .. } => assert_eq!(exit_code, Some(-1073741819)),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn example_l_settings_reloaded_push_parses() {
    let s = r#"{"v":1,"seq":1050,"body":{"type":"settings_reloaded","data":{"ok":false,"errors":[{"key":"logging.level","message":"expected one of error|warn|info|debug|trace, got \"verbose\""}]}}}"#;
    assert_json_roundtrip::<Push>(s);
}

// ---------------------------------------------------------------------------
// envelope and tagging shape
// ---------------------------------------------------------------------------

#[test]
fn envelope_omits_id_when_none_and_seq_when_none() {
    let req: Request = Envelope::request(1, Command::Ping);
    let v = serde_json::to_value(&req).unwrap();
    assert!(v.get("id").is_some());
    assert!(v.get("seq").is_none(), "request must not carry seq: {v}");

    let ev: Push = Envelope::event(10, Event::Degraded { reason: "x".into() });
    let v = serde_json::to_value(&ev).unwrap();
    assert!(v.get("id").is_none(), "event must not carry id: {v}");
    assert!(v.get("seq").is_some());
}

#[test]
fn envelope_constructors_set_version_and_ids() {
    let r: Request = Envelope::request(77, Command::GetConfig);
    assert_eq!(r.v, PROTOCOL_VERSION);
    assert_eq!(r.id, Some(77));
    assert_eq!(r.seq, None);

    let rep: Response = Envelope::reply(78, Reply::Ok);
    assert_eq!(rep.v, PROTOCOL_VERSION);
    assert_eq!(rep.id, Some(78));
    assert_eq!(rep.seq, None);

    let ev: Push = Envelope::event(
        5,
        Event::ShuttingDown {
            reason: "stop".into(),
        },
    );
    assert_eq!(ev.v, PROTOCOL_VERSION);
    assert_eq!(ev.id, None);
    assert_eq!(ev.seq, Some(5));
}

#[test]
fn unit_commands_serialize_without_data_key() {
    let cases = [
        (Command::Ping, "ping"),
        (Command::GetStatus, "get_status"),
        (Command::StartSession, "start_session"),
        (Command::StopSession, "stop_session"),
        (Command::SplitSession, "split_session"),
        (Command::GetConfig, "get_config"),
        (Command::ReloadConfig, "reload_config"),
        (
            Command::Shutdown {
                reason: None,
                mode: None,
            },
            "shutdown",
        ),
    ];
    for (cmd, name) in cases {
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v, json!({"type": name}), "command {name}");
        assert!(v.get("data").is_none(), "command {name} has data key");
    }
}

#[test]
fn unit_reply_ok_serializes_as_type_ok_only() {
    assert_eq!(
        serde_json::to_value(&Reply::Ok).unwrap(),
        json!({"type": "ok"})
    );
}

#[test]
fn unit_reply_shutdown_ack_serializes_as_type_only() {
    assert_eq!(
        serde_json::to_value(&Reply::ShutdownAck).unwrap(),
        json!({"type": "shutdown_ack"})
    );
}

#[test]
fn shutdown_request_envelope_deserializes() {
    let req: Request = serde_json::from_str(r#"{"v":1,"id":3,"body":{"type":"shutdown"}}"#)
        .expect("shutdown request parses");
    assert_eq!(req.id, Some(3));
    assert_eq!(
        req.body,
        Command::Shutdown {
            reason: None,
            mode: None
        }
    );
}

#[test]
fn unknown_extra_fields_are_accepted() {
    let c: Command =
        serde_json::from_str(r#"{"type":"clip_now","data":{"seconds":5,"extra":true}}"#).unwrap();
    assert_eq!(c, Command::ClipNow { seconds: 5 });
}

#[test]
fn unknown_command_type_fails_to_deserialize() {
    assert!(serde_json::from_str::<Command>(r#"{"type":"nope"}"#).is_err());
}

#[test]
fn every_command_variant_roundtrips() {
    let cmds = vec![
        Command::Hello {
            proto_min: 1,
            proto_max: 1,
            client: "clipcheck-ui".into(),
            client_version: "0.1.0".into(),
        },
        Command::Ping,
        Command::GetStatus,
        Command::StartSession,
        Command::StopSession,
        Command::SplitSession,
        Command::ClipNow { seconds: 30 },
        Command::GetConfig,
        Command::Shutdown {
            reason: None,
            mode: None,
        },
        Command::PatchConfig {
            patch: json!({"logging": {"level": "debug"}}),
            dry_run: true,
        },
        Command::ReloadConfig,
        Command::RenameRecording {
            recording_id: "r1".into(),
            name: "n".into(),
        },
        Command::DeleteRecording {
            recording_id: "r1".into(),
        },
        Command::RegisterEditedRecording {
            source_recording_id: Some("r1".into()),
            path: r"C:\out\edit.mp4".into(),
            keep_source_time: false,
        },
        Command::RegisterEditedRecording {
            source_recording_id: None,
            path: r"C:\out\edit.mp4".into(),
            keep_source_time: false,
        },
        Command::Subscribe {
            topics: vec![Topic::Levels],
        },
        Command::Unsubscribe {
            topics: vec![Topic::Settings],
        },
    ];
    for c in cmds {
        let s = serde_json::to_string(&c).unwrap();
        let back: Command = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c, "command roundtrip failed for {s}");
    }
}

#[test]
fn every_reply_variant_roundtrips() {
    let replies = vec![
        Reply::HelloAck {
            proto: 1,
            daemon_version: "0.1.0".into(),
            daemon_pid: 4120,
            features: vec!["clip_save".into()],
        },
        Reply::Pong { unix_ms: -5 },
        Reply::Status(Box::new(sample_status())),
        Reply::Ok,
        Reply::ShutdownAck,
        Reply::ClipQueued {
            clip_id: "c1".into(),
        },
        Reply::Config {
            config: json!({"a": 1}),
            config_version: 1,
            path: r"C:\Users\someone\AppData\Roaming\ClipCheck\config.toml".into(),
        },
        Reply::ConfigRejected {
            diagnostics: vec![DiagnosticDto {
                code: "CFG-V001".into(),
                severity: DiagSeverity::Error,
                path: "replay.max_seconds".into(),
                message: "too short".into(),
            }],
        },
        Reply::RecordingRegistered {
            recording_id: "r1".into(),
        },
        Reply::Subscribed {
            topics: vec![Topic::State],
        },
        Reply::Unsubscribed {
            topics: vec![Topic::Clips],
        },
        Reply::Error {
            code: ErrorCode::Busy,
            message: "busy".into(),
            retryable: true,
        },
    ];
    for r in replies {
        let s = serde_json::to_string(&r).unwrap();
        let back: Reply = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r, "reply roundtrip failed for {s}");
    }
}

#[test]
fn every_event_variant_roundtrips() {
    let events = vec![
        Event::StateChanged {
            state: RecState::Armed,
            game: Some(sample_game()),
        },
        Event::GameDetected {
            game: sample_game(),
        },
        Event::GameLost {
            game_id: "valorant.exe".into(),
            reason: "exited".into(),
        },
        Event::SessionPartStarted {
            session_id: "s".into(),
            part: 1,
            path: "p".into(),
        },
        Event::SessionPartFinished {
            session_id: "s".into(),
            part: 1,
            path: "p".into(),
            duration_ms: 1000,
            duration_unknown: false,
            bytes: 2000,
            started_unix_ms: 1791510843000,
            parts_saved: 1,
            recovered: true,
        },
        Event::ClipSaved {
            clip_id: "c".into(),
            path: "p".into(),
            duration_ms: 30000,
            bytes: 123,
        },
        Event::CaptureHealth {
            fps: 59.5,
            dropped_frames: 3,
            black_frames_active: false,
        },
        Event::EncoderSelected {
            codec: "h264".into(),
            encoder: "nvenc_h264".into(),
        },
        Event::AudioLevels {
            tracks: vec![TrackLevel {
                track: 0,
                peak_db: -3.5,
                rms_db: -20.25,
            }],
        },
        Event::MicMuteChanged { muted: true },
        Event::DetectionPauseChanged { paused: false },
        Event::SettingsReloaded {
            ok: false,
            errors: vec![ConfigIssue {
                key: "logging.level".into(),
                message: "bad".into(),
            }],
        },
        Event::WorkerRestarted {
            attempt: 2,
            reason: "crash".into(),
            exit_code: None,
        },
        Event::Degraded {
            reason: "disk".into(),
        },
        Event::Warning {
            code: "W1".into(),
            message: "m".into(),
        },
        Event::ShuttingDown {
            reason: "user".into(),
        },
        Event::AppExit,
    ];
    for e in events {
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e, "event roundtrip failed for {s}");
    }
}

#[test]
fn topic_error_code_recstate_autostart_wire_names_are_snake_case() {
    assert_eq!(
        serde_json::to_value(Topic::Levels).unwrap(),
        json!("levels")
    );
    assert_eq!(serde_json::to_value(Topic::Clips).unwrap(), json!("clips"));
    assert_eq!(
        serde_json::to_value(Topic::Settings).unwrap(),
        json!("settings")
    );

    assert_eq!(
        serde_json::to_value(ErrorCode::FrameTooLarge).unwrap(),
        json!("frame_too_large")
    );
    assert_eq!(
        serde_json::to_value(ErrorCode::ProtoMismatch).unwrap(),
        json!("proto_mismatch")
    );
    assert_eq!(
        serde_json::to_value(ErrorCode::NotImplemented).unwrap(),
        json!("not_implemented")
    );
    assert_eq!(
        serde_json::to_value(ErrorCode::WorkerUnavailable).unwrap(),
        json!("worker_unavailable")
    );

    assert_eq!(
        serde_json::to_value(RecState::BufferOnly).unwrap(),
        json!("buffer_only")
    );
    assert_eq!(serde_json::to_value(RecState::Idle).unwrap(), json!("idle"));

    assert_eq!(
        serde_json::to_value(AutostartState::DisabledByWindows).unwrap(),
        json!("disabled_by_windows")
    );
    assert_eq!(
        serde_json::to_value(AutostartState::PathStale).unwrap(),
        json!("path_stale")
    );
}

#[test]
fn error_code_wire_name_decodes_back() {
    let c: ErrorCode = serde_json::from_str(r#""frame_too_large""#).unwrap();
    assert_eq!(c, ErrorCode::FrameTooLarge);
}

// ---------------------------------------------------------------------------
// Event::topic mapping
// ---------------------------------------------------------------------------

#[test]
fn event_topic_mapping_matches_spec() {
    let g = sample_game;
    assert_eq!(
        Event::StateChanged {
            state: RecState::Idle,
            game: None
        }
        .topic(),
        Some(Topic::State)
    );
    assert_eq!(
        Event::GameDetected { game: g() }.topic(),
        Some(Topic::State)
    );
    assert_eq!(
        Event::GameLost {
            game_id: "x".into(),
            reason: "r".into()
        }
        .topic(),
        Some(Topic::State)
    );
    assert_eq!(
        Event::Degraded { reason: "r".into() }.topic(),
        Some(Topic::State)
    );
    assert_eq!(
        Event::DetectionPauseChanged { paused: true }.topic(),
        Some(Topic::State)
    );
    assert_eq!(
        Event::MicMuteChanged { muted: true }.topic(),
        Some(Topic::State)
    );

    assert_eq!(
        Event::ClipSaved {
            clip_id: "c".into(),
            path: "p".into(),
            duration_ms: 1,
            bytes: 1
        }
        .topic(),
        Some(Topic::Clips)
    );

    assert_eq!(
        Event::SessionPartStarted {
            session_id: "s".into(),
            part: 1,
            path: "p".into()
        }
        .topic(),
        Some(Topic::Sessions)
    );
    assert_eq!(
        Event::SessionPartFinished {
            session_id: "s".into(),
            part: 1,
            path: "p".into(),
            duration_ms: 1,
            duration_unknown: false,
            bytes: 1,
            recovered: false,
            started_unix_ms: 1,
            parts_saved: 1,
        }
        .topic(),
        Some(Topic::Sessions)
    );

    assert_eq!(
        Event::CaptureHealth {
            fps: 60.0,
            dropped_frames: 0,
            black_frames_active: false
        }
        .topic(),
        Some(Topic::Health)
    );
    assert_eq!(
        Event::EncoderSelected {
            codec: "h264".into(),
            encoder: "e".into()
        }
        .topic(),
        Some(Topic::Health)
    );

    assert_eq!(
        Event::AudioLevels { tracks: vec![] }.topic(),
        Some(Topic::Levels)
    );

    assert_eq!(
        Event::WorkerRestarted {
            attempt: 1,
            reason: "r".into(),
            exit_code: None
        }
        .topic(),
        Some(Topic::Warnings)
    );
    assert_eq!(
        Event::Warning {
            code: "c".into(),
            message: "m".into()
        }
        .topic(),
        Some(Topic::Warnings)
    );

    assert_eq!(
        Event::SettingsReloaded {
            ok: true,
            errors: vec![]
        }
        .topic(),
        Some(Topic::Settings)
    );

    assert_eq!(
        Event::ShuttingDown { reason: "r".into() }.topic(),
        None,
        "ShuttingDown is always delivered and has no topic"
    );
    assert_eq!(
        Event::AppExit.topic(),
        None,
        "AppExit is always delivered and has no topic"
    );
    assert_eq!(
        serde_json::to_value(Event::AppExit).unwrap(),
        json!({"type": "app_exit"}),
        "AppExit wire name is app_exit"
    );
}

// ---------------------------------------------------------------------------
// negotiate
// ---------------------------------------------------------------------------

#[test]
fn negotiate_picks_highest_common_version() {
    assert_eq!(negotiate(1, 1, 1, 1), Some(1));
    assert_eq!(negotiate(1, 3, 2, 5), Some(3));
    assert_eq!(negotiate(1, 5, 2, 4), Some(4));
}

#[test]
fn negotiate_returns_none_when_ranges_do_not_overlap() {
    assert_eq!(negotiate(2, 2, 1, 1), None);
    assert_eq!(negotiate(3, 4, 1, 2), None);
    assert_eq!(negotiate(1, 1, 2, 3), None);
}

#[test]
fn negotiate_returns_none_for_inverted_client_range() {
    assert_eq!(negotiate(2, 1, 1, 5), None);
}

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

#[test]
fn protocol_and_transport_constants_have_spec_values() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(PROTOCOL_MIN_SUPPORTED, 1);
    assert_eq!(MAX_FRAME_BYTES, 1_048_576);
    assert_eq!(MAX_CLIENTS, 8);
    assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(3));
    assert_eq!(PIPE_NAME, r"\\.\pipe\clipcheck-ipc");
}

// ---------------------------------------------------------------------------
// discovery and meter commands, replies, and events
// ---------------------------------------------------------------------------

#[test]
fn discovery_commands_serialize_to_spec_shapes() {
    let cases = [
        (Command::ListAudioApps, "list_audio_apps"),
        (Command::ListAudioDevices, "list_audio_devices"),
        (Command::ListEncoders, "list_encoders"),
        (Command::ListRunningApps, "list_running_apps"),
    ];
    for (cmd, name) in cases {
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v, json!({"type": name}), "command {name}");
        assert!(v.get("data").is_none(), "command {name} has data key");
        let back: Command = serde_json::from_value(v).unwrap();
        assert_eq!(back, cmd, "command {name} did not round-trip");
    }
}

#[test]
fn mic_meter_command_shapes_and_roundtrip() {
    let on = Command::MicMeter {
        enable: true,
        device: None,
    };
    let v = serde_json::to_value(&on).unwrap();
    assert_eq!(
        v,
        json!({"type": "mic_meter", "data": {"enable": true, "device": null}})
    );
    assert_eq!(serde_json::from_value::<Command>(v).unwrap(), on);

    let off = Command::MicMeter {
        enable: false,
        device: Some("{x}".into()),
    };
    let v = serde_json::to_value(&off).unwrap();
    assert_eq!(
        v,
        json!({"type": "mic_meter", "data": {"enable": false, "device": "{x}"}})
    );
    assert_eq!(serde_json::from_value::<Command>(v).unwrap(), off);
}

#[test]
fn mic_meter_device_defaults_to_none_when_absent() {
    let c: Command =
        serde_json::from_str(r#"{"type":"mic_meter","data":{"enable":true}}"#).unwrap();
    assert_eq!(
        c,
        Command::MicMeter {
            enable: true,
            device: None
        }
    );
}

#[test]
fn subscribe_meter_topic_roundtrips() {
    let c = Command::Subscribe {
        topics: vec![Topic::Meter],
    };
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(
        v,
        json!({"type": "subscribe", "data": {"topics": ["meter"]}})
    );
    assert_eq!(serde_json::from_value::<Command>(v).unwrap(), c);
}

#[test]
fn topic_meter_wire_name_is_meter() {
    assert_eq!(serde_json::to_value(Topic::Meter).unwrap(), json!("meter"));
    let t: Topic = serde_json::from_value(json!("meter")).unwrap();
    assert_eq!(t, Topic::Meter);
}

#[test]
fn mic_level_event_roundtrips_and_maps_to_meter_topic() {
    let ev = Event::MicLevel {
        rms_db: -42.5,
        peak_db: -30.0,
    };
    assert_eq!(ev.topic(), Some(Topic::Meter));
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!(
        v,
        json!({"type": "mic_level", "data": {"rms_db": -42.5, "peak_db": -30.0}})
    );
    assert_eq!(serde_json::from_value::<Event>(v).unwrap(), ev);

    let push: Push = Envelope::event(77, ev.clone());
    let mut buf = Vec::new();
    write_frame(&mut buf, &push).unwrap();
    let back: Push = read_frame(&mut Cursor::new(buf)).unwrap();
    assert_eq!(back, push);
}

#[test]
fn audio_apps_reply_roundtrips_with_and_without_icon() {
    let with_icon = Reply::AudioApps {
        apps: vec![AudioAppDto {
            exe: "Discord.exe".into(),
            name: "Discord".into(),
            icon_png_b64: Some("iVBORw0KGgo=".into()),
            active: true,
            peak: 0.25,
            last_active_unix_ms: 1791510843000,
        }],
    };
    let v = serde_json::to_value(&with_icon).unwrap();
    assert_eq!(v["type"], json!("audio_apps"));
    assert_eq!(v["data"]["apps"][0]["icon_png_b64"], json!("iVBORw0KGgo="));
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), with_icon);

    let no_icon = Reply::AudioApps {
        apps: vec![AudioAppDto {
            exe: "Spotify.exe".into(),
            name: "Spotify".into(),
            icon_png_b64: None,
            active: false,
            peak: 0.0,
            last_active_unix_ms: 1,
        }],
    };
    let v = serde_json::to_value(&no_icon).unwrap();
    assert!(v["data"]["apps"][0]["icon_png_b64"].is_null(), "{v}");
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), no_icon);
}

#[test]
fn audio_devices_reply_roundtrips() {
    let r = Reply::AudioDevices {
        capture: vec![DeviceDto {
            id: "{0.0.1.00000000}.{abc}".into(),
            name: "Microphone (USB)".into(),
            is_default: true,
        }],
        render: vec![
            DeviceDto {
                id: "{0.0.0.00000000}.{def}".into(),
                name: "Speakers".into(),
                is_default: true,
            },
            DeviceDto {
                id: "{0.0.0.00000000}.{ghi}".into(),
                name: "Headphones".into(),
                is_default: false,
            },
        ],
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["type"], json!("audio_devices"));
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), r);
}

#[test]
fn encoders_reply_roundtrips() {
    let r = Reply::Encoders {
        encoder_ready: true,
        gpus: vec![GpuDto {
            name: "NVIDIA GeForce RTX 4070".into(),
            vendor: "nvidia".into(),
            index: 0,
            dedicated_mb: 12288,
        }],
        encoders: vec![EncoderDto {
            backend: "nvenc".into(),
            codec: "h264".into(),
            encoder_name: "h264_nvenc".into(),
            works: true,
        }],
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["type"], json!("encoders"));
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), r);

    let empty = Reply::Encoders {
        encoder_ready: false,
        gpus: Vec::new(),
        encoders: Vec::new(),
    };
    let v = serde_json::to_value(&empty).unwrap();
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), empty);
}

#[test]
fn running_apps_reply_roundtrips_with_and_without_icon() {
    let with_icon = Reply::RunningApps {
        apps: vec![RunningAppDto {
            exe: "VALORANT-Win64-Shipping.exe".into(),
            name: "VALORANT".into(),
            icon_png_b64: Some("iVBORw0KGgo=".into()),
            pid: 18844,
            is_game: true,
            in_allow_list: false,
            suggested: true,
        }],
    };
    let v = serde_json::to_value(&with_icon).unwrap();
    assert_eq!(v["type"], json!("running_apps"));
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), with_icon);

    let no_icon = Reply::RunningApps {
        apps: vec![RunningAppDto {
            exe: "notepad.exe".into(),
            name: "Untitled - Notepad".into(),
            icon_png_b64: None,
            pid: 4321,
            is_game: false,
            in_allow_list: false,
            suggested: false,
        }],
    };
    let v = serde_json::to_value(&no_icon).unwrap();
    assert!(v["data"]["apps"][0]["icon_png_b64"].is_null(), "{v}");
    assert_eq!(serde_json::from_value::<Reply>(v).unwrap(), no_icon);
}

#[test]
fn running_app_without_icon_key_deserializes_as_none() {
    let dto: RunningAppDto = serde_json::from_value(json!({
        "exe": "game.exe",
        "name": "Game",
        "pid": 7,
        "is_game": true,
        "in_allow_list": false,
        "suggested": true
    }))
    .expect("icon_png_b64 key is optional");
    assert_eq!(dto.icon_png_b64, None);
}

#[test]
fn audio_app_without_icon_key_deserializes_as_none() {
    let dto: AudioAppDto = serde_json::from_value(json!({
        "exe": "Discord.exe",
        "name": "Discord",
        "active": false,
        "peak": 0.0,
        "last_active_unix_ms": 5
    }))
    .expect("icon_png_b64 key is optional");
    assert_eq!(dto.icon_png_b64, None);
}

#[test]
fn mic_test_command_shapes_and_roundtrip() {
    let none = Command::MicTest { device: None };
    let v = serde_json::to_value(&none).unwrap();
    assert_eq!(v, json!({"type": "mic_test", "data": {"device": null}}));
    assert_eq!(serde_json::from_value::<Command>(v).unwrap(), none);

    let some = Command::MicTest {
        device: Some("abc".into()),
    };
    let v = serde_json::to_value(&some).unwrap();
    assert_eq!(v, json!({"type": "mic_test", "data": {"device": "abc"}}));
    assert_eq!(serde_json::from_value::<Command>(v).unwrap(), some);
}

#[test]
fn mic_test_command_device_defaults_to_none_when_absent() {
    let c: Command = serde_json::from_str(r#"{"type":"mic_test","data":{}}"#).unwrap();
    assert_eq!(c, Command::MicTest { device: None });
}

/// The exact JSON the UI sends (apps/ui/src/lib/discovery.ts). Commands with fields need the
/// `data` envelope; a flat `{"type":"mic_meter","enable":true}` is rejected with "missing field `data`".
#[test]
fn ui_sent_meter_and_mic_test_requests_parse() {
    let meter: Command =
        serde_json::from_str(r#"{"type":"mic_meter","data":{"enable":true,"device":null}}"#)
            .unwrap();
    assert_eq!(
        meter,
        Command::MicMeter {
            enable: true,
            device: None
        }
    );
    let test: Command =
        serde_json::from_str(r#"{"type":"mic_test","data":{"device":"{abc}"}}"#).unwrap();
    assert_eq!(
        test,
        Command::MicTest {
            device: Some("{abc}".into())
        }
    );
}

#[test]
fn mic_test_reply_wire_shape_and_roundtrip() {
    let r = Reply::MicTest {
        wav_base64: "UklGRg==".into(),
        duration_ms: 5000,
        peak_db: -12.5,
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v.get("type"), Some(&json!("mic_test")));
    let data = v
        .get("data")
        .and_then(Value::as_object)
        .expect("data object");
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["duration_ms", "peak_db", "wav_base64"]);
    assert_eq!(data["wav_base64"], json!("UklGRg=="));
    assert_eq!(data["duration_ms"], json!(5000));
    assert_eq!(data["peak_db"], json!(-12.5));

    match serde_json::from_value::<Reply>(v).unwrap() {
        Reply::MicTest {
            wav_base64,
            duration_ms,
            peak_db,
        } => {
            assert_eq!(wav_base64, "UklGRg==");
            assert_eq!(duration_ms, 5000);
            assert_eq!(peak_db, -12.5);
        }
        other => panic!("expected MicTest, got {other:?}"),
    }
}

#[test]
fn mic_test_reply_full_clip_frame_fits_max_frame_bytes() {
    // 5 s mono 16-bit WAV = 44 + 2 * 240000 bytes; base64 with padding = 4 * ceil(n / 3).
    let wav_len = 44 + 2 * 240_000usize;
    let b64_len = 4 * wav_len.div_ceil(3);
    assert_eq!(b64_len, 640_060, "base64 length of a 5 s clip");

    let resp: Response = Envelope::reply(
        7,
        Reply::MicTest {
            wav_base64: "A".repeat(b64_len),
            duration_ms: 5000,
            peak_db: -3.0,
        },
    );
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &resp).expect("a 5 s clip must fit in one frame");
    let body_len = buf.len() - 4;
    assert!(
        body_len < MAX_FRAME_BYTES,
        "frame body {body_len} bytes is not below MAX_FRAME_BYTES {MAX_FRAME_BYTES}"
    );
    assert!(body_len < 1 << 20, "literal 1 MiB check");
}
