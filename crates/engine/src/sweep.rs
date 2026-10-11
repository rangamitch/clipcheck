//! Startup sweep of run directories left behind by crashed sessions (issue #197).
//!
//! Each attach creates `work_dir/run-<nanos>/`. A crash skips `Drop` and
//! `detach`, so the directory and its ring segments stay on disk. This module
//! removes such directories once they are old enough. A directory that holds
//! staged session parts (`stage-s*`) is never removed, because those parts are
//! the only copy of the session. It is reported instead, and salvage (#172)
//! decides what happens to it.
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
/// Most directory entries examined by one sweep. Bounds the work done at attach.
const MAX_ENTRIES: usize = 1024;

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
/// - it holds no `stage-s*` directory. Any such directory is kept and listed
///   in `kept_for_salvage`. If the directory cannot be read, it is kept too.
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
                report.kept_for_salvage.push(path);
                continue;
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
