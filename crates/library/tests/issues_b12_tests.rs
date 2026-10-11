//! Regression tests for library issues #28 (bad_enum column index), #30 (schema
//! version written once) and #110 (audio track storage API). Black-box: public API,
//! plus a separate rusqlite connection for the raw-SQL corruption and version checks.

use std::time::Duration;

use clipcheck_library::db::{AudioTrackRow, Library, NewRecording, RecStatus, RecordingKind};
use clipcheck_library::schema::{migrate, user_version, SCHEMA_VERSION};
use clipcheck_library::LibError;
use rusqlite::{Connection, Error as SqlError};
use tempfile::{tempdir, TempDir};

/// Zero-padded 26-char id so the ids are valid ULID-length strings.
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

/// A file-backed library in a fresh temp dir, with one storage root and one recording.
fn lib_with_recording() -> (TempDir, std::path::PathBuf, Library, i64) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("library.db");
    let lib = Library::open(&path).expect("open library");
    let root = lib
        .ensure_storage_root(r"C:\ClipCheck-test-root", 1)
        .expect("root");
    lib.insert_recording(&nrec(1, root)).expect("insert");
    (dir, path, lib, root)
}

fn raw(path: &std::path::Path) -> Connection {
    let c = Connection::open(path).expect("raw open");
    c.busy_timeout(Duration::from_secs(5))
        .expect("busy timeout");
    c
}

// ---------- #28: bad_enum reports the real column index ----------

/// Column index of `kind` in `ROW_SELECT` (rowid, id, kind, ...). Counted from the
/// select list in `db.rs`; `rowid` is index 0 and must not be reported.
const KIND_COL: usize = 2;
/// Column index of `status` in `ROW_SELECT`.
const STATUS_COL: usize = 17;

fn conversion_index(err: LibError) -> (usize, String) {
    match err {
        LibError::Db(SqlError::FromSqlConversionFailure(idx, _, msg)) => (idx, msg.to_string()),
        other => panic!("expected FromSqlConversionFailure, got {other:?}"),
    }
}

#[test]
fn issue_28_unknown_kind_reports_kind_column_not_rowid() {
    let (_dir, path, lib, _root) = lib_with_recording();
    let id = oid(1);
    {
        let c = raw(&path);
        // The CHECK on kind would refuse this value, so disable checks for this one
        // connection to simulate a corrupt row.
        c.execute_batch("PRAGMA ignore_check_constraints = ON;")
            .expect("pragma");
        let n = c
            .execute("UPDATE recordings SET kind = 'bogus' WHERE id = ?1", [&id])
            .expect("corrupt kind");
        assert_eq!(n, 1);
    }
    let err = lib.get_recording(&id).expect_err("corrupt kind must fail");
    let (idx, msg) = conversion_index(err);
    assert_eq!(idx, KIND_COL, "column index must be kind, not rowid (0)");
    assert_ne!(idx, 0, "rowid is column 0");
    assert!(msg.contains("unknown kind 'bogus'"), "message: {msg}");
}

#[test]
fn issue_28_unknown_status_reports_status_column() {
    let (_dir, path, lib, _root) = lib_with_recording();
    let id = oid(1);
    {
        let c = raw(&path);
        c.execute_batch("PRAGMA ignore_check_constraints = ON;")
            .expect("pragma");
        c.execute(
            "UPDATE recordings SET status = 'exploded' WHERE id = ?1",
            [&id],
        )
        .expect("corrupt status");
    }
    let err = lib
        .get_recording(&id)
        .expect_err("corrupt status must fail");
    let (idx, msg) = conversion_index(err);
    assert_eq!(idx, STATUS_COL, "column index must be status");
    assert!(msg.contains("unknown status 'exploded'"), "message: {msg}");
}

#[test]
fn issue_28_valid_row_still_reads() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let row = lib
        .get_recording(&oid(1))
        .expect("read")
        .expect("row exists");
    assert_eq!(row.kind, RecordingKind::Clip);
    assert_eq!(row.status, RecStatus::Ready);
}

// ---------- #30: schema version written once from SCHEMA_VERSION ----------

#[test]
fn issue_30_user_version_equals_schema_version_after_open() {
    let (_dir, path, _lib, _root) = lib_with_recording();
    let c = raw(&path);
    assert_eq!(user_version(&c).expect("user_version"), SCHEMA_VERSION);
}

#[test]
fn issue_30_migrate_sets_user_version_and_is_idempotent() {
    let c = Connection::open_in_memory().expect("mem");
    migrate(&c).expect("first migrate");
    assert_eq!(user_version(&c).expect("uv"), SCHEMA_VERSION);
    migrate(&c).expect("second migrate is a no-op");
    assert_eq!(user_version(&c).expect("uv"), SCHEMA_VERSION);
}

#[test]
fn issue_30_fts_built_version_marker_is_no_longer_written() {
    let (_dir, path, _lib, _root) = lib_with_recording();
    let c = raw(&path);
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM library_meta WHERE key = 'fts_built_version'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(n, 0, "unused fts_built_version marker must not be written");
}

// ---------- #110: audio track storage API ----------

fn track(index: u32, codec: &str, title: Option<&str>) -> AudioTrackRow {
    AudioTrackRow {
        track_index: index,
        codec: codec.to_string(),
        channels: 2,
        sample_rate: 48_000,
        bitrate_kbps: Some(192),
        language: Some("en".to_string()),
        title: title.map(str::to_string),
    }
}

#[test]
fn issue_110_audio_tracks_round_trip_in_track_order() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let id = oid(1);
    let written = vec![
        track(1, "opus", Some("Voice chat")),
        track(0, "aac", Some("Game")),
    ];
    lib.set_audio_tracks(&id, &written).expect("set");

    let read = lib.audio_tracks(&id).expect("get");
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].track_index, 0, "ordered by track index");
    assert_eq!(read[0], track(0, "aac", Some("Game")));
    assert_eq!(read[1], track(1, "opus", Some("Voice chat")));
}

#[test]
fn issue_110_optional_fields_round_trip_as_none() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let id = oid(1);
    let bare = AudioTrackRow {
        track_index: 0,
        codec: "aac".into(),
        channels: 1,
        sample_rate: 44_100,
        bitrate_kbps: None,
        language: None,
        title: None,
    };
    lib.set_audio_tracks(&id, std::slice::from_ref(&bare))
        .expect("set");
    let read = lib.audio_tracks(&id).expect("get");
    assert_eq!(read, vec![bare]);
}

#[test]
fn issue_110_set_replaces_previous_tracks() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let id = oid(1);
    lib.set_audio_tracks(&id, &[track(0, "aac", None), track(1, "aac", None)])
        .expect("first");
    lib.set_audio_tracks(&id, &[track(0, "opus", Some("Only"))])
        .expect("second");
    let read = lib.audio_tracks(&id).expect("get");
    assert_eq!(read, vec![track(0, "opus", Some("Only"))]);
}

#[test]
fn issue_110_empty_slice_clears_stored_tracks() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let id = oid(1);
    lib.set_audio_tracks(&id, &[track(0, "aac", None)])
        .expect("set");
    lib.set_audio_tracks(&id, &[]).expect("clear");
    assert!(lib.audio_tracks(&id).expect("get").is_empty());
}

#[test]
fn issue_110_recording_without_tracks_reads_empty() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    assert!(lib.audio_tracks(&oid(1)).expect("get").is_empty());
}

#[test]
fn issue_110_tracks_are_scoped_to_their_recording() {
    let (_dir, _path, lib, root) = lib_with_recording();
    lib.insert_recording(&nrec(2, root)).expect("insert second");
    lib.set_audio_tracks(&oid(1), &[track(0, "aac", None)])
        .expect("set first");
    assert!(lib.audio_tracks(&oid(2)).expect("get").is_empty());
}

#[test]
fn issue_110_set_for_unknown_recording_fails() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let err = lib.set_audio_tracks(&oid(999), &[track(0, "aac", None)]);
    assert!(err.is_err(), "unknown recording must be rejected");
}

#[test]
fn issue_110_tracks_removed_with_their_recording() {
    let (_dir, _path, lib, _root) = lib_with_recording();
    let id = oid(1);
    lib.set_audio_tracks(&id, &[track(0, "aac", None)])
        .expect("set");
    lib.delete_recording_record(&id).expect("delete");
    assert!(lib.audio_tracks(&id).expect("get").is_empty());
}
