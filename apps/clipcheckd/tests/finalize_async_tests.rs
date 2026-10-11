//! Finalize-async tests (D48, issue #165): the daemon writes session parts off its service
//! thread. Split and stop reply Ok at once, status and ping stay responsive while a part is
//! written, a part write failure is reported as a `session_part` warning, and the daemon keeps
//! running.
//!
//! Windows only. Needs native test media (`clipcheck_testkit::available()`) and an interactive desktop for the test
//! window. Missing test media is a `SKIPPED:` report.

#![cfg(windows)]

mod common;

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_ipc::proto::{
    Command as Cmd, Event, RecState, Reply, ShutdownMode, StatusDto, Topic,
};

use common::window::{press_chord, TestWindow, VK_CONTROL, VK_F20, VK_MENU, VK_SHIFT};
use common::*;

/// Writes `<data_dir>/config.toml` for the recording tests. The test exe is the only allowlisted
/// game, and `chord` is the single clip hotkey. Output stays under `data_dir/recordings`.
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

/// Sends `Shutdown` in force mode (a session may be recording) and waits for the process to exit.
fn force_shutdown(c: &mut Client, d: &mut Daemon) {
    match c.call(Cmd::Shutdown {
        reason: None,
        mode: Some(ShutdownMode::Force),
    }) {
        Reply::ShutdownAck => {}
        other => eprintln!("note: force shutdown reply was {other:?}"),
    }
    let status = wait_exit(d, Duration::from_secs(60));
    eprintln!("note: daemon exit status {status:?}");
}

/// Milliseconds with one decimal, for report lines.
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Runs `f` and returns its result with the elapsed time.
fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

/// Index of the first seen event matching `pred`.
fn seen_index(c: &Client, pred: impl Fn(&Event) -> bool) -> Option<usize> {
    c.seen.iter().position(pred)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A session records for about 8 s. StopSession replies Ok without waiting for the part write.
/// Sixteen status calls back to back each answer within 1 s. SessionPartFinished for part 1
/// arrives within 60 s, and finalizing_parts returns to 0.
#[test]
fn status_poll_during_stop_stays_fast() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("status_poll_during_stop_stays_fast");
        return;
    }
    let window = TestWindow::create().unwrap_or_else(|e| {
        panic!("status_poll_during_stop_stays_fast: cannot create the test window: {e}")
    });
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Sessions, Topic::State, Topic::Warnings]);
    wait_ready(&mut c, data);
    wait_for_game(&mut c, data);

    if c.status().session.is_none() {
        c.expect_ok(Cmd::StartSession, "start_session");
    }
    wait_status(
        &mut c,
        Duration::from_secs(15),
        "session after start",
        |s| s.session.is_some(),
    );
    c.pump_for(Duration::from_secs(8));

    let (reply, stop_dur) = timed(|| c.call(Cmd::StopSession));
    match reply {
        Reply::Ok => {}
        other => panic!(
            "stop_session: expected Ok, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    }
    println!("stop_session reply time: {:.1} ms", ms(stop_dur));

    let mut max_status = Duration::ZERO;
    let mut saw_finalizing = false;
    for i in 0..15 {
        let (frame, dur) = timed(|| c.call(Cmd::GetStatus));
        let s = match frame {
            Reply::Status(s) => s,
            other => panic!("get_status #{i}: expected Status, got {other:?}"),
        };
        assert!(
            dur < Duration::from_millis(1000),
            "get_status #{i} took {:.1} ms (limit 1000 ms)\nlog:\n{}",
            ms(dur),
            log_text(data)
        );
        if s.finalizing_parts >= 1 {
            saw_finalizing = true;
        }
        max_status = max_status.max(dur);
    }
    println!(
        "max get_status latency over 15 calls: {:.1} ms",
        ms(max_status)
    );
    println!("observed finalizing_parts >= 1 during poll: {saw_finalizing} (report-only)");

    c.wait_event(
        Duration::from_secs(60),
        "SessionPartFinished for part 1",
        |e| matches!(e, Event::SessionPartFinished { part: 1, .. }),
    );
    let s = wait_status(
        &mut c,
        Duration::from_secs(10),
        "finalizing_parts == 0",
        |s| s.finalizing_parts == 0,
    );
    assert_eq!(s.finalizing_parts, 0, "finalizing_parts after finish");

    drop(window);
    force_shutdown(&mut c, &mut d);
}

/// Split with an active clip hotkey. Split replies Ok at once. Three clip chord presses and
/// pings are interleaved, and every reply is under 1 s. SessionPartFinished part 1 arrives before
/// SessionPartStarted part 2. At least one ClipSaved arrives. The session is at part 2.
#[test]
fn split_during_hotkey_burst() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("split_during_hotkey_burst");
        return;
    }
    let window = TestWindow::create().unwrap_or_else(|e| {
        panic!("split_during_hotkey_burst: cannot create the test window: {e}")
    });
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![
        Topic::Clips,
        Topic::Sessions,
        Topic::State,
        Topic::Warnings,
    ]);
    wait_ready(&mut c, data);
    wait_for_game(&mut c, data);

    if c.status().session.is_none() {
        c.expect_ok(Cmd::StartSession, "start_session");
    }
    wait_status(
        &mut c,
        Duration::from_secs(15),
        "session after start",
        |s| s.session.as_ref().is_some_and(|x| x.part == 1),
    );
    // Fill the replay buffer before the first clip.
    c.pump_for(Duration::from_secs(6));

    let (reply, split_dur) = timed(|| c.call(Cmd::SplitSession));
    match reply {
        Reply::Ok => {}
        other => panic!(
            "split_session: expected Ok, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    }
    println!("split_session reply time: {:.1} ms", ms(split_dur));
    assert!(
        split_dur < Duration::from_millis(1000),
        "split_session took {:.1} ms (limit 1000 ms)",
        ms(split_dur)
    );

    let mut max_reply = split_dur;
    for i in 0..3 {
        press_chord(&[VK_CONTROL, VK_MENU, VK_SHIFT, VK_F20]);
        let (reply, dur) = timed(|| c.call(Cmd::Ping));
        match reply {
            Reply::Pong { .. } => {}
            other => panic!("ping after press #{i}: expected Pong, got {other:?}"),
        }
        assert!(
            dur < Duration::from_millis(1000),
            "ping after press #{i} took {:.1} ms (limit 1000 ms)\nlog:\n{}",
            ms(dur),
            log_text(data)
        );
        max_reply = max_reply.max(dur);
        println!("ping after press #{i}: {:.1} ms", ms(dur));
        thread::sleep(Duration::from_millis(300));
    }
    println!("max reply latency in burst: {:.1} ms", ms(max_reply));

    c.wait_event(Duration::from_secs(60), "SessionPartFinished part 1", |e| {
        matches!(e, Event::SessionPartFinished { part: 1, .. })
    });
    c.wait_event(Duration::from_secs(60), "SessionPartStarted part 2", |e| {
        matches!(e, Event::SessionPartStarted { part: 2, .. })
    });

    // Let any queued clip saves finish, up to 60 s.
    let deadline = Instant::now() + Duration::from_secs(60);
    while c.clip_saved_count() < 3 && Instant::now() < deadline {
        c.pump_for(Duration::from_millis(250));
    }
    println!("ClipSaved events received: {} of 3", c.clip_saved_count());
    assert!(
        c.clip_saved_count() >= 1,
        "no ClipSaved arrived after the hotkey burst\nlog:\n{}",
        log_text(data)
    );

    let fin = seen_index(&c, |e| {
        matches!(e, Event::SessionPartFinished { part: 1, .. })
    });
    let started = seen_index(&c, |e| {
        matches!(e, Event::SessionPartStarted { part: 2, .. })
    });
    match (fin, started) {
        (Some(f), Some(s)) => assert!(
            s > f,
            "SessionPartStarted part 2 (index {s}) came before SessionPartFinished part 1 (index {f})"
        ),
        other => panic!("missing events for ordering check: {other:?}"),
    }

    let s = wait_status(&mut c, Duration::from_secs(15), "session at part 2", |s| {
        s.session.as_ref().is_some_and(|x| x.part == 2)
    });
    assert_eq!(s.session.as_ref().map(|x| x.part), Some(2));
    c.ping();

    drop(window);
    force_shutdown(&mut c, &mut d);
}

/// The Sessions folder is replaced by a regular file while part 1 records. StopSession still
/// replies Ok. A session_part warning containing "could not be saved" arrives within 60 s, with no
/// SessionPartFinished for part 1. The daemon answers ping and finalizing_parts returns to 0.
#[test]
fn part_write_failure_is_reported_and_daemon_survives() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("part_write_failure_is_reported_and_daemon_survives");
        return;
    }
    let window = TestWindow::create().unwrap_or_else(|e| {
        panic!("part_write_failure_is_reported_and_daemon_survives: cannot create the test window: {e}")
    });
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Sessions, Topic::State, Topic::Warnings]);
    wait_ready(&mut c, data);
    wait_for_game(&mut c, data);

    if c.status().session.is_none() {
        c.expect_ok(Cmd::StartSession, "start_session");
    }
    wait_status(
        &mut c,
        Duration::from_secs(15),
        "session after start",
        |s| s.session.is_some(),
    );
    c.pump_for(Duration::from_secs(6));

    let sessions = data.join("recordings").join("Sessions");
    // The daemon creates `Sessions` only when a part is written, so it may not exist yet while
    // part 1 records. Remove it if present, then put a regular file in its place.
    fs::create_dir_all(data.join("recordings")).expect("create recordings root");
    if sessions.is_dir() {
        fs::remove_dir_all(&sessions)
            .unwrap_or_else(|e| panic!("cannot remove {}: {e}", sessions.display()));
    }
    fs::write(&sessions, b"blocker").expect("create blocker file named Sessions");

    c.expect_ok(Cmd::StopSession, "stop_session");

    let warning = c.wait_event(Duration::from_secs(60), "session_part warning", |e| {
        matches!(e, Event::Warning { code, message }
            if code == "session_part" && message.contains("could not be saved"))
    });
    println!("warning event: {warning:?}");

    let finished_part1 = c
        .seen
        .iter()
        .any(|e| matches!(e, Event::SessionPartFinished { part: 1, .. }));
    assert!(
        !finished_part1,
        "SessionPartFinished part 1 arrived although the write must fail\nlog:\n{}",
        log_text(data)
    );

    c.ping();

    // Status must eventually show nothing being finalized. Absent key is 0.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let n = c.status().finalizing_parts;
        if n == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "finalizing_parts still {n} after 60 s\nlog:\n{}",
            log_text(data)
        );
        thread::sleep(Duration::from_millis(200));
    }

    fs::remove_file(&sessions).expect("remove blocker file");
    c.ping();

    drop(window);
    force_shutdown(&mut c, &mut d);
}
