//! Issue #196: recorder errors raised during daemon shutdown must reach the daemon log.
//!
//! When the daemon shuts down while a session is recording, the final session part is saved
//! during shutdown. If that save fails, the recorder emits `session not saved: ...`. That event
//! must be drained and written to `<data-dir>/logs/clipcheckd.log` at WARN or ERROR level before
//! the process exits.
//!
//! Windows only. Each test needs native test media (`clipcheck_testkit::available()`) and an interactive desktop for a
//! synthetic game window. If either is missing the test prints `SKIP:` and returns without
//! passing. Every daemon uses a temp data dir and a unique pipe name, and is shut down over IPC.

#![cfg(windows)]
mod common;

use std::fs;
use std::path::Path;
use std::process::ExitStatus;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_ipc::proto::{Command as Cmd, RecState, Reply, ShutdownMode, StatusDto};
use common::window::TestWindow;
use common::*;

/// The text the recorder uses for a failed final save (`recorder.rs`, `detach`).
const NOT_SAVED: &str = "session not saved";

/// Writes `<data_dir>/config.toml`. The only allowlisted game is the test exe. The session
/// storage root is `root`. Logs go to `<data_dir>/logs`, and the replay location is empty, so
/// nothing is written outside `data_dir` except under `root`.
fn write_config(data_dir: &Path, exe: &str, root: &Path) {
    let logs = data_dir.join("logs");
    let text = format!(
        r#"[storage]
root = '{root}'

[detection]
mode = "allowlist"
allow = ["{exe}"]
auto_record = true
stop_grace_s = 3

[replay]
enabled = true
max_seconds = 60
location = ""

[video]
encoder = "software"
codec = "h264"
preset = "low"

[advanced]
detect_poll_ms = 250
log_dir = '{logs}'
"#,
        root = root.display(),
        logs = logs.display(),
        exe = exe,
    );
    fs::write(data_dir.join("config.toml"), text).expect("failed to write config.toml");
}

/// Polls status until the daemon reports an active session, or fails after `timeout`.
fn wait_session_active(c: &mut Client, timeout: Duration, data_dir: &Path) -> Box<StatusDto> {
    let deadline = Instant::now() + timeout;
    loop {
        let s = c.status();
        if s.session.is_some() {
            return s;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out after {timeout:?} waiting for an active session; last status: {s:?}\nlog:\n{}",
                log_text(data_dir)
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Waits until the encoder probe has finished and the recorder is up. Any degraded reason other
/// than "encoder probe in progress" fails the test.
fn wait_ready(c: &mut Client, data_dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = c.status();
        match (s.state, s.degraded_reason.as_deref()) {
            (st, None) if st != RecState::Degraded => return,
            (_, Some(r)) if r.contains("in progress") => {}
            (_, Some(r)) => panic!(
                "recorder did not start: degraded_reason = {r:?}\nlog:\n{}",
                log_text(data_dir)
            ),
            (_, None) => {}
        }
        if Instant::now() >= deadline {
            panic!(
                "encoder probe did not finish within 30 s; last status: {s:?}\nlog:\n{}",
                log_text(data_dir)
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Sends Shutdown, expects `ShutdownAck` (mode `force`: a session may be recording, D47), and waits for the process to exit within 20 s.
fn shutdown_and_wait(c: &mut Client, d: &mut Daemon) -> ExitStatus {
    match c.call(Cmd::Shutdown {
        reason: None,
        mode: Some(ShutdownMode::Force),
    }) {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    wait_exit(d, Duration::from_secs(20))
}

/// Finds the lines of `log` that contain `needle`.
fn lines_with<'a>(log: &'a str, needle: &str) -> Vec<&'a str> {
    log.lines().filter(|l| l.contains(needle)).collect()
}

/// Counts non-empty `.mp4` files under `dir`, recursively. A missing dir counts as zero.
fn count_mp4(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                count_mp4(&p)
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"))
                && fs::metadata(&p).is_ok_and(|m| m.len() > 0)
            {
                1
            } else {
                0
            }
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Scenario setup
// ---------------------------------------------------------------------------

/// Returns `Some(window)` when native test media and a desktop are available. Otherwise prints a loud
/// `SKIP:` line naming the test and returns `None`.
fn prerequisites(test: &str) -> Option<TestWindow> {
    if !clipcheck_testkit::available() {
        eprintln!("SKIP: {test}: native test media unavailable");
        return None;
    }
    match TestWindow::create() {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("SKIP: {test}: {e}");
            None
        }
    }
}

/// Starts the daemon, waits for the recorder and the test game, and waits for the session that
/// the recorder starts on attach. Then lets the recorder run for 10 s so segments are cut and
/// staged. Returns the daemon and the client.
fn record_a_session(data: &Path, exe: &str, pipe: &str, root: &Path) -> (Daemon, Client) {
    write_config(data, exe, root);
    let mut d = spawn(data, pipe);
    let mut c = Client::handshake(&mut d, pipe);
    c.subscribe_all();
    wait_ready(&mut c, data);
    c.wait_game(exe, Duration::from_secs(15));
    // The recorder starts a session on its own when the game attaches (begin_session).
    wait_session_active(&mut c, Duration::from_secs(20), data);
    // The segment tracker cuts and stages segments on its own. Give it time before shutdown.
    c.pump_for(Duration::from_secs(10));
    (d, c)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The final session part cannot be saved, so the recorder emits `session not saved` during
/// shutdown. The daemon log must contain that message at WARN or ERROR level.
///
/// The sessions directory sits under `<root>/sessions`. `root` is `<data>/blocker/recordings`,
/// where `blocker` is a regular file, so `create_dir_all` fails.
#[test]
fn shutdown_logs_session_not_saved_warning() {
    const NAME: &str = "shutdown_logs_session_not_saved_warning";
    let _g = lock();
    let Some(_window) = prerequisites(NAME) else {
        return;
    };
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    let blocker = data.join("blocker");
    fs::write(&blocker, b"not a directory").expect("write blocker file");
    let root = blocker.join("recordings");
    let pipe = unique_pipe();

    let (mut d, mut c) = record_a_session(data, &exe, &pipe, &root);
    let status = shutdown_and_wait(&mut c, &mut d);
    let log = log_text(data);

    let hits = lines_with(&log, NOT_SAVED);
    assert!(
        !hits.is_empty(),
        "expected a '{NOT_SAVED}' line in the daemon log after shutdown\nexit: {status:?}\nlog:\n{log}"
    );
    assert!(
        hits.iter()
            .any(|l| l.contains(" WARN ") || l.contains(" ERROR ")),
        "'{NOT_SAVED}' was logged, but not at WARN or ERROR level: {hits:?}"
    );
    assert_eq!(
        count_mp4(&root),
        0,
        "no session part should have been written under {}",
        root.display()
    );
}

/// Negative control: a shutdown whose final session part saves successfully logs no
/// `session not saved` line, and the part exists on disk.
#[test]
fn shutdown_with_successful_save_logs_no_warning() {
    const NAME: &str = "shutdown_with_successful_save_logs_no_warning";
    let _g = lock();
    let Some(_window) = prerequisites(NAME) else {
        return;
    };
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    let root = data.join("recordings");
    let pipe = unique_pipe();

    let (mut d, mut c) = record_a_session(data, &exe, &pipe, &root);
    let status = shutdown_and_wait(&mut c, &mut d);
    let log = log_text(data);

    assert_eq!(
        status.code(),
        Some(0),
        "daemon exit status was {status:?}\nlog:\n{log}"
    );
    let hits = lines_with(&log, NOT_SAVED);
    assert!(
        hits.is_empty(),
        "a successful save must not log '{NOT_SAVED}': {hits:?}\nlog:\n{log}"
    );
    assert!(
        count_mp4(&root) >= 1,
        "the final session part was not written under {}\nlog:\n{log}",
        root.display()
    );
}
