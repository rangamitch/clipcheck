//! Tests for `clipcheck_ui_lib::ipc::DaemonClient`.
//!
//! Most tests spawn the built `clipcheck-ui` binary with `--daemon` and talk to it through
//! the real `DaemonClient`. The timeout test uses a raw `PipeServer` that answers only the
//! handshake. The daemon's single-instance mutex is session-global, so every test takes
//! `TEST_LOCK`. Each daemon is wrapped in a guard that shuts it down and kills it on drop.

mod common;

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{Command as Cmd, Envelope, Reply};
use clipcheck_ipc::PipeServer;
use clipcheck_ui_lib::ipc::{timeout_for, ClientError};
use common::{
    body_type, conn_log_contains, connected_count, start_client, unique_pipe, wait_connected,
    wait_connected_after, wait_until, Daemon, CONNECT_WAIT, TEST_LOCK,
};
use serde_json::{json, Value};

/// Request timeout used by most tests.
const REQ_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Unit tests (no processes)
// ---------------------------------------------------------------------------

#[test]
fn timeout_for_uses_long_timeout_only_for_config_writes() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        timeout_for(&json!({"type": "get_status"})),
        Duration::from_secs(5)
    );
    assert_eq!(
        timeout_for(&json!({"type": "ping"})),
        Duration::from_secs(5)
    );
    assert_eq!(
        timeout_for(&json!({"type": "patch_config", "data": {}})),
        Duration::from_secs(30)
    );
    assert_eq!(
        timeout_for(&json!({"type": "reload_config"})),
        Duration::from_secs(30)
    );
    assert_eq!(timeout_for(&json!({})), Duration::from_secs(5));
}

#[test]
fn client_error_display_strings_are_non_empty() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for e in [
        ClientError::Disconnected,
        ClientError::Timeout,
        ClientError::InvalidCommand("bad".into()),
    ] {
        assert!(!e.to_string().is_empty(), "empty Display for {e:?}");
    }
}

// ---------------------------------------------------------------------------
// Integration tests (real daemon)
// ---------------------------------------------------------------------------

#[test]
fn connects_and_fires_on_connection_true() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);

    wait_connected(&mut d, &conns);
    assert!(
        client.is_connected(),
        "is_connected() is false after on_connection(true)"
    );

    client.stop();
}

#[test]
fn get_status_returns_status_body() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let body = client
        .request(json!({"type": "get_status"}), REQ_TIMEOUT)
        .expect("get_status request failed");
    assert_eq!(body_type(&body), "status", "body was {body}");

    client.stop();
}

#[test]
fn get_config_returns_config_body() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let body = client
        .request(json!({"type": "get_config"}), REQ_TIMEOUT)
        .expect("get_config request failed");
    assert_eq!(body_type(&body), "config", "body was {body}");
    assert!(
        body.get("data").and_then(|d| d.get("config")).is_some(),
        "config body has no data.config: {body}"
    );

    client.stop();
}

#[test]
fn dry_run_patch_returns_config_checked() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let cmd = json!({"type": "patch_config", "data": {
        "patch": {"replay": {"max_seconds": 120}},
        "dry_run": true
    }});
    let body = client
        .request(cmd, timeout_for(&json!({"type": "patch_config"})))
        .expect("dry-run patch request failed");
    assert_eq!(body_type(&body), "config_checked", "body was {body}");

    client.stop();
}

#[test]
fn invalid_patch_reply_is_reported_as_body_not_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let cmd = json!({"type": "patch_config", "data": {
        "patch": {"no_such_key": 1},
        "dry_run": true
    }});
    let result = client.request(cmd, Duration::from_secs(30));
    let body = result.expect("invalid patch should come back as Ok(body), not Err");
    // The daemon answers an unknown key with a Reply::Error (bad_request), not with
    // ConfigRejected. The client must pass that through as Ok(body).
    let t = body_type(&body);
    assert_eq!(
        t, "error",
        "daemon returned {t:?} for unknown key (body {body})"
    );
    assert_eq!(
        body.get("data")
            .and_then(|d| d.get("code"))
            .and_then(Value::as_str),
        Some("bad_request"),
        "error code for unknown key was not bad_request: {body}"
    );

    client.stop();
}

#[test]
fn real_patch_pushes_config_changed_event() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let cmd = json!({"type": "patch_config", "data": {
        "patch": {"replay": {"max_seconds": 120}},
        "dry_run": false
    }});
    let body = client
        .request(cmd, timeout_for(&json!({"type": "patch_config"})))
        .expect("real patch request failed");
    assert_ne!(body_type(&body), "error", "patch returned error: {body}");

    let found = wait_until(Duration::from_secs(3), || {
        events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|e| body_type(e) == "config_changed")
    });
    assert!(
        found,
        "no config_changed event within 3 s; events: {:?}",
        events.lock().unwrap_or_else(|e| e.into_inner())
    );
    let evs = events.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let ev = evs
        .iter()
        .find(|e| body_type(e) == "config_changed")
        .expect("config_changed present");
    assert!(ev.get("seq").is_some(), "event has no seq key: {ev}");

    client.stop();
}

#[test]
fn concurrent_requests_get_matching_replies() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let client_ref = &client;
    let problems: Vec<String> = thread::scope(|s| {
        let handles: Vec<_> = (0..8usize)
            .map(|t| {
                s.spawn(move || {
                    let mut problems = Vec::new();
                    for j in 0..50usize {
                        let (cmd, want) = if (t + j) % 2 == 0 {
                            (json!({"type": "ping"}), "pong")
                        } else {
                            (json!({"type": "get_status"}), "status")
                        };
                        match client_ref.request(cmd, REQ_TIMEOUT) {
                            Ok(body) => {
                                if body_type(&body) != want {
                                    problems.push(format!(
                                        "thread {t} req {j}: wanted {want}, got {body}"
                                    ));
                                }
                            }
                            Err(e) => problems.push(format!("thread {t} req {j}: {e}")),
                        }
                    }
                    problems
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker thread panicked"))
            .collect()
    });
    assert!(
        problems.is_empty(),
        "{} of 400 requests failed or got cross-talk, first: {:?}",
        problems.len(),
        problems.first()
    );

    client.stop();
}

#[test]
fn kill_daemon_reports_disconnect_and_requests_fail_fast() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    d.kill();
    let t0 = Instant::now();
    let saw_false = wait_until(Duration::from_secs(2), || conn_log_contains(&conns, false));
    assert!(
        saw_false,
        "on_connection(false) did not fire within 2 s of killing the daemon"
    );
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(
        !client.is_connected(),
        "is_connected() still true after loss"
    );

    let res = client.request(json!({"type": "get_status"}), REQ_TIMEOUT);
    assert_eq!(res, Err(ClientError::Disconnected), "request after kill");

    client.stop();
}

#[test]
fn reconnects_to_new_daemon_on_same_pipe() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut first = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut first, &conns);

    let connects_before = connected_count(&conns);
    first.kill();
    assert!(
        wait_until(Duration::from_secs(2), || conn_log_contains(&conns, false)),
        "on_connection(false) did not fire after kill"
    );

    // Declared after the client, so the new daemon is shut down before the client stops.
    let mut second = Daemon::spawn(dir.path(), &pipe);
    wait_connected_after(&mut second, &conns, connects_before);
    let body = client
        .request(json!({"type": "get_status"}), REQ_TIMEOUT)
        .expect("get_status after reconnect failed");
    assert_eq!(body_type(&body), "status", "body was {body}");

    client.stop();
}

#[test]
fn request_times_out_against_silent_peer() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pipe = unique_pipe("clipcheck-ui-client-test");

    // Handshake-only fake daemon. After Hello and Subscribe it reads, but never answers,
    // any other command. It keeps the connection open until `release` is dropped.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let server_pipe = pipe.clone();
    let server = thread::spawn(move || {
        let mut server = PipeServer::bind(&server_pipe).expect("bind fake daemon pipe");
        let mut conn = server.accept().expect("accept fake daemon connection");
        while let Ok(frame) = read_frame::<_, Envelope<Cmd>>(&mut conn) {
            let id = frame.id.unwrap_or(0);
            match frame.body {
                Cmd::Hello { .. } => {
                    let reply = Envelope::reply(
                        id,
                        Reply::HelloAck {
                            proto: 1,
                            daemon_version: "t".into(),
                            daemon_pid: 0,
                            features: vec![],
                        },
                    );
                    if write_frame(&mut conn, &reply).is_err() {
                        break;
                    }
                }
                Cmd::Subscribe { .. } => {
                    let reply = Envelope::reply(id, Reply::Subscribed { topics: vec![] });
                    if write_frame(&mut conn, &reply).is_err() {
                        break;
                    }
                }
                _ => {
                    // Never reply. Keep the connection open until the test releases it.
                    let _ = release_rx.recv();
                    break;
                }
            }
        }
    });

    let (client, _events, conns) = start_client(&pipe);
    assert!(
        wait_until(CONNECT_WAIT, || conn_log_contains(&conns, true)),
        "client did not connect to the fake daemon"
    );

    let t0 = Instant::now();
    let res = client.request(json!({"type": "get_status"}), Duration::from_millis(500));
    let elapsed = t0.elapsed();
    assert_eq!(
        res,
        Err(ClientError::Timeout),
        "expected Timeout, got {res:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(2),
        "timeout took {elapsed:?}, expected about 0.5 s"
    );

    drop(release_tx);
    client.stop();
    let _ = server.join();
}

#[test]
fn invalid_command_returns_invalid_command_error() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let res = client.request(json!({"type": "no_such_command"}), REQ_TIMEOUT);
    assert!(
        matches!(res, Err(ClientError::InvalidCommand(_))),
        "expected Err(InvalidCommand(_)), got {res:?}"
    );

    client.stop();
}

#[test]
fn stop_returns_promptly_with_no_server() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pipe = unique_pipe("clipcheck-ui-client-test");
    let (client, _events, conns) = start_client(&pipe);
    // Negative check: no server exists, so nothing can connect. This is a fixed, bounded
    // settle time that lets the client's retry loop start, because "no connection" cannot
    // be polled for. The real assertion is that stop() returns promptly below.
    thread::sleep(Duration::from_millis(300));

    let t0 = Instant::now();
    client.stop();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "stop() took {elapsed:?} with no server"
    );
    assert!(
        !conn_log_contains(&conns, true),
        "on_connection(true) fired with no server"
    );
}
