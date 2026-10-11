//! Integration tests for `Library::stats` (GitHub #131).
//!
//! All tests use in-memory libraries, so nothing touches the real data folder.

use clipcheck_library::query::LibraryStats;
use clipcheck_library::{
    Library, ListQuery, NewGame, NewRecording, RecStatus, RecordingKind, RecordingRow, SortKey,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A 26-character id derived from a number (zero-padded).
fn oid(n: u64) -> String {
    format!("{:026}", n)
}

/// Base recording: a ready clip with no game. Callers adjust the fields they care about.
fn base(n: u64, root: i64) -> NewRecording {
    NewRecording {
        id: oid(n),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id: root,
        rel_path: format!("u/{}/Clip{:03}.mp4", oid(n), n),
        file_name: format!("Clip{:03}.mp4", n),
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

/// Session part: sets kind, session id and part index.
fn session_part(n: u64, root: i64) -> NewRecording {
    let mut r = base(n, root);
    r.kind = RecordingKind::Session;
    r.session_id = Some(oid(90_000 + n));
    r.part_index = Some(1);
    r
}

fn new_game(id: &str, name: &str) -> NewGame {
    NewGame {
        id: id.to_string(),
        display_name: name.to_string(),
        folder_name: name.to_string(),
        exe_name: None,
        store_id: None,
    }
}

fn lib_with_root() -> (Library, i64) {
    let lib = Library::open_in_memory().expect("open_in_memory");
    let root = lib.ensure_storage_root("C:\\Clips", 1).expect("root");
    (lib, root)
}

/// Walks every page of `q` with the given limit and returns all rows.
fn walk_all(lib: &Library, mut q: ListQuery, limit: u32) -> Vec<RecordingRow> {
    q.limit = limit;
    let mut all = Vec::new();
    let mut pages = 0usize;
    loop {
        let page = lib.list(&q).expect("list page");
        assert!(page.items.len() <= limit as usize, "page exceeds limit");
        all.extend(page.items);
        pages += 1;
        assert!(pages < 10_000, "pagination did not terminate");
        match page.next_cursor {
            Some(c) => q.cursor = Some(c),
            None => break,
        }
    }
    all
}

fn is_clip(r: &RecordingRow) -> bool {
    matches!(r.kind, RecordingKind::Clip)
}

// ---------------------------------------------------------------------------
// 1. empty library
// ---------------------------------------------------------------------------

#[test]
fn empty_library_stats_are_all_zero() {
    let (lib, _root) = lib_with_root();
    let s = lib.stats(&ListQuery::default()).expect("stats");
    assert_eq!(s, LibraryStats::default());
    assert_eq!(s.recordings, 0);
    assert_eq!(s.clips, 0);
    assert_eq!(s.sessions, 0);
    assert_eq!(s.total_size_bytes, 0);
    assert_eq!(s.total_duration_ms, 0);
}

#[test]
fn library_without_any_root_is_all_zero() {
    let lib = Library::open_in_memory().expect("open_in_memory");
    let s = lib.stats(&ListQuery::default()).expect("stats");
    assert_eq!(s, LibraryStats::default());
}

// ---------------------------------------------------------------------------
// 2. known totals
// ---------------------------------------------------------------------------

#[test]
fn mixed_rows_give_exact_totals_and_counts() {
    let (lib, root) = lib_with_root();
    lib.upsert_game(&new_game("exe:a.exe", "Alpha"), 1)
        .expect("game");

    let mut r1 = base(1, root);
    r1.game_id = Some("exe:a.exe".into());
    r1.size_bytes = 1_000;
    r1.duration_ms = 5_000;
    lib.insert_recording(&r1).expect("r1");

    let mut r2 = base(2, root);
    r2.game_id = Some("exe:a.exe".into());
    r2.size_bytes = 2_000;
    r2.duration_ms = 7_000;
    r2.status = RecStatus::Pending;
    lib.insert_recording(&r2).expect("r2");

    let mut r3 = base(3, root);
    r3.size_bytes = 3_000;
    r3.duration_ms = 0;
    lib.insert_recording(&r3).expect("r3");

    let mut s1 = session_part(4, root);
    s1.game_id = Some("exe:a.exe".into());
    s1.size_bytes = 4_000;
    s1.duration_ms = 60_000;
    lib.insert_recording(&s1).expect("s1");

    let mut s2 = session_part(5, root);
    s2.size_bytes = 5_000;
    s2.duration_ms = 120_000;
    s2.status = RecStatus::Pending;
    lib.insert_recording(&s2).expect("s2");

    let s = lib.stats(&ListQuery::default()).expect("stats");
    assert_eq!(s.recordings, 5);
    assert_eq!(s.clips, 3);
    assert_eq!(s.sessions, 2);
    assert_eq!(s.total_size_bytes, 15_000);
    assert_eq!(s.total_duration_ms, 192_000);
}

#[test]
fn zero_size_and_duration_rows_are_counted_but_add_nothing() {
    let (lib, root) = lib_with_root();
    let mut r = base(1, root);
    r.size_bytes = 0;
    r.duration_ms = 0;
    lib.insert_recording(&r).expect("insert");
    let s = lib.stats(&ListQuery::default()).expect("stats");
    assert_eq!(s.recordings, 1);
    assert_eq!(s.clips, 1);
    assert_eq!(s.sessions, 0);
    assert_eq!(s.total_size_bytes, 0);
    assert_eq!(s.total_duration_ms, 0);
}

// ---------------------------------------------------------------------------
// 3. cross-check against paged list
// ---------------------------------------------------------------------------

/// Builds a varied dataset: three games (two named, one unknown), mixed kinds, favorites,
/// sizes and durations, and a sprinkling of non-listed statuses.
fn varied_library() -> (Library, i64) {
    let (lib, root) = lib_with_root();
    lib.upsert_game(&new_game("exe:a.exe", "Alpha"), 1)
        .expect("game a");
    lib.upsert_game(&new_game("exe:b.exe", "Bravo"), 1)
        .expect("game b");

    for i in 0..40u64 {
        let mut r = if i % 3 == 0 {
            session_part(i + 1, root)
        } else {
            base(i + 1, root)
        };
        r.game_id = match i % 3 {
            0 => Some("exe:a.exe".into()),
            1 => Some("exe:b.exe".into()),
            _ => None,
        };
        r.size_bytes = ((i as i64 * 137) % 1000 + 1) * 100;
        r.duration_ms = (i as i64) * 1_000 + 500;
        r.status = match i % 8 {
            5 => RecStatus::Recording,
            6 => RecStatus::Missing,
            7 => RecStatus::Error,
            n if n % 2 == 0 => RecStatus::Ready,
            _ => RecStatus::Pending,
        };
        lib.insert_recording(&r).expect("insert");
        if i % 4 == 0 {
            lib.set_favorite(&oid(i + 1), true).expect("favorite");
        }
    }
    (lib, root)
}

fn query_variants() -> Vec<(&'static str, ListQuery)> {
    let base_q = ListQuery::default();
    vec![
        ("default", base_q.clone()),
        (
            "game=a",
            ListQuery {
                game: Some("exe:a.exe".into()),
                ..base_q.clone()
            },
        ),
        (
            "game=b",
            ListQuery {
                game: Some("exe:b.exe".into()),
                ..base_q.clone()
            },
        ),
        (
            "kind=clip",
            ListQuery {
                kind: Some(RecordingKind::Clip),
                ..base_q.clone()
            },
        ),
        (
            "kind=session",
            ListQuery {
                kind: Some(RecordingKind::Session),
                ..base_q.clone()
            },
        ),
        (
            "favorites",
            ListQuery {
                favorites_only: true,
                ..base_q.clone()
            },
        ),
        (
            "game+kind+favorites",
            ListQuery {
                game: Some("exe:a.exe".into()),
                kind: Some(RecordingKind::Session),
                favorites_only: true,
                ..base_q.clone()
            },
        ),
        (
            "sizes",
            ListQuery {
                min_size: Some(20_000),
                max_size: Some(80_000),
                ..base_q.clone()
            },
        ),
        (
            "text",
            ListQuery {
                text: Some("Clip01".into()),
                ..base_q.clone()
            },
        ),
    ]
}

#[test]
fn stats_matches_sums_from_paged_list_for_every_filter() {
    let (lib, _root) = varied_library();

    for (label, q) in query_variants() {
        let rows = walk_all(&lib, q.clone(), 2);
        let want_recordings = rows.len() as u64;
        let want_clips = rows.iter().filter(|r| is_clip(r)).count() as u64;
        let want_sessions = rows.len() as u64 - want_clips;
        let want_size: i64 = rows.iter().map(|r| r.size_bytes).sum();
        let want_dur: i64 = rows.iter().map(|r| r.duration_ms).sum();

        let got = lib
            .stats(&q)
            .unwrap_or_else(|e| panic!("{label}: stats failed: {e:?}"));
        assert_eq!(got.recordings, want_recordings, "{label}: recordings");
        assert_eq!(got.clips, want_clips, "{label}: clips");
        assert_eq!(got.sessions, want_sessions, "{label}: sessions");
        assert_eq!(got.total_size_bytes, want_size, "{label}: total_size_bytes");
        assert_eq!(
            got.total_duration_ms, want_dur,
            "{label}: total_duration_ms"
        );
        assert_eq!(
            got.clips + got.sessions,
            got.recordings,
            "{label}: clips + sessions must equal recordings"
        );
    }
}

#[test]
fn cross_check_is_not_vacuous() {
    // Guards the cross-check above: the dataset must produce non-empty results for the
    // filters that matter, otherwise the equality would hold trivially.
    let (lib, _root) = varied_library();
    let all = walk_all(&lib, ListQuery::default(), 2);
    assert!(!all.is_empty());
    for (label, q) in query_variants() {
        let s = lib.stats(&q).expect("stats");
        if label == "game+kind+favorites" || label == "text" {
            assert!(s.recordings > 0, "{label} should match at least one row");
        }
    }
}

#[test]
fn stats_ignores_sort_limit_and_cursor() {
    let (lib, _root) = varied_library();
    let plain = lib.stats(&ListQuery::default()).expect("plain");
    let noisy = ListQuery {
        sort: SortKey::SizeAsc,
        limit: 1,
        cursor: Some("not-a-real-cursor".into()),
        ..ListQuery::default()
    };
    let got = lib
        .stats(&noisy)
        .expect("stats must ignore cursor and limit");
    assert_eq!(got, plain);
}

// ---------------------------------------------------------------------------
// 4. non-listed statuses are excluded
// ---------------------------------------------------------------------------

#[test]
fn recording_missing_and_error_rows_are_excluded() {
    let (lib, root) = lib_with_root();

    let mut keep = base(1, root);
    keep.size_bytes = 100;
    keep.duration_ms = 10;
    lib.insert_recording(&keep).expect("keep");

    let mut keep_pending = base(2, root);
    keep_pending.size_bytes = 200;
    keep_pending.duration_ms = 20;
    keep_pending.status = RecStatus::Pending;
    lib.insert_recording(&keep_pending).expect("keep pending");

    for (n, status) in [
        (3u64, RecStatus::Recording),
        (4, RecStatus::Missing),
        (5, RecStatus::Error),
    ] {
        let mut r = base(n, root);
        r.size_bytes = 1_000_000;
        r.duration_ms = 1_000_000;
        r.status = status;
        lib.insert_recording(&r).expect("excluded row");
    }

    let mut excluded_session = session_part(6, root);
    excluded_session.size_bytes = 1_000_000;
    excluded_session.duration_ms = 1_000_000;
    excluded_session.status = RecStatus::Missing;
    lib.insert_recording(&excluded_session)
        .expect("excluded session");

    let s = lib.stats(&ListQuery::default()).expect("stats");
    assert_eq!(s.recordings, 2);
    assert_eq!(s.clips, 2);
    assert_eq!(s.sessions, 0);
    assert_eq!(s.total_size_bytes, 300);
    assert_eq!(s.total_duration_ms, 30);

    let listed = walk_all(&lib, ListQuery::default(), 2);
    assert_eq!(listed.len(), 2, "list and stats must agree on the row set");
}

#[test]
fn library_with_only_non_listed_rows_is_all_zero() {
    let (lib, root) = lib_with_root();
    let mut r = base(1, root);
    r.status = RecStatus::Error;
    r.size_bytes = 500;
    lib.insert_recording(&r).expect("insert error row");
    let s = lib.stats(&ListQuery::default()).expect("stats");
    assert_eq!(s, LibraryStats::default());
    assert!(lib
        .list(&ListQuery::default())
        .expect("list")
        .items
        .is_empty());
}
