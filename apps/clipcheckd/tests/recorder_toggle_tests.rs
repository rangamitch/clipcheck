//! Tests for the recorder on/off switch (`Service::set_recorder_enabled`), driven through the
//! in-process `Service` API with no daemon process and no named pipe.
//!
//! Like `issues_b12_service_tests.rs`, the service runs in degraded mode (`no_encoders: true`),
//! so no recorder, capture or game is involved. The state file lives in a temp data dir, and
//! the config keeps every output and hotkey away from the user's recordings and keys.
//!
//! Windows only.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_ipc::proto::{ErrorCode, RecState, StatusDto};
use clipcheck_library::Library;
use clipcheckd::events::EventBus;
use clipcheckd::service::{Service, ServiceOptions};

const STATE_FILE: &str = "recorder-state.json";
const OFF: &str = "recorder is turned off";

/// Config that cannot detect a game, write outside `dir`, or register a global hotkey.
fn isolated_config(dir: &Path) -> Config {
    let mut config = Config::default();
    config.storage.root = dir.join("recordings");
    config.advanced.log_dir = dir.join("logs");
    config.replay.location = dir.join("replay");
    config.detection.mode = DetectionMode::Allowlist;
    config.detection.allow = Vec::new();
    let mut hotkeys = Hotkeys::default();
    hotkeys.clips.clear();
    hotkeys.session.split = None;
    hotkeys.session.stop_start = None;
    hotkeys.session.toggle_buffer = None;
    hotkeys.session.mute_mic = None;
    config.hotkeys = hotkeys;
    config
}

/// Starts a degraded service on `dir`. Calling it twice with one `dir` simulates a restart.
fn start_in(dir: &Path) -> Service {
    let library = Library::open(dir.join("library.sqlite")).expect("open library");
    let bus = Arc::new(EventBus::new());
    Service::start(ServiceOptions {
        config: isolated_config(dir),
        data_dir: dir.to_path_buf(),
        library: Arc::new(Mutex::new(library)),
        bus,
        no_encoders: true,
    })
    .expect("start service")
}

/// Polls status until the encoder probe has finished and the service reports `Degraded`.
fn wait_degraded(service: &Service) -> StatusDto {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = service.status().expect("status answers while degraded");
        if matches!(status.state, RecState::Degraded) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "service never reached Degraded; last state {:?}",
            status.state
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_state(dir: &Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).expect("state file exists");
    serde_json::from_str(&text).expect("state file is JSON")
}

#[test]
fn recorder_is_on_by_default_without_a_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    let status = wait_degraded(&service);
    assert!(
        status.recorder_enabled,
        "absent state file means the recorder is on"
    );
    assert!(
        !dir.path().join(STATE_FILE).exists(),
        "starting alone writes no state file"
    );
    service.stop();
}

#[test]
fn turning_off_updates_status_and_persists_the_choice() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);

    service.set_recorder_enabled(false).expect("turn off");
    let status = service.status().expect("status");
    assert!(!status.recorder_enabled);
    assert_eq!(
        read_state(dir.path()),
        serde_json::json!({ "enabled": false })
    );
    assert!(
        !dir.path().join(format!("{STATE_FILE}.tmp")).exists(),
        "the temporary file is renamed away"
    );
    service.stop();
}

#[test]
fn turning_back_on_updates_status_and_persists_the_choice() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);

    service.set_recorder_enabled(false).expect("turn off");
    service.set_recorder_enabled(true).expect("turn on");
    let status = service.status().expect("status");
    assert!(status.recorder_enabled);
    assert_eq!(
        read_state(dir.path()),
        serde_json::json!({ "enabled": true })
    );
    service.stop();
}

#[test]
fn setting_the_same_choice_twice_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);
    service.set_recorder_enabled(false).expect("first off");
    service.set_recorder_enabled(false).expect("second off");
    assert!(!service.status().unwrap().recorder_enabled);
    service.stop();
}

#[test]
fn off_choice_survives_a_daemon_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let service = start_in(dir.path());
        wait_degraded(&service);
        service.set_recorder_enabled(false).expect("turn off");
        service.stop();
    }
    let service = start_in(dir.path());
    let status = wait_degraded(&service);
    assert!(
        !status.recorder_enabled,
        "the recorder stays off after a restart"
    );
    service.stop();
}

#[test]
fn unreadable_state_file_leaves_the_recorder_on() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(STATE_FILE), "this is not json").unwrap();
    let service = start_in(dir.path());
    let status = wait_degraded(&service);
    assert!(
        status.recorder_enabled,
        "a damaged state file must not switch the recorder off"
    );
    service.stop();
}

#[test]
fn recorder_commands_fail_with_a_clear_error_while_off() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);
    service.set_recorder_enabled(false).expect("turn off");

    let start = service
        .start_session()
        .expect_err("start must fail while off");
    assert_eq!(start.code, ErrorCode::InvalidState);
    assert_eq!(start.message, OFF);

    let stop = service
        .stop_session()
        .expect_err("stop must fail while off");
    assert_eq!(stop.code, ErrorCode::InvalidState);
    assert_eq!(stop.message, OFF);

    let split = service
        .split_session()
        .expect_err("split must fail while off");
    assert_eq!(split.code, ErrorCode::InvalidState);
    assert_eq!(split.message, OFF);

    let clip = service.clip(10).expect_err("clip must fail while off");
    assert_eq!(clip.code, ErrorCode::InvalidState);
    assert_eq!(clip.message, OFF);

    service.stop();
}

#[test]
fn clip_length_is_still_checked_first_while_off() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);
    service.set_recorder_enabled(false).expect("turn off");
    let err = service.clip(0).expect_err("zero seconds is never valid");
    assert_eq!(err.code, ErrorCode::BadRequest);
    service.stop();
}

#[test]
fn recorder_commands_do_not_report_off_while_on() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);
    // Degraded mode has no recorder, so the commands fail for that reason, not the switch.
    let err = service
        .start_session()
        .expect_err("no recorder in degraded mode");
    assert_ne!(err.message, OFF);
    service.set_recorder_enabled(true).expect("turn on");
    let err = service.start_session().expect_err("still no recorder");
    assert_ne!(err.message, OFF);
    service.stop();
}

#[test]
fn turning_off_while_degraded_reports_no_game_and_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);
    service.set_recorder_enabled(false).expect("turn off");
    let status = service.status().expect("status");
    assert!(status.game.is_none());
    assert!(status.session.is_none());
    assert!(!status.recorder_enabled);
    service.stop();
}
