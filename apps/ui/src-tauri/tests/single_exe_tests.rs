//! Tests for the single-exe daemon glue in `clipcheck_ui_lib::daemon`.
//!
//! The argument helpers are checked directly. The integration tests spawn the built
//! `clipcheck-ui` binary with `--daemon` and talk to it over the named pipe, the same way
//! `apps/clipcheckd/tests/daemon.rs` does. `maybe_run_daemon` and `ensure_daemon_running`
//! are never called here: the first would run the recorder in the test process, and the
//! second uses `current_exe()`, which is the test binary under `cargo test`.
//!
//! The daemon's single-instance mutex is session-global, so every integration test takes
//! `TEST_LOCK`. Each daemon is wrapped in a guard that shuts it down and reaps it on drop.

mod common;

use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use clipcheck_ipc::frame::{read_frame, write_frame};
use clipcheck_ipc::proto::{Command as Cmd, Envelope, Reply, Response};
use clipcheck_ipc::{PipeClient, PipeConn};
use clipcheck_ui_lib::daemon::{
    daemon_args, ensure_daemon_running_from, is_daemon_mode, strip_daemon_flag, DaemonError,
    DaemonStart, DAEMON_FLAG, PIPE_NAME_ENV,
};
use common::{hello_cmd, shutdown_quietly, unique_pipe, wait_until, Daemon, TEST_LOCK};

/// Exit code `clipcheckd` uses when another daemon already holds the instance mutex.
const EXIT_ALREADY_RUNNING: i32 = 3;
/// How long a freshly spawned daemon may take to start answering on its pipe.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a daemon may take to exit after Shutdown.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Sends a request with the given id and returns the reply, checking the id is echoed.
fn call(conn: &mut PipeConn, id: u64, cmd: Cmd) -> Response {
    write_frame(conn, &Envelope::request(id, cmd)).expect("write request frame");
    let resp: Response = read_frame(conn).expect("read reply frame");
    assert_eq!(
        resp.id,
        Some(id),
        "reply id does not echo request id: {resp:?}"
    );
    resp
}

/// Completes the handshake (Hello, id 1) and returns the daemon pid it reports.
fn hello(conn: &mut PipeConn) -> u32 {
    match call(conn, 1, hello_cmd()).body {
        Reply::HelloAck {
            proto, daemon_pid, ..
        } => {
            assert_eq!(proto, 1, "negotiated protocol version");
            daemon_pid
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }
}

fn assert_pong(conn: &mut PipeConn, id: u64) {
    match call(conn, id, Cmd::Ping).body {
        Reply::Pong { .. } => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}

/// Sends Shutdown over IPC (after Hello) and checks for ShutdownAck.
fn ipc_shutdown(conn: &mut PipeConn, id: u64) {
    match call(
        conn,
        id,
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
}

/// Waits for the child to exit. Panics on timeout.
fn wait_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let mut status = None;
    let exited = wait_until(timeout, || {
        status = child.try_wait().expect("try_wait failed");
        status.is_some()
    });
    if !exited {
        panic!("daemon did not exit within {timeout:?}");
    }
    status.expect("status is set once wait_until succeeds")
}

/// Polls the pipe until a client connects. Panics if the daemon exits first or on timeout.
fn wait_for_pipe(d: &mut Daemon) -> PipeConn {
    let pipe = d.pipe.clone();
    let mut conn = None;
    let mut last_err = String::new();
    let answered = wait_until(READY_TIMEOUT, || {
        if let Some(status) = d.child.try_wait().expect("try_wait failed") {
            panic!("daemon exited with {status:?} before pipe {pipe} answered");
        }
        match PipeClient::connect(&pipe, Duration::from_millis(500)) {
            Ok(c) => {
                conn = Some(c);
                true
            }
            Err(e) => {
                last_err = e.to_string();
                false
            }
        }
    });
    if !answered {
        panic!("pipe {pipe} did not answer within {READY_TIMEOUT:?}: {last_err}");
    }
    conn.expect("connection is set once wait_until succeeds")
}

/// Guard for a daemon started by `ensure_daemon_running_from`. The test holds no `Child`
/// for it, so the guard shuts it down by pipe, then uses `taskkill` as a fallback.
struct DetachedDaemon {
    pid: u32,
    pipe: String,
}

impl Drop for DetachedDaemon {
    fn drop(&mut self) {
        shutdown_quietly(&self.pipe);
        let _ = Command::new("taskkill")
            .args(["/PID", &self.pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// True when `pid` appears as a token in `tasklist` output for that pid.
fn process_running(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("failed to run tasklist");
    let text = String::from_utf8_lossy(&out.stdout);
    let wanted = pid.to_string();
    text.split_whitespace().any(|token| token == wanted)
}

/// Win32 window enumeration through raw FFI, so no `windows` crate is needed.
#[allow(non_snake_case)]
mod win32 {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;
    pub type EnumProc = unsafe extern "system" fn(Hwnd, isize) -> i32;

    #[link(name = "user32")]
    extern "system" {
        pub fn EnumWindows(callback: EnumProc, lparam: isize) -> i32;
        pub fn GetWindowThreadProcessId(hwnd: Hwnd, process_id: *mut u32) -> u32;
        pub fn IsWindowVisible(hwnd: Hwnd) -> i32;
    }
}

/// Every top-level window as (owning pid, is visible).
fn top_level_windows() -> Vec<(u32, bool)> {
    unsafe extern "system" fn collect(hwnd: win32::Hwnd, lparam: isize) -> i32 {
        let out = &mut *(lparam as *mut Vec<(u32, bool)>);
        let mut pid: u32 = 0;
        win32::GetWindowThreadProcessId(hwnd, &mut pid);
        out.push((pid, win32::IsWindowVisible(hwnd) != 0));
        1
    }
    let mut out: Vec<(u32, bool)> = Vec::new();
    // SAFETY: `collect` only reads and writes through the `lparam` pointer, which points at
    // `out`, and `out` outlives the synchronous EnumWindows call.
    unsafe {
        win32::EnumWindows(collect, &mut out as *mut Vec<(u32, bool)> as isize);
    }
    out
}

// ---------------------------------------------------------------------------
// Unit tests (no processes)
// ---------------------------------------------------------------------------

#[test]
fn is_daemon_mode_detects_flag_after_program_name() {
    assert!(is_daemon_mode(&argv(&["a.exe", "--daemon"])));
    assert!(is_daemon_mode(&argv(&[
        "a.exe",
        "--data-dir",
        "x",
        "--daemon"
    ])));
    assert!(is_daemon_mode(&argv(&[
        "a.exe",
        "--daemon",
        "--pipe-name",
        "p"
    ])));
    assert!(!is_daemon_mode(&argv(&["a.exe"])));
    assert!(
        !is_daemon_mode(&argv(&["--daemon"])),
        "argv[0] must be ignored"
    );
    assert!(!is_daemon_mode(&argv(&["a.exe", "--daemonx"])));
    assert!(!is_daemon_mode(&[]));
}

#[test]
fn strip_daemon_flag_removes_all_flags_and_keeps_argv0() {
    assert_eq!(
        strip_daemon_flag(&argv(&["a.exe", "--daemon", "--pipe-name", "p"])),
        argv(&["a.exe", "--pipe-name", "p"])
    );
    assert_eq!(
        strip_daemon_flag(&argv(&["a.exe", "--daemon", "x", "--daemon"])),
        argv(&["a.exe", "x"])
    );
    assert_eq!(strip_daemon_flag(&argv(&["--daemon"])), argv(&["--daemon"]));
    assert_eq!(
        strip_daemon_flag(&argv(&["--daemon", "--daemon"])),
        argv(&["--daemon"])
    );
}

#[test]
fn daemon_args_forward_only_set_values() {
    assert_eq!(daemon_args(None, None), argv(&["--daemon"]));
    assert_eq!(
        daemon_args(Some("d"), Some("p")),
        argv(&["--daemon", "--data-dir", "d", "--pipe-name", "p"])
    );
    assert_eq!(
        daemon_args(Some("d"), None),
        argv(&["--daemon", "--data-dir", "d"])
    );
    assert_eq!(
        daemon_args(None, Some("p")),
        argv(&["--daemon", "--pipe-name", "p"])
    );
}

#[test]
fn constants_have_documented_values() {
    assert_eq!(DAEMON_FLAG, "--daemon");
    assert_eq!(PIPE_NAME_ENV, "CLIPCHECK_PIPE_NAME");
}

/// `maybe_run_daemon` is not called here. This checks the property it relies on: the
/// gate (`is_daemon_mode`) is false for the args it passes on after stripping the flag,
/// so the daemon never re-enters daemon mode.
#[test]
fn daemon_gate_agrees_with_strip() {
    let args = argv(&["a.exe", "--daemon", "--pipe-name", "p"]);
    assert!(is_daemon_mode(&args), "gate must be open for daemon args");
    let stripped = strip_daemon_flag(&args);
    assert!(
        !is_daemon_mode(&stripped),
        "stripped args still open the daemon gate: {stripped:?}"
    );
}

// ---------------------------------------------------------------------------
// Integration tests (spawn the built clipcheck-ui binary)
// ---------------------------------------------------------------------------

#[test]
fn daemon_mode_serves_ipc_and_exits_on_shutdown() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Declared before the daemon, so the daemon is shut down before the dir is removed.
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let mut conn = wait_for_pipe(&mut d);

    let pid = hello(&mut conn);
    assert_eq!(pid, d.pid(), "daemon_pid does not match child pid");
    assert!(
        dir.path().join("config.toml").is_file(),
        "config.toml not created"
    );
    assert!(
        dir.path().join("library.db").is_file(),
        "library.db not created"
    );

    assert_pong(&mut conn, 2);
    ipc_shutdown(&mut conn, 3);
    drop(conn);

    let status = wait_exit(&mut d.child, EXIT_TIMEOUT);
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

#[test]
fn daemon_mode_creates_no_visible_window() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let mut conn = wait_for_pipe(&mut d);
    hello(&mut conn);

    let windows = top_level_windows();
    assert!(
        !windows.is_empty(),
        "EnumWindows returned no top-level windows at all; the check would be vacuous"
    );
    let pid = d.pid();
    let visible_owned: Vec<&(u32, bool)> = windows
        .iter()
        .filter(|(owner, visible)| *owner == pid && *visible)
        .collect();
    assert!(
        visible_owned.is_empty(),
        "daemon pid {pid} owns {} visible top-level window(s)",
        visible_owned.len()
    );

    ipc_shutdown(&mut conn, 2);
    drop(conn);
    let status = wait_exit(&mut d.child, EXIT_TIMEOUT);
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

#[test]
fn daemon_already_running_exits_3() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let pipe = unique_pipe("clipcheck-ui-test");

    let mut a = Daemon::spawn(dir_a.path(), &pipe);
    let mut conn_a = wait_for_pipe(&mut a);
    hello(&mut conn_a);

    // Single instance is per data folder, so the second daemon must share the first one's folder.
    let mut b = Daemon::spawn(dir_a.path(), &pipe);
    let status_b = wait_exit(&mut b.child, EXIT_TIMEOUT);
    assert_eq!(
        status_b.code(),
        Some(EXIT_ALREADY_RUNNING),
        "second daemon exit status was {status_b:?}"
    );

    assert_pong(&mut conn_a, 2);
    ipc_shutdown(&mut conn_a, 3);
    drop(conn_a);
    let status_a = wait_exit(&mut a.child, EXIT_TIMEOUT);
    assert_eq!(
        status_a.code(),
        Some(0),
        "first daemon exit status was {status_a:?}"
    );
}

#[test]
fn daemon_with_other_data_dir_on_same_pipe_exits_2() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let pipe = unique_pipe("clipcheck-ui-test");

    let mut a = Daemon::spawn(dir_a.path(), &pipe);
    let mut conn_a = wait_for_pipe(&mut a);
    hello(&mut conn_a);

    let mut b = Daemon::spawn(dir_b.path(), &pipe);
    let status_b = wait_exit(&mut b.child, EXIT_TIMEOUT);
    assert_eq!(
        status_b.code(),
        Some(2),
        "pipe owned by another daemon: {status_b:?}"
    );

    assert_pong(&mut conn_a, 2);
    ipc_shutdown(&mut conn_a, 3);
    drop(conn_a);
    let status_a = wait_exit(&mut a.child, EXIT_TIMEOUT);
    assert_eq!(
        status_a.code(),
        Some(0),
        "first daemon exit status was {status_a:?}"
    );
}

#[test]
fn ensure_daemon_running_from_spawns_detached_daemon() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().to_str().expect("tempdir path is not UTF-8");
    let pipe = unique_pipe("clipcheck-ui-test");
    let exe = Path::new(env!("CARGO_BIN_EXE_clipcheck-ui"));

    let pid = match ensure_daemon_running_from(exe, Some(data), Some(&pipe)) {
        Ok(DaemonStart::Spawned { pid }) => pid,
        other => panic!("expected Ok(Spawned {{ pid }}), got {other:?}"),
    };
    assert!(pid > 0, "spawned pid is zero");
    let guard = DetachedDaemon {
        pid,
        pipe: pipe.clone(),
    };

    // The function has returned, so the daemon must still be answering.
    {
        let mut conn = PipeClient::connect(&pipe, Duration::from_secs(2))
            .expect("pipe not answering right after ensure_daemon_running_from returned");
        let daemon_pid = hello(&mut conn);
        assert_eq!(daemon_pid, pid, "answering daemon is not the spawned pid");
    }

    // Still answering a second later, so it survived the caller's return. This is a
    // deliberate fixed wait, not a condition poll: the thing under test is that the
    // detached daemon keeps running on its own after the caller returns, and a daemon that
    // died early would only show up as elapsed time. The wait is bounded to 1 s.
    std::thread::sleep(Duration::from_secs(1));
    let mut conn = PipeClient::connect(&pipe, Duration::from_secs(2))
        .expect("pipe stopped answering 1 s after ensure_daemon_running_from returned");
    hello(&mut conn);
    ipc_shutdown(&mut conn, 2);
    drop(conn);

    // The pipe must stop answering and the process must exit.
    if !wait_until(EXIT_TIMEOUT, || {
        PipeClient::connect(&pipe, Duration::from_millis(300)).is_err()
    }) {
        panic!("pipe {pipe} still answering {EXIT_TIMEOUT:?} after Shutdown");
    }
    if !wait_until(EXIT_TIMEOUT, || !process_running(pid)) {
        panic!("daemon pid {pid} still running {EXIT_TIMEOUT:?} after Shutdown");
    }
    drop(guard);
}

#[test]
fn ensure_daemon_running_from_returns_already_when_running() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-test");
    let exe = Path::new(env!("CARGO_BIN_EXE_clipcheck-ui"));

    let mut d = Daemon::spawn(dir.path(), &pipe);
    {
        // Drop the probe connection before calling ensure, so the probe is not blocked.
        let mut conn = wait_for_pipe(&mut d);
        hello(&mut conn);
    }

    let data = dir.path().to_str().expect("tempdir path is not UTF-8");
    match ensure_daemon_running_from(exe, Some(data), Some(&pipe)) {
        Ok(DaemonStart::Already) => {}
        other => panic!("expected Ok(Already) with a daemon running, got {other:?}"),
    }

    let mut conn = PipeClient::connect(&pipe, Duration::from_secs(2))
        .expect("pipe not answering after ensure returned Already");
    hello(&mut conn);
    ipc_shutdown(&mut conn, 2);
    drop(conn);
    let status = wait_exit(&mut d.child, EXIT_TIMEOUT);
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

#[test]
fn ensure_daemon_running_from_returns_error_when_exe_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().to_str().expect("tempdir path is not UTF-8");
    let missing = dir.path().join("nope.exe");
    let pipe = unique_pipe("clipcheck-ui-test");

    match ensure_daemon_running_from(&missing, Some(data), Some(&pipe)) {
        Err(DaemonError::Io(_)) => {}
        other => panic!("expected Err(DaemonError::Io(_)) for a missing exe, got {other:?}"),
    }
}
