//! Crash salvage of staged session parts (issues #26, #172).
//!
//! Tests 1 to 5 need no test media: they use fake bytes.
//! Tests 6 and 7 need native test media (`clipcheck_testkit`). They
//! print `SKIPPED: <test>: <reason>` and return when it is unavailable.

use std::fs;
use std::io::Write;
use std::path::Path;

use clipcheck_engine::sessionfile::{recovered_part_path, SalvageStage};
use clipcheck_testkit::MediaSpec;
use clipcheckd::salvage::{discover, salvage_stages, SalvageOutcome};

const RUN: &str = "run-20260101-000000";

/// Writes `SKIPPED: <test>: <reason>` straight to stderr, then returns from the test.
macro_rules! skip {
    ($reason:expr) => {{
        let test = std::thread::current().name().unwrap_or("?").to_owned();
        let _ = writeln!(std::io::stderr(), "SKIPPED: {test}: {}", $reason);
        return;
    }};
}

/// Creates `work/RUN/stage-s<seq>-p<part>/` holding the given files.
fn make_stage(work: &Path, seq: u64, part: u32, files: &[(&str, &[u8])]) -> SalvageStage {
    let run_dir = work.join(RUN);
    let stage_dir = run_dir.join(format!("stage-s{seq}-p{part}"));
    fs::create_dir_all(&stage_dir).unwrap();
    for (name, bytes) in files {
        fs::write(stage_dir.join(name), bytes).unwrap();
    }
    SalvageStage {
        run_dir,
        stage_dir,
        session_seq: seq,
        part,
        segments: files.len(),
    }
}

/// Writes `n` one-second 160x120 fragmented segments as `part-000000.mp4`, `part-000001.mp4`,
/// ... with the native test generator (the same layout the recorder stages).
fn encode_segments(stage_dir: &Path, n: usize) {
    fs::create_dir_all(stage_dir).unwrap();
    let spec = MediaSpec::h264_aac(n as f64).with_video(160, 120, 15);
    let tmp = stage_dir.join("tmp");
    let segs = clipcheck_testkit::write_segments(&tmp, &spec, 1.0).expect("write test segments");
    assert_eq!(segs.len(), n, "expected {n} segments");
    for (i, seg) in segs.iter().enumerate() {
        fs::rename(seg, stage_dir.join(format!("part-{i:06}.mp4"))).unwrap();
    }
    fs::remove_dir_all(&tmp).unwrap();
}

fn no_recovered_files(sessions: &Path) -> bool {
    match fs::read_dir(sessions) {
        Ok(entries) => !entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("recovered-")),
        Err(_) => true,
    }
}

// ---- tests that need no encoder ----

#[test]
fn failing_recover_keeps_staged_part_and_writes_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let stage = make_stage(dir.path(), 1, 1, &[("part-000000.mp4", b"fake segment")]);
    let sessions = dir.path().join("sessions");
    let out = recovered_part_path(&sessions, &stage);

    let outcomes = salvage_stages(&sessions, vec![stage.clone()]);

    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        SalvageOutcome::Failed { stage: s, error } => {
            assert_eq!(s, &stage);
            assert!(!error.is_empty());
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(
        stage.stage_dir.join("part-000000.mp4").is_file(),
        "staged part must survive a failure"
    );
    assert!(!out.exists(), "no output may be written on failure");
    assert!(
        no_recovered_files(&sessions),
        "no recovered-* file may exist"
    );
}

#[test]
fn existing_output_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let stage = make_stage(dir.path(), 1, 1, &[("part-000000.mp4", b"fake segment")]);
    let sessions = dir.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let out = recovered_part_path(&sessions, &stage);
    fs::write(&out, b"user data").unwrap();

    let outcomes = salvage_stages(&sessions, vec![stage.clone()]);

    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        SalvageOutcome::Failed { error, .. } => {
            assert!(error.contains("output already exists"), "{error}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(fs::read(&out).unwrap(), b"user data");
    assert!(stage.stage_dir.join("part-000000.mp4").is_file());
}

#[test]
fn failing_stage_does_not_stop_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    // Stage A fails fast on a collision with its output name.
    let stage_a = make_stage(dir.path(), 1, 1, &[("part-000000.mp4", b"a")]);
    fs::write(recovered_part_path(&sessions, &stage_a), b"user data").unwrap();
    // Stage B has no output yet, so it is actually attempted (and fails on the fake bytes).
    let stage_b = make_stage(dir.path(), 1, 2, &[("part-000000.mp4", b"b")]);

    let outcomes = salvage_stages(&sessions, vec![stage_a.clone(), stage_b.clone()]);

    assert_eq!(outcomes.len(), 2, "both stages must yield an outcome");
    match &outcomes[0] {
        SalvageOutcome::Failed { stage, error } => {
            assert_eq!(stage, &stage_a);
            assert!(error.contains("output already exists"), "{error}");
        }
        other => panic!("stage A: expected Failed, got {other:?}"),
    }
    match &outcomes[1] {
        SalvageOutcome::Failed { stage, error } => {
            assert_eq!(stage, &stage_b);
            assert!(
                !error.contains("already exists"),
                "stage B was not attempted: {error}"
            );
        }
        other => panic!("stage B: expected Failed, got {other:?}"),
    }
    assert!(stage_a.stage_dir.join("part-000000.mp4").is_file());
    assert!(stage_b.stage_dir.join("part-000000.mp4").is_file());
    assert!(!recovered_part_path(&sessions, &stage_b).exists());
}

#[test]
fn discover_on_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(discover(&dir.path().join("does-not-exist")).is_empty());
}

#[test]
fn discover_finds_stages_and_ignores_other_entries() {
    let dir = tempfile::tempdir().unwrap();
    // A real stage in a run directory.
    make_stage(dir.path(), 2, 1, &[("part-000000.mp4", b"x")]);
    // A run directory with no stage directories.
    fs::create_dir_all(dir.path().join("run-20260101-000001").join("unrelated")).unwrap();
    // A stage directory with no part files.
    fs::create_dir_all(dir.path().join("run-20260101-000002").join("stage-s1-p1")).unwrap();
    // A stage-looking directory outside any run directory.
    let other = dir.path().join("other").join("stage-s1-p1");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("part-000000.mp4"), b"x").unwrap();

    let found = discover(dir.path());

    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].session_seq, 2);
    assert_eq!(found[0].part, 1);
    assert_eq!(found[0].segments, 1);
    assert!(found[0].stage_dir.ends_with("stage-s2-p1"));
}

#[test]
fn empty_input_gives_empty_output() {
    let dir = tempfile::tempdir().unwrap();
    assert!(salvage_stages(&dir.path().join("sessions"), Vec::new()).is_empty());
}

// ---- native test media tests ----

#[test]
fn salvage_concatenates_three_real_segments() {
    if !clipcheck_testkit::available() {
        skip!("native test media unavailable");
    }
    let dir = tempfile::tempdir().unwrap();
    let stage_dir = dir.path().join(RUN).join("stage-s1-p1");
    encode_segments(&stage_dir, 3);

    let stages = discover(dir.path());
    assert_eq!(stages.len(), 1, "found: {stages:?}");
    let stage = stages[0].clone();
    assert_eq!(stage.segments, 3);

    let sessions = dir.path().join("sessions");
    let expected_out = recovered_part_path(&sessions, &stage);
    assert!(expected_out.ends_with("recovered-run-20260101-000000-s1-p1.mp4"));

    let outcomes = salvage_stages(&sessions, vec![stage.clone()]);
    assert_eq!(outcomes.len(), 1);
    let part = match &outcomes[0] {
        SalvageOutcome::Recovered { stage: s, part } => {
            assert_eq!(s, &stage);
            part.clone()
        }
        other => panic!("expected Recovered, got {other:?}"),
    };

    assert_eq!(part.segments, 3);
    assert!(part.bytes > 0);
    assert_eq!(part.path, expected_out);
    assert!(expected_out.is_file());
    assert_eq!(fs::metadata(&expected_out).unwrap().len(), part.bytes);

    for i in 0..3 {
        assert!(
            !stage_dir.join(format!("part-{i:06}.mp4")).exists(),
            "staged segment {i} should be gone after success"
        );
    }
    assert!(!stage_dir.exists(), "empty stage dir should be removed");
    assert!(discover(dir.path()).is_empty(), "nothing left to salvage");

    let info = clipcheck_engine::catalog::probe_media(&expected_out).expect("probe recovered part");
    let d = info.duration_ms as f64 / 1000.0;
    assert!((d - 3.0).abs() <= 0.5, "duration {d} s, expected about 3 s");
}

#[test]
fn real_stage_recovers_and_fake_stage_fails_in_order() {
    if !clipcheck_testkit::available() {
        skip!("native test media unavailable");
    }
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join(RUN).join("stage-s1-p1");
    encode_segments(&real_dir, 2);
    let fake = make_stage(dir.path(), 1, 2, &[("part-000000.mp4", b"not a video")]);

    let stages = discover(dir.path());
    assert_eq!(stages.len(), 2, "found: {stages:?}");
    let sessions = dir.path().join("sessions");
    let outcomes = salvage_stages(&sessions, stages.clone());

    assert_eq!(outcomes.len(), 2);
    match &outcomes[0] {
        SalvageOutcome::Recovered { stage, part } => {
            assert_eq!(stage.part, 1);
            assert_eq!(part.segments, 2);
            assert!(part.path.is_file());
        }
        other => panic!("stage 1: expected Recovered, got {other:?}"),
    }
    match &outcomes[1] {
        SalvageOutcome::Failed { stage, error } => {
            assert_eq!(stage, &fake);
            assert!(!error.is_empty());
        }
        other => panic!("stage 2: expected Failed, got {other:?}"),
    }
    assert!(
        fake.stage_dir.join("part-000000.mp4").is_file(),
        "failed stage keeps its part"
    );
    assert!(
        !recovered_part_path(&sessions, &fake).exists(),
        "failed stage must not leave an output file"
    );
}
