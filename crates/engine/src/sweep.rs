//! Startup sweep of run directories left behind by crashed sessions (issue #197).
//!
//! Each attach creates `work_dir/run-<nanos>/`. A crash skips `Drop` and
//! `detach`, so the directory and its ring segments stay on disk. This module
//! removes such directories once they are old enough. A directory that holds
//! staged session parts (`stage-s*`) is kept for [`SALVAGE_KEEP`] after its
//! last activity, because those parts are the only copy of the session. While
//! it is kept it is reported, and salvage (#172) decides what happens to it.
//! Once it is older than that, it is deleted like any other stale run.
//!
//! The sweep is best effort. Problems are returned in [`SweepReport::errors`]
//! and never abort the caller.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Name prefix of run directories created by the recorder.
pub(crate) const RUN_PREFIX: &str = "run-";
/// Name prefix of staged session part directories inside a run directory.
pub(crate) const STAGE_PREFIX: &str = "stage-s";
/// Run directories whose modification time is older than this are stale.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// A crashed session's staged parts are offered for recovery for this long
/// after the run's last activity. After that the run is deleted like any other
/// stale run, so crashed stage folders cannot pile up forever.
pub(crate) const SALVAGE_KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Most directory entries examined by one sweep. Bounds the work done at attach.
const MAX_ENTRIES: usize = 1024;
/// Most entries examined when finding a run's last activity. A run with more
/// entries than this is treated as kept.
const MAX_WALK_ENTRIES: usize = 10_000;

/// Outcome of one [`sweep_stale_runs`] call.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// Stale run directories that were deleted.
    pub removed: Vec<PathBuf>,
    /// Stale run directories kept because they hold staged session parts.
    pub kept_for_salvage: Vec<PathBuf>,
    /// Problems met during the sweep. Each one names the path concerned.
    pub errors: Vec<String>,
}

/// Deletes stale `run-*` directories directly under `root`.
///
/// A directory is removed only when all of these hold:
/// - its name starts with [`RUN_PREFIX`] and it is not `current`;
/// - it is a real directory (files and symbolic links are left alone);
/// - its modification time is at least `max_age` before `now`;
/// - if it holds a `stage-s*` directory, its last activity (see
///   [`last_activity`]) is at least [`SALVAGE_KEEP`] before `now`. Until then
///   it is kept and listed in `kept_for_salvage`. If it cannot be inspected,
///   it is kept too and the problem is reported in `errors`.
///
/// A missing `root` is not an error. At most [`MAX_ENTRIES`] entries of `root`
/// are examined.
pub(crate) fn sweep_stale_runs(
    root: &Path,
    current: &Path,
    now: SystemTime,
    max_age: Duration,
) -> SweepReport {
    let mut report = SweepReport::default();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return report,
        Err(e) => {
            report
                .errors
                .push(format!("could not read {}: {e}", root.display()));
            return report;
        }
    };
    for entry in entries.take(MAX_ENTRIES).flatten() {
        let path = entry.path();
        let is_run = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(RUN_PREFIX));
        if !is_run || path == current {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        match now.duration_since(modified) {
            Ok(age) if age >= max_age => {}
            _ => continue,
        }
        match has_stage_dir(&path) {
            Ok(false) => {}
            Ok(true) => {
                let salvage_expired = match last_activity(&path) {
                    Ok(last) => {
                        matches!(now.duration_since(last), Ok(age) if age >= SALVAGE_KEEP)
                    }
                    Err(e) => {
                        report
                            .errors
                            .push(format!("could not inspect {}: {e}", path.display()));
                        report.kept_for_salvage.push(path);
                        continue;
                    }
                };
                if !salvage_expired {
                    report.kept_for_salvage.push(path);
                    continue;
                }
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("could not inspect {}: {e}", path.display()));
                report.kept_for_salvage.push(path);
                continue;
            }
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => report.removed.push(path),
            Err(e) => report
                .errors
                .push(format!("could not delete {}: {e}", path.display())),
        }
    }
    report
}

/// Newest modification time of `run_dir` and of everything under it.
///
/// Symbolic links are neither followed nor counted. Entries that cannot be read
/// are skipped. Failing to read `run_dir` itself, or meeting more than
/// [`MAX_WALK_ENTRIES`] entries, is an error, since the walk then cannot say
/// how recent the run is.
fn last_activity(run_dir: &Path) -> io::Result<SystemTime> {
    let mut newest = fs::symlink_metadata(run_dir)?.modified()?;
    let mut pending = vec![run_dir.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if dir.as_path() == run_dir => return Err(e),
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                return Err(io::Error::other(format!(
                    "more than {MAX_WALK_ENTRIES} entries under {}",
                    run_dir.display()
                )));
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if let Ok(modified) = meta.modified() {
                newest = newest.max(modified);
            }
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(newest)
}

/// True when `run_dir` directly contains a directory whose name starts with
/// [`STAGE_PREFIX`].
fn has_stage_dir(run_dir: &Path) -> io::Result<bool> {
    for entry in fs::read_dir(run_dir)?.flatten() {
        let is_stage = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGE_PREFIX));
        if is_stage && entry.path().is_dir() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    /// A `now` two days after the fixtures were created, so they are stale.
    fn later() -> SystemTime {
        SystemTime::now() + 2 * DAY
    }

    /// A `now` `days` days after the moment of the call.
    fn days_from_now(days: u32) -> SystemTime {
        SystemTime::now() + DAY * days
    }

    /// Creates `run-<name>/stage-s1-p1/part-0001.ts` under `root` and returns
    /// the run directory, the stage directory and the part file.
    fn stage_run(root: &Path, name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let run = root.join(name);
        let stage = run.join("stage-s1-p1");
        fs::create_dir_all(&stage).unwrap();
        let part = stage.join("part-0001.ts");
        fs::write(&part, b"x").unwrap();
        (run, stage, part)
    }

    #[test]
    fn missing_root_is_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("absent");
        let report = sweep_stale_runs(&root, &root.join("run-now"), later(), STALE_AFTER);
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn stale_run_without_stages_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("run-1");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("seg-0001.ts"), b"x").unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            later(),
            STALE_AFTER,
        );
        assert_eq!(report.removed, vec![old.clone()]);
        assert!(report.kept_for_salvage.is_empty());
        assert!(report.errors.is_empty());
        assert!(!old.exists());
    }

    #[test]
    fn fresh_run_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("run-2");
        fs::create_dir(&fresh).unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            SystemTime::now(),
            STALE_AFTER,
        );
        assert_eq!(report, SweepReport::default());
        assert!(fresh.exists());
    }

    #[test]
    fn current_run_is_protected() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("run-cur");
        fs::create_dir(&current).unwrap();
        let report = sweep_stale_runs(dir.path(), &current, later(), STALE_AFTER);
        assert_eq!(report, SweepReport::default());
        assert!(current.exists());
    }

    #[test]
    fn stale_run_with_stage_dir_is_kept_for_salvage() {
        let dir = tempfile::tempdir().unwrap();
        let crashed = dir.path().join("run-3");
        let stage = crashed.join("stage-s1-p1");
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("part-0001.ts"), b"x").unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            later(),
            STALE_AFTER,
        );
        assert!(report.removed.is_empty());
        assert_eq!(report.kept_for_salvage, vec![crashed.clone()]);
        assert!(stage.exists());
    }

    #[test]
    fn stage_run_older_than_salvage_keep_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (run, _stage, _part) = stage_run(dir.path(), "run-6");
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            days_from_now(8),
            STALE_AFTER,
        );
        assert_eq!(report.removed, vec![run.clone()]);
        assert!(report.kept_for_salvage.is_empty());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!run.exists());
    }

    #[test]
    fn stage_run_within_salvage_keep_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (run, _stage, part) = stage_run(dir.path(), "run-7");
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            later(),
            STALE_AFTER,
        );
        assert!(report.removed.is_empty());
        assert_eq!(report.kept_for_salvage, vec![run.clone()]);
        assert!(part.exists());
    }

    #[test]
    fn stage_run_is_judged_by_its_newest_inner_file() {
        let dir = tempfile::tempdir().unwrap();
        let base = SystemTime::now();
        let (run, _stage, part) = stage_run(dir.path(), "run-9");
        // The run dir and its other entries date from `base`, which is 8 days
        // before `now`. Only the part file is recent: 1 day before `now`.
        fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .unwrap()
            .set_modified(base + DAY * 7)
            .unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            base + DAY * 8,
            STALE_AFTER,
        );
        assert!(report.removed.is_empty(), "{:?}", report.removed);
        assert_eq!(report.kept_for_salvage, vec![run.clone()]);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(part.exists());
    }

    #[test]
    fn stage_run_with_inner_file_dated_in_the_future_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let base = SystemTime::now();
        let (run, _stage, part) = stage_run(dir.path(), "run-10");
        fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .unwrap()
            .set_modified(base + DAY * 30)
            .unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            base + DAY * 8,
            STALE_AFTER,
        );
        assert!(report.removed.is_empty(), "{:?}", report.removed);
        assert_eq!(report.kept_for_salvage, vec![run.clone()]);
        assert!(part.exists());
    }

    #[test]
    fn current_stage_run_is_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (current, stage, part) = stage_run(dir.path(), "run-8");
        let report = sweep_stale_runs(dir.path(), &current, days_from_now(8), STALE_AFTER);
        assert_eq!(report, SweepReport::default());
        assert!(current.exists());
        assert!(stage.exists());
        assert!(part.exists());
    }

    #[test]
    fn stage_named_file_does_not_protect_run() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("run-4");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("stage-s1-p1"), b"not a dir").unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            later(),
            STALE_AFTER,
        );
        assert_eq!(report.removed, vec![old.clone()]);
        assert!(!old.exists());
    }

    #[test]
    fn files_and_other_names_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("run-5");
        fs::write(&file, b"a file named like a run").unwrap();
        let other = dir.path().join("keep-me");
        fs::create_dir(&other).unwrap();
        let report = sweep_stale_runs(
            dir.path(),
            &dir.path().join("run-cur"),
            later(),
            STALE_AFTER,
        );
        assert_eq!(report, SweepReport::default());
        assert!(file.exists());
        assert!(other.exists());
    }
}
