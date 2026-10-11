//! Gallery listing: filters, sort keys and keyset pagination (07 §4.7).
//!
//! Pagination is keyset-based: the cursor records the sort value and id of the last row
//! returned, and the next page selects rows strictly after it in the same index order.
//! Every sort ends with `id` as a tiebreak, so the order is total and stable.
//! Cursors are lowercase hex of a small text payload; they carry a hash of the query so a
//! cursor cannot be replayed against a different filter or sort.

use rusqlite::params_from_iter;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::db::{row_from, row_select, RecordingKind, RecordingRow};
use crate::{LibError, Library};

/// Sort order for listings. Each variant maps to a column and direction; `id` breaks ties.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SortKey {
    /// Newest recording first (`created_at_ms` descending).
    #[default]
    Newest,
    /// Oldest recording first (`created_at_ms` ascending).
    Oldest,
    /// File name ascending, case-insensitive.
    NameAsc,
    /// File name descending, case-insensitive.
    NameDesc,
    /// Largest file first.
    SizeDesc,
    /// Smallest file first.
    SizeAsc,
    /// Longest duration first.
    DurationDesc,
}

/// A listing request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListQuery {
    /// Free text, matched against file name, game name and tags (prefix match per token).
    pub text: Option<String>,
    /// Restrict to one game id.
    pub game: Option<String>,
    /// Restrict to one kind.
    pub kind: Option<RecordingKind>,
    /// Only favorites.
    pub favorites_only: bool,
    /// Minimum size in bytes (inclusive).
    pub min_size: Option<u64>,
    /// Maximum size in bytes (inclusive).
    pub max_size: Option<u64>,
    /// Sort order.
    pub sort: SortKey,
    /// Page size. Clamped to 1..=200.
    pub limit: u32,
    /// Cursor from a previous [`Page::next_cursor`] for the same query.
    pub cursor: Option<String>,
}

impl Default for ListQuery {
    fn default() -> Self {
        ListQuery {
            text: None,
            game: None,
            kind: None,
            favorites_only: false,
            min_size: None,
            max_size: None,
            sort: SortKey::default(),
            limit: 50,
            cursor: None,
        }
    }
}

/// One page of a listing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// Rows on this page, in sort order.
    pub items: Vec<RecordingRow>,
    /// Cursor for the next page, or `None` if this is the last page.
    pub next_cursor: Option<String>,
}

/// Maximum page size.
const MAX_LIMIT: u32 = 200;

/// Column expression, direction and key type for a sort.
struct SortSpec {
    column: &'static str,
    desc: bool,
    text_key: bool,
}

fn spec(sort: SortKey) -> SortSpec {
    match sort {
        SortKey::Newest => SortSpec {
            column: "r.created_at_ms",
            desc: true,
            text_key: false,
        },
        SortKey::Oldest => SortSpec {
            column: "r.created_at_ms",
            desc: false,
            text_key: false,
        },
        SortKey::NameAsc => SortSpec {
            column: "r.file_name COLLATE NOCASE",
            desc: false,
            text_key: true,
        },
        SortKey::NameDesc => SortSpec {
            column: "r.file_name COLLATE NOCASE",
            desc: true,
            text_key: true,
        },
        SortKey::SizeDesc => SortSpec {
            column: "r.size_bytes",
            desc: true,
            text_key: false,
        },
        SortKey::SizeAsc => SortSpec {
            column: "r.size_bytes",
            desc: false,
            text_key: false,
        },
        SortKey::DurationDesc => SortSpec {
            column: "r.duration_ms",
            desc: true,
            text_key: false,
        },
    }
}

/// Sort value of the last row of a page, as stored in the cursor.
#[derive(Clone, Debug, PartialEq)]
enum CursorKey {
    Int(i64),
    Text(String),
}

fn key_of(sort: SortKey, row: &RecordingRow) -> CursorKey {
    match sort {
        SortKey::Newest | SortKey::Oldest => CursorKey::Int(row.created_at_ms),
        SortKey::NameAsc | SortKey::NameDesc => CursorKey::Text(row.file_name.clone()),
        SortKey::SizeDesc | SortKey::SizeAsc => CursorKey::Int(row.size_bytes),
        SortKey::DurationDesc => CursorKey::Int(row.duration_ms),
    }
}

fn saturating_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Builds an FTS5 MATCH expression: each whitespace token is reduced to alphanumerics and
/// underscores, quoted, prefix-matched, and the tokens are AND-ed. Returns `None` if nothing
/// is left.
fn fts_expression(text: &str) -> Option<String> {
    let tokens: Vec<String> = text
        .split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/// FNV-1a over the canonical form of the query (everything except limit and cursor).
fn query_hash(q: &ListQuery) -> u64 {
    let canonical = format!(
        "{:?}|{:?}|{:?}|{}|{:?}|{:?}|{:?}",
        q.text, q.game, q.kind, q.favorites_only, q.min_size, q.max_size, q.sort
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

fn invalid_cursor() -> LibError {
    LibError::Invalid("malformed cursor".into())
}

/// Payload: `v1\n<qhash:016x>\n<i|t>\n<id>\n<key>`. `key` is last so it may hold anything.
fn encode_cursor(qhash: u64, key: &CursorKey, id: &str) -> String {
    let (tag, val) = match key {
        CursorKey::Int(i) => ('i', i.to_string()),
        CursorKey::Text(s) => ('t', s.clone()),
    };
    let raw = format!("v1\n{qhash:016x}\n{tag}\n{id}\n{val}");
    hex_encode(raw.as_bytes())
}

fn decode_cursor(s: &str) -> Result<(u64, CursorKey, String), LibError> {
    let bytes = hex_decode(s).ok_or_else(invalid_cursor)?;
    let raw = String::from_utf8(bytes).map_err(|_| invalid_cursor())?;
    let mut parts = raw.splitn(5, '\n');
    if parts.next() != Some("v1") {
        return Err(invalid_cursor());
    }
    let qhash = u64::from_str_radix(parts.next().ok_or_else(invalid_cursor)?, 16)
        .map_err(|_| invalid_cursor())?;
    let tag = parts.next().ok_or_else(invalid_cursor)?;
    let id = parts.next().ok_or_else(invalid_cursor)?.to_string();
    let val = parts.next().ok_or_else(invalid_cursor)?;
    let key = match tag {
        "i" => CursorKey::Int(val.parse().map_err(|_| invalid_cursor())?),
        "t" => CursorKey::Text(val.to_string()),
        _ => return Err(invalid_cursor()),
    };
    Ok((qhash, key, id))
}

/// Totals over a set of recordings, as returned by [`Library::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryStats {
    /// Number of matching recordings (any kind).
    pub recordings: u64,
    /// Number of matching clips.
    pub clips: u64,
    /// Number of matching session parts.
    pub sessions: u64,
    /// Sum of `size_bytes` over the matching recordings.
    pub total_size_bytes: i64,
    /// Sum of `duration_ms` over the matching recordings.
    pub total_duration_ms: i64,
}

/// Appends the status rule and the filters of `q` (game, kind, favorites, sizes, text) to
/// `sql`, binding their values in `params`. Shared by [`Library::list`] and [`Library::stats`],
/// so both see the same row set. The SQL must already select from `recordings r`.
fn push_filters(q: &ListQuery, sql: &mut String, params: &mut Vec<Value>) {
    sql.push_str(" WHERE r.status IN ('ready','pending')");
    if let Some(game) = &q.game {
        sql.push_str(" AND r.game_id = ?");
        params.push(Value::Text(game.clone()));
    }
    if let Some(kind) = q.kind {
        sql.push_str(" AND r.kind = ?");
        params.push(Value::Text(kind.as_str().to_string()));
    }
    if q.favorites_only {
        sql.push_str(" AND r.is_favorite = 1");
    }
    if let Some(min) = q.min_size {
        sql.push_str(" AND r.size_bytes >= ?");
        params.push(Value::Integer(saturating_i64(min)));
    }
    if let Some(max) = q.max_size {
        sql.push_str(" AND r.size_bytes <= ?");
        params.push(Value::Integer(saturating_i64(max)));
    }
    if let Some(expr) = q.text.as_deref().and_then(fts_expression) {
        sql.push_str(
            " AND r.id IN (SELECT rec_id FROM recordings_fts WHERE recordings_fts MATCH ?)",
        );
        params.push(Value::Text(expr));
    }
}

impl Library {
    /// Lists recordings matching `q`, one page at a time.
    ///
    /// Only rows with status `ready` or `pending` are listed. Passing a cursor from a previous
    /// page of the same query continues the listing; a cursor from a different query fails
    /// with [`LibError::Invalid`].
    pub fn list(&self, q: &ListQuery) -> Result<Page, LibError> {
        let limit = q.limit.clamp(1, MAX_LIMIT) as usize;
        let qhash = query_hash(q);
        let sp = spec(q.sort);
        let dir = if sp.desc { "DESC" } else { "ASC" };
        let op = if sp.desc { "<" } else { ">" };

        let mut sql = String::from(row_select());
        let mut params: Vec<Value> = Vec::new();
        push_filters(q, &mut sql, &mut params);

        if let Some(cursor) = &q.cursor {
            let (cq, key, id) = decode_cursor(cursor)?;
            if cq != qhash {
                return Err(LibError::Invalid("cursor does not match this query".into()));
            }
            let key_value = match (key, sp.text_key) {
                (CursorKey::Int(i), false) => Value::Integer(i),
                (CursorKey::Text(s), true) => Value::Text(s),
                _ => return Err(LibError::Invalid("cursor does not match this sort".into())),
            };
            let col = sp.column;
            sql.push_str(&format!(
                " AND ({col} {op} ? OR ({col} = ? AND r.id {op} ?))"
            ));
            params.push(key_value.clone());
            params.push(key_value);
            params.push(Value::Text(id));
        }

        sql.push_str(&format!(
            " ORDER BY {} {dir}, r.id {dir} LIMIT ?",
            sp.column
        ));
        params.push(Value::Integer(i64::try_from(limit + 1).unwrap_or(i64::MAX)));

        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut items: Vec<RecordingRow> = stmt
            .query_map(params_from_iter(params.iter()), row_from)?
            .collect::<rusqlite::Result<_>>()?;

        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items
                .last()
                .map(|last| encode_cursor(qhash, &key_of(q.sort, last), &last.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    /// Totals over the recordings matching `q`, computed by one aggregate SQL query.
    ///
    /// Uses the same row set as [`Library::list`]: only rows with status `ready` or `pending`,
    /// joined to their storage root, and the same filters (`text`, `game`, `kind`,
    /// `favorites_only`, `min_size`, `max_size`). The fields `sort`, `limit` and `cursor` are
    /// ignored. `LibraryStats::default()` is returned for an empty set.
    pub fn stats(&self, q: &ListQuery) -> Result<LibraryStats, LibError> {
        let mut sql = String::from(
            "SELECT COUNT(*), \
             COALESCE(SUM(CASE WHEN r.kind = 'clip' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN r.kind = 'session' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(r.size_bytes), 0), \
             COALESCE(SUM(r.duration_ms), 0) \
             FROM recordings r JOIN storage_roots sr ON sr.id = r.root_id",
        );
        let mut params: Vec<Value> = Vec::new();
        push_filters(q, &mut sql, &mut params);

        let mut stmt = self.conn.prepare_cached(&sql)?;
        stmt.query_row(params_from_iter(params.iter()), |row| {
            let count = |i: usize| -> rusqlite::Result<u64> {
                let v: i64 = row.get(i)?;
                Ok(u64::try_from(v).unwrap_or(0))
            };
            Ok(LibraryStats {
                recordings: count(0)?,
                clips: count(1)?,
                sessions: count(2)?,
                total_size_bytes: row.get(3)?,
                total_duration_ms: row.get(4)?,
            })
        })
        .map_err(LibError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{NewRecording, RecStatus};

    fn rec(i: usize) -> NewRecording {
        let file_name = format!("Clip{:05}.mp4", (i * 7919) % 10_000);
        NewRecording {
            id: format!("{:026}", i),
            kind: if i.is_multiple_of(3) {
                RecordingKind::Session
            } else {
                RecordingKind::Clip
            },
            session_id: None,
            part_index: None,
            game_id: None,
            root_id: 1,
            rel_path: format!("u\\{file_name}"),
            file_name,
            ext: "mp4".into(),
            size_bytes: ((i * 31) % 1000) as i64,
            file_mtime_ms: 1,
            created_at_ms: (i / 10) as i64,
            added_at_ms: 1,
            duration_ms: ((i * 17) % 500) as i64,
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

    fn library_with(n: usize) -> Library {
        let lib = Library::open_in_memory().expect("open");
        let root = lib.ensure_storage_root("C:\\Clips", 1).expect("root");
        for i in 0..n {
            let mut r = rec(i);
            r.root_id = root;
            lib.insert_recording(&r).expect("insert");
        }
        lib
    }

    fn walk(lib: &Library, mut q: ListQuery) -> Vec<RecordingRow> {
        let mut all = Vec::new();
        loop {
            let page = lib.list(&q).expect("page");
            all.extend(page.items);
            match page.next_cursor {
                Some(c) => q.cursor = Some(c),
                None => break,
            }
        }
        all
    }

    #[test]
    fn cursor_roundtrip_and_rejects_garbage() {
        let c = encode_cursor(42, &CursorKey::Text("a\nb".into()), "ID");
        let (h, k, id) = decode_cursor(&c).expect("decode");
        assert_eq!(
            (h, k, id.as_str()),
            (42, CursorKey::Text("a\nb".into()), "ID")
        );
        assert!(decode_cursor("zz").is_err());
        assert!(decode_cursor("6162").is_err());
    }

    #[test]
    fn fts_expression_strips_operators() {
        assert_eq!(
            fts_expression("  ab\"c* (d) "),
            Some("\"abc\"* \"d\"*".to_string())
        );
        assert_eq!(fts_expression("\"* ()"), None);
    }

    #[test]
    fn keyset_walks_10k_rows_in_every_sort_without_gaps_or_dupes() {
        let n = 10_000;
        let lib = library_with(n);
        for sort in [
            SortKey::Newest,
            SortKey::Oldest,
            SortKey::NameAsc,
            SortKey::NameDesc,
            SortKey::SizeDesc,
            SortKey::SizeAsc,
            SortKey::DurationDesc,
        ] {
            let q = ListQuery {
                sort,
                limit: 137,
                ..ListQuery::default()
            };
            let rows = walk(&lib, q);
            assert_eq!(rows.len(), n, "sort {sort:?}");
            let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), n, "duplicates in {sort:?}");
        }
    }

    #[test]
    fn name_sort_is_case_insensitive_and_ordered() {
        let lib = library_with(300);
        let rows = walk(
            &lib,
            ListQuery {
                sort: SortKey::NameAsc,
                limit: 50,
                ..ListQuery::default()
            },
        );
        for pair in rows.windows(2) {
            let a = pair[0].file_name.to_ascii_lowercase();
            let b = pair[1].file_name.to_ascii_lowercase();
            assert!(a < b || (a == b && pair[0].id < pair[1].id));
        }
    }

    #[test]
    fn cursor_from_other_query_is_rejected() {
        let lib = library_with(20);
        let page = lib
            .list(&ListQuery {
                limit: 5,
                ..ListQuery::default()
            })
            .expect("p");
        let cursor = page.next_cursor.expect("cursor");
        let other = ListQuery {
            sort: SortKey::Oldest,
            cursor: Some(cursor),
            limit: 5,
            ..ListQuery::default()
        };
        assert!(matches!(lib.list(&other), Err(LibError::Invalid(_))));
    }

    #[test]
    fn filters_apply() {
        let lib = library_with(60);
        let sessions = walk(
            &lib,
            ListQuery {
                kind: Some(RecordingKind::Session),
                limit: 7,
                ..ListQuery::default()
            },
        );
        assert_eq!(sessions.len(), 20);
        assert!(sessions.iter().all(|r| r.kind == RecordingKind::Session));
        let sized = walk(
            &lib,
            ListQuery {
                min_size: Some(100),
                max_size: Some(199),
                ..ListQuery::default()
            },
        );
        assert!(sized.iter().all(|r| (100..=199).contains(&r.size_bytes)));
    }

    #[test]
    fn repeated_list_calls_do_not_leak_filters() {
        let lib = library_with(60);
        let all = ListQuery {
            limit: 200,
            ..ListQuery::default()
        };
        let sessions = ListQuery {
            kind: Some(RecordingKind::Session),
            limit: 200,
            ..ListQuery::default()
        };
        let sized = ListQuery {
            min_size: Some(100),
            max_size: Some(199),
            limit: 200,
            ..ListQuery::default()
        };
        let first_sessions = lib.list(&sessions).expect("sessions");
        assert_eq!(first_sessions.items.len(), 20);
        assert_eq!(lib.list(&all).expect("all").items.len(), 60);
        let sized_page = lib.list(&sized).expect("sized");
        assert!(sized_page
            .items
            .iter()
            .all(|r| (100..=199).contains(&r.size_bytes)));
        assert_eq!(
            lib.list(&sessions).expect("sessions again").items,
            first_sessions.items
        );
        assert_eq!(lib.list(&all).expect("all again").items.len(), 60);
    }
}
