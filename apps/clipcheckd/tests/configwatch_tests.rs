//! Regression tests for issue #191: hand edits to `config.toml` must be picked up by the
//! config watcher without a restart, invalid files must be rejected, and writes that do not
//! change the config in effect must not announce anything.
//!
//! Driven through the public in-process `Service` and `ConfigWatcher` API. The service runs in
//! degraded mode (`no_encoders: true`) with storage, logs and replay in a temp dir, the detector
//! allowlist empty and the global hotkeys removed, so no game, recording or user app is touched.
//!
//! Every wait is bounded: event waits use `recv_timeout` with a deadline and service calls have
//! their own request timeout.
//!
//! Windows only.

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_ipc::proto::{Event, Topic};
use clipcheck_library::Library;
use clipcheckd::configwatch::ConfigWatcher;
use clipcheckd::events::{Delivery, EventBus, Subscription};
use clipcheckd::service::{ConfigOutcome, Service, ServiceOptions};
use serde_json::json;

/// Longest time a valid edit may take to reach a `ConfigChanged` event (poll is 300 ms,
/// stability needs one more poll; 3 s leaves a wide margin).
const PICKUP: Duration = Duration::from_secs(3);
/// How long to watch for an event that must not arrive.
const QUIET: Duration = Duration::from_millis(1500);

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

/// A running service, the config file it reads, a watcher on that file, and a subscriber
/// that receives settings events. Field order matters: the watcher is dropped (stopped)
/// before the service.
struct Watched {
    watch: ConfigWatcher,
    _sub: Subscription,
    rx: Receiver<Delivery>,
    path: PathBuf,
    /// The config in effect when the watcher started; the file on disk matches it.
    base: Config,
    h: Harness,
}

/// Writes the config in effect to the config file, starts the watcher and subscribes to
/// settings events. Waits one second so the watcher's first signature is the written file.
fn setup() -> Watched {
    let h = start_degraded();
    let view = h.service.config().expect("config view");
    let path = PathBuf::from(&view.path);
    let base: Config =
        serde_json::from_value(view.config.clone()).expect("config view is a full Config");
    write_config(&path, &base);
    thread::sleep(Duration::from_millis(1000));

    let (sub, rx) = h.bus.subscribe();
    sub.set_topics(&[Topic::Settings]);
    let watch =
        ConfigWatcher::start(path.clone(), h.service.clone()).expect("start config watcher");
    Watched {
        watch,
        _sub: sub,
        rx,
        path,
        base,
        h,
    }
}

fn write_config(path: &Path, config: &Config) {
    let text = config.to_toml_string().expect("serialize config");
    fs::write(path, text).expect("write config.toml");
}

/// The `general.start_minimized` value the service has in effect.
fn start_minimized(service: &Service) -> bool {
    service.config().expect("config view").config["general"]["start_minimized"]
        .as_bool()
        .expect("start_minimized is a bool")
}

/// Waits up to `within` for a `ConfigChanged` event. Returns its `(apply_class,
/// restart_required)` fields, or `None` when none arrives in time.
fn next_config_changed(rx: &Receiver<Delivery>, within: Duration) -> Option<(String, bool)> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let delivery = rx.recv_timeout(left).ok()?;
        if let Event::ConfigChanged {
            apply_class,
            restart_required,
        } = &*delivery.event
        {
            return Some((apply_class.clone(), *restart_required));
        }
    }
}

/// Counts `ConfigChanged` events that are queued or arrive within `window`.
fn count_config_changed(rx: &Receiver<Delivery>, window: Duration) -> usize {
    let deadline = Instant::now() + window;
    let mut count = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(delivery) => {
                if matches!(&*delivery.event, Event::ConfigChanged { .. }) {
                    count += 1;
                }
            }
            Err(_) => break,
        }
    }
    count
}

/// Test 1: a valid hand edit is picked up, announced, and applied.
#[test]
fn issue191_valid_hand_edit_is_picked_up() {
    let w = setup();
    let before = start_minimized(&w.h.service);
    assert_eq!(before, w.base.general.start_minimized);

    let mut edited = w.base.clone();
    edited.general.start_minimized = !before;
    write_config(&w.path, &edited);

    let event = next_config_changed(&w.rx, PICKUP);
    assert!(
        event.is_some(),
        "a valid hand edit must publish ConfigChanged within {PICKUP:?}"
    );
    assert_eq!(
        start_minimized(&w.h.service),
        !before,
        "the edited value must be in effect"
    );
}

/// Test 2: an invalid edit is rejected and keeps the current settings; the watcher then
/// recovers when a valid file is written.
#[test]
fn issue191_invalid_edit_is_rejected_then_watcher_recovers() {
    let w = setup();
    let before = start_minimized(&w.h.service);

    fs::write(&w.path, "this is = = not toml\n").expect("write invalid config");
    let published = count_config_changed(&w.rx, QUIET);
    assert_eq!(
        published, 0,
        "an invalid file must publish no ConfigChanged"
    );
    assert_eq!(
        start_minimized(&w.h.service),
        before,
        "an invalid file must leave the current settings in effect"
    );

    let mut valid = w.base.clone();
    valid.general.start_minimized = !before;
    write_config(&w.path, &valid);
    let event = next_config_changed(&w.rx, PICKUP);
    assert!(
        event.is_some(),
        "after an invalid file, a valid file must be picked up within {PICKUP:?}"
    );
    assert_eq!(
        start_minimized(&w.h.service),
        !before,
        "the watcher must recover and apply the new valid file"
    );
}

/// Test 3: rewriting the file with identical content (new mtime, same bytes) publishes nothing.
#[test]
fn issue191_identical_rewrite_publishes_nothing() {
    let w = setup();
    let text = fs::read(&w.path).expect("read config");
    let mtime_before = fs::metadata(&w.path)
        .expect("metadata")
        .modified()
        .expect("mtime");

    // Make sure the rewrite lands on a later timestamp, so the watcher really sees a change.
    thread::sleep(Duration::from_millis(50));
    fs::write(&w.path, &text).expect("rewrite identical config");
    let mtime_after = fs::metadata(&w.path)
        .expect("metadata")
        .modified()
        .expect("mtime");
    assert_ne!(
        mtime_before, mtime_after,
        "the test needs a new mtime to prove the watcher ignores a no-op write"
    );

    let published = count_config_changed(&w.rx, QUIET);
    assert_eq!(
        published, 0,
        "an identical rewrite must publish no ConfigChanged"
    );
}

/// Test 4: a change made through the service's own patch path yields exactly one
/// `ConfigChanged`. The watcher then sees the file it wrote and must stay silent.
#[test]
fn issue191_own_patch_yields_exactly_one_event() {
    let w = setup();
    let before = start_minimized(&w.h.service);

    let outcome =
        w.h.service
            .patch_config(json!({"general": {"start_minimized": !before}}), false)
            .expect("patch_config");
    assert!(
        matches!(outcome, ConfigOutcome::Applied { .. }),
        "patch must be applied, got {outcome:?}"
    );

    let count = count_config_changed(&w.rx, QUIET);
    assert_eq!(
        count, 1,
        "a patch must publish exactly one ConfigChanged (the watcher must not add a second)"
    );
    assert_eq!(start_minimized(&w.h.service), !before);
}

/// Test 5: after `stop()` returns, the watcher no longer picks up edits.
#[test]
fn issue191_edits_after_stop_are_ignored() {
    let w = setup();
    let before = start_minimized(&w.h.service);

    w.watch.stop();

    let mut edited = w.base.clone();
    edited.general.start_minimized = !before;
    write_config(&w.path, &edited);

    let published = count_config_changed(&w.rx, QUIET);
    assert_eq!(published, 0, "a stopped watcher must not reload the file");
    assert_eq!(
        start_minimized(&w.h.service),
        before,
        "a stopped watcher must not change the settings in effect"
    );
}
