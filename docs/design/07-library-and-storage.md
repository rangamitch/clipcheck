# 07 — Library and Storage (`crates/library`)

Status: Draft v1 · Owner: library · Depends on: `core`, `encode` (FFmpeg wrapper), `session`, `clipedit`, `ipc`, `apps/ui` (Tauri commands)

This doc specifies the `library` crate: the SQLite schema, recordings root and per-game folders, storage relocation, filename templates, rename and delete, the folder watcher, thumbnails and hover strips, media probing, the gallery query API, the storage quota policy, and the IPC/Tauri surface. It follows the architecture in `00-brief.md`; where it refines a brief decision it says so under "Decisions refined from brief" below.

---

## 0. Decisions refined from brief

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **`clipcheckd` is the only writer of `library.db`.** The UI opens a read-only WAL handle for every gallery/player query. Every mutation (rename, delete, favorite, tag, storage move, quota run) is an IPC request to the service, which executes it on its single writer connection. | Brief says the UI reads only and sends commands to the service. A single writer removes cross-process write contention and makes the rename/delete atomicity argument tractable. |
| D2 | Recordings store `root_id` + `rel_path`, not an absolute path. | Required so that "leave existing files" after a storage change keeps old rows resolvable, and so that a moved root is a single row update. |
| D3 | Thumbnails and hover strips are **files** under `%LOCALAPPDATA%\ClipCheck\thumbs\<recording_id>\`; the DB stores metadata and relative paths only. | Keeps the DB small and page-cache friendly; thumbnails are regenerable so they do not belong in the backup-relevant DB. |
| D4 | Auto-delete (quota policy) is **permanent**, not Recycle Bin. Manual delete goes to the Recycle Bin. | The Recycle Bin lives on the same volume and does not free quota. The user opts in explicitly and the UI states this. |
| D5 | Favorites are **always** protected from auto-delete. It is not a config key. | Brief requires protected favorites; making it configurable invites accidental data loss. |
| D6 | Keyset (cursor) pagination only. No `OFFSET` in the gallery API. | Stable pages while the watcher inserts/removes rows; O(log n) per page at 10k+ items. |
| D7 | The library does not decode video itself. It consumes two traits (`MediaProbe`, `FrameSource`) implemented by the in-house FFmpeg wrapper in `crates/encode`. | Keeps FFmpeg FFI in one place; makes the library testable with fakes. |

---

## 1. Goal & scope

### 1.1 Goal
Provide the persistent, searchable, self-healing catalog of every clip and session file ClipCheck has produced or that the user has placed in a recordings root, plus the file operations the gallery needs (rename, delete, relocate, enforce quota) without ever letting the database and the disk disagree for long.

### 1.2 In scope
- SQLite schema v1 (full DDL in §4.1), migrations, WAL setup.
- Storage roots: one *active* root for new recordings, plus zero or more *legacy* roots that remain indexed.
- Per-game subfolders and the filename template engine.
- Changing the storage location with `MoveExisting` or `LeaveExisting`.
- In-app rename (disk + DB) with collision handling and illegal-character rules.
- Delete to Recycle Bin, and permanent delete (used by auto-delete and by explicit user choice after a warning).
- Folder watcher and reconciliation for externally added, removed, renamed, or modified files.
- Poster thumbnail and hover-preview strip generation.
- Media probing (container, duration, size, video codec/resolution/fps, audio tracks).
- Gallery query API: filter, sort, keyset pagination, facets, text search.
- Quota and free-space policy with auto-delete of the oldest unprotected recordings.
- Tauri commands and service IPC messages for all of the above.

### 1.3 Out of scope
- Writing video (owned by `session`, `buffer`, `encode`).
- Trimming and remuxing clips (owned by `clipedit`; `clipedit` calls `library.insert_clip` with the output).
- The Svelte gallery/player UI (owned by the UI doc). This doc defines the data it receives.
- Cloud sync, sharing, or any network I/O. Explicitly none.
- Editing file contents. The library never rewrites a video file except by moving or renaming it.
- Tag/favorite *sync* between machines. Not applicable: local only.

---

## 2. User-facing behavior

### 2.1 Gallery
- Default view: all recordings with `status IN ('ready','pending')`, sort = **Date (newest first)**, page size 60.
- Missing files (`status = 'missing'`) are hidden by default; a filter chip "Show missing" reveals them with a red badge and a "Remove from library" action.
- Filter bar: text search (matches file name, game name, tag names; prefix match per word), kind (Clips / Sessions / Both), game (multi-select with counts), tags (multi-select, mode Any/All), favorites only, date range, duration range, size range.
- Sort menu: Name A–Z / Z–A, Size largest/smallest, Date newest/oldest, Duration longest/shortest, Game A–Z / Z–A, Kind (clip, then session).
- Result header shows the total count ("10,482 recordings"). The count is computed once per filter change, not per page.
- Scrolling loads the next page when 15 rows from the bottom. Pages are 60 items; max 200.
- Hover over a poster: after 150 ms the poster cross-fades to the hover-preview strip; moving the mouse horizontally across the card selects the strip frame `floor(x / card_width * samples)`. Strip absent → poster stays and a background generation is requested.

### 2.2 Rename
- Activating Rename (F2, context menu, or title double-click) shows an inline field containing the stem (without extension). Extension is shown greyed and cannot be edited.
- Enter commits; Esc cancels.
- Accepted input: 1–120 Unicode characters, after trimming whitespace. Input containing any of `< > : " / \ | ? *` or a control character is **rejected** with the list of offending characters shown (not silently stripped; the user typed it). Trailing dots and spaces are rejected. Reserved Windows names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, with or without extension) are rejected.
- If another file in the same folder already has the target name (case-insensitive), the rename is rejected with "A file named 'X' already exists." No automatic suffix on manual rename.
- A recording with `status = 'recording'` cannot be renamed; the item shows a lock icon and a tooltip.
- A rename that fails because the file is open in another program shows "The file is in use by another program. Try again after closing it." (retry details in §5.3).

### 2.3 Delete
- Delete key or context menu → confirmation dialog: "Move 3 recordings (4.2 GB) to the Recycle Bin?" with [Cancel] [Move to Recycle Bin].
- Shift+Delete → dialog with a red warning: "This cannot be undone." [Cancel] [Delete permanently]. Permanent delete is available for manual use; it is not the default.
- Favorites require a second confirmation: "'<name>' is a favorite. Delete it anyway?" They are not skipped silently.
- If the Recycle Bin is unavailable for the volume (for example a removable drive with bin disabled), the dialog says so and offers only permanent delete.

### 2.4 Favorites and tags
- Star toggles favorite instantly (optimistic UI, service confirms within 50 ms typically).
- Tags: type to create or select; tag names are case-insensitive, unique, up to 40 characters. Deleting a tag removes it from all recordings after a confirmation that shows the count.

### 2.5 Recordings root and storage location (Settings → Storage)
- Shows: current active root path, free space on its volume, total size of all ClipCheck recordings, number of recordings per root.
- **Change location…** opens a folder picker. Validation (blocking, with reason):
  - Path must exist or be creatable; must be writable (a probe file is created and removed).
  - Must not be the same as, inside, or a parent of an existing registered root (prevents double indexing).
  - Must not be a Windows system folder or drive root.
  - Must have free space ≥ `total_size_of_moved_items + 1 GB` when Move is chosen.
- After a valid selection, a choice dialog:
  - **Move existing recordings to the new location** (default for a single-user library): all current recordings are moved; progress shown; cancellable; resumable after app or PC restart. Items that are missing on disk are left as-is with status `missing`.
  - **Leave existing recordings where they are**: the old root stays registered as a *legacy* root. Its recordings remain in the gallery, remain watched, can be renamed/deleted/favorited, and count toward the quota. New recordings go to the new root. The legacy root can later be removed with "Stop indexing this folder" (removes DB rows only; files untouched; requires confirmation).
- Changing the location does not move thumbnails (they are in `%LOCALAPPDATA%`).
- Per-game subfolders are re-created under the new root as needed.

### 2.6 Storage quota and auto-delete (Settings → Storage)
- **Storage limit**: off, or N GB (1–10,000).
- **When over the limit**: "Do nothing" (default) or "Delete the oldest recordings". Enabling the second option shows a warning: "Deleted recordings cannot be recovered. Favorites are never deleted."
- **Keep at least N GB free on the recordings drive**: 0–500 GB, default 5.
- When auto-delete runs, a toast says "Removed 2 old recordings (1.1 GB) to stay under your storage limit." Each deletion is logged.
- If over the limit and nothing is deletable (all favorites or in-progress), a persistent banner says "Storage limit reached. Only favorites or in-progress recordings remain."

### 2.7 External changes
- Files added to a recordings root by Explorer or another tool appear in the gallery within ~3 s (watcher debounce) with `created_at` = file creation time and game = "Unsorted" unless the file sits in a `<GameFolder>` that matches an existing game folder name, in which case it is assigned to that game.
- Files deleted outside the app disappear within ~3 s.
- Files renamed outside the app are re-linked (same row, new name) rather than shown as delete+add. Favorites and tags are kept.

---

## 3. Architecture & data flow

### 3.1 Process placement

```
 clipcheckd (service)                                          clipcheck (UI)
 ┌─────────────────────────────────────────────────────┐       ┌───────────────────────────────┐
 │ session / buffer / clipedit                         │       │ Tauri commands (src-tauri)    │
 │   │ begin_recording / finish_recording / insert_clip│       │  read-only:                   │
 │   ▼                                                 │       │   list_recordings, facets,    │
 │ library::Writer (one rusqlite conn, WAL, serialized)│◄──IPC─┤   get_recording, storage info │
 │   ▲   ▲                                             │       │  mutations → IPC requests     │
 │   │   └── library::Watcher (notify, per root)       │       └──────────────┬────────────────┘
 │   │   └── library::Reconciler (startup, periodic)   │                      │ read-only conn
 │   │   └── library::Thumbs (BELOW_NORMAL worker)     │                      ▼
 │   │   └── library::Quota (hourly + on change)       │          %APPDATA%\ClipCheck\library.db
 │   └── library::StorageJobs (move/leave/detach)     │◄─────────── (WAL, shared)
 └─────────────────────────────────────────────────────┘
          │ files
          ▼
  <root>\<GameFolder>\*.mp4 / *.mkv         %LOCALAPPDATA%\ClipCheck\thumbs\<id>\{poster.jpg,strip.jpg}
```

### 3.2 Crate module layout

```
crates/library/
  src/
    lib.rs              // Library facade, re-exports
    schema/
      mod.rs            // migrations runner, PRAGMAs
      v1.sql            // DDL from §4.1 (include_str!)
    model.rs            // RecordingRow, GameRow, TagRow, enums
    writer.rs           // Writer: single connection, transactions, FTS maintenance
    reader.rs           // ReaderPool: read-only connections (also used in-process by tests)
    query.rs            // RecordingQuery, SortKey, cursor codec, keyset SQL builder, facets
    naming/
      mod.rs            // NameTemplate, NameContext, sanitize, validate_user_stem
      reserved.rs       // reserved names
    roots.rs            // StorageRoot registry, path canonicalization, validation
    rename.rs           // rename algorithm §5.3
    delete.rs           // delete algorithm §5.4, recycle.rs (IFileOperation)
    recycle.rs          // Windows Recycle Bin wrapper
    watcher.rs          // FolderWatcher (notify), debounce, rename pairing
    reconcile.rs        // full-scan reconciliation §5.5
    probe.rs            // ProbeService: MediaProbe trait consumer, stability check
    thumbs.rs           // ThumbService: poster + strip generation, cache invalidation
    quota.rs            // QuotaPolicy, enforcement §5.7
    storage_job.rs      // move/leave/detach jobs (persisted) §5.6
    fileid.rs           // GetFileInformationByHandleEx(FileIdInfo) wrapper
    game_folder.rs      // GameId → folder name mapping, uniqueness
    error.rs
```

### 3.3 Dependency rules
- `library` depends on `core` (ids, config structs, errors), `rusqlite` (bundled), `notify`, `windows`, `ulid`, `unicode-normalization`, `chrono`, `serde`, `serde_json`, `base64`, `crc32fast`.
- `library` does **not** depend on `session`, `buffer`, `clipedit`, or `ipc`. Those depend on `library` (through its `LibraryHandle` trait, §4.6) so the service can inject a fake in tests.
- `library` receives `Arc<dyn MediaProbe>` and `Arc<dyn FrameSource>` at construction; the service wires the `encode` FFmpeg implementations.
- Thread model: one writer thread owns the write connection and receives commands on a bounded `crossbeam` channel (capacity 256). Watcher, reconciler, thumbs, and quota post commands to that channel. Readers are never on the writer thread.

### 3.4 Startup sequence (service)
1. Open `%APPDATA%\ClipCheck\library.db` (create if missing). Apply PRAGMAs (§4.1.0). Run migrations.
2. Load `storage_roots`. For each `active` or `legacy` root: check online (path exists). Offline roots are kept but not watched; they re-check every 30 s.
3. Crash recovery (§5.8): rows in `recording` state and jobs in `running` state are resolved.
4. Start watchers for online roots.
5. Schedule: probe queue drain, reconcile pass for each online root (if `last_reconcile_ms` older than `reconcile_interval_min`), thumbnail backfill (low priority), quota check.

---

## 4. Key types, traits and function signatures

### 4.0 Conventions
- All timestamps are `i64` Unix milliseconds UTC. Local-time rendering happens only in the filename template (§4.4) and the UI.
- `RecordingId`, `SessionId`: ULID (26-char Crockford base32), from `core::ids`. Stored as `TEXT`.
- `GameId`: from `core::ids`. Canonical string form stored in `games.id` (see §4.1.3).
- All relative paths are stored with `/` separators, NFC-normalized, no leading slash.
- Paths returned to the UI are absolute, with `\` separators, produced at read time as `root.path + '\' + rel_path.replace('/', '\')`.

### 4.1 SQLite schema — full DDL

#### 4.1.0 Connection PRAGMAs (applied on every connection)
```sql
PRAGMA journal_mode = WAL;        -- once, at DB creation; persists
PRAGMA synchronous = NORMAL;      -- WAL + NORMAL: a power loss may drop the last commit, never corrupts
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store = MEMORY;
PRAGMA cache_size = -2048;        -- writer: 2 MiB page cache (service RSS budget)
PRAGMA mmap_size = 0;             -- no mmap in service (RSS budget)
-- UI read-only connections: cache_size = -8192 (8 MiB), mmap_size = 33554432 (32 MiB)
```

#### 4.1.1 Migration v1 (full DDL)
```sql
-- v1: library schema. Applied with PRAGMA user_version = 1 inside one transaction.

CREATE TABLE library_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
-- keys used: 'fts_built_version', 'last_reconcile_ms:<root_id>', 'schema_created_at_ms'

CREATE TABLE storage_roots (
    id           INTEGER PRIMARY KEY,
    path         TEXT    NOT NULL UNIQUE COLLATE NOCASE
                         CHECK (length(path) BETWEEN 3 AND 240),   -- canonical absolute, no trailing '\'
    state        TEXT    NOT NULL DEFAULT 'active'
                         CHECK (state IN ('active','legacy')),
    added_at_ms  INTEGER NOT NULL,
    last_reconcile_ms INTEGER NOT NULL DEFAULT 0
);
-- Exactly one row with state='active' at any time (enforced below).
CREATE UNIQUE INDEX storage_roots_one_active ON storage_roots(state) WHERE state = 'active';

CREATE TABLE games (
    id            TEXT    PRIMARY KEY,                      -- canonical GameId string, e.g. 'exe:eldenring.exe' or 'exe:eldenring.exe|steam:1245620'
    display_name  TEXT    NOT NULL CHECK (length(display_name) BETWEEN 1 AND 200),
    folder_name   TEXT    NOT NULL UNIQUE COLLATE NOCASE
                          CHECK (length(folder_name) BETWEEN 1 AND 80),
    exe_name      TEXT,
    store_id      TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX games_name ON games(display_name COLLATE NOCASE, id);

CREATE TABLE recordings (
    id              TEXT    PRIMARY KEY CHECK (length(id) = 26),     -- RecordingId (ULID)
    kind            TEXT    NOT NULL CHECK (kind IN ('clip','session')),
    session_id      TEXT    CHECK (session_id IS NULL OR length(session_id) = 26),
    part_no         INTEGER CHECK (part_no IS NULL OR part_no >= 1),
    game_id         TEXT    REFERENCES games(id) ON DELETE SET NULL,
    root_id         INTEGER NOT NULL REFERENCES storage_roots(id) ON DELETE RESTRICT,
    rel_path        TEXT    NOT NULL CHECK (length(rel_path) BETWEEN 1 AND 400),
    file_name       TEXT    NOT NULL,                              -- last segment of rel_path, NFC, for sort/search
    ext             TEXT    NOT NULL CHECK (ext IN ('mp4','mkv')),
    size_bytes      INTEGER NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    file_mtime_ms   INTEGER NOT NULL,
    created_at_ms   INTEGER NOT NULL,                              -- recording start (UTC); file ctime for external files
    added_at_ms     INTEGER NOT NULL,                              -- row insert time
    win_file_id     BLOB    CHECK (win_file_id IS NULL OR length(win_file_id) = 24),  -- u64 volume serial || 16-byte FILE_ID
    duration_ms     INTEGER NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    width           INTEGER CHECK (width IS NULL OR width > 0),
    height          INTEGER CHECK (height IS NULL OR height > 0),
    fps_num         INTEGER CHECK (fps_num IS NULL OR fps_num > 0),
    fps_den         INTEGER CHECK (fps_den IS NULL OR fps_den > 0),
    video_codec     TEXT,                                          -- 'h264' | 'hevc' | 'av1'
    container       TEXT,                                          -- 'mp4' | 'mkv' as reported by probe
    bitrate_kbps    INTEGER,
    audio_track_count INTEGER NOT NULL DEFAULT 0 CHECK (audio_track_count >= 0),
    probe_version   INTEGER NOT NULL DEFAULT 0,                    -- 0 = never probed
    status          TEXT    NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('recording','pending','ready','missing','error')),
    error_detail    TEXT,
    is_favorite     INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0,1)),
    UNIQUE (root_id, rel_path)
);

-- Sort/filter indexes. Every gallery sort key has an index ending in id for keyset tiebreak.
CREATE INDEX rec_sort_created  ON recordings(created_at_ms, id);
CREATE INDEX rec_sort_name     ON recordings(file_name COLLATE NOCASE, id);
CREATE INDEX rec_sort_size     ON recordings(size_bytes, id);
CREATE INDEX rec_sort_duration ON recordings(duration_ms, id);
CREATE INDEX rec_sort_kind     ON recordings(kind, created_at_ms, id);
CREATE INDEX rec_game_created  ON recordings(game_id, created_at_ms, id);
CREATE INDEX rec_fav_created   ON recordings(created_at_ms, id) WHERE is_favorite = 1;
CREATE INDEX rec_session       ON recordings(session_id, part_no) WHERE session_id IS NOT NULL;
CREATE INDEX rec_status        ON recordings(status, created_at_ms, id);
CREATE INDEX rec_fileid        ON recordings(win_file_id) WHERE win_file_id IS NOT NULL;
CREATE INDEX rec_root          ON recordings(root_id, status);

CREATE TABLE tags (
    id    INTEGER PRIMARY KEY,
    name  TEXT    NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 40),
    color TEXT    CHECK (color IS NULL OR (length(color) = 7 AND substr(color,1,1) = '#'))
);

CREATE TABLE recording_tags (
    recording_id TEXT    NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    tag_id       INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (recording_id, tag_id)
) WITHOUT ROWID;
CREATE INDEX rt_tag ON recording_tags(tag_id, recording_id);

CREATE TABLE audio_tracks (
    recording_id TEXT    NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    track_index  INTEGER NOT NULL CHECK (track_index >= 0),       -- 0-based stream order in container
    codec        TEXT    NOT NULL,                                 -- 'aac' | 'opus' | other
    channels     INTEGER NOT NULL CHECK (channels > 0),
    sample_rate  INTEGER NOT NULL CHECK (sample_rate > 0),
    bitrate_kbps INTEGER,
    language     TEXT    CHECK (language IS NULL OR length(language) BETWEEN 2 AND 8),
    title        TEXT    CHECK (title IS NULL OR length(title) <= 120),
    PRIMARY KEY (recording_id, track_index)
) WITHOUT ROWID;

CREATE TABLE thumbnails (
    recording_id    TEXT    NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL CHECK (kind IN ('poster','strip')),
    rel_file        TEXT    NOT NULL,                              -- '<recording_id>/poster.jpg' or '<recording_id>/strip.jpg'
    width           INTEGER NOT NULL CHECK (width > 0),           -- poster: 320; strip: cols*160
    height          INTEGER NOT NULL CHECK (height > 0),          -- poster: 180; strip: rows*90
    cols            INTEGER NOT NULL DEFAULT 1 CHECK (cols >= 1),
    rows            INTEGER NOT NULL DEFAULT 1 CHECK (rows >= 1),
    samples         INTEGER NOT NULL DEFAULT 1 CHECK (samples >= 1), -- strip: number of frames used
    interval_ms     INTEGER NOT NULL DEFAULT 0 CHECK (interval_ms >= 0),
    byte_size       INTEGER NOT NULL CHECK (byte_size > 0),
    source_size     INTEGER NOT NULL,                             -- recording.size_bytes when generated
    source_mtime_ms INTEGER NOT NULL,                             -- recording.file_mtime_ms when generated
    generated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (recording_id, kind)
) WITHOUT ROWID;

-- Full-text search. Maintained by Writer in the same transaction as recordings/tags writes.
-- Contains denormalized text; rebuilt from scratch when library_meta.fts_built_version != FTS_VERSION.
CREATE VIRTUAL TABLE recordings_fts USING fts5(
    rec_id UNINDEXED,
    file_name,
    game_name,
    tags,
    tokenize = "unicode61 remove_diacritics 2"
);

-- Persisted storage relocation / detach jobs (§5.6).
CREATE TABLE storage_jobs (
    id              INTEGER PRIMARY KEY,
    kind            TEXT    NOT NULL CHECK (kind IN ('move_existing','detach_root')),
    from_root_id    INTEGER NOT NULL REFERENCES storage_roots(id) ON DELETE RESTRICT,
    to_root_id      INTEGER REFERENCES storage_roots(id) ON DELETE RESTRICT, -- NULL for detach_root
    state           TEXT    NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued','running','paused','done','failed','cancelled')),
    total_items     INTEGER NOT NULL DEFAULT 0,
    done_items      INTEGER NOT NULL DEFAULT 0,
    total_bytes     INTEGER NOT NULL DEFAULT 0,
    done_bytes      INTEGER NOT NULL DEFAULT 0,
    error           TEXT,
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL,
    CHECK (kind <> 'move_existing' OR to_root_id IS NOT NULL)
);
CREATE INDEX storage_jobs_state ON storage_jobs(state);

CREATE TABLE storage_job_items (
    job_id       INTEGER NOT NULL REFERENCES storage_jobs(id) ON DELETE CASCADE,
    recording_id TEXT    NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    state        TEXT    NOT NULL DEFAULT 'todo' CHECK (state IN ('todo','copying','done','skipped','failed')),
    dest_rel_path TEXT,                                           -- path chosen at job start (collision-free)
    error        TEXT,
    PRIMARY KEY (job_id, recording_id)
) WITHOUT ROWID;
CREATE INDEX storage_job_items_todo ON storage_job_items(job_id, state);

-- Quota enforcement log (what was auto-deleted and why), capped at 500 rows by the writer.
CREATE TABLE deletion_log (
    id             INTEGER PRIMARY KEY,
    at_ms          INTEGER NOT NULL,
    recording_id   TEXT    NOT NULL,
    display_name   TEXT    NOT NULL,
    size_bytes     INTEGER NOT NULL,
    reason         TEXT    NOT NULL CHECK (reason IN ('quota','free_space','user_permanent','user_recycle','root_detached'))
);
CREATE INDEX deletion_log_at ON deletion_log(at_ms);
```

#### 4.1.2 Migration v1 post-steps (in the same transaction)
```sql
INSERT INTO library_meta(key, value) VALUES ('schema_created_at_ms', CAST(strftime('%s','now') AS TEXT) || '000');
INSERT INTO library_meta(key, value) VALUES ('fts_built_version', '1');
```

#### 4.1.3 Column semantics that matter

| Table.column | Semantics |
|---|---|
| `recordings.created_at_ms` | Recording start time (session/clip). External files: file creation time. Used for "Date" sort and quota oldest-first. |
| `recordings.file_mtime_ms` | Last-known mtime; change ⇒ reprobe (§5.5). |
| `recordings.win_file_id` | `VolumeSerialNumber` (u64, LE) ‖ `FILE_ID_INFO.FileId` (16 bytes). Used to pair rename events and detect moved files. |
| `recordings.status` | Lifecycle, §5.1. |
| `recordings.duration_ms = 0` with `probe_version = 0` | Not yet probed. Sorting by duration puts these first; the gallery shows "—" for them. |
| `games.id` | Canonical `GameId` string. Equal to `core::GameId::canonical()`. |
| `games.folder_name` | Filesystem folder name under the root (sanitized, unique, case-insensitive). Assigned once; changing `display_name` does not rename the folder. |
| `recordings_fts.tags` | Space-joined tag names at last write. Updated by the writer on tag change. |

#### 4.1.4 Reader SQL for "current root path" (used in every query)
```sql
SELECT r.id, sr.path || '\' || replace(r.rel_path, '/', '\') AS abs_path
FROM recordings r JOIN storage_roots sr ON sr.id = r.root_id;
```

### 4.2 Core model types

```rust
// model.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingKind { Clip, Session }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecStatus { Recording, Pending, Ready, Missing, Error }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ext { Mp4, Mkv }

#[derive(Clone, Debug, Serialize)]
pub struct RecordingRow {
    pub id: RecordingId,
    pub kind: RecordingKind,
    pub session_id: Option<SessionId>,
    pub part_no: Option<u32>,
    pub game_id: Option<GameId>,
    pub game_name: Option<String>,        // joined from games.display_name
    pub root_id: RootId,
    pub abs_path: String,                 // derived at read time (§4.1.4)
    pub file_name: String,
    pub ext: Ext,
    pub size_bytes: u64,
    pub created_at_ms: i64,
    pub file_mtime_ms: i64,
    pub duration_ms: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<(u32, u32)>,          // (num, den)
    pub video_codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub audio_tracks: Vec<AudioTrackInfo>,// loaded only in get_recording, empty in lists
    pub status: RecStatus,
    pub error_detail: Option<String>,
    pub is_favorite: bool,
    pub tags: Vec<TagRef>,                // id + name; loaded in lists (small, batch-joined)
    pub thumbs: ThumbRefs,                // which thumbnails exist + their URLs (clip://)
}

#[derive(Clone, Debug, Serialize)]
pub struct AudioTrackInfo {
    pub index: u32, pub codec: String, pub channels: u32, pub sample_rate: u32,
    pub bitrate_kbps: Option<u32>, pub language: Option<String>, pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TagRef { pub id: i64, pub name: String, pub color: Option<String> }

#[derive(Clone, Debug, Serialize, Default)]
pub struct ThumbRefs {
    pub poster_url: Option<String>,       // clip://thumb/<id>/poster.jpg
    pub strip: Option<StripInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StripInfo {
    pub url: String,                      // clip://thumb/<id>/strip.jpg
    pub cols: u32, pub rows: u32, pub samples: u32,
    pub cell_w: u32, pub cell_h: u32,     // 160 x 90
    pub interval_ms: u32,
}

#[derive(Clone, Debug)]
pub struct StorageRoot {
    pub id: RootId,
    pub path: PathBuf,
    pub state: RootState,                 // Active | Legacy
    pub online: bool,                     // runtime only
    pub recording_count: u64,
    pub total_bytes: u64,
}
```

### 4.3 Probing traits (implemented by `encode`)

```rust
// probe.rs — trait boundary; implementations live in crates/encode/src/ffmpeg/probe.rs
pub trait MediaProbe: Send + Sync {
    /// Header-level probe. Must not decode video. Bounded: probesize <= 5 MiB, analyzeduration <= 2 s.
    fn probe(&self, path: &Path) -> Result<ProbeResult, ProbeError>;
}

pub struct ProbeResult {
    pub container: String,                // 'mp4' | 'matroska,webm' normalized to 'mp4' | 'mkv'
    pub duration_ms: u64,
    pub video: Option<VideoInfo>,         // None => treat as error (no video stream)
    pub audio: Vec<AudioInfo>,
    pub bitrate_kbps: Option<u32>,
}
pub struct VideoInfo { pub codec: String, pub width: u32, pub height: u32, pub fps_num: u32, pub fps_den: u32 }
pub struct AudioInfo { pub codec: String, pub channels: u32, pub sample_rate: u32,
                       pub bitrate_kbps: Option<u32>, pub language: Option<String>, pub title: Option<String> }

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("file is incomplete (no moov/index yet)")] Incomplete,
    #[error("no decodable video stream")] NoVideo,
    #[error("ffmpeg: {0}")] Ffmpeg(String),
    #[error(transparent)] Io(#[from] std::io::Error),
}

/// Decoding of single frames for thumbnails. Keyframe-only decode, seek-before-input.
pub trait FrameSource: Send + Sync {
    /// Decode the first keyframe at or after `at_ms`, scaled to fit within max_w x max_h (letterboxed to exact size by caller).
    fn keyframe_at(&self, path: &Path, at_ms: u64, max_w: u32, max_h: u32) -> Result<RgbFrame, ProbeError>;
}
pub struct RgbFrame { pub width: u32, pub height: u32, pub rgb: Vec<u8> /* packed RGB24 */ }
```

### 4.4 Naming

```rust
// naming/mod.rs
pub struct NameTemplate { raw: String, tokens: Vec<Token> }

#[derive(Debug, Clone, PartialEq)]
enum Token { Lit(String), Game, Yyyy, Mm, Dd, HH, Mi, Ss, Kind, Part, Len, Id }

impl NameTemplate {
    /// Accepted tokens: {game} {yyyy} {MM} {dd} {HH} {mm} {ss} {kind} {part} {len} {id}
    /// Rules: 1..=120 chars raw; at least one of {game} {kind} {id} {yyyy}; no unknown tokens; braces balanced.
    pub fn parse(raw: &str) -> Result<Self, TemplateError>;
    pub fn default_template() -> Self; // "{game} {yyyy}-{MM}-{dd} {HH}-{mm}-{ss} {kind}"
    pub fn render(&self, ctx: &NameContext) -> String; // returns stem only, sanitized
}

pub struct NameContext<'a> {
    pub game_display: Option<&'a str>,    // None => unknown_game_folder text
    pub start_local: chrono::DateTime<chrono::Local>,
    pub kind: RecordingKind,
    pub part_no: Option<u32>,             // sessions
    pub clip_len_s: Option<u32>,          // clips
    pub id: &'a RecordingId,
}

/// Replace illegal characters with '_' ; collapse runs of whitespace; trim trailing dots/spaces;
/// strip control chars; NFC; if result is a reserved name, prefix '_'; truncate to `max_chars`.
pub fn sanitize_component(raw: &str, max_chars: usize) -> String;

/// Strict validation for user-typed rename input. Does not modify except trim + NFC.
pub fn validate_user_stem(raw: &str) -> Result<String, NameError>;

#[derive(Debug, thiserror::Error)]
pub enum NameError {
    #[error("contains illegal characters: {0:?}")] IllegalChars(Vec<char>),
    #[error("reserved name")] Reserved,
    #[error("name is empty")] Empty,
    #[error("name too long (max {max})")] TooLong { max: usize },
    #[error("name cannot end with a dot or space")] TrailingDotOrSpace,
}
```

Full path budget: `root.path (≤ 240) + '\' + game_folder (≤ 80) + '\' + stem (≤ 120) + '.' + ext (3)`. Stems are capped so that the total stays ≤ 259 characters (MAX_PATH). If the root is so long that the budget cannot be met with a 16-character minimum stem, the root is rejected at validation time with "This folder path is too long for ClipCheck (max N characters)."

### 4.5 Rename, delete, and storage types

```rust
// rename.rs
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    #[error(transparent)] InvalidName(#[from] NameError),
    #[error("a file named '{0}' already exists in this folder")] Collision(String),
    #[error("recording is still being written")] Busy,
    #[error("file is in use by another program")] InUse,
    #[error("recording not found")] NotFound,
    #[error("file is missing from disk")] MissingOnDisk,
    #[error("I/O error: {0}")] Io(#[from] std::io::Error),
    #[error("database error: {0}")] Db(#[from] rusqlite::Error),
}

impl Writer {
    /// `new_stem` is the user's input without extension. Returns the updated row.
    pub fn rename(&mut self, id: &RecordingId, new_stem: &str) -> Result<RecordingRow, RenameError>;
}

// delete.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteMode { RecycleBin, Permanent }

#[derive(Debug, Serialize)]
pub struct DeleteReport { pub deleted: Vec<RecordingId>, pub failed: Vec<(RecordingId, String)>, pub bytes_freed: u64 }

impl Writer {
    pub fn delete(&mut self, ids: &[RecordingId], mode: DeleteMode, reason: DeleteReason) -> Result<DeleteReport, LibError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteReason { UserRecycle, UserPermanent, Quota, FreeSpace, RootDetached }

// recycle.rs
pub fn send_to_recycle_bin(paths: &[PathBuf]) -> Result<(), RecycleError>;
#[derive(Debug, thiserror::Error)]
pub enum RecycleError { #[error("Recycle Bin unavailable for this drive")] Unavailable, #[error("operation failed: 0x{0:08X}")] Hresult(u32) }

// storage (roots.rs, storage_job.rs)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaveBehind { MoveExisting, LeaveExisting }

#[derive(Debug, Clone, Serialize)]
pub struct StorageJobStatus {
    pub id: i64, pub kind: String, pub state: String,
    pub done_items: u64, pub total_items: u64, pub done_bytes: u64, pub total_bytes: u64,
    pub error: Option<String>,
}
```

### 4.6 Library facade and the trait the rest of the service uses

```rust
// lib.rs
pub struct Library { writer: WriterHandle, readers: ReaderPool, cfg: Arc<ArcSwap<LibraryConfig>>, /* … */ }

/// Used by session/buffer/clipedit so they do not depend on rusqlite.
pub trait LibraryHandle: Send + Sync {
    /// Called when a session part or clip file is created. Inserts row with status 'recording'
    /// (sessions) or 'pending' (clips, already complete). Chooses the final file name under the active root.
    fn begin_recording(&self, req: BeginRecording) -> Result<BeginRecordingResult, LibError>;
    /// Called after the writer has finalized the file. Triggers probe + thumbnails + quota check.
    fn finish_recording(&self, id: &RecordingId, final_size: u64) -> Result<(), LibError>;
    /// Called if a session part is abandoned (crash, disk full). Row status -> 'error'.
    fn fail_recording(&self, id: &RecordingId, detail: &str) -> Result<(), LibError>;
}

pub struct BeginRecording {
    pub id: RecordingId,                  // generated by caller so it can be embedded in logs
    pub kind: RecordingKind,
    pub session: Option<(SessionId, u32)>,// (session_id, part_no)
    pub game: Option<GameInfo>,           // None => unknown_game_folder
    pub start_utc_ms: i64,
    pub clip_len_s: Option<u32>,
    pub ext: Ext,
}
pub struct GameInfo { pub id: GameId, pub display_name: String, pub exe_name: String, pub store_id: Option<String> }
pub struct BeginRecordingResult { pub abs_path: PathBuf }   // caller opens this path for writing

#[derive(Debug, thiserror::Error)]
pub enum LibError {
    #[error("database: {0}")] Db(#[from] rusqlite::Error),
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("not found")] NotFound,
    #[error("invalid request: {0}")] Invalid(String),
    #[error("root offline: {0}")] RootOffline(String),
    #[error("insufficient disk space")] DiskFull,
    #[error(transparent)] Rename(#[from] RenameError),
}
```

### 4.7 Query API

```rust
// query.rs
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct RecordingQuery {
    pub text: Option<String>,              // FTS5; tokens are quoted and prefix-matched
    pub kinds: Vec<RecordingKind>,         // empty = both
    pub game_ids: Vec<GameId>,             // empty = any; ids with NULL game are addressed via `include_unknown_game`
    pub include_unknown_game: bool,
    pub tag_ids: Vec<i64>,
    pub tag_match: TagMatch,               // Any (default) | All
    pub favorites_only: bool,
    pub statuses: Vec<RecStatus>,          // empty = {Ready, Pending}; Missing requires explicit inclusion
    pub created_from_ms: Option<i64>,
    pub created_to_ms: Option<i64>,
    pub min_duration_ms: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub sort: SortKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum TagMatch { #[default] Any, All }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SortKey { pub field: SortField, pub dir: SortDir }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum SortField { Name, Size, Created, Duration, Game, Kind }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum SortDir { Asc, Desc }

impl Default for SortKey { fn default() -> Self { SortKey { field: SortField::Created, dir: SortDir::Desc } } }

#[derive(Debug, Serialize)]
pub struct Page {
    pub items: Vec<RecordingRow>,
    pub next_cursor: Option<String>,       // None = last page
    pub total: Option<u64>,                // present only when requested (first page)
}

impl Library {
    /// `limit` is clamped to 1..=200. `cursor` is produced by a previous page for the SAME query
    /// (checked via a hash of the query; mismatch => LibError::Invalid).
    pub fn query(&self, q: &RecordingQuery, cursor: Option<&str>, limit: u32, want_total: bool) -> Result<Page, LibError>;

    /// Counts per game and per tag for the sidebar, computed over the query with that facet's own filter removed.
    pub fn facets(&self, q: &RecordingQuery) -> Result<Facets, LibError>;

    pub fn get(&self, id: &RecordingId) -> Result<Option<RecordingRow>, LibError>;
}

#[derive(Debug, Serialize)]
pub struct Facets { pub games: Vec<FacetCount<GameId>>, pub tags: Vec<FacetCount<i64>>, pub kinds: [u64; 2] }
#[derive(Debug, Serialize)]
pub struct FacetCount<K> { pub key: K, pub label: String, pub count: u64 }
```

#### 4.7.1 Cursor format
Opaque to the UI: `base64url( JSON{ "v": 1, "q": <u64 hash of canonical JSON of RecordingQuery>, "k": <last sort value>, "id": <last id> } )`. `k` is `i64` for numeric sorts, `String` for name/game/kind (game uses `games.display_name`, then `games.id`).

#### 4.7.2 Keyset SQL (example: Name ascending)
```sql
SELECT r.*, g.display_name AS game_name, sr.path AS root_path
FROM recordings r
LEFT JOIN games g ON g.id = r.game_id
JOIN storage_roots sr ON sr.id = r.root_id
WHERE r.status IN (?, ?)                              -- default statuses
  AND (:cursor_k IS NULL OR (r.file_name COLLATE NOCASE > :cursor_k
       OR (r.file_name COLLATE NOCASE = :cursor_k AND r.id > :cursor_id)))
ORDER BY r.file_name COLLATE NOCASE ASC, r.id ASC
LIMIT :limit_plus_one;                                -- fetch limit+1 to know if next page exists
```
The query builder appends filter predicates as `AND r.id IN (SELECT recording_id FROM recording_tags WHERE tag_id IN (...) GROUP BY recording_id HAVING COUNT(DISTINCT tag_id) = :n)` for `TagMatch::All`, and `... WHERE tag_id IN (...)` for `Any`. Text search is `AND r.id IN (SELECT rec_id FROM recordings_fts WHERE recordings_fts MATCH :fts)`, because `recordings_fts.rec_id` is the ULID text (UNINDEXED), not the rowid.

Index usage: the planner must use the sort index (`rec_sort_name`) for the ORDER BY. Enforced by `EXPLAIN QUERY PLAN` assertions in tests (§10).

#### 4.7.3 FTS query construction
Input `text` is split on whitespace; each token is stripped of FTS operators and wrapped as `"token"*`; tokens are AND-ed (`"a"* "b"*`). Empty after stripping → no FTS predicate.

### 4.8 Watcher and reconciler

```rust
// watcher.rs
pub struct FolderWatcher { /* one notify::RecommendedWatcher per online root */ }

#[derive(Debug, Clone)]
pub enum FsChange {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    Overflow { root: RootId },            // event loss → full reconcile
}

impl FolderWatcher {
    pub fn start(root: RootId, path: &Path, tx: Sender<FsChange>) -> Result<Self, WatchError>;
    pub fn stop(self);
}

// reconcile.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileMode {
    /// Compare DB with a full directory walk. Insert new, reprobe changed, mark/remove gone, pair renames by file id.
    Full,
    /// Only the given paths (debounced watcher batch).
    Paths,
}

#[derive(Debug, Default, Serialize)]
pub struct ReconcileReport {
    pub root: RootId, pub scanned: u64, pub inserted: u64, pub updated: u64,
    pub relinked: u64, pub marked_missing: u64, pub removed: u64, pub duration_ms: u64,
}

impl Writer {
    pub fn reconcile(&mut self, root: RootId, mode: ReconcileMode, paths: Option<&[PathBuf]>) -> Result<ReconcileReport, LibError>;
}
```

### 4.9 Thumbnails

```rust
// thumbs.rs
pub enum ThumbKind { Poster, Strip }

pub struct ThumbService { dir: PathBuf, frames: Arc<dyn FrameSource>, lib: Arc<Library>, /* worker */ }

pub const POSTER_W: u32 = 320;  pub const POSTER_H: u32 = 180;
pub const CELL_W: u32 = 160;    pub const CELL_H: u32 = 90;
pub const STRIP_COLS: u32 = 10; // rows = ceil(samples / 10), max 6 rows (60 samples)

impl ThumbService {
    /// Returns immediately if an up-to-date thumbnail exists. Otherwise enqueues generation and returns Pending.
    pub fn ensure(&self, id: &RecordingId, kind: ThumbKind, priority: Priority) -> ThumbState;
    pub fn invalidate(&self, id: &RecordingId);     // on size/mtime change or rename-with-new-content
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority { Interactive, Background }      // Interactive = UI asked; Background = backfill
pub enum ThumbState { Ready(ThumbRefs), Pending, Failed(String) }
```

### 4.10 Quota and storage jobs

```rust
// quota.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoDeletePolicy { Off, Oldest }

#[derive(Clone, Debug)]
pub struct QuotaPolicy {
    pub quota_bytes: Option<u64>,         // None = unlimited
    pub min_free_bytes: u64,              // 0 = disabled
    pub auto_delete: AutoDeletePolicy,
}

#[derive(Debug, Serialize)]
pub struct QuotaStatus {
    pub used_bytes: u64,
    pub quota_bytes: Option<u64>,
    pub free_bytes_on_active_root: u64,
    pub state: QuotaState,
    pub protected_bytes: u64,             // favorites + in-progress; cannot be reclaimed
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum QuotaState { Ok, OverLimit, FreeSpaceLow, Blocked }   // Blocked = over and nothing deletable

impl Writer {
    pub fn enforce_quota(&mut self, policy: &QuotaPolicy, now_ms: i64) -> Result<QuotaReport, LibError>;
    pub fn quota_status(&self, policy: &QuotaPolicy) -> Result<QuotaStatus, LibError>;
}
#[derive(Debug, Serialize)]
pub struct QuotaReport { pub deleted: Vec<RecordingId>, pub reclaimed_bytes: u64, pub status: QuotaStatus }

// storage_job.rs
impl Writer {
    /// Validates target, inserts storage_roots row (state 'active'), demotes previous active to 'legacy' (LeaveExisting) or keeps it
    /// as the job source (MoveExisting), creates storage_jobs + storage_job_items, returns job id.
    pub fn set_active_root(&mut self, new_path: &Path, behind: LeaveBehind) -> Result<StorageJobId, LibError>;
    pub fn start_detach_root(&mut self, root_id: RootId) -> Result<StorageJobId, LibError>;
    pub fn cancel_job(&mut self, job: StorageJobId) -> Result<(), LibError>;
    pub fn job_status(&self, job: StorageJobId) -> Result<StorageJobStatus, LibError>;
    /// Driven by a dedicated job thread; processes up to `budget` items per call so writes interleave with others.
    pub fn step_job(&mut self, job: StorageJobId, budget: Duration) -> Result<JobStep, LibError>;
}
pub enum JobStep { Progressed, Done, Paused(String) }
```

### 4.11 Config struct (owned by this feature; §6 lists keys)

```rust
// core::config::LibraryConfig — deserialized from [library] in config.toml
pub struct LibraryConfig {
    pub recordings_root: PathBuf,                 // active root
    pub per_game_folders: bool,
    pub filename_template: String,
    pub unknown_game_folder: String,
    pub quota_gb: u32,                            // 0 = unlimited
    pub auto_delete: AutoDeletePolicy,
    pub min_free_space_gb: u32,
    pub watch_enabled: bool,
    pub reconcile_interval_min: u32,
    pub strip_samples: u32,
    pub thumbnails_background: bool,
}
```

### 4.12 Tauri command signatures (src-tauri, UI side)

```rust
// Read-only: executed on the UI's read-only connection pool
#[tauri::command] fn library_query(q: RecordingQuery, cursor: Option<String>, limit: u32, want_total: bool) -> Result<Page, String>;
#[tauri::command] fn library_facets(q: RecordingQuery) -> Result<Facets, String>;
#[tauri::command] fn library_get(id: String) -> Result<Option<RecordingRow>, String>;
#[tauri::command] fn library_storage_info() -> Result<StorageInfo, String>;      // roots, free space, counts (reads DB + GetDiskFreeSpaceExW)
#[tauri::command] fn library_quota_status() -> Result<QuotaStatus, String>;     // read-only computation
#[tauri::command] fn library_job_status(job_id: i64) -> Result<StorageJobStatus, String>;
#[tauri::command] fn library_template_preview(template: String) -> Result<TemplatePreview, String>; // pure function, no DB
#[tauri::command] fn library_reveal(id: String) -> Result<(), String>;           // explorer.exe /select,"<path>" (no IPC needed)

// Mutations: forwarded to clipcheckd over the named pipe; the command awaits the service reply
#[tauri::command] async fn library_rename(id: String, new_stem: String) -> Result<RecordingRow, String>;
#[tauri::command] async fn library_delete(ids: Vec<String>, mode: DeleteMode) -> Result<DeleteReport, String>;
#[tauri::command] async fn library_set_favorite(ids: Vec<String>, value: bool) -> Result<u32, String>;
#[tauri::command] async fn library_set_tags(ids: Vec<String>, add: Vec<i64>, remove: Vec<i64>) -> Result<u32, String>;
#[tauri::command] async fn library_tag_create(name: String, color: Option<String>) -> Result<TagRef, String>;
#[tauri::command] async fn library_tag_delete(tag_id: i64) -> Result<u32, String>;
#[tauri::command] async fn library_thumb_request(id: String, kind: String) -> Result<(), String>; // fire-and-forget; UI listens for event
#[tauri::command] async fn library_set_recordings_root(path: String, behind: LeaveBehind) -> Result<i64, String>;
#[tauri::command] async fn library_job_cancel(job_id: i64) -> Result<(), String>;
#[tauri::command] async fn library_detach_root(root_id: i64) -> Result<i64, String>;
#[tauri::command] async fn library_quota_run_now() -> Result<QuotaReport, String>;
#[tauri::command] async fn library_permanent_delete_confirmed(ids: Vec<String>) -> Result<DeleteReport, String>; // same as library_delete(Permanent); separate for audit
```

Tauri event names emitted to the UI (see §7 for payloads): `library://changed`, `library://job`, `library://quota`, `library://thumb`.

---

## 5. Algorithms and state machines

### 5.1 Recording row lifecycle

```
                 begin_recording (session part)
   (none) ───────────────────────────────────────► recording
                                                      │ finish_recording(size)
                                                      ▼
   external file seen (watcher) ──► pending ──► ready ◄──── probe OK (probe_version = current)
                                       │          │
                       probe Incomplete│          │ file gone (watcher/reconcile)
                                       ▼          ▼
                                    pending    missing ──(file reappears, same win_file_id)──► pending
                                       │          │
                       probe NoVideo / repeated   │ removed by user / reconcile after 30 days
                       failure (3x)               ▼
                                     error     (row deleted)
   recording ──fail_recording──► error
   recording ──crash recovery (file exists)──► pending ──► ready
   recording ──crash recovery (file gone)───► missing
```

Rules:
- `recording` rows are never renamed, deleted by reconcile, reprobed, or auto-deleted. The writer owns them.
- `pending` rows are probed by the probe queue; the probe queue retries `Incomplete` every 10 s up to 6 times (1 min), then `error`.
- `missing` rows are hidden by default. A missing row is deleted automatically after 30 days, or immediately when the user chooses "Remove from library". Deletion of a missing row removes only the DB row and thumbnails.

### 5.2 File name generation for new recordings (`begin_recording`)

```
fn choose_path(req: &BeginRecording, cfg: &LibraryConfig, root: &StorageRoot) -> (RelPath, PathBuf):
  1. folder = if cfg.per_game_folders:
        if req.game is Some(g): game_folder_for(g)     // §5.2.1
        else: sanitize(cfg.unknown_game_folder, 80) or "Other"
     else: ""                                          // flat
  2. stem = template.render(NameContext{ game_display, start_local = to_local(req.start_utc_ms), kind, part_no, clip_len_s, id })
     if stem.is_empty(): stem = format!("ClipCheck {}", local_ts_string)   // edge case E18
  3. Budget: max_stem = min(120, 259 - len(root.path) - 1 - len(folder)-(folder?1:0) - 1 - 3)
     if max_stem < 16: fail LibError::Invalid("path too long")
     stem = truncate_chars(stem, max_stem)  (truncate game name first, keep {kind} and time intact by re-rendering with game shortened)
  4. candidate = folder/stem.ext ; if exists on disk OR in DB (same root, NFC, case-insensitive) → append " (2)", " (3)", ... up to 999
  5. create folder if missing (create_dir_all); return (rel, abs)
  6. Insert row status = recording (sessions) / pending (clips, after clipedit writes file), with win_file_id = NULL until file exists
```

#### 5.2.1 Game folder assignment
```
fn game_folder_for(g: &GameInfo) -> String:
  if games has id = g.id: return games.folder_name
  base = sanitize_component(g.display_name, 60) or "Game"
  name = base; n = 2
  while exists games.folder_name ~ name (NOCASE) or name is reserved: name = format!("{base} ({n})"); n++
  INSERT games(id, display_name, folder_name=name, exe_name, store_id, created, updated)
  return name
```
Display name changes update `games.display_name` only; folder name is stable.

### 5.3 Rename algorithm

```
fn rename(id, new_stem_raw):
  1. stem = validate_user_stem(new_stem_raw)?                       -- E: illegal chars / reserved / trailing dot
  2. BEGIN IMMEDIATE (writer txn)
     row = SELECT ... FROM recordings WHERE id=?  ;  if none → NotFound
     if row.status == recording → Busy
     if row.status == missing   → MissingOnDisk
  3. new_rel = dirname(row.rel_path) + "/" + stem + "." + row.ext
     new_abs = root.path + "\" + new_rel
     if new_rel.nfc_lower == row.rel_path.nfc_lower → if identical bytes: return row (no-op); else case-only change (go to 4 with two-step)
  4. Collision pre-check (DB):   SELECT 1 FROM recordings WHERE root_id=? AND rel_path = ? COLLATE NOCASE AND id <> ?  → Collision
     Collision pre-check (FS):  if fs_exists_case_insensitive(new_abs) and not the same file (win_file_id differs) → Collision
  5. Disk rename with retry:
       if case-only: MoveFileExW(old, tmp) then MoveFileExW(tmp, new)      -- tmp = old + ".clipcheck-rename-<ulid>.tmp"
       else:         MoveFileExW(old, new, 0)                              -- no MOVEFILE_REPLACE_EXISTING
       on ERROR_SHARING_VIOLATION / ERROR_ACCESS_DENIED: retry 4x (100, 250, 500, 1000 ms); then InUse
       on ERROR_ALREADY_EXISTS / ERROR_FILE_EXISTS: Collision (race with another program)
       on ERROR_FILE_NOT_FOUND: MissingOnDisk (and mark row 'missing' in the txn)
  6. UPDATE recordings SET rel_path=?, file_name=?, file_mtime_ms = ? WHERE id=?
     Update recordings_fts row (file_name)
     COMMIT
  7. If step 6 fails (DB error): MoveFileExW(new, old) to restore; return Db error.
     If restore also fails: log CRITICAL; set row status 'pending' with rel_path = NEW (the file IS at new path);
     the reconciler will re-link via win_file_id on next pass. (Edge E7.)
  8. Thumbnails keyed by id → unaffected. Emit library://changed(updated: [id]).
```

Atomicity statement: the filesystem rename and the DB update are not one atomic unit. The algorithm guarantees that after any failure the row either points to the file's real location or is marked for reconciliation by file id. The user never observes a row pointing at a path that does not exist without the watcher/reconciler correcting it within one reconcile cycle (≤ 3 s for watched roots).

### 5.4 Delete algorithm

```
fn delete(ids, mode, reason):
  1. rows = SELECT ... WHERE id IN ids
  2. For each row:
       if status == recording → failed("in progress")
       if mode == RecycleBin and reason is UserRecycle:
           collect abs paths
  3. If any RecycleBin: send_to_recycle_bin(paths_batch)           -- one IFileOperation call for the batch
       on RecycleError::Unavailable → return Err (UI offers permanent)
       on partial failure: per-item retry once after 200 ms; remaining → failed[]
  4. For Permanent / Quota / FreeSpace: DeleteFileW per path; sharing violation retry as in §5.3
       (a file that is open → failed("in use"), not deleted from DB)
  5. BEGIN IMMEDIATE
       DELETE FROM recordings WHERE id IN (succeeded)      -- cascades recording_tags, audio_tracks, thumbnails
       DELETE FROM recordings_fts WHERE rec_id IN (succeeded)
       INSERT INTO deletion_log(...) for each succeeded
     COMMIT
  6. Thumbnail directories: remove_dir_all(thumbs/<id>) best effort (orphan sweeper at startup catches leftovers)
  7. Return DeleteReport; emit library://changed(removed: [...])
```

Note: a file sent to the Recycle Bin is no longer under the root, so the watcher sees "Removed" and the reconciler sees no row (already deleted) → no action. Ordering: step 3 precedes step 5 deliberately; if step 5 fails, the watcher's Removed event will delete the row later.

### 5.5 Watcher and reconciliation

#### 5.5.1 Event pipeline (per root)
```
notify events ──► raw queue ──► debounce (500 ms quiet window, max hold 3 s)
                               ──► classify:
                                     Created(p) / Modified(p) with ext in {mp4,mkv} and not temp name
                                     Removed(p)
                                     Renamed{from,to}
                                     Overflow → schedule Full reconcile for root (ignore other events)
                               ──► Writer command batch (≤ 200 paths per batch)
```

Temp names ignored: names starting with `~`, ending with `.tmp`, `.part`, or containing `.clipcheck-rename-`. Files under `__` prefixed folders ignored.

#### 5.5.2 Handling a Created or Modified path
```
on Created/Modified(p):
  row = by rel_path (root, rel)
  if row exists:
      if row.status == recording → ignore
      fs = metadata(p)
      if fs.size != row.size_bytes or fs.mtime != row.file_mtime_ms:
          if row.status == missing: status = pending (relinked)
          size/mtime updated; invalidate thumbnails; enqueue probe
  else:
      id_new = FileId(p)
      if exists row with same win_file_id and rel_path not on disk → RENAME (relink) — §5.5.4
      else: INSERT row (kind=clip if name starts with 'clip' else session?  →  kind inferred: clip if template kind token present 'clip-', else 'session'; status=pending, created_at=ctime, game by folder match)
      enqueue probe with stability check (below)
```

#### 5.5.3 Stability check before probing an external file
```
probe_when_stable(p):
  s1 = size(p); sleep 1 s; s2 = size(p); if s1 != s2 → retry later (up to 60 s)
  open(p, FILE_SHARE_READ) must succeed; if sharing violation → retry in 2 s (writer still has it)
  then MediaProbe::probe(p); on Incomplete → pending, retry per §5.1
```

#### 5.5.4 Rename pairing
```
on Renamed{from, to}:
  row_from = by rel_path(from)
  if row_from exists → UPDATE rel_path = rel(to), file_name = name(to) ; status unchanged; keep favorites/tags
  else (not indexed) → treat as Created(to)
on Removed(from) without a paired Created within 2 s:
  if row exists:
     status = missing  (do not delete immediately)
     and remember (win_file_id) for 30 days
Created(to) later with same win_file_id as a 'missing' row → RELINK: update rel_path, status = ready, file_mtime etc.
```

#### 5.5.5 Full reconcile (`ReconcileMode::Full`)
```
1. Walk root recursively (FindFirstFileExW with FIND_FIRST_EX_LARGE_FETCH, skip reparse points), collecting (rel, size, mtime, file_id_lazy)
   Only ext mp4/mkv, not temp names.
2. Load all rows for root: map rel_path(NFC, lower) → row ; map win_file_id → row.
3. For each disk file F:
     a) row by rel_path exists:
          unchanged (size/mtime equal) → skip
          changed → update, reprobe
     b) no row by path, but row by win_file_id exists and row.rel_path not on disk → relink (rename) 
     c) else → insert new row (external)
4. For each row not matched to a disk file:
     status 'recording' → skip (writer's)
     status 'missing' older than 30 days → delete row (reason: none; log)
     else → status 'missing' (first time) ; win_file_id retained
5. Update storage_roots.last_reconcile_ms
Budget: 10,000 files in < 3 s (warm cache), see §9.
```

Full reconcile holds the writer for at most 200 rows per transaction (chunked), so UI-initiated mutations interleave.

### 5.6 Storage relocation and detach jobs

```
set_active_root(new_path, behind):
  1. Validate (§2.5). Canonicalize new_path (GetFinalPathNameByHandleW). Check overlap with all roots (prefix, case-insensitive, both directions).
  2. Check free space on new volume ≥ (MoveExisting ? total_bytes_of_active_root_rows : 0) + 1 GiB.
  3. BEGIN IMMEDIATE:
       UPDATE storage_roots SET state='legacy' WHERE state='active'   -- demote first: the partial unique index allows one 'active'
       INSERT storage_roots(path=new, state='active')                  -- (both cases; old root keeps its rows)
     If MoveExisting:
       INSERT storage_jobs(kind='move_existing', from_root_id=old, to_root_id=new, state='queued', totals=…)
       INSERT storage_job_items(job_id, recording_id, 'todo') for each row where root_id = old and status <> 'recording'
       (after job completes the old root is demoted to 'legacy' with zero rows and is then removed automatically)
     COMMIT
  4. Update config recordings_root = new_path (via settings writer). Future begin_recording uses the new active root immediately, even while the move job runs.

step_job(job, budget):
  loop until budget exhausted:
    item = SELECT next 'todo' item
    if none: mark job done; if from_root has 0 rows: DELETE storage_roots(from) (only if LeaveExisting never applied) ; return Done
    rec = row; src = from_root + rel ; if status == missing: mark item skipped; continue
    dst_rel = reserve collision-free rel in to_root (same rel_path if free; else suffix " (2)")
    mark item 'copying' (txn)
    copy: CopyFileExW(src, dst_tmp = dst + ".clipcheck-move-<ulid>.tmp", COPY_FILE_NO_BUFFERING? no; flags: 0)
          → verify size(dst_tmp) == size(src) ; FlushFileBuffers(dst_tmp)
          → MoveFileExW(dst_tmp, dst, 0)           -- atomic on same volume; cross-volume already copied to tmp
          → DeleteFileW(src)                       -- on failure: log warning, leave src; row points to dst anyway
    BEGIN IMMEDIATE:
       UPDATE recordings SET root_id = to_root, rel_path = dst_rel WHERE id = ?
       UPDATE storage_job_items SET state='done', dest_rel_path=? WHERE ...
       UPDATE storage_jobs SET done_items+1, done_bytes+size, updated = now
    COMMIT
  Crash safety: a crash between MoveFileExW(dst) and the DB commit leaves the file at dst and the row at src.
    On restart, the item is 'copying'. Recovery: if dst exists with size == src size and src exists → treat as copied, verify then delete src,
    update row. If dst is missing → re-copy. If both missing → item failed (file lost) and row marked missing.
  Cancel: stop at next item boundary; job 'cancelled'; rows already moved stay in to_root; remaining rows stay in from_root.
    Roots are then both registered (from_root becomes 'legacy' if it still has rows). No data loss.
```

Detach (`start_detach_root`): deletes the root's recordings rows (after a confirmation that shows the count), cascading to thumbnails, tags, tracks; files untouched; `deletion_log` rows with reason `root_detached`. The root row is deleted. Runs as a single transaction in chunks of 500 rows; resumable by job row. Rejected for the active root.

### 5.7 Quota and free-space enforcement

```
enforce_quota(policy, now):
  used = SUM(size_bytes) over all roots WHERE status <> 'missing'      -- missing files consume no disk
  free = GetDiskFreeSpaceExW(active_root)
  over_quota = policy.quota_bytes.map(|q| used > q).unwrap_or(false)
  low_free   = policy.min_free_bytes > 0 && free < policy.min_free_bytes
  if !(over_quota || low_free) or policy.auto_delete == Off:
      return status(state = if over_quota||low_free {OverLimit/FreeSpaceLow} else {Ok})
  need_bytes_quota = over_quota ? used - quota : 0
  need_bytes_free  = low_free ? min_free - free : 0
  target = max(need_bytes_quota, need_bytes_free)
  candidates = SELECT id, size_bytes, root_id, rel_path FROM recordings
               WHERE is_favorite = 0
                 AND status = 'ready'
                 AND created_at_ms < now - 60_000       -- never the file just written
               ORDER BY created_at_ms ASC, id ASC
  reclaimed = 0; deleted = []
  for c in candidates:
     if reclaimed >= target: break
     if file is open by another process (exclusive open fails) → skip
     delete(c.id, Permanent, reason = Quota|FreeSpace)   -- D4
     reclaimed += c.size_bytes; deleted.push(c.id)
  if reclaimed < target: state = Blocked (protected or in-progress files prevent reaching the limit)
  return report
```

Protected set: favorites, `recording` rows, rows created in the last 60 s, rows in an active storage job's `copying` state. Protected bytes are reported in `QuotaStatus.protected_bytes`.

Triggers: after `finish_recording`, after a probe completes (size known), on hourly timer, on policy change, and on startup. Writes are serialized through the writer; an enforcement run is limited to 50 deletions per invocation to keep the writer responsive; remaining work is re-queued.

### 5.8 Crash recovery at startup

```
1. Delete orphan thumbnail directories (no matching row)                             -- cheap, batch of 100
2. rows status='recording':
     file exists → status='pending'  → probe queue (fragmented MP4/MKV usually readable to last fragment)
     file missing → status='error' with detail 'interrupted; file not found'
3. storage_jobs in 'running' → 'paused' (resumes on user action or automatically on startup if from_root online and to_root online)
4. storage_job_items in 'copying' → recovery per §5.6
5. FTS: if library_meta.fts_built_version != CURRENT → rebuild recordings_fts from recordings+tags (single txn, chunked)
6. Run Full reconcile for each online root
```

### 5.9 Query execution plan (summary)

```
query(q, cursor, limit):
  L = clamp(limit, 1, 200)
  sort = q.sort ; key_expr = map(sort.field) ; order = (key_expr dir, r.id dir)
  where = build_predicates(q)                              -- status, kinds, game, tags, fav, ranges, fts
  if cursor: decode → verify query hash → add keyset predicate with ( k, id )
  rows = SELECT ... ORDER BY order LIMIT L+1
  next = rows.len() > L ? encode(last visible row) : None
  total = if want_total: SELECT COUNT(*) with same where (no keyset) — uses rec_* indexes, measured ≤ 15 ms at 50k
  attach tags (batch IN query), thumbs (batch thumbnails IN query), audio_tracks only in get()
```

Stable pagination under insertion: keyset ensures a newly inserted row with a sort key ahead of the cursor shifts the next page by one row at most (the UI dedupes by id). Rows deleted between pages simply do not appear.

### 5.10 Thumbnail generation

```
poster(id):
  row = get(id); dur = row.duration_ms; if dur == 0 → fail("unprobed")
  t = clamp(dur * 0.10, 1000, dur - 500)  ms
  frame = FrameSource.keyframe_at(path, t, 320, 180)         -- letterboxed to 320x180 with black bars
  encode JPEG q=80 → write thumbs/<id>/poster.jpg (tmp + rename)
  UPSERT thumbnails(kind='poster', width=320, height=180, source_size, source_mtime, ...)

strip(id, samples = cfg.strip_samples, default 60):
  n = clamp(samples, 10, 60); cols = 10; rows = ceil(n/10)
  interval = dur / n
  for i in 0..n:
     t = (i + 0.5) * interval  (ms)
     frame = keyframe_at(path, t, 160, 90)                    -- keyframe-only decode ⇒ ~1 GOP decode per sample
     blit into sprite at (i % cols)*160, (i / cols)*90
  encode JPEG q=75 → thumbs/<id>/strip.jpg
  UPSERT thumbnails(kind='strip', width=cols*160, height=rows*90, cols, rows, samples=n, interval_ms)

Scheduling:
  - Interactive requests run on the 'thumb-interactive' lane (1 worker)
  - Background backfill runs on 'thumb-background' lane (1 worker, THREAD_PRIORITY_BELOW_NORMAL, background mode for I/O)
  - Poster is generated first, strip second; new recordings enqueue both as Background after probe success.
  - Generation of a strip for a 1 h recording with 60 samples: 60 keyframe seeks ≈ 1.5–3 s total CPU with HW decode disabled.
  - If the recording is still 'recording' → no thumbnails until finished (strip would be incomplete). Poster allowed once ≥ 5 s exists.
Invalidation: on reconcile when size/mtime differ from the thumbnails row's source_* columns → delete row + files, re-enqueue.
```

### 5.11 Probe pipeline
```
ProbeService worker (1 thread, BELOW_NORMAL):
  pop job (id, path) from queue
  stability check (§5.5.3) if external; skip for our own finished files (size known, finish_recording path)
  r = MediaProbe.probe(path)
  match r:
    Ok(p) → BEGIN; UPDATE recordings SET duration_ms, width, height, fps, video_codec, container, bitrate,
             audio_track_count, probe_version = CURRENT_PROBE, status='ready', size_bytes = fs.size, file_mtime_ms = fs.mtime;
             REPLACE audio_tracks rows; UPDATE recordings_fts file_name ; COMMIT ; enqueue thumbnails (Background); quota check
    Err(Incomplete) → retry in 10 s (max 6), keep status 'pending'
    Err(NoVideo | Ffmpeg) → status='error', error_detail
```
`CURRENT_PROBE = 1`. Bumping it triggers background re-probe of all rows where `probe_version < CURRENT_PROBE`.

---

## 6. Config keys owned by this feature

All keys live under `[library]` in `%APPDATA%\ClipCheck\config.toml`. The Settings doc renders them; hot-reloadable. Changing `recordings_root` must go through `library_set_recordings_root` (§7), not a plain config edit, so the DB and disk stay consistent; a raw edit of the key while the service runs is detected and handled as if the user chose "Leave existing" (with a notification).

| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `library.recordings_root` | string (absolute path) | `%USERPROFILE%\Videos\ClipCheck` | valid writable path, §2.5 | Active root. Set via IPC, not raw edit. |
| `library.per_game_folders` | bool | `true` | — | When false, all files go directly in the root. |
| `library.unknown_game_folder` | string | `Other` | 1–60 chars, sanitized | Folder for recordings without a known game. |
| `library.filename_template` | string | `{game} {yyyy}-{MM}-{dd} {HH}-{mm}-{ss} {kind}` | 1–120 chars; tokens in §4.4 | Live preview in UI via `library_template_preview`. |
| `library.quota_gb` | u32 | `0` | 0 = unlimited; 1–10000 | Counts all registered roots. |
| `library.auto_delete` | enum | `off` | `off`, `oldest` | `oldest` = permanent delete of oldest non-favorite ready recordings. |
| `library.min_free_space_gb` | u32 | `5` | 0–500 | 0 disables the free-space trigger. |
| `library.watch_enabled` | bool | `true` | — | Disables watchers; reconcile still runs at startup and on interval. |
| `library.reconcile_interval_min` | u32 | `360` | 15–1440 | Periodic full reconcile per online root. |
| `library.strip_samples` | u32 | `60` | 10–60 | Hover-preview frame count; grid is 10 columns. |
| `library.thumbnails_background` | bool | `true` | — | When false, thumbnails are generated only on demand (interactive lane). |

Not configurable (fixed by design): favorites are always protected (D5); thumbnail sizes; DB location; reconcile missing-row grace period (30 days); debounce windows.

---

## 7. IPC messages and events

Messages are length-prefixed JSON on `\\.\pipe\clipcheck-ipc` (brief). Types live in `crates/ipc` as variants of the existing `Request`/`Event` enums; this doc defines the library variants.

### 7.1 Requests (UI → service)

```json
{ "type": "library.rename", "id": "01J...", "new_stem": "Boss run" }
{ "type": "library.delete", "ids": ["01J..."], "mode": "recycle_bin" | "permanent", "reason": "user" }
{ "type": "library.set_favorite", "ids": ["01J..."], "value": true }
{ "type": "library.set_tags", "ids": ["01J..."], "add": [3], "remove": [5] }
{ "type": "library.tag_create", "name": "ace", "color": "#ffaa00" }
{ "type": "library.tag_delete", "tag_id": 7 }
{ "type": "library.thumb_request", "id": "01J...", "kind": "poster" | "strip" }
{ "type": "library.set_recordings_root", "path": "D:\\Clips", "behind": "move_existing" | "leave_existing" }
{ "type": "library.detach_root", "root_id": 2 }
{ "type": "library.job_cancel", "job_id": 4 }
{ "type": "library.quota_run_now" }
{ "type": "library.remove_missing", "ids": ["01J..."] }
{ "type": "library.reconcile_now", "root_id": 1 }
```

Responses (Ok/Err envelope, same as other IPC): `RecordingRow` for rename; `DeleteReport`; `u32` counts; `TagRef`; `{ "job_id": 4 }`; `QuotaReport`.

Error codes returned in `Err.code` (stable strings): `name.illegal_chars` (detail: chars), `name.reserved`, `name.empty`, `name.too_long`, `name.trailing_dot`, `rename.collision`, `rename.busy`, `rename.in_use`, `rename.missing_on_disk`, `recycle.unavailable`, `root.invalid`, `root.overlap`, `root.not_writable`, `root.insufficient_space`, `root.offline`, `job.not_found`, `query.stale_cursor`, `io.error`, `db.error`.

### 7.2 Events (service → UI, broadcast)

```json
{ "type": "library.changed", "added": ["01J..."], "updated": ["01J..."], "removed": ["01J..."], "missing": ["01J..."] }
{ "type": "library.job", "job_id": 4, "state": "running", "done_items": 120, "total_items": 3400, "done_bytes": 1234567, "total_bytes": 987654321, "error": null }
{ "type": "library.quota", "state": "over_limit", "used_bytes": 0, "quota_bytes": 0, "free_bytes_on_active_root": 0, "protected_bytes": 0 }
{ "type": "library.thumb", "id": "01J...", "kind": "strip", "state": "ready" | "failed", "error": null }
{ "type": "library.deleted_auto", "count": 2, "reclaimed_bytes": 1181116006 }
{ "type": "library.root_offline", "root_id": 1, "path": "E:\\Clips" }
{ "type": "library.root_online", "root_id": 1 }
```

`library.changed` is coalesced: at most one event per 250 ms, with lists capped at 500 ids each; above that the event carries `"full": true` and the UI re-queries.

### 7.3 Tauri event bridge
The UI's Rust side subscribes to the pipe and re-emits as `library://changed`, `library://job`, `library://quota`, `library://thumb`. Svelte stores listen on those. The gallery reacts to `library.changed` by re-running the current page query if any visible id is affected, otherwise it just updates the total.

### 7.4 Thumbnail protocol
`clip://thumb/<recording_id>/<poster|strip>.jpg` is served by the UI process from `%LOCALAPPDATA%\ClipCheck\thumbs\`, with 404 for missing files. The UI requests generation via `library.thumb_request` on first render when `ThumbRefs` is empty. Path traversal is prevented by accepting only a 26-char ULID and the two fixed names.

---

## 8. Edge cases and failure modes

| # | Case | Behavior |
|---|------|----------|
| E1 | Recordings root is on a removed or unplugged drive. | Root marked offline (path check every 30 s). Its rows stay, shown with an "offline" badge; not watched; excluded from quota "free" calculation; `begin_recording` on an offline active root fails with `root.offline` and the service falls back to the **last online legacy root** only if the user enabled that in settings; otherwise the session is refused and a notification shows. |
| E2 | Path > MAX_PATH (259 chars). | Stem budget enforced at creation (§5.2 step 3). Root paths that cannot fit a 16-char stem are rejected at validation. External files with overlong paths are skipped with a log line (`reconcile.skip.long_path`). |
| E3 | Reserved names (CON, NUL, COM1, …) from game names or templates. | `sanitize_component` prefixes `_`. User rename input rejected with `name.reserved`. |
| E4 | Case-only rename (`Clip.mp4` → `clip.mp4`) on case-insensitive NTFS. | Two-step rename through a temp name (§5.3 step 5). Collision pre-check ignores the same file by id. |
| E5 | Target name exists on disk but not in DB (user-made file), or in DB under another root. | Manual rename: `rename.collision`. Template: suffix " (2)". |
| E6 | OBS, antivirus, or Explorer preview holds the file open. | Rename/delete retry 4× with backoff, then `in_use`. Delete never removes a DB row for a file that still exists. |
| E7 | DB update fails after the disk rename succeeded. | Attempt to rename back. If that also fails, row is set to `pending` at the new path and reconciliation re-links it by `win_file_id` within one reconcile cycle; logged as CRITICAL. |
| E8 | Watcher event loss (buffer overflow, network share, sleep/resume). | `Overflow` → full reconcile. Resume-from-sleep and root reappearance → full reconcile. Periodic full reconcile is the safety net. |
| E9 | Partially written MP4 without `moov` (crash or still recording in an external tool). | `ProbeError::Incomplete` → pending, retry 6×10 s, then `error` with detail "incomplete file". User can "Retry probe" from the context menu. |
| E10 | Unicode normalization: macOS-style NFD names copied to NTFS, or NFD typed in rename. | All stored names are NFC. Collision checks compare NFC + case-insensitive. The original on-disk name is not rewritten (only DB display uses NFC). |
| E11 | Filename template yields an empty stem (e.g. template `{game}` with unknown game). | Fallback stem `ClipCheck <local timestamp>`. |
| E12 | Same physical file appears under two rel_paths (hard link or copy via shell preserves no id). | Hard link: `win_file_id` duplicate → second row is treated as a new recording. Duplicate rows are allowed; the gallery shows both. Documented; no dedupe. |
| E13 | Clock change, DST fall-back, or NTP jump. | Sorting uses `created_at_ms` (UTC) + ULID tiebreak; ULIDs are monotonic within a process. Filename local time may repeat across DST fall-back; collision suffix handles it. |
| E14 | Storage move interrupted (power loss, crash, cancel). | Per-item state machine in `storage_job_items` (§5.6). Resume on startup. Partial destination files are verified by size before adoption and otherwise removed. No row ever points to a missing file without a `missing` status. |
| E15 | Storage move target is inside the source, or the source inside the target. | Rejected at validation (`root.overlap`). |
| E16 | Free space shrinks during a move. | Each copy is verified; `CopyFileExW` failure with ERROR_DISK_FULL → job `paused` with error "destination full"; resume after user frees space or picks another target. Source files are never deleted before the destination is verified. |
| E17 | All recordings are favorites and quota is exceeded. | Quota state `Blocked`; banner shown; no deletion. Recording continues (the session writer will then fail with `DiskFull` if the disk is actually full — session doc handles that). |
| E18 | Recycle Bin disabled or unsupported for the volume (removable, network share). | `recycle.unavailable`; UI offers permanent delete with a confirmation. |
| E19 | Bulk external copy of 10,000 files into a root (events flood). | Debounced batches of ≤ 200 paths; per-batch writer transaction ≤ 200 rows; probes queued and rate-limited to 2 concurrent (I/O). Gallery shows rows as they appear; total count updates via `library.changed`. |
| E20 | Lock contention between UI reads and writer. | WAL: readers never block the writer and vice versa. UI read transactions are kept short (one statement per command, no long cursors). A long-lived reader would block WAL checkpoints, so the UI reader pool closes idle connections after 30 s. |
| E21 | WAL file growth during a long reconcile. | Writer runs `PRAGMA wal_checkpoint(PASSIVE)` after every 1,000 pages written; `TRUNCATE` at startup and when the service goes idle for 5 min. |
| E22 | Thumbnail directory exists without DB row, or row without file. | Startup sweeper removes orphan dirs. Missing thumbnail file for an existing row → row deleted, regeneration enqueued on demand. |
| E23 | Favorite file deleted externally. | Row → `missing`; favorite flag kept until the row is removed (30 days or user action) so re-adding the file restores it. |
| E24 | Quota enforcement races with an in-progress session part. | Writer serializes it; `recording` rows and rows created in the last 60 s are protected (§5.7). |
| E25 | Name exceeds NTFS limits with multi-byte characters. | Length is measured in UTF-16 code units (what NTFS counts), not Unicode scalar values, for path budget. |
| E26 | A user renames a file in Explorer while the rename dialog is open in the app. | The UI request fails with `rename.missing_on_disk` or `rename.collision`; the gallery refreshes the row. |
| E27 | Library DB corrupt (`SQLITE_CORRUPT`). | Service renames the DB to `library.db.corrupt-<ts>`, creates a new DB, runs a full reconcile of all roots (rows rebuilt; favorites/tags lost and reported in a notification). Files are never touched by this recovery. |
| E28 | Permanent delete of a file still referenced by a running clipedit export. | `in_use` via exclusive open attempt; the export holds a read handle with FILE_SHARE_READ only, so the delete fails cleanly. |
| E29 | Recording root is the same folder as the temp buffer or logs. | Rejected at validation (`root.invalid`): the buffer ring and logs are excluded from the watcher by construction. |
| E30 | Root on a network share with unreliable change notifications. | Warn in Settings; watcher still starts; periodic reconcile interval min 15 min is enforced when the root is a remote drive. |

---

## 9. Performance and resource budget

### 9.1 Targets

| Operation | Budget | Measured on (reference) |
|---|---|---|
| Service idle, library loaded, no roots changing | < 2 MB RSS attributable to library; 0 % CPU (no polling; watcher is event-driven) | 10k rows |
| Service RSS total (budget from brief < 15 MB) | library contributes ≤ 3 MB (writer cache 2 MiB, buffers) | — |
| UI read-only connection pool | ≤ 8 MiB page cache + 32 MiB mmap (mapped, not resident-costed) | — |
| `query` first page (60 items, sort Date, no filter), 10k rows | p95 ≤ 5 ms | i7-class, NVMe, warm cache |
| `query` first page, 50k rows, text search + game + tag filter | p95 ≤ 25 ms | — |
| `query` next page (keyset) | p95 ≤ 3 ms at any depth | — |
| `COUNT(*)` with filter, 50k rows | ≤ 15 ms | — |
| `facets` over 50k rows | ≤ 30 ms | — |
| Rename (incl. disk + DB, no in-use) | p95 ≤ 40 ms | NTFS, same folder |
| Delete 100 items to Recycle Bin | ≤ 2 s (dominated by shell) | — |
| Full reconcile, 10,000 files, warm cache | ≤ 3 s; cold cache ≤ 15 s | — |
| Full reconcile, 10,000 files, unchanged | CPU ≤ 1 core-second | — |
| Probe one file (header only) | ≤ 30 ms | 1 h MP4 |
| Poster thumbnail | ≤ 300 ms CPU | 1080p H.264 |
| Strip (60 samples), 1 h recording | ≤ 3 s CPU, ≤ 250 KB JPEG | 1080p H.264, SW decode |
| Thumbnail worker priority | BELOW_NORMAL + background I/O mode | — |
| Writer transactions | ≤ 200 rows per txn in bulk paths; commit p95 ≤ 10 ms | — |
| DB size | ≈ 1.2 KB per recording row + FTS ≈ 0.3 KB; 50k recordings ≈ 80 MB max before vacuum | — |

### 9.2 Sizing assumptions
- A recording row averages ~1.2 KB including indexes; 10k recordings ≈ 12 MB DB.
- Thumbnail files: poster ~25 KB; strip ~150–250 KB. 10k recordings ≈ 2.5 GB of thumbnails worst case. Strips are the main cost; the UI requests them lazily (on hover or first scroll into view). A global thumbs cap is not in v1 (see §12).

### 9.3 Resource discipline
- No thread polls the file system on a timer except the 30 s root-online check and the periodic reconcile (default 6 h).
- Watcher uses `ReadDirectoryChangesW` with a 64 KiB buffer per root (notify default is 16 KiB; raised to reduce overflow).
- FFmpeg decode for thumbnails runs with `threads = 2`, no hardware decode (keeps GPU free for capture/encode).
- DB page cache sized small in the service; UI cache larger because it is read-heavy.

---

## 10. Test plan

### 10.1 Unit tests (pure Rust, no disk)
- `NameTemplate::parse`: all tokens; rejects unknown token `{foo}`, unbalanced `{`, empty, > 120 chars, no required token.
- `sanitize_component`: every illegal char; control chars; trailing dot/space; reserved names (case-insensitive, with extensions `CON.txt`); NFC; truncation by UTF-16 units; emoji and CJK.
- `validate_user_stem`: returns exact `IllegalChars` set; trailing dot; reserved; empty after trim.
- Path budget: stem truncation keeps `{kind}` and timestamp; game name truncated first.
- Cursor codec: round trip; hash mismatch → `Invalid`; tampered base64 → `Invalid`.
- Keyset predicate builder: for each `SortField` × `SortDir`, generated SQL matches a golden file.
- FTS query builder: operators stripped (`AND`, `"`, `*`, `:`, `-`, `NEAR`); empty → None.
- `TagMatch::All` predicate correctness with 0, 1, 3 tags.
- `QuotaPolicy` decision table: over/under × low-free × Off/Oldest × favorites present.
- `ReconcileMode::Full` pairing logic against an in-memory fake directory (table-driven): new, unchanged, changed, moved (same file id), removed, missing older than 30 days.

### 10.2 Integration tests (real SQLite, temp dirs on NTFS)
- Migration v1 on empty DB; PRAGMAs verified (`journal_mode=wal`, `foreign_keys=1`); re-open is idempotent.
- `EXPLAIN QUERY PLAN` for each gallery sort uses the matching `rec_sort_*` index (no `USE TEMP B-TREE FOR ORDER BY` at 10k rows).
- Rename: normal; case-only; collision with disk-only file; collision with DB-only row (case-insensitive); in-use (hold a handle without `FILE_SHARE_DELETE`) → `InUse` after retries; DB failure injection (via a trigger that `RAISE(ABORT)`) → rollback of disk rename verified.
- Delete to Recycle Bin: file appears under `$Recycle.Bin` for the user SID; DB row removed; thumbnails removed. Test uses a dedicated temp volume where bin is enabled; skipped if unavailable.
- Permanent delete: file gone; `deletion_log` row present.
- Storage move (MoveExisting): 100 synthetic recordings across two temp volumes (or two folders on one volume plus a forced cross-volume path via `MOVEFILE_COPY_ALLOWED` simulation); job interrupted at item 50 (kill via test hook) → restart → completes; all rows point to existing files; no duplicate files remain.
- Storage move cancel mid-way: roots both valid; no row points to absent files.
- Storage move (LeaveExisting): old rows still queryable; new recording goes to new root; old root watcher still active.
- Detach root: rows removed, files intact, `deletion_log` has `root_detached`.
- Watcher: create file, wait for stability, appears as `pending` → `ready`; rename outside app → same row id, tags kept; delete outside → `missing`; overflow simulated → full reconcile runs.
- Reconcile: 10k-file synthetic tree; assert time budget; assert idempotence (second run changes nothing).
- Quota: 100 recordings, mixed favorites; quota exceeded → oldest non-favorite deleted until under; all favorites → `Blocked`; in-progress rows never deleted; per-run cap of 50 honored.
- Crash injection: kill process at each numbered point of §5.3, §5.4, §5.6 (use a `fail_point!` macro compiled under the `failpoints` feature). After restart, invariant check: `∀ row (status ∈ {ready,pending}) → file exists at abs_path`; `∀ file in root (ext mp4/mkv) → row exists or was just created`.
- Corruption recovery (E27): garbage-filled `library.db` → renamed, new DB built from disk.

### 10.3 Property tests (proptest)
- Random sequences of {insert, rename, tag, favorite, delete, query page} against a model (`BTreeMap`); after each step, `query` with sort k returns exactly the model's ordering, and pages concatenate to the full list with no duplicates or gaps.
- Keyset pagination under concurrent inserts: pages never skip a row that existed before the query started and was not deleted (dedupe allowed).
- Sanitizer idempotence: `sanitize(sanitize(x)) == sanitize(x)`; output never contains illegal chars; output never a reserved name; UTF-16 length ≤ limit.

### 10.4 Performance tests (benchmarks, run in CI nightly on a self-hosted Windows runner)
- Synthetic library 10k and 50k rows (generated with realistic distributions: 80% clips, names from template, 5% favorites, 30% tagged, 1–5 tags each).
- Each query in §9.1 measured p50/p95 over 200 iterations; regression gate +20 %.
- Reconcile 10k files, cold and warm.
- Strip generation on a 1 h 1080p fixture: CPU time gate.

### 10.5 Manual test checklist (before v1)
- Rename with OBS still recording a different file in the same folder.
- Change storage to a USB stick, unplug mid-move, replug, resume.
- Shift+Delete on a favorite; confirm double prompt.
- Drag 500 files into a root in Explorer while the gallery is open; counts converge.
- Turn on Oldest auto-delete with 2 GB limit; verify toast and deletion_log.
- Non-English Windows locale; accented game name `Pokémon` → folder and FTS search `pokemon` matches (`remove_diacritics 2`).

---

## 11. Implementation task breakdown

Ordered. Each task ~0.5–2 days. Earlier tasks unblock later ones; tasks in the same group may run in parallel.

**Group A: foundations**
1. **Schema and migrations**: `schema/v1.sql` (§4.1), migration runner, PRAGMAs, `library_meta`, corruption detection and recovery (E27). Tests: 10.2 migration.
2. **Writer thread and command channel**: `writer.rs`, `WriterHandle`, bounded channel, transaction helpers, WAL checkpoint policy (E21). Tests: writer serialization, commit timing.
3. **Naming**: `naming/` — template parse/render, `sanitize_component`, `validate_user_stem`, reserved list, UTF-16 budget. Pure; highest test density. Tests: 10.1 naming, 10.3 sanitizer.
4. **Storage roots and path canonicalization**: `roots.rs`, `GetFinalPathNameByHandleW` canonicalization, overlap rules, writability probe, free space (`GetDiskFreeSpaceExW`), `storage_roots` CRUD.
5. **File identity**: `fileid.rs` — `GetFileInformationByHandleEx(FileIdInfo)` → 24-byte blob; fallback to `GetFileInformationByHandle` for FAT/exFAT (returns None when unsupported; relink by path only).

**Group B: recording lifecycle and probing**
6. **Probe service**: `probe.rs` + `MediaProbe` trait; `encode` FFmpeg implementation of `probe`; status machine §5.1; stability check §5.5.3; retry policy. Tests: fixtures (complete, truncated, audio-only, 5-audio-track MKV).
7. **Begin/finish/fail recording API**: `LibraryHandle` impl, `choose_path` (§5.2), game folder assignment (§5.2.1), crash recovery §5.8 steps 1–3.
8. **Query engine**: `query.rs` — predicate builder, keyset SQL, cursor codec, FTS query builder, facets, tags/thumbs batch loaders. Tests: 10.1 keyset/FTS, 10.2 EXPLAIN, 10.3 model test.

**Group C: file operations**
9. **Rename**: `rename.rs` per §5.3, including case-only two-step, retry/backoff, rollback path (E7). Tests: 10.2 rename matrix.
10. **Recycle Bin + delete**: `recycle.rs` (IFileOperation, FOF_ALLOWUNDO, batch), `delete.rs` per §5.4, `deletion_log`. Tests: 10.2 delete.
11. **Watcher**: `watcher.rs` (notify, 64 KiB buffer), debounce, temp-name filter, overflow → reconcile. Tests: watcher event scripts with a fake event source plus one real-FS smoke test.
12. **Reconciler**: `reconcile.rs` §5.5.5 chunked transactions, rename pairing by file id, missing grace period, idempotence. Tests: 10.1 pairing table, 10.2 10k budget.

**Group D: thumbnails, quota, relocation**
13. **Thumbnail service**: `thumbs.rs` poster + strip per §5.10, two lanes, invalidation, orphan sweeper, `clip://thumb` handler (UI side). Tests: fixture frames, sprite layout golden image test, CPU budget test.
14. **Quota and free-space policy**: `quota.rs` §5.7, protected set, per-run cap, `Blocked` state, toasts via events, hourly timer. Tests: 10.2 quota, 10.1 decision table.
15. **Storage jobs**: `storage_job.rs` — `set_active_root`, `step_job` with copy-verify-move-delete, resume, cancel, detach. Tests: 10.2 move/leave/detach, crash injection.

**Group E: surface**
16. **IPC and service wiring**: request/event variants in `crates/ipc`, dispatch in `clipcheckd`, event coalescing (§7.2), error code mapping (§7.1), config hot-reload for `[library]` keys (§6), `library_set_recordings_root` path.
17. **Tauri commands and read-only pool**: `src-tauri` commands (§4.12), read-only connection pool with idle close (E20), `library://` event bridge, `library_reveal` via `explorer.exe /select`, template preview command.
18. **Hardening and perf gates**: failpoint crash suite (10.2), proptest models (10.3), 10k/50k benchmarks and CI gate (10.4), manual checklist (10.5).

---

## 12. Open questions

1. **Thumbnail cache cap.** Strips at ~200 KB each can reach ~2.5 GB for 10k recordings. Should v1 cap thumbnail storage (LRU eviction of strips for old recordings) or accept the cost? Proposed: accept in v1; add LRU cap in v1.1 with `library.thumbs_max_gb`.
2. **Auto-delete permanence.** D4 makes auto-delete permanent. Alternative: route to the Recycle Bin and count bin contents toward the quota. Needs a product decision; the brief does not say.
3. **Fallback to legacy root when the active root is offline (E1).** Recording to a legacy root silently breaks the "new recordings go to the active root" rule. Proposed: off by default, opt-in setting. Confirm with the session doc owner.
4. **Who owns `[library]` config writes.** This doc routes `recordings_root` changes through IPC (`library_set_recordings_root`). The Settings doc must agree that the settings writer calls into the library for that one key. Confirm the Settings doc does not write it directly.
5. **Kind inference for external files (§5.5.2).** External files lack a session/clip origin. Proposed: a `clip-` or `session-` token in the file name if present, else `session` if duration > 5 min, else `clip`. Alternative: always `session` for external files. Decide before the UI shows the kind chip.
6. **Game detection for externally added files.** Matching by folder name is heuristic (user may rename a folder). Proposed: keep it; unmatched files show "Unsorted" and the user can reassign game via a "Set game" action (not in this doc's API yet, trivial to add as `library.set_game`).
7. **Sharing the FFmpeg wrapper with `clipedit`.** Both need probe/decode. Proposed: probe and keyframe decode live in `encode::ffmpeg`; `clipedit` owns remux. Confirm with the encode doc owner that the probe trait in §4.3 is acceptable as their public surface.
8. **Watcher on network or removable roots.** Notification reliability is poor. E30 proposes shorter reconcile intervals. Is the 15 min floor acceptable, or should remote roots be polled only?
9. **FAT32/exFAT removable drives.** There is no stable 128-bit file id on FAT; relink-after-external-rename falls back to path-only and can mis-link if a file is replaced. Proposed: accept and document; do not attempt content hashing in v1.
10. **Deletion of missing rows after 30 days.** Is a silent grace-period delete acceptable, or should the UI prompt once? Proposed: silent, logged, and visible in the "Missing" filter until then.
11. **Per-recording notes.** Not in v1 schema. Adding a `notes` column later is a migration; confirm it is out of scope for v1.
12. **`library.recordings_root` default for multi-user PCs.** `%USERPROFILE%\Videos\ClipCheck` is per-user. Confirm this is the intended behavior and that the DB under `%APPDATA%` is per-user too (it is, per brief).
