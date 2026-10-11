#![cfg(windows)]

//! Named-pipe transport tests. Every test uses its own unique pipe name so it
//! never touches the production pipe. Server threads are bounded: each one
//! reports through a channel that the test waits on with a timeout.

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::frame::{read_frame, write_frame, FrameError};
use clipcheck_ipc::pipe::{default_pipe_name, PipeClient, PipeConn, PipeServer};
use clipcheck_ipc::proto::{Command, Envelope, Reply, PROTOCOL_VERSION};

const WAIT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_VERSION: &str = "test-daemon";
const DAEMON_PID: u32 = 4242;

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

fn hello_command() -> Command {
    Command::Hello {
        proto_min: 1,
        proto_max: PROTOCOL_VERSION,
        client: "clipcheck-test".to_string(),
        client_version: "0.0.0".to_string(),
    }
}

fn expected_hello_ack() -> Reply {
    Reply::HelloAck {
        proto: PROTOCOL_VERSION,
        daemon_version: DAEMON_VERSION.to_string(),
        daemon_pid: DAEMON_PID,
        features: Vec::new(),
    }
}

/// Client side: send a Hello request with `id` and read the reply.
fn client_hello(conn: &mut PipeConn, id: u64) -> Envelope<Reply> {
    write_frame(conn, &Envelope::request(id, hello_command())).expect("client write Hello");
    read_frame::<_, Envelope<Reply>>(conn).expect("client read HelloAck")
}

fn assert_hello_ack(env: &Envelope<Reply>, id: u64) {
    assert_eq!(env.id, Some(id), "reply id must echo the request id");
    assert_eq!(env.v, PROTOCOL_VERSION, "reply protocol version");
    assert_eq!(env.body, expected_hello_ack(), "HelloAck body");
}

/// Server side: read one Hello request and answer with HelloAck.
fn serve_hello(conn: &mut PipeConn) -> Result<(), String> {
    let req: Envelope<Command> = read_frame(conn).map_err(|e| format!("server read: {e}"))?;
    if !matches!(req.body, Command::Hello { .. }) {
        return Err(format!("expected Hello, got {:?}", req.body));
    }
    let id = req.id.ok_or_else(|| "request carried no id".to_string())?;
    let ack = Envelope::reply(id, expected_hello_ack());
    write_frame(conn, &ack).map_err(|e| format!("server write: {e}"))
}

/// Waits for a server thread's result, then joins it.
fn finish<T>(rx: mpsc::Receiver<T>, handle: thread::JoinHandle<()>) -> T {
    let result = rx
        .recv_timeout(WAIT)
        .expect("server thread did not report within the wait limit");
    handle.join().expect("server thread panicked");
    result
}

#[test]
fn handshake_round_trip() {
    let name = unique_name("handshake");
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            let mut conn = server.accept().map_err(|e| format!("accept: {e}"))?;
            serve_hello(&mut conn)
        })();
        let _ = tx.send(result);
    });

    let mut conn = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
    let reply = client_hello(&mut conn, 7);
    assert_hello_ack(&reply, 7);
    drop(conn);

    let result = finish(rx, server);
    assert!(result.is_ok(), "server side failed: {result:?}");
}

#[test]
fn second_first_instance_bind_fails() {
    let name = unique_name("dup");
    let first = PipeServer::bind(&name).expect("first bind must succeed");
    let second = PipeServer::bind(&name);
    match second {
        Ok(_) => panic!("second bind on the same name unexpectedly succeeded"),
        Err(e) => {
            println!("second bind error kind = {:?}, error = {e}", e.kind());
            assert_eq!(
                e.kind(),
                io::ErrorKind::PermissionDenied,
                "FIRST_PIPE_INSTANCE conflict should map to ERROR_ACCESS_DENIED"
            );
        }
    }
    drop(first);
}

#[test]
fn connect_to_unserved_name_times_out() {
    let name = unique_name("nobody");
    let start = Instant::now();
    let result = PipeClient::connect(&name, Duration::from_millis(300));
    let elapsed = start.elapsed();
    match result {
        Ok(_) => panic!("connect to an unserved name unexpectedly succeeded"),
        Err(e) => {
            println!("unserved connect error kind = {:?}, error = {e}", e.kind());
            assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        }
    }
    println!("unserved connect elapsed = {elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "connect took too long: {elapsed:?}"
    );
}

#[test]
fn sequential_clients_each_complete_handshake() {
    let name = unique_name("seq");
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            for i in 0..2 {
                let mut conn = server.accept().map_err(|e| format!("accept #{i}: {e}"))?;
                serve_hello(&mut conn).map_err(|e| format!("conn #{i}: {e}"))?;
            }
            Ok(())
        })();
        let _ = tx.send(result);
    });

    for id in [11u64, 12u64] {
        let mut conn = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
        let reply = client_hello(&mut conn, id);
        assert_hello_ack(&reply, id);
    }

    let result = finish(rx, server);
    assert!(result.is_ok(), "server side failed: {result:?}");
}

#[test]
fn concurrent_clients_each_complete_handshake() {
    let name = unique_name("conc");
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            let mut handlers = Vec::new();
            for i in 0..2 {
                let mut conn = server.accept().map_err(|e| format!("accept #{i}: {e}"))?;
                handlers.push(thread::spawn(move || serve_hello(&mut conn)));
            }
            for h in handlers {
                h.join()
                    .map_err(|_| "handler thread panicked".to_string())?
                    .map_err(|e| format!("handler: {e}"))?;
            }
            Ok(())
        })();
        let _ = tx.send(result);
    });

    let clients: Vec<_> = [21u64, 22u64]
        .into_iter()
        .map(|id| {
            let name = name.clone();
            thread::spawn(move || {
                let mut conn = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
                let reply = client_hello(&mut conn, id);
                (id, reply)
            })
        })
        .collect();

    for c in clients {
        let (id, reply) = c.join().expect("client thread panicked");
        assert_hello_ack(&reply, id);
    }

    let result = finish(rx, server);
    assert!(result.is_ok(), "server side failed: {result:?}");
}

#[test]
fn server_name_matches_bind_argument() {
    let name = unique_name("servername");
    let server = PipeServer::bind(&name).expect("bind");
    assert_eq!(server.name(), name.as_str());
}

#[test]
fn peer_close_is_observed_as_error() {
    let name = unique_name("peerclose");
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            let conn = server.accept().map_err(|e| format!("accept: {e}"))?;
            drop(conn);
            Ok(())
        })();
        let _ = tx.send(result);
    });

    let mut conn = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
    let result = read_frame::<_, Envelope<Reply>>(&mut conn);
    match &result {
        Ok(v) => panic!("read after peer close unexpectedly returned {v:?}"),
        Err(e) => println!("read after peer close error = {e}"),
    }
    assert!(
        matches!(
            result,
            Err(FrameError::Eof | FrameError::Truncated | FrameError::Io(_))
        ),
        "unexpected error variant: {result:?}"
    );

    let server_result = finish(rx, server);
    assert!(
        server_result.is_ok(),
        "server side failed: {server_result:?}"
    );
}

#[test]
fn default_pipe_name_has_expected_shape() {
    let name = default_pipe_name().expect("default_pipe_name");
    assert!(
        name.starts_with(r"\\.\pipe\clipcheck-ipc-S-1-"),
        "unexpected default pipe name: {name}"
    );
    assert!(
        !name.chars().any(char::is_whitespace),
        "default pipe name contains whitespace: {name}"
    );
}

/// Upper bound for the write-to-receive latency in the concurrency tests.
const CONCURRENT_LATENCY: Duration = Duration::from_millis(500);

/// Server writes through a clone while another clone of the same connection is blocked in a
/// read. The client receives the written frame promptly, and the blocked read still completes
/// once the client sends a frame.
#[test]
fn concurrent_read_and_write_through_server_clone() {
    let name = unique_name("srv-rw");
    let (write_tx, write_rx) = mpsc::channel::<Instant>();
    let (checked_tx, checked_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<Result<Envelope<Command>, String>>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<Envelope<Command>, String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            let mut conn = server.accept().map_err(|e| format!("accept: {e}"))?;
            let mut writer = conn.try_clone().map_err(|e| format!("clone: {e}"))?;

            // Thread X blocks in read_frame on `conn`. The client sends nothing yet.
            let (x_tx, x_rx) = mpsc::channel::<Result<Envelope<Command>, String>>();
            let x = thread::spawn(move || {
                let r = read_frame::<_, Envelope<Command>>(&mut conn)
                    .map_err(|e| format!("reader X: {e}"));
                let _ = x_tx.send(r);
            });
            // Give X time to enter its blocking read before the write happens.
            thread::sleep(Duration::from_millis(100));

            let pong = Envelope::reply(
                1,
                Reply::Pong {
                    unix_ms: 1_700_000_000_000,
                },
            );
            write_frame(&mut writer, &pong).map_err(|e| format!("write via clone: {e}"))?;
            let _ = write_tx.send(Instant::now());

            // X must still be blocked after the write has completed.
            match x_rx.recv_timeout(Duration::from_millis(300)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                other => {
                    return Err(format!(
                        "reader X returned before the client sent anything: {other:?}"
                    ))
                }
            }
            // Tell the client the check passed, so it sends its frame only after this.
            let _ = checked_tx.send(());

            // The client's frame (sent after it read the pong) unblocks X.
            let got = x_rx
                .recv_timeout(WAIT)
                .map_err(|e| format!("reader X did not unblock: {e}"))?;
            x.join().map_err(|_| "reader X panicked".to_string())?;
            got
        })();
        let _ = done_tx.send(result);
    });

    let mut client = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
    let mut client_reader = client.try_clone().expect("client try_clone");
    let (pong_tx, pong_rx) = mpsc::channel::<(Instant, Result<Envelope<Reply>, String>)>();
    let reader = thread::spawn(move || {
        let r = read_frame::<_, Envelope<Reply>>(&mut client_reader)
            .map_err(|e| format!("client read: {e}"));
        let _ = pong_tx.send((Instant::now(), r));
    });

    let (received_at, pong) = pong_rx
        .recv_timeout(WAIT)
        .expect("client did not receive the server write within the wait limit");
    let pong = pong.expect("client read of server frame failed");
    let written_at = write_rx
        .recv_timeout(WAIT)
        .expect("server did not report its write time");
    let latency = received_at.saturating_duration_since(written_at);
    println!("server write -> client receive latency = {latency:?}");
    assert!(
        latency <= CONCURRENT_LATENCY,
        "server write took {latency:?} to reach the client, limit {CONCURRENT_LATENCY:?}"
    );
    assert_eq!(pong.id, Some(1), "pushed frame id");
    assert!(
        matches!(pong.body, Reply::Pong { unix_ms } if unix_ms == 1_700_000_000_000),
        "pushed frame body: {:?}",
        pong.body
    );
    reader.join().expect("client reader panicked");

    checked_rx
        .recv_timeout(WAIT)
        .expect("server did not finish its blocked-read check");
    write_frame(&mut client, &Envelope::request(2, Command::Ping)).expect("client write request");
    let got = finish(done_rx, server).expect("server side failed");
    assert_eq!(got.id, Some(2), "request frame id seen by reader X");
    assert!(
        matches!(got.body, Command::Ping),
        "request body seen by reader X: {:?}",
        got.body
    );
}

/// Client reader blocked on one clone while the main thread writes through the other clone.
/// The server receives the frame within the latency limit and the client reader still waits.
#[test]
fn concurrent_read_and_write_through_client_clone() {
    let name = unique_name("cli-rw");
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (recv_tx, recv_rx) = mpsc::channel::<(Instant, Result<Envelope<Command>, String>)>();
    let (done_tx, done_rx) = mpsc::channel::<Result<(), String>>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            let mut conn = server.accept().map_err(|e| format!("accept: {e}"))?;
            let mut writer = conn.try_clone().map_err(|e| format!("clone: {e}"))?;

            let (y_tx, y_rx) = mpsc::channel::<(Instant, Result<Envelope<Command>, String>)>();
            thread::spawn(move || {
                let r = read_frame::<_, Envelope<Command>>(&mut conn)
                    .map_err(|e| format!("server read: {e}"));
                let _ = y_tx.send((Instant::now(), r));
            });
            let got = y_rx
                .recv_timeout(WAIT)
                .map_err(|e| format!("server did not receive the client frame: {e}"))?;
            let _ = recv_tx.send(got);

            // Reply only after the client has confirmed its reader is still blocked.
            go_rx
                .recv_timeout(WAIT)
                .map_err(|e| format!("client never sent the go signal: {e}"))?;
            let reply = Envelope::reply(5, Reply::Pong { unix_ms: 99 });
            write_frame(&mut writer, &reply).map_err(|e| format!("server reply: {e}"))?;
            Ok(())
        })();
        let _ = done_tx.send(result);
    });

    let mut conn = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
    let mut reader_conn = conn.try_clone().expect("client try_clone");
    let (r_tx, r_rx) = mpsc::channel::<Result<Envelope<Reply>, String>>();
    let reader = thread::spawn(move || {
        let r = read_frame::<_, Envelope<Reply>>(&mut reader_conn)
            .map_err(|e| format!("client reader: {e}"));
        let _ = r_tx.send(r);
    });
    // Let the reader enter its blocking read before the main thread writes.
    thread::sleep(Duration::from_millis(100));

    write_frame(&mut conn, &Envelope::request(5, Command::Ping)).expect("client write");
    let written_at = Instant::now();

    let (received_at, req) = recv_rx
        .recv_timeout(WAIT)
        .expect("server did not receive the client write within the wait limit");
    let latency = received_at.saturating_duration_since(written_at);
    println!("client write -> server receive latency = {latency:?}");
    assert!(
        latency <= CONCURRENT_LATENCY,
        "client write took {latency:?} to reach the server, limit {CONCURRENT_LATENCY:?}"
    );
    let req = req.expect("server read of client frame failed");
    assert_eq!(req.id, Some(5), "request id seen by server");
    assert!(
        matches!(req.body, Command::Ping),
        "request body: {:?}",
        req.body
    );

    match r_rx.recv_timeout(Duration::from_millis(200)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        other => panic!("client reader returned before the server replied: {other:?}"),
    }

    go_tx.send(()).expect("send go signal");
    let reply = r_rx
        .recv_timeout(WAIT)
        .expect("client reader did not unblock after the server reply")
        .expect("client reader failed");
    assert_eq!(reply.id, Some(5), "reply id seen by client reader");
    assert!(
        matches!(reply.body, Reply::Pong { unix_ms: 99 }),
        "reply body: {:?}",
        reply.body
    );
    reader.join().expect("client reader panicked");
    finish(done_rx, server).expect("server side failed");
}

/// 200 frames written from one clone are read, in order and intact, by a thread that reads
/// through another clone of the same connection.
#[test]
fn many_frames_from_one_clone_read_in_order_by_another_thread() {
    const N: u64 = 200;
    let name = unique_name("many");
    let (done_tx, done_rx) = mpsc::channel::<Result<(), String>>();
    let (client_done_tx, client_done_rx) = mpsc::channel::<()>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut server = PipeServer::bind(&server_name).map_err(|e| format!("bind: {e}"))?;
            let conn = server.accept().map_err(|e| format!("accept: {e}"))?;
            let mut writer = conn.try_clone().map_err(|e| format!("clone: {e}"))?;
            for i in 0..N {
                let frame = Envelope::reply(i, Reply::Pong { unix_ms: i as i64 });
                write_frame(&mut writer, &frame).map_err(|e| format!("write frame {i}: {e}"))?;
            }
            // Keep the pipe open until the client has read everything.
            client_done_rx
                .recv_timeout(WAIT)
                .map_err(|e| format!("client did not finish: {e}"))?;
            Ok(())
        })();
        let _ = done_tx.send(result);
    });

    let conn = PipeClient::connect(&name, CONNECT_TIMEOUT).expect("client connect");
    let mut reader_conn = conn.try_clone().expect("client try_clone");
    let (r_tx, r_rx) = mpsc::channel::<Result<Vec<(Option<u64>, i64)>, String>>();
    let reader = thread::spawn(move || {
        let mut got = Vec::new();
        for i in 0..N {
            match read_frame::<_, Envelope<Reply>>(&mut reader_conn) {
                Ok(env) => match env.body {
                    Reply::Pong { unix_ms } => got.push((env.id, unix_ms)),
                    other => {
                        let _ = r_tx.send(Err(format!("frame {i}: unexpected body {other:?}")));
                        return;
                    }
                },
                Err(e) => {
                    let _ = r_tx.send(Err(format!("frame {i}: read error {e}")));
                    return;
                }
            }
        }
        let _ = r_tx.send(Ok(got));
    });

    let got = r_rx
        .recv_timeout(WAIT)
        .expect("reader did not finish within the wait limit")
        .expect("reader failed");
    assert_eq!(got.len(), N as usize, "frame count");
    for (i, (id, unix_ms)) in got.iter().enumerate() {
        assert_eq!(*id, Some(i as u64), "frame {i} id out of order");
        assert_eq!(*unix_ms, i as i64, "frame {i} payload out of order");
    }
    reader.join().expect("reader panicked");
    let _ = client_done_tx.send(());
    finish(done_rx, server).expect("server side failed");
}
