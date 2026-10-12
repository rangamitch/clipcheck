//! Public-API tests for `clipcheck_ui_lib::editor::paths` (pure path and naming helpers).
//!
//! No test reads the real LOCALAPPDATA: the only env-dependent case calls `editor_root(None)`,
//! which falls back to `std::env::temp_dir()`, and only checks the suffix and the temp prefix.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use clipcheck_ui_lib::editor::paths::{
    autosave_file, cache_dir, editor_root, export_dir, is_inside, peaks_file, project_file,
    projects_dir, sanitize_stem, unique_output_path,
};

const ULID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn never_exists(_: &Path) -> bool {
    false
}

// ---- editor_root / projects_dir ----------------------------------------------------------------

#[test]
fn editor_root_uses_local_app_data() {
    let root = editor_root(Some(r"C:\Users\me\AppData\Local"));
    assert_eq!(
        root,
        PathBuf::from(r"C:\Users\me\AppData\Local\ClipCheck\editor")
    );
}

#[test]
fn editor_root_trims_local_app_data() {
    let root = editor_root(Some("  C:\\Users\\me\\AppData\\Local  "));
    assert_eq!(
        root,
        PathBuf::from(r"C:\Users\me\AppData\Local\ClipCheck\editor")
    );
}

#[test]
fn editor_root_blank_local_app_data_falls_back_to_temp() {
    let root = editor_root(Some("   "));
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(root.ends_with(Path::new("ClipCheck").join("editor")));
}

#[test]
fn editor_root_empty_local_app_data_falls_back_to_temp() {
    let root = editor_root(Some(""));
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(root.ends_with(Path::new("ClipCheck").join("editor")));
}

#[test]
fn editor_root_none_falls_back_to_temp() {
    let root = editor_root(None);
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(root.ends_with(Path::new("ClipCheck").join("editor")));
}

#[test]
fn editor_root_is_absolute_when_local_app_data_is() {
    assert!(editor_root(Some(r"D:\Data")).is_absolute());
}

#[test]
fn projects_dir_is_projects_child_of_root() {
    let root = Path::new(r"D:\Data\ClipCheck\editor");
    assert_eq!(
        projects_dir(root),
        PathBuf::from(r"D:\Data\ClipCheck\editor\projects")
    );
}

// ---- project_file / autosave_file id validation -----------------------------------------------

#[test]
fn project_file_accepts_ulid() {
    let dir = Path::new(r"D:\ed\projects");
    let p = project_file(dir, ULID).expect("ulid accepted");
    assert_eq!(p, dir.join(format!("{ULID}.ccproj")));
}

#[test]
fn project_file_rejects_parent_traversal() {
    assert_eq!(project_file(Path::new(r"D:\ed\projects"), "../x"), None);
}

#[test]
fn project_file_rejects_traversal_inside_long_id() {
    assert_eq!(
        project_file(Path::new(r"D:\ed\projects"), "01ARZ3..\\..\\evil"),
        None
    );
}

#[test]
fn project_file_rejects_separators() {
    assert_eq!(project_file(Path::new("p"), "01ARZ3NDEK/TSV4RRFF"), None);
    assert_eq!(project_file(Path::new("p"), "01ARZ3NDEK\\TSV4RRFF"), None);
}

#[test]
fn project_file_rejects_too_short() {
    assert_eq!(project_file(Path::new("p"), "abcdefghi"), None); // 9 chars
}

#[test]
fn project_file_accepts_minimum_length() {
    assert!(project_file(Path::new("p"), "abcdefghij").is_some()); // 10 chars
}

#[test]
fn project_file_accepts_maximum_length() {
    let id = "a".repeat(40);
    assert!(project_file(Path::new("p"), &id).is_some());
}

#[test]
fn project_file_rejects_too_long() {
    let id = "a".repeat(41);
    assert_eq!(project_file(Path::new("p"), &id), None);
}

#[test]
fn project_file_rejects_empty() {
    assert_eq!(project_file(Path::new("p"), ""), None);
}

#[test]
fn project_file_rejects_unicode() {
    assert_eq!(project_file(Path::new("p"), "01ARZ3NDEKTSVé4RR"), None);
}

#[test]
fn project_file_rejects_whitespace_and_dots() {
    assert_eq!(project_file(Path::new("p"), "01ARZ3NDEK TSV4RR"), None);
    assert_eq!(project_file(Path::new("p"), "01ARZ3NDEK.TSV4RR"), None);
}

#[test]
fn autosave_file_accepts_ulid() {
    let dir = Path::new(r"D:\ed\projects");
    assert_eq!(
        autosave_file(dir, ULID),
        Some(dir.join(format!("{ULID}.autosave")))
    );
}

#[test]
fn autosave_file_rejects_bad_ids() {
    let dir = Path::new(r"D:\ed\projects");
    assert_eq!(autosave_file(dir, "../x"), None);
    assert_eq!(autosave_file(dir, "short"), None);
    assert_eq!(autosave_file(dir, &"b".repeat(41)), None);
    assert_eq!(autosave_file(dir, "01ARZ3NDEKTSVÄ4RR"), None);
}

#[test]
fn autosave_and_project_names_differ_only_in_extension() {
    let dir = Path::new("p");
    let a = project_file(dir, ULID).expect("some");
    let b = autosave_file(dir, ULID).expect("some");
    assert_eq!(a.with_extension(""), b.with_extension(""));
    assert_ne!(a, b);
}

// ---- cache_dir ----------------------------------------------------------------------------------

#[test]
fn cache_dir_default_is_root_cache() {
    let root = Path::new(r"D:\ed");
    assert_eq!(cache_dir(root, ""), PathBuf::from(r"D:\ed\cache"));
}

#[test]
fn cache_dir_blank_setting_is_default() {
    let root = Path::new(r"D:\ed");
    assert_eq!(cache_dir(root, "   \t "), PathBuf::from(r"D:\ed\cache"));
}

#[test]
fn cache_dir_override_wins_and_is_trimmed() {
    let root = Path::new(r"D:\ed");
    assert_eq!(cache_dir(root, "  E:\\Cache  "), PathBuf::from(r"E:\Cache"));
}

// ---- peaks_file ---------------------------------------------------------------------------------

#[test]
fn peaks_file_format_is_size_mtime_stream_buckets() {
    let p = peaks_file(Path::new(r"C:\c"), 1234, 1_700_000_000_000, 1, 800);
    assert_eq!(p, PathBuf::from(r"C:\c\peaks\1234-1700000000000-1-800.pk"));
}

#[test]
fn peaks_file_negative_mtime_keeps_sign() {
    let p = peaks_file(Path::new(r"C:\c"), 10, -5, 0, 4);
    assert_eq!(p, PathBuf::from(r"C:\c\peaks\10--5-0-4.pk"));
}

#[test]
fn peaks_file_max_size_formats_fully() {
    let p = peaks_file(Path::new("c"), u64::MAX, i64::MIN, u32::MAX, 0);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(name, format!("{}-{}-{}-0.pk", u64::MAX, i64::MIN, u32::MAX));
}

#[test]
fn peaks_file_lives_under_peaks_folder() {
    let p = peaks_file(Path::new(r"C:\c"), 1, 1, 0, 1);
    assert_eq!(p.parent(), Some(Path::new(r"C:\c\peaks")));
}

// ---- export_dir ---------------------------------------------------------------------------------

#[test]
fn export_dir_default_is_storage_root_exports() {
    let p = export_dir(Path::new(r"D:\ClipCheck"), "");
    assert_eq!(p, PathBuf::from(r"D:\ClipCheck\Exports"));
}

#[test]
fn export_dir_strips_trailing_backslashes() {
    assert_eq!(
        export_dir(Path::new(r"D:\ClipCheck\"), ""),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
    assert_eq!(
        export_dir(Path::new(r"D:\ClipCheck\\"), ""),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

#[test]
fn export_dir_strips_trailing_forward_slashes() {
    assert_eq!(
        export_dir(Path::new("D:/ClipCheck/"), ""),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

#[test]
fn export_dir_bare_drive_keeps_root_separator() {
    assert_eq!(
        export_dir(Path::new("D:"), ""),
        PathBuf::from(r"D:\Exports")
    );
}

#[test]
fn export_dir_drive_root_with_separator() {
    assert_eq!(
        export_dir(Path::new(r"D:\"), ""),
        PathBuf::from(r"D:\Exports")
    );
}

#[test]
fn export_dir_explicit_setting_wins_and_is_trimmed() {
    let p = export_dir(Path::new(r"D:\ClipCheck"), r"  E:\Out  ");
    assert_eq!(p, PathBuf::from(r"E:\Out"));
}

#[test]
fn export_dir_blank_setting_uses_default() {
    assert_eq!(
        export_dir(Path::new(r"D:\ClipCheck"), "   "),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

#[test]
fn export_dir_passes_variables_through_unexpanded() {
    // Expansion of %VARS% is the caller's job; this function must not touch them.
    let p = export_dir(Path::new(r"D:\ClipCheck"), r"%USERPROFILE%\Videos");
    assert_eq!(p, PathBuf::from(r"%USERPROFILE%\Videos"));
}

#[test]
fn export_dir_empty_root_gives_rooted_path_not_relative() {
    // Suspected edge: an empty storage root yields "\Exports" (drive-relative), not "Exports".
    let p = export_dir(Path::new(""), "");
    assert!(p.has_root());
    assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("Exports"));
}

// ---- unique_output_path -------------------------------------------------------------------------

#[test]
fn unique_output_path_first_candidate_when_free() {
    let p = unique_output_path(Path::new(r"D:\Exports"), "Clip", "mp4", &never_exists);
    assert_eq!(p, PathBuf::from(r"D:\Exports\Clip.mp4"));
}

#[test]
fn unique_output_path_second_candidate_when_first_taken() {
    let taken = PathBuf::from(r"D:\Exports\Clip.mp4");
    let exists = move |p: &Path| p == taken;
    let p = unique_output_path(Path::new(r"D:\Exports"), "Clip", "mp4", &exists);
    assert_eq!(p, PathBuf::from(r"D:\Exports\Clip (2).mp4"));
}

#[test]
fn unique_output_path_skips_taken_suffixes_in_order() {
    let taken = [
        r"D:\Exports\Clip.mp4",
        r"D:\Exports\Clip (2).mp4",
        r"D:\Exports\Clip (3).mp4",
    ];
    let exists = move |p: &Path| taken.iter().any(|t| Path::new(t) == p);
    let p = unique_output_path(Path::new(r"D:\Exports"), "Clip", "mp4", &exists);
    assert_eq!(p, PathBuf::from(r"D:\Exports\Clip (4).mp4"));
}

#[test]
fn unique_output_path_leading_dot_extension_is_normalised() {
    let a = unique_output_path(Path::new("d"), "s", ".mp4", &never_exists);
    let b = unique_output_path(Path::new("d"), "s", "mp4", &never_exists);
    assert_eq!(a, b);
    assert_eq!(a.extension().and_then(|e| e.to_str()), Some("mp4"));
}

#[test]
fn unique_output_path_empty_extension_has_no_dot() {
    let p = unique_output_path(Path::new("d"), "still", "", &never_exists);
    assert_eq!(p, PathBuf::from("d").join("still"));
}

#[test]
fn unique_output_path_empty_extension_sequences_without_dot() {
    let taken = [PathBuf::from("d").join("still")];
    let exists = move |p: &Path| taken.iter().any(|t| t == p);
    let p = unique_output_path(Path::new("d"), "still", "", &exists);
    assert_eq!(p, PathBuf::from("d").join("still (2)"));
}

#[test]
fn unique_output_path_stem_with_dots_is_kept() {
    let p = unique_output_path(Path::new("d"), "my.clip", "mp4", &never_exists);
    assert_eq!(p, PathBuf::from("d").join("my.clip.mp4"));
}

#[test]
fn unique_output_path_checks_each_candidate_once_until_free() {
    let calls = Cell::new(0u32);
    let exists = |p: &Path| {
        calls.set(calls.get() + 1);
        p.ends_with("s.mp4") || p.ends_with("s (2).mp4") || p.ends_with("s (3).mp4")
    };
    let p = unique_output_path(Path::new("d"), "s", "mp4", &exists);
    assert_eq!(p, PathBuf::from("d").join("s (4).mp4"));
    assert_eq!(calls.get(), 4);
}

#[test]
fn unique_output_path_falls_back_after_9999_candidates() {
    let calls = Cell::new(0u32);
    let always = |_: &Path| {
        calls.set(calls.get() + 1);
        true
    };
    let p = unique_output_path(Path::new("d"), "s", "mp4", &always);
    assert_eq!(p, PathBuf::from("d").join("s (10000).mp4"));
    assert_eq!(
        calls.get(),
        9999,
        "fallback must not be checked against the filesystem"
    );
}

#[test]
fn unique_output_path_last_real_candidate_is_9999() {
    let exists = |p: &Path| !p.ends_with("s (9999).mp4");
    let p = unique_output_path(Path::new("d"), "s", "mp4", &exists);
    assert_eq!(p, PathBuf::from("d").join("s (9999).mp4"));
}

// ---- sanitize_stem ------------------------------------------------------------------------------

#[test]
fn sanitize_removes_windows_illegal_chars() {
    assert_eq!(sanitize_stem("a<b>c:d*e?f\"g|h\\i/j"), "abcdefghij");
}

#[test]
fn sanitize_removes_control_chars() {
    assert_eq!(sanitize_stem("a\u{0007}b\u{0000}c"), "abc");
}

#[test]
fn sanitize_collapses_whitespace() {
    assert_eq!(sanitize_stem("  my   clip \n x "), "my clip x");
}

#[test]
fn sanitize_trims_trailing_dots_and_spaces() {
    assert_eq!(sanitize_stem("name... "), "name");
}

#[test]
fn sanitize_trims_leading_dots() {
    assert_eq!(sanitize_stem("...name"), "name");
}

#[test]
fn sanitize_keeps_inner_dots() {
    assert_eq!(sanitize_stem("v1.2 final"), "v1.2 final");
}

#[test]
fn sanitize_reserved_con_is_guarded() {
    assert_eq!(sanitize_stem("CON"), "_CON");
}

#[test]
fn sanitize_reserved_is_case_insensitive() {
    assert_eq!(sanitize_stem("con"), "_con");
    assert_eq!(sanitize_stem("nul"), "_nul");
}

#[test]
fn sanitize_reserved_with_extension_is_guarded() {
    assert_eq!(sanitize_stem("nul.txt"), "_nul.txt");
    assert_eq!(sanitize_stem("con.tar.gz"), "_con.tar.gz");
}

#[test]
fn sanitize_reserved_com_and_lpt_are_guarded() {
    assert_eq!(sanitize_stem("COM1"), "_COM1");
    assert_eq!(sanitize_stem("lpt9"), "_lpt9");
}

#[test]
fn sanitize_not_reserved_names_pass_through() {
    assert_eq!(sanitize_stem("COM10"), "COM10");
    assert_eq!(sanitize_stem("CONSOLE"), "CONSOLE");
    assert_eq!(sanitize_stem("nullify"), "nullify");
}

#[test]
fn sanitize_reserved_after_leading_space_is_guarded() {
    assert_eq!(sanitize_stem("  CON  "), "_CON");
}

#[test]
fn sanitize_empty_falls_back_to_export() {
    assert_eq!(sanitize_stem(""), "Export");
}

#[test]
fn sanitize_only_illegal_chars_falls_back_to_export() {
    assert_eq!(sanitize_stem("<>|"), "Export");
}

#[test]
fn sanitize_only_dots_and_spaces_falls_back_to_export() {
    assert_eq!(sanitize_stem(" ... "), "Export");
}

#[test]
fn sanitize_caps_length_at_120_chars() {
    let out = sanitize_stem(&"a".repeat(200));
    assert_eq!(out.chars().count(), 120);
}

#[test]
fn sanitize_cap_is_on_char_boundary_for_multibyte() {
    let out = sanitize_stem(&"é".repeat(200));
    assert_eq!(out.chars().count(), 120);
    assert!(out.chars().all(|c| c == 'é'));
}

#[test]
fn sanitize_cap_handles_wide_chars() {
    let out = sanitize_stem(&"日本".repeat(100));
    assert_eq!(out.chars().count(), 120);
}

#[test]
fn sanitize_cap_then_trims_trailing_space() {
    // 119 'a', a space, then more text: the 120-char cut ends on a space, which is trimmed.
    let input = format!("{} bbb", "a".repeat(119));
    let out = sanitize_stem(&input);
    assert_eq!(out, "a".repeat(119));
}

#[test]
fn sanitize_keeps_unicode_letters() {
    assert_eq!(sanitize_stem("Ünïcödé Clip"), "Ünïcödé Clip");
}

#[test]
fn sanitize_output_never_contains_separators() {
    let out = sanitize_stem("x\\y/z:w");
    assert!(!out.contains('\\') && !out.contains('/') && !out.contains(':'));
}

// ---- is_inside ----------------------------------------------------------------------------------

#[test]
fn is_inside_child_path() {
    assert!(is_inside(
        Path::new(r"D:\Clips"),
        Path::new(r"D:\Clips\a.mp4")
    ));
}

#[test]
fn is_inside_is_case_insensitive() {
    assert!(is_inside(Path::new("D:\\Clips"), Path::new("d:\\CLIPS\\x")));
}

#[test]
fn is_inside_accepts_mixed_separators() {
    assert!(is_inside(
        Path::new("D:/Clips"),
        Path::new("D:\\Clips\\sub/x")
    ));
}

#[test]
fn is_inside_rejects_parent_traversal_in_path() {
    assert!(!is_inside(
        Path::new(r"D:\Clips"),
        Path::new(r"D:\Clips\..\Other")
    ));
}

#[test]
fn is_inside_rejects_parent_traversal_in_root() {
    assert!(!is_inside(
        Path::new(r"D:\Clips\..\X"),
        Path::new(r"D:\X\y")
    ));
}

#[test]
fn is_inside_equal_paths_count_as_inside() {
    assert!(is_inside(Path::new(r"D:\Clips"), Path::new(r"D:\Clips")));
}

#[test]
fn is_inside_trailing_separator_on_either_side() {
    assert!(is_inside(Path::new(r"D:\Clips\"), Path::new(r"D:\Clips\x")));
    assert!(is_inside(Path::new(r"D:\Clips"), Path::new(r"D:\Clips\")));
}

#[test]
fn is_inside_rejects_sibling_with_common_prefix() {
    assert!(!is_inside(Path::new(r"D:\A"), Path::new(r"D:\Ab\x")));
}

#[test]
fn is_inside_rejects_other_drive() {
    assert!(!is_inside(Path::new(r"D:\Clips"), Path::new(r"E:\Clips\x")));
}

#[test]
fn is_inside_rejects_parent_of_root() {
    assert!(!is_inside(
        Path::new(r"D:\Clips\sub"),
        Path::new(r"D:\Clips")
    ));
}

#[test]
fn is_inside_ignores_empty_and_dot_components() {
    assert!(is_inside(
        Path::new(r"D:\\Clips"),
        Path::new(r"D:\Clips\.\x")
    ));
}

#[test]
fn is_inside_rejects_unrelated_path() {
    assert!(!is_inside(Path::new(r"D:\Clips"), Path::new(r"C:\Windows")));
}
