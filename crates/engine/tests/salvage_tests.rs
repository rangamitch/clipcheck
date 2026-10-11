//! Tests for the public salvage API in `clipcheck_engine::sessionfile`.
//! Uses fake part files only; never encodes or probes media.

use std::fs;
use std::path::{Path, PathBuf};

use clipcheck_engine::sessionfile::{
    find_salvage_stages, recover, recovered_part_path, remove_empty_stage, SalvageStage,
};

fn fake_part(dir: &Path, n: u32) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("part-{n:06}.mp4")), b"fake-mp4-bytes").unwrap();
}

fn stage_with_part(work: &Path, run: &str, name: &str) -> PathBuf {
    let dir = work.join(run).join(name);
    fake_part(&dir, 1);
    dir
}

fn stage_struct(
    run_dir: &Path,
    stage_dir: &Path,
    seq: u64,
    part: u32,
    segments: usize,
) -> SalvageStage {
    SalvageStage {
        run_dir: run_dir.to_path_buf(),
        stage_dir: stage_dir.to_path_buf(),
        session_seq: seq,
        part,
        segments,
    }
}

fn snapshot(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            out.push(p.clone());
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn discovery_lists_two_runs_two_stages_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    // Insert out of order to prove sorting.
    stage_with_part(work, "run-b", "stage-s2-p1");
    stage_with_part(work, "run-a", "stage-s1-p2");
    stage_with_part(work, "run-a", "stage-s1-p1");
    stage_with_part(work, "run-b", "stage-s1-p1");

    let found = find_salvage_stages(work, None).unwrap();
    let keys: Vec<(String, u64, u32)> = found
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
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            ("run-a".to_string(), 1, 1),
            ("run-a".to_string(), 1, 2),
            ("run-b".to_string(), 1, 1),
            ("run-b".to_string(), 2, 1),
        ]
    );
    for s in &found {
        assert_eq!(s.segments, 1);
        assert_eq!(s.stage_dir.parent().unwrap(), s.run_dir);
    }
}

#[test]
fn current_run_is_excluded() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    stage_with_part(work, "run-live", "stage-s1-p1");
    stage_with_part(work, "run-old", "stage-s1-p1");

    let live = work.join("run-live");
    let found = find_salvage_stages(work, Some(&live)).unwrap();
    assert_eq!(found.len(), 1, "got {found:?}");
    assert!(found[0].run_dir.ends_with("run-old"));
}

#[test]
fn run_dir_without_stage_dirs_is_not_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    fs::create_dir_all(work.join("run-empty")).unwrap();
    fs::write(work.join("run-empty").join("unrelated.txt"), b"x").unwrap();
    stage_with_part(work, "run-real", "stage-s1-p1");

    let found = find_salvage_stages(work, None).unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].run_dir.ends_with("run-real"));
}

#[test]
fn stage_dir_with_no_part_files_is_not_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    let empty_stage = work.join("run-x").join("stage-s1-p1");
    fs::create_dir_all(&empty_stage).unwrap();
    fs::write(empty_stage.join("concat.txt"), b"file 'x'").unwrap();
    fs::write(empty_stage.join("notes.txt"), b"x").unwrap();

    let found = find_salvage_stages(work, None).unwrap();
    assert!(found.is_empty(), "got {found:?}");
}

#[test]
fn unparsable_stage_names_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    stage_with_part(work, "run-n", "stage-sX-p1");
    stage_with_part(work, "run-n", "stage-s1");
    stage_with_part(work, "run-n", "stage-s1-p1-extra");
    stage_with_part(work, "run-n", "stage-s1-p1"); // the only valid one

    let found = find_salvage_stages(work, None).unwrap();
    assert_eq!(found.len(), 1, "got {found:?}");
    assert_eq!(found[0].session_seq, 1);
    assert_eq!(found[0].part, 1);
    assert!(found[0].stage_dir.ends_with("stage-s1-p1"));
}

#[test]
fn plain_file_named_like_stage_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    let run = work.join("run-f");
    fs::create_dir_all(&run).unwrap();
    // A regular file, not a directory, that carries a valid stage name.
    fs::write(run.join("stage-s1-p1"), b"not a dir").unwrap();

    let found = find_salvage_stages(work, None).unwrap();
    assert!(found.is_empty(), "got {found:?}");
}

#[test]
fn non_run_dirs_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    stage_with_part(work, "sessions", "stage-s1-p1");
    stage_with_part(work, "other-run", "stage-s1-p1");
    stage_with_part(work, "run-ok", "stage-s1-p1");

    let found = find_salvage_stages(work, None).unwrap();
    assert_eq!(found.len(), 1, "got {found:?}");
    assert!(found[0].run_dir.ends_with("run-ok"));
}

#[test]
fn segments_count_matches_part_files() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    let stage = work.join("run-s").join("stage-s4-p2");
    fs::create_dir_all(&stage).unwrap();
    for n in 1..=3u32 {
        fs::write(stage.join(format!("part-{n:06}.mp4")), b"b").unwrap();
    }
    fs::write(stage.join("concat.txt"), b"x").unwrap();

    let found = find_salvage_stages(work, None).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].segments, 3);
    assert_eq!(found[0].session_seq, 4);
    assert_eq!(found[0].part, 2);
}

#[test]
fn find_does_not_modify_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path();
    stage_with_part(work, "run-a", "stage-s1-p1");
    stage_with_part(work, "run-a", "stage-s1-p2");
    fs::create_dir_all(work.join("run-empty")).unwrap();
    fs::write(
        work.join("run-a").join("stage-s1-p1").join("concat.txt"),
        b"c",
    )
    .unwrap();

    let before = snapshot(work);
    let _ = find_salvage_stages(work, Some(&work.join("run-a"))).unwrap();
    let _ = find_salvage_stages(work, None).unwrap();
    let after = snapshot(work);
    assert_eq!(before, after);
}

#[test]
fn missing_work_dir_is_empty_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let found = find_salvage_stages(&missing, None).unwrap();
    assert!(found.is_empty());
    assert!(!missing.exists());
}

#[test]
fn recovered_part_path_exact_string() {
    let stage = stage_struct(
        Path::new("/work/run-123"),
        Path::new("/work/run-123/stage-s2-p3"),
        2,
        3,
        1,
    );
    let sessions = Path::new("/sessions");
    let p = recovered_part_path(sessions, &stage);
    assert_eq!(p, sessions.join("recovered-run-123-s2-p3.mp4"));
    assert_eq!(
        p.file_name().unwrap().to_string_lossy(),
        "recovered-run-123-s2-p3.mp4"
    );
}

#[test]
fn remove_empty_stage_refuses_when_part_present() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = stage_with_part(tmp.path(), "run-r", "stage-s1-p1");
    let stage = stage_struct(&tmp.path().join("run-r"), &stage_dir, 1, 1, 1);

    assert!(!remove_empty_stage(&stage));
    assert!(stage_dir.is_dir());
    assert!(stage_dir.join("part-000001.mp4").is_file());
}

#[test]
fn remove_empty_stage_removes_empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("run-r").join("stage-s1-p1");
    fs::create_dir_all(&stage_dir).unwrap();
    let stage = stage_struct(&tmp.path().join("run-r"), &stage_dir, 1, 1, 0);

    assert!(remove_empty_stage(&stage));
    assert!(!stage_dir.exists());
}

#[test]
fn remove_empty_stage_removes_dir_with_only_concat() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("run-r").join("stage-s1-p1");
    fs::create_dir_all(&stage_dir).unwrap();
    fs::write(stage_dir.join("concat.txt"), b"file 'a'").unwrap();
    let stage = stage_struct(&tmp.path().join("run-r"), &stage_dir, 1, 1, 0);

    assert!(remove_empty_stage(&stage));
    assert!(!stage_dir.exists());
}

#[test]
fn remove_empty_stage_refuses_dir_with_extra_file() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("run-r").join("stage-s1-p1");
    fs::create_dir_all(&stage_dir).unwrap();
    fs::write(stage_dir.join("concat.txt"), b"file 'a'").unwrap();
    fs::write(stage_dir.join("keep-me.txt"), b"important").unwrap();
    let stage = stage_struct(&tmp.path().join("run-r"), &stage_dir, 1, 1, 0);

    assert!(!remove_empty_stage(&stage));
    assert!(stage_dir.is_dir());
    assert!(stage_dir.join("keep-me.txt").is_file());
}

#[test]
fn recover_missing_dir_returns_none_and_does_not_create() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("run-m").join("stage-s1-p1");
    let out = tmp.path().join("out.mp4");

    let res = recover(stage_dir.clone(), &out).unwrap();
    assert!(res.is_none());
    assert!(!stage_dir.exists());
    assert!(!out.exists());
}

#[test]
fn recover_empty_existing_dir_returns_none_and_keeps_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("run-e").join("stage-s1-p1");
    fs::create_dir_all(&stage_dir).unwrap();
    let out = tmp.path().join("out.mp4");

    let res = recover(stage_dir.clone(), &out).unwrap();
    assert!(res.is_none());
    assert!(stage_dir.is_dir());
    assert!(!out.exists());
}
