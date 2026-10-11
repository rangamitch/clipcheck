//! Integration tests for `clipcheck_engine::catalog`.
//!
//! Native test media from `clipcheck-testkit` generates the inputs; `clipcheck-mp4` reads them.
//! Each test builds its own tempdir and in-memory library. The shared 3 s source
//! clip is encoded once per test binary and copied into each tempdir.

#[macro_use]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use clipcheck_engine::catalog::{
    make_thumbnail, probe_media, register_imported, register_recording, CatalogError, RegisterMeta,
};
use clipcheck_library::db::RecordingSource;
use clipcheck_library::{Library, ListQuery, RecStatus, RecordingKind};
use clipcheck_testkit::{AudioSpec, MediaSpec};
use tempfile::TempDir;

const CREATED_MS: i64 = 1_700_000_000_000;
const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

/// The 3 s source fixture, encoded once per test binary under the shared fixture
/// directory. Tests copy it, so none of them re-encodes it.
fn cached_source_mp4() -> &'static Path {
    static SRC: OnceLock<PathBuf> = OnceLock::new();
    SRC.get_or_init(|| {
        let out = common::fixture_dir().join("source.mp4");
        let spec = MediaSpec::h264_aac(3.0).with_video(320, 240, 25);
        clipcheck_testkit::write_mp4(&out, &spec).expect("testkit could not write source mp4");
        out
    })
}

/// Per-test environment: a tempdir holding everything the test creates.
struct Env {
    tmp: TempDir,
}

impl Env {
    fn new() -> Option<Self> {
        common::vendored_tools()?;
        Some(Env {
            tmp: tempfile::tempdir().expect("tempdir"),
        })
    }

    /// Storage root (created).
    fn root(&self) -> PathBuf {
        let p = self.tmp.path().join("root");
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Thumbnail directory (not created; registration must create what it needs).
    fn thumbs(&self) -> PathBuf {
        self.tmp.path().join("thumbs")
    }

    /// Copies the 3 s source fixture to `<tmp>/media/source.mp4` and returns its path.
    fn make_mp4(&self) -> PathBuf {
        let dir = self.tmp.path().join("media");
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("source.mp4");
        fs::copy(cached_source_mp4(), &out).expect("copy source fixture");
        out
    }

    /// Copies `src` to `dest` (creating parent directories) and returns `dest`.
    fn copy_to(&self, src: &Path, dest: PathBuf) -> PathBuf {
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(src, &dest).unwrap();
        dest
    }

    /// Writes a non-video file at `<tmp>/media/corrupt.mp4`.
    fn make_corrupt(&self) -> PathBuf {
        let dir = self.tmp.path().join("media");
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("corrupt.mp4");
        let bytes: Vec<u8> = b"not a video at all".repeat(64);
        fs::write(&p, bytes).unwrap();
        p
    }
}

/// Builds an `Env`, or skips the test when native test media is unavailable.
macro_rules! env_or_skip {
    () => {
        match Env::new() {
            Some(env) => env,
            None => skip!("native test media unavailable"),
        }
    };
}

fn meta(game: &str) -> RegisterMeta<'_> {
    RegisterMeta {
        kind: RecordingKind::Clip,
        game,
        session_id: None,
        part_index: None,
        created_ms: CREATED_MS,
    }
}

fn rel(parts: &[&str]) -> String {
    parts
        .iter()
        .fold(PathBuf::new(), |p, s| p.join(s))
        .to_string_lossy()
        .into_owned()
}

fn list_len(lib: &Library) -> usize {
    lib.list(&ListQuery::default()).expect("list").items.len()
}

#[test]
fn probe_media_reports_expected_fields() {
    let env = env_or_skip!();
    let file = env.make_mp4();
    let info = probe_media(&file).expect("probe ok");

    assert!(
        (2500..=3500).contains(&info.duration_ms),
        "duration_ms = {}",
        info.duration_ms
    );
    assert_eq!(info.width, 320);
    assert_eq!(info.height, 240);
    assert!((24.0..=26.0).contains(&info.fps), "fps = {}", info.fps);
    assert_eq!(info.video_codec, "h264");
    assert_eq!(
        info.audio_tracks.len(),
        1,
        "audio tracks: {:?}",
        info.audio_tracks
    );
    let a = &info.audio_tracks[0];
    assert_eq!(a.codec, "aac");
    assert!(a.sample_rate > 0, "sample_rate = {}", a.sample_rate);
    assert!(a.channels >= 1, "channels = {}", a.channels);
    assert_eq!(a.index, 0);
    assert_eq!(info.size_bytes, fs::metadata(&file).unwrap().len());
}

#[test]
fn probe_media_corrupt_file_is_err() {
    let env = env_or_skip!();
    let file = env.make_corrupt();
    let r = probe_media(&file);
    assert!(r.is_err(), "expected Err, got {r:?}");
}

#[test]
fn probe_media_missing_file_is_not_found() {
    // The probe reads the file in-process (D58), so a missing file is the NotFound case.
    let env = env_or_skip!();
    let missing = env.tmp.path().join("nope.mp4");
    let err = probe_media(&missing).expect_err("expected Err");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "kind = {:?}",
        err.kind()
    );
}

#[test]
fn make_thumbnail_creates_parent_and_writes_jpeg() {
    let env = env_or_skip!();
    let file = env.make_mp4();
    let out = env.tmp.path().join("thumbs").join("abc").join("poster.jpg");
    assert!(!out.parent().unwrap().exists());

    make_thumbnail(&file, &out, 0.75, 160).expect("thumbnail ok");

    let bytes = fs::read(&out).expect("poster exists");
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "not a JPEG header");
}

#[test]
fn make_thumbnail_past_end_yields_last_frame() {
    // Native decoder behaviour (D58): a time past the end gives the last frame, not an error.
    let env = env_or_skip!();
    let file = env.make_mp4();
    let out = env.tmp.path().join("past").join("poster.jpg");

    make_thumbnail(&file, &out, 999.0, 160).expect("past-end thumbnail ok");
    let bytes = fs::read(&out).expect("poster exists");
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "not a JPEG header");
}

#[test]
fn make_thumbnail_corrupt_input_is_err() {
    let env = env_or_skip!();
    let file = env.make_corrupt();
    let out = env.tmp.path().join("bad").join("poster.jpg");

    let r = make_thumbnail(&file, &out, 0.5, 160);
    assert!(r.is_err(), "expected Err, got {r:?}");
    assert!(!out.exists(), "output file should not exist after failure");
}

#[test]
fn register_happy_path_stores_row() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("game_dir").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("register ok");
    assert_eq!(id.len(), 26, "id = {id}");

    let row = lib.get_recording(&id).unwrap().expect("row exists");
    assert!(
        matches!(row.status, RecStatus::Ready),
        "status = {:?}",
        row.status
    );
    assert!(
        (2500..=3500).contains(&row.duration_ms),
        "duration_ms = {}",
        row.duration_ms
    );
    assert_eq!(row.file_name, "clip.mp4");
    assert_eq!(row.ext, "mp4");
    assert_eq!(row.rel_path, rel(&["game_dir", "clip.mp4"]));
    assert_eq!(row.game_id.as_deref(), Some("exe:testgame.exe"));
    assert_eq!(row.game_name.as_deref(), Some("testgame.exe"));
    assert_eq!(row.created_at_ms, CREATED_MS);
    assert!(row.size_bytes > 0);
    assert_eq!(row.root_path, root.to_string_lossy());
    assert!(
        matches!(row.kind, RecordingKind::Clip),
        "kind = {:?}",
        row.kind
    );

    let page = lib.list(&ListQuery::default()).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, id);
}

#[test]
fn register_writes_poster_thumbnail() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("game_dir").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("register ok");

    let poster = env.thumbs().join(&id).join("poster.jpg");
    let bytes = fs::read(&poster).expect("poster.jpg exists");
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "not a JPEG header");
}

#[test]
fn register_is_idempotent() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("game_dir").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let first = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("first register ok");
    let second = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("second register ok");

    assert_eq!(first, second);
    assert_eq!(list_len(&lib), 1);
}

#[test]
fn register_outside_root_is_not_under_root() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let other = tempfile::tempdir().unwrap();
    let file = other.path().join("clip.mp4");
    fs::copy(&src, &file).unwrap();
    let lib = Library::open_in_memory().unwrap();

    let r = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"));
    assert!(
        matches!(r, Err(CatalogError::NotUnderRoot { .. })),
        "got {r:?}"
    );
    assert_eq!(list_len(&lib), 0);
}

#[test]
fn register_unsupported_extension_is_rejected() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("x").join("clip.avi"));
    let lib = Library::open_in_memory().unwrap();

    let r = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"));
    match r {
        Err(CatalogError::UnsupportedExt(ref s)) => assert_eq!(s, "avi"),
        other => panic!("expected UnsupportedExt(\"avi\"), got {other:?}"),
    }
    assert_eq!(list_len(&lib), 0);
}

#[test]
fn register_missing_file_inserts_nothing() {
    let env = env_or_skip!();
    let root = env.root();
    let file = root.join("game_dir").join("gone.mp4");
    let lib = Library::open_in_memory().unwrap();

    let r = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"));
    assert!(matches!(r, Err(CatalogError::Io(_))), "got {r:?}");
    assert_eq!(list_len(&lib), 0);
}

#[test]
fn register_corrupt_file_fails_without_row() {
    let env = env_or_skip!();
    let root = env.root();
    let file = env.copy_to(
        &env.make_corrupt(),
        root.join("game_dir").join("corrupt.mp4"),
    );
    let lib = Library::open_in_memory().unwrap();

    let r = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"));
    assert!(matches!(r, Err(CatalogError::Io(_))), "got {r:?}");
    assert_eq!(list_len(&lib), 0);
}

#[test]
fn register_thumbnail_failure_is_nonfatal() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("game_dir").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();
    // A regular file where the thumbnails directory should be makes every poster write fail.
    fs::write(env.thumbs(), b"not a directory").expect("block thumbs dir");

    let id = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("register should succeed without a poster");

    assert!(lib.get_recording(&id).unwrap().is_some());
    assert!(
        !env.thumbs().join(&id).join("poster.jpg").exists(),
        "poster should not exist"
    );
}

#[test]
fn register_session_part_stores_session_fields() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("sess").join("part2.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_recording(
        &lib,
        &env.thumbs(),
        &root,
        &file,
        RegisterMeta {
            kind: RecordingKind::Session,
            game: "",
            session_id: Some(SESSION_ID.to_string()),
            part_index: Some(2),
            created_ms: CREATED_MS,
        },
    )
    .expect("register ok");

    let row = lib.get_recording(&id).unwrap().expect("row exists");
    assert!(
        matches!(row.kind, RecordingKind::Session),
        "kind = {:?}",
        row.kind
    );
    assert_eq!(row.session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(row.part_index, Some(2));
    assert_eq!(row.game_id, None);
    assert_eq!(row.game_name, None);
}

/// Registers `file` with `register_recording`, or `register_imported` when `import` is set.
fn register_with(
    env: &Env,
    lib: &Library,
    root: &Path,
    file: &Path,
    import: bool,
) -> Result<String, CatalogError> {
    let f = if import {
        register_imported
    } else {
        register_recording
    };
    f(lib, &env.thumbs(), root, file, meta("exe:testgame.exe"))
}

#[test]
fn register_imported_row_has_source_import() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("Clips").join("Game").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_with(&env, &lib, &root, &file, true).expect("import registers");
    let row = lib.get_recording(&id).unwrap().expect("row");
    assert_eq!(row.source, RecordingSource::Import);
    assert!(row.playable);
    assert_eq!(row.ext, "mp4");
}

#[test]
fn register_recording_row_has_source_recorded() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("game_dir").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_with(&env, &lib, &root, &file, false).expect("registers");
    let row = lib.get_recording(&id).unwrap().expect("row");
    assert_eq!(row.source, RecordingSource::Recorded);
    assert!(row.playable);
}

#[test]
fn register_m4v_and_mov_are_stored_as_mp4_with_real_name() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let lib = Library::open_in_memory().unwrap();
    for name in ["m.m4v", "q.MOV"] {
        let file = env.copy_to(&src, root.join("Game").join(name));
        let id = register_with(&env, &lib, &root, &file, true)
            .unwrap_or_else(|e| panic!("{name} must register: {e:?}"));
        let row = lib.get_recording(&id).unwrap().expect("row");
        assert_eq!(row.ext, "mp4", "{name}");
        assert_eq!(row.file_name, name, "{name} keeps its real name");
        assert!(
            row.rel_path.ends_with(name),
            "{name} keeps its real rel_path"
        );
        assert!(row.playable, "{name} is playable");
    }
    assert_eq!(list_len(&lib), 2);
}

#[test]
fn register_mkv_is_stored_not_playable() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("Game").join("clip.mkv"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_with(&env, &lib, &root, &file, true).expect("mkv registers");
    let row = lib.get_recording(&id).unwrap().expect("row");
    assert_eq!(row.ext, "mkv");
    assert!(!row.playable, "mkv is not playable in the webview");
    assert_eq!(row.source, RecordingSource::Import);
}

#[test]
fn register_webm_is_rejected_as_unsupported() {
    let env = env_or_skip!();
    let root = env.root();
    let src = env.make_mp4();
    let file = env.copy_to(&src, root.join("Game").join("clip.webm"));
    let lib = Library::open_in_memory().unwrap();

    match register_with(&env, &lib, &root, &file, true) {
        Err(CatalogError::UnsupportedExt(ref s)) => assert_eq!(s, "webm"),
        other => panic!("expected UnsupportedExt(\"webm\"), got {other:?}"),
    }
    assert_eq!(list_len(&lib), 0);
}

/// Generates a 3 s mp4 with video plus two AAC audio streams (stereo, 44.1 kHz) at
/// `<tmp>/media/two_audio.mp4`, titling audio stream 0 and 1 with `titles`.
/// The titles are written as per-stream `name` tags, which `stream_title` reads.
/// Stereo 44.1 kHz AAC track with a title, the layout the two-audio tests check.
fn stereo_44k(freq_hz: f32, title: &str) -> AudioSpec {
    let mut a = AudioSpec::titled(freq_hz, title);
    a.sample_rate = 44_100;
    a.channels = 2;
    a
}

fn make_two_audio_mp4(env: &Env, titles: [&str; 2]) -> PathBuf {
    let dir = env.tmp.path().join("media");
    fs::create_dir_all(&dir).unwrap();
    let out = dir.join("two_audio.mp4");
    let spec = MediaSpec::h264_aac(3.0)
        .with_video(320, 240, 25)
        .with_audio(vec![
            stereo_44k(440.0, titles[0]),
            stereo_44k(880.0, titles[1]),
        ]);
    clipcheck_testkit::write_mp4(&out, &spec).expect("testkit could not write two-audio mp4");
    out
}

#[test]
fn register_recording_stores_two_audio_track_rows() {
    let env = env_or_skip!();
    let root = env.root();
    let src = make_two_audio_mp4(&env, ["Game", "Mic"]);
    let file = env.copy_to(&src, root.join("Game").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();

    let id = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("register ok");

    let rows = lib.audio_tracks(&id).expect("audio_tracks ok");
    assert_eq!(rows.len(), 2, "rows = {rows:?}");

    assert_eq!(rows[0].track_index, 0);
    assert_eq!(rows[0].codec, "aac");
    assert_eq!(rows[0].channels, 2);
    assert_eq!(rows[0].sample_rate, 44100);
    assert_eq!(rows[0].bitrate_kbps, None);
    assert_eq!(rows[0].language, None);
    assert_eq!(rows[0].title.as_deref(), Some("Game"));

    assert_eq!(rows[1].track_index, 1);
    assert_eq!(rows[1].codec, "aac");
    assert_eq!(rows[1].channels, 2);
    assert_eq!(rows[1].sample_rate, 44100);
    assert_eq!(rows[1].bitrate_kbps, None);
    assert_eq!(rows[1].language, None);
    assert_eq!(rows[1].title.as_deref(), Some("Mic"));
}

#[test]
fn register_recording_truncates_long_audio_title_to_120_chars() {
    let env = env_or_skip!();
    let root = env.root();
    let long: String = "x".repeat(150);
    let src = make_two_audio_mp4(&env, [long.as_str(), "Mic"]);

    // Guard: the probe must actually report the full 150-char title, otherwise the
    // truncation assertion below would pass vacuously.
    let info = probe_media(&src).expect("probe ok");
    assert_eq!(
        info.audio_tracks[0]
            .title
            .as_deref()
            .map(|t| t.chars().count()),
        Some(150),
        "probe title = {:?}",
        info.audio_tracks[0].title
    );

    let file = env.copy_to(&src, root.join("Game").join("clip.mp4"));
    let lib = Library::open_in_memory().unwrap();
    let id = register_recording(&lib, &env.thumbs(), &root, &file, meta("exe:testgame.exe"))
        .expect("register ok");

    let rows = lib.audio_tracks(&id).expect("audio_tracks ok");
    assert_eq!(rows.len(), 2, "rows = {rows:?}");
    let t0 = rows[0].title.as_deref().expect("title stored");
    assert_eq!(t0.chars().count(), 120, "title len");
    assert_eq!(t0, &long[..120]);
    assert_eq!(rows[1].title.as_deref(), Some("Mic"));
}
