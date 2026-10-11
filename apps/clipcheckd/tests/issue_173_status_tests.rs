//! Tests for issue #173: `StatusDto.hotkeys_down` reported by the in-process `Service`.
//!
//! The service runs in degraded mode (`no_encoders: true`) with every global hotkey removed from
//! the config, so the hotkey service has nothing to register and must not report itself down.
//! Storage, logs and the library live in a temp dir. Nothing touches the user's recordings or
//! hotkeys.
//!
//! Windows only.

#![cfg(windows)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_ipc::proto::{RecState, StatusDto};
use clipcheck_library::Library;
use clipcheckd::events::EventBus;
use clipcheckd::service::{Service, ServiceOptions};

/// A service running in degraded mode with everything outside `dir` disabled.
struct Harness {
    service: Service,
    _bus: Arc<EventBus>,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.service.stop();
    }
}

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

fn start_degraded() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path().join("library.sqlite")).expect("open library");
    let bus = Arc::new(EventBus::new());
    let service = Service::start(ServiceOptions {
        config: isolated_config(dir.path()),
        data_dir: dir.path().to_path_buf(),
        library: Arc::new(Mutex::new(library)),
        bus: Arc::clone(&bus),
        no_encoders: true,
    })
    .expect("start service");
    Harness {
        service,
        _bus: bus,
        _dir: dir,
    }
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

/// Issue #173: with no global hotkeys configured, the hotkey service has nothing to register
/// and must not report itself down.
#[test]
fn issue_173_status_reports_hotkeys_up_when_no_hotkeys_configured() {
    let h = start_degraded();
    let status = wait_degraded(&h.service);
    assert!(
        !status.hotkeys_down,
        "no hotkeys are configured, so hotkeys_down must be false"
    );
}
