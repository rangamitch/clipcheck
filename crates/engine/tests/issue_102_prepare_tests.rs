//! Regression tests for issue #102: probing and poster writing happen outside the library,
//! and `commit_prepared` resolves a race where the same file is registered twice.
//!
//! Native test media from `clipcheck-testkit` generates the inputs. Each test
//! builds its own tempdir and in-memory library. Missing native media skips each test
//! visibly (see `common::skip_or_fail`).

#[path = "common/mod.rs"]
mod common;

use clipcheck_testkit::MediaSpec;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use clipcheck_engine::catalog::{
    commit_prepared, find_registered, prepare_recording, register_recording, CatalogError,
    RegisterMeta,
};
use clipcheck_library::db::RecordingSource;
use clipcheck_library::{Library, ListQuery, RecStatus, RecordingKind};
use tempfile::TempDir;

const CREATED_MS: i64 = 1_700_000_000_000;

/// `true` when native test media is available, or `false` after reporting a visible skip.
fn media_or_skip(test: &str) -> bool {
    let ok = common::vendored_tools().is_some();
    if !ok {
        common::skip_or_fail(&format!("{test}: native test media unavailable"));
    }
    ok
}

/// The 3 s source clip, encoded once per test binary. Tests copy it.
fn cached_source_mp4() -> &'static Path {
    static SRC: OnceLock<PathBuf> = OnceLock::new();
    SRC.get_or_init(|| {
        let out = common::fixture_dir().join("issue102_source.mp4");
        let spec = MediaSpec::h264_aac(3.0).with_video(320, 240, 25);
        clipcheck_testkit::write_mp4(&out, &spec).expect("testkit could not write test mp4");
        out
    })
}

/// Per-test environment: a tempdir.
struct Env {
    tmp: TempDir,
}

impl Env {
    fn new() -> Self {
        Env {
            tmp: tempfile::tempdir().expect("tempdir"),
        }
    }

    /// Storage root (created).
    fn root(&self) -> PathBuf {
        let p = self.tmp.path().join("root");
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Thumbnail directory (not created; the code under test must create what it needs).
    fn thumbs(&self) -> PathBuf {
        self.tmp.path().join("thumbs")
    }

    /// Copies the source clip to `dest` (creating parent directories) and returns `dest`.
    fn copy_source_to(&self, dest: PathBuf) -> PathBuf {
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(cached_source_mp4(), &dest).expect("copy source fixture");
        dest
    }
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

fn list_len(lib: &Library) -> usize {
    lib.list(&ListQuery::default()).expect("list").items.len()
}

/// Counts every file named `name` below `dir` (zero when `dir` does not exist).
fn count_named(dir: &Path, name: &str) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .map(|e| e.expect("dir entry").path())
        .map(|p| {
            if p.is_dir() {
                count_named(&p, name)
            } else {
                usize::from(p.file_name().is_some_and(|n| n == name))
            }
        })
        .sum()
}

/// Counts every file below `dir` (zero when `dir` does not exist).
fn count_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .map(|e| e.expect("dir entry").path())
        .map(|p| if p.is_dir() { count_files(&p) } else { 1 })
        .sum()
}

#[test]
fn issue102_prepare_creates_no_row_and_commit_matches_register() {
    let test = "issue102_prepare_creates_no_row_and_commit_matches_register";
    if !media_or_skip(test) {
        return;
    }
    let env = Env::new();
    let root = env.root();
    let lib = Library::open_in_memory().unwrap();

    // Reference: one-shot registration of an identical second file.
    let ref_file = env.copy_source_to(root.join("ref").join("reference.mp4"));
    let ref_id = register_recording(
        &lib,
        &env.tmp.path().join("ref_thumbs"),
        &root,
        &ref_file,
        meta("exe:testgame.exe"),
    )
    .expect("register reference ok");
    let ref_row = lib.get_recording(&ref_id).unwrap().expect("reference row");
    let ref_count_before = list_len(&lib);
    assert_eq!(ref_count_before, 1, "reference registration row count");

    // A fresh library; prepare must not add a row.
    let lib2 = Library::open_in_memory().unwrap();
    let file = env.copy_source_to(root.join("game_dir").join("clip.mp4"));
    let prepared = prepare_recording(&env.thumbs(), &root, &file).expect("prepare ok");
    assert_eq!(
        list_len(&lib2),
        0,
        "prepare_recording must not insert a library row"
    );

    let id = commit_prepared(
        &lib2,
        prepared,
        meta("exe:testgame.exe"),
        RecordingSource::Recorded,
    )
    .expect("commit ok");
    assert_eq!(id.len(), 26, "id = {id}");
    assert_eq!(list_len(&lib2), 1, "one row after commit");

    let row = lib2
        .get_recording(&id)
        .unwrap()
        .expect("committed row exists");
    assert!(
        matches!(row.status, RecStatus::Ready),
        "status = {:?}",
        row.status
    );
    assert!(row.duration_ms > 0, "duration_ms = {}", row.duration_ms);
    assert!(
        (row.duration_ms - ref_row.duration_ms).abs() <= 50,
        "committed duration {} vs register_recording duration {}",
        row.duration_ms,
        ref_row.duration_ms
    );
    assert_eq!(row.game_id.as_deref(), Some("exe:testgame.exe"));
    assert_eq!(row.created_at_ms, CREATED_MS);
}

#[test]
fn issue102_find_registered_none_before_and_some_after_commit() {
    let test = "issue102_find_registered_none_before_and_some_after_commit";
    if !media_or_skip(test) {
        return;
    }
    let env = Env::new();
    let root = env.root();
    let lib = Library::open_in_memory().unwrap();
    let file = env.copy_source_to(root.join("clip.mp4"));

    let before = find_registered(&lib, &root, &file).expect("find before");
    assert!(
        before.is_none(),
        "expected None before commit, got {before:?}"
    );

    let prepared = prepare_recording(&env.thumbs(), &root, &file).expect("prepare ok");
    let id =
        commit_prepared(&lib, prepared, meta(""), RecordingSource::Recorded).expect("commit ok");

    let after = find_registered(&lib, &root, &file).expect("find after");
    assert_eq!(after.as_deref(), Some(id.as_str()), "find after commit");
}

#[test]
fn issue102_double_commit_returns_same_id_and_discards_second_poster() {
    let test = "issue102_double_commit_returns_same_id_and_discards_second_poster";
    if !media_or_skip(test) {
        return;
    }
    let env = Env::new();
    let root = env.root();
    let lib = Library::open_in_memory().unwrap();
    let file = env.copy_source_to(root.join("clip.mp4"));
    let thumbs = env.thumbs();

    // Two attempts both probe and write a poster before either commits.
    let first = prepare_recording(&thumbs, &root, &file).expect("prepare first ok");
    let second = prepare_recording(&thumbs, &root, &file).expect("prepare second ok");
    assert_eq!(
        count_named(&thumbs, "poster.jpg"),
        2,
        "both attempts should have written a poster before any commit"
    );

    let id_first =
        commit_prepared(&lib, first, meta(""), RecordingSource::Recorded).expect("commit first ok");
    let id_second = commit_prepared(&lib, second, meta(""), RecordingSource::Recorded)
        .expect("commit second ok");

    assert_eq!(id_first, id_second, "both commits must return the same id");
    assert_eq!(list_len(&lib), 1, "only one row must exist");

    assert_eq!(
        count_named(&thumbs, "poster.jpg"),
        1,
        "the losing attempt's poster must be removed"
    );
    assert!(
        thumbs.join(&id_first).join("poster.jpg").is_file(),
        "the winning attempt's poster must remain under thumbs/{id_first}"
    );
    assert_eq!(count_files(&thumbs), 1, "no other files in thumbs_dir");
}

#[test]
fn issue102_prepare_missing_or_unsupported_writes_nothing() {
    let test = "issue102_prepare_missing_or_unsupported_writes_nothing";
    if !media_or_skip(test) {
        return;
    }
    let env = Env::new();
    let root = env.root();
    let thumbs = env.thumbs();

    let missing = root.join("missing.mp4");
    let r = prepare_recording(&thumbs, &root, &missing);
    assert!(r.is_err(), "missing file: expected Err, got {r:?}");
    assert_eq!(count_files(&thumbs), 0, "missing file must write no poster");
    assert!(!thumbs.exists(), "missing file must not create thumbs_dir");

    let unsupported = env.copy_source_to(root.join("clip.txt"));
    let r = prepare_recording(&thumbs, &root, &unsupported);
    assert!(
        matches!(r, Err(CatalogError::UnsupportedExt(_))),
        "unsupported extension: expected UnsupportedExt, got {r:?}"
    );
    assert_eq!(
        count_files(&thumbs),
        0,
        "unsupported ext must write no poster"
    );
    assert!(
        !thumbs.exists(),
        "unsupported ext must not create thumbs_dir"
    );
}

#[test]
fn issue102_prepare_does_not_need_library_lock() {
    let test = "issue102_prepare_does_not_need_library_lock";
    if !media_or_skip(test) {
        return;
    }
    let env = Env::new();
    let root = env.root();
    let file = env.copy_source_to(root.join("clip.mp4"));
    let lib = Mutex::new(Library::open_in_memory().unwrap());

    // The guard is held for the whole prepare call. prepare_recording takes no library
    // argument, so this must complete without deadlock and without touching any row.
    let guard = lib.lock().expect("lock library");
    let prepared = prepare_recording(&env.thumbs(), &root, &file)
        .expect("prepare ok while library lock is held");
    assert_eq!(list_len(&guard), 0, "prepare must not insert a row");
    drop(guard);

    let guard = lib.lock().expect("lock library");
    commit_prepared(&guard, prepared, meta(""), RecordingSource::Recorded).expect("commit ok");
    assert_eq!(list_len(&guard), 1);
}
