//! Integration tests for issue #197: `Recorder::attach` sweeps stale run
//! directories under the work directory.
//!
//! The sweep runs at the start of `attach`, before window capture starts. The
//! tests attach with a dummy window handle (0), so capture fails straight
//! after the sweep. The run directory the attach created is removed again.
//! Whatever `attach` returns, the sweep has already run. Nothing is captured
//! and no game or ClipCheck process is started.
//!
//! Issue #193 (audio warnings while recording) is not covered here. The only
//! path that reports a failing audio source mid-run is a stream that stops
//! while running (`run_mixer` in `audiofeed.rs`). The public API has no way to
//! make a source stop mid-run. A source that fails to start is reported from
//! `attach` itself, not mid-session, so testing it would not show the behaviour
//! the issue asks for.

use std::fs;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

use clipcheck_encode::tuning::EncoderTuning;
use clipcheck_engine::recorder::{Recorder, RecorderConfig, RecorderEvent};
use tempfile::TempDir;

/// Backdate used for stale run directories. Older than the 24 h threshold.
const STALE_HOURS: i64 = 48;

/// A fresh, temporary root holding a `work` directory.
struct Env {
    _root: TempDir,
    work: PathBuf,
}

fn env() -> Env {
    let root = TempDir::new().expect("tempdir");
    let work = root.path().join("work");
    fs::create_dir_all(&work).expect("create work dir");
    Env { _root: root, work }
}

fn config(env: &Env) -> RecorderConfig {
    let base = env.work.parent().expect("work parent").to_path_buf();
    RecorderConfig {
        encoder_name: "libx264".to_string(),
        fps: 30,
        canvas: (1280, 720),
        canvas_height: 720,
        bitrate_kbps: 4000,
        buffer_enabled: true,
        buffer_max_secs: 60.0,
        work_dir: env.work.clone(),
        clips_dir: base.join("clips"),
        sessions_dir: base.join("sessions"),
        per_game_folders: true,
        session_auto_start: false,
        audio_tracks: Vec::new(),
        tuning: EncoderTuning::default(),
    }
}

/// Sets the modification time of `path` to `hours` ago. On Windows the path is
/// opened with FILE_FLAG_BACKUP_SEMANTICS so a directory can be used too. Run it
/// last, after the directory's contents are written. No subprocess is spawned.
fn backdate(path: &Path, hours: i64) {
    let when = SystemTime::now() - Duration::from_secs(hours as u64 * 3600);
    #[cfg(windows)]
    let file = {
        // FILE_FLAG_BACKUP_SEMANTICS lets Windows open a directory as a handle.
        let mut opts = fs::OpenOptions::new();
        opts.write(true)
            .custom_flags(0x0200_0000 /* FILE_FLAG_BACKUP_SEMANTICS */);
        opts.open(path)
    };
    #[cfg(not(windows))]
    let file = fs::File::open(path);
    file.and_then(|f| f.set_modified(when))
        .expect("backdate directory");
}

/// Attaches with a dummy handle and returns the attach result and every event
/// sent during the call. A successful attach is detached again.
fn attach_and_collect(
    rec: &mut Recorder,
    rx: &Receiver<RecorderEvent>,
) -> (Result<(), String>, Vec<RecorderEvent>) {
    let result = rec.attach(0, "Game").map_err(|e| e.to_string());
    if result.is_ok() {
        rec.detach();
    }
    let events: Vec<RecorderEvent> = rx.try_iter().collect();
    (result, events)
}

fn sweep_errors(events: &[RecorderEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            RecorderEvent::Error(text) if text.starts_with("stale run sweep") => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn stale_runs_are_removed_and_other_entries_kept() {
    let env = env();

    let stale = env.work.join("run-1000");
    fs::create_dir(&stale).expect("stale dir");
    fs::write(stale.join("seg-000000.mp4"), b"segment").expect("stale segment");
    backdate(&stale, STALE_HOURS);

    let fresh = env.work.join("run-2000");
    fs::create_dir(&fresh).expect("fresh dir");
    fs::write(fresh.join("seg-000000.mp4"), b"segment").expect("fresh segment");

    let other_dir = env.work.join("keep-me");
    fs::create_dir(&other_dir).expect("other dir");
    backdate(&other_dir, STALE_HOURS);

    let run_file = env.work.join("run-3000.bin");
    fs::write(&run_file, b"a file named like a run").expect("run-named file");
    backdate(&run_file, STALE_HOURS);

    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(config(&env), tx);
    let (_result, events) = attach_and_collect(&mut rec, &rx);

    assert!(!stale.exists(), "stale run directory must be removed");
    assert!(
        fresh.exists(),
        "run directory younger than 24 h must be kept"
    );
    assert!(other_dir.exists(), "non run-* directory must be kept");
    assert!(run_file.exists(), "a file named run-* must be kept");
    assert!(
        sweep_errors(&events).is_empty(),
        "sweep reported errors: {:?}",
        sweep_errors(&events)
    );
}

#[test]
fn stale_run_with_stage_directory_is_never_deleted() {
    let env = env();

    let crashed = env.work.join("run-crashed-1");
    let stage = crashed.join("stage-s1-p1");
    fs::create_dir_all(&stage).expect("stage dir");
    let part = stage.join("part-0001.ts");
    fs::write(&part, b"the only copy of this session part").expect("part file");
    backdate(&crashed, STALE_HOURS);

    // A recent run with staged parts is also kept, and gets no warning.
    let live = env.work.join("run-live-2");
    fs::create_dir_all(live.join("stage-s1-p1")).expect("live stage dir");

    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(config(&env), tx);
    let (_first, events) = attach_and_collect(&mut rec, &rx);

    assert!(crashed.exists(), "stale run with a stage dir must be kept");
    assert!(part.exists(), "staged session part must survive the sweep");
    assert!(live.exists(), "recent run with a stage dir must be kept");
    let warnings: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            RecorderEvent::Warning(text) => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("run-crashed-1")),
        "no Warning names the kept run directory; warnings: {warnings:?}"
    );
    assert!(
        !warnings.iter().any(|w| w.contains("run-live-2")),
        "recent run must not be reported: {warnings:?}"
    );

    // A second attach must not delete it either.
    let (_second, _events) = attach_and_collect(&mut rec, &rx);
    assert!(
        part.exists(),
        "staged session part must survive a second attach"
    );
}

#[test]
fn missing_work_directory_is_not_a_sweep_error() {
    let env = env();
    fs::remove_dir_all(&env.work).expect("remove work dir");

    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(config(&env), tx);
    let (_result, events) = attach_and_collect(&mut rec, &rx);

    assert!(
        sweep_errors(&events).is_empty(),
        "a missing work dir must not be reported: {:?}",
        sweep_errors(&events)
    );
}
