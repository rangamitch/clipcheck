//! Integration tests for storage roots, scan bookkeeping, externally indexed rows and the
//! v1 -> v2 schema migration (DESIGN.md D24).

use std::fs;
use std::path::{Path, PathBuf};

use clipcheck_library::schema::{self, V1_SQL};
use clipcheck_library::{
    ExternalRecording, LibError, Library, ListQuery, NewGame, NewRecording, RecStatus,
    RecordingKind,
};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A 26-character id derived from a number.
fn oid(n: u64) -> String {
    format!("{:026}", n)
}

fn p(path: &Path) -> String {
    path.display().to_string()
}

/// Opens a file-backed library (WAL) in a fresh temp dir.
fn file_lib(dir: &Path) -> (Library, PathBuf) {
    let path = dir.join("lib.db");
    (Library::open(&path).expect("open library"), path)
}

/// An external row for a root. `rel` may use backslashes, as on Windows.
fn ext_rec(n: u64, root: i64, rel: &str, game: Option<&str>, playable: bool) -> ExternalRecording {
    let file_name = rel.rsplit(['\\', '/']).next().unwrap().to_owned();
    let ext = if file_name.ends_with(".mkv") {
        "mkv"
    } else {
        "mp4"
    }
    .to_owned();
    ExternalRecording {
        id: oid(n),
        kind: RecordingKind::Clip,
        game_id: game.map(str::to_owned),
        root_id: root,
        rel_path: rel.to_owned(),
        file_name,
        ext,
        size_bytes: 100_000 + n as i64,
        file_mtime_ms: 1_000 + n as i64,
        created_at_ms: 1_000 + n as i64,
        added_at_ms: 2_000,
        duration_ms: 1_500,
        width: Some(320),
        height: Some(240),
        fps: Some((30, 1)),
        video_codec: Some("h264".into()),
        audio_track_count: 1,
        playable,
    }
}

fn all_rows(lib: &Library) -> Vec<clipcheck_library::RecordingRow> {
    lib.list(&ListQuery::default()).expect("list").items
}

fn raw(path: &Path) -> Connection {
    Connection::open(path).expect("raw connection")
}

fn status_of(lib: &Library, root: i64, id: &str) -> RecStatus {
    lib.list_paths_for_root(root)
        .expect("paths")
        .into_iter()
        .find(|r| r.0 == id)
        .unwrap_or_else(|| panic!("no row {id}"))
        .4
}

// ---------------------------------------------------------------------------
// a. upsert_root
// ---------------------------------------------------------------------------

#[test]
fn a_upsert_root_first_active_second_legacy_same_path_same_id_and_relabel() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());

    let id1 = lib.upsert_root("C:\\Medal\\Clips", "Main", false).unwrap();
    let id2 = lib.upsert_root("D:\\Archive\\Clips", "", false).unwrap();
    assert_ne!(id1, id2);

    let again = lib
        .upsert_root("C:\\Medal\\Clips", "Renamed", true)
        .unwrap();
    assert_eq!(again, id1, "same path must return the same id");

    let roots = lib.list_roots_with_stats().unwrap();
    assert_eq!(roots.len(), 2, "same path must not create a second root");
    // primary first
    assert_eq!(roots[0].id, id1);
    assert!(roots[0].is_primary);
    assert_eq!(roots[0].kind, "active");
    assert_eq!(roots[0].label, "Renamed");
    assert!(roots[0].read_only);
    assert_eq!(roots[0].availability, "online");
    // second is legacy
    assert_eq!(roots[1].id, id2);
    assert!(!roots[1].is_primary);
    assert_eq!(roots[1].kind, "legacy");
    assert_eq!(roots[1].path, "D:\\Archive\\Clips");
    assert!(!roots[1].read_only);
    assert_eq!(roots[1].last_scan_unix_ms, None);
}

// ---------------------------------------------------------------------------
// b. set_primary_root
// ---------------------------------------------------------------------------

#[test]
fn b_set_primary_root_flips_exactly_one_and_guards_read_only_and_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());

    let id1 = lib.upsert_root("C:\\A\\Clips", "", false).unwrap(); // active
    let ro = lib.upsert_root("E:\\ReadOnly", "", true).unwrap(); // legacy, read-only
    let other = lib.upsert_root("F:\\Other", "", false).unwrap(); // legacy

    assert!(
        matches!(lib.set_primary_root(ro), Err(LibError::Invalid(_))),
        "a read-only root must not become primary"
    );
    assert!(matches!(
        lib.set_primary_root(9999),
        Err(LibError::NotFound)
    ));
    // failed attempts leave the primary untouched
    let roots = lib.list_roots_with_stats().unwrap();
    assert_eq!(roots[0].id, id1);
    assert!(roots[0].is_primary);

    lib.set_primary_root(other).unwrap();
    let roots = lib.list_roots_with_stats().unwrap();
    let active: Vec<_> = roots.iter().filter(|r| r.kind == "active").collect();
    assert_eq!(active.len(), 1, "exactly one active root");
    assert_eq!(active[0].id, other);
    assert!(active[0].is_primary);
    assert_eq!(roots.iter().find(|r| r.id == id1).unwrap().kind, "legacy");
    assert_eq!(roots[0].id, other, "new primary listed first");
}

// ---------------------------------------------------------------------------
// c. set_root_availability
// ---------------------------------------------------------------------------

#[test]
fn c_root_availability_round_trips_and_unknown_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());
    let id = lib.upsert_root("C:\\Medal\\Clips", "", false).unwrap();

    lib.set_root_availability(id, false).unwrap();
    let info = lib.list_roots_with_stats().unwrap();
    assert_eq!(info[0].availability, "offline");

    lib.set_root_availability(id, true).unwrap();
    let info = lib.list_roots_with_stats().unwrap();
    assert_eq!(info[0].availability, "online");

    assert!(matches!(
        lib.set_root_availability(9999, false),
        Err(LibError::NotFound)
    ));
}

// ---------------------------------------------------------------------------
// d. remove_root_keep_files
// ---------------------------------------------------------------------------

#[test]
fn d_remove_legacy_root_deletes_rows_but_keeps_files_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());

    let main_dir = dir.path().join("Main");
    let legacy_dir = dir.path().join("Legacy");
    fs::create_dir_all(&main_dir).unwrap();
    fs::create_dir_all(legacy_dir.join("Apex")).unwrap();
    let keep = legacy_dir.join("Apex").join("MedalTVKeep.mp4");
    fs::write(&keep, vec![1u8; 1024]).unwrap();

    let primary = lib.upsert_root(&p(&main_dir), "", false).unwrap();
    let legacy = lib.upsert_root(&p(&legacy_dir), "", false).unwrap();
    lib.write_external_batch(
        &[ext_rec(1, legacy, "Apex\\MedalTVKeep.mp4", None, true)],
        &[],
    )
    .unwrap();
    assert_eq!(lib.list_paths_for_root(legacy).unwrap().len(), 1);

    lib.remove_root_keep_files(legacy).unwrap();

    assert!(keep.exists(), "removing a root must never delete files");
    assert!(lib.list_paths_for_root(legacy).unwrap().is_empty());
    assert!(lib
        .list_roots_with_stats()
        .unwrap()
        .iter()
        .all(|r| r.id != legacy));
    let page = lib
        .list(&ListQuery {
            text: Some("MedalTVKeep".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        page.items.is_empty(),
        "search rows of the removed root are gone"
    );

    assert!(
        matches!(
            lib.remove_root_keep_files(primary),
            Err(LibError::Invalid(_))
        ),
        "the primary root cannot be removed"
    );
    assert!(matches!(
        lib.remove_root_keep_files(9999),
        Err(LibError::NotFound)
    ));
}

// ---------------------------------------------------------------------------
// e. find_by_rel_path, touch_scan, list_paths_for_root
// ---------------------------------------------------------------------------

#[test]
fn e_find_by_rel_path_touch_scan_and_per_root_paths() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());
    let r1 = lib.upsert_root("C:\\One", "", false).unwrap();
    let r2 = lib.upsert_root("D:\\Two", "", false).unwrap();

    lib.write_external_batch(
        &[
            ext_rec(1, r1, "Apex\\MedalTVApex1.mp4", None, true),
            ext_rec(2, r1, "Valorant\\MedalTVVal1.mp4", None, true),
            // same relative path under another root is a different row
            ext_rec(3, r2, "Apex\\MedalTVApex1.mp4", None, true),
        ],
        &[],
    )
    .unwrap();

    assert_eq!(
        lib.find_by_rel_path(r1, "Apex\\MedalTVApex1.mp4").unwrap(),
        Some(oid(1))
    );
    assert_eq!(
        lib.find_by_rel_path(r2, "Apex\\MedalTVApex1.mp4").unwrap(),
        Some(oid(3))
    );
    assert_eq!(
        lib.find_by_rel_path(r1, "apex\\medaltvapex1.mp4").unwrap(),
        None,
        "find_by_rel_path is an exact match"
    );
    assert_eq!(lib.find_by_rel_path(r1, "nope.mp4").unwrap(), None);

    lib.touch_scan(r1, 777).unwrap();
    let roots = lib.list_roots_with_stats().unwrap();
    assert_eq!(
        roots.iter().find(|r| r.id == r1).unwrap().last_scan_unix_ms,
        Some(777)
    );
    assert_eq!(
        roots.iter().find(|r| r.id == r2).unwrap().last_scan_unix_ms,
        None
    );
    assert!(matches!(lib.touch_scan(9999, 1), Err(LibError::NotFound)));

    let p1 = lib.list_paths_for_root(r1).unwrap();
    let mut ids: Vec<_> = p1.iter().map(|r| r.0.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec![oid(1), oid(2)], "only rows of root r1");
    let p2 = lib.list_paths_for_root(r2).unwrap();
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].0, oid(3));
    assert_eq!(p2[0].1, "Apex\\MedalTVApex1.mp4");
    assert_eq!(p2[0].2, 100_003);
    assert_eq!(p2[0].3, 1_003);
    assert_eq!(p2[0].4, RecStatus::Ready);
}

// ---------------------------------------------------------------------------
// f. mark_missing
// ---------------------------------------------------------------------------

#[test]
fn f_mark_missing_counts_only_changes_and_hides_rows_from_list() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());
    let root = lib.upsert_root("C:\\Medal\\Clips", "", false).unwrap();
    let other = lib.upsert_root("D:\\Other", "", false).unwrap();

    lib.write_external_batch(
        &[
            ext_rec(1, root, "Apex\\MedalTVA1.mp4", None, true),
            ext_rec(2, root, "Apex\\MedalTVA2.mp4", None, true),
            ext_rec(3, root, "Apex\\MedalTVA3.mp4", None, true),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(all_rows(&lib).len(), 3);

    let n = lib.mark_missing(root, &[oid(1), oid(2)]).unwrap();
    assert_eq!(n, 2);
    let again = lib.mark_missing(root, &[oid(1)]).unwrap();
    assert_eq!(again, 0, "already-missing rows are not counted again");
    assert_eq!(lib.mark_missing(root, &[]).unwrap(), 0);
    assert_eq!(
        lib.mark_missing(other, &[oid(3)]).unwrap(),
        0,
        "rows of another root are not touched"
    );

    let ids: Vec<_> = all_rows(&lib).into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![oid(3)], "missing rows disappear from list()");

    let info = lib.list_roots_with_stats().unwrap();
    let stats = info.iter().find(|r| r.id == root).unwrap();
    assert_eq!(stats.clip_count, 1, "counts exclude missing rows");
    assert_eq!(stats.total_bytes, 100_003);

    let paths = lib.list_paths_for_root(root).unwrap();
    assert_eq!(paths.len(), 3, "missing rows remain in list_paths_for_root");
    assert_eq!(status_of(&lib, root, &oid(1)), RecStatus::Missing);
    assert_eq!(status_of(&lib, root, &oid(2)), RecStatus::Missing);
    assert_eq!(status_of(&lib, root, &oid(3)), RecStatus::Ready);
}

// ---------------------------------------------------------------------------
// g. write_external_batch (insert, update, FTS, playable, game_name)
// ---------------------------------------------------------------------------

#[test]
fn g_write_external_batch_inserts_updates_restores_and_is_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, path) = file_lib(dir.path());
    let root = lib.upsert_root("C:\\Medal\\Clips", "", false).unwrap();
    lib.upsert_game(
        &NewGame {
            id: "game-apex".into(),
            display_name: "Apex".into(),
            folder_name: "Apex".into(),
            exe_name: None,
            store_id: None,
        },
        1_000,
    )
    .unwrap();

    let new = vec![
        ext_rec(1, root, "Apex\\MedalTVApexA.mp4", Some("game-apex"), true),
        ext_rec(2, root, "Apex\\MedalTVApexB.mkv", Some("game-apex"), false),
        ext_rec(3, root, "MedalTVLoose.mp4", None, true),
    ];
    lib.write_external_batch(&new, &[]).unwrap();

    let rows = all_rows(&lib);
    assert_eq!(rows.len(), 3);
    let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(row(&oid(1)).game_name.as_deref(), Some("Apex"));
    assert_eq!(row(&oid(2)).ext, "mkv");
    assert_eq!(row(&oid(3)).game_name, None);
    assert_eq!(row(&oid(3)).kind, RecordingKind::Clip);

    // source and playable are not in RecordingRow; check them directly
    let c = raw(&path);
    let (src, play): (String, i64) = c
        .query_row(
            "SELECT source, playable FROM recordings WHERE id = ?1",
            [oid(1)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((src.as_str(), play), ("external", 1));
    let (src, play): (String, i64) = c
        .query_row(
            "SELECT source, playable FROM recordings WHERE id = ?1",
            [oid(2)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((src.as_str(), play), ("external", 0), "mkv is not playable");
    drop(c);

    // FTS finds the file by its name
    let hits = lib
        .list(&ListQuery {
            text: Some("MedalTVApexB".into()),
            ..Default::default()
        })
        .unwrap()
        .items;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, oid(2));

    // Update: a row goes missing, then a refresh restores it to ready with new size.
    assert_eq!(lib.mark_missing(root, &[oid(1)]).unwrap(), 1);
    assert_eq!(status_of(&lib, root, &oid(1)), RecStatus::Missing);
    let upd = ExternalRecording {
        size_bytes: 555,
        file_mtime_ms: 9_999,
        duration_ms: 2_000,
        // the next fields are ignored on update
        rel_path: "Wrong\\x.mp4".into(),
        game_id: None,
        ..ext_rec(1, root, "Apex\\MedalTVApexA.mp4", None, true)
    };
    lib.write_external_batch(&[], &[upd]).unwrap();

    assert_eq!(status_of(&lib, root, &oid(1)), RecStatus::Ready);
    let paths = lib.list_paths_for_root(root).unwrap();
    let r1 = paths.iter().find(|r| r.0 == oid(1)).unwrap();
    assert_eq!((r1.2, r1.3), (555, 9_999));
    let c = raw(&path);
    let (rel, dur): (String, i64) = c
        .query_row(
            "SELECT rel_path, duration_ms FROM recordings WHERE id = ?1",
            [oid(1)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        rel, "Apex\\MedalTVApexA.mp4",
        "update never changes the path"
    );
    assert_eq!(dur, 2_000);
    drop(c);
    let row1 = all_rows(&lib)
        .into_iter()
        .find(|r| r.id == oid(1))
        .expect("restored row is listed");
    assert_eq!(
        row1.game_name.as_deref(),
        Some("Apex"),
        "update keeps the game"
    );
    assert_eq!(row1.size_bytes, 555);

    // FTS refreshed on update: the name still matches
    let hits = lib
        .list(&ListQuery {
            text: Some("MedalTVApexA".into()),
            ..Default::default()
        })
        .unwrap()
        .items;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, oid(1));

    // a malformed id is rejected before anything is written
    let bad = ExternalRecording {
        id: "short".into(),
        ..ext_rec(9, root, "Apex\\MedalTVBad.mp4", None, true)
    };
    assert!(matches!(
        lib.write_external_batch(&[bad], &[]),
        Err(LibError::Invalid(_))
    ));
    assert_eq!(all_rows(&lib).len(), 3);
}

// ---------------------------------------------------------------------------
// h. external_ready_files
// ---------------------------------------------------------------------------

#[test]
fn h_external_ready_files_returns_only_ready_external_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());
    let root_dir = dir.path().join("Clips");
    let root = lib.upsert_root(&p(&root_dir), "", false).unwrap();

    // a recorded row (source = recorded) that is ready must not appear
    lib.insert_recording(&NewRecording {
        id: oid(10),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id: root,
        rel_path: "Recorded\\rec.mp4".into(),
        file_name: "rec.mp4".into(),
        ext: "mp4".into(),
        size_bytes: 10,
        file_mtime_ms: 1,
        created_at_ms: 1,
        added_at_ms: 1,
        duration_ms: 1_500,
        status: RecStatus::Ready,
        error_detail: None,
        end_reason: None,
        recovered: false,
        degraded: false,
        dropped_packets: 0,
        gap_ms: 0,
        needs_faststart: false,
    })
    .unwrap();

    lib.write_external_batch(
        &[
            ext_rec(1, root, "Apex\\MedalTVA1.mp4", None, true),
            ext_rec(2, root, "Apex\\MedalTVA2.mp4", None, true),
            ext_rec(3, root, "Apex\\MedalTVA3.mp4", None, true),
        ],
        &[],
    )
    .unwrap();
    lib.mark_missing(root, &[oid(2)]).unwrap();

    let mut files = lib.external_ready_files().unwrap();
    files.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<_> = files.iter().map(|f| f.id.clone()).collect();
    assert_eq!(ids, vec![oid(1), oid(3)]);
    assert_eq!(files[0].root_path, p(&root_dir));
    assert_eq!(files[0].rel_path, "Apex\\MedalTVA1.mp4");
    assert_eq!(files[0].duration_ms, 1_500);
}

// ---------------------------------------------------------------------------
// i. migration from v1 with data
// ---------------------------------------------------------------------------

#[test]
fn i_migrating_a_v1_database_keeps_rows_and_adds_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");
    {
        let conn = Connection::open(&path).unwrap();
        schema::apply_writer_pragmas(&conn).unwrap();
        conn.execute_batch(V1_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
        conn.execute(
            "INSERT INTO storage_roots(id, path, state, added_at_ms) \
             VALUES (1, 'C:\\Medal\\Clips', 'active', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO games(id, display_name, folder_name, created_at_ms, updated_at_ms) \
             VALUES ('g1', 'Apex', 'Apex', 1, 1)",
            [],
        )
        .unwrap();
        for (n, name, ext) in [
            (1u64, "MedalTVApex1.mp4", "mp4"),
            (2, "MedalTVApex2.mkv", "mkv"),
        ] {
            conn.execute(
                "INSERT INTO recordings (id, kind, game_id, root_id, rel_path, file_name, ext, \
                 size_bytes, file_mtime_ms, created_at_ms, added_at_ms, duration_ms, status) \
                 VALUES (?1, 'clip', 'g1', 1, ?2, ?3, ?4, 4096, 111, 111, 112, 1500, 'ready')",
                rusqlite::params![oid(n), format!("Apex\\{name}"), name, ext],
            )
            .unwrap();
        }
    }

    let lib = Library::open(&path).expect("open migrates v1");
    {
        let c = raw(&path);
        assert_eq!(schema::user_version(&c).unwrap(), 2);
        let (rpath, ro, label, avail, scan): (String, i64, String, String, Option<i64>) = c
            .query_row(
                "SELECT path, read_only, label, availability, last_scan_unix_ms \
                 FROM storage_roots WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(rpath, "C:\\Medal\\Clips");
        assert_eq!(
            (ro, label.as_str(), avail.as_str(), scan),
            (0, "", "online", None)
        );

        let mut stmt = c
            .prepare("SELECT id, source, playable, rel_path FROM recordings ORDER BY id")
            .unwrap();
        let rows: Vec<(String, String, i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 2, "rows survive the migration");
        for (id, source, playable, rel) in &rows {
            assert_eq!(source, "recorded", "{id} gets the default source");
            assert_eq!(*playable, 1, "{id} gets the default playable");
            assert!(rel.starts_with("Apex\\"));
        }
    }

    // migrated rows are still listed, with their game
    let items = all_rows(&lib);
    assert_eq!(items.len(), 2);
    let r1 = items.iter().find(|r| r.id == oid(1)).unwrap();
    assert_eq!(r1.game_name.as_deref(), Some("Apex"));
    assert_eq!(r1.size_bytes, 4096);
    drop(lib);

    // re-opening is idempotent
    let lib = Library::open(&path).expect("second open");
    assert_eq!(all_rows(&lib).len(), 2);
    assert_eq!(schema::user_version(&raw(&path)).unwrap(), 2);
    drop(lib);
}

// ---------------------------------------------------------------------------
// j. game_id_for_folder
// ---------------------------------------------------------------------------

#[test]
fn j_game_id_for_folder_is_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, _) = file_lib(dir.path());
    lib.upsert_game(
        &NewGame {
            id: "g-apex".into(),
            display_name: "Apex Legends".into(),
            folder_name: "Apex".into(),
            exe_name: None,
            store_id: None,
        },
        1,
    )
    .unwrap();

    assert_eq!(
        lib.game_id_for_folder("apex").unwrap(),
        Some("g-apex".into())
    );
    assert_eq!(
        lib.game_id_for_folder("APEX").unwrap(),
        Some("g-apex".into())
    );
    assert_eq!(lib.game_id_for_folder("Valorant").unwrap(), None);
}
