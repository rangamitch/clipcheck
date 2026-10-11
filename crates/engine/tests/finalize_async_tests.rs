//! Tests for the asynchronous session finalizer (DESIGN.md D48).
//!
//! Idle-only tests run everywhere. Window tests need Windows Graphics Capture, and skip with
//! `SKIPPED: <test>: <reason>` when it is missing (`CLIPCHECK_REQUIRE_CAPTURE=1` turns
//! skips into failures). Window tests take the shared `common::serial()` lock.

#[macro_use]
mod common;

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::sync::mpsc;
#[cfg(windows)]
use std::sync::mpsc::Receiver;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use clipcheck_engine::recorder::Recorder;
#[cfg(windows)]
use clipcheck_engine::recorder::RecorderError;
#[cfg(windows)]
use clipcheck_engine::recorder::RecorderEvent;

use common::make_config;
#[cfg(windows)]
use common::serial;
#[cfg(windows)]
use common::wait_until;
#[cfg(windows)]
use common::win_window::TestWindow;

/// Fill colour of the test window (COLORREF 0x00BBGGRR).
#[cfg(windows)]
const WINDOW_FILL: u32 = 0x0030_6090;

/// Attaches `$hwnd` to `$rec`. `Unsupported` skips the test.
#[cfg(windows)]
macro_rules! attach_or_skip {
    ($rec:expr, $hwnd:expr, $game:expr) => {
        match $rec.attach($hwnd, $game) {
            Ok(()) => {}
            Err(RecorderError::Unsupported) => {
                skip!("recording is not supported on this platform")
            }
            Err(e) => panic!("attach to the test window failed: {e:?}"),
        }
    };
}

/// Receives events until `stop` returns true for one of them (that event is
/// included) or `timeout` elapses. Returns every event received, in order.
#[cfg(windows)]
fn collect_until<F>(
    rx: &Receiver<RecorderEvent>,
    timeout: Duration,
    mut stop: F,
) -> Vec<RecorderEvent>
where
    F: FnMut(&RecorderEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            return out;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(ev) => {
                let done = stop(&ev);
                out.push(ev);
                if done {
                    return out;
                }
            }
            Err(_) => return out,
        }
    }
}

/// Waits until the replay buffer span has grown by `secs` past `base`.
#[cfg(windows)]
fn wait_footage_past(rec: &Recorder, base: f64, secs: f64, timeout: Duration) {
    let want = base + secs;
    let reached = wait_until(timeout, || rec.status().buffer_secs >= want);
    assert!(
        reached,
        "buffer reached {:.2} s, wanted {want:.2} s (base {base:.2} s + {secs} s)",
        rec.status().buffer_secs
    );
}

#[cfg(windows)]
fn run_dirs(work: &Path) -> Vec<String> {
    std::fs::read_dir(work)
        .expect("read work_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("run-"))
        .collect()
}

/// Number of files under `p` (recursive). A plain file counts as one.
#[cfg(windows)]
fn count_files(p: &Path) -> usize {
    if p.is_dir() {
        std::fs::read_dir(p)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| count_files(&e.path()))
            .sum()
    } else if p.is_file() {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// idle test (all platforms)
// ---------------------------------------------------------------------------

#[test]
fn wait_idle_on_idle_recorder_is_true() {
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _work, _clips, _sessions) = make_config(root.path(), "libx264", false, false);
    let (tx, _rx) = mpsc::channel();
    let rec = Recorder::new(cfg, tx);
    assert!(
        rec.wait_idle(Duration::from_millis(100)),
        "wait_idle on an idle recorder returned false"
    );
    assert_eq!(
        rec.status().finalizing,
        0,
        "finalizing not 0 on idle recorder"
    );
}

// ---------------------------------------------------------------------------
// window tests (Windows only)
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn split_returns_quickly_and_parts_arrive_in_order() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _work, _clips, _sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    rec.session_start().expect("session_start");
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));

    let t = Instant::now();
    rec.session_split().expect("session_split #1");
    let d_split1 = t.elapsed();

    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));

    let t = Instant::now();
    rec.session_split().expect("session_split #2");
    let d_split2 = t.elapsed();

    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));

    let t = Instant::now();
    rec.session_stop().expect("session_stop");
    let d_stop = t.elapsed();

    println!("split1 {d_split1:?}, split2 {d_split2:?}, stop {d_stop:?}");
    for (name, d) in [("split1", d_split1), ("split2", d_split2), ("stop", d_stop)] {
        assert!(
            d < Duration::from_secs(2),
            "{name} took {d:?}, expected < 2 s"
        );
    }

    let evs = collect_until(&rx, Duration::from_secs(60), |e| {
        matches!(e, RecorderEvent::SessionPartSaved { part_index: 3, .. })
    });
    assert!(
        !evs.iter().any(|e| matches!(
            e,
            RecorderEvent::SessionPartFailed { .. } | RecorderEvent::Error(_)
        )),
        "unexpected failure or error event: {evs:?}"
    );
    let parts: Vec<(u32, bool, std::path::PathBuf)> = evs
        .iter()
        .filter_map(|e| match e {
            RecorderEvent::SessionPartSaved {
                path,
                part_index,
                final_part,
            } => Some((*part_index, *final_part, path.clone())),
            _ => None,
        })
        .collect();
    let order: Vec<(u32, bool)> = parts.iter().map(|(i, f, _)| (*i, *f)).collect();
    assert_eq!(
        order,
        vec![(1, false), (2, false), (3, true)],
        "SessionPartSaved order mismatch"
    );
    for (idx, _, path) in &parts {
        assert!(path.is_file(), "part {idx} missing: {}", path.display());
        let size = std::fs::metadata(path).expect("metadata").len();
        assert!(size > 0, "part {idx} is empty: {}", path.display());
        let d = reader_duration(path);
        assert!(d > 0.0, "part {idx} duration {d} not > 0");
        println!("part {idx}: {} bytes, {d:.3} s", size);
    }
    rec.detach();
}

#[cfg(windows)]
#[test]
fn status_stays_responsive_while_finalizing() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _work, _clips, _sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    rec.session_start().expect("session_start");
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));
    rec.session_split().expect("session_split");

    let mut max = Duration::ZERO;
    let mut saw_finalizing = false;
    for i in 0..20 {
        let t = Instant::now();
        let st = rec.status();
        let d = t.elapsed();
        max = max.max(d);
        if st.finalizing >= 1 {
            saw_finalizing = true;
        }
        assert!(
            d < Duration::from_millis(200),
            "status() call {i} took {d:?}"
        );
    }
    println!(
        "max status() duration {max:?}; observed finalizing >= 1: {saw_finalizing} (report-only)"
    );

    assert!(
        rec.wait_idle(Duration::from_secs(60)),
        "wait_idle did not complete within 60 s"
    );
    assert_eq!(
        rec.status().finalizing,
        0,
        "finalizing not 0 after wait_idle"
    );

    rec.session_stop().expect("session_stop");
    assert!(
        rec.wait_idle(Duration::from_secs(60)),
        "wait_idle after stop false"
    );
    rec.detach();
}

#[cfg(windows)]
#[test]
fn stop_write_failure_is_an_event() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, work, _clips, sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    rec.session_start().expect("session_start");
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));

    // Make the part unwritable: the Sessions dir becomes a regular file.
    std::fs::remove_dir_all(&sessions).expect("remove sessions dir");
    std::fs::write(&sessions, b"not a directory").expect("write blocking file");

    rec.session_stop()
        .expect("session_stop must return Ok even when the write will fail");

    let evs = collect_until(
        &rx,
        Duration::from_secs(60),
        |e| matches!(e, RecorderEvent::Error(m) if m.starts_with("session not saved:")),
    );
    let fail_idx = evs.iter().position(|e| {
        matches!(
            e,
            RecorderEvent::SessionPartFailed {
                part_index: 1,
                final_part: true,
                ..
            }
        )
    });
    let err_idx = evs
        .iter()
        .position(|e| matches!(e, RecorderEvent::Error(m) if m.starts_with("session not saved:")));
    let fail_idx = fail_idx.unwrap_or_else(|| panic!("no SessionPartFailed(1, final) in {evs:?}"));
    let err_idx = err_idx.unwrap_or_else(|| panic!("no 'session not saved:' Error in {evs:?}"));
    assert!(
        fail_idx < err_idx,
        "Error arrived before SessionPartFailed: {evs:?}"
    );

    let kept: PathBuf = match &evs[fail_idx] {
        RecorderEvent::SessionPartFailed { kept_stage, .. } => kept_stage.clone(),
        _ => unreachable!(),
    };
    assert!(kept.exists(), "kept_stage missing: {}", kept.display());
    let n = count_files(&kept);
    assert!(n >= 1, "kept_stage has no files: {}", kept.display());
    println!("kept_stage {} holds {n} file(s)", kept.display());

    rec.detach();
    assert!(
        rec.wait_idle(Duration::from_secs(60)),
        "wait_idle after detach did not complete within 60 s"
    );
    let rest: Vec<RecorderEvent> = rx.try_iter().collect();
    let kept_str = kept.display().to_string();
    let names_kept = rest
        .iter()
        .any(|e| matches!(e, RecorderEvent::Error(m) if m.contains(&kept_str)));
    assert!(
        names_kept,
        "no Error after detach names the kept stage {kept_str}; events: {rest:?}"
    );
    assert!(
        kept.exists(),
        "kept stage removed by cleanup: {}",
        kept.display()
    );
    let _ = work;
}

#[cfg(windows)]
#[test]
fn detach_returns_and_wait_idle_completes() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, work, _clips, _sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    rec.session_start().expect("session_start");
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));

    let t = Instant::now();
    rec.detach();
    let d = t.elapsed();
    println!("detach returned in {d:?}");

    assert!(
        rec.wait_idle(Duration::from_secs(60)),
        "wait_idle did not complete within 60 s"
    );
    let evs: Vec<RecorderEvent> = rx.try_iter().collect();
    let final_path = evs.iter().find_map(|e| match e {
        RecorderEvent::SessionPartSaved {
            path,
            final_part: true,
            ..
        } => Some(path.clone()),
        _ => None,
    });
    let final_path =
        final_path.unwrap_or_else(|| panic!("no final SessionPartSaved after detach: {evs:?}"));
    assert!(
        final_path.is_file(),
        "final part missing: {}",
        final_path.display()
    );

    let leftover = run_dirs(&work);
    assert!(
        leftover.is_empty(),
        "run dirs left after wait_idle: {leftover:?}"
    );
}

#[cfg(windows)]
#[test]
fn drop_waits_for_queued_part() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _work, _clips, _sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    rec.session_start().expect("session_start");
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(20));

    rec.session_stop().expect("session_stop");
    drop(rec);

    let evs: Vec<RecorderEvent> = rx.try_iter().collect();
    let path = evs
        .iter()
        .find_map(|e| match e {
            RecorderEvent::SessionPartSaved {
                path,
                final_part: true,
                ..
            } => Some(path.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no SessionPartSaved(final) after drop: {evs:?}"));
    assert!(
        path.is_file(),
        "part file missing after drop returned: {}",
        path.display()
    );
}

/// Duration of the movie in seconds, read with the in-repo MP4 reader.
#[allow(dead_code)]
fn reader_duration(path: &std::path::Path) -> f64 {
    clipcheck_mp4::reader::read_movie(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .duration_secs
}
