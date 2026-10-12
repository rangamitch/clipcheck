//! Loopback HTTP media server. The webview streams recordings and audio sidecars straight from
//! this server (off the UI thread, with no size cap per request). Every URL carries a random
//! per-run token, so other local processes and web pages cannot read recordings.
//! Routes: `/{token}/rec/{id}` and `/{token}/aud/{id}/{n}`.

use std::collections::HashMap;
use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::media::{self, RangeSpec};
use crate::{commands, player};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const PATH_TTL: Duration = Duration::from_secs(30);
const CHUNK: usize = 256 * 1024;

/// A running media server.
pub struct MediaServer {
    port: u16,
    token: String,
}

static SERVER: OnceLock<Option<MediaServer>> = OnceLock::new();
static PATHS: Mutex<Option<HashMap<String, (PathBuf, Instant)>>> = Mutex::new(None);
/// Imported files registered with [`register_extra_path`], keyed by their random URL key. Only
/// these exact canonical paths are served under `/{token}/imp/{key}`.
static IMPORTS: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);

/// Starts the server once (idempotent). `None` when binding failed.
pub fn start() -> Option<&'static MediaServer> {
    SERVER
        .get_or_init(|| {
            let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("clipcheck-ui: media server bind failed: {e}");
                    return None;
                }
            };
            let port = listener.local_addr().ok()?.port();
            let token = generate_token();
            let spawned = std::thread::Builder::new()
                .name("media-accept".into())
                .spawn({
                    let token = token.clone();
                    move || accept_loop(listener, token)
                });
            if let Err(e) = spawned {
                eprintln!("clipcheck-ui: media server thread failed: {e}");
                return None;
            }
            Some(MediaServer { port, token })
        })
        .as_ref()
}

impl MediaServer {
    /// `http://127.0.0.1:{port}/{token}`
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/{}", self.port, self.token)
    }
}

/// Base URL of the media server, or `None` if it could not start.
#[tauri::command]
pub fn media_base_url() -> Option<String> {
    start().map(MediaServer::base_url)
}

/// 128 random bits as 32 hex chars, from `RandomState` (OS-seeded) mixed with time and pid.
fn generate_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut parts = [0u64; 2];
    for (i, part) in parts.iter_mut().enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        h.write_usize(i);
        *part = h.finish();
    }
    format!("{:016x}{:016x}", parts[0], parts[1])
}

fn accept_loop(listener: TcpListener, token: String) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        if ACTIVE_CONNS.fetch_add(1, Ordering::AcqRel) >= MAX_CONNS {
            ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
            let mut s = stream;
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = write_simple(&mut s, 503, true);
            continue;
        }
        let token = token.clone();
        let spawned = std::thread::Builder::new()
            .name("media-conn".into())
            .spawn(move || {
                let _guard = ConnGuard;
                handle_connection(stream, &token);
            });
        if spawned.is_err() {
            ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Simultaneous connections allowed; each gets its own short-lived thread, so idle keep-alive
/// sockets never hold a worker that another media element is waiting on.
const MAX_CONNS: usize = 64;
static ACTIVE_CONNS: AtomicUsize = AtomicUsize::new(0);

struct ConnGuard;
impl Drop for ConnGuard {
    fn drop(&mut self) {
        ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    method: String,
    target: String,
    range: Option<String>,
    close: bool,
}

enum ReadError {
    /// Peer closed, timed out or I/O failed: drop the connection silently.
    Closed,
    /// Reply with this status, then close.
    Reject(u16),
}

fn handle_connection(stream: TcpStream, token: &str) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let req = match read_request(&mut reader) {
            Ok(r) => r,
            Err(ReadError::Closed) => return,
            Err(ReadError::Reject(status)) => {
                let _ = write_simple(&mut writer, status, true);
                return;
            }
        };
        let keep = !req.close && handle_request(&mut writer, &req, token);
        if !keep {
            return;
        }
    }
}

/// Reads one request head. Header block is capped at 16 KiB.
fn read_request<R: BufRead>(reader: &mut R) -> Result<Request, ReadError> {
    let mut total = 0usize;
    let mut lines: Vec<String> = Vec::new();
    loop {
        let remaining = MAX_HEADER_BYTES - total;
        if remaining == 0 {
            return Err(ReadError::Reject(431));
        }
        let mut buf = Vec::new();
        let n = reader
            .by_ref()
            .take(remaining as u64)
            .read_until(b'\n', &mut buf)
            .map_err(|_| ReadError::Closed)?;
        if n == 0 {
            return Err(ReadError::Closed);
        }
        total += n;
        if buf.last() != Some(&b'\n') {
            return Err(if total >= MAX_HEADER_BYTES {
                ReadError::Reject(431)
            } else {
                ReadError::Closed
            });
        }
        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
        if line.is_empty() {
            // Tolerate stray blank lines before the request line.
            if lines.is_empty() {
                continue;
            }
            break;
        }
        lines.push(line);
    }
    parse_request(&lines).ok_or(ReadError::Reject(400))
}

/// Parses the request line and headers (already split into lines).
fn parse_request(lines: &[String]) -> Option<Request> {
    let mut first = lines.first()?.split_whitespace();
    let method = first.next()?.to_ascii_uppercase();
    let target = first.next()?.to_string();
    let version = first.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    let mut close = version == "HTTP/1.0";
    let mut range = None;
    for line in &lines[1..] {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("range") {
            range = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("connection") {
            let v = value.to_ascii_lowercase();
            if v.contains("close") {
                close = true;
            } else if v.contains("keep-alive") {
                close = false;
            }
        }
    }
    Some(Request {
        method,
        target,
        range,
        close,
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Route {
    Rec(String),
    Aud(String, u32),
    /// An imported file registered with `register_extra_path` (key, not a recording id).
    Imp(String),
}

/// Matches `/{token}/rec/{id}` and `/{token}/aud/{id}/{n}`; `None` for anything else.
fn match_route(target: &str, token: &str) -> Option<Route> {
    let path = target.split(['?', '#']).next()?;
    let rest = path.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [t, "rec", id] if *t == token && media::is_valid_id(id) => Some(Route::Rec((*id).into())),
        [t, "aud", id, n] if *t == token && media::is_valid_id(id) => {
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(Route::Aud((*id).into(), n.parse().ok()?))
        }
        [t, "imp", key] if *t == token && media::is_valid_id(key) => {
            Some(Route::Imp((*key).into()))
        }
        _ => None,
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        503 => "Service Unavailable",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

/// Builds a response head including the CORS headers every response carries. The wildcard
/// origin is acceptable only because every URL holds the secret per-run token.
fn build_head(status: u16, extra: &[(&str, String)], close: bool) -> String {
    let mut s = format!("HTTP/1.1 {status} {}\r\n", status_text(status));
    for (k, v) in extra {
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        s.push_str("\r\n");
    }
    s.push_str("Access-Control-Allow-Origin: *\r\n");
    s.push_str("Access-Control-Allow-Headers: Range\r\n");
    s.push_str("Access-Control-Allow-Methods: GET, HEAD, OPTIONS\r\n");
    s.push_str("Access-Control-Expose-Headers: Content-Range, Accept-Ranges, Content-Length\r\n");
    s.push_str("Cross-Origin-Resource-Policy: cross-origin\r\n");
    s.push_str(if close {
        "Connection: close\r\n"
    } else {
        "Connection: keep-alive\r\n"
    });
    s.push_str("\r\n");
    s
}

/// Empty-body response (errors, preflight).
fn write_simple(w: &mut TcpStream, status: u16, close: bool) -> std::io::Result<()> {
    let head = build_head(status, &[("Content-Length", "0".to_string())], close);
    w.write_all(head.as_bytes())?;
    w.flush()
}

fn etag(size: u64, mtime_ms: i64) -> String {
    format!("W/\"{size:x}-{mtime_ms:x}\"")
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn cache_dir() -> PathBuf {
    player::cache_dir_from(
        std::env::var("CLIPCHECK_CACHE_DIR").ok().as_deref(),
        std::env::var("LOCALAPPDATA").ok().as_deref(),
    )
}

/// Recording path for `id`, cached for 30 s so range requests do not hit SQLite.
fn recording_path(id: &str, force: bool) -> Option<PathBuf> {
    if !force {
        let guard = PATHS.lock().ok()?;
        if let Some((p, at)) = guard.as_ref().and_then(|m| m.get(id)) {
            if at.elapsed() < PATH_TTL {
                return Some(p.clone());
            }
        }
    }
    let db = commands::try_default_db_path().ok()?;
    let path = media::resolve_recording_path(&db, id).ok()?;
    if let Ok(mut guard) = PATHS.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(id.to_string(), (path.clone(), Instant::now()));
    }
    Some(path)
}

/// Opens an imported file by its registered key. `None` for unknown keys.
fn open_import(key: &str) -> Option<(File, &'static str, std::fs::Metadata)> {
    let path = {
        let guard = IMPORTS.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().and_then(|m| m.get(key)).cloned()?
    };
    let file = File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let ct = path
        .extension()
        .and_then(|e| e.to_str())
        .map(media::content_type_for_ext)
        .unwrap_or("application/octet-stream");
    Some((file, ct, meta))
}

/// Registers an imported media file for playback and returns its loopback URL
/// (`http://127.0.0.1:{port}/{token}/imp/{key}`). Only registered files are served, each under a
/// fresh random key; registering the same file again returns the same URL. `None` when the file
/// is not a regular file or the server is not running.
pub fn register_extra_path(path: &Path) -> Option<String> {
    let server = start()?;
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.is_file() {
        return None;
    }
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !IMPORT_EXTS.contains(&ext.as_str()) {
        return None;
    }
    let mut guard = IMPORTS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let existing = map
        .iter()
        .find(|(_, p)| **p == canonical)
        .map(|(k, _)| k.clone());
    let key = match existing {
        Some(k) => k,
        None => {
            let k = generate_token();
            map.insert(k.clone(), canonical);
            k
        }
    };
    Some(format!("{}/imp/{key}", server.base_url()))
}

/// Forgets every registered import. Their URLs stop working until the files are registered again.
pub fn clear_imports() {
    let mut guard = IMPORTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        map.clear();
    }
}

/// Extensions accepted by [`register_extra_path`] (lower case).
const IMPORT_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "m4a", "mp3", "wav", "flac", "aac", "ogg", "wma", "png", "jpg", "jpeg",
    "bmp",
];

/// Largest answer to an open-ended `Range: bytes=N-` request on a recording or import. Browsers
/// allow only six connections per host, so an unbounded answer would block the others.
const OPEN_RANGE_CAP: u64 = 8 * 1024 * 1024;

/// Byte cap for a recording or import response: bounded only for an open-ended Range request.
fn media_cap(req: &Request) -> u64 {
    let open_ended = req
        .range
        .as_deref()
        .is_some_and(|r| r.trim_end().ends_with('-'));
    if open_ended {
        OPEN_RANGE_CAP
    } else {
        u64::MAX
    }
}

/// Opens the file a route points to. Returns the file, its content type and metadata.
fn open_route(route: &Route) -> Option<(File, &'static str, std::fs::Metadata)> {
    if let Route::Imp(key) = route {
        return open_import(key);
    }
    let id = match route {
        Route::Rec(id) | Route::Aud(id, _) => id,
        Route::Imp(_) => return None,
    };
    for force in [false, true] {
        let rec = recording_path(id, force)?;
        let target: (PathBuf, &'static str) = match route {
            Route::Rec(_) => {
                let ct = rec
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(media::content_type_for_ext)
                    .unwrap_or("application/octet-stream");
                (rec, ct)
            }
            Route::Aud(id, n) => {
                let Ok(meta) = std::fs::metadata(&rec) else {
                    continue;
                };
                (
                    media::sidecar_path(&cache_dir(), id, meta.len(), mtime_ms(&meta), *n),
                    "audio/mp4",
                )
            }
            Route::Imp(_) => return None,
        };
        if let Ok(file) = File::open(&target.0) {
            if let Ok(meta) = file.metadata() {
                return Some((file, target.1, meta));
            }
        }
        // A stale cached path (moved or deleted file) gets one fresh resolve.
    }
    None
}

/// Handles one request. Returns false when the connection should be closed.
fn handle_request(w: &mut TcpStream, req: &Request, token: &str) -> bool {
    let method = req.method.as_str();
    let result = match method {
        "OPTIONS" => write_simple(w, 204, false),
        "GET" | "HEAD" => match match_route(&req.target, token) {
            None => write_simple(w, 404, false),
            Some(route) => match open_route(&route) {
                None => write_simple(w, 404, false),
                Some((file, ct, meta)) => {
                    let cap = match route {
                        Route::Rec(_) | Route::Imp(_) => media_cap(req),
                        Route::Aud(..) => AUDIO_CHUNK,
                    };
                    serve(w, file, ct, &meta, req, method == "HEAD", cap)
                }
            },
        },
        _ => write_simple(w, 405, false),
    };
    result.is_ok()
}

/// Audio sidecars are answered in bounded chunks. Chromium allows only 6 HTTP/1.1 connections per
/// host:port, and an open-ended audio response stays open while its element buffers: with 6
/// audio tracks that left no connection for the video, which then never loaded or seeked.
const AUDIO_CHUNK: u64 = 512 * 1024;

fn serve(
    w: &mut TcpStream,
    mut file: File,
    content_type: &str,
    meta: &std::fs::Metadata,
    req: &Request,
    head_only: bool,
    cap: u64,
) -> std::io::Result<()> {
    let len = meta.len();
    let tag = etag(len, mtime_ms(meta));
    let common = |extra: Vec<(&'static str, String)>| {
        let mut v = vec![
            ("Content-Type", content_type.to_string()),
            ("Accept-Ranges", "bytes".to_string()),
            ("Cache-Control", "private, max-age=0".to_string()),
            ("ETag", tag.clone()),
        ];
        v.extend(extra);
        v
    };
    let (status, start, span, mut extra) =
        match media::parse_range_capped(req.range.as_deref(), len, cap) {
            RangeSpec::Unsatisfiable => {
                let head = build_head(
                    416,
                    &[
                        ("Content-Length", "0".to_string()),
                        ("Content-Range", format!("bytes */{len}")),
                        ("Accept-Ranges", "bytes".to_string()),
                    ],
                    false,
                );
                w.write_all(head.as_bytes())?;
                return w.flush();
            }
            RangeSpec::Full if len > cap => (
                206,
                0,
                cap,
                vec![("Content-Range", format!("bytes 0-{}/{len}", cap - 1))],
            ),
            RangeSpec::Full => (200, 0, len, Vec::new()),
            RangeSpec::Partial {
                start,
                end_inclusive,
            } => {
                if start > end_inclusive || end_inclusive >= len {
                    let head = build_head(
                        416,
                        &[
                            ("Content-Length", "0".to_string()),
                            ("Content-Range", format!("bytes */{len}")),
                        ],
                        false,
                    );
                    w.write_all(head.as_bytes())?;
                    return w.flush();
                }
                (
                    206,
                    start,
                    end_inclusive - start + 1,
                    vec![(
                        "Content-Range",
                        format!("bytes {start}-{end_inclusive}/{len}"),
                    )],
                )
            }
        };
    extra.push(("Content-Length", span.to_string()));
    let head = build_head(status, &common(extra), false);
    w.write_all(head.as_bytes())?;
    if head_only || span == 0 {
        return w.flush();
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; CHUNK];
    let mut left = span;
    while left > 0 {
        let want = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            // File shrank under us: the declared length cannot be met, so drop the connection.
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        // A write error means the client closed or seeked away: stop quietly.
        w.write_all(&buf[..n])?;
        left -= n as u64;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_hex_and_varies() {
        let a = generate_token();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, generate_token());
    }

    #[test]
    fn routes() {
        assert_eq!(
            match_route("/tok/rec/ABC123?x=1", "tok"),
            Some(Route::Rec("ABC123".into()))
        );
        assert_eq!(
            match_route("/tok/aud/ABC/2", "tok"),
            Some(Route::Aud("ABC".into(), 2))
        );
        assert_eq!(match_route("/bad/rec/ABC", "tok"), None);
        assert_eq!(match_route("/tok/rec/a-b", "tok"), None);
        assert_eq!(match_route("/tok/aud/ABC/x", "tok"), None);
        assert_eq!(match_route("/tok/other/ABC", "tok"), None);
    }

    #[test]
    fn parses_request() {
        let lines: Vec<String> = [
            "GET /t/rec/A HTTP/1.1",
            "Host: x",
            "Range: bytes=5-",
            "Connection: close",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let r = parse_request(&lines).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.range.as_deref(), Some("bytes=5-"));
        assert!(r.close);
        assert!(parse_request(&["GET /".to_string()]).is_none());
    }

    #[test]
    fn head_has_cors() {
        let h = build_head(206, &[("Content-Length", "1".to_string())], false);
        assert!(h.starts_with("HTTP/1.1 206 Partial Content\r\n"));
        assert!(h.contains("Access-Control-Allow-Origin: *\r\n"));
        assert!(h.contains("Cross-Origin-Resource-Policy: cross-origin\r\n"));
        assert!(h.ends_with("\r\n\r\n"));
    }
}
