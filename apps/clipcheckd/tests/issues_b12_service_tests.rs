//! Regression tests for the `service.rs` issues in batch B12, driven through the public
//! in-process `Service` API (no daemon process, no named pipe).
//!
//! The service runs in degraded mode (`no_encoders: true`), so no recorder, capture or game is
//! involved. Storage, logs and the library live in a temp dir, the detector allowlist is empty,
//! and the global hotkeys are removed, so nothing touches the user's recordings or hotkeys.
//!
//! Issues whose behaviour only exists while a recorder or a detected game is running are not
//! tested here, because the service exposes no seam for them. Those are listed in the report.
//!
//! Windows only.

#![cfg(windows)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_ipc::proto::{ErrorCode, RecState, StatusDto, Topic};
use clipcheck_library::Library;
use clipcheckd::events::{Delivery, EventBus};
use clipcheckd::service::{Service, ServiceOptions};

/// A service running in degraded mode with everything outside `dir` disabled.
struct Harness {
    service: Service,
    bus: Arc<EventBus>,
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
        bus,
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

/// Issue #55: status fields that have no source must read as the values the service really
/// has. With no recorder there is no buffer, so the byte estimate is 0, and no mute or pause
/// state is reported as active.
#[test]
fn issue_55_status_without_recorder_reports_no_buffer_or_mute() {
    let h = start_degraded();
    let status = wait_degraded(&h.service);
    assert_eq!(
        status.buffer_bytes, 0,
        "no recorder means no buffered bytes"
    );
    assert_eq!(status.buffer_seconds, 0);
    assert!(
        !status.mic_muted,
        "no mute feature exists, so mic_muted must be false"
    );
    assert!(!status.detection_paused);
    assert!(status.game.is_none());
    assert!(status.session.is_none());
    assert!(
        status
            .degraded_reason
            .as_deref()
            .is_some_and(|r| !r.is_empty()),
        "degraded state must carry a reason"
    );
}

/// Issues #160 and #98: a rejected split must not announce a new part. The split is refused
/// before anything is published, so no `SessionPartStarted` or `SessionStarted` reaches
/// subscribers.
#[test]
fn issue_160_rejected_split_publishes_no_session_part() {
    let h = start_degraded();
    wait_degraded(&h.service);
    let (sub, rx) = h.bus.subscribe();
    sub.set_topics(&[Topic::Sessions]);

    let err = h
        .service
        .split_session()
        .expect_err("split must fail without a recorder");
    assert_eq!(err.code, ErrorCode::WorkerUnavailable);
    assert!(!err.message.is_empty(), "error must explain why");

    let delivered: Vec<Delivery> = rx.try_iter().collect();
    assert!(
        delivered.is_empty(),
        "a rejected split published {} session event(s)",
        delivered.len()
    );
}

/// Issue #160 (companion): a rejected start must not publish session events either, and
/// stop must be refused the same way, so no session state is announced without a recorder.
#[test]
fn issue_160_rejected_start_and_stop_publish_no_session_events() {
    let h = start_degraded();
    wait_degraded(&h.service);
    let (sub, rx) = h.bus.subscribe();
    sub.set_topics(&[Topic::Sessions]);

    let start = h.service.start_session().expect_err("start must fail");
    assert_eq!(start.code, ErrorCode::WorkerUnavailable);
    let stop = h.service.stop_session().expect_err("stop must fail");
    assert_eq!(stop.code, ErrorCode::WorkerUnavailable);
    assert!(
        rx.try_iter().next().is_none(),
        "session events were published"
    );
}
