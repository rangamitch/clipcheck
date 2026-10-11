//! Tests for `clipcheck_library::fsops`: naming helpers, unique paths, and the
//! disk + database rename operation.

use std::fs;
use std::path::{Path, PathBuf};

use clipcheck_library::db::{Library, NewRecording, RecStatus, RecordingKind};
use clipcheck_library::fsops::{
    expand_template, rename_on_disk_and_db, sanitize_filename, unique_path, validate_user_stem,
    FsOpError, NameError, TemplateVars, MAX_FILENAME_UTF16, MAX_STEM_UTF16,
};
use tempfile::{tempdir, TempDir};

fn utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// sanitize_filename
// ---------------------------------------------------------------------------

#[test]
fn sanitize_replaces_each_illegal_char() {
    for c in ['\\', '/', ':', '*', '?', '"', '<', '>', '|'] {
        let input = format!("a{c}b");
        assert_eq!(sanitize_filename(&input), "a_b", "char {c:?}");
    }
}

#[test]
fn sanitize_removes_control_chars() {
    assert_eq!(sanitize_filename("a\u{0007}b\tc\nd"), "abcd");
}

#[test]
fn sanitize_trims_surrounding_whitespace() {
    assert_eq!(sanitize_filename("   hello world  "), "hello world");
}

#[test]
fn sanitize_removes_trailing_dots_and_spaces() {
    assert_eq!(sanitize_filename("name..."), "name");
    assert_eq!(sanitize_filename("name . ."), "name");
    assert_eq!(sanitize_filename("x. "), "x");
}

#[test]
fn sanitize_empty_or_dots_gives_untitled() {
    assert_eq!(sanitize_filename(""), "Untitled");
    assert_eq!(sanitize_filename("   "), "Untitled");
    assert_eq!(sanitize_filename("..."), "Untitled");
    assert_eq!(sanitize_filename(". . ."), "Untitled");
}

#[test]
fn sanitize_prefixes_reserved_names() {
    for name in [
        "CON", "PRN", "AUX", "NUL", "COM1", "LPT9", "com1", "lpt5", "nul",
    ] {
        assert_eq!(sanitize_filename(name), format!("_{name}"), "name {name}");
    }
}

#[test]
fn sanitize_reserved_names_with_extension() {
    assert_eq!(sanitize_filename("nul.txt"), "_nul.txt");
    assert_eq!(sanitize_filename("CON.mp4"), "_CON.mp4");
    assert_eq!(sanitize_filename("Lpt1.log"), "_Lpt1.log");
}

#[test]
fn sanitize_com0_is_not_reserved() {
    assert_eq!(sanitize_filename("COM0"), "COM0");
    assert_eq!(sanitize_filename("com0.txt"), "com0.txt");
}

#[test]
fn sanitize_caps_length_at_max() {
    let long = "a".repeat(300);
    let out = sanitize_filename(&long);
    assert_eq!(utf16(&out), MAX_FILENAME_UTF16);
    assert_eq!(MAX_FILENAME_UTF16, 200);
}

#[test]
fn sanitize_reserved_with_long_extension_stays_within_cap() {
    let input = format!("nul.{}", "x".repeat(300));
    let out = sanitize_filename(&input);
    assert!(utf16(&out) <= MAX_FILENAME_UTF16, "len {}", utf16(&out));
    assert!(out.starts_with("_nul."), "got {out}");
}

#[test]
fn sanitize_keeps_emoji_and_cjk() {
    assert_eq!(sanitize_filename("🎮 Zelda"), "🎮 Zelda");
    assert_eq!(sanitize_filename("ゼルダの伝説"), "ゼルダの伝説");
}

#[test]
fn sanitize_is_idempotent() {
    let inputs = [
        "a/b:c*d?e\"f<g>h|i",
        "CON",
        "CON.",
        "nul.txt",
        "COM1 ",
        "...",
        "  x. ",
        "",
        "Zelda: BotW",
        "🎮 ゼルダ",
        "a\u{0001}b",
        "NUL. .",
    ];
    for input in inputs {
        let once = sanitize_filename(input);
        let twice = sanitize_filename(&once);
        assert_eq!(once, twice, "input {input:?}");
    }
    let long = "b".repeat(300);
    assert_eq!(
        sanitize_filename(&sanitize_filename(&long)),
        sanitize_filename(&long)
    );
}

#[test]
fn sanitize_output_never_contains_illegal_chars() {
    let input = "x\\y/z:w*v?u\"t<s>r|q\u{0002}p";
    let out = sanitize_filename(input);
    for c in ['\\', '/', ':', '*', '?', '"', '<', '>', '|'] {
        assert!(!out.contains(c), "output {out:?} contains {c:?}");
    }
    assert!(!out.chars().any(|c| c.is_control()));
    assert_eq!(out, "x_y_z_w_v_u_t_s_r_qp");
}

// ---------------------------------------------------------------------------
// validate_user_stem
// ---------------------------------------------------------------------------

#[test]
fn validate_trims_and_returns_valid_name() {
    assert_eq!(validate_user_stem("  My Clip  ").unwrap(), "My Clip");
}

#[test]
fn validate_empty_and_blank_are_empty_error() {
    assert_eq!(validate_user_stem(""), Err(NameError::Empty));
    assert_eq!(validate_user_stem("   "), Err(NameError::Empty));
}

#[test]
fn validate_rejects_slash_as_illegal() {
    match validate_user_stem("a/b") {
        Err(NameError::IllegalChars(chars)) => assert!(chars.contains(&'/'), "{chars:?}"),
        other => panic!("expected IllegalChars, got {other:?}"),
    }
}

#[test]
fn validate_rejects_trailing_dot_or_space() {
    assert_eq!(
        validate_user_stem("foo."),
        Err(NameError::TrailingDotOrSpace)
    );
    assert_eq!(validate_user_stem("foo "), Ok("foo".to_string()));
}

#[test]
fn validate_rejects_reserved() {
    assert_eq!(validate_user_stem("CON"), Err(NameError::Reserved));
}

#[test]
fn validate_length_limit_is_120() {
    assert_eq!(MAX_STEM_UTF16, 120);
    let ok = "a".repeat(120);
    assert_eq!(validate_user_stem(&ok).unwrap(), ok);
    let too_long = "a".repeat(121);
    assert_eq!(
        validate_user_stem(&too_long),
        Err(NameError::TooLong { max: 120 })
    );
}

// ---------------------------------------------------------------------------
// expand_template
// ---------------------------------------------------------------------------

const TS_2024: i64 = 1_709_210_096_000; // 2024-02-29 12:34:56 UTC

fn clip_vars(game: &str, ts: i64) -> TemplateVars<'_> {
    TemplateVars {
        game,
        timestamp_ms: ts,
        kind: RecordingKind::Clip,
        part: None,
    }
}

#[test]
fn template_compact_date_pattern() {
    assert_eq!(
        expand_template(
            "{game} {yyyy-MM-dd HH-mm-ss} {kind}",
            &clip_vars("Elden Ring", TS_2024)
        ),
        "Elden Ring 2024-02-29 12-34-56 clip"
    );
}

#[test]
fn template_separate_date_tokens() {
    assert_eq!(
        expand_template(
            "{game} {yyyy}-{MM}-{dd} {HH}-{mm}-{ss} {kind}",
            &clip_vars("Elden Ring", TS_2024)
        ),
        "Elden Ring 2024-02-29 12-34-56 clip"
    );
}

#[test]
fn template_epoch_zero() {
    assert_eq!(
        expand_template("{yyyy-MM-dd HH-mm-ss}", &clip_vars("", 0)),
        "1970-01-01 00-00-00"
    );
}

#[test]
fn template_negative_timestamp() {
    assert_eq!(
        expand_template("{yyyy-MM-dd HH-mm-ss}", &clip_vars("", -1000)),
        "1969-12-31 23-59-59"
    );
}

#[test]
fn template_leap_day_2000() {
    assert_eq!(
        expand_template("{yyyy-MM-dd HH-mm-ss}", &clip_vars("", 951_782_400_000)),
        "2000-02-29 00-00-00"
    );
}

#[test]
fn template_part_some_and_none() {
    let with_part = TemplateVars {
        game: "G",
        timestamp_ms: 0,
        kind: RecordingKind::Session,
        part: Some(3),
    };
    assert_eq!(
        expand_template("{game} part {part}", &with_part),
        "G part 3"
    );

    let without = TemplateVars {
        part: None,
        ..with_part
    };
    assert_eq!(expand_template("{game} part {part}", &without), "G part");
}

#[test]
fn template_unknown_token_stays_literal() {
    assert_eq!(
        expand_template("{foo} {kind}", &clip_vars("G", 0)),
        "{foo} clip"
    );
}

#[test]
fn template_game_with_illegal_chars_is_sanitized() {
    assert_eq!(
        expand_template("{game} {kind}", &clip_vars("Zelda: BotW", 0)),
        "Zelda_ BotW clip"
    );
}

#[test]
fn template_empty_game_falls_back_to_clipcheck_name() {
    assert_eq!(
        expand_template("{game}", &clip_vars("", TS_2024)),
        "ClipCheck 2024-02-29 12-34-56"
    );
    let out = expand_template("{game}", &clip_vars("", TS_2024));
    assert!(out.starts_with("ClipCheck 20"), "got {out}");
}

#[test]
fn template_kind_session() {
    let vars = TemplateVars {
        game: "G",
        timestamp_ms: 0,
        kind: RecordingKind::Session,
        part: None,
    };
    assert_eq!(expand_template("{kind}", &vars), "session");
}

#[test]
fn template_slash_in_literal_text_becomes_underscore() {
    assert_eq!(
        expand_template("a/b {kind}", &clip_vars("G", 0)),
        "a_b clip"
    );
}

// ---------------------------------------------------------------------------
// unique_path
// ---------------------------------------------------------------------------

#[test]
fn unique_path_free_name_is_returned_as_is() {
    let dir = tempdir().unwrap();
    let p = unique_path(dir.path(), "stem", "mp4");
    assert_eq!(p, dir.path().join("stem.mp4"));
    assert_eq!(p.parent().unwrap(), dir.path());
}

#[test]
fn unique_path_adds_suffix_when_taken() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("stem.mp4"), b"x").unwrap();
    let p = unique_path(dir.path(), "stem", "mp4");
    assert_eq!(p, dir.path().join("stem (2).mp4"));
    assert_eq!(p.parent().unwrap(), dir.path());
}

#[test]
fn unique_path_skips_taken_suffixes() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("stem.mp4"), b"x").unwrap();
    fs::write(dir.path().join("stem (2).mp4"), b"x").unwrap();
    assert_eq!(
        unique_path(dir.path(), "stem", "mp4"),
        dir.path().join("stem (3).mp4")
    );
}

#[test]
fn unique_path_empty_ext_has_no_dot() {
    let dir = tempdir().unwrap();
    assert_eq!(
        unique_path(dir.path(), "README", ""),
        dir.path().join("README")
    );
    fs::write(dir.path().join("README"), b"x").unwrap();
    assert_eq!(
        unique_path(dir.path(), "README", ""),
        dir.path().join("README (2)")
    );
}

// ---------------------------------------------------------------------------
// rename_on_disk_and_db fixtures
// ---------------------------------------------------------------------------

const ORIGINAL: &[u8] = b"ORIGINAL-BYTES";

struct Fx {
    _dir: TempDir,
    root: PathBuf,
    lib: Library,
    id: String,
}

fn new_row(id: &str, root_id: i64, rel: &str, status: RecStatus) -> NewRecording {
    let file_name = rel.rsplit(['/', '\\']).next().unwrap().to_string();
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_string())
        .unwrap_or_default();
    NewRecording {
        id: id.to_string(),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id,
        rel_path: rel.to_string(),
        file_name,
        ext,
        size_bytes: ORIGINAL.len() as i64,
        file_mtime_ms: 0,
        created_at_ms: 0,
        added_at_ms: 0,
        duration_ms: 0,
        status,
        error_detail: None,
        end_reason: None,
        recovered: false,
        degraded: false,
        dropped_packets: 0,
        gap_ms: 0,
        needs_faststart: false,
    }
}

const MAIN_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OTHER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

/// Creates a root with `rel` on disk (when `write_file`) and one recording row for it.
fn fx(rel: &str, status: RecStatus, write_file: bool) -> Fx {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    if write_file {
        fs::write(&full, ORIGINAL).unwrap();
    }
    let lib = Library::open_in_memory().unwrap();
    let root_id = lib.ensure_storage_root(root.to_str().unwrap(), 0).unwrap();
    lib.insert_recording(&new_row(MAIN_ID, root_id, rel, status))
        .unwrap();
    Fx {
        _dir: dir,
        root,
        lib,
        id: MAIN_ID.to_string(),
    }
}

// ---------------------------------------------------------------------------
// rename_on_disk_and_db
// ---------------------------------------------------------------------------

#[test]
fn rename_normal_moves_file_and_updates_db() {
    let f = fx("clip.mp4", RecStatus::Ready, true);
    let out = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap();

    assert!(out.changed);
    assert_eq!(out.old_rel_path, "clip.mp4");
    assert_eq!(out.row.file_name, "New.mp4");
    assert_eq!(out.row.rel_path, "New.mp4");

    assert!(!f.root.join("clip.mp4").exists());
    assert_eq!(fs::read(f.root.join("New.mp4")).unwrap(), ORIGINAL);
    assert_eq!(dir_names(&f.root), vec!["New.mp4".to_string()]);

    let db = f.lib.get_recording(&f.id).unwrap().expect("row exists");
    assert_eq!(db.file_name, "New.mp4");
    assert_eq!(db.rel_path, "New.mp4");
}

#[test]
fn rename_in_subfolder_keeps_directory() {
    let f = fx("Game/clip.mp4", RecStatus::Ready, true);
    let out = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap();

    assert!(out.changed);
    assert_eq!(out.row.rel_path, "Game/New.mp4");
    assert_eq!(out.row.file_name, "New.mp4");
    assert!(!f.root.join("Game/clip.mp4").exists());
    assert_eq!(fs::read(f.root.join("Game/New.mp4")).unwrap(), ORIGINAL);

    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.rel_path, "Game/New.mp4");
}

#[test]
fn rename_same_name_is_no_op() {
    let f = fx("clip.mp4", RecStatus::Ready, true);
    let out = rename_on_disk_and_db(&f.lib, &f.id, "clip", &f.root).unwrap();

    assert!(!out.changed);
    assert_eq!(out.row.file_name, "clip.mp4");
    assert_eq!(fs::read(f.root.join("clip.mp4")).unwrap(), ORIGINAL);
    assert_eq!(dir_names(&f.root), vec!["clip.mp4".to_string()]);
}

#[test]
fn rename_case_only_changes_disk_name() {
    let f = fx("clip.mp4", RecStatus::Ready, true);
    let out = rename_on_disk_and_db(&f.lib, &f.id, "Clip", &f.root).unwrap();

    assert!(out.changed);
    assert_eq!(out.row.file_name, "Clip.mp4");
    // Check the exact on-disk spelling; exists() is case-insensitive on Windows.
    assert_eq!(dir_names(&f.root), vec!["Clip.mp4".to_string()]);
    assert_eq!(fs::read(f.root.join("Clip.mp4")).unwrap(), ORIGINAL);

    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.file_name, "Clip.mp4");
    assert_eq!(db.rel_path, "Clip.mp4");
}

#[test]
fn rename_collision_with_disk_only_file_is_refused() {
    let f = fx("clip.mp4", RecStatus::Ready, true);
    fs::write(f.root.join("New.mp4"), b"OTHER").unwrap();

    let err = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap_err();
    assert!(
        matches!(err, FsOpError::Collision(ref n) if n == "New.mp4"),
        "got {err:?}"
    );

    assert_eq!(fs::read(f.root.join("clip.mp4")).unwrap(), ORIGINAL);
    assert_eq!(fs::read(f.root.join("New.mp4")).unwrap(), b"OTHER");
    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.file_name, "clip.mp4");
    assert_eq!(db.rel_path, "clip.mp4");
}

#[test]
fn rename_recording_status_is_busy() {
    let f = fx("clip.mp4", RecStatus::Recording, true);
    let err = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap_err();
    assert!(matches!(err, FsOpError::Busy), "got {err:?}");
    assert!(f.root.join("clip.mp4").exists());
}

#[test]
fn rename_missing_status_is_missing_on_disk() {
    let f = fx("clip.mp4", RecStatus::Missing, false);
    let err = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap_err();
    assert!(matches!(err, FsOpError::MissingOnDisk), "got {err:?}");
}

#[test]
fn rename_unknown_id_is_not_found() {
    let f = fx("clip.mp4", RecStatus::Ready, true);
    let err =
        rename_on_disk_and_db(&f.lib, "01ZZZZZZZZZZZZZZZZZZZZZZZZ", "New", &f.root).unwrap_err();
    assert!(matches!(err, FsOpError::NotFound), "got {err:?}");
}

#[test]
fn rename_invalid_stem_is_name_error_and_nothing_changes() {
    let f = fx("clip.mp4", RecStatus::Ready, true);

    let err = rename_on_disk_and_db(&f.lib, &f.id, "a/b", &f.root).unwrap_err();
    assert!(
        matches!(err, FsOpError::Name(NameError::IllegalChars(_))),
        "got {err:?}"
    );

    let err = rename_on_disk_and_db(&f.lib, &f.id, "CON", &f.root).unwrap_err();
    assert!(
        matches!(err, FsOpError::Name(NameError::Reserved)),
        "got {err:?}"
    );

    assert_eq!(dir_names(&f.root), vec!["clip.mp4".to_string()]);
    assert_eq!(fs::read(f.root.join("clip.mp4")).unwrap(), ORIGINAL);
    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.file_name, "clip.mp4");
}

#[test]
fn rename_file_missing_on_disk_is_error_and_db_unchanged() {
    let f = fx("clip.mp4", RecStatus::Ready, false);
    let err = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap_err();
    assert!(
        matches!(err, FsOpError::MissingOnDisk | FsOpError::Io(_)),
        "got {err:?}"
    );
    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.file_name, "clip.mp4");
    assert_eq!(db.rel_path, "clip.mp4");
    assert!(dir_names(&f.root).is_empty());
}

#[test]
fn rename_db_failure_rolls_back_file_rename() {
    let f = fx("Game/clip.mp4", RecStatus::Ready, true);
    // A second row already owns the target rel_path under the same root. The
    // UNIQUE(root_id, rel_path) constraint makes the DB update fail.
    let root_id = f.lib.get_recording(&f.id).unwrap().unwrap().root_id;
    f.lib
        .insert_recording(&new_row(
            OTHER_ID,
            root_id,
            "Game/New.mp4",
            RecStatus::Ready,
        ))
        .unwrap();

    let err = rename_on_disk_and_db(&f.lib, &f.id, "New", &f.root).unwrap_err();
    assert!(matches!(err, FsOpError::DbRolledBack(_)), "got {err:?}");

    // File is back at its original path with original content.
    assert_eq!(
        dir_names(&f.root.join("Game")),
        vec!["clip.mp4".to_string()]
    );
    assert_eq!(fs::read(f.root.join("Game/clip.mp4")).unwrap(), ORIGINAL);

    let db = f.lib.get_recording(&f.id).unwrap().unwrap();
    assert_eq!(db.file_name, "clip.mp4");
    assert_eq!(db.rel_path, "Game/clip.mp4");
}
