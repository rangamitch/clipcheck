//! Integration tests for external library folder scanning (engine::scan).
//!
//! Tests that need real media generate it with clipcheck-testkit. If that is unavailable
//! the test skips (see common/mod.rs).

#[macro_use]
mod common;

use clipcheck_testkit::MediaSpec;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, MAIN_SEPARATOR};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use clipcheck_engine::catalog::probe_media;
use clipcheck_engine::scan::{
    derive_game_and_kind, generate_missing_thumbnails, plan_scan, scan_root, walk_root, FoundFile,
    ScanOptions, ScanPlan,
};
use clipcheck_library::{Library, ListQuery, PathRow, RecStatus, RecordingKind, RecordingRow};

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

fn set_mtime(path: &Path, t: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Creates a directory symlink where the platform allows it. Returns false if not permitted.
fn try_symlink_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
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

fn all_rows(lib: &Library) -> Vec<RecordingRow> {
    lib.list(&ListQuery::default()).expect("list").items
}

fn scan(
    lib: &Library,
    root_id: i64,
    root: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
) -> clipcheck_engine::scan::ScanReport {
    scan_root(lib, root_id, root, opts, cancel, &mut |_| {}).expect("scan_root")
}

// ---------------------------------------------------------------------------
// A. walk_root on a Medal-shaped tree
// ---------------------------------------------------------------------------

#[test]
fn a_walk_returns_only_real_clips_from_a_medal_shaped_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    let big = 200_000;

    // real clips (sized, not decoded: the walk does not look inside)
    dummy(&root.join("Apex").join("MedalTVApex2024-01-01.mp4"), big);
    dummy(&root.join("Apex").join("MedalTVApex2024-01-02.mp4"), big);
    dummy(&root.join("Valorant").join("MedalTVValorant2024.mp4"), big);
    dummy(&root.join("MedalTVRoot.mp4"), big);

    // junk that must be skipped
    dummy(&root.join("Thumbnails").join("x.mp4"), big); // ignored dir
    dummy(&root.join("Temp").join("y.mp4"), big); // ignored dir
    dummy(&root.join("Temp").join("MedalTVjunk.mp4"), big); // ignored dir
    dummy(&root.join("$RECYCLE.BIN").join("z.mp4"), big); // ignored dir (case-insensitive)
    dummy(&root.join("Apex").join("$R27ETFQ.mp4"), 200_000); // $r prefix
    dummy(&root.join("Apex").join("$R0OQDXM.jpg"), 200_000); // wrong extension
    dummy(&root.join("Apex").join("~MedalTVtemp.mp4"), big); // ~ prefix
    dummy(&root.join("Apex").join("notes.txt"), big); // wrong extension
    dummy(&root.join("Apex").join("small.mp4"), 10_000); // below min_bytes
    dummy(&root.join("Apex").join("MedalTVEmpty.mp4"), 0); // zero bytes

    // a folder link pointing outside the root must not be followed
    let outside = tmp.path().join("outside");
    dummy(&outside.join("MedalTVLinked.mp4"), big);
    let linked = try_symlink_dir(&outside, &root.join("LinkedGame"));
    if !linked {
        common::skip_or_fail(
            "directory symlinks not permitted; the linked-folder check did not run",
        );
    }

    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);
    let walk = walk_root(&root, &opts, &cancel, &mut |_| {}).expect("walk");

    let got: Vec<(String, i64)> = walk
        .files
        .iter()
        .map(|f| (f.rel_path.clone(), f.size))
        .collect();
    let mut expected = vec![
        (sep(&["Apex", "MedalTVApex2024-01-01.mp4"]), big as i64),
        (sep(&["Apex", "MedalTVApex2024-01-02.mp4"]), big as i64),
        ("MedalTVRoot.mp4".to_owned(), big as i64),
        (sep(&["Valorant", "MedalTVValorant2024.mp4"]), big as i64),
    ];
    expected.sort();
    assert_eq!(got, expected, "walk must return only the real clips");
    assert!(
        walk.files
            .windows(2)
            .all(|w| w[0].rel_path <= w[1].rel_path),
        "files must be sorted by rel_path"
    );
    assert!(
        walk.unreadable.is_empty(),
        "unreadable: {:?}",
        walk.unreadable
    );
    assert!(walk.files.iter().all(|f| f.mtime_ms > 0));
}

// ---------------------------------------------------------------------------
// B. walk_root errors: missing root and cancellation
// ---------------------------------------------------------------------------

#[test]
fn b_walk_missing_root_is_an_error_and_cancel_is_interrupted() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = ScanOptions::default();

    let not_cancelled = AtomicBool::new(false);
    assert!(walk_root(&tmp.path().join("nope"), &opts, &not_cancelled, &mut |_| {}).is_err());

    let root = tmp.path().join("Clips");
    dummy(&root.join("Apex").join("MedalTVA.mp4"), 200_000);
    let cancelled = AtomicBool::new(true);
    let err =
        walk_root(&root, &opts, &cancelled, &mut |_| {}).expect_err("cancelled walk must fail");
    assert_eq!(err.kind(), ErrorKind::Interrupted);
}

// ---------------------------------------------------------------------------
// C. walk progress callback
// ---------------------------------------------------------------------------

#[test]
fn c_walk_reports_progress_every_200_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("flat");
    fs::create_dir_all(&root).unwrap();
    for i in 0..450 {
        dummy(&root.join(format!("ok{i}.mp4")), 10);
    }
    let opts = ScanOptions {
        min_bytes: 0,
        ..ScanOptions::default()
    };
    let cancel = AtomicBool::new(false);
    let mut calls: Vec<usize> = Vec::new();
    let walk = walk_root(&root, &opts, &cancel, &mut |n| calls.push(n)).unwrap();

    assert_eq!(walk.files.len(), 450);
    assert!(calls.contains(&200), "progress calls: {calls:?}");
    assert!(calls.contains(&400), "progress calls: {calls:?}");
    assert_eq!(calls.last(), Some(&450), "final report is the total");
}

// ---------------------------------------------------------------------------
// D. plan_scan (pure)
// ---------------------------------------------------------------------------

#[test]
fn d01_empty_and_empty_is_empty_plan() {
    assert_eq!(plan_scan(&[], &[]), ScanPlan::default());
}

#[test]
fn d02_file_without_row_is_new() {
    let a = ff("a.mp4", 100, 5);
    let plan = plan_scan(std::slice::from_ref(&a), &[]);
    assert_eq!(plan.new, vec![a]);
    assert!(plan.changed.is_empty() && plan.missing.is_empty() && plan.unchanged == 0);
}

#[test]
fn d03_ready_row_with_same_size_and_mtime_is_unchanged() {
    let plan = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 100, 5, RecStatus::Ready)],
    );
    assert_eq!(
        plan,
        ScanPlan {
            unchanged: 1,
            ..ScanPlan::default()
        }
    );
}

#[test]
fn d04_ready_row_with_different_size_is_changed() {
    let plan = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 99, 5, RecStatus::Ready)],
    );
    assert_eq!(plan.changed, vec![("1".to_owned(), ff("a.mp4", 100, 5))]);
    assert_eq!(plan.unchanged, 0);
    assert!(plan.new.is_empty() && plan.missing.is_empty());
}

#[test]
fn d05_ready_row_with_different_mtime_is_changed() {
    let plan = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 100, 4, RecStatus::Ready)],
    );
    assert_eq!(plan.changed, vec![("1".to_owned(), ff("a.mp4", 100, 5))]);
    assert_eq!(plan.unchanged, 0);
}

#[test]
fn d06_ready_row_not_on_disk_is_missing() {
    let plan = plan_scan(&[], &[row("1", "a.mp4", 100, 5, RecStatus::Ready)]);
    assert_eq!(plan.missing, vec!["1".to_owned()]);
    assert!(plan.new.is_empty() && plan.changed.is_empty() && plan.unchanged == 0);
}

#[test]
fn d07_already_missing_row_not_on_disk_is_left_alone() {
    let plan = plan_scan(&[], &[row("1", "a.mp4", 100, 5, RecStatus::Missing)]);
    assert_eq!(plan, ScanPlan::default());
}

#[test]
fn d08_missing_row_back_on_disk_is_changed_even_with_same_size_and_mtime() {
    let plan = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 100, 5, RecStatus::Missing)],
    );
    assert_eq!(plan.changed, vec![("1".to_owned(), ff("a.mp4", 100, 5))]);
    assert_eq!(plan.unchanged, 0);
}

#[test]
fn d09_pending_row_not_on_disk_is_missing_and_on_disk_is_changed() {
    let gone = plan_scan(&[], &[row("1", "a.mp4", 100, 5, RecStatus::Pending)]);
    assert_eq!(gone.missing, vec!["1".to_owned()]);

    let back = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 100, 5, RecStatus::Pending)],
    );
    assert_eq!(back.changed.len(), 1);
    assert_eq!(back.unchanged, 0);
}

#[test]
fn d10_recording_row_not_on_disk_is_left_alone() {
    let plan = plan_scan(&[], &[row("1", "a.mp4", 100, 5, RecStatus::Recording)]);
    assert_eq!(plan, ScanPlan::default());
}

#[test]
fn d11_error_row_on_disk_is_changed() {
    let plan = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 100, 5, RecStatus::Error)],
    );
    assert_eq!(plan.changed, vec![("1".to_owned(), ff("a.mp4", 100, 5))]);
    assert_eq!(plan.unchanged, 0);
}

#[test]
fn d12_recording_row_on_disk_is_unchanged_whatever_the_size() {
    let plan = plan_scan(
        &[ff("a.mp4", 100, 5)],
        &[row("1", "a.mp4", 1, 1, RecStatus::Recording)],
    );
    assert_eq!(
        plan,
        ScanPlan {
            unchanged: 1,
            ..ScanPlan::default()
        }
    );
}

#[cfg(windows)]
#[test]
fn d13_path_match_is_case_insensitive_on_windows() {
    let found = [ff(&sep(&["Apex", "A.MP4"]), 100, 5)];
    let rows = [row("1", &sep(&["apex", "a.mp4"]), 100, 5, RecStatus::Ready)];
    let plan = plan_scan(&found, &rows);
    assert_eq!(
        plan,
        ScanPlan {
            unchanged: 1,
            ..ScanPlan::default()
        }
    );
}

#[cfg(not(windows))]
#[test]
fn d13_path_match_is_case_sensitive_off_windows() {
    let found = [ff("Apex/A.MP4", 100, 5)];
    let rows = [row("1", "apex/a.mp4", 100, 5, RecStatus::Ready)];
    let plan = plan_scan(&found, &rows);
    assert_eq!(plan.new.len(), 1);
    assert_eq!(plan.missing, vec!["1".to_owned()]);
}

// ---------------------------------------------------------------------------
// E. end-to-end scans (needs native test media)
// ---------------------------------------------------------------------------

#[test]
fn e_end_to_end_add_rescan_change_and_missing() {
    tools_or_skip!();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    let apex1 = root.join("Apex").join("MedalTVApex1.mp4");
    let apex2 = root.join("Apex").join("MedalTVApex2.mp4");
    let val1 = root.join("Valorant").join("MedalTVValorant1.mp4");
    make_mp4(&apex1);
    make_mp4(&apex2);
    make_mp4(&val1);
    // junk that must not be indexed
    dummy(&root.join("Thumbnails").join("x.mp4"), 200_000);
    dummy(&root.join("Apex").join("$R27ETFQ.mp4"), 200_000);

    let probe = probe_media(&apex1).expect("probe generated clip");
    assert!(probe.duration_ms > 0);
    assert_eq!(probe.video_codec, "h264");

    let lib = Library::open(tmp.path().join("lib.db")).unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);

    // first scan
    let r1 = scan(&lib, rid, &root, &opts, &cancel);
    assert_eq!(r1.added, 3, "{r1:?}");
    assert!(r1.failed.is_empty(), "failed: {:?}", r1.failed);
    assert!(!r1.offline && !r1.cancelled);
    assert_eq!(r1.marked_missing, 0);

    // second scan: nothing changed
    let r2 = scan(&lib, rid, &root, &opts, &cancel);
    assert_eq!(
        (r2.added, r2.updated, r2.unchanged, r2.marked_missing),
        (0, 0, 3, 0),
        "{r2:?}"
    );
    assert!(r2.failed.is_empty());

    // change: add one clip, delete one, touch one (bytes unchanged)
    fs::copy(&apex1, root.join("Apex").join("MedalTVApex3.mp4")).unwrap();
    fs::remove_file(&val1).unwrap();
    let later = fs::metadata(&apex2).unwrap().modified().unwrap() + Duration::from_secs(3600);
    set_mtime(&apex2, later);

    let r3 = scan(&lib, rid, &root, &opts, &cancel);
    assert_eq!(
        (r3.added, r3.updated, r3.marked_missing, r3.unchanged),
        (1, 1, 1, 1),
        "{r3:?}"
    );
    assert!(r3.failed.is_empty(), "failed: {:?}", r3.failed);

    // the library view
    let rows = all_rows(&lib);
    assert_eq!(rows.len(), 3, "missing rows are hidden from list()");
    for r in &rows {
        assert_eq!(r.kind, RecordingKind::Clip);
        assert_eq!(r.game_name.as_deref(), Some("Apex"));
        assert_eq!(r.root_id, rid);
    }
    let mut names: Vec<_> = rows.iter().map(|r| r.file_name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["MedalTVApex1.mp4", "MedalTVApex2.mp4", "MedalTVApex3.mp4"]
    );

    let paths = lib.list_paths_for_root(rid).unwrap();
    let valorant_rel = sep(&["Valorant", "MedalTVValorant1.mp4"]);
    let gone = paths
        .iter()
        .find(|r| r.1 == valorant_rel)
        .expect("deleted row kept");
    assert_eq!(gone.4, RecStatus::Missing);
    assert!(paths.iter().filter(|r| r.4 == RecStatus::Ready).count() == 3);
    assert!(lib.game_id_for_folder("Valorant").unwrap().is_some());

    // only source = 'external' rows that are ready are returned
    let external = lib.external_ready_files().unwrap();
    assert_eq!(external.len(), 3, "three ready external rows");
}

// ---------------------------------------------------------------------------
// F. probe failure is reported, writes nothing, and is retried
// ---------------------------------------------------------------------------

#[test]
fn f_probe_failure_is_reported_writes_nothing_and_is_retried() {
    tools_or_skip!();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    make_mp4(&root.join("Apex").join("MedalTVgood.mp4"));
    dummy(&root.join("Apex").join("MedalTVbroken.mp4"), 200_000);

    let lib = Library::open(tmp.path().join("lib.db")).unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);

    let r1 = scan(&lib, rid, &root, &opts, &cancel);
    assert_eq!(r1.added, 1, "{r1:?}");
    assert_eq!(r1.failed.len(), 1, "{:?}", r1.failed);
    assert!(
        r1.failed[0].0.ends_with("MedalTVbroken.mp4"),
        "{:?}",
        r1.failed
    );
    let names: Vec<_> = all_rows(&lib).into_iter().map(|r| r.file_name).collect();
    assert_eq!(
        names,
        vec!["MedalTVgood.mp4"],
        "the broken file wrote no row"
    );

    let r2 = scan(&lib, rid, &root, &opts, &cancel);
    assert_eq!(r2.added, 0, "{r2:?}");
    assert_eq!(r2.unchanged, 1);
    assert_eq!(
        r2.failed.len(),
        1,
        "broken file is listed again on the next scan"
    );
    assert!(r2.failed[0].0.ends_with("MedalTVbroken.mp4"));
    assert_eq!(all_rows(&lib).len(), 1);
}

// ---------------------------------------------------------------------------
// G. offline root: rows are kept, nothing marked missing
// ---------------------------------------------------------------------------

#[test]
fn g_root_that_disappears_is_offline_and_rows_are_not_marked_missing() {
    tools_or_skip!();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    make_mp4(&root.join("Apex").join("MedalTVApex1.mp4"));

    let lib = Library::open(tmp.path().join("lib.db")).unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);

    let r1 = scan(&lib, rid, &root, &opts, &cancel);
    assert_eq!(r1.added, 1, "{r1:?}");

    let away = tmp.path().join("Clips_away");
    fs::rename(&root, &away).unwrap();
    let r2 = scan(&lib, rid, &root, &opts, &cancel);
    // restore before asserting so a failure does not leave the folder renamed
    fs::rename(&away, &root).unwrap();

    assert!(r2.offline, "{r2:?}");
    assert_eq!(r2.marked_missing, 0);
    assert_eq!(r2.added, 0);
    assert!(!r2.cancelled);
    let info = lib.list_roots_with_stats().unwrap();
    assert_eq!(info[0].availability, "offline");
    let paths = lib.list_paths_for_root(rid).unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].4, RecStatus::Ready);
    assert_eq!(all_rows(&lib).len(), 1);

    // back online: the same file is unchanged and the root is online again
    let r3 = scan(&lib, rid, &root, &opts, &cancel);
    assert!(!r3.offline);
    assert_eq!(
        (r3.unchanged, r3.marked_missing, r3.added),
        (1, 0, 0),
        "{r3:?}"
    );
    assert_eq!(
        lib.list_roots_with_stats().unwrap()[0].availability,
        "online"
    );
}

// ---------------------------------------------------------------------------
// H. cancel before the scan: nothing added, nothing marked missing
// ---------------------------------------------------------------------------

#[test]
fn h_cancelled_scan_changes_nothing() {
    tools_or_skip!();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    let clip = root.join("Apex").join("MedalTVApex1.mp4");
    make_mp4(&clip);

    let lib = Library::open(tmp.path().join("lib.db")).unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    let opts = ScanOptions::default();
    let running = AtomicBool::new(false);
    assert_eq!(scan(&lib, rid, &root, &opts, &running).added, 1);

    // the file disappears, and the scan is cancelled before it starts: no row may go missing
    fs::remove_file(&clip).unwrap();
    let cancel = AtomicBool::new(true);
    let r = scan(&lib, rid, &root, &opts, &cancel);
    assert!(r.cancelled, "{r:?}");
    assert_eq!((r.added, r.updated, r.marked_missing), (0, 0, 0), "{r:?}");
    assert_eq!(lib.list_paths_for_root(rid).unwrap()[0].4, RecStatus::Ready);
    assert_eq!(all_rows(&lib).len(), 1);
}

// ---------------------------------------------------------------------------
// I. derive_game_and_kind table
// ---------------------------------------------------------------------------

#[test]
fn i_derive_game_and_kind_table() {
    let cases: &[(&str, &str, &str, RecordingKind)] = &[
        ("Clips", "Apex\\a.mp4", "Apex", RecordingKind::Clip),
        ("Clips", "a.mp4", "Unsorted", RecordingKind::Clip),
        ("Medal", "Clips\\Apex\\a.mp4", "Apex", RecordingKind::Clip),
        (
            "Medal",
            "Sessions\\Apex\\Part1.mp4",
            "Apex",
            RecordingKind::Session,
        ),
        (
            "Clips",
            "Sessions\\Apex\\x.mp4",
            "Apex",
            RecordingKind::Session,
        ),
    ];
    for (root_name, rel, game, kind) in cases {
        let (g, k) = derive_game_and_kind(root_name, rel);
        assert_eq!(
            (g.as_str(), k),
            (*game, *kind),
            "root={root_name} rel={rel}"
        );
    }
}

// ---------------------------------------------------------------------------
// J. plan_scan performance smoke test
// ---------------------------------------------------------------------------

#[test]
fn j_plan_scan_with_20k_rows_is_fast_enough() {
    let n = 20_000usize;
    let found: Vec<FoundFile> = (0..n)
        .map(|i| {
            ff(
                &format!("Game{}{}MedalTV{i}.mp4", i % 50, MAIN_SEPARATOR),
                100_000,
                1_000,
            )
        })
        .collect();
    // half the rows match a found file, half point at files that are gone
    let existing: Vec<PathRow> = (0..n)
        .map(|i| {
            let rel = if i < n / 2 {
                found[i].rel_path.clone()
            } else {
                format!("Old{}MedalTV{i}.mp4", MAIN_SEPARATOR)
            };
            row(&oid(i), &rel, 100_000, 1_000, RecStatus::Ready)
        })
        .collect();

    let started = Instant::now();
    let plan = plan_scan(&found, &existing);
    let elapsed = started.elapsed();

    assert_eq!(plan.unchanged, n / 2);
    assert_eq!(plan.new.len(), n / 2);
    assert_eq!(plan.missing.len(), n / 2);
    assert!(
        elapsed < Duration::from_millis(2_000),
        "plan_scan took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// K. generate_missing_thumbnails
// ---------------------------------------------------------------------------

#[test]
fn k_thumbnails_respect_the_attempt_limit_and_skip_existing() {
    tools_or_skip!();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Clips");
    make_mp4(&root.join("Apex").join("MedalTVApex1.mp4"));
    make_mp4(&root.join("Apex").join("MedalTVApex2.mp4"));
    make_mp4(&root.join("Valorant").join("MedalTVValorant1.mp4"));

    let lib = Library::open(tmp.path().join("lib.db")).unwrap();
    let rid = lib
        .upsert_root(&root.display().to_string(), "Clips", false)
        .unwrap();
    let opts = ScanOptions::default();
    let cancel = AtomicBool::new(false);
    assert_eq!(scan(&lib, rid, &root, &opts, &cancel).added, 3);

    let ids: Vec<String> = all_rows(&lib).into_iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 3);
    let thumbs = tmp.path().join("thumbs");
    let posters = |ids: &[String]| {
        ids.iter()
            .filter(|id| thumbs.join(id.as_str()).join("poster.jpg").is_file())
            .count()
    };

    let first = generate_missing_thumbnails(&lib, &thumbs, 2).unwrap();
    assert_eq!(first, 2);
    assert_eq!(posters(&ids), 2);

    let second = generate_missing_thumbnails(&lib, &thumbs, 10).unwrap();
    assert_eq!(second, 1, "only the remaining row needs a poster");
    assert_eq!(posters(&ids), 3);

    let third = generate_missing_thumbnails(&lib, &thumbs, 10).unwrap();
    assert_eq!(third, 0);
}
