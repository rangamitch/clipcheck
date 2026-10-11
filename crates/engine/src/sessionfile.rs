//! Session file assembly from encoder segments (task E3, design D18).
//!
//! Finished keyframe-aligned segments are hard-linked (or copied, when linking
//! fails) into a per-session stage directory as `part-{n:06}.mp4` in arrival
//! order. `finalize` losslessly concatenates them in process (native MP4
//! remux, no FFmpeg, D62) into a faststart MP4. Splitting a session is `finalize` followed by
//! continuing to `add_segment` on the same assembler, which keeps splits
//! zero-gap because the segments are cut on keyframes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clipcheck_buffer::extract::{concat_publish, write_concat_list};

use crate::sweep::{RUN_PREFIX, STAGE_PREFIX};

const CONCAT_LIST: &str = "concat.txt";

/// Result of a successful [`SessionAssembler::finalize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedPart {
    /// The renamed output file.
    pub path: PathBuf,
    /// Size of the output file in bytes.
    pub bytes: u64,
    /// Number of staged segments that were concatenated.
    pub segments: usize,
}

/// Collects segments for one session part and assembles them into an MP4.
#[derive(Debug)]
pub struct SessionAssembler {
    stage_dir: PathBuf,
    staged: Vec<PathBuf>,
    next_index: u64,
}

impl SessionAssembler {
    /// Creates an assembler over `stage_dir` (created if missing).
    ///
    /// Staged parts already present in the directory are adopted in part
    /// order, so numbering continues past them and `finalize` includes them.
    /// Callers that want to salvage a crashed session should run [`recover`]
    /// first.
    pub fn new(stage_dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&stage_dir)?;
        let existing = scan_staged(&stage_dir)?;
        let next_index = existing.last().map_or(0, |(n, _)| n + 1);
        let staged = existing.into_iter().map(|(_, p)| p).collect();
        Ok(Self {
            stage_dir,
            staged,
            next_index,
        })
    }

    /// Stages `seg` as the next part. Hard-links it, falling back to a copy
    /// when linking fails (for example across volumes), so deleting the
    /// original segment never breaks the session.
    pub fn add_segment(&mut self, seg: &Path) -> io::Result<()> {
        let dst = part_path(&self.stage_dir, self.next_index);
        if fs::hard_link(seg, &dst).is_err() {
            fs::copy(seg, &dst)?;
        }
        self.staged.push(dst);
        self.next_index += 1;
        Ok(())
    }

    /// Number of segments currently staged.
    pub fn segment_count(&self) -> usize {
        self.staged.len()
    }

    /// The directory holding this part's staged segments.
    pub(crate) fn stage_dir(&self) -> &Path {
        &self.stage_dir
    }

    /// Concatenates the staged segments into `out` and resets the assembler.
    ///
    /// Writes `<out>.tmp.mp4` and renames it onto `out`. Staged files are
    /// removed only after the rename succeeds. On remux failure the staged
    /// files are kept and the error is returned. Finalizing with
    /// no segments returns `InvalidInput` and writes nothing.
    pub fn finalize(&mut self, out: &Path) -> io::Result<FinalizedPart> {
        if self.staged.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no segments staged to finalize",
            ));
        }
        let segments = self.staged.len();
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }

        let list = self.stage_dir.join(CONCAT_LIST);
        write_concat_list(&list, &self.staged, 0.0)?;
        let result = concat_publish(&list, out, None);
        let _ = fs::remove_file(&list);
        let bytes = result?;

        // The output is in place; the staged copies are now redundant. Removal
        // is best effort: a file that survives here is picked up by `recover`.
        for p in self.staged.drain(..) {
            let _ = fs::remove_file(p);
        }

        Ok(FinalizedPart {
            path: out.to_path_buf(),
            bytes,
            segments,
        })
    }
}

/// Assembles leftover staged parts in `stage_dir` into `out` after a crash.
///
/// Returns `Ok(None)` when `stage_dir` is missing or holds no staged parts.
/// A missing `stage_dir` is never created. The directory is scanned once,
/// inside [`SessionAssembler::new`].
pub fn recover(stage_dir: PathBuf, out: &Path) -> io::Result<Option<FinalizedPart>> {
    // Checked first because `SessionAssembler::new` creates missing directories.
    if !stage_dir.is_dir() {
        return Ok(None);
    }
    let mut asm = SessionAssembler::new(stage_dir)?;
    if asm.segment_count() == 0 {
        return Ok(None);
    }
    asm.finalize(out).map(Some)
}

fn part_path(dir: &Path, n: u64) -> PathBuf {
    dir.join(format!("part-{n:06}.mp4"))
}

/// Lists `part-NNNNNN.mp4` files in `dir`, sorted by part number.
fn scan_staged(dir: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut parts = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let num = path
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(|f| f.strip_prefix("part-"))
            .and_then(|f| f.strip_suffix(".mp4"))
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| d.parse::<u64>().ok());
        if let Some(n) = num {
            parts.push((n, path));
        }
    }
    parts.sort_by_key(|(n, _)| *n);
    Ok(parts)
}

/// A staged session part left behind in a run directory, found by
/// [`find_salvage_stages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalvageStage {
    /// The `run-*` directory that holds the stage directory.
    pub run_dir: PathBuf,
    /// The `stage-s<seq>-p<part>` directory with the staged parts.
    pub stage_dir: PathBuf,
    /// Session sequence number parsed from the stage directory name.
    pub session_seq: u64,
    /// Part number parsed from the stage directory name.
    pub part: u32,
    /// Number of `part-NNNNNN.mp4` files in the stage directory.
    pub segments: usize,
}

/// Parses `stage-s<seq>-p<part>` into `(seq, part)`. Returns `None` for any
/// other name, including names where either number is not plain digits.
fn parse_stage_name(name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix(STAGE_PREFIX)?;
    let (seq, part) = rest.split_once("-p")?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(seq) || !digits(part) {
        return None;
    }
    Some((seq.parse().ok()?, part.parse().ok()?))
}

/// Finds staged session parts left in `run-*` directories under `work_dir`.
///
/// Only reports: nothing is deleted or modified. Recovery is [`recover`].
/// `current_run` is skipped when given, so the live run is never reported.
/// Stage directories with names that do not parse, or with no `part-*.mp4`,
/// are ignored. Unreadable run or stage directories are skipped. A missing
/// `work_dir` gives an empty list. The result is sorted by run directory
/// name, then session sequence, then part.
pub fn find_salvage_stages(
    work_dir: &Path,
    current_run: Option<&Path>,
) -> io::Result<Vec<SalvageStage>> {
    let runs = match fs::read_dir(work_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in runs.flatten() {
        let run_dir = entry.path();
        let is_run = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(RUN_PREFIX));
        if !is_run || !run_dir.is_dir() || current_run == Some(run_dir.as_path()) {
            continue;
        }
        let Ok(stages) = fs::read_dir(&run_dir) else {
            continue;
        };
        for stage in stages.flatten() {
            let Some((session_seq, part)) = stage.file_name().to_str().and_then(parse_stage_name)
            else {
                continue;
            };
            let stage_dir = stage.path();
            if !stage_dir.is_dir() {
                continue;
            }
            let Ok(parts) = scan_staged(&stage_dir) else {
                continue;
            };
            if parts.is_empty() {
                continue;
            }
            found.push(SalvageStage {
                run_dir: run_dir.clone(),
                stage_dir,
                session_seq,
                part,
                segments: parts.len(),
            });
        }
    }
    found.sort_by(|a, b| {
        a.run_dir
            .file_name()
            .cmp(&b.run_dir.file_name())
            .then(a.session_seq.cmp(&b.session_seq))
            .then(a.part.cmp(&b.part))
    });
    Ok(found)
}

/// Output path for salvaging `stage`: `sessions_dir/recovered-<run dir name>-s<seq>-p<part>.mp4`.
///
/// Pure: nothing is created. The `recovered-` prefix is for salvage output
/// only, so the name never collides with a normal session file. Callers must
/// not name normal sessions with that prefix.
pub fn recovered_part_path(sessions_dir: &Path, stage: &SalvageStage) -> PathBuf {
    let run = stage
        .run_dir
        .file_name()
        .map_or_else(|| "run".to_string(), |n| n.to_string_lossy().into_owned());
    sessions_dir.join(format!(
        "recovered-{run}-s{}-p{}.mp4",
        stage.session_seq, stage.part
    ))
}

/// Removes `stage.stage_dir` when it holds nothing worth keeping.
///
/// The directory is removed only if it contains no entries except an optional
/// file named `concat.txt`. That file is deleted first. Any part file or other
/// entry makes this return `false` without changing anything. Never recursive.
/// Returns whether the directory was removed.
pub fn remove_empty_stage(stage: &SalvageStage) -> bool {
    let Ok(entries) = fs::read_dir(&stage.stage_dir) else {
        return false;
    };
    let mut concat = None;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let is_concat_file = entry.file_name().to_str() == Some(CONCAT_LIST)
            && entry.file_type().is_ok_and(|t| t.is_file());
        if !is_concat_file {
            return false;
        }
        concat = Some(entry.path());
    }
    if let Some(list) = concat {
        if fs::remove_file(&list).is_err() {
            return false;
        }
    }
    fs::remove_dir(&stage.stage_dir).is_ok()
}

#[cfg(test)]
mod salvage_tests {
    use super::*;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn stage(run_dir: &Path, seq: u64, part: u32, segments: usize) -> SalvageStage {
        SalvageStage {
            run_dir: run_dir.to_path_buf(),
            stage_dir: run_dir.join(format!("stage-s{seq}-p{part}")),
            session_seq: seq,
            part,
            segments,
        }
    }

    #[test]
    fn parse_stage_name_accepts_only_plain_numbers() {
        assert_eq!(parse_stage_name("stage-s3-p12"), Some((3, 12)));
        assert_eq!(parse_stage_name("stage-s+3-p1"), None);
        assert_eq!(parse_stage_name("stage-sX-p1"), None);
        assert_eq!(parse_stage_name("stage-s1-pX"), None);
        assert_eq!(parse_stage_name("stage-s1"), None);
        assert_eq!(parse_stage_name("other-s1-p1"), None);
    }

    #[test]
    fn find_ignores_current_run_bad_names_and_empty_stages() {
        let dir = tempdir().unwrap();
        let work = dir.path();
        let run_a = work.join("run-a");
        touch(&run_a.join("stage-s2-p0").join("part-000000.mp4"));
        touch(&run_a.join("stage-s2-p0").join("part-000001.mp4"));
        touch(&run_a.join("stage-s1-p3").join("part-000000.mp4"));
        fs::create_dir_all(run_a.join("stage-s1-p0")).unwrap();
        touch(&run_a.join("stage-sX-p1").join("part-000000.mp4"));
        touch(&run_a.join("notes").join("part-000000.mp4"));
        let run_b = work.join("run-b");
        touch(&run_b.join("stage-s5-p0").join("part-000000.mp4"));
        let run_c = work.join("run-c");
        touch(&run_c.join("stage-s0-p0").join("part-000000.mp4"));
        touch(
            &work
                .join("other")
                .join("stage-s9-p9")
                .join("part-000000.mp4"),
        );
        touch(&work.join("run-file"));

        let found = find_salvage_stages(work, Some(run_b.as_path())).unwrap();
        let got: Vec<(String, u64, u32, usize)> = found
            .iter()
            .map(|s| {
                (
                    s.run_dir
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    s.session_seq,
                    s.part,
                    s.segments,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("run-a".to_string(), 1, 3, 1),
                ("run-a".to_string(), 2, 0, 2),
                ("run-c".to_string(), 0, 0, 1),
            ]
        );
        assert_eq!(found[1].stage_dir, run_a.join("stage-s2-p0"));
    }

    #[test]
    fn find_missing_work_dir_is_empty() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert_eq!(find_salvage_stages(&missing, None).unwrap(), Vec::new());
        assert!(!missing.exists());
    }

    #[test]
    fn recovered_path_format() {
        let s = stage(Path::new("work/run-123"), 4, 2, 1);
        let p = recovered_part_path(Path::new("sessions"), &s);
        assert_eq!(p, Path::new("sessions").join("recovered-run-123-s4-p2.mp4"));
    }

    #[test]
    fn remove_empty_stage_refuses_parts_and_unknown_entries() {
        let dir = tempdir().unwrap();
        let run = dir.path().join("run-1");
        touch(&run.join("stage-s1-p0").join("part-000000.mp4"));
        let s = stage(&run, 1, 0, 1);
        assert!(!remove_empty_stage(&s));
        assert!(s.stage_dir.is_dir());

        touch(&run.join("stage-s2-p0").join("junk.txt"));
        assert!(!remove_empty_stage(&stage(&run, 2, 0, 0)));
        assert!(run.join("stage-s2-p0").join("junk.txt").is_file());
    }

    #[test]
    fn remove_empty_stage_removes_empty_and_concat_only() {
        let dir = tempdir().unwrap();
        let run = dir.path().join("run-1");
        fs::create_dir_all(run.join("stage-s1-p0")).unwrap();
        assert!(remove_empty_stage(&stage(&run, 1, 0, 0)));
        assert!(!run.join("stage-s1-p0").exists());

        touch(&run.join("stage-s2-p0").join("concat.txt"));
        assert!(remove_empty_stage(&stage(&run, 2, 0, 0)));
        assert!(!run.join("stage-s2-p0").exists());
    }

    #[test]
    fn recover_missing_dir_is_none_and_not_created() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("stage-s1-p0");
        let out = dir.path().join("out.mp4");
        let got = recover(missing.clone(), &out).unwrap();
        assert!(got.is_none());
        assert!(!missing.exists());
    }

    #[test]
    fn recover_empty_dir_is_none() {
        let dir = tempdir().unwrap();
        let stage_dir = dir.path().join("stage-s1-p0");
        fs::create_dir_all(&stage_dir).unwrap();
        let out = dir.path().join("out.mp4");
        let got = recover(stage_dir, &out).unwrap();
        assert!(got.is_none());
        assert!(!out.exists());
    }
}
