//! Issue #179 tests for `PipeConn::cancel_pending`. Black-box: public API only.
//!
//! Every blocking step runs under a watchdog, so a hang fails the test instead
//! of blocking the run. Pipe names include the process id and a counter.
#![cfg(windows)]

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::pipe::{PipeClient, PipeConn, PipeServer};

/// Raw Win32 code for ERROR_OPERATION_ABORTED, the error a cancelled read returns.
const ERROR_OPERATION_ABORTED: i32 = 995;
/// Upper bound for any single blocking step.
const WATCHDOG: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the blocked reader is given to block before the cancel is sent.
const SETTLE: Duration = Duration::from_millis(200);

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A pipe name that no other test or process uses.
fn unique_name(tag: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        r"\\.\pipe\clipcheck-test-{}-179-{}-{}-{}",
        std::process::id(),
        tag,
        n,
        nanos
    )
}

/// Binds a unique pipe on a helper thread, accepts one client there, and connects
/// one client here. Returns (server side, client side). The server side must stay
/// alive for the test, because dropping it closes the pipe.
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

/// Reads up to `len` bytes on a helper thread, bounded by the watchdog.
/// Returns the connection back to the caller along with the result.
fn read_bounded(mut conn: PipeConn, len: usize) -> (PipeConn, io::Result<Vec<u8>>) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = vec![0u8; len];
        let r = conn.read(&mut buf).map(|n| {
            buf.truncate(n);
            buf
        });
        let _ = tx.send((conn, r));
    });
    rx.recv_timeout(WATCHDOG)
        .expect("read did not return within the watchdog")
}

/// (a) A reader blocked on a clone returns 995 promptly after cancel on the original.
#[test]
fn cancel_while_reader_blocked_returns_995_promptly() {
    let (_srv, cli) = pair("blocked");
    let mut reader = cli.try_clone().expect("clone");
    let (tx, rx) = mpsc::channel::<io::Result<usize>>();
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 16];
        let _ = tx.send(reader.read(&mut buf));
    });

    // The server never writes, so the reader must still be blocked here.
    thread::sleep(SETTLE);
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "reader returned before cancel; it was not blocked"
    );

    let started = Instant::now();
    cli.cancel_pending().expect("cancel_pending must succeed");
    let result = rx
        .recv_timeout(WATCHDOG)
        .expect("reader still blocked after cancel_pending");
    let elapsed = started.elapsed();

    let err = result.expect_err("a cancelled read must fail");
    assert_eq!(
        err.raw_os_error(),
        Some(ERROR_OPERATION_ABORTED),
        "expected ERROR_OPERATION_ABORTED (995), got {err:?}"
    );
    assert!(
        elapsed < WATCHDOG,
        "cancel took {elapsed:?}, longer than the watchdog"
    );
    handle.join().expect("reader thread exits");
}

/// (b) Cancel before any read: the read fails with 995 and does not block.
#[test]
fn cancel_before_read_fails_read_with_995_without_blocking() {
    let (_srv, cli) = pair("before");
    cli.cancel_pending().expect("cancel_pending must succeed");

    let started = Instant::now();
    let (_conn, result) = read_bounded(cli, 16);
    let elapsed = started.elapsed();

    let err = result.expect_err("a read after cancel must fail");
    assert_eq!(
        err.raw_os_error(),
        Some(ERROR_OPERATION_ABORTED),
        "expected 995, got {err:?}"
    );
    assert!(
        elapsed < WATCHDOG,
        "read after cancel took {elapsed:?}, so it blocked"
    );
}

/// (c) A read that completed with data before the cancel keeps its data, and the
/// next read fails with 995. (Data written before the cancel that no read has
/// consumed yet is NOT delivered, because the sticky flag fails later reads at once.)
#[test]
fn completed_read_keeps_data_and_next_read_fails_after_cancel() {
    let (mut srv, cli) = pair("data");
    srv.write_all(b"abc").expect("server write");
    srv.flush().expect("server flush");

    let (cli, first) = read_bounded(cli, 3);
    let first = first.expect("read of data written before cancel must succeed");
    assert_eq!(first, b"abc", "completed read must keep its data");

    cli.cancel_pending().expect("cancel_pending must succeed");

    let (_conn, second) = read_bounded(cli, 3);
    let err = second.expect_err("the read after cancel must fail");
    assert_eq!(
        err.raw_os_error(),
        Some(ERROR_OPERATION_ABORTED),
        "expected 995 on the read after cancel, got {err:?}"
    );
}

/// (d) Repeated cancel returns Ok, on the original and on a clone, and the sticky
/// flag then applies to the shared instance.
#[test]
fn double_cancel_is_ok_and_applies_to_shared_instance() {
    let (_srv, cli) = pair("double");
    let clone = cli.try_clone().expect("clone");

    clone.cancel_pending().expect("first cancel must be Ok");
    clone.cancel_pending().expect("second cancel must be Ok");
    cli.cancel_pending()
        .expect("cancel on the original must be Ok");

    let (_conn, result) = read_bounded(cli, 8);
    let err = result.expect_err("read on the shared instance must fail after cancel");
    assert_eq!(
        err.raw_os_error(),
        Some(ERROR_OPERATION_ABORTED),
        "expected 995, got {err:?}"
    );
}
