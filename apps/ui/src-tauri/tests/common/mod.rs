//! Helpers shared by the daemon integration tests (`closing_tests`, `ipc_client_tests`,
//! `single_exe_tests`). Test-only; never used by src/. Each test binary includes this module
//! with `mod common;`, so each binary gets its own `TEST_LOCK`.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{Command as Cmd, Envelope};
use clipcheck_ipc::PipeClient;
use clipcheck_ui_lib::daemon::DAEMON_FLAG;
use clipcheck_ui_lib::ipc::DaemonClient;
use serde_json::Value;

/// How long a freshly spawned daemon may take to connect a client.
pub const CONNECT_WAIT: Duration = Duration::from_secs(15);

/// The daemon's single-instance mutex is session-global, so every test that starts a daemon
/// takes this lock first.
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Events pushed by a `DaemonClient`, in arrival order.
pub type Events = Arc<Mutex<Vec<Value>>>;
/// Connection changes reported by a `DaemonClient` (`true` = connected).
pub type Conns = Arc<Mutex<Vec<bool>>>;

/// A pipe name that no other test or daemon uses. `prefix` names the test file.
pub fn unique_pipe(prefix: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_nanos();
    format!(r"\\.\pipe\{prefix}-{}-{}-{}", std::process::id(), n, nanos)
}

/// Polls `cond` every 50 ms until it returns true or `timeout` passes. Returns whether
/// `cond` became true. Use this for every positive wait.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The `hello` command every test client sends first.
pub fn hello_cmd() -> Cmd {
    Cmd::Hello {
        proto_min: 1,
        proto_max: 1,
        client: "test".into(),
        client_version: "0.0.0".into(),
    }
}

/// Best-effort Hello plus Shutdown over a fresh connection. Errors are ignored.
pub fn shutdown_quietly(pipe: &str) {
    let Ok(mut conn) = PipeClient::connect(pipe, Duration::from_millis(500)) else {
        return;
    };
    if write_frame(&mut conn, &Envelope::request(1, hello_cmd())).is_err() {
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

/// Owns a daemon child started with `--daemon`. Shuts it down over IPC on drop, then kills
/// it if it has not exited within three seconds.
pub struct Daemon {
    pub child: Child,
    pub pipe: String,
}

impl Daemon {
    pub fn spawn(data_dir: &Path, pipe: &str) -> Daemon {
        let child = Command::new(env!("CARGO_BIN_EXE_clipcheck-ui"))
            .arg(DAEMON_FLAG)
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--pipe-name")
            .arg(pipe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn clipcheck-ui --daemon");
        Daemon {
            child,
            pipe: pipe.to_owned(),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Kills the process and reaps it. Used to simulate a crash.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // The IPC call runs on its own thread so a hung daemon cannot block the drop.
        let pipe = self.pipe.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            shutdown_quietly(&pipe);
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_secs(3));

        if wait_until(Duration::from_secs(3), || {
            matches!(self.child.try_wait(), Ok(Some(_)))
        }) {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts a `DaemonClient` on `pipe` that records events and connection changes.
pub fn start_client(pipe: &str) -> (DaemonClient, Events, Conns) {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let conns: Conns = Arc::new(Mutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let cn = Arc::clone(&conns);
    let client = DaemonClient::start(
        Some(pipe.to_owned()),
        Box::new(move |v| ev.lock().unwrap_or_else(|e| e.into_inner()).push(v)),
        Box::new(move |c| cn.lock().unwrap_or_else(|e| e.into_inner()).push(c)),
    );
    (client, events, conns)
}

/// True if the client has reported `value` as a connection change.
pub fn conn_log_contains(conns: &Conns, value: bool) -> bool {
    conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&value)
}

/// Number of `on_connection(true)` calls so far.
pub fn connected_count(conns: &Conns) -> usize {
    conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|c| **c)
        .count()
}

/// Waits until `on_connection(true)` has fired more than `before` times. Panics if the
/// daemon exits first or on timeout.
pub fn wait_connected_after(d: &mut Daemon, conns: &Conns, before: usize) {
    wait_until(CONNECT_WAIT, || {
        connected_count(conns) > before || matches!(d.child.try_wait(), Ok(Some(_)))
    });
    if connected_count(conns) > before {
        return;
    }
    if let Some(status) = d.child.try_wait().expect("try_wait failed") {
        panic!("daemon exited with {status:?} before the client connected");
    }
    panic!("client did not report connection within {CONNECT_WAIT:?}");
}

/// Waits for the first `on_connection(true)`.
pub fn wait_connected(d: &mut Daemon, conns: &Conns) {
    wait_connected_after(d, conns, 0);
}

/// The `type` field of a reply body, or `<none>`.
pub fn body_type(body: &Value) -> &str {
    body.get("type").and_then(Value::as_str).unwrap_or("<none>")
}
