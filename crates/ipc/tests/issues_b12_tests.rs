//! Regression tests for ipc issues #34 (required v1 fields) and #179
//! (`PipeConn::cancel_pending`). Black-box: public API only.

use clipcheck_ipc::proto::{Event, Push, Reply, Response};
use serde_json::{json, Value};

/// A complete status frame. Every field is present, so each test can remove one.
fn status_frame() -> Value {
    json!({
        "v": 1,
        "id": 3,
        "body": {
            "type": "status",
            "data": {
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
            }
        }
    })
}

/// Removes `key` from the object at `obj_path` (keys from the root of `v`).
fn drop_key(mut v: Value, obj_path: &[&str], key: &str) -> Value {
    let mut cur = &mut v;
    for p in obj_path {
        cur = cur.get_mut(*p).expect("path exists");
    }
    cur.as_object_mut()
        .expect("object")
        .remove(key)
        .unwrap_or_else(|| panic!("fixture has no key {key}"));
    v
}

fn status_data() -> &'static [&'static str] {
    &["body", "data"]
}

fn session_obj() -> Vec<&'static str> {
    vec!["body", "data", "session"]
}

// ---------- #34: required v1 fields fail to decode when missing ----------

#[test]
fn issue_34_complete_status_frame_parses() {
    let r = serde_json::from_value::<Response>(status_frame());
    let r = r.expect("control: full status frame must parse");
    match r.body {
        Reply::Status(st) => {
            assert_eq!(st.server_unix_ms, 1_791_510_843_500);
            assert_eq!(st.buffer_secs, 300.0);
            assert!(!st.settings_pending_restart);
        }
        other => panic!("expected status, got {other:?}"),
    }
}

#[test]
fn issue_34_status_missing_server_unix_ms_fails_to_decode() {
    let v = drop_key(status_frame(), status_data(), "server_unix_ms");
    let err = serde_json::from_value::<Response>(v).expect_err("must fail");
    assert!(
        err.to_string().contains("server_unix_ms"),
        "error should name the field: {err}"
    );
}

#[test]
fn issue_34_status_missing_buffer_secs_fails_to_decode() {
    let v = drop_key(status_frame(), status_data(), "buffer_secs");
    assert!(serde_json::from_value::<Response>(v).is_err());
}

#[test]
fn issue_34_status_missing_settings_pending_restart_fails_to_decode() {
    let v = drop_key(status_frame(), status_data(), "settings_pending_restart");
    assert!(serde_json::from_value::<Response>(v).is_err());
}

#[test]
fn issue_34_session_info_missing_parts_saved_fails_to_decode() {
    let path = session_obj();
    let refs: Vec<&str> = path.to_vec();
    let v = drop_key(status_frame(), &refs, "parts_saved");
    assert!(serde_json::from_value::<Response>(v).is_err());
}

#[test]
fn issue_34_optional_game_info_still_defaults_to_none() {
    // Option fields without a default are still absent-tolerant in serde. The
    // attribute was redundant, so removing it must not change this.
    let v = drop_key(status_frame(), status_data(), "game_info");
    let r: Response = serde_json::from_value(v).expect("absent Option parses");
    match r.body {
        Reply::Status(st) => assert!(st.game_info.is_none()),
        other => panic!("expected status, got {other:?}"),
    }
}

#[test]
fn issue_34_config_reply_missing_path_fails_to_decode() {
    let v = json!({"v": 1, "id": 9, "body": {"type": "config", "data": {
        "config": {"a": 1}, "config_version": 1
    }}});
    assert!(serde_json::from_value::<Response>(v).is_err());
}

#[test]
fn issue_34_config_reply_missing_config_version_fails_to_decode() {
    let v = json!({"v": 1, "id": 9, "body": {"type": "config", "data": {
        "config": {"a": 1}, "path": "C:\\cfg\\config.toml"
    }}});
    assert!(serde_json::from_value::<Response>(v).is_err());
}

#[test]
fn issue_34_config_reply_complete_parses() {
    let v = json!({"v": 1, "id": 9, "body": {"type": "config", "data": {
        "config": {"a": 1}, "config_version": 1, "path": "C:\\cfg\\config.toml"
    }}});
    let r: Response = serde_json::from_value(v).expect("control");
    match r.body {
        Reply::Config {
            path,
            config_version,
            ..
        } => {
            assert_eq!(path, "C:\\cfg\\config.toml");
            assert_eq!(config_version, 1);
        }
        other => panic!("expected config, got {other:?}"),
    }
}

fn part_finished_frame() -> Value {
    json!({"v": 1, "seq": 77, "body": {"type": "session_part_finished", "data": {
        "session_id": "01JDK7W2Q9Z6H4T1V8B3N5M7KC",
        "part": 2,
        "path": "C:\\Videos\\session-part2.mkv",
        "duration_ms": 60000,
        "bytes": 1048576,
        "recovered": false,
        "started_unix_ms": 1791510843000i64,
        "parts_saved": 2
    }}})
}

#[test]
fn issue_34_session_part_finished_complete_parses() {
    let p: Push = serde_json::from_value(part_finished_frame()).expect("control");
    match p.body {
        Event::SessionPartFinished {
            started_unix_ms,
            parts_saved,
            ..
        } => {
            assert_eq!(started_unix_ms, 1_791_510_843_000);
            assert_eq!(parts_saved, 2);
        }
        other => panic!("expected session_part_finished, got {other:?}"),
    }
}

#[test]
fn issue_34_session_part_finished_missing_started_unix_ms_fails() {
    let v = drop_key(part_finished_frame(), &["body", "data"], "started_unix_ms");
    assert!(serde_json::from_value::<Push>(v).is_err());
}

#[test]
fn issue_34_session_part_finished_missing_parts_saved_fails() {
    let v = drop_key(part_finished_frame(), &["body", "data"], "parts_saved");
    assert!(serde_json::from_value::<Push>(v).is_err());
}

#[test]
fn issue_34_config_applied_diagnostics_still_default_to_empty() {
    // Warnings are additive, so `diagnostics` keeps its default.
    let v = json!({"v": 1, "id": 2, "body": {"type": "config_applied", "data": {
        "apply_class": "live", "restart_required": false
    }}});
    let r: Response = serde_json::from_value(v).expect("absent diagnostics parses");
    match r.body {
        Reply::ConfigApplied { diagnostics, .. } => assert!(diagnostics.is_empty()),
        other => panic!("expected config_applied, got {other:?}"),
    }
}

// ---------- #179: PipeConn::cancel_pending (Windows only) ----------

#[cfg(windows)]
mod cancel {
    use std::io::{self, Read};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use clipcheck_ipc::pipe::{PipeClient, PipeConn, PipeServer};

    const WATCHDOG: Duration = Duration::from_secs(20);
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const CANCEL_DEADLINE: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(25);

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A pipe name that no other test or process uses.
    fn unique_name(tag: &str) -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!(
            r"\\.\pipe\clipcheck-test-{}-{}-{}-{}",
            std::process::id(),
            tag,
            n,
            nanos
        )
    }

    /// Binds a unique pipe on a helper thread (PipeServer is not Send), accepts one
    /// client there, and connects one client here. Returns (server side, client side).
    fn pair(tag: &str) -> (PipeConn, PipeConn) {
        let name = unique_name(tag);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (conn_tx, conn_rx) = mpsc::channel::<io::Result<PipeConn>>();
        let server_name = name.clone();
        let acceptor = thread::spawn(move || {
            let mut server = match PipeServer::bind(&server_name) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("bind: {e}")));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            let _ = conn_tx.send(server.accept());
        });
        ready_rx
            .recv_timeout(WATCHDOG)
            .expect("server did not bind in time")
            .expect("bind failed");
        let client = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
        let srv = conn_rx
            .recv_timeout(WATCHDOG)
            .expect("accept did not finish in time")
            .expect("accept failed");
        acceptor.join().expect("acceptor panicked");
        (srv, client)
    }

    #[test]
    fn issue_179_cancel_pending_unblocks_a_blocked_reader_on_a_clone() {
        // The server accepts and never writes, so a read on the client clone blocks.
        let (_srv, cli) = pair("cancel_blocked");
        let mut reader = cli.try_clone().expect("clone");
        let (tx, rx) = mpsc::channel::<io::Result<usize>>();
        let reader_thread = thread::spawn(move || {
            let mut buf = [0u8; 1];
            let _ = tx.send(reader.read(&mut buf));
        });

        // The reader may not have issued its read yet. A cancel with nothing pending
        // still succeeds, so repeat the cancel until the reader returns or the deadline passes.
        let deadline = Instant::now() + CANCEL_DEADLINE;
        let result = loop {
            cli.cancel_pending().expect("cancel_pending must succeed");
            match rx.recv_timeout(POLL) {
                Ok(r) => break r,
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                Err(e) => panic!("reader still blocked after cancel_pending: {e:?}"),
            }
        };
        assert!(
            result.is_err(),
            "a cancelled read must fail, got {result:?}"
        );
        reader_thread.join().expect("reader thread exits");
    }

    #[test]
    fn issue_179_cancel_pending_with_nothing_pending_succeeds() {
        let (_srv, cli) = pair("cancel_idle");
        cli.cancel_pending()
            .expect("nothing pending counts as success");
        cli.cancel_pending().expect("repeat is still success");
    }
}
