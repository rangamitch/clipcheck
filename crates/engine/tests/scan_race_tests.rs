//! Tests for the rescan race guard: a row absent from the walk is marked missing only when its
//! file is also gone from disk (`plan_scan_checked`, and `scan_root` which uses the disk check).

#[macro_use]
mod common;

use clipcheck_testkit::MediaSpec;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, MAIN_SEPARATOR};
use std::sync::atomic::AtomicBool;

use clipcheck_engine::scan::{
    apply_scan, plan_scan, plan_scan_checked, scan_root, FoundFile, RootTarget, ScanOptions,
    ScanPlan,
};
use clipcheck_library::{ExternalRecording, Library, PathRow, RecStatus, RecordingKind};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Writes a real, small H.264 mp4 above the 64 KiB default minimum. Generated natively by
/// `clipcheck-testkit`; the frame content is chosen so the file is over the minimum.
fn make_mp4(out: &Path) {
    fs::create_dir_all(out.parent().unwrap()).unwrap();
    let spec = MediaSpec::h264_aac(1.5).with_video(640, 360, 30);
    clipcheck_testkit::write_mp4(out, &spec).expect("testkit could not write test mp4");
    assert!(
        fs::metadata(out).unwrap().len() >= 65_536,
        "generated clip is below the 64 KiB minimum"
    );
}

/// Writes `n` bytes of zeros, creating parent folders.
fn dummy(path: &Path, n: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![0u8; n]).unwrap();
}

fn oid(n: usize) -> String {
    format!("{:026}", n)
}

fn ff(rel: &str, size: i64, mtime_ms: i64) -> FoundFile {
    FoundFile {
        rel_path: rel.to_owned(),
        size,
        mtime_ms,
    }
}

fn row(id: &str, rel: &str, size: i64, mtime_ms: i64, status: RecStatus) -> PathRow {
    (id.to_owned(), rel.to_owned(), size, mtime_ms, status)
}

fn sep(parts: &[&str]) -> String {
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

fn ext_rec(id: &str, root_id: i64, rel: &str, size: i64) -> ExternalRecording {
    ExternalRecording {
        id: id.to_owned(),
        kind: RecordingKind::Clip,
        game_id: None,
        root_id,
        rel_path: rel.to_owned(),
        file_name: rel.rsplit(MAIN_SEPARATOR).next().unwrap().to_owned(),
        ext: "mp4".to_owned(),
        size_bytes: size,
        file_mtime_ms: 5,
        created_at_ms: 5,
        added_at_ms: 5,
        duration_ms: 1000,
        width: Some(320),
        height: Some(240),
        fps: Some((30, 1)),
        video_codec: Some("h264".to_owned()),
        audio_track_count: 0,
        playable: true,
    }
}

fn status_of(lib: &Library, root_id: i64, rel: &str) -> RecStatus {
    lib.list_paths_for_root(root_id)
        .unwrap()
        .into_iter()
        .find(|r| r.1.eq_ignore_ascii_case(rel))
        .expect("row exists")
        .4
}

// ---------------------------------------------------------------------------
// 1. plan_scan_checked
// ---------------------------------------------------------------------------

#[test]
fn absent_row_that_exists_on_disk_is_not_missing() {
    let rows = [row("1", "a.mp4", 100, 5, RecStatus::Ready)];
    let plan = plan_scan_checked(&[], &rows, &|rel| rel == "a.mp4");
    assert_eq!(plan, ScanPlan::default());
}

#[test]
fn absent_row_that_does_not_exist_is_missing() {
    let rows = [row("1", "a.mp4", 100, 5, RecStatus::Ready)];
    let plan = plan_scan_checked(&[], &rows, &|_| false);
    assert_eq!(plan.missing, vec!["1".to_owned()]);
    assert!(plan.new.is_empty() && plan.changed.is_empty() && plan.unchanged == 0);
}

#[test]
fn present_rows_are_diffed_exactly_as_plan_scan_does() {
    let found = [
        ff("same.mp4", 100, 5),
        ff("resized.mp4", 101, 5),
        ff("brandnew.mp4", 7, 7),
    ];
    let rows = [
        row("1", "same.mp4", 100, 5, RecStatus::Ready),
        row("2", "resized.mp4", 100, 5, RecStatus::Ready),
    ];
    // exists() is true for everything: only absent rows may be affected, and none are absent
    let checked = plan_scan_checked(&found, &rows, &|_| true);
    let plain = plan_scan(&found, &rows);
    assert_eq!(checked, plain);
    assert!(checked.missing.is_empty());
    assert_eq!(checked.unchanged, 1);
    assert_eq!(checked.changed.len(), 1);
    assert_eq!(checked.new.len(), 1);
}

#[test]
fn mixed_fifty_row_set_reports_exactly_rows_absent_and_not_on_disk() {
    let n = 50usize;
    let rel = |i: usize| format!("Game{}{}Clip{i}.mp4", i % 5, MAIN_SEPARATOR);
    let rows: Vec<PathRow> = (0..n)
        .map(|i| row(&oid(i), &rel(i), 100, 5, RecStatus::Ready))
        .collect();
    // rows with i % 3 != 0 are still in the walk
    let found: Vec<FoundFile> = (0..n)
        .filter(|i| i % 3 != 0)
        .map(|i| ff(&rel(i), 100, 5))
        .collect();
    // absent rows (i % 3 == 0) exist on disk when i is even
    let on_disk = |r: &str| {
        (0..n)
            .filter(|i| i % 2 == 0 && i % 3 == 0)
            .any(|i| rel(i) == r)
    };

    let calls: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let exists = |r: &str| {
        calls.borrow_mut().push(r.to_owned());
        on_disk(r)
    };
    let plan = plan_scan_checked(&found, &rows, &exists);

    let expected_missing: Vec<String> = (0..n)
        .filter(|i| i % 3 == 0 && i % 2 == 1)
        .map(oid)
        .collect();
    assert_eq!(expected_missing.len(), 8);
    assert_eq!(plan.missing, expected_missing);
    assert_eq!(plan.unchanged, 33);
    assert!(plan.new.is_empty() && plan.changed.is_empty());

    // closure called once per absent, non-missing row, and never for a row in the walk
    let calls = calls.into_inner();
    let absent: Vec<String> = (0..n).filter(|i| i % 3 == 0).map(rel).collect();
    assert_eq!(calls.len(), absent.len(), "one call per absent row");
    for a in &absent {
        assert_eq!(
            calls.iter().filter(|c| *c == a).count(),
            1,
            "exists() called once for {a}"
        );
    }
}

#[test]
fn exists_is_not_called_for_rows_in_walk_or_already_missing_or_recording() {
    let calls = Cell::new(0usize);
    let exists = |_: &str| {
        calls.set(calls.get() + 1);
        false
    };
    let found = [ff("walked.mp4", 100, 5)];
    let rows = [
        row("1", "walked.mp4", 100, 5, RecStatus::Ready),
        row("2", "gone_already.mp4", 100, 5, RecStatus::Missing),
        row("3", "recording.mp4", 100, 5, RecStatus::Recording),
    ];
    let plan = plan_scan_checked(&found, &rows, &exists);
    assert_eq!(calls.get(), 0, "no disk check for these rows");
    assert_eq!(plan.unchanged, 1);
    assert!(plan.missing.is_empty());
}

#[test]
fn empty_inputs_give_empty_plan_without_disk_checks() {
    let plan = plan_scan_checked(&[], &[], &|_: &str| -> bool {
        panic!("exists must not be called with no rows")
    });
    assert_eq!(plan, ScanPlan::default());
}

#[test]
fn duplicate_found_entries_are_all_accounted_for() {
    let found = [ff("dup.mp4", 100, 5), ff("dup.mp4", 100, 5)];
    let plan = plan_scan_checked(&found, &[], &|_| false);
    assert_eq!(plan.new.len() + plan.changed.len() + plan.unchanged, 2);

    let rows = [row("1", "dup.mp4", 100, 5, RecStatus::Ready)];
    let plan = plan_scan_checked(&found, &rows, &|_| false);
    assert!(plan.missing.is_empty());
}

#[test]
fn unicode_and_spaces_paths_match_and_are_guarded() {
    let rel = "Café Clips\u{00f1}/🎬 my clip.mp4";
    let found = [ff(rel, 100, 5)];
    let rows = [
        row("1", rel, 100, 5, RecStatus::Ready),
        row(
            "2",
            "Caf\u{00e9} Clips/gone \u{1f3ac}.mp4",
            100,
            5,
            RecStatus::Ready,
        ),
    ];
    let plan = plan_scan_checked(&found, &rows, &|r| r.contains("gone"));
    assert_eq!(plan.unchanged, 1);
    assert!(plan.missing.is_empty(), "unicode row on disk stays ready");

    let plan = plan_scan_checked(&found, &rows, &|_| false);
    assert_eq!(plan.missing, vec!["2".to_owned()]);
}

#[test]
fn backslash_found_path_does_not_mark_slash_row_missing() {
    let slash = "Apex/clip.mp4".to_owned();
    let back = "Apex\\clip.mp4".to_owned();
    let rows = [row("1", &slash, 100, 5, RecStatus::Ready)];
    let plan = plan_scan_checked(&[ff(&back, 100, 5)], &rows, &|_| true);
    // exists() is consulted with the stored row path, so a row on disk is never marked missing
    assert!(plan.missing.is_empty());
}

// ---------------------------------------------------------------------------
// 2. plan_scan equivalence when exists() is always false
// ---------------------------------------------------------------------------

#[test]
fn plan_scan_checked_with_never_exists_equals_plan_scan() {
    let statuses = [
        RecStatus::Ready,
        RecStatus::Missing,
        RecStatus::Pending,
        RecStatus::Error,
        RecStatus::Recording,
    ];
    for case in 0..40usize {
        let found: Vec<FoundFile> = (0..(case % 7))
            .map(|i| ff(&format!("c{case}_{}.mp4", i * 2), 100 + i as i64, 5))
            .collect();
        let rows: Vec<PathRow> = (0..(case % 9))
            .map(|i| {
                row(
                    &oid(case * 100 + i),
                    &format!("c{case}_{i}.mp4"),
                    100,
                    5,
                    statuses[(i + case) % statuses.len()],
                )
            })
            .collect();
        let checked = plan_scan_checked(&found, &rows, &|_| false);
        let plain = plan_scan(&found, &rows);
        assert_eq!(format!("{checked:?}"), format!("{plain:?}"), "case {case}");
        assert_eq!(checked, plain, "case {case}");
    }
}

// ---------------------------------------------------------------------------
// 3. integration: apply the plan to a real library
// ---------------------------------------------------------------------------

#[test]
fn walk_miss_of_a_file_still_on_disk_keeps_row_ready_and_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    let rel = sep(&["Apex", "MedalTVRace.mp4"]);
    let file = root.join(&rel);
    dummy(&file, 200_000);

    let lib = Library::open_in_memory().unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    lib.write_external_batch(&[ext_rec(&oid(1), rid, &rel, 200_000)], &[])
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);
    let apply = |plan: &ScanPlan| {
        apply_scan(
            &lib,
            RootTarget {
                root_id: rid,
                root: &root,
            },
            plan,
            &opts,
            &mut |_| {},
            &cancel,
        )
        .unwrap()
    };

    // the walk omitted the file (it was registered after the listing), but it is on disk
    let existing = lib.list_paths_for_root(rid).unwrap();
    let plan = plan_scan_checked(&[], &existing, &|r| root.join(r).exists());
    assert!(plan.missing.is_empty(), "{plan:?}");
    let report = apply(&plan);
    assert_eq!(report.marked_missing, 0);
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Ready);

    // the file really goes away: the next scan marks it missing
    fs::remove_file(&file).unwrap();
    let existing = lib.list_paths_for_root(rid).unwrap();
    let plan = plan_scan_checked(&[], &existing, &|r| root.join(r).exists());
    assert_eq!(plan.missing, vec![oid(1)]);
    assert_eq!(apply(&plan).marked_missing, 1);
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Missing);

    // re-created: a real clip is probed, which needs a real mp4
    tools_or_skip!();
    make_mp4(&file);
    let walk = clipcheck_engine::scan::walk_root(&root, &opts, &cancel, &mut |_| {}).unwrap();
    let existing = lib.list_paths_for_root(rid).unwrap();
    let plan = plan_scan_checked(&walk.files, &existing, &|r| root.join(r).exists());
    assert_eq!(plan.changed.len(), 1, "{plan:?}");
    let report = apply_scan(
        &lib,
        RootTarget {
            root_id: rid,
            root: &root,
        },
        &plan,
        &opts,
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Ready);
}

#[test]
fn scan_root_keeps_row_for_file_on_disk_that_walk_skips_then_marks_missing() {
    // a 10-byte file is below min_bytes, so the walk never lists it, but it exists on disk
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    let rel = sep(&["Apex", "MedalTVSmall.mp4"]);
    let file = root.join(&rel);
    dummy(&file, 10);

    let lib = Library::open_in_memory().unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    lib.write_external_batch(&[ext_rec(&oid(2), rid, &rel, 10)], &[])
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);
    let scan =
        |lib: &Library| scan_root(lib, rid, &root, &opts, &cancel, &mut |_| {}).expect("scan_root");

    let r1 = scan(&lib);
    assert!(!r1.offline);
    assert_eq!(r1.marked_missing, 0, "{r1:?}");
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Ready);

    fs::remove_file(&file).unwrap();
    let r2 = scan(&lib);
    assert_eq!(r2.marked_missing, 1, "{r2:?}");
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Missing);
}

#[test]
fn scan_root_recovers_a_missing_row_when_the_file_returns() {
    tools_or_skip!();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    let rel = sep(&["Apex", "MedalTVBack.mp4"]);
    let file = root.join(&rel);
    make_mp4(&file);

    let lib = Library::open_in_memory().unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    let size = fs::metadata(&file).unwrap().len() as i64;
    lib.write_external_batch(&[ext_rec(&oid(3), rid, &rel, size)], &[])
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);
    let scan =
        |lib: &Library| scan_root(lib, rid, &root, &opts, &cancel, &mut |_| {}).expect("scan_root");

    fs::remove_file(&file).unwrap();
    let r1 = scan(&lib);
    assert_eq!(r1.marked_missing, 1, "{r1:?}");
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Missing);

    make_mp4(&file);
    let r2 = scan(&lib);
    assert!(r2.failed.is_empty(), "{:?}", r2.failed);
    assert_eq!(r2.marked_missing, 0, "{r2:?}");
    assert_eq!(status_of(&lib, rid, &rel), RecStatus::Ready);
}
