//! Issue #195: when a session part fails to finalize, `Recorder::detach` must keep
//! that part's staged segments. Everything else in the run directory is removed.
//!
//! Needs Windows Graphics Capture and a real window. When either is missing the test
//! prints `SKIPPED: <test>: <reason>` and returns. Set `CLIPCHECK_REQUIRE_CAPTURE=1` to
//! make every skip a failure instead.
//! Window tests take the shared `common::serial()` lock.

#![cfg(windows)]

#[macro_use]
mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_engine::recorder::{Recorder, RecorderError, RecorderEvent};

use common::make_config;
use common::serial;
use common::win_window::TestWindow;

/// Fill colour of the test window (COLORREF 0x00BBGGRR, dark orange-brown).
const WINDOW_FILL: u32 = 0x0030_6090;

/// Run directories (`run-*`) under `work`.
fn run_dirs(work: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(work)
        .expect("read work_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("run-"))
                .unwrap_or(false)
        })
        .collect()
}

/// Staged session directories (`stage-s*`) directly under `run_dir`.
fn stage_dirs(run_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(run_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("stage-s"))
                            .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Number of regular files directly inside `dir`.
fn file_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .count()
        })
        .unwrap_or(0)
}

/// Drains every event that is already queued.
fn drain(rx: &Receiver<RecorderEvent>) -> Vec<RecorderEvent> {
    rx.try_iter().collect()
}

// ---------------------------------------------------------------------------
// test
// ---------------------------------------------------------------------------

/// Session part fails to finalize because `sessions` is a regular file. Its staged
/// segments must survive `detach`. The rest of the run directory is removed.
///
/// Asynchronous contract (D48): `session_stop` queues the part and returns Ok. The
/// write failure arrives later as `SessionPartFailed` (with the kept stage dir),
/// followed by an `Error` that names the kept path. `detach` returns before the
/// final part is written and before cleanup. Cleanup runs on the finalizer thread,
/// so the test waits with `wait_idle` before it checks the directories and events.
#[test]
fn failed_session_part_keeps_staged_segments_through_detach() {
    let test = "failed_session_part_keeps_staged_segments_through_detach";
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, work, _clips, sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);

    match rec.attach(win.hwnd, "Test Game") {
        Ok(()) => {}
        Err(e @ (RecorderError::Io(_) | RecorderError::Unsupported)) => {
            skip!("{test}: attach failed: {e}");
        }
        Err(e) => panic!("{test}: attach failed unexpectedly: {e:?}"),
    }

    rec.session_start().expect("session_start");

    // Wait until the tracker has staged at least one segment in the stage dir.
    let run_dir = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let found = run_dirs(&work)
                .into_iter()
                .find(|r| stage_dirs(r).iter().any(|s| file_count(s) > 0));
            if let Some(r) = found {
                break r;
            }
            if Instant::now() >= deadline {
                panic!("{test}: no staged segment appeared within 30 s");
            }
            thread::sleep(Duration::from_millis(200));
        }
    };
    let stage_before: Vec<PathBuf> = stage_dirs(&run_dir);
    assert!(!stage_before.is_empty(), "no stage dir before failure");

    // Force finalize to fail: a regular file where the sessions directory must be.
    std::fs::remove_dir_all(&sessions).expect("remove sessions dir");
    std::fs::write(&sessions, b"not a directory").expect("write blocker file");

    // Asynchronous (D48): the part is only queued, so session_stop returns Ok.
    let stop = rec.session_stop();
    assert!(
        stop.is_ok(),
        "session_stop should queue the part and return Ok, got {stop:?}"
    );

    // The write failure arrives later as SessionPartFailed for the final part.
    let mut events: Vec<RecorderEvent> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let kept_stage = loop {
        let now = Instant::now();
        if now >= deadline {
            panic!("{test}: no SessionPartFailed for part 1 within 60 s; events: {events:?}");
        }
        match rx.recv_timeout(deadline - now) {
            Ok(ev) => {
                let kept = match &ev {
                    RecorderEvent::SessionPartFailed {
                        part_index: 1,
                        final_part: true,
                        kept_stage,
                        ..
                    } => Some(kept_stage.clone()),
                    _ => None,
                };
                events.push(ev);
                if let Some(k) = kept {
                    break k;
                }
            }
            Err(e) => panic!("{test}: no SessionPartFailed received: {e:?}; events: {events:?}"),
        }
    };
    assert!(
        stage_before.contains(&kept_stage),
        "kept_stage {} is not one of the stage dirs before the failure: {stage_before:?}",
        kept_stage.display()
    );

    // Cleanup, and the Error that names the kept dirs, run on the finalizer thread
    // after detach. Wait for them before collecting the remaining events.
    rec.detach();
    assert!(
        rec.wait_idle(Duration::from_secs(60)),
        "finalizer did not finish within 60 s"
    );
    events.extend(drain(&rx));

    // The error event must name the kept path.
    let named_kept = events.iter().any(|ev| match ev {
        RecorderEvent::Error(msg) => stage_before
            .iter()
            .any(|s| msg.contains(&*s.display().to_string())),
        _ => false,
    });

    // Kept: each stage dir from before, with its segment files.
    for stage in &stage_before {
        assert!(stage.is_dir(), "stage dir removed: {}", stage.display());
        assert!(
            file_count(stage) > 0,
            "stage dir {} has no segment files left",
            stage.display()
        );
    }

    // Everything else in the run dir is removed.
    if run_dir.exists() {
        let leftovers: Vec<String> = std::fs::read_dir(&run_dir)
            .expect("read run dir")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| !stage_before.contains(p))
            .map(|p| p.display().to_string())
            .collect();
        assert!(
            leftovers.is_empty(),
            "entries other than kept stage dirs remain: {leftovers:?}"
        );
    }

    assert!(
        named_kept,
        "no RecorderEvent::Error names the kept path; events: {events:?}"
    );
}
