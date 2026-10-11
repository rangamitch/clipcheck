//! Gallery commands.
//!
//! The logic lives in plain functions over [`Library`] (or over a database path) so it can be
//! tested without a Tauri runtime. The `#[tauri::command]` wrappers at the bottom only resolve
//! the database path and map errors to strings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use clipcheck_library::{
    LibError, Library, ListQuery, Page, RecStatus, RecordingKind, RecordingRow, SortKey,
};
use serde::{Deserialize, Serialize};

/// Environment variable naming the data directory that contains `library.db`.
pub const DATA_DIR_ENV: &str = "CLIPCHECK_DATA_DIR";
/// Environment variable that enables the debug-only dev seed when set to `1`.
pub const DEV_SEED_ENV: &str = "CLIPCHECK_DEV_SEED";
/// Library database file name inside the data directory.
pub const DB_FILE_NAME: &str = "library.db";
/// Page size used when the request does not specify one.
pub const DEFAULT_PAGE_SIZE: u32 = 60;

/// Set once the "database missing" notice has been printed, so it appears only once.
static MISSING_DB_LOGGED: AtomicBool = AtomicBool::new(false);

/// Errors from the Gallery commands.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// The library returned an error.
    #[error("library: {0}")]
    Library(#[from] LibError),
    /// Neither `CLIPCHECK_DATA_DIR` nor `APPDATA` is set, so the library location is unknown.
    #[error("no data directory: set {DATA_DIR_ENV} or APPDATA")]
    NoDataDir,
}

/// Sort choices offered by the Gallery. Serialized as snake_case strings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDto {
    /// Newest first.
    #[default]
    Newest,
    /// Oldest first.
    Oldest,
    /// File name A-Z.
    NameAsc,
    /// File name Z-A.
    NameDesc,
    /// Largest file first.
    Largest,
    /// Smallest file first.
    Smallest,
    /// Longest duration first.
    Longest,
}

impl From<SortDto> for SortKey {
    fn from(s: SortDto) -> Self {
        match s {
            SortDto::Newest => SortKey::Newest,
            SortDto::Oldest => SortKey::Oldest,
            SortDto::NameAsc => SortKey::NameAsc,
            SortDto::NameDesc => SortKey::NameDesc,
            SortDto::Largest => SortKey::SizeDesc,
            SortDto::Smallest => SortKey::SizeAsc,
            SortDto::Longest => SortKey::DurationDesc,
        }
    }
}

/// Request body for `list_recordings`. Fields are camelCase on the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListQueryDto {
    /// Free text. Blank means no text filter.
    pub text: Option<String>,
    /// Game id. Blank or absent means all games.
    pub game: Option<String>,
    /// `clip` or `session`. Absent means both.
    pub kind: Option<RecordingKind>,
    /// Only favorites.
    pub favorites_only: bool,
    /// Sort order.
    pub sort: SortDto,
    /// Page size. Absent means [`DEFAULT_PAGE_SIZE`]; the library clamps to 1..=200.
    pub limit: Option<u32>,
    /// Cursor from the previous page's `nextCursor`.
    pub cursor: Option<String>,
}

impl ListQueryDto {
    /// Converts the request into a library query. Blank strings are treated as absent.
    pub fn to_list_query(&self) -> ListQuery {
        ListQuery {
            text: non_blank(self.text.as_deref()),
            game: non_blank(self.game.as_deref()),
            kind: self.kind,
            favorites_only: self.favorites_only,
            min_size: None,
            max_size: None,
            sort: self.sort.into(),
            limit: self.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            cursor: non_blank(self.cursor.as_deref()),
        }
    }
}

fn non_blank(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// One recording as the Gallery shows it. Fields are camelCase on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDto {
    /// ULID.
    pub id: String,
    /// `clip` or `session`.
    pub kind: RecordingKind,
    /// Session ULID for session parts.
    pub session_id: Option<String>,
    /// 1-based part index for session parts.
    pub part_index: Option<i64>,
    /// Game id.
    pub game_id: Option<String>,
    /// Game display name.
    pub game_name: Option<String>,
    /// File name including extension.
    pub file_name: String,
    /// `mp4` or `mkv`.
    pub ext: String,
    /// File size in bytes.
    pub size_bytes: i64,
    /// Duration in milliseconds.
    pub duration_ms: i64,
    /// Creation time, UTC milliseconds.
    pub created_at_ms: i64,
    /// `pending` or `ready` (listings exclude other statuses).
    pub status: RecStatus,
    /// Favorite flag.
    pub is_favorite: bool,
    /// The file was repaired after an unclean stop.
    pub recovered: bool,
    /// Recorded with degraded quality.
    pub degraded: bool,
    /// Needs a faststart remux before streaming.
    pub needs_faststart: bool,
}

impl From<RecordingRow> for RecordingDto {
    fn from(r: RecordingRow) -> Self {
        RecordingDto {
            id: r.id,
            kind: r.kind,
            session_id: r.session_id,
            part_index: r.part_index,
            game_id: r.game_id,
            game_name: r.game_name,
            file_name: r.file_name,
            ext: r.ext,
            size_bytes: r.size_bytes,
            duration_ms: r.duration_ms,
            created_at_ms: r.created_at_ms,
            status: r.status,
            is_favorite: r.is_favorite,
            recovered: r.recovered,
            degraded: r.degraded,
            needs_faststart: r.needs_faststart,
        }
    }
}

/// One page of results. `nextCursor` is `null` on the last page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDto {
    /// Rows in sort order.
    pub items: Vec<RecordingDto>,
    /// Cursor for the next page, or `null`.
    pub next_cursor: Option<String>,
}

impl From<Page> for PageDto {
    fn from(p: Page) -> Self {
        PageDto {
            items: p.items.into_iter().map(RecordingDto::from).collect(),
            next_cursor: p.next_cursor,
        }
    }
}

/// Library-wide totals for the Gallery header. Fields are camelCase on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsDto {
    /// Number of listable recordings (pending or ready).
    pub recordings: u64,
    /// Number of clips.
    pub clips: u64,
    /// Number of session parts.
    pub sessions: u64,
    /// Sum of file sizes in bytes.
    pub total_size_bytes: i64,
    /// Sum of durations in milliseconds.
    pub total_duration_ms: i64,
}

/// Lists one page of recordings matching `dto`.
pub fn query_page(lib: &Library, dto: &ListQueryDto) -> Result<PageDto, CommandError> {
    Ok(lib.list(&dto.to_list_query())?.into())
}

/// Totals over every listable recording, computed by one aggregate query in the library.
pub fn query_stats(lib: &Library) -> Result<StatsDto, CommandError> {
    let totals = lib.stats(&ListQuery::default())?;
    Ok(StatsDto {
        recordings: totals.recordings,
        clips: totals.clips,
        sessions: totals.sessions,
        total_size_bytes: totals.total_size_bytes,
        total_duration_ms: totals.total_duration_ms,
    })
}

/// Prints the "database missing" notice the first time an absent file is read.
fn note_missing_db(db: &Path) {
    if !MISSING_DB_LOGGED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "clipcheck-ui: library database {} does not exist yet; showing an empty library",
            db.display()
        );
    }
}

/// Lists one page from the database at `db`. A missing file (the daemon has not created it
/// yet) yields an empty page, not an error; the first such read is logged.
pub fn page_at(db: &Path, dto: &ListQueryDto) -> Result<PageDto, CommandError> {
    if !db.exists() {
        note_missing_db(db);
        return Ok(PageDto::default());
    }
    let lib = Library::open_readonly(db)?;
    query_page(&lib, dto)
}

/// Totals from the database at `db`. A missing file (not created yet) yields zeroed totals,
/// not an error; the first such read is logged.
pub fn stats_at(db: &Path) -> Result<StatsDto, CommandError> {
    if !db.exists() {
        note_missing_db(db);
        return Ok(StatsDto::default());
    }
    let lib = Library::open_readonly(db)?;
    query_stats(&lib)
}

/// Resolves the library database path from the given values (pure, for tests).
///
/// `CLIPCHECK_DATA_DIR` wins and names the directory holding `library.db`. Otherwise the path is
/// `%APPDATA%\ClipCheck\library.db`. If neither is set (blank counts as unset), returns
/// [`CommandError::NoDataDir`]; there is no working-directory fallback.
pub fn resolve_db_path(
    data_dir: Option<&str>,
    appdata: Option<&str>,
) -> Result<PathBuf, CommandError> {
    if let Some(dir) = data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join(DB_FILE_NAME));
    }
    if let Some(app) = appdata.map(str::trim).filter(|a| !a.is_empty()) {
        return Ok(PathBuf::from(app).join("ClipCheck").join(DB_FILE_NAME));
    }
    Err(CommandError::NoDataDir)
}

/// Legacy form of [`resolve_db_path`] that never fails: when neither value is set it returns
/// `library.db` in the working directory. Kept only because `tests/gallery_commands.rs`
/// asserts that fallback. Production code must use [`resolve_db_path`] or
/// [`try_default_db_path`].
pub fn db_path_from(data_dir: Option<&str>, appdata: Option<&str>) -> PathBuf {
    resolve_db_path(data_dir, appdata).unwrap_or_else(|_| PathBuf::from(DB_FILE_NAME))
}

/// Resolves the library database path from the process environment, failing with
/// [`CommandError::NoDataDir`] when neither `CLIPCHECK_DATA_DIR` nor `APPDATA` is set.
pub fn try_default_db_path() -> Result<PathBuf, CommandError> {
    let data_dir = std::env::var(DATA_DIR_ENV).ok();
    let appdata = std::env::var("APPDATA").ok();
    resolve_db_path(data_dir.as_deref(), appdata.as_deref())
}

/// Legacy environment lookup that never fails: falls back to `library.db` in the working
/// directory (see [`db_path_from`]). Prefer [`try_default_db_path`], which reports the error.
pub fn default_db_path() -> PathBuf {
    try_default_db_path().unwrap_or_else(|_| PathBuf::from(DB_FILE_NAME))
}

/// Returns true when the dev seed is requested (`CLIPCHECK_DEV_SEED=1`).
pub fn dev_seed_requested(env_value: Option<&str>) -> bool {
    env_value.map(str::trim) == Some("1")
}

#[cfg(debug_assertions)]
mod dev {
    //! Debug-only fake library data, so the Gallery can be viewed without recordings.

    use std::path::Path;

    use clipcheck_library::{Library, NewGame, NewRecording, RecStatus, RecordingKind};

    use super::{dev_seed_requested, CommandError, DEV_SEED_ENV};

    const HOUR_MS: i64 = 3_600_000;
    const MINUTE_MS: i64 = 60_000;
    const ROOT_PATH: &str = r"C:\Users\Dev\Videos\ClipCheck";

    // Seed shape. Each value is a function of the index `i`, so the same index always gives
    // the same row. `n` below is `i` as i64; the strides spread values across their span.
    /// Every `SESSION_EVERY`-th index is a session part; the others are clips.
    const SESSION_EVERY: usize = 3;
    /// Every `DESKTOP_EVERY`-th index has no game (desktop capture).
    const DESKTOP_EVERY: usize = 6;
    /// Session parts alternate between this many part indexes.
    const SESSION_PARTS: i64 = 2;
    /// Creation time steps back `n * CREATED_STEP_MS` from `now`...
    const CREATED_STEP_MS: i64 = 17 * HOUR_MS;
    /// ...minus `(n * n % CREATED_JITTER_MODULUS)` minutes of jitter.
    const CREATED_JITTER_MODULUS: i64 = 11;
    /// Clip size is `1 + (n * SIZE_STRIDE_MB) % SIZE_SPAN_MB` megabytes.
    const SIZE_STRIDE_MB: i64 = 7_919;
    const SIZE_SPAN_MB: i64 = 500;
    /// Sessions are this many times larger than clips at the same size.
    const SESSION_SIZE_FACTOR: i64 = 4;
    /// Clip duration is `CLIP_BASE_DURATION_MS + (n * CLIP_DURATION_STRIDE_MS) % CLIP_DURATION_SPAN_MS`.
    const CLIP_BASE_DURATION_MS: i64 = 5_000;
    const CLIP_DURATION_STRIDE_MS: i64 = 13_001;
    const CLIP_DURATION_SPAN_MS: i64 = 240_000;
    /// Session duration uses the same form with the session constants.
    const SESSION_BASE_DURATION_MS: i64 = 1_800_000;
    const SESSION_DURATION_STRIDE_MS: i64 = 60_011;
    const SESSION_DURATION_SPAN_MS: i64 = 5_400_000;
    /// Flags: every `N`-th index is a favorite, recovered, degraded or needs a faststart remux.
    const FAVORITE_EVERY: usize = 7;
    const RECOVERED_EVERY: usize = 17;
    const DEGRADED_EVERY: usize = 23;
    const FASTSTART_EVERY: usize = 5;

    /// (display name, folder slug) for the seeded games.
    const GAMES: [(&str, &str); 5] = [
        ("Apex Legends", "apex"),
        ("Valorant", "valorant"),
        ("Elden Ring", "eldenring"),
        ("Minecraft", "minecraft"),
        ("Rocket League", "rocketleague"),
    ];

    /// Number of fake recordings written by [`maybe_dev_seed`].
    pub const DEV_SEED_COUNT: usize = 60;

    /// Inserts `count` fake recordings (clips and sessions across the seeded games, with varied
    /// sizes, durations and dates, and some favorites). Returns how many were inserted.
    pub fn dev_seed(lib: &Library, count: usize, now_ms: i64) -> Result<usize, CommandError> {
        let root_id = lib.ensure_storage_root(ROOT_PATH, now_ms)?;
        for (name, slug) in GAMES {
            lib.upsert_game(
                &NewGame {
                    id: format!("game-{slug}"),
                    display_name: name.to_owned(),
                    folder_name: slug.to_owned(),
                    exe_name: None,
                    store_id: None,
                },
                now_ms,
            )?;
        }
        for i in 0..count {
            // `i < count` and the count is a small seed size, so this cannot fail.
            let n = i64::try_from(i).expect("dev seed index fits in i64");
            let is_session = i % SESSION_EVERY == SESSION_EVERY - 1;
            let game = if i % DESKTOP_EVERY == DESKTOP_EVERY - 1 {
                None
            } else {
                Some(GAMES[i % GAMES.len()])
            };
            let slug = game.map_or("desktop", |(_, s)| s);
            let kind = if is_session {
                RecordingKind::Session
            } else {
                RecordingKind::Clip
            };
            let ext = if is_session { "mkv" } else { "mp4" };
            let created_at_ms =
                now_ms - n * CREATED_STEP_MS - (n * n % CREATED_JITTER_MODULUS) * MINUTE_MS;
            let size_mb = 1 + (n * SIZE_STRIDE_MB) % SIZE_SPAN_MB;
            let size_bytes = size_mb * 1_000_000 * if is_session { SESSION_SIZE_FACTOR } else { 1 };
            let duration_ms = if is_session {
                SESSION_BASE_DURATION_MS
                    + (n * SESSION_DURATION_STRIDE_MS) % SESSION_DURATION_SPAN_MS
            } else {
                CLIP_BASE_DURATION_MS + (n * CLIP_DURATION_STRIDE_MS) % CLIP_DURATION_SPAN_MS
            };
            let id = format!("01DEV{:021}", i);
            let file_name = if is_session {
                format!("{slug}_session_{i:03}.{ext}")
            } else {
                format!("{slug}_clip_{i:03}.{ext}")
            };
            let session_id = is_session.then(|| format!("01SES{:021}", i / SESSION_EVERY));
            let part_index = is_session.then_some(1 + n % SESSION_PARTS);
            lib.insert_recording(&NewRecording {
                id: id.clone(),
                kind,
                session_id,
                part_index,
                game_id: game.map(|(_, s)| format!("game-{s}")),
                root_id,
                rel_path: format!("{slug}\\{file_name}"),
                file_name,
                ext: ext.to_owned(),
                size_bytes,
                file_mtime_ms: created_at_ms,
                created_at_ms,
                added_at_ms: created_at_ms,
                duration_ms,
                status: RecStatus::Ready,
                error_detail: None,
                end_reason: None,
                recovered: i % RECOVERED_EVERY == 0,
                degraded: i % DEGRADED_EVERY == 0,
                dropped_packets: 0,
                gap_ms: 0,
                needs_faststart: i % FASTSTART_EVERY == 0,
            })?;
            if i % FAVORITE_EVERY == 0 {
                lib.set_favorite(&id, true)?;
            }
        }
        Ok(count)
    }

    /// Creates and seeds the library at `db` when `CLIPCHECK_DEV_SEED=1` and the file does not
    /// exist yet. Returns true when a seed was written. Never overwrites an existing database.
    pub fn maybe_dev_seed(db: &Path) -> Result<bool, CommandError> {
        let requested = std::env::var(DEV_SEED_ENV).ok();
        if !dev_seed_requested(requested.as_deref()) || db.exists() {
            return Ok(false);
        }
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CommandError::Library(clipcheck_library::LibError::Io(e)))?;
        }
        let lib = Library::open(db)?;
        let now = clipcheck_library::schema::now_ms();
        dev_seed(&lib, DEV_SEED_COUNT, now)?;
        Ok(true)
    }
}

#[cfg(debug_assertions)]
pub use dev::{dev_seed, maybe_dev_seed, DEV_SEED_COUNT};

/// Tauri command: one page of recordings for the Gallery.
#[tauri::command]
pub fn list_recordings(query: ListQueryDto) -> Result<PageDto, String> {
    let db = try_default_db_path().map_err(|e| e.to_string())?;
    page_at(&db, &query).map_err(|e| e.to_string())
}

/// Tauri command: library-wide totals for the Gallery header.
#[tauri::command]
pub fn get_library_stats() -> Result<StatsDto, String> {
    let db = try_default_db_path().map_err(|e| e.to_string())?;
    stats_at(&db).map_err(|e| e.to_string())
}

/// Maximum number of game ids served per icon request.
pub const MAX_ICON_IDS: usize = 500;

/// Cached game icons (PNG bytes) for `game_ids`, read from the data directory beside `db`.
/// Ids without an icon are omitted. Only the first [`MAX_ICON_IDS`] ids are considered.
pub fn icons_for(db: &Path, game_ids: &[String]) -> HashMap<String, Vec<u8>> {
    let Some(data_dir) = db.parent() else {
        return HashMap::new();
    };
    let capped = &game_ids[..game_ids.len().min(MAX_ICON_IDS)];
    clipcheck_library::icons::read_icons(data_dir, capped)
}

/// Tauri command: cached game icons for the Gallery cards, keyed by game id.
///
/// Returns an empty map (and logs why) when the data directory is not configured, because the
/// icon response has no error channel and the cards then just show no icon.
#[tauri::command]
pub fn get_game_icons(game_ids: Vec<String>) -> HashMap<String, Vec<u8>> {
    match try_default_db_path() {
        Ok(db) => icons_for(&db, &game_ids),
        Err(e) => {
            eprintln!("clipcheck-ui: game icons unavailable: {e}");
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICON_PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn icons_for_is_empty_without_icon_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join(DB_FILE_NAME);
        let ids = vec!["exe:game.exe".to_owned()];
        assert!(icons_for(&db, &ids).is_empty());
    }

    #[test]
    fn icons_for_returns_written_icon_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join(DB_FILE_NAME);
        clipcheck_library::icons::write_icon(dir.path(), "exe:game.exe", &ICON_PNG)
            .expect("write icon");
        let ids = vec!["exe:game.exe".to_owned(), "exe:other.exe".to_owned()];
        let got = icons_for(&db, &ids);
        assert_eq!(got.len(), 1);
        assert_eq!(got["exe:game.exe"], ICON_PNG.to_vec());
    }

    #[test]
    fn db_path_prefers_data_dir_env() {
        let p = db_path_from(Some(r"D:\data"), Some(r"C:\Users\x\AppData\Roaming"));
        assert_eq!(p, PathBuf::from(r"D:\data").join("library.db"));
    }

    #[test]
    fn db_path_falls_back_to_appdata() {
        let p = db_path_from(Some("  "), Some(r"C:\Roaming"));
        assert_eq!(
            p,
            PathBuf::from(r"C:\Roaming")
                .join("ClipCheck")
                .join("library.db")
        );
    }

    #[test]
    fn blank_filters_become_none() {
        let dto = ListQueryDto {
            text: Some("   ".into()),
            game: Some(String::new()),
            cursor: Some(" ".into()),
            ..ListQueryDto::default()
        };
        let q = dto.to_list_query();
        assert_eq!(q.text, None);
        assert_eq!(q.game, None);
        assert_eq!(q.cursor, None);
        assert_eq!(q.limit, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn sort_dto_maps_to_library_keys() {
        assert_eq!(SortKey::from(SortDto::Largest), SortKey::SizeDesc);
        assert_eq!(SortKey::from(SortDto::Smallest), SortKey::SizeAsc);
        assert_eq!(SortKey::from(SortDto::Longest), SortKey::DurationDesc);
        assert_eq!(SortKey::from(SortDto::NameAsc), SortKey::NameAsc);
    }

    #[test]
    fn dev_seed_requires_exact_one() {
        assert!(dev_seed_requested(Some("1")));
        assert!(dev_seed_requested(Some(" 1 ")));
        assert!(!dev_seed_requested(Some("true")));
        assert!(!dev_seed_requested(None));
    }

    #[test]
    fn missing_db_gives_empty_page_and_zero_stats() {
        let dir = std::env::temp_dir().join("clipcheck-ui-missing-db-unit");
        let db = dir.join("does-not-exist.db");
        let page = page_at(&db, &ListQueryDto::default()).expect("page");
        assert!(page.items.is_empty());
        assert_eq!(page.next_cursor, None);
        assert_eq!(stats_at(&db).expect("stats"), StatsDto::default());
    }
}
