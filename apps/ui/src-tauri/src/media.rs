//! `clip://` media protocol: HTTP range serving of recordings and audio sidecars.
//! Contract: docs/impl/player-mvp.md.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use clipcheck_library::Library;

/// An open-ended or oversized range is capped to this many bytes.
pub const MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// Where the audio sidecar for track `n` of a recording lives.
pub fn sidecar_path(cache_dir: &Path, id: &str, size: u64, mtime_ms: i64, n: u32) -> PathBuf {
    cache_dir
        .join("audio")
        .join(id)
        .join(format!("{size}-{mtime_ms}"))
        .join(format!("track{n}.m4a"))
}

/// Parsed `Range` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSpec {
    Full,
    Partial { start: u64, end_inclusive: u64 },
    Unsatisfiable,
}

/// A response for the protocol handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Media lookup failures.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("bad recording id")]
    BadId,
    #[error("not found")]
    NotFound,
    #[error("database: {0}")]
    Db(String),
    #[error("io: {0}")]
    Io(String),
}

/// Parses an unsigned decimal made only of ASCII digits. Rejects signs, spaces and overflow.
fn parse_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok()
}

/// Parses a `Range` header against a file of `len` bytes.
pub fn parse_range(header: Option<&str>, len: u64) -> RangeSpec {
    parse_range_capped(header, len, MAX_CHUNK)
}

/// Like [`parse_range`] but with no cap on the range length (used by the streaming server).
pub fn parse_range_uncapped(header: Option<&str>, len: u64) -> RangeSpec {
    parse_range_capped(header, len, u64::MAX)
}

/// Like [`parse_range`] with a caller-chosen cap on the range length.
pub fn parse_range_capped(header: Option<&str>, len: u64, cap: u64) -> RangeSpec {
    let Some(header) = header else {
        return RangeSpec::Full;
    };
    let header = header.trim();
    let Some(prefix) = header.get(..6) else {
        return RangeSpec::Full;
    };
    if !prefix.eq_ignore_ascii_case("bytes=") {
        return RangeSpec::Full;
    }
    let Some(spec) = header.get(6..) else {
        return RangeSpec::Full;
    };
    let spec = spec.trim();
    // Multi-range requests are not supported; serve the whole file instead.
    if spec.contains(',') {
        return RangeSpec::Full;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return RangeSpec::Full;
    };
    let (a, b) = (a.trim(), b.trim());

    // Resolve (first, last) inclusive bounds; malformed syntax falls back to Full.
    let (first, last) = if a.is_empty() {
        // Suffix form "-n": the last n bytes.
        let Some(n) = parse_digits(b) else {
            return RangeSpec::Full;
        };
        if len == 0 || n == 0 {
            return RangeSpec::Unsatisfiable;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let Some(start) = parse_digits(a) else {
            return RangeSpec::Full;
        };
        let end = if b.is_empty() {
            None
        } else {
            let Some(end) = parse_digits(b) else {
                return RangeSpec::Full;
            };
            Some(end)
        };
        if len == 0 || start >= len {
            return RangeSpec::Unsatisfiable;
        }
        match end {
            None => (start, len - 1),
            // last-byte-pos < first-byte-pos makes the spec invalid: ignore the header.
            Some(end) if end < start => return RangeSpec::Full,
            Some(end) => (start, end.min(len - 1)),
        }
    };

    let end_inclusive = last.min(first.saturating_add(cap - 1));
    RangeSpec::Partial {
        start: first,
        end_inclusive,
    }
}

/// Content type for a file extension (no dot, any case).
pub fn content_type_for_ext(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "m4a" => "audio/mp4",
        _ => "application/octet-stream",
    }
}

/// True when `id` is 1..=40 ASCII alphanumerics.
pub fn is_valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Absolute path of a recording from the library database.
pub fn resolve_recording_path(db: &Path, id: &str) -> Result<PathBuf, MediaError> {
    if !is_valid_id(id) {
        return Err(MediaError::BadId);
    }
    if !db.is_file() {
        return Err(MediaError::NotFound);
    }
    let lib = Library::open_readonly(db).map_err(|e| MediaError::Db(e.to_string()))?;
    let row = lib
        .get_recording(id)
        .map_err(|e| MediaError::Db(e.to_string()))?
        .ok_or(MediaError::NotFound)?;
    let path = Path::new(&row.root_path).join(&row.rel_path);
    if !path.is_file() {
        return Err(MediaError::NotFound);
    }
    Ok(path)
}

fn header(name: &str, value: String) -> (String, String) {
    (name.to_string(), value)
}

fn text_response(status: u16, msg: &str) -> MediaResponse {
    MediaResponse {
        status,
        headers: vec![
            header("Content-Type", "text/plain".to_string()),
            header("Content-Length", msg.len().to_string()),
            header("Cache-Control", "no-store".to_string()),
        ],
        body: msg.as_bytes().to_vec(),
    }
}

fn error_response(err: &MediaError) -> MediaResponse {
    match err {
        MediaError::BadId => text_response(400, "bad id"),
        MediaError::NotFound => text_response(404, "not found"),
        MediaError::Db(_) => text_response(500, "database error"),
        MediaError::Io(_) => text_response(500, "io error"),
    }
}

fn unsatisfiable_response(len: u64) -> MediaResponse {
    MediaResponse {
        status: 416,
        headers: vec![
            header("Content-Type", "text/plain".to_string()),
            header("Content-Length", "0".to_string()),
            header("Content-Range", format!("bytes */{len}")),
            header("Accept-Ranges", "bytes".to_string()),
            header("Cache-Control", "no-store".to_string()),
        ],
        body: Vec::new(),
    }
}

/// Serves one file (or sidecar) honoring the `Range` header. Reads only the requested bytes.
fn serve_file(path: &Path, content_type: &str, range: Option<&str>) -> MediaResponse {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return text_response(404, "not found");
        }
        Err(e) => return error_response(&MediaError::Io(e.to_string())),
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => return error_response(&MediaError::Io(e.to_string())),
    };

    let spec = parse_range(range, len);
    let (status, bounds): (u16, Option<(u64, u64)>) = match spec {
        RangeSpec::Unsatisfiable => return unsatisfiable_response(len),
        RangeSpec::Full if len <= MAX_CHUNK => (200, (len > 0).then(|| (0, len - 1))),
        RangeSpec::Full => (206, Some((0, MAX_CHUNK - 1))),
        RangeSpec::Partial {
            start,
            end_inclusive,
        } => {
            if start > end_inclusive || end_inclusive >= len {
                return unsatisfiable_response(len);
            }
            (206, Some((start, end_inclusive)))
        }
    };

    let body = match bounds {
        None => Vec::new(),
        Some((start, end)) => {
            let span = end - start + 1;
            if let Err(e) = file.seek(SeekFrom::Start(start)) {
                return error_response(&MediaError::Io(e.to_string()));
            }
            let mut buf = vec![0u8; span as usize];
            if let Err(e) = file.read_exact(&mut buf) {
                return error_response(&MediaError::Io(e.to_string()));
            }
            buf
        }
    };

    let mut headers = vec![
        header("Content-Type", content_type.to_string()),
        header("Content-Length", body.len().to_string()),
        header("Accept-Ranges", "bytes".to_string()),
        header("Cache-Control", "no-store".to_string()),
    ];
    if status == 206 {
        if let Some((start, end)) = bounds {
            headers.push(header(
                "Content-Range",
                format!("bytes {start}-{end}/{len}"),
            ));
        }
    }

    MediaResponse {
        status,
        headers,
        body,
    }
}

/// Where the poster JPEG for a recording lives. The daemon keeps `library.db` in its data
/// directory and writes posters to `<data_dir>/thumbs/<id>/poster.jpg`, so the data
/// directory is the parent of the db path.
pub fn poster_path(db: &Path, id: &str) -> PathBuf {
    db.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("thumbs")
        .join(id)
        .join("poster.jpg")
}

/// Replaces (or adds) a header by case-insensitive name.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    headers.push(header(name, value));
}

/// Adds the CORS headers every media response carries. The app page (`http://tauri.localhost`)
/// and the media origin (`http://clip.localhost`) differ, and Web Audio needs CORS to read the
/// sidecars through `createMediaElementSource`.
fn with_cors(mut resp: MediaResponse) -> MediaResponse {
    set_header(
        &mut resp.headers,
        "Access-Control-Allow-Origin",
        "*".to_string(),
    );
    set_header(
        &mut resp.headers,
        "Access-Control-Expose-Headers",
        "Content-Range, Accept-Ranges, Content-Length".to_string(),
    );
    resp
}

/// Answer to a CORS preflight (`OPTIONS`): 204 with no body.
fn preflight_response() -> MediaResponse {
    with_cors(MediaResponse {
        status: 204,
        headers: vec![
            header(
                "Access-Control-Allow-Methods",
                "GET, HEAD, OPTIONS".to_string(),
            ),
            header("Access-Control-Allow-Headers", "Range".to_string()),
            header("Cache-Control", "no-store".to_string()),
        ],
        body: Vec::new(),
    })
}

/// Entry point for the protocol handler: answers `OPTIONS` preflights, otherwise serves as
/// `serve` does. `method` is the HTTP method of the request.
pub fn serve_with_method(
    db: &Path,
    cache_dir: &Path,
    method: &str,
    url_path: &str,
    range: Option<&str>,
) -> MediaResponse {
    if method.eq_ignore_ascii_case("OPTIONS") {
        return preflight_response();
    }
    serve(db, cache_dir, url_path, range)
}

/// Serves `/rec/<id>`, `/aud/<id>/<n>` and `/thumb/<id>`. Every response carries CORS headers.
pub fn serve(db: &Path, cache_dir: &Path, url_path: &str, range: Option<&str>) -> MediaResponse {
    with_cors(serve_route(db, cache_dir, url_path, range))
}

fn serve_route(db: &Path, cache_dir: &Path, url_path: &str, range: Option<&str>) -> MediaResponse {
    let path = url_path.split(['?', '#']).next().unwrap_or("");
    let Some(rest) = path.strip_prefix('/') else {
        return text_response(400, "bad path");
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        ["rec", id] => {
            let file = match resolve_recording_path(db, id) {
                Ok(p) => p,
                Err(e) => return error_response(&e),
            };
            let ct = file
                .extension()
                .and_then(|e| e.to_str())
                .map(content_type_for_ext)
                .unwrap_or("application/octet-stream");
            serve_file(&file, ct, range)
        }
        ["aud", id, n] => {
            let Some(n) = parse_digits(n).and_then(|v| u32::try_from(v).ok()) else {
                return text_response(400, "bad track");
            };
            let file = match resolve_recording_path(db, id) {
                Ok(p) => p,
                Err(e) => return error_response(&e),
            };
            let meta = match std::fs::metadata(&file) {
                Ok(m) => m,
                Err(e) => return error_response(&MediaError::Io(e.to_string())),
            };
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or(0);
            let sidecar = sidecar_path(cache_dir, id, meta.len(), mtime_ms, n);
            serve_file(&sidecar, "audio/mp4", range)
        }
        ["thumb", id] => {
            if !is_valid_id(id) {
                return error_response(&MediaError::BadId);
            }
            let mut resp = serve_file(&poster_path(db, id), "image/jpeg", range);
            if resp.status == 200 {
                set_header(
                    &mut resp.headers,
                    "Cache-Control",
                    "max-age=3600".to_string(),
                );
            }
            resp
        }
        _ => text_response(400, "bad path"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(start: u64, end_inclusive: u64) -> RangeSpec {
        RangeSpec::Partial {
            start,
            end_inclusive,
        }
    }

    #[test]
    fn parse_range_cases() {
        let len = 100;
        assert_eq!(parse_range(None, len), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=0-9"), len), partial(0, 9));
        assert_eq!(parse_range(Some("BYTES= 10 - 19 "), len), partial(10, 19));
        assert_eq!(parse_range(Some("bytes=90-"), len), partial(90, 99));
        assert_eq!(parse_range(Some("bytes=-10"), len), partial(90, 99));
        assert_eq!(parse_range(Some("bytes=-500"), len), partial(0, 99));
        assert_eq!(parse_range(Some("bytes=0-999"), len), partial(0, 99));
        assert_eq!(
            parse_range(Some("bytes=100-"), len),
            RangeSpec::Unsatisfiable
        );
        assert_eq!(parse_range(Some("bytes=-0"), len), RangeSpec::Unsatisfiable);
        // end < start is an invalid byte-range-spec: the header is ignored (Full).
        assert_eq!(parse_range(Some("bytes=50-10"), len), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=500-100"), 1000), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=0-0"), 0), RangeSpec::Unsatisfiable);
        assert_eq!(parse_range(Some("items=0-1"), len), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), len), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=abc"), len), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=-"), len), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=+5-"), len), RangeSpec::Full);
        assert_eq!(
            parse_range(Some("bytes=99999999999999999999999-"), len),
            RangeSpec::Full
        );
        assert_eq!(
            parse_range(Some("bytes=0-99999999999999999999999"), len),
            RangeSpec::Full
        );
    }

    #[test]
    fn parse_range_caps_to_max_chunk() {
        let big = 100 * 1024 * 1024;
        assert_eq!(
            parse_range(Some("bytes=0-"), big),
            partial(0, MAX_CHUNK - 1)
        );
        assert_eq!(
            parse_range(Some("bytes=5-"), big),
            partial(5, 5 + MAX_CHUNK - 1)
        );
        assert_eq!(
            parse_range(Some("bytes=0-"), u64::MAX),
            partial(0, MAX_CHUNK - 1)
        );
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type_for_ext("MP4"), "video/mp4");
        assert_eq!(content_type_for_ext("m4v"), "video/mp4");
        assert_eq!(content_type_for_ext("mov"), "video/quicktime");
        assert_eq!(content_type_for_ext("mkv"), "video/x-matroska");
        assert_eq!(content_type_for_ext("m4a"), "audio/mp4");
        assert_eq!(content_type_for_ext("avi"), "application/octet-stream");
    }

    #[test]
    fn ids() {
        assert!(is_valid_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(is_valid_id(&"a".repeat(40)));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id(&"a".repeat(41)));
        assert!(!is_valid_id("ab-cd"));
        assert!(!is_valid_id("../etc"));
    }

    #[test]
    fn serve_rejects_bad_paths_without_db() {
        let db = Path::new("clipcheck-media-test-missing.db");
        let cache = Path::new("clipcheck-media-test-cache");
        assert_eq!(serve(db, cache, "/nope", None).status, 400);
        assert_eq!(serve(db, cache, "/rec/", None).status, 400);
        assert_eq!(serve(db, cache, "/rec/bad-id", None).status, 400);
        assert_eq!(serve(db, cache, "/aud/01ABC/x", None).status, 400);
        assert_eq!(serve(db, cache, "/rec/01ABC", None).status, 404);
        assert_eq!(serve(db, cache, "/rec/01ABC?x=1#y", None).status, 404);
        assert_eq!(serve(db, cache, "/aud/01ABC/1", None).status, 404);
    }

    #[test]
    fn serve_file_range_responses() {
        let dir = std::env::temp_dir().join(format!("clipcheck-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.bin");
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::write(&path, &data).unwrap();

        let full = serve_file(&path, "video/mp4", None);
        assert_eq!(full.status, 200);
        assert_eq!(full.body, data);

        let part = serve_file(&path, "video/mp4", Some("bytes=10-19"));
        assert_eq!(part.status, 206);
        assert_eq!(part.body, data[10..=19].to_vec());
        assert!(part
            .headers
            .contains(&("Content-Range".to_string(), "bytes 10-19/1000".to_string())));
        assert!(part
            .headers
            .contains(&("Content-Length".to_string(), "10".to_string())));

        let suffix = serve_file(&path, "video/mp4", Some("bytes=-4"));
        assert_eq!(suffix.status, 206);
        assert_eq!(suffix.body, data[996..].to_vec());

        let unsat = serve_file(&path, "video/mp4", Some("bytes=1000-"));
        assert_eq!(unsat.status, 416);
        assert!(unsat.body.is_empty());
        assert!(unsat
            .headers
            .contains(&("Content-Range".to_string(), "bytes */1000".to_string())));

        let empty = dir.join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        let e = serve_file(&empty, "video/mp4", None);
        assert_eq!(e.status, 200);
        assert!(e.body.is_empty());

        assert_eq!(
            serve_file(&dir.join("missing.bin"), "video/mp4", None).status,
            404
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
