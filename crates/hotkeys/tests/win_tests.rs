#![cfg(windows)]

//! Windows integration tests for the RegisterHotKey backend.
//!
//! These tests register real global hotkeys and inject real keystrokes with
//! `SendInput`, so they must run on an interactive desktop, single-threaded:
//! `cargo test -p clipcheck-hotkeys --test win_tests -- --test-threads=1`
//!
//! If `SendInput` injects nothing (UIPI, a secure desktop, or a remote
//! session), the test FAILS with the OS error. It never passes silently.

use std::mem::size_of;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use clipcheck_hotkeys::{
    ApplyError, Binding, Chord, FailureReason, HotkeyAction, HotkeyEvent, HotkeyRegistry,
    HotkeyService, PressKind, Registry, StartError,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24, VK_MENU, VK_SHIFT,
};

const MODS: [VIRTUAL_KEY; 3] = [VK_CONTROL, VK_MENU, VK_SHIFT];

fn chord(s: &str) -> Chord {
    s.parse::<Chord>()
        .unwrap_or_else(|e| panic!("failed to parse chord {s:?}: {e:?}"))
}

/// Builds a registry from `(chord, action, enabled)` triples, clip max 300s.
fn registry(list: &[(&str, HotkeyAction, bool)]) -> Registry {
    let bindings: Vec<Binding> = list
        .iter()
        .map(|(c, a, en)| Binding {
            chord: chord(c),
            action: a.clone(),
            press: PressKind::Tap,
            enabled: *en,
        })
        .collect();
    HotkeyRegistry::build(&bindings, 300).unwrap_or_else(|e| panic!("registry build failed: {e:?}"))
}

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Presses mods + key, then releases key + mods. Returns SendInput's count.
///
/// Panics when nothing was injected, with the OS error, so a blocked desktop
/// fails the test instead of skipping it.
fn send_chord(mods: &[VIRTUAL_KEY], key: VIRTUAL_KEY) -> u32 {
    let mut inputs: Vec<INPUT> = Vec::new();
    for m in mods {
        inputs.push(key_input(*m, false));
    }
    inputs.push(key_input(key, false));
    inputs.push(key_input(key, true));
    for m in mods.iter().rev() {
        inputs.push(key_input(*m, true));
    }
    let expected = inputs.len() as u32;
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent == 0 {
        let os_error = std::io::Error::last_os_error();
        panic!(
            "SendInput injected 0 of {expected} events (OS error: {os_error}). \
             Synthetic input is blocked (UIPI, secure desktop, or remote session). \
             Run these tests on an interactive desktop."
        );
    }
    sent
}

#[test]
fn fires_on_sendinput() {
    let reg = registry(&[("Ctrl+Alt+Shift+F23", HotkeyAction::SessionSplit, true)]);
    let (tx, rx) = mpsc::channel::<HotkeyEvent>();
    let (svc, _) = HotkeyService::start(&reg, tx).unwrap_or_else(|e| panic!("start failed: {e:?}"));

    let sent = send_chord(&MODS, VK_F23);
    let event = rx.recv_timeout(Duration::from_secs(3));
    svc.stop();
    assert_eq!(
        event,
        Ok(HotkeyEvent {
            action: HotkeyAction::SessionSplit
        }),
        "expected SessionSplit event within 3s (SendInput returned {sent})"
    );
}

#[test]
fn second_instance_reports_taken() {
    let reg = registry(&[("Ctrl+Alt+Shift+F23", HotkeyAction::SessionSplit, true)]);
    let (tx_a, _rx_a) = mpsc::channel::<HotkeyEvent>();
    let (svc_a, _) = HotkeyService::start(&reg, tx_a)
        .unwrap_or_else(|e| panic!("service A should start: {e:?}"));

    let (tx_b, _rx_b) = mpsc::channel::<HotkeyEvent>();
    let result = HotkeyService::start(&reg, tx_b);
    let (svc_b, failures) = match result {
        Ok(v) => v,
        Err(e) => {
            svc_a.stop();
            panic!("service B should start (conflict is per binding): {e:?}");
        }
    };
    svc_b.stop();
    svc_a.stop();

    assert!(
        failures
            .iter()
            .any(|f| f.reason == FailureReason::TakenByAnotherApp),
        "expected a TakenByAnotherApp failure, got {failures:?}"
    );
}

#[test]
fn stop_joins_promptly() {
    let reg = registry(&[("Ctrl+Alt+Shift+F16", HotkeyAction::MuteMic, true)]);
    let (tx, _rx) = mpsc::channel::<HotkeyEvent>();
    let (svc, _) = HotkeyService::start(&reg, tx).unwrap_or_else(|e| panic!("start failed: {e:?}"));

    let t = Instant::now();
    svc.stop();
    let elapsed = t.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "stop() took {elapsed:?}, expected < 2s"
    );
}

#[test]
fn apply_swaps_bindings() {
    let old_reg = registry(&[("Ctrl+Alt+Shift+F22", HotkeyAction::SessionSplit, true)]);
    let new_reg = registry(&[("Ctrl+Alt+Shift+F20", HotkeyAction::ToggleBuffer, true)]);
    let (tx, rx) = mpsc::channel::<HotkeyEvent>();
    let (svc, _) =
        HotkeyService::start(&old_reg, tx).unwrap_or_else(|e| panic!("start failed: {e:?}"));

    match svc.apply(new_reg) {
        Ok(failures) if failures.is_empty() => {}
        other => {
            svc.stop();
            panic!("apply should return Ok(no failures), got {other:?}");
        }
    }

    send_chord(&MODS, VK_F20);
    let new_event = rx.recv_timeout(Duration::from_secs(3));
    if new_event
        != Ok(HotkeyEvent {
            action: HotkeyAction::ToggleBuffer,
        })
    {
        svc.stop();
        panic!("expected ToggleBuffer after apply, got {new_event:?}");
    }

    send_chord(&MODS, VK_F22);
    let old_event = rx.recv_timeout(Duration::from_millis(500));
    svc.stop();
    assert!(
        matches!(old_event, Err(RecvTimeoutError::Timeout)),
        "old chord Ctrl+Alt+Shift+F22 should produce no event, got {old_event:?}"
    );
}

#[test]
fn apply_reports_taken_chord() {
    let empty = registry(&[("Ctrl+Alt+Shift+F15", HotkeyAction::MuteMic, false)]);
    let (tx_a, _rx_a) = mpsc::channel::<HotkeyEvent>();
    let (svc_a, _) = HotkeyService::start(&empty, tx_a)
        .unwrap_or_else(|e| panic!("service A (no enabled bindings) should start: {e:?}"));

    // Service B holds F19 for the duration of this test.
    let holder = registry(&[("Ctrl+Alt+Shift+F19", HotkeyAction::SessionStopStart, true)]);
    let (tx_b, _rx_b) = mpsc::channel::<HotkeyEvent>();
    let (svc_b, _) = HotkeyService::start(&holder, tx_b)
        .unwrap_or_else(|e| panic!("service B should start: {e:?}"));

    let want = registry(&[("Ctrl+Alt+Shift+F19", HotkeyAction::ToggleBuffer, true)]);
    let result = svc_a.apply(want);
    svc_a.stop();
    svc_b.stop();

    match result {
        Ok(v) => {
            assert!(!v.is_empty(), "failure list should not be empty");
            assert_eq!(
                v[0].reason,
                FailureReason::TakenByAnotherApp,
                "expected TakenByAnotherApp, got {:?}",
                v[0]
            );
        }
        other => panic!("expected Ok(failures), got {other:?}"),
    }
}

#[test]
fn disabled_bindings_not_registered() {
    let reg = registry(&[("Ctrl+Alt+Shift+F18", HotkeyAction::MuteMic, false)]);
    let (tx_a, _rx_a) = mpsc::channel::<HotkeyEvent>();
    let (svc_a, _) = HotkeyService::start(&reg, tx_a)
        .unwrap_or_else(|e| panic!("disabled-only service should start: {e:?}"));

    let (tx_b, _rx_b) = mpsc::channel::<HotkeyEvent>();
    let result = HotkeyService::start(&reg, tx_b);
    svc_a.stop();
    match result {
        Ok((svc_b, failures)) => {
            svc_b.stop();
            assert!(
                failures.is_empty(),
                "disabled chord should be free, got {failures:?}"
            );
        }
        Err(e) => panic!("chord of a disabled binding should be free, got {e:?}"),
    }
}

#[test]
fn drop_stops_service() {
    let reg = registry(&[(
        "Ctrl+Alt+Shift+F17",
        HotkeyAction::ClipLast {
            name: "t".to_string(),
            seconds: 10,
        },
        true,
    )]);
    {
        let (tx, _rx) = mpsc::channel::<HotkeyEvent>();
        let _svc =
            HotkeyService::start(&reg, tx).unwrap_or_else(|e| panic!("first start failed: {e:?}"));
        // dropped at end of scope, no explicit stop
    }

    let (tx, _rx) = mpsc::channel::<HotkeyEvent>();
    match HotkeyService::start(&reg, tx) {
        Ok((svc, _)) => svc.stop(),
        Err(e) => panic!("chord should be free after drop, got {e:?}"),
    }
}

#[test]
fn refused_binding_does_not_block_others() {
    let holder = registry(&[("Ctrl+Alt+Shift+F21", HotkeyAction::SessionSplit, true)]);
    let (tx_a, _rx_a) = mpsc::channel::<HotkeyEvent>();
    let (svc_a, _) = HotkeyService::start(&holder, tx_a)
        .unwrap_or_else(|e| panic!("service A should start: {e:?}"));

    let reg_b = registry(&[
        ("Ctrl+Alt+Shift+F21", HotkeyAction::ToggleBuffer, true),
        ("Ctrl+Alt+Shift+F22", HotkeyAction::MuteMic, true),
    ]);
    let (tx_b, rx_b) = mpsc::channel::<HotkeyEvent>();
    let (svc_b, failures) = match HotkeyService::start(&reg_b, tx_b) {
        Ok(v) => v,
        Err(e) => {
            svc_a.stop();
            panic!("service B should start (conflict is per binding): {e:?}");
        }
    };

    let sent = send_chord(&MODS, VK_F22);
    let event = rx_b.recv_timeout(Duration::from_secs(3));
    svc_b.stop();
    svc_a.stop();

    assert_eq!(
        failures.len(),
        1,
        "expected exactly one failure, got {failures:?}"
    );
    assert_eq!(failures[0].chord, chord("Ctrl+Alt+Shift+F21"));
    assert_eq!(failures[0].action, HotkeyAction::ToggleBuffer);
    assert_eq!(failures[0].reason, FailureReason::TakenByAnotherApp);

    assert_eq!(
        event,
        Ok(HotkeyEvent {
            action: HotkeyAction::MuteMic
        }),
        "non-conflicting F22 binding should still fire MuteMic (SendInput returned {sent})"
    );
}

#[test]
fn apply_reports_refused_and_keeps_others() {
    let holder = registry(&[("Ctrl+Alt+Shift+F24", HotkeyAction::SessionStopStart, true)]);
    let (tx_a, _rx_a) = mpsc::channel::<HotkeyEvent>();
    let (svc_a, _) = HotkeyService::start(&holder, tx_a)
        .unwrap_or_else(|e| panic!("service A should start: {e:?}"));

    let start_reg = registry(&[("Ctrl+Alt+Shift+F23", HotkeyAction::SessionSplit, true)]);
    let (tx_b, rx_b) = mpsc::channel::<HotkeyEvent>();
    let (svc_b, _) = match HotkeyService::start(&start_reg, tx_b) {
        Ok(v) => v,
        Err(e) => {
            svc_a.stop();
            panic!("service B should start: {e:?}");
        }
    };

    let want = registry(&[
        ("Ctrl+Alt+Shift+F24", HotkeyAction::ToggleBuffer, true),
        ("Ctrl+Alt+Shift+F22", HotkeyAction::MuteMic, true),
    ]);
    let result = svc_b.apply(want);
    let sent = send_chord(&MODS, VK_F22);
    let event = rx_b.recv_timeout(Duration::from_secs(3));
    svc_b.stop();
    svc_a.stop();

    let failures = match result {
        Ok(f) => f,
        Err(e) => panic!("apply should return Ok(failures), got {e:?}"),
    };
    assert_eq!(
        failures.len(),
        1,
        "expected exactly one failure, got {failures:?}"
    );
    assert_eq!(failures[0].chord, chord("Ctrl+Alt+Shift+F24"));
    assert_eq!(failures[0].reason, FailureReason::TakenByAnotherApp);

    assert_eq!(
        event,
        Ok(HotkeyEvent {
            action: HotkeyAction::MuteMic
        }),
        "non-conflicting F22 binding should be active after apply (SendInput returned {sent})"
    );
}

#[test]
fn all_conflicting_bindings_report_and_stay_silent() {
    let holder = registry(&[
        ("Ctrl+Alt+Shift+F21", HotkeyAction::SessionSplit, true),
        ("Ctrl+Alt+Shift+F24", HotkeyAction::SessionStopStart, true),
    ]);
    let (tx_a, rx_a) = mpsc::channel::<HotkeyEvent>();
    let (svc_a, _) = HotkeyService::start(&holder, tx_a)
        .unwrap_or_else(|e| panic!("service A should start: {e:?}"));

    let reg_b = registry(&[
        ("Ctrl+Alt+Shift+F21", HotkeyAction::ToggleBuffer, true),
        ("Ctrl+Alt+Shift+F24", HotkeyAction::MuteMic, true),
    ]);
    let (tx_b, rx_b) = mpsc::channel::<HotkeyEvent>();
    let (svc_b, failures) = match HotkeyService::start(&reg_b, tx_b) {
        Ok(v) => v,
        Err(e) => {
            svc_a.stop();
            panic!("service B should start: {e:?}");
        }
    };

    let sent_21 = send_chord(&MODS, VK_F21);
    let owner_event = rx_a.recv_timeout(Duration::from_secs(3));
    let sent_24 = send_chord(&MODS, VK_F24);
    let silent = rx_b.recv_timeout(Duration::from_millis(500));
    svc_b.stop();
    svc_a.stop();

    assert_eq!(failures.len(), 2, "expected two failures, got {failures:?}");
    assert!(
        failures
            .iter()
            .all(|f| f.reason == FailureReason::TakenByAnotherApp),
        "both failures should be TakenByAnotherApp, got {failures:?}"
    );

    assert_eq!(
        owner_event,
        Ok(HotkeyEvent {
            action: HotkeyAction::SessionSplit
        }),
        "holder service A should receive the F21 press (SendInput returned {sent_21})"
    );
    assert!(
        matches!(silent, Err(RecvTimeoutError::Timeout)),
        "service B should get no events for refused chords (F24 sent {sent_24}), got {silent:?}"
    );
}

#[test]
fn start_and_apply_errors_display() {
    fn assert_std_error<E: std::error::Error>(_: &E) {}

    let window = StartError::Window(5);
    assert_std_error(&window);
    assert!(
        window.to_string().contains('5'),
        "Window(5) display should include the code, got {window}"
    );

    let spawn = StartError::Spawn("x".into());
    assert_std_error(&spawn);
    assert!(
        spawn.to_string().contains('x'),
        "Spawn display should include the message, got {spawn}"
    );

    let no_reply = StartError::NoReply;
    assert_std_error(&no_reply);
    assert!(!no_reply.to_string().is_empty(), "NoReply display is empty");

    assert_std_error(&ApplyError::Stopped);
    assert!(
        !ApplyError::Stopped.to_string().is_empty(),
        "ApplyError::Stopped display is empty"
    );
}

#[test]
fn stop_with_two_bindings_joins_promptly() {
    let reg = registry(&[
        ("Ctrl+Alt+Shift+F21", HotkeyAction::SessionSplit, true),
        ("Ctrl+Alt+Shift+F22", HotkeyAction::MuteMic, true),
    ]);
    let (tx, _rx) = mpsc::channel::<HotkeyEvent>();
    let (svc, failures) =
        HotkeyService::start(&reg, tx).unwrap_or_else(|e| panic!("start failed: {e:?}"));
    assert!(
        failures.is_empty(),
        "both chords should be free, got {failures:?}"
    );

    let t = Instant::now();
    svc.stop();
    let elapsed = t.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "stop() with two bindings took {elapsed:?}, expected < 2s"
    );
}
