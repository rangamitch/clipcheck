//! Crash salvage of staged session parts (issues #26, #172).
//!
//! After a crash, a session part can be left as `part-NNNNNN.mp4` files in a
//! stage directory under a `run-*` directory. [`discover`] finds those stages
//! and [`salvage_stages`] concatenates each one into a `recovered-*.mp4` file
//! in the sessions directory. Staged files are never deleted on failure.

use std::fs;
use std::path::Path;

use clipcheck_engine::sessionfile::{
    find_salvage_stages, recover, recovered_part_path, remove_empty_stage, FinalizedPart,
    SalvageStage,
};

use crate::logging;

/// Result of salvaging one stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SalvageOutcome {
    /// The staged parts were concatenated into `part.path`; the stage dir was removed when empty.
    Recovered {
        stage: SalvageStage,
        part: FinalizedPart,
    },
    /// Nothing could be done. The staged files are untouched and still on disk.
    Failed { stage: SalvageStage, error: String },
}

/// Salvages every stage in order. Pure of any service state; joins the staged parts through `recover`. Blocking, so callers run it on a worker thread.
///
/// Each stage is written to `recovered_part_path(sessions_dir, stage)`. An
/// existing output file is never overwritten; that stage fails and the batch
/// continues. One log line is written per outcome.
pub fn salvage_stages(sessions_dir: &Path, stages: Vec<SalvageStage>) -> Vec<SalvageOutcome> {
    let mut outcomes = Vec::with_capacity(stages.len());
    for stage in stages {
        let outcome = salvage_one(sessions_dir, stage);
        match &outcome {
            SalvageOutcome::Recovered { stage, part } => logging::info(&format!(
                "salvage: recovered {} segments from {} into {} ({} bytes)",
                part.segments,
                stage.stage_dir.display(),
                part.path.display(),
                part.bytes
            )),
            SalvageOutcome::Failed { stage, error } => logging::warn(&format!(
                "salvage: failed for {}: {}",
                stage.stage_dir.display(),
                error
            )),
        }
        outcomes.push(outcome);
    }
    outcomes
}

fn salvage_one(sessions_dir: &Path, stage: SalvageStage) -> SalvageOutcome {
    let out = recovered_part_path(sessions_dir, &stage);
    if let Err(e) = fs::create_dir_all(sessions_dir) {
        return SalvageOutcome::Failed {
            stage,
            error: format!("cannot create sessions dir: {e}"),
        };
    }
    if out.exists() {
        return SalvageOutcome::Failed {
            stage,
            error: format!("output already exists: {}", out.display()),
        };
    }
    match recover(stage.stage_dir.clone(), &out) {
        Ok(Some(part)) => {
            let _ = remove_empty_stage(&stage);
            SalvageOutcome::Recovered { stage, part }
        }
        Ok(None) => SalvageOutcome::Failed {
            stage,
            error: "no staged parts".to_string(),
        },
        Err(e) => SalvageOutcome::Failed {
            stage,
            error: e.to_string(),
        },
    }
}

/// Finds salvageable stages under `work_dir`. Reports only; nothing is changed.
///
/// Call this only while no recorder run is active, because an active run's
/// stage directories look the same as leftovers. On an io error a warning is
/// logged and an empty list is returned.
pub fn discover(work_dir: &Path) -> Vec<SalvageStage> {
    match find_salvage_stages(work_dir, None) {
        Ok(stages) => stages,
        Err(e) => {
            logging::warn(&format!(
                "salvage: cannot scan {} for leftover parts: {}",
                work_dir.display(),
                e
            ));
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stage(root: &Path) -> SalvageStage {
        let run_dir = root.join("run-20260101-000000");
        let stage_dir = run_dir.join("stage-s1-p1");
        fs::create_dir_all(&stage_dir).unwrap();
        fs::write(stage_dir.join("part-000001.mp4"), b"fake segment").unwrap();
        SalvageStage {
            run_dir,
            stage_dir,
            session_seq: 1,
            part: 1,
            segments: 1,
        }
    }

    #[test]
    fn failing_recover_keeps_staged_part_and_writes_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let stage = make_stage(dir.path());
        let sessions = dir.path().join("sessions");
        let out = recovered_part_path(&sessions, &stage);
        let part_file = stage.stage_dir.join("part-000001.mp4");

        let outcomes = salvage_stages(&sessions, vec![stage.clone()]);

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            SalvageOutcome::Failed { stage: s, error } => {
                assert_eq!(s, &stage);
                assert!(!error.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(part_file.exists(), "staged part must survive a failure");
        assert!(!out.exists(), "no output may be written on failure");
    }

    #[test]
    fn existing_output_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let stage = make_stage(dir.path());
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let out = recovered_part_path(&sessions, &stage);
        fs::write(&out, b"user data").unwrap();

        let outcomes = salvage_stages(&sessions, vec![stage.clone()]);

        match &outcomes[0] {
            SalvageOutcome::Failed { error, .. } => {
                assert!(error.contains("output already exists"), "{error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(fs::read(&out).unwrap(), b"user data");
        assert!(stage.stage_dir.join("part-000001.mp4").exists());
    }

    #[test]
    fn discover_on_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(discover(&missing).is_empty());
    }

    #[test]
    fn discover_finds_stage_with_part() {
        let dir = tempfile::tempdir().unwrap();
        let stage = make_stage(dir.path());
        let found = discover(dir.path());
        assert_eq!(found, vec![stage]);
    }
}
