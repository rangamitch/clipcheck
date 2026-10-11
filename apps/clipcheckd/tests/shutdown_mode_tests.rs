//! Shutdown mode tests (D47): spawn the real `clipcheckd` and send `Shutdown` in each mode.
//!
//! Windows only. The no-session tests need neither test media nor a desktop. The session tests need
//! native test media (`clipcheck_testkit::available()`) and an interactive desktop for their test window; missing test media
//! is a `SKIPPED:` report (`common::skip_no_encoders`).

#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_ipc::proto::{
    Command as Cmd, ErrorCode, RecState, Reply, ShutdownMode, StatusDto, Topic,
};

use common::window::TestWindow;
use common::*;

/// Writes `<data_dir>/config.toml`: the test exe is the only allowlisted game and recording
/// output stays under `data_dir`.
fn write_config(data_dir: &Path, exe: &str, chord: &str) {
    let recordings = data_dir.join("recordings");
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

[[hotkeys.clips]]
name = "test"
keys = "{chord}"
seconds = 5

[advanced]
detect_poll_ms = 250
log_dir = '{logs}'
"#,
        root = recordings.display(),
        logs = logs.display(),
        exe = exe,
        chord = chord,
    );
    fs::write(data_dir.join("config.toml"), text).expect("failed to write config.toml");
}

/// Polls status until `pred` holds or `timeout` expires.
fn wait_status(
    c: &mut Client,
    timeout: Duration,
    what: &str,
    pred: impl Fn(&StatusDto) -> bool,
) -> Box<StatusDto> {
    let deadline = Instant::now() + timeout;
    loop {
        let s = c.status();
        if pred(&s) {
            return s;
        }
        if Instant::now() >= deadline {
            panic!("timed out after {timeout:?} waiting for {what}; last status: {s:?}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Waits until the encoder probe has finished and the recorder is up.
fn wait_ready(c: &mut Client, data_dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = c.status();
        let reason = s.degraded_reason.clone();
        match reason.as_deref() {
            Some(r) if !r.contains("in progress") => panic!(
                "recorder did not start: degraded_reason = {r:?}\nlog:\n{}",
                log_text(data_dir)
            ),
            None if s.state != RecState::Degraded => return,
            _ => {}
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

/// Polls status until a game is attached. Returns the game name.
fn wait_for_game(c: &mut Client, data: &Path) -> String {
    let s = wait_status(c, Duration::from_secs(30), "game_info", |s| {
        s.game_info.is_some()
    });
    let name = s
        .game_info
        .as_ref()
        .map(|g| g.name.clone())
        .unwrap_or_default();
    assert!(
        !name.is_empty(),
        "game_info.name is empty: {s:?}\nlog:\n{}",
        log_text(data)
    );
    name
}

/// Recursively finds files named `file_name` under `root`.
fn find_files(root: &Path, file_name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_files(&path, file_name, out);
        } else if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(file_name))
        {
            out.push(path);
        }
    }
}

/// Asserts that `reply` is a retryable `busy` refusal.
fn assert_busy(reply: &Reply, what: &str, data: &Path) {
    match reply {
        Reply::Error {
            code: ErrorCode::Busy,
            retryable: true,
            ..
        } => {}
        other => panic!(
            "{what}: expected Error {{ code: Busy, retryable: true }}, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    }
}

// ---------------------------------------------------------------------------
// No recording session: every mode is acknowledged and the daemon exits.
// ---------------------------------------------------------------------------

/// With no session, a `graceful` shutdown is acked and the daemon process exits.
#[test]
fn no_session_graceful_is_acked_and_exits() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
    let mut c = Client::handshake(&mut d, &pipe);
    match c.call(Cmd::Shutdown {
        reason: None,
        mode: Some(ShutdownMode::Graceful),
    }) {
        Reply::ShutdownAck => {}
        other => panic!(
            "graceful with no session: expected ShutdownAck, got {other:?}\nlog:\n{}",
            log_text(dir.path())
        ),
    }
    wait_exit(&mut d, Duration::from_secs(30));
}

/// With no session, each mode (none, graceful, update, force, reason "update" alone) is acked and
/// the daemon exits. One daemon per mode.
#[test]
fn no_session_each_mode_acked() {
    let _g = lock();
    let cases: Vec<(&str, Option<&str>, Option<ShutdownMode>)> = vec![
        ("none", None, None),
        ("graceful", None, Some(ShutdownMode::Graceful)),
        ("update", None, Some(ShutdownMode::Update)),
        ("force", None, Some(ShutdownMode::Force)),
        ("reason_update", Some("update"), None),
    ];
    for (name, reason, mode) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
        let mut c = Client::handshake(&mut d, &pipe);
        let reply = c.call(Cmd::Shutdown {
            reason: reason.map(str::to_owned),
            mode,
        });
        assert!(
            matches!(reply, Reply::ShutdownAck),
            "{name} with no session: expected ShutdownAck, got {reply:?}\nlog:\n{}",
            log_text(dir.path())
        );
        wait_exit(&mut d, Duration::from_secs(30));
    }
}

// ---------------------------------------------------------------------------
// Needs native test media and a desktop.
// ---------------------------------------------------------------------------

/// Needs native test media and a desktop. With an active session: no mode, `graceful`, `update` and reason
/// `update` (no mode) are refused with retryable `busy` and the daemon keeps running with the
/// session still active. Then `force` acks, the daemon exits, and `session-part1.mp4` is saved
/// under the storage root with size > 0.
#[test]
fn session_active_busy_modes_then_force_saves_part() {
    let _g = lock();
    let name = "session_active_busy_modes_then_force_saves_part";
    if !clipcheck_testkit::available() {
        skip_no_encoders(name);
        return;
    }
    let window = TestWindow::create()
        .unwrap_or_else(|e| panic!("{name}: cannot create the test window: {e}"));
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Sessions, Topic::State]);
    wait_ready(&mut c, data);
    wait_for_game(&mut c, data);

    if c.status().session.is_none() {
        c.expect_ok(Cmd::StartSession, "start_session");
    }
    wait_status(&mut c, Duration::from_secs(15), "session active", |s| {
        s.session.is_some()
    });
    // Let the part record some frames before the shutdown attempts.
    c.pump_for(Duration::from_secs(4));

    // Refused modes. After each refusal the daemon must still answer and the session must live.
    let refused: Vec<(&str, Option<&str>, Option<ShutdownMode>)> = vec![
        ("no mode", None, None),
        ("graceful", None, Some(ShutdownMode::Graceful)),
        ("update", None, Some(ShutdownMode::Update)),
        ("reason update without mode", Some("update"), None),
    ];
    for (what, reason, mode) in refused {
        let reply = c.call(Cmd::Shutdown {
            reason: reason.map(str::to_owned),
            mode,
        });
        assert_busy(&reply, what, data);
        c.ping();
        let s = c.status();
        assert!(
            s.session.is_some(),
            "{what}: session ended after a busy refusal: {s:?}\nlog:\n{}",
            log_text(data)
        );
    }

    // Force: acked, then the process exits, and the part is saved.
    let reply = c.call(Cmd::Shutdown {
        reason: None,
        mode: Some(ShutdownMode::Force),
    });
    assert!(
        matches!(reply, Reply::ShutdownAck),
        "force with active session: expected ShutdownAck, got {reply:?}\nlog:\n{}",
        log_text(data)
    );
    wait_exit(&mut d, Duration::from_secs(30));

    let mut parts = Vec::new();
    find_files(&data.join("recordings"), "session-part1.mp4", &mut parts);
    assert!(
        !parts.is_empty(),
        "no session-part1.mp4 under {}\nlog:\n{}",
        data.join("recordings").display(),
        log_text(data)
    );
    let sizes: Vec<u64> = parts
        .iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .collect();
    assert!(
        sizes.iter().any(|&s| s > 0),
        "session-part1.mp4 files are empty: {parts:?} sizes {sizes:?}\nlog:\n{}",
        log_text(data)
    );
    drop(window);
}

/// Needs native test media and a desktop. The replay buffer alone (attached game, no session) does not
/// count as recording: after StopSession, `graceful` is acked and the daemon exits.
#[test]
fn buffer_only_graceful_is_acked() {
    let _g = lock();
    let name = "buffer_only_graceful_is_acked";
    if !clipcheck_testkit::available() {
        skip_no_encoders(name);
        return;
    }
    let window = TestWindow::create()
        .unwrap_or_else(|e| panic!("{name}: cannot create the test window: {e}"));
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Sessions, Topic::State]);
    wait_ready(&mut c, data);
    wait_for_game(&mut c, data);

    if c.status().session.is_none() {
        c.expect_ok(Cmd::StartSession, "start_session");
    }
    wait_status(&mut c, Duration::from_secs(15), "session active", |s| {
        s.session.is_some()
    });
    c.expect_ok(Cmd::StopSession, "stop_session");
    wait_status(&mut c, Duration::from_secs(30), "session ended", |s| {
        s.session.is_none()
    });
    let s = c.status();
    assert!(
        s.game_info.is_some(),
        "game detached after stop; the buffer-only case needs an attached game: {s:?}"
    );

    match c.call(Cmd::Shutdown {
        reason: None,
        mode: Some(ShutdownMode::Graceful),
    }) {
        Reply::ShutdownAck => {}
        other => panic!(
            "graceful with buffer only: expected ShutdownAck, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    }
    wait_exit(&mut d, Duration::from_secs(30));
    drop(window);
}
