//! Tests for issue #149: leftover case-only rename temp files.
//!
//! Covers `fsops::recover_rename_temps` and the public case-only rename path through
//! `rename_on_disk_and_db`. The forced second-step failure cases need the private
//! `move_file_with` injection point, which is not reachable from an integration test.

use std::fs;
use std::path::{Path, PathBuf};

use clipcheck_library::db::{Library, NewRecording, RecStatus, RecordingKind};
use clipcheck_library::fsops::{recover_rename_temps, rename_on_disk_and_db, Recovered};
use tempfile::{tempdir, TempDir};

const BYTES: &[u8] = b"\x00\x01RECORDING\xff\xfe-payload";

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn temp_name(original: &str, millis: u64) -> String {
    format!("{original}.clipcheck-rename-{millis}.tmp")
}

// ---------------------------------------------------------------------------
// recover_rename_temps
// ---------------------------------------------------------------------------

#[test]
fn recover_restores_free_original_name() {
    let dir = tempdir().unwrap();
    let temp = dir.path().join(temp_name("Clip.mp4", 1_700_000_000_000));
    fs::write(&temp, BYTES).unwrap();

    let recovered = recover_rename_temps(dir.path());
    assert_eq!(recovered.len(), 1, "got {recovered:?}");
    match &recovered[0] {
        Recovered::Restored {
            temp: t,
            original: o,
        } => {
            assert_eq!(t, &temp);
            assert_eq!(o, &dir.path().join("Clip.mp4"));
        }
        other => panic!("expected Restored, got {other:?}"),
    }
    assert!(!temp.exists());
    assert_eq!(names_in(dir.path()), vec!["Clip.mp4".to_string()]);
    assert_eq!(fs::read(dir.path().join("Clip.mp4")).unwrap(), BYTES);
}

#[test]
fn recover_restores_unicode_and_spaces_in_original_name() {
    let dir = tempdir().unwrap();
    let original = "Spiel Übersicht 🎬 clip 1.mkv";
    let temp = dir.path().join(temp_name(original, 42));
    fs::write(&temp, BYTES).unwrap();

    let recovered = recover_rename_temps(dir.path());
    assert!(
        matches!(recovered.as_slice(), [Recovered::Restored { .. }]),
        "got {recovered:?}"
    );
    assert_eq!(names_in(dir.path()), vec![original.to_string()]);
    assert_eq!(fs::read(dir.path().join(original)).unwrap(), BYTES);
}

#[test]
fn recover_restores_multiple_temps_for_different_originals() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(temp_name("a.mp4", 1)), b"A").unwrap();
    fs::write(dir.path().join(temp_name("b.mkv", 2)), b"B").unwrap();

    let recovered = recover_rename_temps(dir.path());
    assert_eq!(recovered.len(), 2, "got {recovered:?}");
    assert!(recovered
        .iter()
        .all(|r| matches!(r, Recovered::Restored { .. })));
    assert_eq!(
        names_in(dir.path()),
        vec!["a.mp4".to_string(), "b.mkv".to_string()]
    );
    assert_eq!(fs::read(dir.path().join("a.mp4")).unwrap(), b"A");
    assert_eq!(fs::read(dir.path().join("b.mkv")).unwrap(), b"B");
}

#[test]
fn recover_two_temps_for_same_original_restores_one_and_blocks_other() {
    let dir = tempdir().unwrap();
    let t1 = dir.path().join(temp_name("Clip.mp4", 100));
    let t2 = dir.path().join(temp_name("Clip.mp4", 200));
    fs::write(&t1, b"first").unwrap();
    fs::write(&t2, b"second").unwrap();

    let recovered = recover_rename_temps(dir.path());
    assert_eq!(recovered.len(), 2, "got {recovered:?}");
    let restored = recovered
        .iter()
        .filter(|r| matches!(r, Recovered::Restored { .. }))
        .count();
    let blocked = recovered
        .iter()
        .filter(|r| matches!(r, Recovered::Blocked { .. }))
        .count();
    assert_eq!(restored, 1, "got {recovered:?}");
    assert_eq!(blocked, 1, "got {recovered:?}");

    // Whichever temp won, the original holds its bytes and the loser is still on disk.
    let original = dir.path().join("Clip.mp4");
    let winner = fs::read(&original).unwrap();
    assert!(winner == b"first" || winner == b"second");
    let remaining: Vec<PathBuf> = [&t1, &t2]
        .into_iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();
    assert_eq!(remaining.len(), 1, "exactly one temp must remain");
    let loser_bytes = fs::read(&remaining[0]).unwrap();
    assert_ne!(loser_bytes, winner);
    assert_eq!(names_in(dir.path()).len(), 2);
}

#[test]
fn recover_blocked_when_original_exists_and_never_deletes() {
    let dir = tempdir().unwrap();
    let temp = dir.path().join(temp_name("Clip.mp4", 5));
    fs::write(&temp, b"temp-bytes").unwrap();
    fs::write(dir.path().join("Clip.mp4"), b"occupant").unwrap();

    let recovered = recover_rename_temps(dir.path());
    assert!(
        matches!(recovered.as_slice(), [Recovered::Blocked { .. }]),
        "got {recovered:?}"
    );
    assert_eq!(fs::read(&temp).unwrap(), b"temp-bytes");
    assert_eq!(fs::read(dir.path().join("Clip.mp4")).unwrap(), b"occupant");
    assert_eq!(names_in(dir.path()).len(), 2);
}

#[test]
fn recover_ignores_non_matching_names_and_leaves_them_untouched() {
    let dir = tempdir().unwrap();
    // Names that must NOT be treated as rename temps.
    let ignored = [
        "foo.tmp",
        ".clipcheck-rename-.tmp",
        "x.clipcheck-rename-abc.tmp",
        "x.clipcheck-rename-12.tmp.bak",
        "x.clipcheck-rename-.tmp",
        "x.clipcheck-rename-12",
        "plain.mp4",
    ];
    for name in ignored {
        fs::write(dir.path().join(name), b"keep").unwrap();
    }

    assert!(recover_rename_temps(dir.path()).is_empty());
    assert_eq!(names_in(dir.path()), {
        let mut v: Vec<String> = ignored.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    });
    for name in ignored {
        assert_eq!(fs::read(dir.path().join(name)).unwrap(), b"keep", "{name}");
    }
}

#[test]
fn recover_ignores_subdirectories_and_nested_matches() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("Game");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join(temp_name("Clip.mp4", 9)), b"nested").unwrap();
    // A directory whose name looks like a temp must be ignored too.
    fs::create_dir(dir.path().join(temp_name("Dir.mp4", 3))).unwrap();

    assert!(recover_rename_temps(dir.path()).is_empty());
    assert!(sub.join(temp_name("Clip.mp4", 9)).exists());
    assert!(!dir.path().join("Clip.mp4").exists());
    assert!(!dir.path().join("Dir.mp4").exists());
}

#[test]
fn recover_missing_or_unreadable_dir_returns_empty() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    assert!(recover_rename_temps(&missing).is_empty());

    // A regular file is not a readable directory.
    let file = dir.path().join("plain.txt");
    fs::write(&file, b"x").unwrap();
    assert!(recover_rename_temps(&file).is_empty());
    assert_eq!(fs::read(&file).unwrap(), b"x");
}

#[test]
fn recover_empty_dir_returns_empty() {
    let dir = tempdir().unwrap();
    assert!(recover_rename_temps(dir.path()).is_empty());
}

#[test]
fn recover_is_idempotent_after_restore() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(temp_name("Clip.mp4", 7)), BYTES).unwrap();
    assert_eq!(recover_rename_temps(dir.path()).len(), 1);
    assert!(recover_rename_temps(dir.path()).is_empty());
    assert_eq!(fs::read(dir.path().join("Clip.mp4")).unwrap(), BYTES);
}

// ---------------------------------------------------------------------------
// Case-only rename through the public API
// ---------------------------------------------------------------------------

struct Fx {
    _dir: TempDir,
    root: PathBuf,
    lib: Library,
    id: String,
}

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn row(rel: &str) -> NewRecording {
    let file_name = rel.rsplit(['/', '\\']).next().unwrap().to_string();
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_string())
        .unwrap_or_default();
    NewRecording {
        id: ID.to_string(),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id: 0,
        rel_path: rel.to_string(),
        file_name,
        ext,
        size_bytes: BYTES.len() as i64,
        file_mtime_ms: 0,
        created_at_ms: 0,
        added_at_ms: 0,
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

fn fx(rel: &str) -> Fx {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&full, BYTES).unwrap();
    let lib = Library::open_in_memory().unwrap();
    let root_id = lib.ensure_storage_root(root.to_str().unwrap(), 0).unwrap();
    let mut r = row(rel);
    r.root_id = root_id;
    lib.insert_recording(&r).unwrap();
    Fx {
        _dir: dir,
        root,
        lib,
        id: ID.to_string(),
    }
}

#[test]
fn case_only_rename_leaves_no_temp_file() {
    let f = fx("clip.mp4");
    let out = rename_on_disk_and_db(&f.lib, &f.id, "Clip", &f.root).unwrap();
    assert!(out.changed);

    assert_eq!(names_in(&f.root), vec!["Clip.mp4".to_string()]);
    assert_eq!(fs::read(f.root.join("Clip.mp4")).unwrap(), BYTES);
    assert!(recover_rename_temps(&f.root).is_empty());

    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.file_name, "Clip.mp4");
    assert_eq!(db.rel_path, "Clip.mp4");
}

#[test]
fn case_only_rename_in_subfolder_leaves_no_temp_file() {
    let f = fx("Game/Clip.mkv");
    let out = rename_on_disk_and_db(&f.lib, &f.id, "clip", &f.root).unwrap();
    assert!(out.changed);

    assert_eq!(names_in(&f.root.join("Game")), vec!["clip.mkv".to_string()]);
    assert_eq!(names_in(&f.root), vec!["Game".to_string()]);
    assert_eq!(fs::read(f.root.join("Game/clip.mkv")).unwrap(), BYTES);
    assert!(recover_rename_temps(&f.root.join("Game")).is_empty());

    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.rel_path, "Game/clip.mkv");
}

#[test]
fn case_only_rename_with_unicode_leaves_no_temp_file() {
    let f = fx("ünï clip.mp4");
    let out = rename_on_disk_and_db(&f.lib, &f.id, "ÜNÏ CLIP", &f.root).unwrap();
    assert!(out.changed);

    assert_eq!(names_in(&f.root), vec!["ÜNÏ CLIP.mp4".to_string()]);
    assert_eq!(fs::read(f.root.join("ÜNÏ CLIP.mp4")).unwrap(), BYTES);
    assert!(recover_rename_temps(&f.root).is_empty());
}

#[test]
fn non_case_only_rename_leaves_no_temp_file() {
    let f = fx("clip.mp4");
    let out = rename_on_disk_and_db(&f.lib, &f.id, "other", &f.root).unwrap();
    assert!(out.changed);

    assert_eq!(names_in(&f.root), vec!["other.mp4".to_string()]);
    assert_eq!(fs::read(f.root.join("other.mp4")).unwrap(), BYTES);
    assert!(recover_rename_temps(&f.root).is_empty());
}
