//! Integration tests for the Gallery command layer (`clipcheck_ui_lib::commands`).
//!
//! Dev-seed tests are gated per item with `#[cfg(debug_assertions)]`, so the rest of the file
//! still runs under `cargo test --release`.

use std::collections::HashSet;
use std::path::PathBuf;

use clipcheck_library::{Library, NewGame, NewRecording, RecStatus, RecordingKind};
use clipcheck_ui_lib::commands::{
    db_path_from, dev_seed_requested, page_at, query_page, query_stats, resolve_db_path, stats_at,
    CommandError, ListQueryDto, PageDto, RecordingDto, SortDto, StatsDto, DEFAULT_PAGE_SIZE,
};
#[cfg(debug_assertions)]
use clipcheck_ui_lib::commands::{dev_seed, DEV_SEED_COUNT};

const NOW: i64 = 1_800_000_000_000;

fn ulid(n: u32) -> String {
    format!("01TST{n:021}")
}

fn setup() -> (tempfile::TempDir, Library, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let lib = Library::open(dir.path().join("library.db")).expect("open");
    let root = lib.ensure_storage_root(r"C:\Clips", NOW).expect("root");
    (dir, lib, root)
}

fn rec(n: u32, root: i64, file: &str, created: i64, size: i64, dur: i64) -> NewRecording {
    let ext = file.rsplit('.').next().unwrap_or("mp4").to_owned();
    NewRecording {
        id: ulid(n),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id: root,
        rel_path: format!("clips\\{file}"),
        file_name: file.to_owned(),
        ext,
        size_bytes: size,
        file_mtime_ms: created,
        created_at_ms: created,
        added_at_ms: created,
        duration_ms: dur,
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

fn session_part(
    n: u32,
    root: i64,
    file: &str,
    created: i64,
    session: u32,
    part: i64,
) -> NewRecording {
    let mut r = rec(n, root, file, created, 1_000, 1_000);
    r.kind = RecordingKind::Session;
    r.session_id = Some(format!("01SES{session:021}"));
    r.part_index = Some(part);
    r
}

fn add_games(lib: &Library) {
    for (id, name, folder) in [
        ("game-apex", "Apex Legends", "apex"),
        ("game-valorant", "Valorant", "valorant"),
    ] {
        lib.upsert_game(
            &NewGame {
                id: id.to_owned(),
                display_name: name.to_owned(),
                folder_name: folder.to_owned(),
                exe_name: None,
                store_id: None,
            },
            NOW,
        )
        .expect("upsert game");
    }
}

/// Inserts `count` clips named `clip_001.mp4` .. with distinct creation times.
fn fill(lib: &Library, root: i64, count: u32) {
    for n in 1..=count {
        let size = i64::from(n * 37 % 101);
        let dur = i64::from(n * 53 % 97) * 1_000;
        let created = 1_000_000 + i64::from(n) * 1_000;
        lib.insert_recording(&rec(
            n,
            root,
            &format!("clip_{n:03}.mp4"),
            created,
            size,
            dur,
        ))
        .expect("insert");
    }
}

/// Follows `nextCursor` until it is null and returns every page in order.
fn collect_pages(lib: &Library, mut dto: ListQueryDto) -> Vec<PageDto> {
    let mut pages = Vec::new();
    loop {
        let page = query_page(lib, &dto).expect("page");
        let next = page.next_cursor.clone();
        pages.push(page);
        assert!(pages.len() < 100, "pagination did not terminate");
        match next {
            Some(c) => dto.cursor = Some(c),
            None => break,
        }
    }
    pages
}

fn file_names(lib: &Library, dto: &ListQueryDto) -> Vec<String> {
    query_page(lib, dto)
        .expect("page")
        .items
        .into_iter()
        .map(|r| r.file_name)
        .collect()
}

fn sorted_names(lib: &Library, dto: &ListQueryDto) -> Vec<String> {
    let mut v = file_names(lib, dto);
    v.sort();
    v
}

// ---- 1. Empty database -------------------------------------------------------------------

#[test]
fn empty_library_lists_nothing_and_zero_stats() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().join("library.db")).unwrap();
    let page = query_page(&lib, &ListQueryDto::default()).unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next_cursor, None);
    assert_eq!(query_stats(&lib).unwrap(), StatsDto::default());
}

// ---- 2. Missing database file ------------------------------------------------------------

#[test]
fn missing_database_reads_as_empty_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nope.db");
    assert_eq!(
        page_at(&db, &ListQueryDto::default()).unwrap(),
        PageDto::default()
    );
    assert_eq!(stats_at(&db).unwrap(), StatsDto::default());
    assert!(!db.exists(), "reading a missing library must not create it");
}

// ---- 3. page_at / stats_at on an existing file -------------------------------------------

#[test]
fn page_at_matches_query_page_on_same_db() {
    let (dir, lib, root) = setup();
    lib.insert_recording(&rec(1, root, "a.mp4", 1_000, 10, 100))
        .unwrap();
    lib.insert_recording(&rec(2, root, "b.mp4", 2_000, 20, 200))
        .unwrap();
    let dto = ListQueryDto::default();
    let via_lib = query_page(&lib, &dto).unwrap();
    let stats_lib = query_stats(&lib).unwrap();
    drop(lib);

    let db = dir.path().join("library.db");
    assert_eq!(via_lib.items.len(), 2);
    assert_eq!(page_at(&db, &dto).unwrap(), via_lib);
    assert_eq!(stats_at(&db).unwrap(), stats_lib);
    assert_eq!(stats_lib.recordings, 2);
}

// ---- 4. Sorting --------------------------------------------------------------------------

fn sort_fixture() -> (tempfile::TempDir, Library) {
    let (dir, lib, root) = setup();
    lib.insert_recording(&rec(1, root, "beta.mp4", 3_000, 300, 30_000))
        .unwrap();
    lib.insert_recording(&rec(2, root, "Alpha.mp4", 1_000, 100, 90_000))
        .unwrap();
    lib.insert_recording(&rec(3, root, "gamma.mkv", 2_000, 500, 10_000))
        .unwrap();
    lib.insert_recording(&rec(4, root, "Delta.mp4", 5_000, 200, 60_000))
        .unwrap();
    (dir, lib)
}

fn order(lib: &Library, sort: SortDto) -> Vec<String> {
    file_names(
        lib,
        &ListQueryDto {
            sort,
            ..Default::default()
        },
    )
}

#[test]
fn each_sort_orders_as_documented() {
    let (_dir, lib) = sort_fixture();
    // created: Delta 5000, beta 3000, gamma 2000, Alpha 1000
    assert_eq!(
        order(&lib, SortDto::Newest),
        ["Delta.mp4", "beta.mp4", "gamma.mkv", "Alpha.mp4"]
    );
    assert_eq!(
        order(&lib, SortDto::Oldest),
        ["Alpha.mp4", "gamma.mkv", "beta.mp4", "Delta.mp4"]
    );
    // case-insensitive name order (binary order would put "Delta" before "beta")
    assert_eq!(
        order(&lib, SortDto::NameAsc),
        ["Alpha.mp4", "beta.mp4", "Delta.mp4", "gamma.mkv"]
    );
    assert_eq!(
        order(&lib, SortDto::NameDesc),
        ["gamma.mkv", "Delta.mp4", "beta.mp4", "Alpha.mp4"]
    );
    // size: gamma 500, beta 300, Delta 200, Alpha 100
    assert_eq!(
        order(&lib, SortDto::Largest),
        ["gamma.mkv", "beta.mp4", "Delta.mp4", "Alpha.mp4"]
    );
    assert_eq!(
        order(&lib, SortDto::Smallest),
        ["Alpha.mp4", "Delta.mp4", "beta.mp4", "gamma.mkv"]
    );
    // duration: Alpha 90s, Delta 60s, beta 30s, gamma 10s
    assert_eq!(
        order(&lib, SortDto::Longest),
        ["Alpha.mp4", "Delta.mp4", "beta.mp4", "gamma.mkv"]
    );
}

// ---- 5. Text filter ----------------------------------------------------------------------

#[test]
fn text_filter_matches_file_name_and_game_name_with_prefix() {
    let (_dir, lib, root) = setup();
    add_games(&lib);
    let mut r1 = rec(1, root, "clip_one.mp4", 1_000, 10, 100);
    r1.game_id = Some("game-apex".into()); // matches via game display name "Apex Legends"
    let mut r2 = rec(2, root, "apex_highlight.mp4", 2_000, 10, 100);
    r2.game_id = Some("game-valorant".into()); // matches via file name only
    let r3 = rec(3, root, "other.mp4", 3_000, 10, 100);
    lib.insert_recording(&r1).unwrap();
    lib.insert_recording(&r2).unwrap();
    lib.insert_recording(&r3).unwrap();

    let text = |t: &str| ListQueryDto {
        text: Some(t.into()),
        ..Default::default()
    };

    let both = ["apex_highlight.mp4", "clip_one.mp4"];
    assert_eq!(sorted_names(&lib, &text("apex")), both);
    assert_eq!(sorted_names(&lib, &text("ap")), both, "prefix match");
    assert_eq!(
        sorted_names(&lib, &text("  APEX ")),
        both,
        "trimmed and case-insensitive"
    );
    assert_eq!(
        sorted_names(&lib, &text("valorant")),
        ["apex_highlight.mp4"],
        "game name"
    );
    assert!(sorted_names(&lib, &text("zzz")).is_empty());
    assert_eq!(
        sorted_names(&lib, &text("   ")).len(),
        3,
        "blank text means no filter"
    );
}

// ---- 6. Kind filter ----------------------------------------------------------------------

#[test]
fn kind_filter_returns_only_requested_kind() {
    let (_dir, lib, root) = setup();
    lib.insert_recording(&rec(1, root, "c1.mp4", 1_000, 10, 100))
        .unwrap();
    lib.insert_recording(&rec(2, root, "c2.mp4", 2_000, 10, 100))
        .unwrap();
    lib.insert_recording(&session_part(3, root, "s1.mkv", 3_000, 1, 1))
        .unwrap();
    lib.insert_recording(&session_part(4, root, "s2.mkv", 4_000, 2, 1))
        .unwrap();

    let clips = ListQueryDto {
        kind: Some(RecordingKind::Clip),
        ..Default::default()
    };
    let sessions = ListQueryDto {
        kind: Some(RecordingKind::Session),
        ..Default::default()
    };
    assert_eq!(sorted_names(&lib, &clips), ["c1.mp4", "c2.mp4"]);
    assert_eq!(sorted_names(&lib, &sessions), ["s1.mkv", "s2.mkv"]);
    assert_eq!(sorted_names(&lib, &ListQueryDto::default()).len(), 4);
}

// ---- 7. Game filter ----------------------------------------------------------------------

#[test]
fn game_filter_restricts_to_one_game_and_blank_means_all() {
    let (_dir, lib, root) = setup();
    add_games(&lib);
    let mut r1 = rec(1, root, "a.mp4", 1_000, 10, 100);
    r1.game_id = Some("game-apex".into());
    let mut r2 = rec(2, root, "b.mp4", 2_000, 10, 100);
    r2.game_id = Some("game-valorant".into());
    let r3 = rec(3, root, "c.mp4", 3_000, 10, 100);
    lib.insert_recording(&r1).unwrap();
    lib.insert_recording(&r2).unwrap();
    lib.insert_recording(&r3).unwrap();

    let game = |g: Option<&str>| ListQueryDto {
        game: g.map(Into::into),
        ..Default::default()
    };
    assert_eq!(sorted_names(&lib, &game(Some("game-apex"))), ["a.mp4"]);
    assert_eq!(
        sorted_names(&lib, &game(Some(""))).len(),
        3,
        "blank game means all games"
    );
    assert_eq!(sorted_names(&lib, &game(Some("   "))).len(), 3);
    assert_eq!(sorted_names(&lib, &game(None)).len(), 3);
}

// ---- 8. Favorites ------------------------------------------------------------------------

#[test]
fn favorites_only_returns_starred_rows() {
    let (_dir, lib, root) = setup();
    for n in 1..=5 {
        lib.insert_recording(&rec(
            n,
            root,
            &format!("f{n}.mp4"),
            1_000 * i64::from(n),
            10,
            100,
        ))
        .unwrap();
    }
    lib.set_favorite(&ulid(2), true).unwrap();
    lib.set_favorite(&ulid(4), true).unwrap();

    let dto = ListQueryDto {
        favorites_only: true,
        ..Default::default()
    };
    assert_eq!(sorted_names(&lib, &dto), ["f2.mp4", "f4.mp4"]);
    let page = query_page(&lib, &dto).unwrap();
    assert!(page.items.iter().all(|r| r.is_favorite));
}

// ---- 9. Pagination -----------------------------------------------------------------------

#[test]
fn pages_of_25_cover_all_60_rows_without_duplicates() {
    let (_dir, lib, root) = setup();
    fill(&lib, root, 60);
    let dto = ListQueryDto {
        limit: Some(25),
        ..Default::default()
    };
    let pages = collect_pages(&lib, dto);

    let lens: Vec<usize> = pages.iter().map(|p| p.items.len()).collect();
    assert_eq!(lens, [25, 25, 10]);
    assert!(pages[0].next_cursor.is_some());
    assert!(pages[1].next_cursor.is_some());
    assert_eq!(pages[2].next_cursor, None);

    let ids: Vec<&str> = pages
        .iter()
        .flat_map(|p| p.items.iter().map(|r| r.id.as_str()))
        .collect();
    let unique: HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(ids.len(), 60, "no row repeated or dropped");
    assert_eq!(unique.len(), 60);
    let expected: HashSet<String> = (1..=60).map(ulid).collect();
    assert_eq!(
        unique,
        expected.iter().map(String::as_str).collect::<HashSet<_>>()
    );
}

#[test]
fn pagination_follows_requested_sort_order() {
    let (_dir, lib, root) = setup();
    fill(&lib, root, 60);

    let newest = collect_pages(
        &lib,
        ListQueryDto {
            limit: Some(25),
            ..Default::default()
        },
    );
    let created: Vec<i64> = newest
        .iter()
        .flat_map(|p| p.items.iter().map(|r| r.created_at_ms))
        .collect();
    assert!(
        created.windows(2).all(|w| w[0] > w[1]),
        "newest: strictly descending across pages"
    );

    let by_name = collect_pages(
        &lib,
        ListQueryDto {
            sort: SortDto::NameAsc,
            limit: Some(25),
            ..Default::default()
        },
    );
    let names: Vec<String> = by_name
        .iter()
        .flat_map(|p| p.items.iter().map(|r| r.file_name.to_lowercase()))
        .collect();
    assert_eq!(names.len(), 60);
    assert!(
        names.windows(2).all(|w| w[0] < w[1]),
        "name_asc: strictly ascending across pages"
    );
    assert_eq!(names.first().map(String::as_str), Some("clip_001.mp4"));
    assert_eq!(names.last().map(String::as_str), Some("clip_060.mp4"));
}

#[test]
fn absent_limit_uses_default_page_size_of_60() {
    assert_eq!(DEFAULT_PAGE_SIZE, 60);
    let (_dir, lib, root) = setup();
    fill(&lib, root, 60);
    let page = query_page(&lib, &ListQueryDto::default()).unwrap();
    assert_eq!(page.items.len(), 60);
    assert_eq!(page.next_cursor, None);
}

// ---- 10. Cursor replay against a different query -----------------------------------------

#[test]
fn cursor_from_one_query_is_rejected_by_another() {
    let (_dir, lib, root) = setup();
    fill(&lib, root, 10);
    let first = query_page(
        &lib,
        &ListQueryDto {
            limit: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    let cursor = first
        .next_cursor
        .expect("cursor on a 10-row listing with limit 3");

    let other_sort = ListQueryDto {
        sort: SortDto::Oldest,
        limit: Some(3),
        cursor: Some(cursor.clone()),
        ..Default::default()
    };
    assert!(query_page(&lib, &other_sort).is_err(), "sort changed");

    let other_text = ListQueryDto {
        text: Some("clip".into()),
        limit: Some(3),
        cursor: Some(cursor),
        ..Default::default()
    };
    assert!(query_page(&lib, &other_text).is_err(), "filter changed");
}

// ---- 11. Malformed cursors ---------------------------------------------------------------

#[test]
fn malformed_cursor_is_an_error_not_a_panic() {
    let (_dir, lib, root) = setup();
    fill(&lib, root, 3);
    for bad in ["zz", "not-hex", "abc", "00"] {
        let dto = ListQueryDto {
            cursor: Some(bad.into()),
            ..Default::default()
        };
        assert!(
            query_page(&lib, &dto).is_err(),
            "cursor {bad:?} should be rejected"
        );
    }
}

// ---- 12. Dev seed (debug builds only) ----------------------------------------------------

#[cfg(debug_assertions)]
#[test]
fn dev_seed_writes_60_mixed_rows() {
    let lib = Library::open_in_memory().unwrap();
    assert_eq!(DEV_SEED_COUNT, 60);
    assert_eq!(dev_seed(&lib, DEV_SEED_COUNT, NOW).unwrap(), 60);

    let stats = query_stats(&lib).unwrap();
    assert_eq!(stats.recordings, 60);
    assert_eq!(stats.clips, 40);
    assert_eq!(stats.sessions, 20);
    assert!(stats.total_duration_ms > 0);
    assert!(stats.total_size_bytes > 0);

    let page = query_page(
        &lib,
        &ListQueryDto {
            limit: Some(200),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page.items.len(), 60);
    assert!(page.items.iter().all(|r| r.status == RecStatus::Ready));

    let sessions: Vec<&RecordingDto> = page
        .items
        .iter()
        .filter(|r| r.kind == RecordingKind::Session)
        .collect();
    assert_eq!(sessions.len(), 20);
    for s in &sessions {
        assert!(s.session_id.is_some());
        assert!(
            matches!(s.part_index, Some(1) | Some(2)),
            "part_index {:?}",
            s.part_index
        );
    }

    assert!(
        page.items.iter().any(|r| r.game_name.is_none()),
        "some rows without a game"
    );
    assert!(
        page.items.iter().any(|r| r.game_name.is_some()),
        "some rows with a game"
    );

    let favs = query_page(
        &lib,
        &ListQueryDto {
            favorites_only: true,
            limit: Some(200),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        favs.items.len(),
        9,
        "favorites on i % 7 == 0 for i in 0..60"
    );
}

// ---- 13. Path resolution and dev-seed env parsing ----------------------------------------

#[test]
fn db_path_resolution_precedence() {
    assert_eq!(
        db_path_from(Some(r"D:\data"), None),
        PathBuf::from(r"D:\data\library.db")
    );
    assert_eq!(
        db_path_from(Some("   "), Some(r"C:\Users\someone\AppData\Roaming")),
        PathBuf::from(r"C:\Users\someone\AppData\Roaming\ClipCheck\library.db")
    );
    assert_eq!(
        db_path_from(Some(""), Some(r"C:\Users\someone\AppData\Roaming")),
        PathBuf::from(r"C:\Users\someone\AppData\Roaming\ClipCheck\library.db")
    );
    assert_eq!(
        db_path_from(Some(r"D:\data"), Some(r"C:\Users\someone\AppData\Roaming")),
        PathBuf::from(r"D:\data\library.db"),
        "data dir wins over appdata"
    );
    assert_eq!(db_path_from(None, None), PathBuf::from("library.db"));
}

#[test]
fn dev_seed_env_only_accepts_one() {
    assert!(dev_seed_requested(Some("1")));
    assert!(dev_seed_requested(Some(" 1 ")));
    assert!(!dev_seed_requested(Some("true")));
    assert!(!dev_seed_requested(Some("0")));
    assert!(!dev_seed_requested(Some("")));
    assert!(!dev_seed_requested(None));
}

// ---- 14. Serde wire format ---------------------------------------------------------------

#[test]
fn list_query_dto_parses_camel_case_wire_format() {
    let json = r#"{"text":"x","favoritesOnly":true,"sort":"name_asc","kind":"clip","limit":25,"cursor":null}"#;
    let dto: ListQueryDto = serde_json::from_str(json).expect("parse");
    assert_eq!(dto.text.as_deref(), Some("x"));
    assert!(dto.favorites_only);
    assert_eq!(dto.sort, SortDto::NameAsc);
    assert_eq!(dto.kind, Some(RecordingKind::Clip));
    assert_eq!(dto.limit, Some(25));
    assert_eq!(dto.cursor, None);
    assert_eq!(dto.game, None, "missing game defaults to None");

    let empty: ListQueryDto = serde_json::from_str("{}").expect("empty object");
    assert_eq!(empty, ListQueryDto::default());
}

#[test]
fn page_and_recording_serialize_camel_case_lowercase_status() {
    let lib = Library::open_in_memory().unwrap();
    let root = lib.ensure_storage_root(r"C:\Clips", NOW).unwrap();
    lib.insert_recording(&rec(1, root, "a.mp4", 1_000, 10, 100))
        .unwrap();
    let page = query_page(&lib, &ListQueryDto::default()).unwrap();
    let v = serde_json::to_value(&page).expect("serialize");

    assert!(v["items"].is_array());
    assert!(v.get("nextCursor").is_some(), "nextCursor key present");
    assert!(v["nextCursor"].is_null());

    let item = &v["items"][0];
    assert_eq!(item["fileName"], "a.mp4");
    assert_eq!(item["sizeBytes"], 10);
    assert_eq!(item["durationMs"], 100);
    assert_eq!(item["createdAtMs"], 1_000);
    assert_eq!(item["isFavorite"], false);
    assert_eq!(item["status"], "ready");
    assert_eq!(item["kind"], "clip");
}

// ---- 15. resolve_db_path (no working-directory fallback, #85) ----------------------------

#[test]
fn resolve_db_path_data_dir_names_the_library_file() {
    assert_eq!(
        resolve_db_path(Some(r"D:\data"), None).expect("data dir"),
        PathBuf::from(r"D:\data\library.db")
    );
    assert_eq!(
        resolve_db_path(Some(r"  D:\data  "), None).expect("trimmed data dir"),
        PathBuf::from(r"D:\data\library.db"),
        "surrounding whitespace is ignored"
    );
}

#[test]
fn resolve_db_path_appdata_only_uses_clipcheck_subdirectory() {
    assert_eq!(
        resolve_db_path(None, Some(r"C:\Users\someone\AppData\Roaming")).expect("appdata"),
        PathBuf::from(r"C:\Users\someone\AppData\Roaming\ClipCheck\library.db")
    );
}

#[test]
fn resolve_db_path_without_any_value_is_no_data_dir_error() {
    assert!(
        matches!(resolve_db_path(None, None), Err(CommandError::NoDataDir)),
        "neither value set must fail, not fall back to ./library.db"
    );
}

#[test]
fn resolve_db_path_blank_values_count_as_unset() {
    for (data, app) in [
        (Some(""), None),
        (Some("   "), None),
        (None, Some("")),
        (None, Some("  \t ")),
        (Some(""), Some("")),
        (Some("   "), Some("   ")),
    ] {
        assert!(
            matches!(resolve_db_path(data, app), Err(CommandError::NoDataDir)),
            "data={data:?} appdata={app:?} should be NoDataDir"
        );
    }
}

#[test]
fn resolve_db_path_blank_data_dir_falls_through_to_appdata() {
    assert_eq!(
        resolve_db_path(Some("   "), Some(r"C:\Users\someone\AppData\Roaming")).expect("appdata"),
        PathBuf::from(r"C:\Users\someone\AppData\Roaming\ClipCheck\library.db")
    );
}

#[test]
fn resolve_db_path_data_dir_wins_over_appdata() {
    assert_eq!(
        resolve_db_path(Some(r"D:\data"), Some(r"C:\Users\someone\AppData\Roaming")).expect("both"),
        PathBuf::from(r"D:\data\library.db")
    );
}

#[test]
fn no_data_dir_error_names_the_environment_variables() {
    let msg = CommandError::NoDataDir.to_string();
    assert!(msg.contains("CLIPCHECK_DATA_DIR"), "message: {msg}");
    assert!(msg.contains("APPDATA"), "message: {msg}");
}
