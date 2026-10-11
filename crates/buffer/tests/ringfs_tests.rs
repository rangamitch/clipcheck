use clipcheck_buffer::{RecoveryReport, SegmentDir, SegmentMeta};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Writes `data` as a committed segment `seq` and returns its final path.
fn write_committed(dir: &SegmentDir, seq: u64, data: &[u8]) -> PathBuf {
    let mut w = dir.open_segment(seq).expect("open_segment");
    w.write_all(data).expect("write");
    w.commit().expect("commit");
    dir.segment_path(seq)
}

fn meta_at(seq: u64, path: PathBuf) -> SegmentMeta {
    SegmentMeta {
        seq,
        start_100ns: 0,
        end_100ns: 0,
        first_keyframe_100ns: 0,
        bytes: 0,
        path,
        committed: true,
    }
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn sorted_names(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn segment_path_naming() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    assert_eq!(dir.segment_path(0).file_name().unwrap(), "seg-000000.mp4");
    assert_eq!(dir.segment_path(123).file_name().unwrap(), "seg-000123.mp4");
    assert_eq!(
        dir.segment_path(1234567).file_name().unwrap(),
        "seg-1234567.mp4"
    );
    assert_eq!(dir.segment_path(123).parent().unwrap(), tmp.path());
}

#[test]
fn new_does_not_touch_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("nested").join("segs");
    let dir = SegmentDir::new(&root);
    assert_eq!(dir.root(), root.as_path());
    assert!(!root.exists());
}

#[test]
fn open_segment_creates_root_and_part_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("a").join("b");
    assert!(!root.exists());
    let dir = SegmentDir::new(&root);
    let w = dir.open_segment(7).expect("open_segment");
    assert!(root.is_dir());
    assert!(root.join("seg-000007.mp4.part").is_file());
    assert!(!root.join("seg-000007.mp4").exists());
    assert_eq!(w.part_path(), root.join("seg-000007.mp4.part").as_path());
    drop(w);
}

#[test]
fn write_then_commit_produces_final_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let mut w = dir.open_segment(3).unwrap();
    w.write_all(b"hello").unwrap();
    w.write_all(b"world").unwrap();
    assert_eq!(w.bytes_written(), 10);
    let committed = w.commit().unwrap();
    assert_eq!(committed, 10);
    let final_path = tmp.path().join("seg-000003.mp4");
    assert_eq!(fs::read(&final_path).unwrap(), b"helloworld");
    assert!(!tmp.path().join("seg-000003.mp4.part").exists());
}

#[test]
fn abort_removes_part_and_creates_no_final() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let w = dir.open_segment(4).unwrap();
    w.abort().expect("abort should be Ok");
    assert!(!tmp.path().join("seg-000004.mp4.part").exists());
    assert!(!tmp.path().join("seg-000004.mp4").exists());
}

#[test]
fn abort_after_partial_write_leaves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let mut w = dir.open_segment(5).unwrap();
    w.write_all(&[1u8; 4096]).unwrap();
    w.abort().expect("abort should be Ok");
    assert!(sorted_names(tmp.path()).is_empty());
}

#[test]
fn dropped_writer_leaves_part_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    {
        let mut w = dir.open_segment(9).unwrap();
        w.write_all(b"partial-data").unwrap();
    }
    let part = tmp.path().join("seg-000009.mp4.part");
    assert_eq!(fs::read(&part).unwrap(), b"partial-data");
    assert!(!tmp.path().join("seg-000009.mp4").exists());
}

#[test]
fn open_segment_truncates_existing_part() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    {
        let mut w = dir.open_segment(6).unwrap();
        w.write_all(&[0xAAu8; 100]).unwrap();
    }
    let mut w = dir.open_segment(6).unwrap();
    w.write_all(b"xyz").unwrap();
    assert_eq!(w.commit().unwrap(), 3);
    let final_path = tmp.path().join("seg-000006.mp4");
    assert_eq!(fs::read(&final_path).unwrap(), b"xyz");
}

#[test]
fn commit_replaces_existing_final_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let first = write_committed(&dir, 2, b"old-contents-that-are-longer");
    assert_eq!(fs::read(&first).unwrap(), b"old-contents-that-are-longer");
    let mut w = dir.open_segment(2).unwrap();
    w.write_all(b"new").unwrap();
    assert_eq!(w.commit().unwrap(), 3);
    assert_eq!(fs::read(dir.segment_path(2)).unwrap(), b"new");
    assert!(!tmp.path().join("seg-000002.mp4.part").exists());
}

#[test]
fn delete_evicted_removes_files_and_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let p1 = write_committed(&dir, 1, b"a");
    let p2 = write_committed(&dir, 2, b"b");
    let keep = write_committed(&dir, 3, b"c");
    let evicted = vec![meta_at(1, p1.clone()), meta_at(2, p2.clone())];
    assert_eq!(dir.delete_evicted(&evicted).unwrap(), 2);
    assert!(!p1.exists());
    assert!(!p2.exists());
    assert!(keep.exists());
    assert_eq!(fs::read(&keep).unwrap(), b"c");
}

#[test]
fn delete_evicted_leaves_unrelated_files_and_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = SegmentDir::new(root);
    let p1 = write_committed(&dir, 1, b"a");
    fs::write(root.join("seg-000003.mp4.part"), b"c").unwrap();
    fs::write(root.join("notes.txt"), b"keep").unwrap();
    fs::create_dir(root.join("seg-000009.mp4")).unwrap();
    let evicted = vec![meta_at(1, p1.clone())];
    assert_eq!(dir.delete_evicted(&evicted).unwrap(), 1);
    assert!(!p1.exists());
    assert!(root.join("seg-000003.mp4.part").exists());
    assert_eq!(fs::read(root.join("notes.txt")).unwrap(), b"keep");
    assert!(root.join("seg-000009.mp4").is_dir());
}

#[test]
fn delete_evicted_ignores_already_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let p1 = write_committed(&dir, 1, b"a");
    let missing = tmp.path().join("seg-000042.mp4");
    let evicted = vec![meta_at(42, missing), meta_at(1, p1.clone())];
    assert_eq!(dir.delete_evicted(&evicted).unwrap(), 1);
    assert!(!p1.exists());
}

#[test]
fn delete_evicted_empty_slice_is_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let keep = write_committed(&dir, 1, b"a");
    assert_eq!(dir.delete_evicted(&[]).unwrap(), 0);
    assert!(keep.exists());
}

#[test]
fn delete_evicted_nonexistent_directory_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let bogus = tmp.path().join("no-such-dir").join("seg-000001.mp4");
    let evicted = vec![meta_at(1, bogus)];
    assert_eq!(dir.delete_evicted(&evicted).unwrap(), 0);
}

#[test]
fn scan_for_recovery_classifies_and_sorts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = SegmentDir::new(root);
    fs::write(root.join("seg-000010.mp4"), b"x").unwrap();
    fs::write(root.join("seg-000002.mp4"), b"x").unwrap();
    fs::write(root.join("seg-000005.mp4.part"), b"x").unwrap();
    fs::write(root.join("seg-000001.mp4.part"), b"x").unwrap();
    fs::write(root.join("notes.txt"), b"x").unwrap();
    fs::write(root.join("seg-abc.mp4"), b"x").unwrap();
    fs::write(root.join("seg-000003.mov"), b"x").unwrap();
    fs::write(root.join("seg-000004.mp4.tmp"), b"x").unwrap();
    fs::create_dir(root.join("seg-000009.mp4")).unwrap();

    let report = dir.scan_for_recovery().unwrap();
    assert_eq!(
        names(&report.complete),
        vec!["seg-000002.mp4", "seg-000010.mp4"]
    );
    assert_eq!(
        names(&report.partial),
        vec!["seg-000001.mp4.part", "seg-000005.mp4.part"]
    );
    assert_eq!(report.complete[0], root.join("seg-000002.mp4"));
    assert_eq!(report.partial[1], root.join("seg-000005.mp4.part"));
}

#[test]
fn scan_for_recovery_missing_directory_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path().join("missing"));
    let report = dir.scan_for_recovery().expect("missing root should be Ok");
    assert_eq!(report, RecoveryReport::default());
    assert!(report.complete.is_empty());
    assert!(report.partial.is_empty());
}

#[test]
fn scan_for_recovery_empty_directory_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let report = dir.scan_for_recovery().unwrap();
    assert_eq!(report, RecoveryReport::default());
}

#[test]
fn round_trip_recovery_after_commit_and_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let c1 = write_committed(&dir, 1, b"one");
    let c2 = write_committed(&dir, 2, b"two");
    {
        let mut w = dir.open_segment(3).unwrap();
        w.write_all(b"unfinished").unwrap();
    }
    let report = dir.scan_for_recovery().unwrap();
    assert_eq!(report.complete, vec![c1, c2]);
    assert_eq!(names(&report.partial), vec!["seg-000003.mp4.part"]);
}

#[test]
fn writer_accessors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentDir::new(tmp.path());
    let w = dir.open_segment(123).unwrap();
    assert_eq!(w.seq(), 123);
    assert_eq!(
        w.part_path(),
        tmp.path().join("seg-000123.mp4.part").as_path()
    );
    assert_eq!(w.bytes_written(), 0);
    w.abort().unwrap();
}

#[test]
fn file_in_place_of_root_does_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let file_root = tmp.path().join("plain-file");
    fs::write(&file_root, b"not a directory").unwrap();
    let dir = SegmentDir::new(&file_root);

    let open = dir.open_segment(1);
    assert!(open.is_err());

    // A file where the root should be is not a segment directory: the scan must
    // either report nothing or fail with NotADirectory (Windows os error 267);
    // it must never report recovered segments.
    match dir.scan_for_recovery() {
        Ok(report) => assert_eq!(
            report,
            RecoveryReport::default(),
            "scan of a file root must report nothing"
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::NotADirectory,
            "unexpected error kind for a file root: {e}"
        ),
    }
}
