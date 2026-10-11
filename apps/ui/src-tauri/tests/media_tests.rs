//! Tests for `clipcheck_ui_lib::media` (spec: docs/impl/player-mvp.md, "URLs" and "media.rs").
//! Black-box: only the public API and the spec. Every fixture lives in a TempDir.

mod common_player;

use std::path::{Path, PathBuf};

use clipcheck_ui_lib::media::{
    content_type_for_ext, is_valid_id, parse_range, resolve_recording_path, serve, sidecar_path,
    MediaError, RangeSpec, MAX_CHUNK,
};
use common_player::{hdr, make_db, mtime_ms, pattern, Row};
use tempfile::TempDir;

#[derive(Clone, Copy, Debug)]
enum Want {
    Full,
    P(u64, u64),
    U,
}

fn show(r: &RangeSpec) -> String {
    match r {
        RangeSpec::Full => "Full".into(),
        RangeSpec::Partial {
            start,
            end_inclusive,
        } => format!("Partial({start},{end_inclusive})"),
        RangeSpec::Unsatisfiable => "Unsatisfiable".into(),
    }
}

fn matches_want(r: &RangeSpec, w: Want) -> bool {
    match (r, w) {
        (RangeSpec::Full, Want::Full) | (RangeSpec::Unsatisfiable, Want::U) => true,
        (
            RangeSpec::Partial {
                start,
                end_inclusive,
            },
            Want::P(s, e),
        ) => *start == s && *end_inclusive == e,
        _ => false,
    }
}

/// Per-test fixture: a tempdir with `root/` holding the media file, a DB, and a cache dir.
struct Env {
    _dir: TempDir,
    root: PathBuf,
    db: PathBuf,
    cache: PathBuf,
}

/// Writes `bytes` to `root/<file>` and registers it as recording 01REC100000000000000000000 (ext from file name).
fn env_with(file: &str, bytes: &[u8]) -> Env {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir root");
    std::fs::write(root.join(file), bytes).expect("write media");
    let ext = Path::new(file)
        .extension()
        .expect("ext")
        .to_string_lossy()
        .into_owned();
    let db = make_db(
        &dir.path().join("library.db"),
        &root,
        &[Row {
            id: "01REC100000000000000000000",
            rel_path: file,
            ext: &ext,
            duration_ms: 1000,
        }],
    );
    let cache = dir.path().join("cache");
    Env {
        _dir: dir,
        root,
        db,
        cache,
    }
}

fn get(e: &Env, url: &str, range: Option<&str>) -> clipcheck_ui_lib::media::MediaResponse {
    serve(&e.db, &e.cache, url, range)
}

// ---------------------------------------------------------------- parse_range

#[test]
fn max_chunk_is_8_mib() {
    assert_eq!(MAX_CHUNK, 8 * 1024 * 1024);
}

#[test]
fn parse_range_table() {
    const BIG: u64 = 100 * 1024 * 1024;
    let cases: Vec<(&str, Option<&str>, u64, Vec<Want>)> = vec![
        ("none", None, 1000, vec![Want::Full]),
        ("a-b", Some("bytes=0-99"), 1000, vec![Want::P(0, 99)]),
        ("a-", Some("bytes=500-"), 1000, vec![Want::P(500, 999)]),
        ("-n", Some("bytes=-100"), 1000, vec![Want::P(900, 999)]),
        // RFC 7233: a suffix longer than the representation selects the whole representation.
        ("-n>len", Some("bytes=-2000"), 1000, vec![Want::P(0, 999)]),
        ("-0", Some("bytes=-0"), 1000, vec![Want::U]),
        ("a>=len", Some("bytes=1000-"), 1000, vec![Want::U]),
        (
            "last byte",
            Some("bytes=999-"),
            1000,
            vec![Want::P(999, 999)],
        ),
        (
            "b<a (invalid)",
            Some("bytes=500-100"),
            1000,
            vec![Want::Full],
        ),
        ("len 0 a-b", Some("bytes=0-99"), 0, vec![Want::U]),
        ("len 0 a-", Some("bytes=0-"), 0, vec![Want::U]),
        // RFC 7233: suffix range on an empty representation is unsatisfiable.
        ("len 0 -n", Some("bytes=-5"), 0, vec![Want::U]),
        ("multi-range", Some("bytes=0-0,5-6"), 1000, vec![Want::Full]),
        ("items unit", Some("items=0-1"), 1000, vec![Want::Full]),
        ("garbage", Some("bytes=abc"), 1000, vec![Want::Full]),
        ("empty spec", Some("bytes="), 1000, vec![Want::Full]),
        ("dash only", Some("bytes=-"), 1000, vec![Want::Full]),
        (
            "end clamped",
            Some("bytes=0-99999"),
            1000,
            vec![Want::P(0, 999)],
        ),
        // Range-unit is case-insensitive (RFC 9110 section 14.1); interpretation, see report.
        (
            "uppercase BYTES",
            Some("BYTES=0-99"),
            1000,
            vec![Want::P(0, 99)],
        ),
        (
            "trailing space",
            Some("bytes=0-9 "),
            1000,
            vec![Want::P(0, 9)],
        ),
        // Overflowing start: invalid (=> Full) or >= len (=> Unsatisfiable); both defensible.
        (
            "u64 overflow start",
            Some("bytes=99999999999999999999999-"),
            1000,
            vec![Want::Full, Want::U],
        ),
        // Overflowing end: invalid (=> Full) or clamped to len-1; both defensible.
        (
            "u64 overflow end",
            Some("bytes=0-99999999999999999999999"),
            1000,
            vec![Want::Full, Want::P(0, 999)],
        ),
        (
            "u64::MAX end clamped",
            Some("bytes=0-18446744073709551615"),
            1000,
            vec![Want::P(0, 999)],
        ),
        ("single byte", Some("bytes=7-7"), 10, vec![Want::P(7, 7)]),
        ("exact fit", Some("bytes=0-99"), 100, vec![Want::P(0, 99)]),
        (
            "last of 100",
            Some("bytes=99-99"),
            100,
            vec![Want::P(99, 99)],
        ),
        // Span capping to MAX_CHUNK.
        (
            "open-ended capped",
            Some("bytes=0-"),
            BIG,
            vec![Want::P(0, MAX_CHUNK - 1)],
        ),
        (
            "open-ended capped offset",
            Some("bytes=5-"),
            BIG,
            vec![Want::P(5, 5 + MAX_CHUNK - 1)],
        ),
        (
            "explicit end capped",
            Some("bytes=0-99999999"),
            BIG,
            vec![Want::P(0, MAX_CHUNK - 1)],
        ),
    ];
    let mut bad = Vec::new();
    for (label, h, len, wants) in &cases {
        let got = parse_range(*h, *len);
        if !wants.iter().any(|w| matches_want(&got, *w)) {
            bad.push(format!(
                "[{label}] parse_range({h:?}, {len}) = {}, want one of {:?}",
                show(&got),
                wants
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "parse_range mismatches:\n{}",
        bad.join("\n")
    );
    // Not in the spec: whitespace inside the spec. Observation only.
    eprintln!(
        "OBSERVATION: parse_range(\"bytes=0 - 9\", 1000) = {}",
        show(&parse_range(Some("bytes=0 - 9"), 1000))
    );
}

// ---------------------------------------------------------------- content_type_for_ext

#[test]
fn content_type_table() {
    let cases = [
        ("mp4", "video/mp4"),
        ("m4v", "video/mp4"),
        ("mov", "video/quicktime"),
        ("mkv", "video/x-matroska"),
        ("m4a", "audio/mp4"),
        ("exe", "application/octet-stream"),
        ("txt", "application/octet-stream"),
        ("", "application/octet-stream"),
    ];
    for (ext, want) in cases {
        assert_eq!(content_type_for_ext(ext), want, "ext {ext:?}");
    }
    // Not in the spec; observation only.
    eprintln!(
        "OBSERVATION: content_type_for_ext(\"MP4\") = {:?}",
        content_type_for_ext("MP4")
    );
}

// ---------------------------------------------------------------- is_valid_id

#[test]
fn is_valid_id_table() {
    let forty = "a".repeat(40);
    let fortyone = "a".repeat(41);
    assert!(!is_valid_id(""), "empty");
    assert!(is_valid_id(&forty), "40 chars");
    assert!(!is_valid_id(&fortyone), "41 chars");
    assert!(is_valid_id("a"), "one char");
    assert!(is_valid_id("01ABCxyz0123"), "mixed alnum");
    assert!(!is_valid_id("../x"), "traversal");
    assert!(!is_valid_id("ab/cd"), "slash");
    assert!(!is_valid_id("a b"), "space");
    assert!(!is_valid_id("A-b"), "hyphen");
    assert!(!is_valid_id("\u{e9}abc"), "unicode letter");
    assert!(!is_valid_id("abc\n"), "trailing newline");
}

// ---------------------------------------------------------------- resolve_recording_path

#[test]
fn resolve_recording_path_is_db_driven() {
    let e = env_with("clip.mp4", &pattern(100));
    let p = must_ok(resolve_recording_path(&e.db, "01REC100000000000000000000"));
    assert_eq!(p, e.root.join("clip.mp4"));
    assert!(matches!(
        resolve_recording_path(&e.db, "NOPE"),
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        resolve_recording_path(&e.db, "../x"),
        Err(MediaError::BadId)
    ));
}

fn must_ok<T>(r: Result<T, MediaError>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("resolve_recording_path failed: {e}"),
    }
}

// ---------------------------------------------------------------- serve: video

#[test]
fn rec_full_200_small_file() {
    let data = pattern(1000);
    let e = env_with("clip.mp4", &data);
    let r = get(&e, "/rec/01REC100000000000000000000", None);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, data);
    assert_eq!(
        hdr(&r.headers, "Content-Type").as_deref(),
        Some("video/mp4")
    );
}

#[test]
fn rec_206_exact_bytes_and_headers() {
    let data = pattern(1000);
    let e = env_with("clip.mp4", &data);
    let r = get(&e, "/rec/01REC100000000000000000000", Some("bytes=100-199"));
    assert_eq!(r.status, 206);
    assert_eq!(r.body, data[100..200]);
    assert_eq!(
        hdr(&r.headers, "Content-Range").as_deref(),
        Some("bytes 100-199/1000")
    );
    assert_eq!(hdr(&r.headers, "Content-Length").as_deref(), Some("100"));
    assert_eq!(hdr(&r.headers, "Accept-Ranges").as_deref(), Some("bytes"));
    assert_eq!(
        hdr(&r.headers, "Cache-Control").as_deref(),
        Some("no-store")
    );
    assert_eq!(
        hdr(&r.headers, "Content-Type").as_deref(),
        Some("video/mp4")
    );

    let s = get(&e, "/rec/01REC100000000000000000000", Some("bytes=-50"));
    assert_eq!(s.status, 206);
    assert_eq!(s.body, data[950..]);
    assert_eq!(
        hdr(&s.headers, "Content-Range").as_deref(),
        Some("bytes 950-999/1000")
    );
}

#[test]
fn rec_open_ended_is_capped_to_max_chunk() {
    let len = 20 * 1024 * 1024;
    let data = pattern(len);
    let e = env_with("big.mp4", &data);
    let chunk = usize::try_from(MAX_CHUNK).expect("fits usize");

    let r = get(&e, "/rec/01REC100000000000000000000", Some("bytes=0-"));
    assert_eq!(r.status, 206);
    assert_eq!(r.body.len(), chunk, "body must be MAX_CHUNK");
    assert_eq!(r.body, data[..chunk]);
    assert_eq!(
        hdr(&r.headers, "Content-Range").as_deref(),
        Some(format!("bytes 0-{}/{}", chunk - 1, len).as_str())
    );
    assert_eq!(
        hdr(&r.headers, "Content-Length").as_deref(),
        Some(chunk.to_string().as_str())
    );

    let s = get(&e, "/rec/01REC100000000000000000000", Some("bytes=1000-"));
    assert_eq!(s.status, 206);
    assert_eq!(s.body.len(), chunk);
    assert_eq!(s.body, data[1000..1000 + chunk]);
}

#[test]
fn rec_unsatisfiable_416() {
    let e = env_with("clip.mp4", &pattern(1000));
    let r = get(&e, "/rec/01REC100000000000000000000", Some("bytes=5000-"));
    assert_eq!(r.status, 416);
    assert_eq!(
        hdr(&r.headers, "Content-Range").as_deref(),
        Some("bytes */1000")
    );
}

#[test]
fn rec_bad_paths_are_400() {
    let e = env_with("clip.mp4", &pattern(100));
    for url in [
        "/rec/",
        "/rec/../x",
        "/foo",
        "/aud/01REC100000000000000000000/x",
        "/rec/01REC100000000000000000000/extra",
        "/rec/01REC100000000000000000000%2F..%2Fx",
        "/rec/..%2Foutside.mp4",
        "/rec/01REC100000000000000000000/../../outside.mp4",
    ] {
        let r = get(&e, url, None);
        assert_eq!(r.status, 400, "url {url:?}");
    }
}

#[test]
fn query_string_is_ignored() {
    let data = pattern(300);
    let e = env_with("clip.mp4", &data);
    let r = get(&e, "/rec/01REC100000000000000000000?v=1&t=2", None);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, data);
}

#[test]
fn unknown_id_is_404() {
    let e = env_with("clip.mp4", &pattern(100));
    assert_eq!(get(&e, "/rec/NOPE", None).status, 404);
    assert_eq!(get(&e, "/aud/NOPE/0", None).status, 404);
}

#[test]
fn missing_db_is_404() {
    let e = env_with("clip.mp4", &pattern(100));
    let r = serve(
        &e.root.join("missing.db"),
        &e.cache,
        "/rec/01REC100000000000000000000",
        None,
    );
    assert_eq!(r.status, 404);
}

#[test]
fn deleted_file_is_404() {
    let e = env_with("clip.mp4", &pattern(100));
    std::fs::remove_file(e.root.join("clip.mp4")).expect("remove");
    assert_eq!(get(&e, "/rec/01REC100000000000000000000", None).status, 404);
}

#[test]
fn rel_path_is_db_driven_not_url_driven() {
    // DB row whose rel_path climbs out of the root: resolved from the DB, so it is served.
    // The URL cannot name a path at all (see rec_bad_paths_are_400).
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("clip.mp4"), b"INSIDE").expect("write");
    std::fs::write(dir.path().join("outside.mp4"), b"OUTSIDE").expect("write");
    let db = make_db(
        &dir.path().join("library.db"),
        &root,
        &[
            Row {
                id: "01REC100000000000000000000",
                rel_path: "clip.mp4",
                ext: "mp4",
                duration_ms: 1,
            },
            Row {
                id: "01REC200000000000000000000",
                rel_path: "../outside.mp4",
                ext: "mp4",
                duration_ms: 1,
            },
        ],
    );
    let cache = dir.path().join("cache");
    let inside = serve(&db, &cache, "/rec/01REC100000000000000000000", None);
    assert_eq!(inside.status, 200);
    assert_eq!(inside.body, b"INSIDE");
    let outside = serve(&db, &cache, "/rec/01REC200000000000000000000", None);
    assert_eq!(outside.status, 200);
    assert_eq!(outside.body, b"OUTSIDE");
}

// ---------------------------------------------------------------- serve: audio sidecars

#[test]
fn aud_sidecar_404_until_extracted_then_serves() {
    let media = pattern(1000);
    let e = env_with("clip.mp4", &media);
    let size = 1000u64;
    let mtime = mtime_ms(&e.root.join("clip.mp4"));

    assert_eq!(
        get(&e, "/aud/01REC100000000000000000000/0", None).status,
        404,
        "before extraction"
    );

    let sp = sidecar_path(&e.cache, "01REC100000000000000000000", size, mtime, 0);
    std::fs::create_dir_all(sp.parent().expect("parent")).expect("mkdir cache");
    let audio = vec![0xABu8; 500];
    std::fs::write(&sp, &audio).expect("write sidecar");

    let r = get(&e, "/aud/01REC100000000000000000000/0", None);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, audio);
    assert_eq!(
        hdr(&r.headers, "Content-Type").as_deref(),
        Some("audio/mp4")
    );

    let s = get(&e, "/aud/01REC100000000000000000000/0", Some("bytes=10-19"));
    assert_eq!(s.status, 206);
    assert_eq!(s.body, audio[10..20]);
    assert_eq!(
        hdr(&s.headers, "Content-Range").as_deref(),
        Some("bytes 10-19/500")
    );

    assert_eq!(
        get(&e, "/aud/01REC100000000000000000000/1", None).status,
        404,
        "track 1 not extracted"
    );
}

#[test]
fn aud_path_stays_db_driven() {
    // A URL that tries to smuggle a path into the audio route is rejected outright.
    let e = env_with("clip.mp4", &pattern(100));
    assert_eq!(get(&e, "/aud/..%2Fx/0", None).status, 400);
    assert_eq!(
        get(&e, "/aud/01REC100000000000000000000/../0", None).status,
        400
    );
}

// ---------------------------------------------------------------- content types / sidecar path shape

#[test]
fn sidecar_path_shape() {
    let p = sidecar_path(
        Path::new("C:\\cache"),
        "01REC100000000000000000000",
        1234,
        5678,
        2,
    );
    let want = Path::new("C:\\cache")
        .join("audio")
        .join("01REC100000000000000000000")
        .join("1234-5678")
        .join("track2.m4a");
    assert_eq!(p, want);
}
