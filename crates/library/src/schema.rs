//! Schema DDL, connection PRAGMAs and the migration runner (07 §4.1).

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::LibError;

/// Schema version this build writes and expects. Stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 2;

/// The v1 library schema (07 §4.1.1), with the DESIGN.md §8 D13 session columns added to
/// `recordings`. Applied inside one transaction by [`migrate`].
pub const V1_SQL: &str = r##"
CREATE TABLE library_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE storage_roots (
    id           INTEGER PRIMARY KEY,
    path         TEXT    NOT NULL UNIQUE COLLATE NOCASE
                         CHECK (length(path) BETWEEN 3 AND 240),
    state        TEXT    NOT NULL DEFAULT 'active'
                         CHECK (state IN ('active','legacy')),
    added_at_ms  INTEGER NOT NULL,
    last_reconcile_ms INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX storage_roots_one_active ON storage_roots(state) WHERE state = 'active';

CREATE TABLE games (
    id            TEXT    PRIMARY KEY,
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
    id              TEXT    PRIMARY KEY CHECK (length(id) = 26),
    kind            TEXT    NOT NULL CHECK (kind IN ('clip','session')),
    session_id      TEXT    CHECK (session_id IS NULL OR length(session_id) = 26),
    part_index      INTEGER CHECK (part_index IS NULL OR part_index >= 1),
    game_id         TEXT    REFERENCES games(id) ON DELETE SET NULL,
    root_id         INTEGER NOT NULL REFERENCES storage_roots(id) ON DELETE RESTRICT,
    rel_path        TEXT    NOT NULL CHECK (length(rel_path) BETWEEN 1 AND 400),
    file_name       TEXT    NOT NULL,
    ext             TEXT    NOT NULL CHECK (ext IN ('mp4','mkv')),
    size_bytes      INTEGER NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    file_mtime_ms   INTEGER NOT NULL,
    created_at_ms   INTEGER NOT NULL,
    added_at_ms     INTEGER NOT NULL,
    win_file_id     BLOB    CHECK (win_file_id IS NULL OR length(win_file_id) = 24),
    duration_ms     INTEGER NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    width           INTEGER CHECK (width IS NULL OR width > 0),
    height          INTEGER CHECK (height IS NULL OR height > 0),
    fps_num         INTEGER CHECK (fps_num IS NULL OR fps_num > 0),
    fps_den         INTEGER CHECK (fps_den IS NULL OR fps_den > 0),
    video_codec     TEXT,
    container       TEXT,
    bitrate_kbps    INTEGER,
    audio_track_count INTEGER NOT NULL DEFAULT 0 CHECK (audio_track_count >= 0),
    probe_version   INTEGER NOT NULL DEFAULT 0,
    status          TEXT    NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('recording','pending','ready','missing','error')),
    error_detail    TEXT,
    is_favorite     INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0,1)),
    end_reason      TEXT,
    recovered       INTEGER NOT NULL DEFAULT 0 CHECK (recovered IN (0,1)),
    degraded        INTEGER NOT NULL DEFAULT 0 CHECK (degraded IN (0,1)),
    dropped_packets INTEGER NOT NULL DEFAULT 0 CHECK (dropped_packets >= 0),
    gap_ms          INTEGER NOT NULL DEFAULT 0 CHECK (gap_ms >= 0),
    needs_faststart INTEGER NOT NULL DEFAULT 0 CHECK (needs_faststart IN (0,1)),
    UNIQUE (root_id, rel_path)
);

CREATE INDEX rec_sort_created  ON recordings(created_at_ms, id);
CREATE INDEX rec_sort_name     ON recordings(file_name COLLATE NOCASE, id);
CREATE INDEX rec_sort_size     ON recordings(size_bytes, id);
CREATE INDEX rec_sort_duration ON recordings(duration_ms, id);
CREATE INDEX rec_sort_kind     ON recordings(kind, created_at_ms, id);
CREATE INDEX rec_game_created  ON recordings(game_id, created_at_ms, id);
CREATE INDEX rec_fav_created   ON recordings(created_at_ms, id) WHERE is_favorite = 1;
CREATE INDEX rec_session       ON recordings(session_id, part_index) WHERE session_id IS NOT NULL;
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
    track_index  INTEGER NOT NULL CHECK (track_index >= 0),
    codec        TEXT    NOT NULL,
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
    rel_file        TEXT    NOT NULL,
    width           INTEGER NOT NULL CHECK (width > 0),
    height          INTEGER NOT NULL CHECK (height > 0),
    cols            INTEGER NOT NULL DEFAULT 1 CHECK (cols >= 1),
    rows            INTEGER NOT NULL DEFAULT 1 CHECK (rows >= 1),
    samples         INTEGER NOT NULL DEFAULT 1 CHECK (samples >= 1),
    interval_ms     INTEGER NOT NULL DEFAULT 0 CHECK (interval_ms >= 0),
    byte_size       INTEGER NOT NULL CHECK (byte_size > 0),
    source_size     INTEGER NOT NULL,
    source_mtime_ms INTEGER NOT NULL,
    generated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (recording_id, kind)
) WITHOUT ROWID;

CREATE VIRTUAL TABLE recordings_fts USING fts5(
    rec_id UNINDEXED,
    file_name,
    game_name,
    tags,
    tokenize = "unicode61 remove_diacritics 2"
);

CREATE TABLE storage_jobs (
    id              INTEGER PRIMARY KEY,
    kind            TEXT    NOT NULL CHECK (kind IN ('move_existing','detach_root')),
    from_root_id    INTEGER NOT NULL REFERENCES storage_roots(id) ON DELETE RESTRICT,
    to_root_id      INTEGER REFERENCES storage_roots(id) ON DELETE RESTRICT,
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
    dest_rel_path TEXT,
    error        TEXT,
    PRIMARY KEY (job_id, recording_id)
) WITHOUT ROWID;
CREATE INDEX storage_job_items_todo ON storage_job_items(job_id, state);

CREATE TABLE deletion_log (
    id             INTEGER PRIMARY KEY,
    at_ms          INTEGER NOT NULL,
    recording_id   TEXT    NOT NULL,
    display_name   TEXT    NOT NULL,
    size_bytes     INTEGER NOT NULL,
    reason         TEXT    NOT NULL CHECK (reason IN ('quota','free_space','user_permanent','user_recycle','root_detached'))
);
CREATE INDEX deletion_log_at ON deletion_log(at_ms);
"##;

/// Migration v2 (external library folders, see DESIGN.md D24): storage roots gain
/// read-only, label, availability and scan bookkeeping; recordings gain `source`
/// (`recorded` | `import` | `external`) and `playable`.
///
/// `recordings.status` already allows `missing` (v1), so no table rebuild is needed.
/// `recordings.file_mtime_ms` already exists from v1. Applied inside [`migrate`]'s
/// transaction, only when `user_version` is 1.
pub const V2_SQL: &str = r##"
ALTER TABLE storage_roots ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0
    CHECK (read_only IN (0,1));
ALTER TABLE storage_roots ADD COLUMN label TEXT NOT NULL DEFAULT '';
ALTER TABLE storage_roots ADD COLUMN availability TEXT NOT NULL DEFAULT 'online'
    CHECK (availability IN ('online','offline'));
ALTER TABLE storage_roots ADD COLUMN last_scan_unix_ms INTEGER;
ALTER TABLE recordings ADD COLUMN source TEXT NOT NULL DEFAULT 'recorded'
    CHECK (source IN ('recorded','import','external'));
ALTER TABLE recordings ADD COLUMN playable INTEGER NOT NULL DEFAULT 1
    CHECK (playable IN (0,1));
CREATE INDEX rec_source ON recordings(root_id, source);
"##;

/// Applies the PRAGMAs for the single writer connection (07 §4.1.0).
pub fn apply_writer_pragmas(conn: &Connection) -> Result<(), LibError> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -2048;
         PRAGMA mmap_size = 0;",
    )?;
    Ok(())
}

/// Applies the PRAGMAs for a read-only UI connection (07 §4.1.0).
pub fn apply_reader_pragmas(conn: &Connection) -> Result<(), LibError> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -8192;
         PRAGMA mmap_size = 33554432;",
    )?;
    Ok(())
}

/// Returns `PRAGMA user_version` (0 for a fresh database).
pub fn user_version(conn: &Connection) -> Result<i64, LibError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Brings the database up to [`SCHEMA_VERSION`] inside transactions.
///
/// Fails with [`LibError::Invalid`] if the database was written by a newer schema.
pub fn migrate(conn: &Connection) -> Result<(), LibError> {
    let current = user_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(LibError::Invalid(format!(
            "database schema v{current} is newer than supported v{SCHEMA_VERSION}"
        )));
    }
    if current == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    if current < 1 {
        apply_v1(&tx)?;
    }
    if current < 2 {
        apply_v2(&tx)?;
    }
    // Written once, after every step, so the stored version can only be SCHEMA_VERSION.
    // PRAGMA user_version cannot take a bound parameter; SCHEMA_VERSION is a constant integer.
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    tx.commit()?;
    Ok(())
}

fn apply_v1(tx: &Connection) -> Result<(), LibError> {
    tx.execute_batch(V1_SQL)?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO library_meta(key, value) VALUES ('schema_created_at_ms', ?1)",
        [now.to_string()],
    )?;
    Ok(())
}

fn apply_v2(tx: &Connection) -> Result<(), LibError> {
    tx.execute_batch(V2_SQL)?;
    Ok(())
}

/// Current wall-clock time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_sets_version_and_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open");
        apply_writer_pragmas(&conn).expect("pragmas");
        migrate(&conn).expect("first migrate");
        assert_eq!(user_version(&conn).expect("uv"), SCHEMA_VERSION);
        migrate(&conn).expect("second migrate");
        assert_eq!(user_version(&conn).expect("uv"), SCHEMA_VERSION);
    }

    #[test]
    fn migrate_from_v1_keeps_rows_and_adds_v2_columns() {
        let conn = Connection::open_in_memory().expect("open");
        apply_writer_pragmas(&conn).expect("pragmas");
        conn.execute_batch(V1_SQL).expect("v1");
        conn.execute_batch("PRAGMA user_version = 1;").expect("uv1");
        conn.execute(
            "INSERT INTO storage_roots(path, state, added_at_ms) VALUES ('C:\\Old', 'active', 5)",
            [],
        )
        .expect("root");
        migrate(&conn).expect("migrate");
        assert_eq!(user_version(&conn).expect("uv"), 2);
        let (path, avail, ro): (String, String, i64) = conn
            .query_row(
                "SELECT path, availability, read_only FROM storage_roots",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(
            (path.as_str(), avail.as_str(), ro),
            ("C:\\Old", "online", 0)
        );
        let playable: i64 = conn
            .query_row(
                "SELECT COALESCE((SELECT dflt_value FROM pragma_table_info('recordings') WHERE name='playable'), 'x') = '1'",
                [],
                |r| r.get(0),
            )
            .expect("pragma");
        assert_eq!(playable, 1);
        migrate(&conn).expect("idempotent");
        assert_eq!(user_version(&conn).expect("uv"), 2);
    }

    #[test]
    fn migrate_rejects_newer_schema() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch("PRAGMA user_version = 99;")
            .expect("set");
        assert!(matches!(migrate(&conn), Err(LibError::Invalid(_))));
    }
}
