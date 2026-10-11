//! Issue #105: `register_recording` must return the existing id for a
//! (root_id, rel_path) row in any status, without probing the file or inserting
//! a second row.
//!
//! `register_recording` canonicalizes the file before the row lookup, so the file
//! has to exist on disk for the lookup to be reached. The seeded `rel_path` uses
//! the platform separator, the same form the lookup builds.

use std::path::{Path, PathBuf};

use clipcheck_engine::catalog::{register_recording, CatalogError, RegisterMeta};
use clipcheck_library::db::{NewRecording, RecordingSource};
use clipcheck_library::{Library, RecStatus, RecordingKind};

fn meta() -> RegisterMeta<'static> {
    RegisterMeta {
        kind: RecordingKind::Clip,
        game: "exe:test.exe",
        session_id: None,
        part_index: None,
        created_ms: 1_000,
    }
}

/// Platform-separated form of a `/`-separated relative path.
fn native(rel: &str) -> String {
    PathBuf::from(rel)
        .iter()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

/// Creates a file at `root/rel`, with its parent directories.
fn touch(root: &Path, rel: &str) {
    let file = root.join(rel);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, b"x").expect("write");
}

fn new_row(id: &str, root_id: i64, rel_path: &str, status: RecStatus) -> NewRecording {
    let native_rel = native(rel_path);
    NewRecording {
        id: id.to_string(),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id,
        file_name: native_rel
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_string(),
        rel_path: native_rel,
        ext: "mp4".to_string(),
        size_bytes: 1024,
        file_mtime_ms: 1_000,
        created_at_ms: 1_000,
        added_at_ms: 1_000,
        duration_ms: 5_000,
        status,
        error_detail: None,
        end_reason: None,
        recovered: false,
        degraded: false,
        dropped_packets: 0,
        gap_ms: 0,
        needs_faststart: false,
    }
}

fn root_id_of(lib: &Library, root: &Path) -> i64 {
    lib.ensure_storage_root(&root.to_string_lossy(), 1_000)
        .expect("ensure_storage_root")
}

/// Inserts a row for `rel` under `root` in `status`, with the file on disk.
fn seed(lib: &Library, root: &Path, id: &str, rel: &str, status: RecStatus) {
    touch(root, rel);
    let root_id = root_id_of(lib, root);
    lib.insert_recording_sourced(
        &new_row(id, root_id, rel, status),
        RecordingSource::Recorded,
        true,
    )
    .expect("insert_recording_sourced");
}

const ID_RECORDING: &str = "01HZ0000000000000000000R01";
const ID_PENDING: &str = "01HZ0000000000000000000P01";
const ID_READY: &str = "01HZ0000000000000000000Y01";
const ID_MISSING: &str = "01HZ0000000000000000000M01";
const ID_ERROR: &str = "01HZ0000000000000000000E01";

/// Registers `root/rel` with the bogus tools.
fn register(lib: &Library, root: &Path, rel: &str) -> Result<String, CatalogError> {
    register_recording(lib, &root.join("thumbs"), root, &root.join(rel), meta())
}

/// Asserts that registering `rel` returns `expected` and leaves exactly one row for it.
fn assert_returns_existing(lib: &Library, root: &Path, rel: &str, expected: &str, label: &str) {
    let got = register(lib, root, rel);
    assert_eq!(
        got.as_ref().ok().map(String::as_str),
        Some(expected),
        "{label}: got {got:?}"
    );
    let rows = lib
        .list_paths_for_root(root_id_of(lib, root))
        .expect("list_paths_for_root");
    let matching = rows.iter().filter(|r| r.1 == native(rel)).count();
    assert_eq!(matching, 1, "{label}: rows for {rel} after register");
}

#[test]
fn existing_recording_status_returns_existing_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(
        &lib,
        root.path(),
        ID_RECORDING,
        "game/a.mp4",
        RecStatus::Recording,
    );
    assert_returns_existing(&lib, root.path(), "game/a.mp4", ID_RECORDING, "recording");
}

#[test]
fn existing_pending_status_returns_existing_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(
        &lib,
        root.path(),
        ID_PENDING,
        "game/b.mp4",
        RecStatus::Pending,
    );
    assert_returns_existing(&lib, root.path(), "game/b.mp4", ID_PENDING, "pending");
}

#[test]
fn existing_ready_status_returns_existing_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(&lib, root.path(), ID_READY, "game/c.mp4", RecStatus::Ready);
    assert_returns_existing(&lib, root.path(), "game/c.mp4", ID_READY, "ready");
}

#[test]
fn existing_missing_status_returns_existing_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(
        &lib,
        root.path(),
        ID_MISSING,
        "game/d.mp4",
        RecStatus::Ready,
    );
    let n = lib
        .mark_missing(root_id_of(&lib, root.path()), &[ID_MISSING.to_string()])
        .expect("mark_missing");
    assert_eq!(n, 1, "mark_missing should flip the row");
    assert_returns_existing(&lib, root.path(), "game/d.mp4", ID_MISSING, "missing");
}

/// A `missing` row (issue #105 scenario): while the file is gone nothing can be registered
/// (the path cannot be canonicalized: clean error, row untouched); once the file is back the
/// existing row's id is returned instead of a UNIQUE(root_id, rel_path) failure.
#[test]
fn missing_row_registers_to_existing_id_once_file_returns() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(
        &lib,
        root.path(),
        ID_MISSING,
        "game/gone.mp4",
        RecStatus::Ready,
    );
    lib.mark_missing(root_id_of(&lib, root.path()), &[ID_MISSING.to_string()])
        .expect("mark_missing");
    std::fs::remove_file(root.path().join("game").join("gone.mp4")).expect("remove file");
    assert!(
        register(&lib, root.path(), "game/gone.mp4").is_err(),
        "a file that does not exist cannot be registered"
    );
    touch(root.path(), "game/gone.mp4");
    let got = register(&lib, root.path(), "game/gone.mp4");
    assert_eq!(
        got.as_ref().ok().map(String::as_str),
        Some(ID_MISSING),
        "got {got:?}"
    );
}

#[test]
fn existing_error_status_returns_existing_id() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(&lib, root.path(), ID_ERROR, "game/e.mp4", RecStatus::Error);
    assert_returns_existing(&lib, root.path(), "game/e.mp4", ID_ERROR, "error");
}

#[test]
fn existing_row_is_not_probed() {
    // A probe of this fake file would fail. Success proves the lookup ran first.
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    seed(&lib, root.path(), ID_ERROR, "game/f.mp4", RecStatus::Error);
    let got = register(&lib, root.path(), "game/f.mp4");
    assert!(matches!(got, Ok(ref id) if id == ID_ERROR), "got {got:?}");
}

#[test]
fn fresh_path_goes_through_probe_and_errors_cleanly() {
    let root = tempfile::tempdir().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    touch(root.path(), "game/new.mp4");
    match register(&lib, root.path(), "game/new.mp4") {
        Err(CatalogError::Io(_)) => {}
        other => panic!("expected Io error from unprobeable file, got {other:?}"),
    }
    let rows = lib
        .list_paths_for_root(root_id_of(&lib, root.path()))
        .expect("list_paths_for_root");
    assert!(
        rows.iter().all(|r| r.1 != native("game/new.mp4")),
        "failed probe must not leave a row behind"
    );
}

#[test]
fn same_rel_path_under_two_roots_is_distinct() {
    let root_a = tempfile::tempdir().expect("tempdir a");
    let root_b = tempfile::tempdir().expect("tempdir b");
    let lib = Library::open_in_memory().expect("open");
    seed(
        &lib,
        root_a.path(),
        ID_READY,
        "game/same.mp4",
        RecStatus::Ready,
    );
    touch(root_b.path(), "game/same.mp4");

    let id_a = lib
        .find_by_rel_path(root_id_of(&lib, root_a.path()), &native("game/same.mp4"))
        .expect("find a");
    let id_b = lib
        .find_by_rel_path(root_id_of(&lib, root_b.path()), &native("game/same.mp4"))
        .expect("find b");
    assert_eq!(id_a.as_deref(), Some(ID_READY), "root A row");
    assert_eq!(id_b, None, "root B must not see root A row");

    // Root B's registration must not return root A's id. It falls through to the probe.
    let got = register(&lib, root_b.path(), "game/same.mp4");
    assert!(
        !matches!(got, Ok(ref id) if id == ID_READY),
        "root B returned root A id: {got:?}"
    );
}
