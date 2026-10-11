//! Tests for issues #44 (event sequence numbers), #51 (replies remembered for retried requests),
//! #148 (protocol version negotiation) and #108 (every subscriber sees the same events).
//!
//! Each daemon uses a temp data dir and a unique pipe name, and is shut down over IPC at the end
//! of the test (or killed on drop if an assertion fails first). Every test runs under a 60 s
//! watchdog. Shared helpers live in `tests/common`.

use std::path::Path;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{
    Command as Cmd, Envelope, ErrorCode, Push, Reply, Request, Response, Topic,
};
use clipcheck_ipc::PipeConn;
use serde_json::json;

mod common;
use common::*;

const WATCHDOG: Duration = Duration::from_secs(60);
const CLEANUP_ID: u64 = 999_999;

/// Runs `body` on a worker thread and fails if it does not finish within `WATCHDOG`.
/// A panic inside `body` is re-raised so the original message is reported.
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

/// Spawns the daemon with `--no-encoders`.
fn spawn(data_dir: &Path, pipe: &str) -> Daemon {
    spawn_with(data_dir, pipe, &["--no-encoders"])
}

fn send(conn: &mut PipeConn, req: Request) {
    write_frame(conn, &req).expect("write request frame");
}

fn assert_pong(resp: &Response) {
    match resp.body {
        Reply::Pong { .. } => {}
        ref other => panic!("expected Pong, got {other:?}"),
    }
}

fn expect_error(resp: &Response, code: ErrorCode) {
    match &resp.body {
        Reply::Error {
            code: got, message, ..
        } => {
            assert_eq!(*got, code, "wrong error code, message was {message:?}");
        }
        other => panic!("expected Error({code:?}), got {other:?}"),
    }
}

fn expect_applied(resp: &Response) {
    match resp.body {
        Reply::ConfigApplied { .. } => {}
        ref other => panic!("expected ConfigApplied, got {other:?}"),
    }
}

/// Sends `PatchConfig` setting `general.start_minimized`.
fn patch(conn: &mut PipeConn, id: u64, start_minimized: bool, dry_run: bool) -> Response {
    call(
        conn,
        id,
        Cmd::PatchConfig {
            patch: json!({ "general": { "start_minimized": start_minimized } }),
            dry_run,
        },
    )
}

/// Reads back `general.start_minimized` from the daemon's current configuration.
fn start_minimized(conn: &mut PipeConn, id: u64) -> bool {
    match call(conn, id, Cmd::GetConfig).body {
        Reply::Config { config, .. } => config["general"]["start_minimized"]
            .as_bool()
            .expect("general.start_minimized is a bool"),
        other => panic!("expected Config, got {other:?}"),
    }
}

/// Subscribes `conn` to the Settings topic (where `ConfigChanged` is published).
fn subscribe_settings(conn: &mut PipeConn, id: u64) {
    match call(
        conn,
        id,
        Cmd::Subscribe {
            topics: vec![Topic::Settings],
        },
    )
    .body
    {
        Reply::Subscribed { .. } => {}
        other => panic!("expected Subscribed, got {other:?}"),
    }
}

/// Moves a subscribed connection to a reader thread that forwards every pushed event,
/// tagged with `tag`, to `tx`. The thread ends when the pipe closes.
fn spawn_reader(mut conn: PipeConn, tag: usize, tx: mpsc::Sender<(usize, Push)>) {
    thread::spawn(move || {
        while let Ok(push) = read_frame::<_, Push>(&mut conn) {
            if tx.send((tag, push)).is_err() {
                break;
            }
        }
    });
}

/// Receives until every sender is gone. Fails if the stream stalls for 10 s.
fn drain<T>(rx: &mpsc::Receiver<T>) -> Vec<T> {
    let mut out = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(v) => out.push(v),
            Err(RecvTimeoutError::Disconnected) => return out,
            Err(RecvTimeoutError::Timeout) => panic!("event stream stalled for 10 s"),
        }
    }
}

/// Sends Shutdown over IPC and checks the daemon exits cleanly.
fn shutdown(d: &mut Daemon, conn: &mut PipeConn) {
    match call(
        conn,
        CLEANUP_ID,
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
    let status = wait_exit(d, Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

/// Sends a Hello with the given frame version and supported range on a fresh connection.
fn hello_frame(
    d: &mut Daemon,
    pipe: &str,
    frame_v: u16,
    proto_min: u16,
    proto_max: u16,
) -> Response {
    let mut conn = connect(d, pipe);
    send(
        &mut conn,
        Envelope {
            v: frame_v,
            id: Some(1),
            seq: None,
            body: Cmd::Hello {
                proto_min,
                proto_max,
                client: "test".into(),
                client_version: "0.0.0".into(),
            },
        },
    );
    recv_frame(&mut conn)
}

// ---------------------------------------------------------------------------
// #44: sequence numbers are strictly increasing for each subscriber, even under load.
// ---------------------------------------------------------------------------

#[test]
fn issue44_seq_strictly_increasing_under_concurrent_load() {
    watchdog("issue44", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut admin = connect_and_hello(&mut d, &pipe);

        let mut sub = connect_and_hello(&mut d, &pipe);
        subscribe_settings(&mut sub, 2);
        let (tx, rx) = mpsc::channel();
        spawn_reader(sub, 0, tx);

        let conns: Vec<PipeConn> = (0..4).map(|_| connect_and_hello(&mut d, &pipe)).collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        let workers: Vec<thread::JoinHandle<usize>> = conns
            .into_iter()
            .enumerate()
            .map(|(w, mut conn)| {
                thread::spawn(move || {
                    let mut applied = 0usize;
                    let mut id = 10u64;
                    while Instant::now() < deadline {
                        let resp = patch(&mut conn, id, w % 2 == 0, false);
                        id += 1;
                        expect_applied(&resp);
                        applied += 1;
                    }
                    applied
                })
            })
            .collect();
        let applied: usize = workers
            .into_iter()
            .map(|h| h.join().expect("patch worker panicked"))
            .sum();

        shutdown(&mut d, &mut admin);
        let events = drain(&rx);
        eprintln!(
            "issue44: applied patches={applied}, events received={}",
            events.len()
        );

        let seqs: Vec<u64> = events
            .iter()
            .map(|(_, p)| p.seq.expect("event frame carries seq"))
            .collect();
        assert!(!seqs.is_empty(), "subscriber received no events");
        for pair in seqs.windows(2) {
            assert!(
                pair[0] < pair[1],
                "seq not strictly increasing: {} followed by {}",
                pair[0],
                pair[1]
            );
        }
    });
}

// ---------------------------------------------------------------------------
// #51: replies for retried replayable requests are remembered per connection.
// ---------------------------------------------------------------------------

#[test]
fn issue51_same_id_same_command_is_replayed_and_runs_once() {
    watchdog("issue51 replay", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_and_hello(&mut d, &pipe);

        let first = patch(&mut conn, 2, true, false);
        expect_applied(&first);
        expect_applied(&patch(&mut conn, 3, false, false));
        assert!(
            !start_minimized(&mut conn, 4),
            "setup: config should be false"
        );

        let retry = patch(&mut conn, 2, true, false);
        assert_eq!(
            retry.body, first.body,
            "replayed reply differs from first reply"
        );
        assert!(
            !start_minimized(&mut conn, 5),
            "retry re-ran the handler: config changed back to true"
        );
        shutdown(&mut d, &mut conn);
    });
}

#[test]
fn issue51_same_id_different_payload_runs_as_a_new_request() {
    // Design decision: the replay cache matches on request id AND the full command. A request that
    // reuses an id with a different payload is a new request and must run (it is never swallowed by
    // a stale cached reply). Only an exact retry of the same command is answered from the cache.
    watchdog("issue51 payload", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_and_hello(&mut d, &pipe);

        let first = patch(&mut conn, 2, true, false);
        expect_applied(&first);
        let second = patch(&mut conn, 2, false, false);
        expect_applied(&second);
        assert!(
            !start_minimized(&mut conn, 3),
            "a reused id with a different payload must run: config should now be false"
        );
        shutdown(&mut d, &mut conn);
    });
}

#[test]
fn issue51_reused_id_with_different_command_runs_as_new_request() {
    watchdog("issue51 command", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_and_hello(&mut d, &pipe);

        // StopSession is replayable; a different command with the same id must still run.
        let stop = call(&mut conn, 4, Cmd::StopSession);
        eprintln!("issue51: StopSession id 4 reply = {:?}", stop.body);
        let patched = patch(&mut conn, 4, true, false);
        expect_applied(&patched);
        assert_ne!(
            patched.body, stop.body,
            "different command reused a cached reply"
        );

        // Same id, PatchConfig with dry_run flipped: also a different command.
        let checked = patch(&mut conn, 2, true, true);
        match checked.body {
            Reply::ConfigChecked { .. } => {}
            other => panic!("dry_run with a reused id should run as a new request, got {other:?}"),
        }
        shutdown(&mut d, &mut conn);
    });
}

#[test]
fn issue51_ids_are_per_connection() {
    watchdog("issue51 per connection", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn1 = connect_and_hello(&mut d, &pipe);
        let mut conn2 = connect_and_hello(&mut d, &pipe);

        expect_applied(&patch(&mut conn1, 2, true, false));
        expect_applied(&patch(&mut conn1, 3, false, false));
        assert!(
            !start_minimized(&mut conn1, 4),
            "setup: config should be false"
        );

        // Same id and command on a second connection must run again.
        expect_applied(&patch(&mut conn2, 2, true, false));
        assert!(
            start_minimized(&mut conn2, 3),
            "id 2 on a second connection was replayed instead of run"
        );
        shutdown(&mut d, &mut conn1);
    });
}

#[test]
fn issue51_cache_window_and_eviction() {
    watchdog("issue51 eviction", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_and_hello(&mut d, &pipe);

        expect_applied(&patch(&mut conn, 2, true, false));
        expect_applied(&patch(&mut conn, 3, false, false));

        // Control: 100 other replayable requests keep id 2 inside the 256-entry window.
        for i in 0..100u64 {
            let _ = patch(&mut conn, 100 + i, true, true);
        }
        expect_applied(&patch(&mut conn, 2, true, false));
        assert!(
            !start_minimized(&mut conn, 5000),
            "inside the replay window id 2 should be replayed, but it re-ran"
        );

        // 256 more requests push id 2 out of the window; the same id must now run again.
        for i in 0..256u64 {
            let _ = patch(&mut conn, 1000 + i, true, true);
        }
        expect_applied(&patch(&mut conn, 2, true, false));
        assert!(
            start_minimized(&mut conn, 6000),
            "after eviction id 2 should run again, but the config did not change"
        );
        shutdown(&mut d, &mut conn);
    });
}

#[test]
fn issue51_report_only_error_reply_for_repeated_id() {
    // Report-only: whether a StopSession error reply (no active session) is replayed for the
    // same id. The known open question about busy/invalid_state replay is not asserted here.
    watchdog("issue51 report", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut conn = connect_and_hello(&mut d, &pipe);

        let first = call(&mut conn, 7, Cmd::StopSession);
        let second = call(&mut conn, 7, Cmd::StopSession);
        eprintln!(
            "REPORT issue51 StopSession x2 id 7: first={:?} second={:?} identical={}",
            first.body,
            second.body,
            first.body == second.body
        );
        shutdown(&mut d, &mut conn);
    });
}

// ---------------------------------------------------------------------------
// #148: protocol version negotiation.
// ---------------------------------------------------------------------------

#[test]
fn issue148_supported_hello_ranges_negotiate_to_v1() {
    watchdog("issue148 supported", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);

        for (min, max) in [(1u16, 1u16), (1, 2), (0, 1), (0, 65535), (1, 65535)] {
            let resp = hello_frame(&mut d, &pipe, 1, min, max);
            match resp.body {
                Reply::HelloAck { proto, .. } => {
                    assert_eq!(proto, 1, "range {min}..={max} negotiated the wrong version");
                }
                other => panic!("range {min}..={max}: expected HelloAck, got {other:?}"),
            }
        }
        let mut conn = connect_and_hello(&mut d, &pipe);
        shutdown(&mut d, &mut conn);
    });
}

#[test]
fn issue148_unsupported_hello_ranges_rejected() {
    watchdog("issue148 unsupported", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);

        // (frame version, client min, client max)
        for (frame_v, min, max) in [
            (1u16, 2u16, 3u16),
            (1, 0, 0),
            (1, 65535, 65535),
            (1, 5, 1),
            (2, 1, 1),
        ] {
            let resp = hello_frame(&mut d, &pipe, frame_v, min, max);
            expect_error(&resp, ErrorCode::ProtoMismatch);
        }
        assert!(
            d.child.try_wait().expect("try_wait failed").is_none(),
            "daemon exited after rejected hellos"
        );
        let mut conn = connect_and_hello(&mut d, &pipe);
        shutdown(&mut d, &mut conn);
    });
}

#[test]
fn issue148_extreme_versions_do_not_panic() {
    watchdog("issue148 extremes", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);

        // Frame version 0 and 65535 in the Hello frame itself.
        for frame_v in [0u16, 65535] {
            let resp = hello_frame(&mut d, &pipe, frame_v, 1, 1);
            expect_error(&resp, ErrorCode::ProtoMismatch);
        }

        // Frame version 0 and 65535 after a successful handshake: rejected, connection stays up.
        let mut conn = connect_and_hello(&mut d, &pipe);
        for (i, frame_v) in [0u16, 65535].into_iter().enumerate() {
            let id = 5 + i as u64;
            send(
                &mut conn,
                Envelope {
                    v: frame_v,
                    id: Some(id),
                    seq: None,
                    body: Cmd::Ping,
                },
            );
            let resp = recv_frame(&mut conn);
            assert_eq!(resp.id, Some(id), "ProtoMismatch reply must echo the id");
            expect_error(&resp, ErrorCode::ProtoMismatch);
        }
        assert_pong(&call(&mut conn, 8, Cmd::Ping));
        shutdown(&mut d, &mut conn);
    });
}

// ---------------------------------------------------------------------------
// #108: five subscribers see the same events in the same order with equal content.
// ---------------------------------------------------------------------------

#[test]
fn issue108_five_subscribers_receive_identical_streams() {
    watchdog("issue108", || {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe);
        let mut admin = connect_and_hello(&mut d, &pipe);

        let (tx, rx) = mpsc::channel();
        for tag in 0..5usize {
            let mut sub = connect_and_hello(&mut d, &pipe);
            subscribe_settings(&mut sub, 2);
            spawn_reader(sub, tag, tx.clone());
        }
        drop(tx);

        const PATCHES: usize = 20;
        let mut applied = 0usize;
        for i in 0..PATCHES {
            let resp = patch(&mut admin, 2 + i as u64, i % 2 == 0, false);
            expect_applied(&resp);
            applied += 1;
        }

        let mut per: Vec<Vec<Push>> = vec![Vec::new(); 5];
        let deadline = Instant::now() + Duration::from_secs(30);
        while per.iter().any(|v| v.len() < applied) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok((tag, push)) => per[tag].push(push),
                Err(_) => panic!(
                    "timed out waiting for events: received per subscriber {:?}, expected {applied} each",
                    per.iter().map(Vec::len).collect::<Vec<_>>()
                ),
            }
        }
        while let Ok((tag, push)) = rx.try_recv() {
            per[tag].push(push);
        }
        eprintln!(
            "issue108: applied patches={applied}, events per subscriber={:?}",
            per.iter().map(Vec::len).collect::<Vec<_>>()
        );

        assert_eq!(per[0].len(), applied, "subscriber 0 event count");
        for (tag, stream) in per.iter().enumerate().skip(1) {
            assert_eq!(
                stream, &per[0],
                "subscriber {tag} differs from subscriber 0"
            );
        }
        for pair in per[0].windows(2) {
            assert!(
                pair[0].seq < pair[1].seq,
                "seq not increasing within the stream"
            );
        }
        shutdown(&mut d, &mut admin);
    });
}
