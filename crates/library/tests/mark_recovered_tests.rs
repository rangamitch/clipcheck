//! Black-box tests for `Library::mark_recovered` (public API only).

use clipcheck_library::{LibError, Library, ListQuery, NewRecording, RecStatus, RecordingKind};
use tempfile::TempDir;

fn oid(n: u64) -> String {
    format!("{:026}", n)
}

fn nrec(n: u64, root: i64) -> NewRecording {
    NewRecording {
        id: oid(n),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id: root,
        rel_path: format!("u/{}/clip.mp4", oid(n)),
        file_name: "clip.mp4".to_string(),
        ext: "mp4".to_string(),
        size_bytes: 100,
        file_mtime_ms: 1,
        created_at_ms: 1_000 + n as i64,
        added_at_ms: 1,
        duration_ms: 0,
        status: RecStatus::Ready,
        error_detail: None,
        end_reason: None,
        recovered: false,
        degraded: false,
        dropped_packets: 0,
        gap_ms: 0,
        needs_faststart: false,
    }
}

fn setup() -> (TempDir, Library, i64) {
    let dir = TempDir::new().expect("tempdir");
    let lib = Library::open(dir.path().join("lib.db")).expect("open library");
    let root = lib.ensure_storage_root("C:\\Clips", 1).expect("root");
    (dir, lib, root)
}

#[test]
fn mark_recovered_sets_flag_and_end_reason() {
    let (_d, lib, root) = setup();
    lib.insert_recording(&nrec(1, root)).expect("insert");
    let before = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert!(!before.recovered);
    assert_eq!(before.end_reason, None);

    lib.mark_recovered(&oid(1)).expect("mark_recovered");

    let after = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert!(after.recovered, "recovered flag must be true");
    assert_eq!(after.end_reason.as_deref(), Some("recovered"));
    assert_eq!(after.id, oid(1));
}

#[test]
fn mark_recovered_leaves_other_recordings_untouched() {
    let (_d, lib, root) = setup();
    lib.insert_recording(&nrec(1, root)).expect("insert 1");
    lib.insert_recording(&nrec(2, root)).expect("insert 2");

    lib.mark_recovered(&oid(1)).expect("mark_recovered");

    let other = lib.get_recording(&oid(2)).expect("get").expect("row");
    assert!(!other.recovered, "other row must stay unrecovered");
    assert_eq!(
        other.end_reason, None,
        "other row end_reason must stay None"
    );
}

#[test]
fn mark_recovered_is_idempotent() {
    let (_d, lib, root) = setup();
    lib.insert_recording(&nrec(1, root)).expect("insert");
    lib.mark_recovered(&oid(1)).expect("first call");
    lib.mark_recovered(&oid(1)).expect("second call must be Ok");

    let row = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert!(row.recovered);
    assert_eq!(row.end_reason.as_deref(), Some("recovered"));
}

#[test]
fn mark_recovered_unknown_id_is_not_found() {
    let (_d, lib, root) = setup();
    lib.insert_recording(&nrec(1, root)).expect("insert");
    let err = lib
        .mark_recovered(&oid(999))
        .expect_err("unknown id must fail");
    assert!(matches!(err, LibError::NotFound), "got {err:?}");
    let untouched = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert!(!untouched.recovered);
}

#[test]
fn mark_recovered_empty_id_is_not_found() {
    let (_d, lib, root) = setup();
    lib.insert_recording(&nrec(1, root)).expect("insert");
    let err = lib.mark_recovered("").expect_err("empty id must fail");
    assert!(matches!(err, LibError::NotFound), "got {err:?}");
}

#[test]
fn recovered_row_still_appears_in_list() {
    let (_d, lib, root) = setup();
    lib.insert_recording(&nrec(1, root)).expect("insert 1");
    lib.insert_recording(&nrec(2, root)).expect("insert 2");
    lib.mark_recovered(&oid(1)).expect("mark_recovered");

    let page = lib.list(&ListQuery::default()).expect("list");
    assert_eq!(page.items.len(), 2, "recovered row must remain listed");
    let row = page
        .items
        .iter()
        .find(|r| r.id == oid(1))
        .expect("recovered row in list");
    assert!(row.recovered);
    assert_eq!(row.end_reason.as_deref(), Some("recovered"));
}
