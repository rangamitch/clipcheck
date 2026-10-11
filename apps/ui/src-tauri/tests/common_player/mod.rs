//! Shared helpers for the player/media tests. Test-only; never used by src/.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use clipcheck_library::{Library, NewRecording, RecStatus, RecordingKind};

/// Fixed "now" used for every fixture row.
pub const NOW_MS: i64 = 1_700_000_000_000;

/// One recording row to insert. `rel_path` is relative to the storage root.
pub struct Row<'a> {
    pub id: &'a str,
    pub rel_path: &'a str,
    pub ext: &'a str,
    pub duration_ms: i64,
}

/// Modification time of `p` in UTC milliseconds (fs metadata, as the spec's sidecar key uses).
pub fn mtime_ms(p: &Path) -> i64 {
    let m = std::fs::metadata(p).expect("metadata");
    let d = m
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch");
    i64::try_from(d.as_millis()).expect("mtime fits i64")
}

/// Creates a library DB at `db` with storage root `root` and the given rows. Returns `db`.
/// Rows whose file exists get the real size and mtime; missing files get 0 / NOW_MS.
pub fn make_db(db: &Path, root: &Path, rows: &[Row<'_>]) -> PathBuf {
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).expect("create db parent");
    }
    let lib = Library::open(db).expect("open library");
    let root_id = lib
        .ensure_storage_root(&root.to_string_lossy(), NOW_MS)
        .expect("ensure storage root");
    for r in rows {
        let file = root.join(r.rel_path);
        let (size, mtime) = match std::fs::metadata(&file) {
            Ok(m) => (
                i64::try_from(m.len()).expect("len fits i64"),
                mtime_ms(&file),
            ),
            Err(_) => (0, NOW_MS),
        };
        let file_name = file
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        lib.insert_recording(&NewRecording {
            id: r.id.to_owned(),
            kind: RecordingKind::Clip,
            session_id: None,
            part_index: None,
            game_id: None,
            root_id,
            rel_path: r.rel_path.to_owned(),
            file_name,
            ext: r.ext.to_owned(),
            size_bytes: size,
            file_mtime_ms: mtime,
            created_at_ms: NOW_MS,
            added_at_ms: NOW_MS,
            duration_ms: r.duration_ms,
            status: RecStatus::Ready,
            error_detail: None,
            end_reason: None,
            recovered: false,
            degraded: false,
            dropped_packets: 0,
            gap_ms: 0,
            needs_faststart: false,
        })
        .expect("insert recording");
    }
    db.to_path_buf()
}

/// Deterministic byte pattern of length `len`.
pub fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Case-insensitive header lookup.
pub fn hdr(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// Unwraps a Result, panicking with `what` and the error's Display text.
pub fn must<T, E: std::fmt::Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}
