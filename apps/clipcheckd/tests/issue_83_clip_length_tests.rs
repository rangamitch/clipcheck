//! Regression tests for issue #83: the clip-length rule.
//!
//! The rule lives only in `Core::clip`, behind the public `Service::clip`. A length of 0 or
//! above `MAX_CLIP_SECONDS` is refused with `BadRequest` before any recorder is touched. A
//! valid length must pass validation; in degraded mode (no recorder) it then fails for a
//! reason other than `BadRequest`.
//!
//! The service runs in degraded mode (`no_encoders: true`), so no recorder, capture or game is
//! involved. Storage, logs and the library live in a temp dir, the detector allowlist is empty,
//! and the global hotkeys are removed, so nothing touches the user's recordings or hotkeys.
//!
//! Windows only.

#![cfg(windows)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_ipc::proto::{ErrorCode, RecState, StatusDto};
use clipcheck_library::Library;
use clipcheckd::events::EventBus;
use clipcheckd::service::{Service, ServiceOptions, MAX_CLIP_SECONDS};

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

/// Issue #83: a length of 0 is outside `1..=MAX_CLIP_SECONDS` and must be refused as
/// `BadRequest` with the range in the message.
#[test]
fn issue83_zero_seconds_is_rejected_as_bad_request() {
    let h = start_degraded();
    wait_degraded(&h.service);

    let err = h
        .service
        .clip(0)
        .expect_err("a zero-second clip must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains(&format!("1..={}", MAX_CLIP_SECONDS)),
        "rejection must state the valid range, got {:?}",
        err.message
    );
}

/// Issue #83: a length of 1 is the lower bound of the valid range. It must pass validation,
/// so in degraded mode the failure is not `BadRequest`.
#[test]
fn issue83_one_second_passes_validation() {
    let h = start_degraded();
    wait_degraded(&h.service);

    let err = h
        .service
        .clip(1)
        .expect_err("degraded mode has no recorder, so clip must fail");
    assert_ne!(
        err.code,
        ErrorCode::BadRequest,
        "a valid length must not be rejected as BadRequest: {:?}",
        err.message
    );
}

/// Issue #83: 3600 seconds is the upper bound of the valid range and must pass validation.
#[test]
fn issue83_max_seconds_passes_validation() {
    let h = start_degraded();
    wait_degraded(&h.service);

    let err = h
        .service
        .clip(MAX_CLIP_SECONDS)
        .expect_err("degraded mode has no recorder, so clip must fail");
    assert_ne!(
        err.code,
        ErrorCode::BadRequest,
        "a length of MAX_CLIP_SECONDS must not be rejected as BadRequest: {:?}",
        err.message
    );
}

/// Issue #83: one second above the upper bound must be refused as `BadRequest`.
#[test]
fn issue83_above_max_seconds_is_rejected_as_bad_request() {
    let h = start_degraded();
    wait_degraded(&h.service);

    let err = h
        .service
        .clip(MAX_CLIP_SECONDS + 1)
        .expect_err("a clip above MAX_CLIP_SECONDS must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains(&format!("1..={}", MAX_CLIP_SECONDS)),
        "rejection must state the valid range, got {:?}",
        err.message
    );
}

/// Issue #83: the largest `u32` must be refused as `BadRequest`.
#[test]
fn issue83_u32_max_is_rejected_as_bad_request() {
    let h = start_degraded();
    wait_degraded(&h.service);

    let err = h
        .service
        .clip(u32::MAX)
        .expect_err("u32::MAX seconds must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains(&format!("1..={}", MAX_CLIP_SECONDS)),
        "rejection must state the valid range, got {:?}",
        err.message
    );
}
