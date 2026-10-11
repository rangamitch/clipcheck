//! Tests for hotkey suspension (`Service::set_hotkeys_suspended`), driven through the in-process
//! `Service` API with no daemon process and no named pipe.
//!
//! Like `recorder_toggle_tests.rs`, the service runs in degraded mode (`no_encoders: true`), so no
//! recorder, capture or game is involved. The config registers no global hotkey, and the data
//! dir is a temp dir, so the user's keys and recordings are never touched.
//!
//! Windows only.

#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_ipc::proto::{RecState, StatusDto};
use clipcheck_library::Library;
use clipcheckd::events::EventBus;
use clipcheckd::service::{Service, ServiceOptions};

/// Config that cannot detect a game, write outside `dir`, or register a global hotkey.
fn isolated_config(dir: &std::path::Path) -> Config {
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

/// Starts a degraded service on `dir`.
fn start_in(dir: &std::path::Path) -> Service {
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

/// Polls status until `hotkeys_suspended` equals `want`, or fails after `within`.
fn wait_suspended(service: &Service, want: bool, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        if service.status().expect("status").hotkeys_suspended == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "hotkeys_suspended never became {want} within {within:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn hotkeys_are_not_suspended_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    let status = wait_degraded(&service);
    assert!(
        !status.hotkeys_suspended,
        "a fresh service is not suspended"
    );
    service.stop();
}

#[test]
fn suspending_sets_the_status_flag() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);

    service
        .set_hotkeys_suspended_for(true, Duration::from_secs(60))
        .expect("suspend");
    assert!(service.status().unwrap().hotkeys_suspended);
    service.stop();
}

#[test]
fn suspension_resumes_by_itself_after_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);

    service
        .set_hotkeys_suspended_for(true, Duration::from_millis(300))
        .expect("suspend");
    assert!(service.status().unwrap().hotkeys_suspended);
    wait_suspended(&service, false, Duration::from_secs(5));
    service.stop();
}

#[test]
fn renewing_before_the_timeout_keeps_the_suspension() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);

    let window = Duration::from_millis(400);
    service
        .set_hotkeys_suspended_for(true, window)
        .expect("suspend");
    // Renew every 150 ms, so the window never runs out, for about 900 ms in total.
    let end = Instant::now() + Duration::from_millis(900);
    while Instant::now() < end {
        thread::sleep(Duration::from_millis(150));
        service
            .set_hotkeys_suspended_for(true, window)
            .expect("renew");
        assert!(
            service.status().unwrap().hotkeys_suspended,
            "a renewed suspension must still be in effect"
        );
    }
    // Stop renewing: it must now run out.
    wait_suspended(&service, false, Duration::from_secs(5));
    service.stop();
}

#[test]
fn explicit_resume_clears_the_flag_before_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let service = start_in(dir.path());
    wait_degraded(&service);

    service
        .set_hotkeys_suspended_for(true, Duration::from_secs(60))
        .expect("suspend");
    assert!(service.status().unwrap().hotkeys_suspended);

    service.set_hotkeys_suspended(false).expect("resume");
    assert!(!service.status().unwrap().hotkeys_suspended);
    service.stop();
}
