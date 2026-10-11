//! Shared harness for the `clipcheckd` integration tests.
//!
//! Each test file pulls this in with `mod common;` and then `use common::*;`. Each test binary
//! compiles its own copy, so the `TEST_LOCK` below serializes tests within one binary only.
//! Items that a given test file does not use are allowed to be dead code.

#![allow(dead_code)]

#[cfg(windows)]
pub mod window;

use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{
    Command as Cmd, Envelope, Event, Push, Reply, Response, StatusDto, Topic,
};
use clipcheck_ipc::{PipeClient, PipeConn};
use serde_json::Value;

/// Serializes the tests. The daemon's single-instance mutex is session-global.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Takes the per-binary test lock. A poisoned lock is still usable.
pub fn lock() -> MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Daemon process and pipe helpers
// ---------------------------------------------------------------------------

/// Pids of the daemons this test binary spawned and has not yet reaped. A watchdog reads them
/// (through `live_pids`) to kill stray daemons before it exits the process.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Pids of the daemons this test binary has spawned and not yet reaped.
pub fn live_pids() -> Vec<u32> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Wraps a freshly spawned child and records its pid in `LIVE`.
fn adopt(child: Child) -> Daemon {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(child.id());
    Daemon { child }
}

/// Owns a running daemon child. Kills and reaps it on drop.
pub struct Daemon {
    pub child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let pid = self.child.id();
        LIVE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| *p != pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Returns a pipe name that no other test in this process has used.
pub fn unique_pipe() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_nanos();
    format!(
        r"\\.\pipe\clipcheck-test-{}-{}-{}",
        std::process::id(),
        n,
        nanos
    )
}

/// Spawns `clipcheckd --data-dir <dir> --pipe-name <pipe>`.
pub fn spawn(data_dir: &Path, pipe: &str) -> Daemon {
    spawn_with(data_dir, pipe, &[])
}

/// Spawns `clipcheckd --data-dir <dir> --pipe-name <pipe>` plus any `extra` arguments.
pub fn spawn_with(data_dir: &Path, pipe: &str, extra: &[&str]) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_clipcheckd"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--pipe-name")
        .arg(pipe)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn clipcheckd");
    adopt(child)
}

/// Spawns the daemon with `CLIPCHECK_LOG` removed from the inherited environment, then applies
/// `envs`. When `stderr_to` is set, daemon stderr goes to that file; otherwise it is discarded.
pub fn spawn_env(
    data_dir: &Path,
    pipe: &str,
    envs: &[(&str, &str)],
    stderr_to: Option<&Path>,
) -> Daemon {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_clipcheckd"));
    cmd.arg("--data-dir")
        .arg(data_dir)
        .arg("--pipe-name")
        .arg(pipe)
        .env_remove("CLIPCHECK_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    match stderr_to {
        Some(path) => {
            let file = fs::File::create(path).expect("create stderr capture file");
            cmd.stderr(Stdio::from(file));
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }
    adopt(cmd.spawn().expect("failed to spawn clipcheckd"))
}

/// Polls the pipe until a connection succeeds. Fails fast if the daemon exits meanwhile.
pub fn connect(d: &mut Daemon, pipe: &str) -> PipeConn {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = d.child.try_wait().expect("try_wait failed") {
            panic!("daemon exited early with {status:?} while waiting for pipe {pipe}");
        }
        match PipeClient::connect(pipe, Duration::from_millis(500)) {
            Ok(conn) => return conn,
            Err(e) => {
                if Instant::now() >= deadline {
                    panic!("could not connect to {pipe}: {e}");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Waits for the daemon to exit. On timeout it kills the daemon and panics.
pub fn wait_exit(d: &mut Daemon, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = d.child.try_wait().expect("try_wait failed") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = d.child.kill();
            let _ = d.child.wait();
            panic!("daemon did not exit within {timeout:?} after Shutdown; it was killed");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The Hello request for protocol 1, as sent by the test client.
pub fn hello_cmd() -> Cmd {
    Cmd::Hello {
        proto_min: 1,
        proto_max: 1,
        client: "test".into(),
        client_version: "0.0.0".into(),
    }
}

/// Connects and completes the handshake (Hello with id 1). Panics unless the daemon acks proto 1.
pub fn connect_and_hello(d: &mut Daemon, pipe: &str) -> PipeConn {
    let mut conn = connect(d, pipe);
    let resp = call(&mut conn, 1, hello_cmd());
    match resp.body {
        Reply::HelloAck { proto, .. } => assert_eq!(proto, 1, "negotiated protocol"),
        other => panic!("expected HelloAck, got {other:?}"),
    }
    conn
}

/// Sends Ping with `id` on a raw connection and asserts a Pong reply.
pub fn assert_pong(conn: &mut PipeConn, id: u64) {
    match call(conn, id, Cmd::Ping).body {
        Reply::Pong { .. } => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}

/// Sends one request frame on a raw connection.
pub fn send_frame(conn: &mut PipeConn, cmd_id: u64, cmd: Cmd) {
    write_frame(conn, &Envelope::request(cmd_id, cmd)).expect("write request frame");
}

/// Reads one reply frame from a raw connection.
pub fn recv_frame(conn: &mut PipeConn) -> Response {
    read_frame(conn).expect("read reply frame")
}

/// Sends a request with the given id and returns the reply, checking the id is echoed.
/// Only valid while no reader thread owns a clone of this connection.
pub fn call(conn: &mut PipeConn, id: u64, cmd: Cmd) -> Response {
    send_frame(conn, id, cmd);
    let resp = recv_frame(conn);
    assert_eq!(
        resp.id,
        Some(id),
        "reply id does not echo request id: {resp:?}"
    );
    resp
}

/// Reads `<data_dir>/logs/clipcheckd.log` for failure messages.
pub fn log_text(data_dir: &Path) -> String {
    fs::read_to_string(data_dir.join("logs").join("clipcheckd.log"))
        .unwrap_or_else(|e| format!("<cannot read daemon log: {e}>"))
}

/// Polls the daemon log until it contains `needle` or `timeout` passes. Returns whether it
/// appeared. Use it as the signal before a negative assertion, instead of a fixed sleep.
pub fn wait_for_log(data_dir: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if log_text(data_dir).contains(needle) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The one way a test reports that native test media (the H.264/AAC encoders in
/// `clipcheck-testkit`) is unavailable. It prints a `SKIPPED:` line naming the test, and the
/// caller returns. When `CLIPCHECK_REQUIRE_CAPTURE` is set (CI), unavailable media is a failure
/// instead of a skip.
pub fn skip_no_encoders(test: &str) {
    if std::env::var_os("CLIPCHECK_REQUIRE_CAPTURE").is_some() {
        panic!("{test}: native test media unavailable (clipcheck_testkit::available() is false) and CLIPCHECK_REQUIRE_CAPTURE is set");
    }
    eprintln!("SKIPPED: {test}: native test media unavailable");
}

/// Lowercased file name of this test executable, used as the allowlisted game.
pub fn test_exe_name() -> String {
    std::env::current_exe()
        .expect("current_exe failed")
        .file_name()
        .expect("test exe has no file name")
        .to_string_lossy()
        .to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// IPC client
// ---------------------------------------------------------------------------

pub const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// A handshaken connection to the daemon, with an event queue.
///
/// The connection is split in two. `conn` is the write side, used only for requests. A reader
/// thread owns a clone and forwards every incoming frame over `frames`. Events are therefore
/// received whenever the daemon pushes them, even while the client is idle. No keep-alive
/// frames are sent to receive events.
pub struct Client {
    pub conn: PipeConn,
    /// Frames from the reader thread. `Err` carries the reason the connection ended.
    pub frames: mpsc::Receiver<Result<Value, String>>,
    pub next_id: u64,
    /// Events read from the pipe that no wait has consumed yet.
    pub pending: VecDeque<Event>,
    /// Every event read so far, for failure messages.
    pub seen: Vec<Event>,
}

impl Client {
    /// Connects and completes the Hello handshake (request id 1), then starts the reader thread.
    pub fn handshake(d: &mut Daemon, pipe: &str) -> Client {
        let mut conn = connect(d, pipe);
        write_frame(
            &mut conn,
            &Envelope::request(
                1,
                Cmd::Hello {
                    proto_min: 1,
                    proto_max: 1,
                    client: "test".into(),
                    client_version: "0.0.0".into(),
                },
            ),
        )
        .expect("write Hello");
        let resp: Response = read_frame(&mut conn).expect("read HelloAck");
        match resp.body {
            Reply::HelloAck { proto, .. } => assert_eq!(proto, 1, "negotiated protocol"),
            other => panic!("expected HelloAck, got {other:?}"),
        }

        let mut reader_conn = conn
            .try_clone()
            .expect("clone the IPC connection for the reader thread");
        let (tx, frames) = mpsc::channel::<Result<Value, String>>();
        thread::spawn(move || loop {
            match read_frame::<_, Value>(&mut reader_conn) {
                Ok(frame) => {
                    if tx.send(Ok(frame)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    break;
                }
            }
        });

        Client {
            conn,
            frames,
            next_id: 2,
            pending: VecDeque::new(),
            seen: Vec::new(),
        }
    }

    /// Waits for the next incoming frame until `deadline`. Returns `None` on timeout.
    /// Panics if the connection has failed.
    pub fn next_frame(&mut self, deadline: Instant, what: &str) -> Option<Value> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.frames.recv_timeout(wait) {
            Ok(Ok(frame)) => Some(frame),
            Ok(Err(e)) => panic!("connection to the daemon ended while waiting for {what}: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("reader thread stopped while waiting for {what}")
            }
        }
    }

    /// Queues an incoming event frame. Panics if the frame is a reply, since no request is pending.
    pub fn queue_event(&mut self, frame: Value) {
        let is_reply = frame.get("id").is_some_and(|v| !v.is_null());
        assert!(
            !is_reply,
            "reply frame with no request outstanding: {frame}"
        );
        let push: Push = serde_json::from_value(frame).expect("malformed event frame");
        self.seen.push(push.body.clone());
        self.pending.push_back(push.body);
    }

    /// Sends one request and returns its reply. Event frames read before the reply are queued.
    pub fn call(&mut self, cmd: Cmd) -> Reply {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(&mut self.conn, &Envelope::request(id, cmd)).expect("write request frame");
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let frame = self
                .next_frame(deadline, "a reply")
                .unwrap_or_else(|| panic!("no reply to request {id} within {REPLY_TIMEOUT:?}"));
            let is_reply = frame.get("id").is_some_and(|v| !v.is_null());
            if is_reply {
                let resp: Response = serde_json::from_value(frame).expect("malformed reply frame");
                assert_eq!(
                    resp.id,
                    Some(id),
                    "reply id does not echo request id: {resp:?}"
                );
                return resp.body;
            }
            self.queue_event(frame);
        }
    }

    pub fn ping(&mut self) {
        match self.call(Cmd::Ping) {
            Reply::Pong { .. } => {}
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    pub fn subscribe(&mut self, topics: Vec<Topic>) {
        match self.call(Cmd::Subscribe { topics }) {
            Reply::Subscribed { .. } => {}
            other => panic!("expected Subscribed, got {other:?}"),
        }
    }

    pub fn subscribe_all(&mut self) {
        self.subscribe(vec![
            Topic::Clips,
            Topic::Sessions,
            Topic::State,
            Topic::Warnings,
            Topic::Health,
        ]);
    }

    /// Waits until the daemon reports the test exe as the attached game. Checks `status` first:
    /// a game that was already confirmed before this client subscribed is announced only in
    /// the past, so the event alone would be a race (this is how the real UI recovers too).
    pub fn wait_game(&mut self, exe: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status();
            if status
                .game_info
                .as_ref()
                .is_some_and(|g| g.exe.eq_ignore_ascii_case(exe))
            {
                return;
            }
            if self.pending.iter().any(
                |e| matches!(e, Event::GameDetected { game } if game.exe.eq_ignore_ascii_case(exe)),
            ) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {timeout:?} waiting for {exe} to be attached; last status: {status:?}"
            );
            self.pump_for(Duration::from_millis(250));
        }
    }

    pub fn status(&mut self) -> Box<StatusDto> {
        match self.call(Cmd::GetStatus) {
            Reply::Status(s) => s,
            other => panic!("expected Status, got {other:?}"),
        }
    }

    /// Sends a command that must answer `Ok`.
    pub fn expect_ok(&mut self, cmd: Cmd, what: &str) {
        match self.call(cmd) {
            Reply::Ok => {}
            other => panic!("{what}: expected Ok, got {other:?}"),
        }
    }

    /// Collects incoming events for `dur`. Sends nothing, so the daemon must push on its own.
    pub fn pump_for(&mut self, dur: Duration) {
        let deadline = Instant::now() + dur;
        while let Some(frame) = self.next_frame(deadline, "pumped events") {
            self.queue_event(frame);
        }
    }

    /// Returns the first queued or incoming event matching `pred`. Sends nothing while it waits.
    /// Fails with the timeout, a list of warnings, and the number of events seen.
    pub fn wait_event(
        &mut self,
        timeout: Duration,
        what: &str,
        pred: impl Fn(&Event) -> bool,
    ) -> Event {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(i) = self.pending.iter().position(&pred) {
                return self.pending.remove(i).expect("index was just found");
            }
            match self.next_frame(deadline, what) {
                Some(frame) => self.queue_event(frame),
                None => {
                    let problems: Vec<&Event> = self
                        .seen
                        .iter()
                        .filter(|e| matches!(e, Event::Warning { .. } | Event::Degraded { .. }))
                        .collect();
                    panic!(
                        "timed out after {timeout:?} waiting for {what}; {} events seen: {:?}; warnings/degraded: {problems:?}",
                        self.seen.len(),
                        self.seen
                    );
                }
            }
        }
    }

    /// Returns `(config, config_version, path)` from `GetConfig`.
    pub fn get_config(&mut self) -> (Value, u32, String) {
        match self.call(Cmd::GetConfig) {
            Reply::Config {
                config,
                config_version,
                path,
            } => (config, config_version, path),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    /// The `config` object from `GetConfig`.
    pub fn config(&mut self) -> Value {
        self.get_config().0
    }

    /// Number of `clip_saved` events seen so far.
    pub fn clip_saved_count(&self) -> usize {
        self.seen
            .iter()
            .filter(|e| matches!(e, Event::ClipSaved { .. }))
            .count()
    }
}

// ---------------------------------------------------------------------------
// PNG and base64 helpers
// ---------------------------------------------------------------------------

/// Decodes standard base64 (no `data:` prefix allowed). Test-only helper.
pub fn b64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    assert!(
        !s.starts_with("data:"),
        "icon must not carry a data: prefix"
    );
    assert_eq!(s.len() % 4, 0, "base64 length is not a multiple of 4");
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = T
            .iter()
            .position(|&x| x == c)
            .unwrap_or_else(|| panic!("invalid base64 byte {c:#x}")) as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// Asserts the bytes are a PNG whose IHDR says 32x32.
pub fn assert_png_32(png: &[u8]) {
    assert!(png.len() > 24, "too short to be a PNG: {} bytes", png.len());
    assert_eq!(
        &png[0..8],
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        "PNG signature"
    );
    assert_eq!(&png[12..16], b"IHDR", "first chunk is not IHDR");
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!((w, h), (32, 32), "icon size");
}
