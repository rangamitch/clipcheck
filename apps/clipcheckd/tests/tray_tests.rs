//! Tray tests.
//!
//! The pure decision functions are tested on every platform. The integration tests
//! (Windows only) spawn the real daemon, find its hidden tray window by class and
//! owning pid, post tray messages to it, and check the daemon's reaction over the
//! IPC pipe and the process exit code. A tray window that does not appear within 15 s fails
//! the test (`require_tray`); it is not a skip.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use clipcheckd::tray::{
    decide_exit, format_elapsed, menu_items, tooltip_text, ui_target, MenuItem,
};

#[test]
fn menu_has_exactly_open_then_exit() {
    // Since issue #93 the menu is typed: each item is a MenuItem value with its label.
    let items = menu_items();
    assert_eq!(items.len(), 2, "menu must have exactly two items");
    assert_eq!(items[0], (MenuItem::Open, "Open ClipCheck"));
    assert_eq!(items[1], (MenuItem::Exit, "Exit"));
}

#[test]
fn decide_exit_covers_all_eight_combinations() {
    // (session_active, confirm_enabled, closure answer, expected result, closure called)
    let table: [(bool, bool, bool, bool, bool); 8] = [
        (false, false, false, true, false),
        (false, false, true, true, false),
        (false, true, false, true, false),
        (false, true, true, true, false),
        (true, false, false, true, false),
        (true, false, true, true, false),
        (true, true, false, false, true),
        (true, true, true, true, true),
    ];
    for (session, confirm, answer, expected, should_call) in table {
        let called = Cell::new(false);
        let result = decide_exit(session, confirm, || {
            called.set(true);
            answer
        });
        assert_eq!(
            called.get(),
            should_call,
            "session={session} confirm={confirm} answer={answer}: closure called = {}",
            called.get()
        );
        assert_eq!(
            result, expected,
            "session={session} confirm={confirm} answer={answer}: wrong result"
        );
    }
}

#[test]
fn format_elapsed_examples() {
    assert_eq!(format_elapsed(0), "0:00:00");
    assert_eq!(format_elapsed(3599), "0:59:59");
    assert_eq!(format_elapsed(3600), "1:00:00");
    assert_eq!(format_elapsed(42 * 60 + 17), "0:42:17");
}

#[test]
fn tooltip_text_examples() {
    assert_eq!(tooltip_text(None), "ClipCheck");
    assert_eq!(
        tooltip_text(Some((Some("Hades"), 2537))),
        "ClipCheck \u{2014} recording Hades 0:42:17"
    );
    assert_eq!(
        tooltip_text(Some((None, 5))),
        "ClipCheck \u{2014} recording 0:00:05"
    );
}

#[test]
fn ui_target_examples() {
    let daemon = Path::new(r"C:\app\clipcheckd.exe");
    let app = Path::new(r"C:\app\ClipCheck.exe");
    assert_eq!(
        ui_target(daemon, true),
        PathBuf::from(r"C:\app\ClipCheck.exe")
    );
    assert_eq!(ui_target(daemon, false), daemon.to_path_buf());
    assert_eq!(ui_target(app, true), app.to_path_buf());
}

mod common;

#[cfg(windows)]
mod integration {
    use std::fs;
    use std::path::Path;
    use std::process::{Command, ExitStatus, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use clipcheck_ipc::frame::read_frame;
    use clipcheck_ipc::proto::{Command as Cmd, Reply, Topic};
    use clipcheck_ipc::PipeConn;
    use clipcheckd::tray::{CALLBACK_MESSAGE, MENU_ID_EXIT, TRAY_CLASS};
    use serde_json::Value;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId, PostMessageW,
    };

    use crate::common::*;

    /// `WM_COMMAND`.
    const WM_COMMAND: u32 = 0x0111;
    /// `WM_LBUTTONUP`, sent as the callback lparam.
    const WM_LBUTTONUP: isize = 0x0202;
    /// `WM_LBUTTONDBLCLK`, sent as the callback lparam.
    const WM_LBUTTONDBLCLK: isize = 0x0203;

    /// Spawns the daemon with `--no-encoders`. Test-override variables are cleared first
    /// so the caller's environment cannot leak in; `envs` sets the ones the test needs.
    fn spawn(data_dir: &Path, pipe: &str, envs: &[(&str, &str)]) -> Daemon {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_clipcheckd"));
        cmd.arg("--data-dir")
            .arg(data_dir)
            .arg("--pipe-name")
            .arg(pipe)
            .arg("--no-encoders")
            .env_remove("CLIPCHECK_UI_COMMAND_OVERRIDE")
            .env_remove("CLIPCHECK_TEST_SESSION_ACTIVE")
            .env_remove("CLIPCHECK_TEST_CONFIRM")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        Daemon {
            child: cmd.spawn().expect("failed to spawn clipcheckd"),
        }
    }

    /// Connects, completes the handshake, and subscribes to the state topic.
    fn connect_subscribed(d: &mut Daemon, pipe: &str) -> PipeConn {
        let mut conn = connect(d, pipe);
        match call(&mut conn, 1, hello_cmd()).body {
            Reply::HelloAck { .. } => {}
            other => panic!("expected HelloAck, got {other:?}"),
        }
        match call(
            &mut conn,
            2,
            Cmd::Subscribe {
                topics: vec![Topic::State],
            },
        )
        .body
        {
            Reply::Subscribed { .. } => {}
            other => panic!("expected Subscribed, got {other:?}"),
        }
        conn
    }

    /// Moves the connection to a reader thread that forwards every pushed frame. A
    /// hung daemon then cannot block the test, because callers use `recv_timeout`.
    fn event_stream(mut conn: PipeConn) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(frame) = read_frame::<_, Value>(&mut conn) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn is_app_exit(frame: &Value) -> bool {
        frame.pointer("/body/type").and_then(Value::as_str) == Some("app_exit")
    }

    /// True if an `app_exit` event arrives within `timeout`.
    fn saw_app_exit(rx: &mpsc::Receiver<Value>, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(frame) if is_app_exit(&frame) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    }

    /// Waits for the daemon to exit. `None` on timeout.
    fn wait_exit(d: &mut Daemon, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = d.child.try_wait().expect("try_wait failed") {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Every `ClipCheckTray` window owned by process `pid`. Matching by pid keeps the
    /// test from acting on a tray that belongs to some other ClipCheck process.
    fn tray_windows_of(pid: u32) -> Vec<HWND> {
        let class = wide(TRAY_CLASS);
        let mut found = Vec::new();
        let mut after = HWND::default();
        loop {
            // SAFETY: the class buffer is NUL-terminated and lives for the call.
            let next = unsafe {
                FindWindowExW(
                    HWND::default(),
                    after,
                    PCWSTR(class.as_ptr()),
                    PCWSTR(std::ptr::null()),
                )
            };
            let Ok(hwnd) = next else { break };
            let mut owner = 0u32;
            // SAFETY: `owner` is a valid out-pointer for the call.
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner as *mut u32)) };
            if owner == pid {
                found.push(hwnd);
            }
            after = hwnd;
        }
        found
    }

    /// Waits up to 15 s for the daemon's tray window. `None` means it never appeared
    /// (the caller then skips). A daemon that exits meanwhile is a test failure.
    fn wait_for_tray(d: &mut Daemon) -> Option<HWND> {
        let pid = d.child.id();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = d.child.try_wait().expect("try_wait failed") {
                panic!("daemon exited with {status:?} before its tray window appeared");
            }
            if let Some(hwnd) = tray_windows_of(pid).into_iter().next() {
                return Some(hwnd);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn post(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) {
        // SAFETY: PostMessageW only queues the message; the window is the daemon's.
        unsafe { PostMessageW(hwnd, msg, WPARAM(wparam), LPARAM(lparam)) }
            .expect("PostMessageW failed");
    }

    fn wait_for_file(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        path.exists()
    }

    /// A missing tray window fails the test: the tray is what every test in this module exercises,
    /// and the daemon only runs without it after the icon cannot be added (tray.rs), which these
    /// tests must not hide.
    fn require_tray(d: &mut Daemon) -> HWND {
        wait_for_tray(d).unwrap_or_else(|| {
            panic!(
                "no ClipCheckTray window appeared within 15 s; the tray is required for this test"
            )
        })
    }

    #[test]
    fn i1_left_click_and_double_click_open_ui() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("opened.marker");
        let script = dir.path().join("open_ui.cmd");
        fs::write(
            &script,
            format!("@echo off\r\necho opened > \"{}\"\r\n", marker.display()),
        )
        .expect("write script");
        let pipe = unique_pipe();
        let mut d = spawn(
            dir.path(),
            &pipe,
            &[(
                "CLIPCHECK_UI_COMMAND_OVERRIDE",
                script.to_str().expect("utf-8 path"),
            )],
        );
        let hwnd = require_tray(&mut d);

        post(hwnd, CALLBACK_MESSAGE, 0, WM_LBUTTONUP);
        assert!(
            wait_for_file(&marker, Duration::from_secs(10)),
            "WM_LBUTTONUP did not open the UI (marker missing after 10 s)"
        );

        fs::remove_file(&marker).expect("remove marker");
        // The tray debounces repeated open requests (1.5 s), so the first click above would
        // swallow an immediate double click. Keep posting it until the debounce window has
        // passed; the marker can only appear if WM_LBUTTONDBLCLK itself opens the UI.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut opened = false;
        while std::time::Instant::now() < deadline {
            post(hwnd, CALLBACK_MESSAGE, 0, WM_LBUTTONDBLCLK);
            if wait_for_file(&marker, Duration::from_millis(400)) {
                opened = true;
                break;
            }
        }
        assert!(
            opened,
            "WM_LBUTTONDBLCLK did not open the UI (marker missing after 10 s)"
        );
    }

    #[test]
    fn i2_menu_exit_without_session_exits_zero() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe, &[]);
        let hwnd = require_tray(&mut d);
        let conn = connect_subscribed(&mut d, &pipe);
        let rx = event_stream(conn);

        post(hwnd, WM_COMMAND, usize::from(MENU_ID_EXIT), 0);
        assert!(
            saw_app_exit(&rx, Duration::from_secs(10)),
            "no app_exit event within 10 s of menu Exit"
        );
        let status = wait_exit(&mut d, Duration::from_secs(10));
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(0),
            "daemon did not exit with code 0 within 10 s: {status:?}"
        );
    }

    #[test]
    fn i3_menu_exit_with_session_confirm_no_keeps_running() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(
            dir.path(),
            &pipe,
            &[
                ("CLIPCHECK_TEST_SESSION_ACTIVE", "1"),
                ("CLIPCHECK_TEST_CONFIRM", "no"),
            ],
        );
        let hwnd = require_tray(&mut d);
        let conn = connect_subscribed(&mut d, &pipe);
        let rx = event_stream(conn);

        post(hwnd, WM_COMMAND, usize::from(MENU_ID_EXIT), 0);
        // The exit handler logs this line once the No answer is processed. Waiting for it is the
        // signal; the daemon must still be running after it.
        assert!(
            wait_for_log(
                dir.path(),
                "exit from the tray cancelled",
                Duration::from_secs(10)
            ),
            "the daemon did not log the cancelled exit; log:\n{}",
            log_text(dir.path())
        );

        assert!(
            d.child.try_wait().expect("try_wait failed").is_none(),
            "daemon exited although the user answered No"
        );
        assert!(
            !rx.try_iter().any(|frame| is_app_exit(&frame)),
            "app_exit was sent although the user answered No"
        );
    }

    #[test]
    fn i4_menu_exit_with_session_confirm_yes_exits_zero() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(
            dir.path(),
            &pipe,
            &[
                ("CLIPCHECK_TEST_SESSION_ACTIVE", "1"),
                ("CLIPCHECK_TEST_CONFIRM", "yes"),
            ],
        );
        let hwnd = require_tray(&mut d);
        let conn = connect_subscribed(&mut d, &pipe);
        let rx = event_stream(conn);

        post(hwnd, WM_COMMAND, usize::from(MENU_ID_EXIT), 0);
        assert!(
            saw_app_exit(&rx, Duration::from_secs(10)),
            "no app_exit event within 10 s after confirming Exit"
        );
        let status = wait_exit(&mut d, Duration::from_secs(10));
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(0),
            "daemon did not exit with code 0 within 10 s: {status:?}"
        );
    }

    #[test]
    fn i5_shutdown_command_removes_tray_window() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe, &[]);
        let pid = d.child.id();
        let _hwnd = require_tray(&mut d);
        let mut conn = connect(&mut d, &pipe);
        call(&mut conn, 1, hello_cmd());
        match call(
            &mut conn,
            2,
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

        let status = wait_exit(&mut d, Duration::from_secs(10));
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(0),
            "daemon did not exit with code 0 after Shutdown: {status:?}"
        );
        assert!(
            tray_windows_of(pid).is_empty(),
            "ClipCheckTray window of pid {pid} still exists after exit"
        );
    }

    #[test]
    fn i6_menu_exit_without_session_needs_no_prompt() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn(dir.path(), &pipe, &[("CLIPCHECK_TEST_CONFIRM", "no")]);
        let hwnd = require_tray(&mut d);

        post(hwnd, WM_COMMAND, usize::from(MENU_ID_EXIT), 0);
        let status = wait_exit(&mut d, Duration::from_secs(10));
        assert_eq!(
            status.and_then(|s| s.code()),
            Some(0),
            "no-session Exit must exit with code 0 without asking: {status:?}"
        );
    }
}
