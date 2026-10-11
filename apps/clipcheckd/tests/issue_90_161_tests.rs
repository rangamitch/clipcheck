//! Tests for issue #90 (classification of unparseable requests and the hello version message),
//! issue #161 (RenameRecording, DeleteRecording and RegisterEditedRecording are handled, not
//! `not_implemented`) and the per-connection replay cache from #51.
//!
//! Each daemon uses a temp data dir and a unique pipe, and is shut down over IPC at the end of
//! the test (or killed on drop if an assertion fails first). Every test runs under a watchdog.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{Command as Cmd, ErrorCode, Reply, Response};
use clipcheck_ipc::PipeConn;
use serde_json::{json, Value};

mod common;
use common::*;

const WATCHDOG: Duration = Duration::from_secs(60);
const CLEANUP_ID: u64 = 999_999;

/// Runs `body` on a worker thread and fails if it does not finish within `WATCHDOG`.
fn watchdog(name: &str, body: impl FnOnce() + Send + 'static) {
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        body();
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(WATCHDOG) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
        Err(RecvTimeoutError::Timeout) => panic!("{name}: exceeded the {WATCHDOG:?} watchdog"),
    }
}

fn spawn(data_dir: &Path, pipe: &str) -> Daemon {
    spawn_with(data_dir, pipe, &["--no-encoders"])
}

fn hello_body() -> Value {
    json!({
        "type": "hello",
        "data": {"proto_min": 1, "proto_max": 1, "client": "test", "client_version": "0.0.0"}
    })
}

/// Writes a raw JSON frame and reads one reply.
fn raw_call(conn: &mut PipeConn, frame: Value) -> Response {
    write_frame(conn, &frame).expect("write raw frame");
    read_frame(conn).expect("read reply frame")
}

/// Connects and completes a handshake with a raw Hello frame (id 1).
fn connect_raw(d: &mut Daemon, pipe: &str) -> PipeConn {
    let mut conn = connect(d, pipe);
    let resp = raw_call(&mut conn, json!({"v": 1, "id": 1, "body": hello_body()}));
    match resp.body {
        Reply::HelloAck { proto, .. } => assert_eq!(proto, 1),
        other => panic!("expected HelloAck, got {other:?}"),
    }
    conn
}

fn error_of(resp: &Response) -> (ErrorCode, String) {
    match &resp.body {
        Reply::Error { code, message, .. } => (*code, message.clone()),
        other => panic!("expected Error, got {other:?}"),
    }
}

fn expect_code(resp: &Response, want: ErrorCode, what: &str) {
    let (code, message) = error_of(resp);
    assert_ne!(
        code,
        ErrorCode::NotImplemented,
        "{what}: still not_implemented"
    );
    assert_eq!(code, want, "{what}: wrong code, message was {message:?}");
}

fn shutdown_raw(d: &mut Daemon, conn: &mut PipeConn) {
    match raw_call(
        conn,
        json!({"v": 1, "id": CLEANUP_ID, "body": {"type": "shutdown", "data": {"reason": null}}}),
    )
    .body
    {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(d, Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

// ---------------------------------------------------------------------------
// #90: classification of unparseable requests.
// ---------------------------------------------------------------------------

#[test]
fn issue90_known_type_with_bad_body_is_bad_request() {
    watchdog("issue90 known type bad body", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_raw(&mut d, &pipe);

        // rename_recording needs string fields; a number for recording_id is malformed.
        let r1 = raw_call(
            &mut conn,
            json!({"v": 1, "id": 2, "body": {"type": "rename_recording", "data": {"recording_id": 5}}}),
        );
        assert_eq!(r1.id, Some(2));
        expect_code(&r1, ErrorCode::BadRequest, "rename_recording with bad body");

        // clip_now with a string for seconds is a known type with a malformed body.
        let r2 = raw_call(
            &mut conn,
            json!({"v": 1, "id": 3, "body": {"type": "clip_now", "data": {"seconds": "thirty"}}}),
        );
        assert_eq!(r2.id, Some(3));
        expect_code(&r2, ErrorCode::BadRequest, "clip_now with string seconds");

        shutdown_raw(&mut d, &mut conn);
    });
}

#[test]
fn issue90_unknown_type_is_unknown_type() {
    watchdog("issue90 unknown type", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_raw(&mut d, &pipe);

        let r = raw_call(
            &mut conn,
            json!({"v": 1, "id": 2, "body": {"type": "no_such_command", "data": {}}}),
        );
        assert_eq!(r.id, Some(2));
        expect_code(&r, ErrorCode::UnknownType, "unknown type name");

        shutdown_raw(&mut d, &mut conn);
    });
}

#[test]
fn issue90_missing_or_non_string_type_is_bad_request() {
    watchdog("issue90 missing type", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_raw(&mut d, &pipe);

        let no_type = raw_call(&mut conn, json!({"v": 1, "id": 2, "body": {"data": {}}}));
        assert_eq!(no_type.id, Some(2));
        expect_code(&no_type, ErrorCode::BadRequest, "body without type");

        let numeric_type = raw_call(
            &mut conn,
            json!({"v": 1, "id": 3, "body": {"type": 7, "data": {}}}),
        );
        assert_eq!(numeric_type.id, Some(3));
        expect_code(&numeric_type, ErrorCode::BadRequest, "non-string type");

        shutdown_raw(&mut d, &mut conn);
    });
}

#[test]
fn issue90_unknown_first_frame_type_is_unknown_type() {
    watchdog("issue90 first frame unknown", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);

        // The first frame must be hello. An unknown type name on that frame is unknown_type.
        let mut conn = connect(&mut d, &pipe);
        let r = raw_call(
            &mut conn,
            json!({"v": 1, "id": 1, "body": {"type": "no_such_command", "data": {}}}),
        );
        expect_code(&r, ErrorCode::UnknownType, "unknown first frame type");
        drop(conn);

        // A known non-hello first frame stays unauthorized.
        let mut conn2 = connect(&mut d, &pipe);
        let r2 = raw_call(
            &mut conn2,
            json!({"v": 1, "id": 1, "body": {"type": "ping"}}),
        );
        let (code, _) = error_of(&r2);
        assert_eq!(code, ErrorCode::Unauthorized, "non-hello first frame");
        drop(conn2);

        // The daemon still accepts a proper handshake and shuts down cleanly.
        let mut conn3 = connect_raw(&mut d, &pipe);
        shutdown_raw(&mut d, &mut conn3);
    });
}

#[test]
fn issue90_hello_version_error_names_protocol_version() {
    watchdog("issue90 hello version text", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);

        // Hello with a frame version other than the protocol version (1).
        let mut conn = connect(&mut d, &pipe);
        let r = raw_call(&mut conn, json!({"v": 2, "id": 1, "body": hello_body()}));
        let (code, message) = error_of(&r);
        assert_eq!(code, ErrorCode::ProtoMismatch, "hello with frame version 2");
        assert!(
            message.contains("frame version 1"),
            "hello error text should name protocol version 1, got {message:?}"
        );
        drop(conn);

        let mut conn2 = connect_raw(&mut d, &pipe);
        shutdown_raw(&mut d, &mut conn2);
    });
}

// ---------------------------------------------------------------------------
// #161: rename, delete and register-edited are dispatched to the recordings handlers.
// ---------------------------------------------------------------------------

#[test]
fn issue161_rename_and_delete_unknown_id_return_error_not_not_implemented() {
    watchdog("issue161 unknown id", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut client = Client::handshake(&mut d, &pipe);

        let rename = client.call(Cmd::RenameRecording {
            recording_id: "does-not-exist".into(),
            name: "new name".into(),
        });
        match &rename {
            Reply::Error { code, .. } => {
                assert_ne!(
                    *code,
                    ErrorCode::NotImplemented,
                    "rename still not_implemented"
                );
                assert_eq!(
                    *code,
                    ErrorCode::BadRequest,
                    "rename unknown id: {rename:?}"
                );
            }
            other => panic!("rename unknown id: expected Error, got {other:?}"),
        }

        let delete = client.call(Cmd::DeleteRecording {
            recording_id: "does-not-exist".into(),
        });
        match &delete {
            Reply::Error { code, .. } => {
                assert_ne!(
                    *code,
                    ErrorCode::NotImplemented,
                    "delete still not_implemented"
                );
                assert_eq!(
                    *code,
                    ErrorCode::BadRequest,
                    "delete unknown id: {delete:?}"
                );
            }
            other => panic!("delete unknown id: expected Error, got {other:?}"),
        }

        assert!(matches!(
            client.call(Cmd::Shutdown {
                reason: None,
                mode: None
            }),
            Reply::ShutdownAck
        ));
        let status = wait_exit(&mut d, Duration::from_secs(10));
        assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
    });
}

#[test]
fn issue161_register_edited_bogus_path_returns_error_not_not_implemented() {
    watchdog("issue161 register bogus path", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut client = Client::handshake(&mut d, &pipe);

        let missing = dir
            .path()
            .join("missing-edit.mp4")
            .to_string_lossy()
            .into_owned();
        for (label, path) in [
            ("relative path", "clip.mp4".to_string()),
            ("absolute missing file", missing),
        ] {
            let reply = client.call(Cmd::RegisterEditedRecording {
                source_recording_id: None,
                path,
                keep_source_time: false,
            });
            match &reply {
                Reply::Error { code, .. } => {
                    assert_ne!(
                        *code,
                        ErrorCode::NotImplemented,
                        "{label}: still not_implemented"
                    );
                    assert_eq!(*code, ErrorCode::BadRequest, "{label}: {reply:?}");
                }
                other => panic!("{label}: expected Error, got {other:?}"),
            }
        }

        assert!(matches!(
            client.call(Cmd::Shutdown {
                reason: None,
                mode: None
            }),
            Reply::ShutdownAck
        ));
        let status = wait_exit(&mut d, Duration::from_secs(10));
        assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
    });
}

// ---------------------------------------------------------------------------
// #51: a replayed request id on one connection returns the identical reply.
// ---------------------------------------------------------------------------

#[test]
fn issue51_replayed_stop_session_returns_identical_reply() {
    watchdog("issue51 stop_session replay", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_raw(&mut d, &pipe);

        // No session is active, so StopSession answers an error. A non-retryable error is
        // remembered, so the same id with the same command returns the identical reply.
        let frame = json!({"v": 1, "id": 7, "body": {"type": "stop_session"}});
        let first = raw_call(&mut conn, frame.clone());
        let second = raw_call(&mut conn, frame);
        assert_eq!(first.id, Some(7));
        assert_eq!(second.id, Some(7));
        eprintln!(
            "REPORT issue51 stop_session x2: first={:?} second={:?}",
            first.body, second.body
        );
        assert_eq!(
            second.body, first.body,
            "replayed reply differs from the first reply"
        );

        shutdown_raw(&mut d, &mut conn);
    });
}

#[test]
fn issue51_replayed_clip_now_returns_identical_reply() {
    watchdog("issue51 clip_now replay", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_raw(&mut d, &pipe);

        // Without a recording game, ClipNow answers an error. The same id must replay it.
        let frame = json!({"v": 1, "id": 8, "body": {"type": "clip_now", "data": {"seconds": 30}}});
        let first = raw_call(&mut conn, frame.clone());
        let second = raw_call(&mut conn, frame);
        eprintln!(
            "REPORT issue51 clip_now x2: first={:?} second={:?}",
            first.body, second.body
        );
        assert_eq!(
            second.body, first.body,
            "replayed clip_now reply differs from the first"
        );

        shutdown_raw(&mut d, &mut conn);
    });
}
