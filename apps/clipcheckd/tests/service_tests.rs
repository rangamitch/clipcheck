//! End-to-end recording tests: spawn the real `clipcheckd` and drive it over the named pipe.
//!
//! Windows only.
//! - `degraded_mode_reports_and_stays_up` needs neither test media nor a desktop.
//! - `not_recording_when_no_game` and `full_recording_flow` need native test media (`clipcheck_testkit::available()`).
//!   Otherwise they report `SKIPPED:` through `common::skip_no_encoders` and return. Set
//!   `CLIPCHECK_REQUIRE_CAPTURE` to turn that skip into a failure.
//! - `full_recording_flow` and `idle_subscriber_receives_clip_saved_without_sending` also need an
//!   interactive desktop for their window and hotkey. A window that cannot be created is a
//!   failure (it panics); it is never a skip.
//!
//! The daemon pushes events to a subscribed connection as they happen, even when that connection
//! is idle. `Client` reads frames on a background thread. `Client::wait_event` only waits for
//! queued or incoming event frames with a deadline and sends no keep-alive pings.

#![cfg(windows)]

mod common;

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_engine::catalog::probe_media;
use clipcheck_ipc::proto::{
    Command as Cmd, ErrorCode, Event, RecState, Reply, ShutdownMode, StatusDto, Topic,
};
use clipcheck_library::{Library, ListQuery, RecordingKind};
use common::window::{press_chord, TestWindow, VK_CONTROL, VK_F20, VK_MENU, VK_SHIFT};
use common::*;

/// Writes `<data_dir>/config.toml` for the recording tests.
///
/// The test exe is the only allowlisted game. `storage.root` and `advanced.log_dir` are set
/// explicitly, and `replay.location` is empty, so nothing is written outside `data_dir`.
fn write_config(data_dir: &Path, exe: &str) {
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
keys = "Ctrl+Alt+Shift+F20"
seconds = 5

[advanced]
detect_poll_ms = 250
log_dir = '{logs}'
"#,
        root = recordings.display(),
        logs = logs.display(),
        exe = exe,
    );
    fs::write(data_dir.join("config.toml"), text).expect("failed to write config.toml");
}

/// Turns a reply into `(code, message)`, panicking if it is not an error.
fn expect_error(reply: Reply, what: &str) -> (ErrorCode, String) {
    match reply {
        Reply::Error { code, message, .. } => (code, message),
        other => panic!("{what}: expected Error, got {other:?}"),
    }
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
///
/// While the probe runs, `degraded_reason` is "encoder probe in progress". Any other reason is
/// a real failure.
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

// ---------------------------------------------------------------------------
// Output checks
// ---------------------------------------------------------------------------

/// Checks that `path` is an existing, non-empty file under `<data_dir>/recordings/<subdir>`.
fn check_output_file(data_dir: &Path, subdir: &str, path: &str, bytes: u64, what: &str) {
    let file = Path::new(path);
    assert!(file.is_file(), "{what}: file {path} does not exist");
    let canon_file = fs::canonicalize(file).expect("canonicalize output path");
    let canon_dir = fs::canonicalize(data_dir.join("recordings").join(subdir))
        .unwrap_or_else(|e| panic!("{what}: recordings/{subdir} missing: {e}"));
    assert!(
        canon_file.starts_with(&canon_dir),
        "{what}: {canon_file:?} is not under {canon_dir:?}"
    );
    assert!(bytes > 0, "{what}: event reports bytes = 0");
}

/// Checks a clip file's media probe duration is over 1 s.
fn check_clip_duration(path: &str, duration_ms: u64, what: &str) {
    assert!(
        duration_ms > 1000,
        "{what}: clip_saved duration_ms = {duration_ms}, expected > 1000"
    );
    let info = probe_media(Path::new(path))
        .unwrap_or_else(|e| panic!("{what}: media probe failed on {path}: {e}"));
    assert!(
        info.duration_ms > 1000,
        "{what}: media probe duration = {} ms, expected > 1000",
        info.duration_ms
    );
}

/// Returns a description of the problem when the library does not hold exactly `expected` ready
/// or pending rows of `kind`. The description includes the `library` warnings the daemon sent.
fn library_rows_problem(
    c: &Client,
    data_dir: &Path,
    kind: RecordingKind,
    expected: usize,
    when: &str,
) -> Option<String> {
    // The daemon registers the row just after it publishes the event, so wait (bounded) for it.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let page = loop {
        let lib =
            Library::open_readonly(data_dir.join("library.db")).expect("open library.db read-only");
        let page = lib
            .list(&ListQuery {
                kind: Some(kind),
                limit: 200,
                ..Default::default()
            })
            .expect("list library rows");
        if page.items.len() == expected {
            return None;
        }
        if std::time::Instant::now() >= deadline {
            break page;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let warnings: Vec<String> = c
        .seen
        .iter()
        .filter_map(|e| match e {
            Event::Warning { code, message } => Some(format!("{code}: {message}")),
            _ => None,
        })
        .collect();
    dump_library_state(c, data_dir);
    Some(format!(
        "library {kind:?} rows {when}: expected {expected}, found {}; daemon warnings: {warnings:?}",
        page.items.len()
    ))
}

/// DIAGNOSTIC (temporary): dumps every library row regardless of status, the events the client
/// saw (minus the high-rate ones), the session part files on disk, and the tail of each log file.
fn dump_library_state(c: &Client, data_dir: &Path) {
    eprintln!("=== DIAG library dump begin ({}) ===", data_dir.display());
    match Library::open_readonly(data_dir.join("library.db")) {
        Ok(lib) => match lib.list_roots_with_stats() {
            Ok(roots) => {
                for root in roots {
                    eprintln!(
                        "DIAG root id={} path={} kind={}",
                        root.id, root.path, root.kind
                    );
                    match lib.list_paths_for_root(root.id) {
                        Ok(rows) => {
                            for (id, rel, size, mtime, status) in rows {
                                eprintln!(
                                    "DIAG row id={id} status={status:?} size={size} mtime={mtime} rel={rel}"
                                );
                            }
                        }
                        Err(e) => eprintln!("DIAG list_paths_for_root failed: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("DIAG list_roots_with_stats failed: {e}"),
        },
        Err(e) => eprintln!("DIAG open_readonly failed: {e}"),
    }
    for e in &c.seen {
        let s = format!("{e:?}");
        if s.starts_with("AudioLevels") || s.starts_with("CaptureHealth") {
            continue;
        }
        eprintln!("DIAG seen: {s}");
    }
    let mut files = Vec::new();
    collect_files(&data_dir.join("recordings"), &mut files);
    for f in &files {
        eprintln!("DIAG file on disk: {f}");
    }
    let logs = data_dir.join("logs");
    match fs::read_dir(&logs) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let text =
                    fs::read_to_string(&path).unwrap_or_else(|e| format!("<cannot read: {e}>"));
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(80);
                eprintln!(
                    "DIAG ---- tail of {} ({} lines) ----",
                    path.display(),
                    lines.len()
                );
                for line in &lines[start..] {
                    eprintln!("DIAG | {line}");
                }
            }
        }
        Err(e) => eprintln!("DIAG cannot read logs dir: {e}"),
    }
    eprintln!("=== DIAG library dump end ===");
}

/// DIAGNOSTIC (temporary): lists every file under `dir` recursively.
fn collect_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(format!("{} ({size} bytes)", path.display()));
        }
    }
}

/// Asserts the library holds exactly `expected` rows of `kind` (see `library_rows_problem`).
fn assert_library_rows(
    c: &Client,
    data_dir: &Path,
    kind: RecordingKind,
    expected: usize,
    when: &str,
) {
    if let Some(problem) = library_rows_problem(c, data_dir, kind, expected, when) {
        panic!("{problem}");
    }
}

/// Injects Ctrl+Alt+Shift+F20 with SendInput. RegisterHotKey is global, so focus does not matter.
fn press_clip_hotkey() {
    press_chord(&[VK_CONTROL, VK_MENU, VK_SHIFT, VK_F20]);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Without native test media the daemon starts in degraded mode, answers requests, and shuts down cleanly.
#[test]
fn degraded_mode_reports_and_stays_up() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
    let mut c = Client::handshake(&mut d, &pipe);
    c.ping();

    // The reason is "encoder probe in progress" until the probe fails, then it says recording is off (no encoders).
    let s = wait_status(
        &mut c,
        Duration::from_secs(20),
        "degraded reason saying recording is off",
        |s| {
            s.degraded_reason
                .as_deref()
                .is_some_and(|r| r.to_ascii_lowercase().contains("recording is off"))
        },
    );
    assert_eq!(
        s.state,
        RecState::Degraded,
        "state in --no-encoders mode: {s:?}"
    );

    let (code, msg) = expect_error(c.call(Cmd::ClipNow { seconds: 4 }), "clip_now");
    assert!(
        matches!(code, ErrorCode::WorkerUnavailable | ErrorCode::NotRecording),
        "clip_now in degraded mode: code {code:?}, message {msg:?}"
    );

    let (code, msg) = expect_error(c.call(Cmd::StartSession), "start_session");
    eprintln!("note: start_session in degraded mode returned {code:?}: {msg}");

    c.ping();
    match c.call(Cmd::Shutdown {
        reason: None,
        mode: None,
    }) {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(&mut d, Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

/// With test media but no game, clips are refused and shutdown is clean.
#[test]
fn not_recording_when_no_game() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("not_recording_when_no_game");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = test_exe_name();
    write_config(dir.path(), &exe);
    let pipe = unique_pipe();
    let mut d = spawn_with(dir.path(), &pipe, &[]);
    let mut c = Client::handshake(&mut d, &pipe);

    // Sent right after startup: the probe may still be running, so the error can be either code.
    match c.call(Cmd::ClipNow { seconds: 4 }) {
        Reply::Error { code, message, .. } => assert!(
            matches!(code, ErrorCode::NotRecording | ErrorCode::WorkerUnavailable),
            "clip_now with no game: code {code:?}, message {message:?}"
        ),
        Reply::ClipQueued { clip_id } => {
            eprintln!(
                "note: clip {clip_id} was queued with no game attached; not treated as a failure"
            );
        }
        other => panic!("clip_now: expected Error or ClipQueued, got {other:?}"),
    }

    match c.call(Cmd::Shutdown {
        reason: None,
        mode: None,
    }) {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(&mut d, Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
}

/// Main flow: detect the window, save a clip by command and by hotkey, split and stop a session,
/// observe the game leave, then shut down.
#[test]
fn full_recording_flow() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("full_recording_flow");
        return;
    }
    let window = TestWindow::create()
        .unwrap_or_else(|e| panic!("full_recording_flow: cannot create the test window: {e}"));
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe);
    let pipe = unique_pipe();
    let mut d = spawn_with(data, &pipe, &[]);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe_all();
    wait_ready(&mut c, data);

    // Game detection.
    c.wait_game(&exe, Duration::from_secs(15));

    // Clip by command. Let the replay buffer fill first.
    c.pump_for(Duration::from_secs(8));
    let clip_id = match c.call(Cmd::ClipNow { seconds: 4 }) {
        Reply::ClipQueued { clip_id } => clip_id,
        other => panic!(
            "clip_now: expected ClipQueued, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    };
    let ev = c.wait_event(
        Duration::from_secs(30),
        "clip_saved for the ClipNow clip",
        |e| matches!(e, Event::ClipSaved { clip_id: id, .. } if *id == clip_id),
    );
    let Event::ClipSaved {
        path,
        duration_ms,
        bytes,
        ..
    } = ev
    else {
        panic!("wait_event returned a non-ClipSaved event: {ev:?}");
    };
    check_output_file(data, "clips", &path, bytes, "ClipNow clip");
    check_clip_duration(&path, duration_ms, "ClipNow clip");
    assert_library_rows(&c, data, RecordingKind::Clip, 1, "after the ClipNow clip");

    // Clip by hotkey.
    press_clip_hotkey();
    let ev = c.wait_event(
        Duration::from_secs(15),
        "clip_saved from the hotkey",
        |e| matches!(e, Event::ClipSaved { clip_id: id, .. } if *id != clip_id),
    );
    let Event::ClipSaved {
        path,
        duration_ms,
        bytes,
        ..
    } = ev
    else {
        panic!("wait_event returned a non-ClipSaved event: {ev:?}");
    };
    check_output_file(data, "clips", &path, bytes, "hotkey clip");
    check_clip_duration(&path, duration_ms, "hotkey clip");
    assert_library_rows(&c, data, RecordingKind::Clip, 2, "after the hotkey clip");

    // Session split: part 1 finishes.
    let mut deferred: Vec<String> = Vec::new();
    c.expect_ok(Cmd::SplitSession, "split_session");
    let ev = c.wait_event(
        Duration::from_secs(30),
        "session_part_finished for part 1",
        |e| matches!(e, Event::SessionPartFinished { part: 1, .. }),
    );
    let Event::SessionPartFinished {
        path,
        bytes,
        session_id,
        ..
    } = ev
    else {
        panic!("wait_event returned a non-SessionPartFinished event: {ev:?}");
    };
    check_output_file(data, "sessions", &path, bytes, "session part 1");
    // Session-part problems are deferred so the rest of the flow still runs; they fail at the end.
    if session_id.is_empty() {
        deferred.push("session_part_finished part 1 has an empty session_id".to_owned());
    }
    deferred.extend(library_rows_problem(
        &c,
        data,
        RecordingKind::Session,
        1,
        "after session part 1",
    ));

    // Part 2 needs at least one completed 2 s segment, otherwise there is nothing to save.
    c.pump_for(Duration::from_secs(6));

    // Session stop: part 2 finishes.
    c.expect_ok(Cmd::StopSession, "stop_session");
    let ev = c.wait_event(
        Duration::from_secs(30),
        "session_part_finished for part 2",
        |e| matches!(e, Event::SessionPartFinished { part: 2, .. }),
    );
    let Event::SessionPartFinished {
        path,
        bytes,
        session_id,
        ..
    } = ev
    else {
        panic!("wait_event returned a non-SessionPartFinished event: {ev:?}");
    };
    check_output_file(data, "sessions", &path, bytes, "session part 2");
    if session_id.is_empty() {
        deferred.push("session_part_finished part 2 has an empty session_id".to_owned());
    }
    deferred.extend(library_rows_problem(
        &c,
        data,
        RecordingKind::Session,
        2,
        "after session part 2",
    ));

    // Close the window and expect game_lost for the test exe.
    drop(window);
    let lost = c.wait_event(Duration::from_secs(20), "game_lost for the test exe", |e| {
        matches!(e, Event::GameLost { game_id, .. } if game_id.to_ascii_lowercase().ends_with(exe.as_str()))
    });
    eprintln!("note: lost {lost:?}");

    // Shutdown.
    match c.call(Cmd::Shutdown {
        reason: None,
        mode: None,
    }) {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(&mut d, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(0),
        "daemon exit status was {status:?}\nlog:\n{}",
        log_text(data)
    );

    assert!(
        deferred.is_empty(),
        "session checks failed (the rest of the flow ran):\n  {}",
        deferred.join("\n  ")
    );
}

/// A subscriber that sends nothing after its Subscribe still receives `clip_saved`. The daemon
/// pushes it while a second connection triggers the clip.
#[test]
fn idle_subscriber_receives_clip_saved_without_sending() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("idle_subscriber_receives_clip_saved_without_sending");
        return;
    }
    let window = TestWindow::create().unwrap_or_else(|e| {
        panic!("idle_subscriber_receives_clip_saved_without_sending: cannot create the test window: {e}")
    });
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    let _log_guard = LogOnPanic(data);
    write_config(data, &exe);
    let pipe = unique_pipe();
    let mut d = spawn_with(data, &pipe, &[]);

    // Client A: subscribe to clips, then send nothing for the rest of the test.
    let mut a = Client::handshake(&mut d, &pipe);
    a.subscribe(vec![Topic::Clips]);

    // Client B: drives the recording and issues the clip.
    let mut b = Client::handshake(&mut d, &pipe);
    b.subscribe_all();
    wait_ready(&mut b, data);
    b.wait_game(&exe, Duration::from_secs(15));
    b.pump_for(Duration::from_secs(8));

    let clip_id = match b.call(Cmd::ClipNow { seconds: 1 }) {
        Reply::ClipQueued { clip_id } => clip_id,
        other => panic!(
            "clip_now: expected ClipQueued, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    };

    // A has been silent since its Subscribe. Its clip_saved must arrive by push alone.
    let ev = a.wait_event(
        Duration::from_secs(15),
        "clip_saved pushed to the idle subscriber",
        |e| matches!(e, Event::ClipSaved { clip_id: id, .. } if *id == clip_id),
    );
    let Event::ClipSaved { path, bytes, .. } = ev else {
        panic!("wait_event returned a non-ClipSaved event: {ev:?}");
    };
    check_output_file(data, "clips", &path, bytes, "idle subscriber clip");

    drop(window);
    match b.call(Cmd::Shutdown {
        reason: None,
        mode: Some(ShutdownMode::Force),
    }) {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(&mut d, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(0),
        "daemon exit status was {status:?}\nlog:\n{}",
        log_text(data)
    );
}

/// An idle subscriber does not stop the daemon from shutting down. Shutdown from a second
/// connection exits with status 0 within 3 s.
#[test]
fn shutdown_with_idle_subscriber_exits() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);

    // Client A subscribes and then sends nothing.
    let mut a = Client::handshake(&mut d, &pipe);
    a.subscribe_all();

    // Client B asks the daemon to stop.
    let mut b = Client::handshake(&mut d, &pipe);
    match b.call(Cmd::Shutdown {
        reason: None,
        mode: None,
    }) {
        Reply::ShutdownAck => {}
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(&mut d, Duration::from_secs(3));
    assert_eq!(status.code(), Some(0), "daemon exit status was {status:?}");
    drop(a);
    drop(b);
}

/// Prints the daemon log when the owning test panics, so failures explain themselves.
struct LogOnPanic<'a>(&'a Path);

impl Drop for LogOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "---- daemon log ----\n{}\n---- end log ----",
                log_text(self.0)
            );
        }
    }
}
