//! `ClipCheck.exe --shutdown-daemon`: graceful daemon stop for the installer and the updater.
//!
//! The NSIS installer runs this before it replaces the files (see `installer/README.md`).
//! It only ever stops the daemon that runs from this same executable path. Anything else
//! is left alone, and the installer's path-based force stop handles old builds.
//!
//! Exit codes (contract with `installer/hooks.nsh`):
//! - `0`: the daemon shut down, was not running, or is not ours (nothing was stopped).
//! - `3`: refused, because a recording is in progress.
//! - `4`: any other failure. The installer then waits and force-stops only its own copy.
//!
//! The CLI runs before any Tauri or GUI start, so no window is created.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::daemon::resolve_pipe_name;
use crate::ipc::{ClientError, DaemonClient};

/// Argument that selects this mode.
pub const SHUTDOWN_DAEMON_FLAG: &str = "--shutdown-daemon";

/// Exit code: the daemon is stopped, not running, or not ours.
pub const EXIT_OK: i32 = 0;
/// Exit code: refused because a recording is in progress.
pub const EXIT_RECORDING: i32 = 3;
/// Exit code: any other failure, including the overall timeout.
pub const EXIT_ERROR: i32 = 4;

/// How long to wait for a daemon to answer the pipe. No answer means no daemon.
const CONNECT_WAIT: Duration = Duration::from_secs(2);
/// Timeout for each request (`get_status`, `shutdown`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the pipe to close after the shutdown ack.
const CLOSE_WAIT: Duration = Duration::from_secs(6);
/// Hard limit for the whole command. Exceeding it exits with [`EXIT_ERROR`].
const OVERALL_TIMEOUT: Duration = Duration::from_secs(8);
/// Poll interval while waiting for the connection or the pipe to close.
const POLL: Duration = Duration::from_millis(25);

/// What to do with a daemon found on the pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The daemon runs from another executable (or the status has no path). Do not touch it.
    NotOurs,
    /// The daemon runs from this executable. Send [`update_shutdown_command`].
    Shutdown,
}

/// How a `shutdown` reply is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReply {
    /// `shutdown_ack`: the daemon is exiting.
    Ack,
    /// An error reply with code `busy`: a recording is in progress.
    Busy,
    /// Anything else, including other error codes.
    Other,
}

/// Returns the daemon executable path from a `get_status` reply body, if it has one.
///
/// The field is read from `data.exe_path` (the `StatusDto` layout), with a top-level
/// `exe_path` as fallback. Empty values count as missing.
pub fn status_exe_path(status: &Value) -> Option<&str> {
    status
        .get("data")
        .and_then(|d| d.get("exe_path"))
        .or_else(|| status.get("exe_path"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// Pure decision: stop the daemon only when its reported executable is `own_exe`.
pub fn decide(status: &Value, own_exe: &Path) -> Decision {
    match status_exe_path(status) {
        Some(daemon_exe) if same_exe(daemon_exe, own_exe) => Decision::Shutdown,
        _ => Decision::NotOurs,
    }
}

/// True when two executable paths name the same file: canonicalized where possible,
/// then compared case-insensitively with `\` separators.
pub fn same_exe(a: &str, b: &Path) -> bool {
    normalize_path(&canonical_string(Path::new(a))) == normalize_path(&canonical_string(b))
}

/// Classifies a `shutdown` reply body.
pub fn classify_shutdown_reply(body: &Value) -> ShutdownReply {
    match body.get("type").and_then(Value::as_str) {
        Some("shutdown_ack") => ShutdownReply::Ack,
        Some("error") if error_code(body) == Some("busy") => ShutdownReply::Busy,
        _ => ShutdownReply::Other,
    }
}

/// The `shutdown` command sent for an update: `reason` and `mode` both live in `data`
/// (the daemon reads them there). Used by this CLI and by the updater.
pub fn update_shutdown_command() -> Value {
    json!({ "type": "shutdown", "data": { "reason": "update", "mode": "update" } })
}

/// Error code of an error reply: `data.code`, with a top-level `code` as fallback.
fn error_code(body: &Value) -> Option<&str> {
    body.get("data")
        .and_then(|d| d.get("code"))
        .or_else(|| body.get("code"))
        .and_then(Value::as_str)
}

/// Canonical path as text, or the input unchanged when it cannot be canonicalized.
fn canonical_string(path: &Path) -> String {
    fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

/// Removes the `\\?\` verbatim prefix, uses `\`, and lower-cases (Windows paths).
fn normalize_path(path: &str) -> String {
    path.trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}

/// Runs `--shutdown-daemon` when `args` (argv, program name first) asks for it.
///
/// Returns `None` for a normal launch. Otherwise returns the exit code, and the caller
/// must exit with it before any GUI starts.
pub fn maybe_run_cli(args: &[String]) -> Option<i32> {
    if args.iter().skip(1).any(|a| a == SHUTDOWN_DAEMON_FLAG) {
        Some(run_shutdown_daemon())
    } else {
        None
    }
}

/// Performs the shutdown and returns the exit code. Enforces the overall timeout.
pub fn run_shutdown_daemon() -> i32 {
    let pipe = match resolve_pipe_name(None) {
        Ok(name) => name,
        Err(e) => {
            eprintln!("ClipCheck: cannot determine the daemon pipe: {e}");
            return EXIT_ERROR;
        }
    };
    let own_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("ClipCheck: cannot determine the executable path: {e}");
            return EXIT_ERROR;
        }
    };

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(run_worker(&pipe, &own_exe));
    });
    match rx.recv_timeout(OVERALL_TIMEOUT) {
        Ok(code) => code,
        Err(_) => {
            eprintln!("ClipCheck: timed out while stopping the recorder daemon");
            EXIT_ERROR
        }
    }
}

/// The IO part: connect, check ownership, request shutdown, wait for the pipe to close.
fn run_worker(pipe: &str, own_exe: &Path) -> i32 {
    let client = DaemonClient::start(Some(pipe.to_owned()), Box::new(|_| {}), Box::new(|_| {}));
    if !wait_connected(&client, CONNECT_WAIT) {
        // No daemon answers within the wait: nothing to stop.
        return EXIT_OK;
    }

    let status = match client.request(json!({ "type": "get_status" }), REQUEST_TIMEOUT) {
        Ok(body) => body,
        Err(e) => return report_error("get_status", &e),
    };
    if decide(&status, own_exe) == Decision::NotOurs {
        // Not our daemon (another install, a portable copy, or no path reported).
        return EXIT_OK;
    }

    match client.request(update_shutdown_command(), REQUEST_TIMEOUT) {
        Ok(body) => match classify_shutdown_reply(&body) {
            ShutdownReply::Ack => {}
            ShutdownReply::Busy => {
                eprintln!("ClipCheck: a recording is in progress; the recorder was not stopped");
                return EXIT_RECORDING;
            }
            ShutdownReply::Other => {
                eprintln!("ClipCheck: the recorder refused to shut down");
                return EXIT_ERROR;
            }
        },
        // The daemon closed the pipe before the reply arrived. It is exiting, so go on
        // and wait for the pipe to close.
        Err(ClientError::Disconnected) => {}
        Err(e) => return report_error("shutdown", &e),
    }

    // Waiting is best effort. The installer force-stops its own copy after 10 s.
    wait_closed(&client, CLOSE_WAIT);
    EXIT_OK
}

/// Prints one line for a failed request and returns [`EXIT_ERROR`].
fn report_error(step: &str, error: &ClientError) -> i32 {
    eprintln!("ClipCheck: {step} failed: {error}");
    EXIT_ERROR
}

/// Waits until the client is connected (handshake and subscribe done), up to `timeout`.
fn wait_connected(client: &DaemonClient, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if client.is_connected() {
            return true;
        }
        thread::sleep(POLL);
    }
    client.is_connected()
}

/// Waits until the connection is lost, up to `timeout`.
fn wait_closed(client: &DaemonClient, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while client.is_connected() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Creates a small file to stand in for an executable.
    fn temp_exe(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"x").unwrap();
        path
    }

    #[test]
    fn no_exe_path_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let own = temp_exe(dir.path(), "ClipCheck.exe");
        assert_eq!(
            decide(&json!({ "type": "status", "data": {} }), &own),
            Decision::NotOurs
        );
        assert_eq!(
            decide(
                &json!({ "type": "status", "data": { "exe_path": "  " } }),
                &own
            ),
            Decision::NotOurs
        );
        assert_eq!(decide(&json!({ "type": "error" }), &own), Decision::NotOurs);
    }

    #[test]
    fn other_exe_is_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let own = temp_exe(dir.path(), "ClipCheck.exe");
        let other = temp_exe(dir.path(), "Other.exe");
        let status = json!({ "type": "status", "data": { "exe_path": other.to_string_lossy() } });
        assert_eq!(decide(&status, &own), Decision::NotOurs);
    }

    #[test]
    fn same_exe_is_ours_despite_case_and_separators() {
        let dir = tempfile::tempdir().unwrap();
        let own = temp_exe(dir.path(), "ClipCheck.exe");
        let shouty = own.to_string_lossy().to_uppercase();
        let status = json!({ "type": "status", "data": { "exe_path": shouty } });
        assert_eq!(decide(&status, &own), Decision::Shutdown);

        let forward = own.to_string_lossy().replace('\\', "/");
        let status = json!({ "exe_path": forward });
        assert_eq!(decide(&status, &own), Decision::Shutdown);
    }

    #[test]
    fn verbatim_prefix_is_ignored() {
        assert_eq!(
            normalize_path(r"\\?\C:\Apps\ClipCheck.exe"),
            normalize_path(r"c:\apps\clipcheck.exe")
        );
    }

    #[test]
    fn shutdown_reply_classification() {
        assert_eq!(
            classify_shutdown_reply(&json!({ "type": "shutdown_ack" })),
            ShutdownReply::Ack
        );
        assert_eq!(
            classify_shutdown_reply(&json!({ "type": "error", "data": { "code": "busy" } })),
            ShutdownReply::Busy
        );
        assert_eq!(
            classify_shutdown_reply(&json!({ "type": "error", "data": { "code": "internal" } })),
            ShutdownReply::Other
        );
        assert_eq!(classify_shutdown_reply(&json!({})), ShutdownReply::Other);
    }

    #[test]
    fn update_shutdown_command_has_reason_in_data() {
        let cmd = update_shutdown_command();
        assert_eq!(
            cmd,
            json!({ "type": "shutdown", "data": { "reason": "update", "mode": "update" } })
        );
        assert_eq!(cmd["data"]["reason"], "update");
        assert_eq!(cmd["data"]["mode"], "update");
        assert!(cmd.get("reason").is_none());
    }

    #[test]
    fn flag_detection_skips_program_name() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert!(maybe_run_cli(&args(&["--shutdown-daemon"])).is_none());
        assert!(maybe_run_cli(&args(&["ClipCheck.exe"])).is_none());
    }
}
