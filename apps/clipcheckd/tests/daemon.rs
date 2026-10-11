//! Integration tests that spawn the real `clipcheckd` binary and talk to it over the named pipe.
//!
//! The single-instance mutex `Local\ClipCheck.Recorder` is session-global, so every test takes
//! `TEST_LOCK` to keep daemons from running at the same time. Each daemon is wrapped in a guard
//! that kills and reaps it on drop, releasing the mutex before the next test starts.

use std::fs;
use std::time::Duration;

use clipcheck_core::config::schema::Config;
use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{
    Command as Cmd, Envelope, ErrorCode, RecState, Reply, Request, Response, Topic,
};
use clipcheck_ipc::PipeConn;
use serde_json::json;

mod common;
use common::*;

fn send(conn: &mut PipeConn, req: Request) {
    write_frame(conn, &req).expect("write request frame");
}

fn assert_pong(resp: Response) {
    match resp.body {
        Reply::Pong { .. } => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}

fn assert_error(resp: &Response, code: ErrorCode) -> (bool, String) {
    match &resp.body {
        Reply::Error {
            code: got,
            message,
            retryable,
        } => {
            assert_eq!(*got, code, "wrong error code, message was {message:?}");
            (*retryable, message.clone())
        }
        other => panic!("expected Error({code:?}), got {other:?}"),
    }
}

#[test]
fn handshake_ping_status_config() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let pid = d.child.id();
    let mut conn = connect(&mut d, &pipe);

    let resp = call(&mut conn, 1, hello_cmd());
    match resp.body {
        Reply::HelloAck {
            proto, daemon_pid, ..
        } => {
            assert_eq!(proto, 1);
            assert_eq!(daemon_pid, pid, "daemon_pid does not match child pid");
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }

    match call(&mut conn, 2, Cmd::Ping).body {
        Reply::Pong { unix_ms } => assert!(
            unix_ms > 1_600_000_000_000,
            "unix_ms looks wrong: {unix_ms}"
        ),
        other => panic!("expected Pong, got {other:?}"),
    }

    match call(&mut conn, 3, Cmd::GetStatus).body {
        Reply::Status(status) => {
            assert_eq!(status.state, RecState::Idle);
            assert!(
                status.uptime_s < 60,
                "uptime_s too large: {}",
                status.uptime_s
            );
        }
        other => panic!("expected Status, got {other:?}"),
    }

    match call(&mut conn, 4, Cmd::GetConfig).body {
        Reply::Config { config, .. } => {
            assert_eq!(
                config.get("config_version").and_then(|v| v.as_u64()),
                Some(1),
                "config_version missing or not 1: {config}"
            );
            serde_json::from_value::<Config>(config.clone())
                .unwrap_or_else(|e| panic!("config does not deserialize to Config: {e}\n{config}"));
        }
        other => panic!("expected Config, got {other:?}"),
    }

    assert!(
        dir.path().join("config.toml").is_file(),
        "config.toml not created"
    );
    assert!(
        dir.path().join("library.db").is_file(),
        "library.db not created"
    );
    assert!(
        dir.path().join("logs").join("clipcheckd.log").is_file(),
        "logs/clipcheckd.log not created"
    );
}

#[test]
fn second_instance_exits_3() {
    let _g = lock();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let pipe_a = unique_pipe();
    let pipe_b = unique_pipe();

    let mut a = spawn(dir_a.path(), &pipe_a);
    let mut conn_a = connect_and_hello(&mut a, &pipe_a);

    // Single instance is per DATA FOLDER: the second daemon uses the same folder (other pipe).
    let mut b = spawn(dir_a.path(), &pipe_b);
    let status = wait_exit(&mut b, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(3),
        "second instance exit status was {status:?}"
    );

    assert_pong(call(&mut conn_a, 2, Cmd::Ping));
}

#[test]
fn second_instance_same_pipe_exits_3() {
    let _g = lock();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let pipe = unique_pipe();

    let mut a = spawn(dir_a.path(), &pipe);
    let mut conn_a = connect_and_hello(&mut a, &pipe);

    let mut b = spawn(dir_a.path(), &pipe);
    let status = wait_exit(&mut b, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(3),
        "second instance exit status was {status:?}"
    );

    assert_pong(call(&mut conn_a, 2, Cmd::Ping));
}

/// Written by `app::load_config` when validation or parsing leaves no usable config.
const CORRUPT_CONFIG_LOG: &str = "config has errors, using defaults";

#[test]
fn corrupt_config_does_not_crash() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    let garbage = "this is = = not toml [[[\n";
    fs::write(&config_path, garbage).expect("write garbage config");

    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);
    assert_pong(call(&mut conn, 2, Cmd::Ping));

    // The loader logs this exact line once the corrupt file has been rejected. Waiting for it is
    // the signal that the load finished; the negative checks below run after it.
    assert!(
        wait_for_log(dir.path(), CORRUPT_CONFIG_LOG, Duration::from_secs(10)),
        "daemon never logged the rejected config; log:\n{}",
        log_text(dir.path())
    );
    assert!(
        d.child.try_wait().expect("try_wait failed").is_none(),
        "daemon exited after corrupt config was loaded"
    );

    let on_disk = fs::read_to_string(&config_path).expect("read config.toml");
    assert_eq!(on_disk, garbage, "corrupt config.toml was overwritten");
}

#[test]
fn hello_required_first() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect(&mut d, &pipe);

    let resp = call(&mut conn, 1, Cmd::Ping);
    assert_error(&resp, ErrorCode::Unauthorized);

    let next = read_frame::<_, Response>(&mut conn);
    assert!(
        next.is_err(),
        "connection still open after unauthorized Ping: {next:?}"
    );
}

#[test]
fn subscribe_and_unsubscribe_ack() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(
        &mut conn,
        2,
        Cmd::Subscribe {
            topics: vec![Topic::State],
        },
    )
    .body
    {
        Reply::Subscribed { topics } => assert_eq!(topics, vec![Topic::State]),
        other => panic!("expected Subscribed, got {other:?}"),
    }

    match call(
        &mut conn,
        3,
        Cmd::Unsubscribe {
            topics: vec![Topic::State],
        },
    )
    .body
    {
        Reply::Unsubscribed { topics } => assert_eq!(topics, vec![Topic::State]),
        other => panic!("expected Unsubscribed, got {other:?}"),
    }
}

#[test]
fn unimplemented_commands_error() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    // An unknown key is refused as a bad request; it must not be retryable.
    let resp = call(
        &mut conn,
        2,
        Cmd::PatchConfig {
            patch: json!({"replay": {"no_such_key": 1}}),
            dry_run: false,
        },
    );
    let (retryable, _) = assert_error(&resp, ErrorCode::BadRequest);
    assert!(!retryable, "PatchConfig error should not be retryable");

    // StartSession is a real command now: with no game attached it must fail with a
    // proper error (never NotImplemented, never a hang), and the daemon must stay up.
    let resp = call(&mut conn, 3, Cmd::StartSession);
    match &resp.body {
        Reply::Error { code, .. } => assert_ne!(
            *code,
            ErrorCode::NotImplemented,
            "StartSession is implemented; it must not report not_implemented"
        ),
        other => panic!("expected an error with no game attached, got {other:?}"),
    }

    assert_pong(call(&mut conn, 4, Cmd::Ping));
}

#[test]
fn shutdown_command_exits_zero_within_3s() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    match call(
        &mut conn,
        2,
        Cmd::Shutdown {
            reason: None,
            mode: None,
        },
    )
    .body
    {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }

    let status = wait_exit(&mut d, Duration::from_secs(3));
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

#[test]
fn unknown_command_type() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    write_frame(
        &mut conn,
        &json!({"v": 1, "id": 9, "body": {"type": "no_such_command"}}),
    )
    .expect("write raw frame");
    let resp = recv_frame(&mut conn);
    assert_error(&resp, ErrorCode::UnknownType);
    assert_eq!(resp.id, Some(9), "reply id mismatch: {resp:?}");
}

#[test]
fn version_mismatch_is_reported() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect_and_hello(&mut d, &pipe);

    send(
        &mut conn,
        Envelope {
            v: 2,
            id: Some(7),
            seq: None,
            body: Cmd::Ping,
        },
    );
    let resp = recv_frame(&mut conn);
    assert_error(&resp, ErrorCode::ProtoMismatch);

    assert_pong(call(&mut conn, 8, Cmd::Ping));
}

#[test]
fn hello_version_mismatch_rejected() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut conn = connect(&mut d, &pipe);

    let resp = call(
        &mut conn,
        1,
        Cmd::Hello {
            proto_min: 5,
            proto_max: 6,
            client: "test".into(),
            client_version: "0.0.0".into(),
        },
    );
    assert_error(&resp, ErrorCode::ProtoMismatch);
}

/// Single instance is per data folder, so unrelated folders (a second profile, another test, a
/// packaged copy running in the default folder) never block each other.
#[test]
fn different_data_dirs_run_side_by_side() {
    let _g = lock();
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let pipe_a = unique_pipe();
    let pipe_b = unique_pipe();

    let mut a = spawn(dir_a.path(), &pipe_a);
    let mut conn_a = connect_and_hello(&mut a, &pipe_a);
    let mut b = spawn(dir_b.path(), &pipe_b);
    let mut conn_b = connect_and_hello(&mut b, &pipe_b);

    assert_pong(call(&mut conn_a, 2, Cmd::Ping));
    assert_pong(call(&mut conn_b, 2, Cmd::Ping));
}
