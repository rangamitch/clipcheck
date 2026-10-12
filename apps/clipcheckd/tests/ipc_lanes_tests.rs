//! Lane tests for the daemon's IPC request handling (files lane, discovery lane, inline).
//!
//! The daemon runs slow commands off the connection's request loop, so replies can arrive out of
//! order and are matched by request id. These tests spawn the real `clipcheckd` over the named
//! pipe, pipeline several raw requests before reading, and collect replies by id with a deadline
//! so a hung daemon fails the test instead of hanging it.
//!
//! Only read-only or unknown-id commands are used. No recording or library file is modified, and
//! no microphone is opened (`mic_test` is never sent). The daemon runs against a temp data dir.
//!
//! Windows only.

#![cfg(windows)]

mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use clipcheck_ipc::frame::write_frame;
use clipcheck_ipc::proto::{Command as Cmd, Envelope, ErrorCode, Reply, Response, Topic};
use common::*;

/// Upper bound for collecting all replies of one burst.
const COLLECT: Duration = Duration::from_secs(30);

/// A ping answered later than this after it was sent counts as blocked.
const PING_LIMIT: Duration = Duration::from_secs(5);

/// One received reply: its body, when it arrived, and when its request was sent.
struct Got {
    reply: Reply,
    at: Instant,
}

/// The replies of one burst, keyed by request id, plus the order in which they arrived.
struct Collected {
    by_id: HashMap<u64, Got>,
    arrival: Vec<u64>,
}

/// Writes one request frame on the client's connection without waiting for a reply.
fn send(c: &mut Client, id: u64, cmd: Cmd) -> Instant {
    let sent = Instant::now();
    write_frame(&mut c.conn, &Envelope::request(id, cmd)).expect("write request frame");
    sent
}

/// Reads frames until every id in `want` has exactly one reply. Event frames are queued. A reply
/// with an unknown or duplicate id fails the test. After the last reply, a short grace period
/// checks that no stray reply follows.
fn collect(c: &mut Client, want: &[u64]) -> Collected {
    let deadline = Instant::now() + COLLECT;
    let mut out = Collected {
        by_id: HashMap::new(),
        arrival: Vec::new(),
    };
    while out.by_id.len() < want.len() {
        let frame = match c.next_frame(deadline, "a reply") {
            Some(frame) => frame,
            None => panic!(
                "timed out after {COLLECT:?}: got replies for {:?} (arrival order {:?}), wanted {want:?}",
                {
                    let mut ids: Vec<u64> = out.by_id.keys().copied().collect();
                    ids.sort_unstable();
                    ids
                },
                out.arrival,
            ),
        };
        let at = Instant::now();
        if !frame.get("id").is_some_and(|v| !v.is_null()) {
            c.queue_event(frame);
            continue;
        }
        let resp: Response = serde_json::from_value(frame).expect("malformed reply frame");
        let id = resp.id.expect("reply without an id");
        assert!(
            want.contains(&id),
            "reply for unexpected id {id}: {:?}",
            resp.body
        );
        assert!(
            !out.by_id.contains_key(&id),
            "duplicate reply for id {id}: {:?}",
            resp.body
        );
        out.arrival.push(id);
        out.by_id.insert(
            id,
            Got {
                reply: resp.body,
                at,
            },
        );
    }

    // Grace period: no further reply frame may arrive for this burst.
    let grace = Instant::now() + Duration::from_millis(300);
    while let Some(frame) = c.next_frame(grace, "a stray frame") {
        if frame.get("id").is_some_and(|v| !v.is_null()) {
            let resp: Response = serde_json::from_value(frame).expect("malformed reply frame");
            panic!(
                "extra reply after the burst completed: id {:?} body {:?}",
                resp.id, resp.body
            );
        }
        c.queue_event(frame);
    }
    out
}

/// Asserts that a reply is `Error` with `BadRequest`, which is what an unknown recording id gives.
fn assert_bad_request(got: &Got, id: u64) {
    match &got.reply {
        Reply::Error { code, .. } => assert!(
            matches!(code, ErrorCode::BadRequest),
            "id {id}: expected code bad_request, got {code:?}"
        ),
        other => panic!("id {id}: expected Error(bad_request), got {other:?}"),
    }
}

fn unknown_rename(name: &str) -> Cmd {
    Cmd::RenameRecording {
        recording_id: "no-such-recording-ipc-lanes".into(),
        name: name.into(),
    }
}

// ---------------------------------------------------------------------------
// 1. A lane request gets exactly one reply with its own id and the serial reply kind.
// ---------------------------------------------------------------------------

#[test]
fn lane_requests_reply_with_own_id_and_serial_kind() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    send(&mut c, 2, unknown_rename("x"));
    send(&mut c, 3, Cmd::ListLibraries);
    send(&mut c, 4, Cmd::ScanLibraries { root_id: None });

    let got = collect(&mut c, &[2, 3, 4]);
    assert_bad_request(&got.by_id[&2], 2);
    match &got.by_id[&3].reply {
        Reply::Libraries { .. } => {}
        other => panic!("id 3 (list_libraries): expected Libraries, got {other:?}"),
    }
    match &got.by_id[&4].reply {
        Reply::Ok | Reply::Error { .. } => {}
        other => panic!("id 4 (scan_libraries): expected Ok or Error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 2. Pings sent after a burst of lane requests on the same connection are answered promptly.
// ---------------------------------------------------------------------------

#[test]
fn pings_are_answered_during_lane_burst() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    let mut lane_ids = Vec::new();
    let mut ping_ids = Vec::new();
    let mut ping_sent: HashMap<u64, Instant> = HashMap::new();
    let mut kinds: HashMap<u64, u8> = HashMap::new(); // 0 rename, 1 list, 2 scan

    let mut next = 100u64;
    for i in 0..20u64 {
        let id = next;
        next += 1;
        let (cmd, kind) = match i % 3 {
            0 => (Cmd::ScanLibraries { root_id: None }, 2u8),
            1 => (Cmd::ListLibraries, 1u8),
            _ => (unknown_rename("burst"), 0u8),
        };
        send(&mut c, id, cmd);
        kinds.insert(id, kind);
        lane_ids.push(id);
        if i % 4 == 3 {
            let pid = next;
            next += 1;
            ping_sent.insert(pid, send(&mut c, pid, Cmd::Ping));
            ping_ids.push(pid);
        }
    }

    let mut all: Vec<u64> = lane_ids.clone();
    all.extend(ping_ids.iter().copied());
    let got = collect(&mut c, &all);

    for pid in &ping_ids {
        let g = &got.by_id[pid];
        assert!(
            matches!(g.reply, Reply::Pong { .. }),
            "ping {pid}: expected Pong, got {:?}",
            g.reply
        );
        let latency = g.at.duration_since(ping_sent[pid]);
        assert!(
            latency < PING_LIMIT,
            "ping {pid} took {latency:?} during a lane burst (limit {PING_LIMIT:?})"
        );
    }
    for id in &lane_ids {
        let g = &got.by_id[id];
        match kinds[id] {
            0 => assert_bad_request(g, *id),
            1 => assert!(
                matches!(g.reply, Reply::Libraries { .. }),
                "list_libraries id {id}: expected Libraries, got {:?}",
                g.reply
            ),
            _ => assert!(
                matches!(g.reply, Reply::Ok | Reply::Error { .. }),
                "scan_libraries id {id}: expected Ok or Error, got {:?}",
                g.reply
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Files-lane requests on one connection are answered in the order they were sent.
// ---------------------------------------------------------------------------

#[test]
fn files_lane_replies_arrive_in_send_order() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    send(&mut c, 10, unknown_rename("a"));
    send(&mut c, 11, Cmd::ListLibraries);
    send(&mut c, 12, unknown_rename("b"));
    send(&mut c, 13, Cmd::ListLibraries);

    let got = collect(&mut c, &[10, 11, 12, 13]);
    assert_bad_request(&got.by_id[&10], 10);
    assert!(matches!(got.by_id[&11].reply, Reply::Libraries { .. }));
    assert_bad_request(&got.by_id[&12], 12);
    assert!(matches!(got.by_id[&13].reply, Reply::Libraries { .. }));
    assert_eq!(
        got.arrival,
        vec![10, 11, 12, 13],
        "files-lane replies must arrive in send order"
    );
}

// ---------------------------------------------------------------------------
// 4. Disconnecting with files-lane requests still queued does not hang or crash the daemon.
// ---------------------------------------------------------------------------

#[test]
fn disconnect_with_queued_files_lane_requests_leaves_daemon_healthy() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    {
        let mut c = Client::handshake(&mut d, &pipe);
        for id in 2..14u64 {
            let cmd = if id % 2 == 0 {
                Cmd::ScanLibraries { root_id: None }
            } else {
                unknown_rename("queued")
            };
            send(&mut c, id, cmd);
        }
        // Drop the client here, with requests still queued and replies unread.
    }

    let mut c2 = Client::handshake(&mut d, &pipe);
    c2.ping();
    assert!(
        d.child.try_wait().expect("try_wait failed").is_none(),
        "daemon exited after a client disconnected with queued files-lane requests"
    );
    let log = log_text(dir.path());
    assert!(
        !log.contains("panicked"),
        "daemon panicked after the disconnect:\n{log}"
    );
    // Still serving a full command on the new connection.
    match c2.call(Cmd::GetStatus) {
        Reply::Status(_) => {}
        other => panic!("expected Status on the new connection, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. Inline commands keep working while lane requests are in flight.
// ---------------------------------------------------------------------------

#[test]
fn inline_commands_reply_correctly_while_lanes_busy() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    for id in 2..7u64 {
        send(&mut c, id, Cmd::ScanLibraries { root_id: None });
    }
    send(&mut c, 7, Cmd::GetStatus);
    send(&mut c, 8, Cmd::GetConfig);
    send(
        &mut c,
        9,
        Cmd::Subscribe {
            topics: vec![Topic::State],
        },
    );
    send(&mut c, 10, Cmd::Ping);

    let want: Vec<u64> = (2..11).collect();
    let got = collect(&mut c, &want);

    assert!(
        matches!(got.by_id[&7].reply, Reply::Status(_)),
        "id 7 (get_status): expected Status, got {:?}",
        got.by_id[&7].reply
    );
    match &got.by_id[&8].reply {
        Reply::Config { path, .. } => assert!(
            path.ends_with("config.toml"),
            "get_config path was {path:?}"
        ),
        other => panic!("id 8 (get_config): expected Config, got {other:?}"),
    }
    match &got.by_id[&9].reply {
        Reply::Subscribed { topics } => assert_eq!(topics.len(), 1, "subscribed topics"),
        other => panic!("id 9 (subscribe): expected Subscribed, got {other:?}"),
    }
    assert!(
        matches!(got.by_id[&10].reply, Reply::Pong { .. }),
        "id 10 (ping): expected Pong, got {:?}",
        got.by_id[&10].reply
    );
}

// ---------------------------------------------------------------------------
// 6. One cheap discovery-lane request: exactly one reply with its id, alongside inline and files.
//    No mic_test. Audio devices may be absent, so an Error reply is accepted.
// ---------------------------------------------------------------------------

#[test]
fn discovery_lane_request_gets_exactly_one_reply() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    send(&mut c, 2, Cmd::ListAudioDevices);
    send(&mut c, 3, unknown_rename("after-discovery"));
    send(&mut c, 4, Cmd::Ping);

    let got = collect(&mut c, &[2, 3, 4]);
    match &got.by_id[&2].reply {
        Reply::AudioDevices { .. } | Reply::Error { .. } => {}
        other => panic!("id 2 (list_audio_devices): unexpected reply {other:?}"),
    }
    assert_bad_request(&got.by_id[&3], 3);
    assert!(
        matches!(got.by_id[&4].reply, Reply::Pong { .. }),
        "id 4 (ping): expected Pong, got {:?}",
        got.by_id[&4].reply
    );
}
