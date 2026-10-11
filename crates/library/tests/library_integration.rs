//! Integration tests for the public API of clipcheck-library.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clipcheck_library::schema::{self, SCHEMA_VERSION};
use clipcheck_library::{
    LibError, Library, ListQuery, NewGame, NewRecording, Page, RecStatus, RecordingKind,
    RecordingRow, SortKey,
};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A 26-character id derived from a number (zero-padded so string order == numeric order).
fn oid(n: u64) -> String {
    format!("{:026}", n)
}

/// A recording with all fields filled in. `rel_path` embeds the id so it is unique.
fn rec(n: u64, root: i64, name: &str) -> NewRecording {
    NewRecording {
        id: oid(n),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id: root,
        rel_path: format!("u/{}/{}", oid(n), name),
        file_name: name.to_string(),
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

fn lib_with_root() -> (Library, i64) {
    let lib = Library::open_in_memory().expect("open_in_memory");
    let root = lib.ensure_storage_root("C:\\Clips", 1).expect("root");
    (lib, root)
}

fn ids(rows: &[RecordingRow]) -> Vec<String> {
    rows.iter().map(|r| r.id.clone()).collect()
}

fn sorted_ids(rows: &[RecordingRow]) -> Vec<String> {
    let mut v = ids(rows);
    v.sort();
    v
}

/// Walks every page of a query, asserting each page respects the (clamped) limit.
fn walk(lib: &Library, mut q: ListQuery) -> Vec<RecordingRow> {
    let limit = q.limit.clamp(1, 200) as usize;
    let mut all = Vec::new();
    let mut pages = 0usize;
    loop {
        let page = lib.list(&q).expect("list page");
        assert!(
            page.items.len() <= limit,
            "page of {} exceeds limit {}",
            page.items.len(),
            limit
        );
        all.extend(page.items);
        pages += 1;
        assert!(pages < 100_000, "pagination did not terminate");
        match page.next_cursor {
            Some(c) => q.cursor = Some(c),
            None => break,
        }
    }
    all
}

fn first_page(lib: &Library, q: ListQuery) -> Page {
    lib.list(&q).expect("list")
}

/// Temporary directory removed on drop. Declare it before any Library that uses it so the
/// library is dropped first (required on Windows).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "clipcheck_lib_it_{tag}_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// 1-4: open / WAL / readonly / schema version
// ---------------------------------------------------------------------------

#[test]
fn open_creates_file_db_and_reopen_does_not_remigrate() {
    let dir = TempDir::new("reopen");
    let path = dir.file("lib.db");

    let lib = Library::open(&path).expect("first open");
    assert!(path.exists(), "open() should create the database file");
    drop(lib);

    let created_at = {
        let conn = Connection::open(&path).expect("raw open");
        assert_eq!(schema::user_version(&conn).expect("uv"), 2);
        conn.query_row(
            "SELECT value FROM library_meta WHERE key = 'schema_created_at_ms'",
            [],
            |r| r.get::<_, String>(0),
        )
        .expect("meta")
    };

    let lib2 = Library::open(&path).expect("reopen");
    drop(lib2);

    let conn = Connection::open(&path).expect("raw open 2");
    assert_eq!(schema::user_version(&conn).expect("uv"), SCHEMA_VERSION);
    let created_at_again: String = conn
        .query_row(
            "SELECT value FROM library_meta WHERE key = 'schema_created_at_ms'",
            [],
            |r| r.get(0),
        )
        .expect("meta again");
    assert_eq!(
        created_at, created_at_again,
        "schema_created_at_ms changed, so the schema was re-migrated on reopen"
    );
}

#[test]
fn file_db_is_wal_mode() {
    let dir = TempDir::new("wal");
    let path = dir.file("lib.db");
    let _lib = Library::open(&path).expect("open");

    // journal_mode=WAL is persistent in the file, so a separate connection sees it.
    let conn = Connection::open(&path).expect("raw open");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    assert_eq!(mode.to_lowercase(), "wal");
    // Note: PRAGMA foreign_keys is per-connection, so a fresh raw connection reports 0 even
    // when the library enabled it. FK enforcement is checked behaviourally in
    // `foreign_keys_are_enforced` instead.
}

#[test]
fn open_readonly_reads_rows_written_by_writer() {
    let dir = TempDir::new("ro");
    let path = dir.file("lib.db");
    let writer = Library::open(&path).expect("writer");
    let root = writer.ensure_storage_root("C:\\Clips", 1).expect("root");
    writer
        .insert_recording(&rec(1, root, "one.mp4"))
        .expect("insert");

    let ro = Library::open_readonly(&path).expect("open_readonly");
    let row = ro
        .get_recording(&oid(1))
        .expect("get")
        .expect("row present");
    assert_eq!(row.file_name, "one.mp4");
    assert_eq!(row.root_path, "C:\\Clips");
    let page = ro.list(&ListQuery::default()).expect("list");
    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_none());
    drop(ro);
    drop(writer);
}

#[test]
fn open_readonly_missing_file_is_error_and_creates_nothing() {
    let dir = TempDir::new("ro_missing");
    let path = dir.file("nope.db");
    assert!(Library::open_readonly(&path).is_err());
    assert!(!path.exists(), "open_readonly must not create the file");
}

#[test]
fn open_readonly_rejects_schema_version_mismatch() {
    let dir = TempDir::new("ro_version");
    let path = dir.file("lib.db");
    drop(Library::open(&path).expect("open"));
    {
        let conn = Connection::open(&path).expect("raw");
        conn.execute_batch("PRAGMA user_version = 99;")
            .expect("set uv");
    }
    let res = Library::open_readonly(&path);
    assert!(
        matches!(res, Err(LibError::Invalid(_))),
        "got {:?}",
        res.err()
    );
}

#[test]
fn open_in_memory_schema_version_is_two() {
    // Library does not expose its connection, so verify the same migration path on a raw
    // in-memory connection, and check open_in_memory itself succeeds.
    Library::open_in_memory().expect("open_in_memory");
    let conn = Connection::open_in_memory().expect("raw mem");
    schema::migrate(&conn).expect("migrate");
    assert_eq!(schema::user_version(&conn).expect("uv"), 2);
    assert_eq!(SCHEMA_VERSION, 2);
}

// ---------------------------------------------------------------------------
// 5-7: insert validation, foreign keys, uniqueness
// ---------------------------------------------------------------------------

#[test]
fn foreign_keys_are_enforced() {
    let (lib, _root) = lib_with_root();
    let res = lib.insert_recording(&rec(1, 9999, "orphan.mp4"));
    assert!(matches!(res, Err(LibError::Db(_))), "bad root_id: {res:?}");

    let (lib, root) = lib_with_root();
    let mut r = rec(2, root, "nogame.mp4");
    r.game_id = Some("exe:missing".into());
    let res = lib.insert_recording(&r);
    assert!(matches!(res, Err(LibError::Db(_))), "bad game_id: {res:?}");
}

#[test]
fn duplicate_id_and_duplicate_rel_path_are_rejected() {
    let (lib, root) = lib_with_root();
    let r1 = lib.insert_recording(&rec(1, root, "a.mp4")).expect("first");
    assert!(r1 > 0);

    let dup_id = lib.insert_recording(&rec(1, root, "b.mp4"));
    assert!(dup_id.is_err(), "duplicate id accepted");

    let mut same_path = rec(2, root, "c.mp4");
    same_path.rel_path = rec(1, root, "a.mp4").rel_path;
    assert!(
        lib.insert_recording(&same_path).is_err(),
        "duplicate (root_id, rel_path) accepted"
    );

    // Failed inserts must not leave rows behind.
    assert!(lib.get_recording(&oid(2)).expect("get").is_none());
}

#[test]
fn rowids_are_positive_and_increase() {
    let (lib, root) = lib_with_root();
    let a = lib.insert_recording(&rec(1, root, "a.mp4")).expect("a");
    let b = lib.insert_recording(&rec(2, root, "b.mp4")).expect("b");
    let c = lib.insert_recording(&rec(3, root, "c.mp4")).expect("c");
    assert!(a > 0 && b > a && c > b, "rowids {a} {b} {c}");
    assert_eq!(
        lib.get_recording(&oid(2)).expect("get").expect("row").rowid,
        b
    );
}

#[test]
fn insert_rejects_invalid_inputs() {
    let (lib, root) = lib_with_root();

    let mut r = rec(1, root, "x.mp4");
    r.rel_path = String::new();
    assert!(matches!(
        lib.insert_recording(&r),
        Err(LibError::Invalid(_))
    ));

    let mut r = rec(2, root, "x.mp4");
    r.file_name = String::new();
    assert!(matches!(
        lib.insert_recording(&r),
        Err(LibError::Invalid(_))
    ));

    let mut r = rec(3, root, "x.mp4");
    r.ext = "avi".into();
    assert!(lib.insert_recording(&r).is_err(), "ext avi accepted");

    let mut r = rec(4, root, "x.mp4");
    r.id = "x".repeat(25);
    assert!(lib.insert_recording(&r).is_err(), "25-char id accepted");

    let mut r = rec(5, root, "x.mp4");
    r.part_index = Some(0);
    assert!(lib.insert_recording(&r).is_err(), "part_index 0 accepted");

    // Nothing from the rejected inserts should exist.
    for n in 1..=5 {
        assert!(lib.get_recording(&oid(n)).expect("get").is_none());
    }
}

#[test]
fn kind_round_trips_for_clip_and_session() {
    let (lib, root) = lib_with_root();
    lib.insert_recording(&rec(1, root, "clip.mp4"))
        .expect("clip");

    let mut s = rec(2, root, "session.mp4");
    s.kind = RecordingKind::Session;
    s.session_id = Some(oid(900));
    s.part_index = Some(1);
    lib.insert_recording(&s).expect("session");

    assert_eq!(
        lib.get_recording(&oid(1)).expect("g").expect("r").kind,
        RecordingKind::Clip
    );
    let sess = lib.get_recording(&oid(2)).expect("g").expect("r");
    assert_eq!(sess.kind, RecordingKind::Session);
    assert_eq!(sess.session_id, Some(oid(900)));
    assert_eq!(sess.part_index, Some(1));
}

// ---------------------------------------------------------------------------
// 8: get round-trip
// ---------------------------------------------------------------------------

#[test]
fn get_recording_round_trips_every_field() {
    let (lib, root) = lib_with_root();
    let new = NewRecording {
        id: oid(42),
        kind: RecordingKind::Session,
        session_id: Some(oid(900)),
        part_index: Some(2),
        game_id: None,
        root_id: root,
        rel_path: "sess/part2.mkv".into(),
        file_name: "part2.mkv".into(),
        ext: "mkv".into(),
        size_bytes: 123_456,
        file_mtime_ms: 55,
        created_at_ms: 66,
        added_at_ms: 77,
        duration_ms: 1234,
        status: RecStatus::Error,
        error_detail: Some("probe failed".into()),
        end_reason: Some("user_stop".into()),
        recovered: true,
        degraded: true,
        dropped_packets: 5,
        gap_ms: 42,
        needs_faststart: true,
    };
    let rowid = lib.insert_recording(&new).expect("insert");

    let row = lib.get_recording(&oid(42)).expect("get").expect("some");
    assert_eq!(row.rowid, rowid);
    assert_eq!(row.id, oid(42));
    assert_eq!(row.kind, RecordingKind::Session);
    assert_eq!(row.session_id, Some(oid(900)));
    assert_eq!(row.part_index, Some(2));
    assert_eq!(row.game_id, None);
    assert_eq!(row.game_name, None);
    assert_eq!(row.root_id, root);
    assert_eq!(row.root_path, "C:\\Clips");
    assert_eq!(row.rel_path, "sess/part2.mkv");
    assert_eq!(row.file_name, "part2.mkv");
    assert_eq!(row.ext, "mkv");
    assert_eq!(row.size_bytes, 123_456);
    assert_eq!(row.file_mtime_ms, 55);
    assert_eq!(row.created_at_ms, 66);
    assert_eq!(row.added_at_ms, 77);
    assert_eq!(row.duration_ms, 1234);
    assert_eq!(row.status, RecStatus::Error);
    assert_eq!(row.error_detail.as_deref(), Some("probe failed"));
    assert!(!row.is_favorite);
    assert_eq!(row.end_reason.as_deref(), Some("user_stop"));
    assert!(row.recovered);
    assert!(row.degraded);
    assert_eq!(row.dropped_packets, 5);
    assert_eq!(row.gap_ms, 42);
    assert!(row.needs_faststart);
}

#[test]
fn get_unknown_id_returns_none() {
    let (lib, _root) = lib_with_root();
    assert!(lib.get_recording(&oid(12345)).expect("get").is_none());
}

// ---------------------------------------------------------------------------
// 9: storage roots
// ---------------------------------------------------------------------------

#[test]
fn ensure_storage_root_is_idempotent_and_case_insensitive() {
    let lib = Library::open_in_memory().expect("open");
    let a = lib.ensure_storage_root("C:\\Clips", 1).expect("a");
    assert_eq!(a, lib.ensure_storage_root("C:\\Clips", 2).expect("a again"));
    assert_eq!(
        a,
        lib.ensure_storage_root("c:\\clips", 3)
            .expect("a lowercase"),
        "path comparison is expected to be case-insensitive"
    );
    let b = lib.ensure_storage_root("D:\\Other", 4).expect("b");
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------
// 10: rename
// ---------------------------------------------------------------------------

#[test]
fn rename_updates_row_and_fts() {
    let (lib, root) = lib_with_root();
    lib.insert_recording(&rec(1, root, "alpha.mp4"))
        .expect("insert");
    assert_eq!(
        first_page(
            &lib,
            ListQuery {
                text: Some("alpha".into()),
                ..Default::default()
            }
        )
        .items
        .len(),
        1
    );

    lib.rename_recording_record(&oid(1), "zulu.mp4", "u/renamed/zulu.mp4")
        .expect("rename");

    let row = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert_eq!(row.file_name, "zulu.mp4");
    assert_eq!(row.rel_path, "u/renamed/zulu.mp4");

    let new_hits = first_page(
        &lib,
        ListQuery {
            text: Some("zulu".into()),
            ..Default::default()
        },
    );
    assert_eq!(ids(&new_hits.items), vec![oid(1)]);
    let old_hits = first_page(
        &lib,
        ListQuery {
            text: Some("alpha".into()),
            ..Default::default()
        },
    );
    assert!(old_hits.items.is_empty(), "old name still searchable");
}

#[test]
fn rename_error_cases() {
    let (lib, root) = lib_with_root();
    lib.insert_recording(&rec(1, root, "a.mp4"))
        .expect("insert");
    assert!(matches!(
        lib.rename_recording_record(&oid(999), "b.mp4", "u/b.mp4"),
        Err(LibError::NotFound)
    ));
    assert!(matches!(
        lib.rename_recording_record(&oid(1), "", "u/b.mp4"),
        Err(LibError::Invalid(_))
    ));
}

// ---------------------------------------------------------------------------
// 11: favorites
// ---------------------------------------------------------------------------

#[test]
fn favorite_toggle_round_trips() {
    let (lib, root) = lib_with_root();
    lib.insert_recording(&rec(1, root, "fav.mp4"))
        .expect("insert");
    lib.insert_recording(&rec(2, root, "other.mp4"))
        .expect("insert");
    let fav_q = ListQuery {
        favorites_only: true,
        ..Default::default()
    };

    lib.set_favorite(&oid(1), true).expect("set true");
    assert!(
        lib.get_recording(&oid(1))
            .expect("g")
            .expect("r")
            .is_favorite
    );
    assert_eq!(ids(&first_page(&lib, fav_q.clone()).items), vec![oid(1)]);

    lib.set_favorite(&oid(1), false).expect("set false");
    assert!(
        !lib.get_recording(&oid(1))
            .expect("g")
            .expect("r")
            .is_favorite
    );
    assert!(first_page(&lib, fav_q).items.is_empty());

    assert!(matches!(
        lib.set_favorite(&oid(999), true),
        Err(LibError::NotFound)
    ));
}

// ---------------------------------------------------------------------------
// 12: delete
// ---------------------------------------------------------------------------

#[test]
fn delete_removes_row_and_fts_entry() {
    let (lib, root) = lib_with_root();
    lib.insert_recording(&rec(1, root, "gone.mp4"))
        .expect("insert");
    lib.insert_recording(&rec(2, root, "keep.mp4"))
        .expect("insert");
    assert_eq!(
        first_page(
            &lib,
            ListQuery {
                text: Some("gone".into()),
                ..Default::default()
            }
        )
        .items
        .len(),
        1
    );

    lib.delete_recording_record(&oid(1)).expect("delete");
    assert!(lib.get_recording(&oid(1)).expect("get").is_none());
    let all = first_page(&lib, ListQuery::default());
    assert_eq!(ids(&all.items), vec![oid(2)]);
    let hits = first_page(
        &lib,
        ListQuery {
            text: Some("gone".into()),
            ..Default::default()
        },
    );
    assert!(hits.items.is_empty(), "FTS row survived delete");

    assert!(matches!(
        lib.delete_recording_record(&oid(1)),
        Err(LibError::NotFound)
    ));
}

// ---------------------------------------------------------------------------
// 13: games
// ---------------------------------------------------------------------------

fn game(id: &str, display: &str, folder: &str) -> NewGame {
    NewGame {
        id: id.into(),
        display_name: display.into(),
        folder_name: folder.into(),
        exe_name: Some("game.exe".into()),
        store_id: None,
    }
}

#[test]
fn upsert_game_updates_name_visible_in_recordings_and_fts() {
    let (lib, root) = lib_with_root();
    lib.upsert_game(&game("exe:alpha", "Alpha Quest", "alpha_quest"), 1)
        .expect("upsert");

    let mut r = rec(1, root, "clip1.mp4");
    r.game_id = Some("exe:alpha".into());
    lib.insert_recording(&r).expect("insert");

    let row = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert_eq!(row.game_name.as_deref(), Some("Alpha Quest"));
    assert_eq!(
        ids(&first_page(
            &lib,
            ListQuery {
                text: Some("quest".into()),
                ..Default::default()
            }
        )
        .items),
        vec![oid(1)]
    );

    lib.upsert_game(&game("exe:alpha", "Beta Trial", "different_folder"), 2)
        .expect("second upsert with changed folder must not error");

    let row = lib.get_recording(&oid(1)).expect("get").expect("row");
    assert_eq!(row.game_name.as_deref(), Some("Beta Trial"));
    assert_eq!(
        ids(&first_page(
            &lib,
            ListQuery {
                text: Some("trial".into()),
                ..Default::default()
            }
        )
        .items),
        vec![oid(1)]
    );
    assert!(
        first_page(
            &lib,
            ListQuery {
                text: Some("quest".into()),
                ..Default::default()
            }
        )
        .items
        .is_empty(),
        "old game name still in FTS"
    );

    // folder_name must still be "alpha_quest": another game may not take it...
    assert!(
        lib.upsert_game(&game("exe:other", "Other", "alpha_quest"), 3)
            .is_err(),
        "folder_name appears to have changed on update"
    );
    // ...but the folder the update tried to set was never taken, so it is free.
    lib.upsert_game(&game("exe:free", "Free", "different_folder"), 3)
        .expect("folder from rejected update should be unused");
}

#[test]
fn upsert_game_rejects_empty_display_name() {
    let (lib, _root) = lib_with_root();
    assert!(matches!(
        lib.upsert_game(&game("exe:x", "", "x_folder"), 1),
        Err(LibError::Invalid(_))
    ));
}

// ---------------------------------------------------------------------------
// 14: list filters
// ---------------------------------------------------------------------------

#[test]
fn list_filters_game_kind_favorites_size_and_combined() {
    let (lib, root) = lib_with_root();
    lib.upsert_game(&game("g1", "Game One", "f1"), 1)
        .expect("g1");
    lib.upsert_game(&game("g2", "Game Two", "f2"), 1)
        .expect("g2");

    for n in 1..=8u64 {
        let mut r = rec(n, root, &format!("c{n}.mp4"));
        r.game_id = Some(if n % 2 == 1 { "g1".into() } else { "g2".into() });
        r.kind = if n % 3 == 0 {
            RecordingKind::Session
        } else {
            RecordingKind::Clip
        };
        if r.kind == RecordingKind::Session {
            r.session_id = Some(oid(900));
            r.part_index = Some(n as i64);
        }
        r.size_bytes = (n * 100) as i64;
        lib.insert_recording(&r).expect("insert");
    }
    lib.set_favorite(&oid(2), true).expect("fav2");
    lib.set_favorite(&oid(4), true).expect("fav4");

    let by_game = walk(
        &lib,
        ListQuery {
            game: Some("g1".into()),
            limit: 2,
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&by_game), vec![oid(1), oid(3), oid(5), oid(7)]);

    let sessions = walk(
        &lib,
        ListQuery {
            kind: Some(RecordingKind::Session),
            limit: 1,
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&sessions), vec![oid(3), oid(6)]);

    let favs = walk(
        &lib,
        ListQuery {
            favorites_only: true,
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&favs), vec![oid(2), oid(4)]);

    // sizes: n*100, bounds inclusive -> 300, 400, 500
    let sized = walk(
        &lib,
        ListQuery {
            min_size: Some(300),
            max_size: Some(500),
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&sized), vec![oid(3), oid(4), oid(5)]);

    let min_only = walk(
        &lib,
        ListQuery {
            min_size: Some(700),
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&min_only), vec![oid(7), oid(8)]);

    let combined = walk(
        &lib,
        ListQuery {
            game: Some("g2".into()),
            favorites_only: true,
            min_size: Some(250),
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&combined), vec![oid(4)]);

    let empty = walk(
        &lib,
        ListQuery {
            game: Some("g1".into()),
            kind: Some(RecordingKind::Session),
            ..Default::default()
        },
    );
    assert_eq!(sorted_ids(&empty), vec![oid(3)]);
}

// ---------------------------------------------------------------------------
// 15: status filtering
// ---------------------------------------------------------------------------

#[test]
fn list_includes_only_ready_and_pending() {
    let (lib, root) = lib_with_root();
    let statuses = [
        (1u64, RecStatus::Ready),
        (2, RecStatus::Pending),
        (3, RecStatus::Recording),
        (4, RecStatus::Missing),
        (5, RecStatus::Error),
    ];
    for (n, st) in statuses {
        let mut r = rec(n, root, &format!("s{n}.mp4"));
        r.status = st;
        lib.insert_recording(&r).expect("insert");
    }
    assert_eq!(
        sorted_ids(&walk(&lib, ListQuery::default())),
        vec![oid(1), oid(2)]
    );
    for (n, st) in statuses {
        assert_eq!(
            lib.get_recording(&oid(n))
                .expect("get")
                .expect("row")
                .status,
            st
        );
    }
}

// ---------------------------------------------------------------------------
// 16: text search
// ---------------------------------------------------------------------------

#[test]
fn text_search_tokens_prefix_case_and_diacritics() {
    let (lib, root) = lib_with_root();
    let names = [
        "foo bar.mp4",
        "foo baz.mp4",
        "bar only.mp4",
        "Café Run.mp4",
        "other.mp4",
    ];
    for (i, name) in names.iter().enumerate() {
        lib.insert_recording(&rec(i as u64 + 1, root, name))
            .expect("insert");
    }
    let search = |t: &str| {
        sorted_ids(&walk(
            &lib,
            ListQuery {
                text: Some(t.into()),
                ..Default::default()
            },
        ))
    };

    assert_eq!(search("foo bar"), vec![oid(1)], "both tokens must match");
    assert_eq!(search("foo"), vec![oid(1), oid(2)]);
    assert_eq!(search("fo"), vec![oid(1), oid(2)], "prefix match");
    assert_eq!(search("bar"), vec![oid(1), oid(3)]);
    assert_eq!(search("FOO BAR"), vec![oid(1)], "case-insensitive");
    assert_eq!(search("cafe"), vec![oid(4)], "diacritics removed");
    assert_eq!(search("CAFÉ"), vec![oid(4)], "uppercase diacritic");
    assert_eq!(
        search("\"*()"),
        (1..=5).map(oid).collect::<Vec<_>>(),
        "punctuation-only is no filter"
    );
    assert_eq!(
        search(""),
        (1..=5).map(oid).collect::<Vec<_>>(),
        "empty is no filter"
    );
    assert!(search("zzzz").is_empty());
}

// ---------------------------------------------------------------------------
// 17: large walk
// ---------------------------------------------------------------------------

#[test]
fn walk_2500_rows_across_limits_and_sorts() {
    let lib = Library::open_in_memory().expect("open");
    let root = lib.ensure_storage_root("C:\\Big", 1).expect("root");
    let total = 2500u64;
    for n in 1..=total {
        let mut r = rec(n, root, &format!("clip{n:05}.mp4"));
        r.duration_ms = ((n * 37) % 1000) as i64; // deliberate duplicates
        lib.insert_recording(&r).expect("insert");
    }

    for sort in [SortKey::Newest, SortKey::DurationDesc] {
        for limit in [1u32, 200] {
            let rows = walk(
                &lib,
                ListQuery {
                    sort,
                    limit,
                    ..Default::default()
                },
            );
            assert_eq!(rows.len(), total as usize, "{sort:?} limit {limit}: total");
            let mut v = ids(&rows);
            v.sort();
            v.dedup();
            assert_eq!(
                v.len(),
                total as usize,
                "{sort:?} limit {limit}: duplicates"
            );
        }
    }

    // limit 0 clamps to 1; limit 999 clamps to 200.
    let p0 = first_page(
        &lib,
        ListQuery {
            limit: 0,
            ..Default::default()
        },
    );
    assert_eq!(p0.items.len(), 1);
    assert!(p0.next_cursor.is_some());
    let p999 = first_page(
        &lib,
        ListQuery {
            limit: 999,
            ..Default::default()
        },
    );
    assert_eq!(p999.items.len(), 200);
}

// ---------------------------------------------------------------------------
// 18: cursor validation
// ---------------------------------------------------------------------------

#[test]
fn cursor_validation() {
    let (lib, root) = lib_with_root();
    for n in 1..=10u64 {
        lib.insert_recording(&rec(n, root, &format!("c{n:02}.mp4")))
            .expect("insert");
    }

    for garbage in ["zz", "", "abcd", "68656c6c6f"] {
        let res = lib.list(&ListQuery {
            cursor: Some(garbage.into()),
            ..Default::default()
        });
        assert!(
            matches!(res, Err(LibError::Invalid(_))),
            "cursor {garbage:?}: {res:?}"
        );
    }

    let newest_page = first_page(
        &lib,
        ListQuery {
            limit: 3,
            ..Default::default()
        },
    );
    let newest_cursor = newest_page.next_cursor.clone().expect("cursor");

    let name_cursor = first_page(
        &lib,
        ListQuery {
            sort: SortKey::NameAsc,
            limit: 3,
            ..Default::default()
        },
    )
    .next_cursor
    .expect("name cursor");
    let res = lib.list(&ListQuery {
        cursor: Some(name_cursor),
        limit: 3,
        ..Default::default()
    });
    assert!(
        matches!(res, Err(LibError::Invalid(_))),
        "NameAsc cursor on Newest: {res:?}"
    );

    let res = lib.list(&ListQuery {
        favorites_only: true,
        cursor: Some(newest_cursor.clone()),
        limit: 3,
        ..Default::default()
    });
    assert!(
        matches!(res, Err(LibError::Invalid(_))),
        "favorites flag changed: {res:?}"
    );

    // Changing only the limit keeps the cursor valid, and the next page continues after the
    // last row of the first page.
    let p2 = lib
        .list(&ListQuery {
            limit: 5,
            cursor: Some(newest_cursor),
            ..Default::default()
        })
        .expect("limit change must be allowed");
    assert_eq!(p2.items.len(), 5);
    let first_ids = ids(&newest_page.items);
    assert!(
        p2.items.iter().all(|r| !first_ids.contains(&r.id)),
        "second page overlaps first page"
    );
}

// ---------------------------------------------------------------------------
// 19: ties
// ---------------------------------------------------------------------------

#[test]
fn identical_created_at_ties_are_ordered_by_id() {
    let (lib, root) = lib_with_root();
    for n in 1..=50u64 {
        let mut r = rec(n, root, &format!("t{n:02}.mp4"));
        r.created_at_ms = 5000;
        lib.insert_recording(&r).expect("insert");
    }
    let desc: Vec<String> = (1..=50u64).rev().map(oid).collect();
    let asc: Vec<String> = (1..=50u64).map(oid).collect();

    let newest = walk(
        &lib,
        ListQuery {
            sort: SortKey::Newest,
            limit: 7,
            ..Default::default()
        },
    );
    assert_eq!(
        ids(&newest),
        desc,
        "Newest must be id DESC on ties, each exactly once"
    );

    let oldest = walk(
        &lib,
        ListQuery {
            sort: SortKey::Oldest,
            limit: 7,
            ..Default::default()
        },
    );
    assert_eq!(
        ids(&oldest),
        asc,
        "Oldest must be id ASC on ties, each exactly once"
    );
}

// ---------------------------------------------------------------------------
// 20: ordering across pages
// ---------------------------------------------------------------------------

#[test]
fn sort_orders_hold_across_pages() {
    let (lib, root) = lib_with_root();
    for n in 1..=300u64 {
        let mut r = rec(n, root, &format!("o{n:03}.mp4"));
        r.created_at_ms = ((n * 7919) % 1000) as i64;
        r.size_bytes = ((n * 613) % 5000) as i64;
        lib.insert_recording(&r).expect("insert");
    }

    let newest = walk(
        &lib,
        ListQuery {
            sort: SortKey::Newest,
            limit: 13,
            ..Default::default()
        },
    );
    assert_eq!(newest.len(), 300);
    for w in newest.windows(2) {
        assert!(
            w[0].created_at_ms > w[1].created_at_ms
                || (w[0].created_at_ms == w[1].created_at_ms && w[0].id > w[1].id),
            "Newest out of order: {} then {}",
            w[0].created_at_ms,
            w[1].created_at_ms
        );
    }

    let size_asc = walk(
        &lib,
        ListQuery {
            sort: SortKey::SizeAsc,
            limit: 13,
            ..Default::default()
        },
    );
    assert_eq!(size_asc.len(), 300);
    for w in size_asc.windows(2) {
        assert!(w[0].size_bytes <= w[1].size_bytes, "SizeAsc out of order");
    }
}

// ---------------------------------------------------------------------------
// 21-22: empty library and page boundaries
// ---------------------------------------------------------------------------

#[test]
fn empty_library_lists_nothing() {
    let lib = Library::open_in_memory().expect("open");
    let page = lib.list(&ListQuery::default()).expect("list");
    assert!(page.items.is_empty());
    assert!(page.next_cursor.is_none());
}

#[test]
fn page_boundary_exact_limit_has_no_cursor() {
    let (lib, root) = lib_with_root();
    for n in 1..=50u64 {
        lib.insert_recording(&rec(n, root, &format!("b{n:02}.mp4")))
            .expect("insert");
    }
    let page = lib
        .list(&ListQuery {
            limit: 50,
            ..Default::default()
        })
        .expect("list");
    assert_eq!(page.items.len(), 50);
    assert!(
        page.next_cursor.is_none(),
        "total == limit must not produce a cursor"
    );
}

#[test]
fn page_boundary_limit_plus_one_has_cursor_then_one_item() {
    let (lib, root) = lib_with_root();
    for n in 1..=51u64 {
        lib.insert_recording(&rec(n, root, &format!("b{n:02}.mp4")))
            .expect("insert");
    }
    let p1 = lib
        .list(&ListQuery {
            limit: 50,
            ..Default::default()
        })
        .expect("p1");
    assert_eq!(p1.items.len(), 50);
    let cursor = p1
        .next_cursor
        .expect("total == limit + 1 must produce a cursor");

    let p2 = lib
        .list(&ListQuery {
            limit: 50,
            cursor: Some(cursor),
            ..Default::default()
        })
        .expect("p2");
    assert_eq!(p2.items.len(), 1);
    assert!(p2.next_cursor.is_none());
    assert_eq!(
        p2.items[0].id,
        oid(1),
        "last row in Newest order is the oldest"
    );
}
