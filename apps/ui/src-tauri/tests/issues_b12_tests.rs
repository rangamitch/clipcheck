//! Regression tests for the UI-side fixes in issues #82, #89, #94, #96, #139 and #179.
//!
//! Daemon-facing tests use a fake daemon: a raw `PipeServer` on a unique pipe that answers
//! only the commands a test needs. Nothing here spawns the real recorder on the user's
//! default pipe. The one test that spawns a daemon (issue 82, spawn path) uses the built
//! `clipcheck-ui` binary with a unique pipe and a temp data dir, and shuts it down on drop.
//!
//! `CLIPCHECK_PIPE_NAME` is process-wide, so every test takes `TEST_LOCK` and restores the
//! variable through `EnvGuard`.

use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{Command as Cmd, Envelope, Reply};
use clipcheck_ipc::{default_pipe_name, PipeClient, PipeServer, HANDSHAKE_TIMEOUT};
use clipcheck_ui_lib::daemon::{
    ensure_daemon_running_from, resolve_pipe_name, DaemonStart, PIPE_NAME_ENV,
};
use clipcheck_ui_lib::ipc::{request_shutdown, ClientError, DaemonClient, ShutdownRequest};
use serde_json::Value;

/// How long a freshly started client or daemon may take to connect.
const CONNECT_WAIT: Duration = Duration::from_secs(15);

/// Config files pinned by the version and dependency checks (issue 139).
const TAURI_CONF: &str = include_str!("../tauri.conf.json");
const CARGO_TOML: &str = include_str!("../Cargo.toml");
const PACKAGE_JSON: &str = include_str!("../../package.json");

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn unique_pipe() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_nanos();
    format!(
        r"\\.\pipe\clipcheck-ui-b12-test-{}-{}-{}",
        std::process::id(),
        n,
        nanos
    )
}

/// Sets an environment variable for the lifetime of the guard and restores the old value.
struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> EnvGuard {
        let previous = env::var_os(key);
        env::set_var(key, value);
        EnvGuard { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}

/// Polls `f` until it returns true or `timeout` passes.
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Best-effort Hello plus Shutdown over a fresh connection. Errors are ignored.
fn shutdown_quietly(pipe: &str) {
    let Ok(mut conn) = PipeClient::connect(pipe, Duration::from_millis(500)) else {
        return;
    };
    let hello = Cmd::Hello {
        proto_min: 1,
        proto_max: 1,
        client: "test".into(),
        client_version: "0.0.0".into(),
    };
    if write_frame(&mut conn, &Envelope::request(1, hello)).is_err() {
        return;
    }
    if read_frame::<_, Value>(&mut conn).is_err() {
        return;
    }
    if write_frame(
        &mut conn,
        &Envelope::request(
            2,
            Cmd::Shutdown {
                reason: None,
                mode: None,
            },
        ),
    )
    .is_err()
    {
        return;
    }
    let _ = read_frame::<_, Value>(&mut conn);
}

/// Shuts down a real daemon on `pipe` when dropped.
struct ShutdownOnDrop(String);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        shutdown_quietly(&self.0);
    }
}

/// What the fake daemon answers. Every command is recorded whether or not it is answered.
#[derive(Clone, Copy)]
enum Answers {
    /// Never replies to anything.
    Nothing,
    /// Answers Hello and Subscribe, then goes silent.
    Handshake,
    /// Answers the handshake and Shutdown (with ShutdownAck), then goes silent.
    HandshakeAndShutdown,
}

struct FakeDaemon {
    /// Every command received, in order.
    commands: mpsc::Receiver<Cmd>,
    /// One message when the client closes its end of the pipe.
    closed: mpsc::Receiver<()>,
}

/// Binds `pipe` and serves one connection on a background thread.
fn spawn_fake_daemon(pipe: &str, answers: Answers) -> FakeDaemon {
    let (cmd_tx, commands) = mpsc::channel();
    let (closed_tx, closed) = mpsc::channel();
    let name = pipe.to_owned();
    thread::spawn(move || {
        let mut server = PipeServer::bind(&name).expect("bind fake daemon pipe");
        let Ok(mut conn) = server.accept() else {
            return;
        };
        while let Ok(frame) = read_frame::<_, Envelope<Cmd>>(&mut conn) {
            let id = frame.id.unwrap_or(0);
            let reply = match (&frame.body, answers) {
                (Cmd::Hello { .. }, Answers::Handshake | Answers::HandshakeAndShutdown) => {
                    Some(Reply::HelloAck {
                        proto: 1,
                        daemon_version: "t".into(),
                        daemon_pid: 0,
                        features: vec![],
                    })
                }
                (Cmd::Subscribe { .. }, Answers::Handshake | Answers::HandshakeAndShutdown) => {
                    Some(Reply::Subscribed { topics: vec![] })
                }
                (Cmd::Shutdown { .. }, Answers::HandshakeAndShutdown) => Some(Reply::ShutdownAck),
                _ => None,
            };
            let _ = cmd_tx.send(frame.body);
            if let Some(body) = reply {
                if write_frame(&mut conn, &Envelope::reply(id, body)).is_err() {
                    break;
                }
            }
        }
        let _ = closed_tx.send(());
    });
    FakeDaemon { commands, closed }
}

type Connections = Arc<Mutex<Vec<bool>>>;

/// Starts a `DaemonClient` with `pipe` (None = environment or default) that records
/// connection changes.
fn start_recording(pipe: Option<String>) -> (DaemonClient, Connections) {
    let conns: Connections = Arc::new(Mutex::new(Vec::new()));
    let cn = Arc::clone(&conns);
    let client = DaemonClient::start(
        pipe,
        Box::new(|_| {}),
        Box::new(move |c| cn.lock().unwrap_or_else(|e| e.into_inner()).push(c)),
    );
    (client, conns)
}

fn saw_connected(conns: &Connections) -> bool {
    conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&true)
}

fn saw_disconnected(conns: &Connections) -> bool {
    conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&false)
}

// ---------------------------------------------------------------------------
// #82: one rule for CLIPCHECK_PIPE_NAME, shared by the spawn path and the client
// ---------------------------------------------------------------------------

#[test]
fn issue_82_empty_pipe_env_counts_as_unset() {
    let _g = lock();
    let _env = EnvGuard::set(PIPE_NAME_ENV, "");
    let default = default_pipe_name().expect("default pipe name");

    assert_eq!(resolve_pipe_name(None).expect("resolve"), default);
    assert_eq!(resolve_pipe_name(Some("")).expect("resolve"), default);
    assert_eq!(
        resolve_pipe_name(Some("explicit")).expect("resolve"),
        "explicit"
    );
}

#[test]
fn issue_82_env_pipe_is_used_when_explicit_is_empty() {
    let _g = lock();
    let pipe = unique_pipe();
    let _env = EnvGuard::set(PIPE_NAME_ENV, &pipe);

    assert_eq!(resolve_pipe_name(Some("")).expect("resolve"), pipe);
    assert_eq!(resolve_pipe_name(None).expect("resolve"), pipe);
}

#[test]
fn issue_82_client_connects_to_env_pipe_when_explicit_is_empty() {
    let _g = lock();
    let pipe = unique_pipe();
    let _env = EnvGuard::set(PIPE_NAME_ENV, &pipe);
    let fake = spawn_fake_daemon(&pipe, Answers::Handshake);

    let (client, conns) = start_recording(Some(String::new()));
    assert!(
        wait_until(CONNECT_WAIT, || saw_connected(&conns)),
        "client with an empty explicit pipe did not connect to the CLIPCHECK_PIPE_NAME pipe"
    );
    client.stop();
    let _ = fake.closed.recv_timeout(Duration::from_secs(5));
}

#[test]
fn issue_82_spawn_path_listens_on_env_pipe_when_explicit_is_empty() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let _env = EnvGuard::set(PIPE_NAME_ENV, &pipe);
    // Declared after `dir`, so the daemon is shut down before the directory is removed.
    let _shutdown = ShutdownOnDrop(pipe.clone());
    let exe = Path::new(env!("CARGO_BIN_EXE_clipcheck-ui"));
    let data = dir.path().to_str().expect("temp path is UTF-8");

    match ensure_daemon_running_from(exe, Some(data), Some("")) {
        Ok(DaemonStart::Spawned { .. }) => {}
        Ok(DaemonStart::Already) => panic!("a daemon already answered on the test pipe"),
        Err(e) => panic!("ensure_daemon_running_from with empty explicit pipe failed: {e}"),
    }
    // The spawned daemon must listen on the resolved name, the same one the client uses.
    let conn = PipeClient::connect(&pipe, Duration::from_secs(2));
    assert!(
        conn.is_ok(),
        "spawned daemon is not answering on the CLIPCHECK_PIPE_NAME pipe"
    );
}

// ---------------------------------------------------------------------------
// #89: a pipe name that cannot be resolved is reported, not silently dropped
// ---------------------------------------------------------------------------

#[test]
fn issue_89_try_start_connects_for_resolvable_pipe() {
    let _g = lock();
    let pipe = unique_pipe();
    let fake = spawn_fake_daemon(&pipe, Answers::Handshake);

    let client = DaemonClient::try_start(Some(pipe.clone()), Box::new(|_| {}), Box::new(|_| {}))
        .expect("try_start with an explicit pipe must succeed");
    assert!(
        wait_until(CONNECT_WAIT, || client.is_connected()),
        "try_start client did not connect to the explicit pipe"
    );
    client.stop();
    let _ = fake.closed.recv_timeout(Duration::from_secs(5));
}

#[test]
fn issue_89_try_start_treats_empty_explicit_pipe_as_unset() {
    let _g = lock();
    let pipe = unique_pipe();
    let _env = EnvGuard::set(PIPE_NAME_ENV, &pipe);
    let fake = spawn_fake_daemon(&pipe, Answers::Handshake);

    let client = DaemonClient::try_start(Some(String::new()), Box::new(|_| {}), Box::new(|_| {}))
        .expect("an empty explicit pipe falls back to the environment, which resolves");
    assert!(
        wait_until(CONNECT_WAIT, || client.is_connected()),
        "try_start client did not fall back to CLIPCHECK_PIPE_NAME"
    );
    client.stop();
    let _ = fake.closed.recv_timeout(Duration::from_secs(5));
}

// SKIPPED (#89 error path): `try_start` returns Err only when `default_pipe_name()` fails,
// which means `current_user_sid()` (a Win32 call) failed. A test cannot force that through
// the public API without faking the OS, so only the success path is covered here.

// ---------------------------------------------------------------------------
// #94 and #179: a failed handshake and a stopped client both release the reader's pipe
// ---------------------------------------------------------------------------

#[test]
fn issue_94_failed_handshake_releases_pipe_reader() {
    let _g = lock();
    let pipe = unique_pipe();
    // The daemon accepts the pipe and never answers, so the Hello times out.
    let fake = spawn_fake_daemon(&pipe, Answers::Nothing);

    let (client, conns) = start_recording(Some(pipe.clone()));
    match fake.commands.recv_timeout(CONNECT_WAIT) {
        Ok(Cmd::Hello { .. }) => {}
        Ok(_) => panic!("first command after connect was not Hello"),
        Err(_) => panic!("daemon never received a Hello"),
    }

    // The client gives up after HANDSHAKE_TIMEOUT and closes the link. The reader thread
    // must not keep the pipe open: the daemon side sees the close.
    let closed = fake
        .closed
        .recv_timeout(HANDSHAKE_TIMEOUT + Duration::from_secs(5));
    assert!(
        closed.is_ok(),
        "pipe stayed open after a failed handshake: the reader thread is still blocked"
    );
    assert!(
        !saw_connected(&conns),
        "on_connection(true) fired without a completed handshake"
    );
    assert!(!client.is_connected());
    client.stop();
}

#[test]
fn issue_179_stopping_client_cancels_reader_of_live_link() {
    let _g = lock();
    let pipe = unique_pipe();
    let fake = spawn_fake_daemon(&pipe, Answers::Handshake);

    let (client, conns) = start_recording(Some(pipe.clone()));
    assert!(
        wait_until(CONNECT_WAIT, || saw_connected(&conns)),
        "client did not complete the handshake with the fake daemon"
    );

    // The reader is now blocked in a pipe read that nothing answers. stop() must cancel it.
    let t0 = Instant::now();
    client.stop();
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "stop() did not return promptly"
    );
    let closed = fake.closed.recv_timeout(Duration::from_secs(5));
    assert!(
        closed.is_ok(),
        "reader still holds the pipe after stop(): pending read was not cancelled"
    );
    // A deliberate stop is not a lost connection, so the owner is not told it dropped
    // (`retire` in ipc.rs reports the loss only while the client is not stopping).
    assert!(
        !saw_disconnected(&conns),
        "on_connection(false) fired for a deliberate stop, which is not a lost connection"
    );
}

// ---------------------------------------------------------------------------
// #96: one shared shutdown request with a 3 s ack wait
// ---------------------------------------------------------------------------

#[test]
fn issue_96_request_shutdown_sends_shutdown_and_accepts_ack() {
    let _g = lock();
    let pipe = unique_pipe();
    let fake = spawn_fake_daemon(&pipe, Answers::HandshakeAndShutdown);

    let (client, conns) = start_recording(Some(pipe.clone()));
    assert!(
        wait_until(CONNECT_WAIT, || saw_connected(&conns)),
        "client did not connect to the fake daemon"
    );

    let body =
        request_shutdown(&client, ShutdownRequest::Force).expect("shutdown must be acknowledged");
    assert_ne!(
        body.get("type").and_then(Value::as_str),
        Some("error"),
        "shutdown answered with an error body: {body}"
    );

    let mut saw_shutdown = false;
    while let Ok(cmd) = fake.commands.recv_timeout(Duration::from_secs(2)) {
        if matches!(cmd, Cmd::Shutdown { .. }) {
            saw_shutdown = true;
            break;
        }
    }
    assert!(saw_shutdown, "daemon never received a Shutdown command");
    client.stop();
}

#[test]
fn issue_96_request_shutdown_times_out_after_three_seconds() {
    let _g = lock();
    let pipe = unique_pipe();
    // Answers the handshake but never acknowledges Shutdown.
    let fake = spawn_fake_daemon(&pipe, Answers::Handshake);

    let (client, conns) = start_recording(Some(pipe.clone()));
    assert!(
        wait_until(CONNECT_WAIT, || saw_connected(&conns)),
        "client did not connect to the fake daemon"
    );

    let t0 = Instant::now();
    let res = request_shutdown(&client, ShutdownRequest::Force);
    let elapsed = t0.elapsed();
    assert_eq!(
        res,
        Err(ClientError::Timeout),
        "expected a shutdown timeout"
    );
    assert!(
        elapsed >= Duration::from_millis(2500) && elapsed < Duration::from_secs(6),
        "shutdown wait took {elapsed:?}, expected about 3 s"
    );
    client.stop();
    let _ = fake.closed.recv_timeout(Duration::from_secs(5));
}

// ---------------------------------------------------------------------------
// #139: one version source and no empty feature lists in the manifests
// ---------------------------------------------------------------------------

#[test]
fn issue_139_tauri_conf_does_not_pin_a_second_version() {
    let conf: Value = serde_json::from_str(TAURI_CONF).expect("tauri.conf.json is valid JSON");
    match conf.get("version") {
        // Absent: Tauri takes the version from Cargo.toml.
        None => {}
        Some(v) => assert_eq!(
            v.as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "tauri.conf.json pins a version that can drift from Cargo.toml"
        ),
    }
}

#[test]
fn issue_139_frontend_package_version_matches_cargo_version() {
    let pkg: Value = serde_json::from_str(PACKAGE_JSON).expect("package.json is valid JSON");
    assert_eq!(
        pkg.get("version").and_then(Value::as_str),
        Some(env!("CARGO_PKG_VERSION")),
        "apps/ui/package.json version differs from the Cargo version"
    );
}

#[test]
fn issue_139_cargo_manifest_has_no_empty_feature_lists() {
    // The Tauri CLI rewrites the `tauri` and `tauri-build` dependency lines (with an empty
    // `features = []`) on every `tauri dev`/`tauri build`, so those two are exempt; any
    // other empty feature list is still a mistake.
    let offenders: Vec<&str> = CARGO_TOML
        .lines()
        .filter(|l| l.contains("features = []"))
        .filter(|l| {
            let name = l.split('=').next().unwrap_or("").trim();
            name != "tauri" && name != "tauri-build"
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "Cargo.toml has an empty features list, which does nothing: {offenders:?}"
    );
}

// SKIPPED (#139): the duplicate .gitignore entries are not changed in the working tree, and
// removing CREATE_NO_WINDOW in daemon.rs is a private, Windows-only flag with no observable
// behaviour through the public API.
