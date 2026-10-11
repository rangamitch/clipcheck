//! Tests for issue #39 (cached statements in `Library::list`) and issue #147
//! (`upsert_game` rewrites FTS rows only when the display name changes).
//!
//! Black-box: public API only, plus a separate rusqlite connection to read the raw
//! `recordings_fts` rowids (FTS5 DELETE+INSERT assigns new rowids, so an unchanged rowid
//! set proves no FTS rewrite happened).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use clipcheck_library::{
    LibError, Library, ListQuery, NewGame, NewRecording, Page, RecStatus, RecordingKind,
    RecordingRow, SortKey,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Zero-padded 26-char id so string order == numeric order.
fn oid(n: u64) -> String {
    format!("{:026}", n)
}

fn gm(id: &str, display: &str, folder: &str) -> NewGame {
    NewGame {
        id: id.into(),
        display_name: display.into(),
        folder_name: folder.into(),
        exe_name: Some("game.exe".into()),
        store_id: None,
    }
}

fn nrec(n: u64, root: i64, game: Option<&str>, file_name: &str) -> NewRecording {
    NewRecording {
        id: oid(n),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: game.map(str::to_string),
        root_id: root,
        rel_path: format!("u/{}/{}", oid(n), file_name),
        file_name: file_name.to_string(),
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

fn setup(path: &Path) -> (Library, i64) {
    let lib = Library::open(path).expect("open library");
    let root = lib.ensure_storage_root("C:\\Clips", 1).expect("root");
    (lib, root)
}

/// Pages through a query to the end. Asserts each non-final page is exactly full.
fn list_all(lib: &Library, base: &ListQuery) -> Result<Vec<Page>, LibError> {
    let limit = base.limit.clamp(1, 200) as usize;
    let mut q = base.clone();
    let mut pages = Vec::new();
    loop {
        let page = lib.list(&q)?;
        assert!(page.items.len() <= limit, "page exceeds limit");
        match page.next_cursor.clone() {
            Some(c) => {
                assert_eq!(page.items.len(), limit, "non-final page must be full");
                pages.push(page);
                q.cursor = Some(c);
                assert!(pages.len() < 100_000, "pagination did not terminate");
            }
            None => {
                pages.push(page);
                break;
            }
        }
    }
    Ok(pages)
}

fn flat_ids(pages: &[Page]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|p| p.items.iter().map(|r: &RecordingRow| r.id.clone()))
        .collect()
}

/// Every matching row, across all pages, as ids (paging is part of what is tested).
fn search_ids(lib: &Library, text: &str) -> Vec<String> {
    let q = ListQuery {
        text: Some(text.to_string()),
        limit: 200,
        ..Default::default()
    };
    flat_ids(&list_all(lib, &q).expect("search list"))
}

/// Raw FTS rows (rowid, rec_id) read through a separate connection.
fn fts_rows(path: &Path) -> Vec<(i64, String)> {
    let c = rusqlite::Connection::open(path).expect("side connection");
    let mut st = c
        .prepare("SELECT rowid, rec_id FROM recordings_fts ORDER BY rowid")
        .expect("prepare");
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|x| x.expect("row"))
        .collect()
}

// ---------------------------------------------------------------------------
// #39: cached statements in list()
// ---------------------------------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn next_u(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u() % n
    }
    fn coin(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

const GAMES: [(&str, &str); 5] = [
    ("exe:g0", "Zorbix Arena"),
    ("exe:g1", "Quazar Drift"),
    ("exe:g2", "Plentra Tales"),
    ("exe:g3", "Mirvon Rally"),
    ("exe:g4", "Hexaden Siege"),
];

const WORDS: [&str; 6] = ["clip", "match", "round", "final", "warmup", "replay"];

/// Query-side vocabulary. Includes case variants, prefixes, a non-matching token,
/// punctuation that the query builder strips, and a query that strips to nothing.
const VOCAB: [&str; 22] = [
    "cl", "clip", "CLIP", "mat", "r", "ro", "00", "0", "final", "fi", "wa", "rep", "MP4", "mkv",
    "zor", "Quazar", "plen", "mirv", "hex", "x", "Ar-ena", "!!",
];

/// Mirrors the query builder: keep alphanumerics and underscores, lowercase, drop empties.
fn expr_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Splits into ASCII-alphanumeric runs, lowercased (the unicode61 tokenizer for ASCII text).
fn word_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

#[derive(Clone)]
struct Expect {
    id: String,
    game: Option<String>,
    kind: RecordingKind,
    fav: bool,
    listed: bool,
    file_name: String,
    created: i64,
    size: i64,
    duration: i64,
    tokens: Vec<String>,
}

fn sort_cmp(sort: SortKey, a: &Expect, b: &Expect) -> Ordering {
    let lower = |s: &str| s.to_ascii_lowercase();
    match sort {
        SortKey::Newest => b.created.cmp(&a.created).then(b.id.cmp(&a.id)),
        SortKey::Oldest => a.created.cmp(&b.created).then(a.id.cmp(&b.id)),
        SortKey::NameAsc => lower(&a.file_name)
            .cmp(&lower(&b.file_name))
            .then(a.id.cmp(&b.id)),
        SortKey::NameDesc => lower(&b.file_name)
            .cmp(&lower(&a.file_name))
            .then(b.id.cmp(&a.id)),
        SortKey::SizeDesc => b.size.cmp(&a.size).then(b.id.cmp(&a.id)),
        SortKey::SizeAsc => a.size.cmp(&b.size).then(a.id.cmp(&b.id)),
        SortKey::DurationDesc => b.duration.cmp(&a.duration).then(b.id.cmp(&a.id)),
    }
}

/// Independent reference: filter the inserted rows in Rust, then sort.
fn reference(rows: &[Expect], q: &ListQuery) -> Vec<String> {
    let toks = q.text.as_deref().map(expr_tokens).unwrap_or_default();
    let mut v: Vec<&Expect> = rows
        .iter()
        .filter(|e| e.listed)
        .filter(|e| match &q.game {
            None => true,
            Some(g) => e.game.as_deref() == Some(g.as_str()),
        })
        .filter(|e| q.kind.is_none_or(|k| e.kind == k))
        .filter(|e| !q.favorites_only || e.fav)
        .filter(|e| q.min_size.is_none_or(|m| e.size >= m as i64))
        .filter(|e| q.max_size.is_none_or(|m| e.size <= m as i64))
        .filter(|e| {
            toks.is_empty()
                || toks
                    .iter()
                    .all(|t| e.tokens.iter().any(|w| w.starts_with(t.as_str())))
        })
        .collect();
    v.sort_by(|a, b| sort_cmp(q.sort, a, b));
    v.into_iter().map(|e| e.id.clone()).collect()
}

fn random_query(rng: &mut Lcg) -> ListQuery {
    let text = if rng.coin(55) {
        let n = 1 + rng.below(2) as usize;
        let parts: Vec<&str> = (0..n)
            .map(|_| VOCAB[rng.below(VOCAB.len() as u64) as usize])
            .collect();
        Some(parts.join(" "))
    } else {
        None
    };
    let game = if rng.coin(30) {
        Some(match rng.below(7) {
            5 => "exe:none".to_string(),
            6 => GAMES[0].0.to_string(),
            k => GAMES[k as usize].0.to_string(),
        })
    } else {
        None
    };
    let kind = match rng.below(3) {
        0 => None,
        1 => Some(RecordingKind::Clip),
        _ => Some(RecordingKind::Session),
    };
    let sorts = [
        SortKey::Newest,
        SortKey::Oldest,
        SortKey::NameAsc,
        SortKey::NameDesc,
        SortKey::SizeDesc,
        SortKey::SizeAsc,
        SortKey::DurationDesc,
    ];
    let limits = [0u32, 1, 2, 3, 7, 10, 50, 199, 200, 500];
    ListQuery {
        text,
        game,
        kind,
        favorites_only: rng.coin(25),
        min_size: if rng.coin(20) {
            Some(rng.below(30) * 1000)
        } else {
            None
        },
        max_size: if rng.coin(20) {
            Some(rng.below(50) * 1000)
        } else {
            None
        },
        sort: sorts[rng.below(sorts.len() as u64) as usize],
        limit: limits[rng.below(limits.len() as u64) as usize],
        cursor: None,
    }
}

#[test]
fn issue39_cached_list_matches_reference_under_random_query_sequences() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("lib39.db");
    let (lib, root) = setup(&path);
    for (i, (id, name)) in GAMES.iter().enumerate() {
        lib.upsert_game(&gm(id, name, &format!("f{i}")), 1)
            .expect("upsert game");
    }

    let mut rng = Lcg(0x39_39_39_39);
    let mut rows: Vec<Expect> = Vec::new();
    for n in 0..300u64 {
        let g = rng.below(6) as usize;
        let game = if g == 5 { None } else { Some(GAMES[g]) };
        let kind = if rng.coin(25) {
            RecordingKind::Session
        } else {
            RecordingKind::Clip
        };
        let status = match rng.below(100) {
            0..=69 => RecStatus::Ready,
            70..=84 => RecStatus::Pending,
            85..=89 => RecStatus::Missing,
            90..=94 => RecStatus::Error,
            _ => RecStatus::Recording,
        };
        let word = WORDS[rng.below(WORDS.len() as u64) as usize];
        let word = if rng.coin(50) {
            let mut c = word.chars();
            let first = c.next().expect("nonempty").to_ascii_uppercase();
            format!("{first}{}", c.as_str())
        } else {
            word.to_string()
        };
        let ext = if rng.coin(80) { "mp4" } else { "MKV" };
        let file_name = format!("{word}{:03}.{ext}", rng.below(40));
        let created = 1_000_000 + rng.below(23) as i64 * 1000;
        let size = rng.below(50) as i64 * 1000;
        let duration = rng.below(9) as i64 * 500;
        let fav = rng.coin(30);

        let id = oid(n + 1);
        let mut r = nrec(n + 1, root, game.map(|g| g.0), &file_name);
        r.kind = kind;
        r.status = status;
        r.created_at_ms = created;
        r.size_bytes = size;
        r.duration_ms = duration;
        lib.insert_recording(&r).expect("insert");
        if fav {
            lib.set_favorite(&id, true).expect("favorite");
        }

        let game_name = game.map(|g| g.1);
        let mut tokens = word_tokens(&file_name);
        if let Some(name) = game_name {
            tokens.extend(word_tokens(name));
        }
        rows.push(Expect {
            id,
            game: game.map(|g| g.0.to_string()),
            kind,
            fav,
            listed: matches!(status, RecStatus::Ready | RecStatus::Pending),
            file_name,
            created,
            size,
            duration,
            tokens,
        });
    }
    let listed_count = rows.iter().filter(|e| e.listed).count();
    assert!(listed_count > 150, "dataset should have many listed rows");

    let queries: Vec<ListQuery> = (0..200).map(|_| random_query(&mut rng)).collect();
    let expected: Vec<Vec<String>> = queries.iter().map(|q| reference(&rows, q)).collect();

    // Pass 1: forward order on the writer handle.
    let forward: Vec<Vec<Page>> = queries
        .iter()
        .map(|q| list_all(&lib, q).expect("forward list"))
        .collect();

    // Pass 2: reverse order on the same handle, so cached statements are reused in a
    // different sequence.
    let mut reverse: Vec<Vec<Page>> = vec![Vec::new(); queries.len()];
    for i in (0..queries.len()).rev() {
        reverse[i] = list_all(&lib, &queries[i]).expect("reverse list");
    }

    // Pass 3: second handle, opened read-only on the same file.
    let ro = Library::open_readonly(&path).expect("read-only handle");
    let readonly: Vec<Vec<Page>> = queries
        .iter()
        .map(|q| list_all(&ro, q).expect("readonly list"))
        .collect();

    let mut nonempty = 0usize;
    let mut multi_page = 0usize;
    let mut with_text = 0usize;
    for i in 0..queries.len() {
        let got = flat_ids(&forward[i]);
        let unique: HashSet<&String> = got.iter().collect();
        assert_eq!(
            unique.len(),
            got.len(),
            "query {i}: duplicate rows across pages"
        );
        assert_eq!(
            got, expected[i],
            "query {i} differs from reference: {:?}",
            queries[i]
        );
        assert_eq!(
            reverse[i], forward[i],
            "query {i}: reverse-order pages differ (parameter leakage?)"
        );
        assert_eq!(
            readonly[i], forward[i],
            "query {i}: read-only handle pages differ"
        );
        if !got.is_empty() {
            nonempty += 1;
        }
        if forward[i].len() > 1 {
            multi_page += 1;
        }
        if queries[i].text.is_some() {
            with_text += 1;
        }
    }
    println!(
        "#39 counts: queries=200 nonempty={nonempty} multi_page={multi_page} with_text={with_text} listed_rows={listed_count}"
    );
    assert!(
        nonempty >= 100,
        "too few non-empty queries to be meaningful"
    );
    assert!(
        multi_page >= 20,
        "too few multi-page queries to test paging"
    );
}

// ---------------------------------------------------------------------------
// #147: upsert_game FTS fan-out
// ---------------------------------------------------------------------------

#[test]
fn issue147_same_name_upserts_do_no_fts_writes() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("lib147a.db");
    let (lib, root) = setup(&path);
    lib.upsert_game(&gm("exe:a", "Zorbix Arena", "fa"), 1)
        .expect("upsert");
    for n in 1..=20u64 {
        lib.insert_recording(&nrec(n, root, Some("exe:a"), &format!("clip{n:02}.mp4")))
            .expect("insert");
    }
    let before = fts_rows(&path);
    assert_eq!(before.len(), 20, "one FTS row per recording");

    for i in 0..10i64 {
        lib.upsert_game(&gm("exe:a", "Zorbix Arena", "fa"), 2 + i)
            .expect("same-name upsert");
    }
    assert_eq!(
        fts_rows(&path),
        before,
        "same-name upserts must not rewrite any FTS row"
    );
    assert_eq!(search_ids(&lib, "zorbix").len(), 20);
    assert_eq!(search_ids(&lib, "arena").len(), 20);
}

#[test]
fn issue147_rename_refreshes_every_row_and_drops_old_name() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("lib147b.db");
    let (lib, root) = setup(&path);
    lib.upsert_game(&gm("exe:a", "Zorbix Arena", "fa"), 1)
        .expect("upsert");
    for n in 1..=20u64 {
        lib.insert_recording(&nrec(n, root, Some("exe:a"), &format!("clip{n:02}.mp4")))
            .expect("insert");
    }
    let before = fts_rows(&path);

    lib.upsert_game(&gm("exe:a", "Quazar Drift", "fa"), 2)
        .expect("rename");
    let after = fts_rows(&path);
    // Control: the detector does see a rewrite when the name really changes.
    assert_ne!(before, after, "control: a rename must rewrite FTS rows");
    assert_eq!(
        after.len(),
        20,
        "no duplicate or missing FTS rows after rename"
    );
    let rec_ids: HashSet<&String> = after.iter().map(|(_, id)| id).collect();
    assert_eq!(rec_ids.len(), 20, "each recording has exactly one FTS row");

    assert_eq!(search_ids(&lib, "quazar").len(), 20, "new name finds all");
    assert_eq!(
        search_ids(&lib, "drift").len(),
        20,
        "new name token finds all"
    );
    assert_eq!(
        search_ids(&lib, "zorbix").len(),
        0,
        "old name finds nothing"
    );
    let rows = flat_ids(
        &list_all(
            &lib,
            &ListQuery {
                limit: 200,
                ..Default::default()
            },
        )
        .expect("list"),
    );
    assert_eq!(rows.len(), 20);
}

#[test]
fn issue147_similar_names_do_not_cross_contaminate() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("lib147c.db");
    let (lib, root) = setup(&path);
    lib.upsert_game(&gm("exe:a", "Iron Frontier", "fa"), 1)
        .expect("a");
    lib.upsert_game(&gm("exe:b", "Iron Frontier 2", "fb"), 1)
        .expect("b");
    let mut a_ids = HashSet::new();
    let mut b_ids = HashSet::new();
    for n in 1..=5u64 {
        lib.insert_recording(&nrec(n, root, Some("exe:a"), &format!("clipa{n}.mp4")))
            .expect("insert a");
        a_ids.insert(oid(n));
    }
    for n in 6..=10u64 {
        lib.insert_recording(&nrec(n, root, Some("exe:b"), &format!("clipb{n}.mp4")))
            .expect("insert b");
        b_ids.insert(oid(n));
    }
    let to_set = |v: Vec<String>| -> HashSet<String> { v.into_iter().collect() };
    assert_eq!(
        to_set(search_ids(&lib, "iron")),
        a_ids.union(&b_ids).cloned().collect()
    );
    assert_eq!(search_ids(&lib, "frontier").len(), 10);

    lib.upsert_game(&gm("exe:b", "Lunar Drift", "fb"), 2)
        .expect("rename b");
    assert_eq!(
        to_set(search_ids(&lib, "iron")),
        a_ids,
        "iron must be A only"
    );
    assert_eq!(
        to_set(search_ids(&lib, "lunar")),
        b_ids,
        "lunar must be B only"
    );
    assert_eq!(
        to_set(search_ids(&lib, "drift")),
        b_ids,
        "drift must be B only"
    );
    assert_eq!(search_ids(&lib, "frontier").len(), 5, "old B name gone");
    assert_eq!(search_ids(&lib, "2").len(), 0, "old B name token '2' gone");

    let a_rows = flat_ids(
        &list_all(
            &lib,
            &ListQuery {
                game: Some("exe:a".into()),
                limit: 200,
                ..Default::default()
            },
        )
        .expect("list a"),
    );
    assert_eq!(a_rows.len(), 5);
    let a_names = list_all(
        &lib,
        &ListQuery {
            game: Some("exe:a".into()),
            limit: 200,
            ..Default::default()
        },
    )
    .expect("list a");
    for p in &a_names {
        for r in &p.items {
            assert_eq!(r.game_name.as_deref(), Some("Iron Frontier"));
        }
    }
}

#[test]
fn issue147_recording_added_after_rename_is_found_by_new_name() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("lib147d.db");
    let (lib, root) = setup(&path);
    lib.upsert_game(&gm("exe:a", "Zorbix Arena", "fa"), 1)
        .expect("upsert");
    for n in 1..=20u64 {
        lib.insert_recording(&nrec(n, root, Some("exe:a"), &format!("clip{n:02}.mp4")))
            .expect("insert");
    }
    lib.upsert_game(&gm("exe:a", "Quazar Drift", "fa"), 2)
        .expect("rename");
    lib.insert_recording(&nrec(21, root, Some("exe:a"), "clip21.mp4"))
        .expect("insert after rename");
    assert_eq!(search_ids(&lib, "quazar").len(), 21);
    assert!(search_ids(&lib, "quazar").contains(&oid(21)));
    assert_eq!(search_ids(&lib, "zorbix").len(), 0);
    assert_eq!(fts_rows(&path).len(), 21);
}

#[test]
fn issue147_unusual_names_do_not_break_search() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("lib147e.db");
    let (lib, root) = setup(&path);

    // Empty names are rejected before any SQL runs.
    assert!(matches!(
        lib.upsert_game(&gm("exe:e", "", "fe"), 1),
        Err(LibError::Invalid(_))
    ));
    assert!(matches!(
        lib.upsert_game(&gm("exe:e", "Name", ""), 1),
        Err(LibError::Invalid(_))
    ));

    let wide = "Wide".repeat(50);
    // (display name, search text, expectation: Some(found) or None = observe only)
    let cases: Vec<(String, &str, Option<bool>)> = vec![
        ("Café Löwe".into(), "café", Some(true)),
        ("Café Löwe".into(), "lowe", Some(true)),
        ("Half-Life: 2 (GOTY) & more".into(), "half", Some(true)),
        ("O\"Brien's \"Quote\" Game".into(), "quote", Some(true)),
        ("O\"Brien's \"Quote\" Game".into(), "obriens", None),
        ("日本語ゲーム".into(), "日本語ゲーム", None),
        ("!!!".into(), "!!!", None),
        ("AND OR NOT NEAR".into(), "near", Some(true)),
        ("emoji 🎮 game".into(), "emoji", Some(true)),
        ("'); DROP TABLE recordings;--".into(), "drop", Some(true)),
        (wide, "wide", Some(true)),
    ];

    let mut expected_rows = 0usize;
    for (i, (name, _, _)) in cases.iter().enumerate() {
        let gid = format!("exe:u{i}");
        lib.upsert_game(&gm(&gid, name, &format!("fu{i}")), 1)
            .unwrap_or_else(|e| panic!("upsert {name:?} failed: {e}"));
        lib.insert_recording(&nrec(
            100 + i as u64,
            root,
            Some(&gid),
            &format!("u{i}.mp4"),
        ))
        .unwrap_or_else(|e| panic!("insert for {name:?} failed: {e}"));
        expected_rows += 1;
    }

    for (i, (name, query, expect)) in cases.iter().enumerate() {
        let found = search_ids(&lib, query);
        let own = found.contains(&oid(100 + i as u64));
        println!(
            "#147 unicode observe: name={:?} query={:?} own_row_found={own} total_hits={}",
            name.chars().take(20).collect::<String>(),
            query,
            found.len()
        );
        if let Some(want) = expect {
            assert_eq!(own, *want, "name {name:?} query {query:?}");
        }
    }

    let all = flat_ids(
        &list_all(
            &lib,
            &ListQuery {
                limit: 200,
                ..Default::default()
            },
        )
        .expect("list"),
    );
    assert_eq!(all.len(), expected_rows, "table intact after odd names");
}

#[test]
fn issue147_concurrent_readonly_handle_sees_consistent_snapshots() {
    let dir = TempDir::new().expect("tempdir");
    let path: PathBuf = dir.path().join("lib147f.db");
    let (lib, root) = setup(&path);
    let names = [
        "Zorbix Arena",
        "Quazar Drift",
        "Plentra Tales",
        "Mirvon Rally",
        "Hexaden Siege",
    ];
    let tokens = ["zorbix", "quazar", "plentra", "mirvon", "hexaden"];
    lib.upsert_game(&gm("exe:c", names[0], "fc"), 1)
        .expect("upsert");
    for n in 1..=20u64 {
        lib.insert_recording(&nrec(n, root, Some("exe:c"), &format!("c{n:02}.mp4")))
            .expect("insert");
    }

    const ITER: usize = 300;
    let done = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let read_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let violations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let reader = {
        let (rpath, done, reads, read_errors, violations) = (
            path.clone(),
            done.clone(),
            reads.clone(),
            read_errors.clone(),
            violations.clone(),
        );
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let tokens: Vec<String> = tokens.iter().map(|s| s.to_string()).collect();
        std::thread::spawn(move || {
            let ro = match Library::open_readonly(&rpath) {
                Ok(l) => l,
                Err(e) => {
                    read_errors.lock().unwrap().push(format!("open: {e}"));
                    return;
                }
            };
            loop {
                for (name, tok) in names.iter().zip(tokens.iter()) {
                    let q = ListQuery {
                        text: Some(tok.clone()),
                        limit: 200,
                        ..Default::default()
                    };
                    match ro.list(&q) {
                        Ok(p) => {
                            let n = p.items.len();
                            if n != 0 && n != 20 {
                                violations
                                    .lock()
                                    .unwrap()
                                    .push(format!("search {name}: {n} rows (want 0 or 20)"));
                            }
                        }
                        Err(e) => read_errors.lock().unwrap().push(format!("search: {e}")),
                    }
                }
                // One statement, one snapshot: all rows must agree on the game name.
                let gq = ListQuery {
                    game: Some("exe:c".into()),
                    limit: 200,
                    ..Default::default()
                };
                match ro.list(&gq) {
                    Ok(p) => {
                        let mut seen = HashSet::new();
                        for r in &p.items {
                            seen.insert(r.game_name.clone().unwrap_or_default());
                        }
                        if p.items.len() != 20 || seen.len() != 1 {
                            violations.lock().unwrap().push(format!(
                                "game snapshot: {} rows, names {:?}",
                                p.items.len(),
                                seen
                            ));
                        }
                    }
                    Err(e) => read_errors.lock().unwrap().push(format!("game: {e}")),
                }
                reads.fetch_add(1, AtomicOrdering::Relaxed);
                if done.load(AtomicOrdering::Acquire) {
                    break;
                }
            }
        })
    };

    let mut writer_errors = Vec::new();
    for i in 0..ITER {
        let idx = (i / 3) % 5; // renames every third upsert; repeats in between
        if let Err(e) = lib.upsert_game(&gm("exe:c", names[idx], "fc"), 10 + i as i64) {
            writer_errors.push(e.to_string());
        }
    }
    done.store(true, AtomicOrdering::Release);
    reader.join().expect("reader thread");

    let reads = reads.load(AtomicOrdering::Relaxed);
    let violations = violations.lock().unwrap().clone();
    let read_errors = read_errors.lock().unwrap().clone();
    println!(
        "#147 concurrency: writer_upserts={ITER} writer_errors={} reader_passes={reads} reader_errors={} violations={}",
        writer_errors.len(),
        read_errors.len(),
        violations.len()
    );
    assert!(reads >= 1, "reader made no passes");
    assert!(writer_errors.is_empty(), "writer errors: {writer_errors:?}");
    assert!(read_errors.is_empty(), "reader errors: {read_errors:?}");
    assert!(
        violations.is_empty(),
        "inconsistent snapshots: {violations:?}"
    );

    let final_idx = ((ITER - 1) / 3) % 5;
    for (i, tok) in tokens.iter().enumerate() {
        let want = if i == final_idx { 20 } else { 0 };
        assert_eq!(search_ids(&lib, tok).len(), want, "final state for {tok}");
    }
}
