//! Tests for `clipcheck_ui_lib::closing`.
//!
//! Pure tests cover the decision table, the settings parsing, the status and event
//! predicates. The integration tests spawn the built `clipcheck-ui` binary with `--daemon`
//! (same pattern as `ipc_client_tests.rs`) and check the settings cache against the real
//! daemon. The daemon's single-instance mutex is session-global, so every test takes
//! `TEST_LOCK`. Each daemon is wrapped in a guard that shuts it down and kills it on drop.

mod common;

use std::thread;
use std::time::Duration;

use clipcheck_ui_lib::closing::{
    decide_close, is_app_exit_event, is_config_changed_event, new_cache, read_cache,
    refresh_settings, session_active_in_status, session_active_now, CloseAction, CloseBehavior,
    CloseSettings,
};
use clipcheck_ui_lib::ipc::ClientError;
use common::{
    body_type, conn_log_contains, start_client, unique_pipe, wait_connected, wait_until, Daemon,
    TEST_LOCK,
};
use serde_json::{json, Value};

/// Upper bound for waiting on a `config_changed` event.
const EVENT_WAIT: Duration = Duration::from_secs(10);

/// Sends a `patch_config` with `patch` (not a dry run) and returns the reply body.
fn patch(client: &clipcheck_ui_lib::ipc::DaemonClient, patch: Value) -> Value {
    let cmd = json!({ "type": "patch_config", "data": { "patch": patch, "dry_run": false } });
    client
        .request(cmd, Duration::from_secs(30))
        .expect("patch_config request failed")
}

// ---------------------------------------------------------------------------
// Pure tests (no processes)
// ---------------------------------------------------------------------------

#[test]
fn decide_close_full_table() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use CloseAction::{AskConfirmThenExit, ExitUiOnly, StopDaemonAndExit};
    use CloseBehavior::{Exit, MinimizeToTray};

    // (behavior, confirm_enabled, session_active, daemon_connected) -> expected
    let table: [((CloseBehavior, bool, bool, bool), CloseAction); 16] = [
        // Daemon not connected (Unknown state): minimize_to_tray still exits UI only.
        ((MinimizeToTray, true, true, false), ExitUiOnly),
        ((MinimizeToTray, true, false, false), ExitUiOnly),
        ((MinimizeToTray, false, true, false), ExitUiOnly),
        ((MinimizeToTray, false, false, false), ExitUiOnly),
        // Daemon not connected with exit: Unknown always asks, whatever confirm says.
        ((Exit, true, true, false), AskConfirmThenExit),
        ((Exit, true, false, false), AskConfirmThenExit),
        ((Exit, false, true, false), AskConfirmThenExit),
        ((Exit, false, false, false), AskConfirmThenExit),
        // Daemon connected, minimize_to_tray: always exit UI only.
        ((MinimizeToTray, true, true, true), ExitUiOnly),
        ((MinimizeToTray, true, false, true), ExitUiOnly),
        ((MinimizeToTray, false, true, true), ExitUiOnly),
        ((MinimizeToTray, false, false, true), ExitUiOnly),
        // Daemon connected, exit: confirm only when a session is active and confirm is on.
        ((Exit, true, true, true), AskConfirmThenExit),
        ((Exit, true, false, true), StopDaemonAndExit),
        ((Exit, false, true, true), StopDaemonAndExit),
        ((Exit, false, false, true), StopDaemonAndExit),
    ];

    for ((behavior, confirm, session, connected), expected) in table {
        let got = decide_close(behavior, confirm, session, connected);
        assert_eq!(
            got, expected,
            "decide_close({behavior:?}, confirm={confirm}, session={session}, connected={connected})"
        );
    }
}

#[test]
fn close_settings_defaults() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = CloseSettings::default();
    assert_eq!(d.behavior, CloseBehavior::MinimizeToTray);
    assert!(d.confirm_enabled);
    assert_eq!(CloseBehavior::default(), CloseBehavior::MinimizeToTray);
}

#[test]
fn from_config_parses_exit_and_confirm() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = json!({"general": {"close_behavior": "exit", "confirm_exit_while_recording": false}});
    assert_eq!(
        CloseSettings::from_config(&cfg),
        CloseSettings {
            behavior: CloseBehavior::Exit,
            confirm_enabled: false,
        }
    );
}

#[test]
fn from_config_minimize_to_tray_string() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = json!({"general": {"close_behavior": "minimize_to_tray"}});
    assert_eq!(
        CloseSettings::from_config(&cfg),
        CloseSettings {
            behavior: CloseBehavior::MinimizeToTray,
            confirm_enabled: true,
        }
    );
}

#[test]
fn from_config_unknown_string_falls_back() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = json!({"general": {"close_behavior": "bogus"}});
    assert_eq!(CloseSettings::from_config(&cfg), CloseSettings::default());
}

#[test]
fn from_config_missing_general_uses_defaults() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        CloseSettings::from_config(&json!({"other": {"close_behavior": "exit"}})),
        CloseSettings::default()
    );
    assert_eq!(
        CloseSettings::from_config(&json!({})),
        CloseSettings::default()
    );
}

#[test]
fn from_config_wrong_types_use_defaults() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = json!({"general": {"close_behavior": 5, "confirm_exit_while_recording": "no"}});
    assert_eq!(CloseSettings::from_config(&cfg), CloseSettings::default());
}

#[test]
fn from_config_reply_parses_config_body() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let body = json!({
        "type": "config",
        "data": {
            "config": {"general": {"close_behavior": "exit", "confirm_exit_while_recording": false}},
            "config_version": 1,
            "path": "x"
        }
    });
    assert_eq!(
        CloseSettings::from_config_reply(&body),
        Some(CloseSettings {
            behavior: CloseBehavior::Exit,
            confirm_enabled: false,
        })
    );
}

#[test]
fn from_config_reply_rejects_non_config_bodies() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let err = json!({"type": "error", "data": {"code": "bad_request", "message": "x"}});
    assert_eq!(CloseSettings::from_config_reply(&err), None);
    // A config body without data.config is not usable either.
    assert_eq!(
        CloseSettings::from_config_reply(&json!({"type": "config", "data": {}})),
        None
    );
}

#[test]
fn session_active_in_status_cases() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!session_active_in_status(
        &json!({"type": "status", "data": {"session": null}})
    ));
    assert!(!session_active_in_status(
        &json!({"type": "status", "data": {}})
    ));
    assert!(!session_active_in_status(
        &json!({"type": "error", "data": {"message": "x"}})
    ));
    assert!(session_active_in_status(
        &json!({"type": "status", "data": {"session": {"session_id": "s1"}}})
    ));
    assert!(!session_active_in_status(&json!({})));
}

#[test]
fn is_app_exit_event_cases() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(is_app_exit_event(&json!({"type": "app_exit", "seq": 7})));
    assert!(!is_app_exit_event(&json!({"type": "config_changed"})));
    assert!(!is_app_exit_event(&json!({"type": "shutting_down"})));
    assert!(!is_app_exit_event(&json!({})));
    assert!(!is_app_exit_event(&json!({"type": 5})));
}

#[test]
fn is_config_changed_event_cases() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(is_config_changed_event(&json!({
        "type": "config_changed", "apply_class": "live", "restart_required": false, "seq": 2
    })));
    assert!(!is_config_changed_event(&json!({"type": "app_exit"})));
}

#[test]
fn new_cache_starts_with_defaults() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(read_cache(&new_cache()), CloseSettings::default());
}

// ---------------------------------------------------------------------------
// Integration tests (real daemon)
// ---------------------------------------------------------------------------

#[test]
fn refresh_on_fresh_daemon_returns_defaults_and_fills_cache() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-closing-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    // Start from a non-default cache so the refresh must overwrite it.
    let cache = new_cache();
    *cache.write().expect("cache lock") = CloseSettings {
        behavior: CloseBehavior::Exit,
        confirm_enabled: false,
    };

    let got = refresh_settings(&client, &cache).expect("refresh_settings failed");
    assert_eq!(got, CloseSettings::default(), "returned settings");
    assert_eq!(
        read_cache(&cache),
        CloseSettings::default(),
        "cache after refresh"
    );

    client.stop();
}

#[test]
fn session_inactive_with_no_session() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-closing-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    assert!(!session_active_now(&client), "no session was started");

    client.stop();
}

#[test]
fn config_change_event_refreshes_cache_and_app_exit_is_not_raised() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-closing-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let cache = new_cache();
    refresh_settings(&client, &cache).expect("initial refresh failed");
    assert_eq!(read_cache(&cache), CloseSettings::default());

    let reply = patch(&client, json!({"general": {"close_behavior": "exit"}}));
    assert_eq!(
        body_type(&reply),
        "config_applied",
        "patch reply was {reply}"
    );

    // Wait, bounded, for config_changed to show up among the pushed events.
    let got_change = wait_until(EVENT_WAIT, || {
        events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(is_config_changed_event)
    });
    let seen: Vec<Value> = events.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(
        got_change,
        "no config_changed event within {EVENT_WAIT:?}; events: {seen:?}"
    );
    assert!(
        !seen.iter().any(is_app_exit_event),
        "app_exit was delivered for a config change: {seen:?}"
    );

    let refreshed = refresh_settings(&client, &cache).expect("refresh after change failed");
    assert_eq!(refreshed.behavior, CloseBehavior::Exit, "returned behavior");
    assert!(
        refreshed.confirm_enabled,
        "confirm_enabled should still be true"
    );
    assert_eq!(
        read_cache(&cache).behavior,
        CloseBehavior::Exit,
        "cache behavior"
    );
    assert!(read_cache(&cache).confirm_enabled, "cache confirm_enabled");

    // Restore for the next checks in this daemon.
    let restore = patch(
        &client,
        json!({"general": {"close_behavior": "minimize_to_tray"}}),
    );
    assert_eq!(
        body_type(&restore),
        "config_applied",
        "restore reply was {restore}"
    );
    let restored = refresh_settings(&client, &cache).expect("refresh after restore failed");
    assert_eq!(
        restored.behavior,
        CloseBehavior::MinimizeToTray,
        "restored behavior"
    );

    client.stop();
}

#[test]
fn confirm_flag_patch_is_read_back() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe("clipcheck-ui-closing-test");
    let mut d = Daemon::spawn(dir.path(), &pipe);
    let (client, _events, conns) = start_client(&pipe);
    wait_connected(&mut d, &conns);

    let reply = patch(
        &client,
        json!({"general": {"confirm_exit_while_recording": false}}),
    );
    assert_eq!(
        body_type(&reply),
        "config_applied",
        "patch reply was {reply}"
    );

    let cache = new_cache();
    let got = refresh_settings(&client, &cache).expect("refresh failed");
    assert!(
        !got.confirm_enabled,
        "confirm_enabled should be false after patch"
    );
    assert_eq!(got.behavior, CloseBehavior::MinimizeToTray);
    assert!(!read_cache(&cache).confirm_enabled);

    // Restore the default.
    let restore = patch(
        &client,
        json!({"general": {"confirm_exit_while_recording": true}}),
    );
    assert_eq!(
        body_type(&restore),
        "config_applied",
        "restore reply was {restore}"
    );

    client.stop();
}

#[test]
fn refresh_without_daemon_errors_and_leaves_cache_unchanged() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pipe = unique_pipe("clipcheck-ui-closing-test"); // no daemon is started on this pipe
    let (client, _events, conns) = start_client(&pipe);
    // Negative check: the client retries in the background, and nothing should connect.
    // The wait is a fixed, bounded settle time, because "no connection" cannot be polled
    // for. The real assertions are the Disconnected result and the unchanged cache below.
    thread::sleep(Duration::from_millis(300));
    assert!(
        !conn_log_contains(&conns, true),
        "connected to a pipe with no daemon"
    );

    let cache = new_cache();
    *cache.write().expect("cache lock") = CloseSettings {
        behavior: CloseBehavior::Exit,
        confirm_enabled: true,
    };

    let res = refresh_settings(&client, &cache);
    assert_eq!(
        res,
        Err(ClientError::Disconnected),
        "refresh without daemon"
    );
    assert_eq!(
        read_cache(&cache),
        CloseSettings {
            behavior: CloseBehavior::Exit,
            confirm_enabled: true,
        },
        "cache changed after a failed refresh"
    );
    assert!(
        !session_active_now(&client),
        "session reported active without a daemon"
    );

    client.stop();
}
