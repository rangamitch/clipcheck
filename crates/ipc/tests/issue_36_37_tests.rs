#![cfg(windows)]

//! Tests for issue #36 (one write per frame, concurrent and duplicated writers)
//! and issue #37 (pooled overlapped events keep the handle count stable).
//!
//! Every test uses its own pipe name, so none of them touches the production
//! pipe. Every test body runs under a watchdog that fails it after 20 seconds.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::frame::{read_frame, write_frame, FrameError, MAX_FRAME_BYTES};
use clipcheck_ipc::pipe::{PipeClient, PipeConn, PipeServer};
use serde_json::{json, Value};

const WATCHDOG: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDLE_SLACK: u32 = 50;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Returns a pipe name that no other test or process uses.
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

/// Runs `body` on its own thread and fails if it does not finish within `WATCHDOG`.
/// A panic inside `body` is re-raised on the test thread.
fn bounded<F>(body: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    match rx.recv_timeout(WATCHDOG) {
        Ok(()) => {
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("test body ended without signalling completion"),
        },
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("watchdog: test did not finish within {WATCHDOG:?} (hang)")
        }
    }
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

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetProcessHandleCount(process: isize, handle_count: *mut u32) -> i32;
}

/// Current process handle count, read directly through Win32 GetProcessHandleCount.
fn handle_count() -> u32 {
    let mut n = 0u32;
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
    // `n` is a valid, writable u32 for the duration of the call.
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) };
    assert!(ok != 0, "GetProcessHandleCount failed");
    n
}

/// A JSON value whose serialized form is exactly `n` bytes long (`n >= 1`).
fn body_of_len(n: usize, fill: u8) -> Value {
    match n {
        1 => json!(0),
        _ => Value::String(char::from(fill).to_string().repeat(n - 2)),
    }
}

/// Reads `expected.len()` frames from `r` and checks each one in order.
fn read_expected<R: Read>(r: &mut R, expected: &[Value], label: &str) {
    for (i, want) in expected.iter().enumerate() {
        let got: Value = read_frame(r).unwrap_or_else(|e| panic!("{label}: frame {i}: {e}"));
        assert_eq!(&got, want, "{label}: frame {i} mismatch");
    }
}

#[test]
fn round_trip_frame_sizes_one_write_per_frame() {
    bounded(|| {
        let sizes = [
            1usize,
            4,
            1024,
            64 * 1024,
            MAX_FRAME_BYTES - 1,
            MAX_FRAME_BYTES,
        ];
        let mut a = Vec::new();
        let mut b = Vec::new();
        for round in 0..2 {
            for (i, &n) in sizes.iter().enumerate() {
                let fill = b'a' + ((round * 7 + i) % 26) as u8;
                let v = body_of_len(n, fill);
                assert_eq!(
                    serde_json::to_vec(&v).expect("ser").len(),
                    n,
                    "sizing sanity"
                );
                a.push(v.clone());
                b.push(body_of_len(n, b'A' + ((round * 3 + i) % 26) as u8));
            }
        }

        let (srv, cli) = pair("sizes");
        let mut cli_r = cli.try_clone().expect("clone");
        let mut srv_r = srv.try_clone().expect("clone");
        let mut srv_w = srv;
        let mut cli_w = cli;

        // Server writes A to the client; the client writes B to the server.
        let a_w = a.clone();
        let w_a = thread::spawn(move || {
            for v in &a_w {
                write_frame(&mut srv_w, v).expect("server write");
            }
        });
        let b_w = b.clone();
        let w_b = thread::spawn(move || {
            for v in &b_w {
                write_frame(&mut cli_w, v).expect("client write");
            }
        });
        let r_b = thread::spawn(move || read_expected(&mut srv_r, &b, "server reads B"));
        read_expected(&mut cli_r, &a, "client reads A");

        w_a.join().expect("server writer panicked");
        w_b.join().expect("client writer panicked");
        r_b.join().expect("server reader panicked");
    });
}

#[test]
fn small_frames_round_trip_in_order() {
    bounded(|| {
        let expected: Vec<Value> = (0..300).map(|i| json!({"i": i})).collect();
        let (mut srv, mut cli) = pair("small");
        let to_send = expected.clone();
        let w = thread::spawn(move || {
            for v in &to_send {
                write_frame(&mut srv, v).expect("write");
            }
        });
        read_expected(&mut cli, &expected, "small");
        w.join().expect("writer panicked");
    });
}

#[test]
fn oversized_frame_writes_nothing_and_next_frame_is_intact() {
    bounded(|| {
        let (mut srv, mut cli) = pair("oversize");
        let too_big = body_of_len(MAX_FRAME_BYTES + 1, b'z');
        let err = write_frame(&mut srv, &too_big).expect_err("oversized frame must fail");
        assert!(
            matches!(err, FrameError::TooLarge(n) if n == MAX_FRAME_BYTES + 1),
            "expected TooLarge(MAX+1), got {err:?}"
        );

        write_frame(&mut srv, &json!("after")).expect("valid write after rejection");
        let got: Value = read_frame(&mut cli).expect("reader must see the valid frame first");
        assert_eq!(got, json!("after"));
    });
}

#[test]
fn oversized_length_header_on_wire_is_rejected_by_reader() {
    bounded(|| {
        let (mut srv, mut cli) = pair("badheader");
        let claimed = (MAX_FRAME_BYTES + 1) as u32;
        srv.write_all(&claimed.to_le_bytes()).expect("raw header");
        let err = read_frame::<_, Value>(&mut cli).expect_err("reader must reject");
        assert!(
            matches!(err, FrameError::TooLarge(n) if n == MAX_FRAME_BYTES + 1),
            "expected TooLarge, got {err:?}"
        );
    });
}

#[test]
fn concurrent_writers_under_shared_mutex_never_interleave() {
    bounded(|| {
        const THREADS: usize = 8;
        const PER: usize = 200;
        let (srv, mut cli) = pair("mutex");
        let shared = Arc::new(Mutex::new(srv));

        let mut joins = Vec::new();
        for t in 0..THREADS {
            let shared = Arc::clone(&shared);
            joins.push(thread::spawn(move || {
                for i in 0..PER {
                    let pad_len = (t * 131 + i * 17) % 2000;
                    let frame = json!({"t": t, "i": i, "pad": "p".repeat(pad_len)});
                    let mut conn = shared.lock().unwrap_or_else(|p| p.into_inner());
                    write_frame(&mut *conn, &frame).expect("write under mutex");
                }
            }));
        }

        let mut next = vec![0usize; THREADS];
        for _ in 0..THREADS * PER {
            let v: Value = read_frame(&mut cli).expect("frame must decode (no interleaving)");
            let t = v["t"].as_u64().expect("t field") as usize;
            let i = v["i"].as_u64().expect("i field") as usize;
            assert_eq!(i, next[t], "thread {t}: frame out of order or lost");
            assert_eq!(
                v["pad"].as_str().expect("pad").len(),
                (t * 131 + i * 17) % 2000,
                "thread {t} frame {i}: payload corrupted"
            );
            next[t] += 1;
        }
        for j in joins {
            j.join().expect("writer panicked");
        }
        assert!(next.iter().all(|&n| n == PER), "missing frames: {next:?}");
    });
}

#[test]
fn cloned_conn_full_duplex_without_external_mutex() {
    bounded(|| {
        const N: usize = 500;
        let (srv, cli) = pair("duplex");
        let mut srv_r = srv.try_clone().expect("clone");
        let mut cli_r = cli.try_clone().expect("clone");
        let mut srv_w = srv;
        let mut cli_w = cli;

        let w_s = thread::spawn(move || {
            for i in 0..N {
                let v = json!({"dir": "s2c", "i": i, "pad": "x".repeat(i % 300)});
                write_frame(&mut srv_w, &v).expect("server write");
            }
        });
        let w_c = thread::spawn(move || {
            for i in 0..N {
                let v = json!({"dir": "c2s", "i": i, "pad": "y".repeat(i % 300)});
                write_frame(&mut cli_w, &v).expect("client write");
            }
        });
        let r_s = thread::spawn(move || {
            for i in 0..N {
                let v: Value = read_frame(&mut srv_r).expect("server read");
                assert_eq!(v["dir"], "c2s");
                assert_eq!(v["i"], i);
            }
        });
        for i in 0..N {
            let v: Value = read_frame(&mut cli_r).expect("client read");
            assert_eq!(v["dir"], "s2c");
            assert_eq!(v["i"], i);
        }

        w_s.join().expect("server writer panicked");
        w_c.join().expect("client writer panicked");
        r_s.join().expect("server reader panicked");
    });
}

#[test]
fn two_clone_writers_with_external_mutex_stay_ordered() {
    bounded(|| {
        const PER: usize = 300;
        let (srv, mut cli) = pair("twoclones");
        let mut clone_a = srv.try_clone().expect("clone");
        let mut clone_b = srv;
        let gate = Arc::new(Mutex::new(()));

        let mut joins = Vec::new();
        let g1 = Arc::clone(&gate);
        joins.push(thread::spawn(move || {
            for i in 0..PER {
                let _guard = g1.lock().unwrap_or_else(|p| p.into_inner());
                let v = json!({"w": "A", "i": i, "pad": "a".repeat(i % 500)});
                write_frame(&mut clone_a, &v).expect("write A");
            }
        }));
        let g2 = Arc::clone(&gate);
        joins.push(thread::spawn(move || {
            for i in 0..PER {
                let _guard = g2.lock().unwrap_or_else(|p| p.into_inner());
                let v = json!({"w": "B", "i": i, "pad": "b".repeat(i % 500)});
                write_frame(&mut clone_b, &v).expect("write B");
            }
        }));

        let mut next = [0usize, 0usize];
        for _ in 0..2 * PER {
            let v: Value = read_frame(&mut cli).expect("frame must decode");
            let slot = match v["w"].as_str().expect("w") {
                "A" => 0,
                "B" => 1,
                other => panic!("unknown writer {other}"),
            };
            assert_eq!(v["i"], next[slot], "writer {slot}: out of order");
            assert_eq!(v["pad"].as_str().expect("pad").len(), next[slot] % 500);
            next[slot] += 1;
        }
        for j in joins {
            j.join().expect("writer panicked");
        }
        assert_eq!(next, [PER, PER]);
    });
}

#[test]
fn frame_split_across_many_small_writes_reassembles() {
    bounded(|| {
        let (mut srv, mut cli) = pair("split");
        let value = json!({"split": "abcdefghij".repeat(500)});
        let body = serde_json::to_vec(&value).expect("ser");
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&body);

        let w = thread::spawn(move || {
            for b in &frame[..4] {
                srv.write_all(std::slice::from_ref(b)).expect("header byte");
                srv.flush().expect("flush");
            }
            for chunk in frame[4..].chunks(7) {
                srv.write_all(chunk).expect("body chunk");
            }
            srv
        });
        let got: Value = read_frame(&mut cli).expect("reassembled frame");
        assert_eq!(got, value);
        let _srv = w.join().expect("writer panicked");
    });
}

#[test]
fn peer_closes_mid_header_is_truncated_not_hang() {
    bounded(|| {
        let (mut srv, mut cli) = pair("midheader");
        srv.write_all(&[1u8, 0]).expect("partial header");
        drop(srv);
        let err = read_frame::<_, Value>(&mut cli).expect_err("must fail");
        assert!(matches!(err, FrameError::Truncated), "got {err:?}");
    });
}

#[test]
fn peer_closes_mid_body_is_truncated_not_hang() {
    bounded(|| {
        let (mut srv, mut cli) = pair("midbody");
        srv.write_all(&100u32.to_le_bytes()).expect("header");
        srv.write_all(&[b'x'; 10]).expect("partial body");
        drop(srv);
        let err = read_frame::<_, Value>(&mut cli).expect_err("must fail");
        assert!(matches!(err, FrameError::Truncated), "got {err:?}");
    });
}

#[test]
fn peer_closes_at_frame_boundary_is_eof() {
    bounded(|| {
        let (mut srv, mut cli) = pair("boundary");
        write_frame(&mut srv, &json!("only")).expect("write");
        drop(srv);
        let first: Value = read_frame(&mut cli).expect("first frame");
        assert_eq!(first, json!("only"));
        let err = read_frame::<_, Value>(&mut cli).expect_err("must fail");
        assert!(matches!(err, FrameError::Eof), "got {err:?}");
    });
}

#[test]
fn handle_count_stable_over_20k_frames_and_throughput_floor() {
    bounded(|| {
        const WARMUP: usize = 200;
        const N: usize = 20_000;
        let (mut srv, mut cli) = pair("pool20k");
        let w = thread::spawn(move || {
            for i in 0..(WARMUP + N) {
                write_frame(&mut srv, &json!({"i": i})).expect("write");
            }
            srv
        });

        for i in 0..WARMUP {
            let v: Value = read_frame(&mut cli).expect("warmup read");
            assert_eq!(v["i"], i);
        }
        let before = handle_count();

        let start = Instant::now();
        for i in WARMUP..WARMUP + N {
            let v: Value = read_frame(&mut cli).expect("read");
            assert_eq!(v["i"], i, "frame out of order");
        }
        let elapsed = start.elapsed();
        let _srv = w.join().expect("writer panicked");

        let after = handle_count();
        let fps = N as f64 / elapsed.as_secs_f64();
        eprintln!("20k frames: {elapsed:?}, {fps:.0} frames/s, handles {before} -> {after}");
        assert!(
            after <= before + HANDLE_SLACK,
            "handle count grew from {before} to {after} (slack {HANDLE_SLACK})"
        );
        assert!(
            fps >= 1000.0,
            "throughput {fps:.0} frames/s below floor 1000/s"
        );
    });
}

#[test]
fn handle_count_stable_over_200_short_connections() {
    bounded(|| {
        const N: usize = 200;
        let name = unique_name("short");
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
        let server_name = name.clone();
        let srv_thread = thread::spawn(move || -> io::Result<()> {
            // PipeServer is not Send, so it is bound on this thread.
            let mut server = match PipeServer::bind(&server_name) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(io::Error::new(e.kind(), e.to_string())));
                    return Err(e);
                }
            };
            let _ = ready_tx.send(Ok(()));
            for _ in 0..N {
                let mut conn = server.accept()?;
                let req: Value =
                    read_frame(&mut conn).map_err(|e| io::Error::other(e.to_string()))?;
                write_frame(&mut conn, &json!({"echo": req["i"]}))
                    .map_err(|e| io::Error::other(e.to_string()))?;
            }
            Ok(())
        });

        ready_rx
            .recv_timeout(WATCHDOG)
            .expect("server did not bind in time")
            .expect("bind failed");
        let before = handle_count();

        for i in 0..N {
            let mut cli = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("connect");
            write_frame(&mut cli, &json!({"i": i})).expect("write");
            let reply: Value = read_frame(&mut cli).expect("reply");
            assert_eq!(reply, json!({"echo": i}));
        }
        srv_thread
            .join()
            .expect("server panicked")
            .expect("server io");

        let after = handle_count();
        eprintln!("200 connections: handles {before} -> {after}");
        assert!(
            after <= before + HANDLE_SLACK,
            "handle count grew from {before} to {after} (slack {HANDLE_SLACK})"
        );
    });
}
