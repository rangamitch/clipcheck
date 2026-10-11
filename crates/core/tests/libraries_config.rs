//! Integration tests for extra library folders (`storage.libraries`): config round trip,
//! validation CFG-S009..S014, `all_library_paths`, `sanitize_game_folder`, `layout_dir`
//! and `apply_class` for library changes.

use std::path::PathBuf;

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn lib(path: &str) -> LibraryDir {
    LibraryDir {
        path: path.to_owned(),
        ..LibraryDir::default()
    }
}

fn labelled(path: &str, label: &str) -> LibraryDir {
    LibraryDir {
        path: path.to_owned(),
        label: label.to_owned(),
        ..LibraryDir::default()
    }
}

/// A config whose primary root is `D:\ClipCheck` and whose extra libraries are `libs`.
fn cfg_with(libs: Vec<LibraryDir>) -> Config {
    let mut c = Config::default();
    c.storage.root = PathBuf::from(r"D:\ClipCheck");
    c.storage.libraries = libs;
    c
}

fn with_code<'a>(d: &'a [Diagnostic], code: &str) -> Vec<&'a Diagnostic> {
    d.iter().filter(|x| x.code == code).collect()
}

fn errors_for(c: &Config, code: &str) -> Vec<Diagnostic> {
    with_code(&validate(c), code).into_iter().cloned().collect()
}

fn assert_no_code(c: &Config, code: &str) {
    let d = validate(c);
    assert!(
        with_code(&d, code).is_empty(),
        "expected no {code}, got {d:#?}"
    );
}

fn assert_error_at(c: &Config, code: &str, path: &str) {
    let d = validate(c);
    let hits = with_code(&d, code);
    assert!(!hits.is_empty(), "expected {code}, got {d:#?}");
    assert!(
        hits.iter()
            .any(|x| x.severity == Severity::Error && x.path == path),
        "expected {code} error at {path}, got {d:#?}"
    );
}

// ---------------------------------------------------------------------------
// 1. round trip and parsing
// ---------------------------------------------------------------------------

#[test]
fn default_config_round_trips_with_empty_libraries() {
    let def = Config::default();
    assert!(def.storage.libraries.is_empty());
    let text = def.to_toml_string().expect("serialize");
    let back = Config::from_toml_str(&text).expect("parse");
    assert_eq!(back, def);
    assert!(back.storage.libraries.is_empty());
}

#[test]
fn populated_libraries_round_trip_equal() {
    let mut c = cfg_with(vec![
        labelled(r"D:\Medal\Clips", "Medal"),
        LibraryDir {
            path: r"\\server\share\Clips".to_owned(),
            label: String::new(),
            read_only: true,
            enabled: true,
        },
        LibraryDir {
            path: r"E:\Old\Clips".to_owned(),
            label: "Old".to_owned(),
            read_only: false,
            enabled: false,
        },
    ]);
    c.storage.per_game_folders = false;
    let text = c.to_toml_string().expect("serialize");
    let back = Config::from_toml_str(&text).expect("parse");
    assert_eq!(back, c);
    assert_eq!(back.storage.libraries.len(), 3);
    assert!(back.storage.libraries[1].read_only);
    assert!(!back.storage.libraries[2].enabled);
}

#[test]
fn libraries_tables_parse_from_toml_text() {
    let text = r#"
[storage]
root = 'D:\ClipCheck'

[[storage.libraries]]
path = 'D:\Medal\Clips'
label = 'Medal'
read_only = true
enabled = false

[[storage.libraries]]
path = '%USERPROFILE%\Videos'
"#;
    let cfg = Config::from_toml_str(text).expect("parse");
    assert_eq!(cfg.storage.root, PathBuf::from(r"D:\ClipCheck"));
    assert_eq!(cfg.storage.libraries.len(), 2);
    assert_eq!(cfg.storage.libraries[0].path, r"D:\Medal\Clips");
    assert_eq!(cfg.storage.libraries[0].label, "Medal");
    assert!(cfg.storage.libraries[0].read_only);
    assert!(!cfg.storage.libraries[0].enabled);
    assert_eq!(cfg.storage.libraries[1].path, r"%USERPROFILE%\Videos");
}

#[test]
fn partial_library_entry_takes_field_defaults() {
    let text =
        "[storage]\nroot = 'D:\\ClipCheck'\n[[storage.libraries]]\npath = 'D:\\Medal\\Clips'\n";
    let cfg = Config::from_toml_str(text).expect("parse");
    assert_eq!(cfg.storage.libraries.len(), 1);
    let l = &cfg.storage.libraries[0];
    assert_eq!(l.path, r"D:\Medal\Clips");
    assert_eq!(l.label, "");
    assert!(!l.read_only);
    assert!(l.enabled);
}

#[test]
fn old_file_without_libraries_key_loads_empty() {
    let text = "[storage]\nroot = 'D:\\ClipCheck'\nquota_gb = 5\n";
    let cfg = Config::from_toml_str(text).expect("parse");
    assert!(cfg.storage.libraries.is_empty());
    assert_eq!(cfg.storage.quota_gb, 5);
}

#[test]
fn default_template_still_parses_to_default() {
    let parsed = Config::from_toml_str(Config::default_toml()).expect("parse");
    assert_eq!(parsed, Config::default());
}

// ---------------------------------------------------------------------------
// 2. validation
// ---------------------------------------------------------------------------

#[test]
fn cfg_s009_empty_path_is_error() {
    let c = cfg_with(vec![lib("")]);
    assert_error_at(&c, "CFG-S009", "storage.libraries[0].path");
}

#[test]
fn cfg_s009_whitespace_path_is_error() {
    let c = cfg_with(vec![lib("   ")]);
    assert_error_at(&c, "CFG-S009", "storage.libraries[0].path");
}

#[test]
fn cfg_s009_disabled_empty_path_still_reported() {
    let mut l = lib("");
    l.enabled = false;
    let c = cfg_with(vec![l]);
    assert_error_at(&c, "CFG-S009", "storage.libraries[0].path");
}

#[test]
fn cfg_s010_relative_paths_are_error() {
    for bad in [r"Clips\Medal", "clips"] {
        let c = cfg_with(vec![lib(bad)]);
        assert_error_at(&c, "CFG-S010", "storage.libraries[0].path");
    }
}

#[test]
fn cfg_s010_absolute_forms_are_accepted() {
    for ok in [
        r"D:\Medal",
        "d:/medal",
        r"\\server\share\Clips",
        r"%USERPROFILE%\Videos",
    ] {
        let c = cfg_with(vec![lib(ok)]);
        assert_no_code(&c, "CFG-S010");
    }
}

#[test]
fn cfg_s011_exact_duplicate_reported_on_second_entry() {
    let c = cfg_with(vec![lib(r"F:\Medal\Clips"), lib(r"F:\Medal\Clips")]);
    let d = errors_for(&c, "CFG-S011");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(d[0].path, "storage.libraries[1].path");
}

#[test]
fn cfg_s011_duplicate_after_normalization_case_and_slashes() {
    let c = cfg_with(vec![lib(r"F:\Medal\Clips"), lib("f:/medal/clips/")]);
    assert_error_at(&c, "CFG-S011", "storage.libraries[1].path");
}

#[test]
fn cfg_s011_duplicate_ignoring_trailing_separator() {
    let c = cfg_with(vec![lib(r"F:\Medal\Clips\"), lib(r"F:\Medal\Clips")]);
    assert_error_at(&c, "CFG-S011", "storage.libraries[1].path");
}

#[test]
fn cfg_s011_library_equal_to_primary_root() {
    let c = cfg_with(vec![lib("d:/clipcheck/")]);
    assert_error_at(&c, "CFG-S011", "storage.libraries[0].path");
}

#[test]
fn cfg_s012_seventeen_libraries_is_error() {
    let libs: Vec<LibraryDir> = (0..17).map(|i| lib(&format!(r"D:\L{i:02}"))).collect();
    let c = cfg_with(libs);
    assert_error_at(&c, "CFG-S012", "storage.libraries");
}

#[test]
fn cfg_s012_sixteen_libraries_is_accepted() {
    let libs: Vec<LibraryDir> = (0..16).map(|i| lib(&format!(r"D:\L{i:02}"))).collect();
    let c = cfg_with(libs);
    assert_no_code(&c, "CFG-S012");
}

#[test]
fn cfg_s013_label_of_41_chars_is_error() {
    let c = cfg_with(vec![labelled(r"D:\Medal", &"x".repeat(41))]);
    assert_error_at(&c, "CFG-S013", "storage.libraries[0].label");
}

#[test]
fn cfg_s013_label_of_40_chars_is_accepted() {
    let c = cfg_with(vec![labelled(r"D:\Medal", &"x".repeat(40))]);
    assert_no_code(&c, "CFG-S013");
}

#[test]
fn cfg_s013_label_counts_characters_not_bytes() {
    // 40 characters, 80 bytes: must be accepted.
    let ok = cfg_with(vec![labelled(r"D:\Medal", &"é".repeat(40))]);
    assert_no_code(&ok, "CFG-S013");
    // 41 characters: must be rejected.
    let bad = cfg_with(vec![labelled(r"D:\Medal", &"é".repeat(41))]);
    assert_error_at(&bad, "CFG-S013", "storage.libraries[0].label");
}

#[test]
fn cfg_s014_library_inside_primary_root_is_warning() {
    let c = cfg_with(vec![lib(r"D:\ClipCheck\Extra")]);
    let d = validate(&c);
    let hits = with_code(&d, "CFG-S014");
    assert_eq!(hits.len(), 1, "{d:#?}");
    assert_eq!(hits[0].severity, Severity::Warning);
    assert_eq!(hits[0].path, "storage.libraries[0].path");
    assert!(
        !d.iter().any(|x| x.severity == Severity::Error),
        "nesting must not be an error: {d:#?}"
    );
}

#[test]
fn cfg_s014_library_inside_another_enabled_library_is_warning() {
    let c = cfg_with(vec![lib(r"D:\Medal\Clips"), lib(r"D:\Medal\Clips\x")]);
    let d = validate(&c);
    let hits = with_code(&d, "CFG-S014");
    assert_eq!(hits.len(), 1, "{d:#?}");
    assert_eq!(hits[0].severity, Severity::Warning);
    assert_eq!(hits[0].path, "storage.libraries[1].path");
    assert!(
        !d.iter().any(|x| x.severity == Severity::Error),
        "nesting must not be an error: {d:#?}"
    );
}

#[test]
fn cfg_s014_disabled_nested_library_is_not_reported() {
    let mut inner = lib(r"D:\ClipCheck\Extra");
    inner.enabled = false;
    let c = cfg_with(vec![inner]);
    assert_no_code(&c, "CFG-S014");

    let mut inner2 = lib(r"D:\Medal\Clips\x");
    inner2.enabled = false;
    let c2 = cfg_with(vec![lib(r"D:\Medal\Clips"), inner2]);
    assert_no_code(&c2, "CFG-S014");
}

#[test]
fn cfg_s014_exact_duplicate_is_s011_not_s014() {
    let c = cfg_with(vec![lib(r"D:\Medal\Clips"), lib(r"D:\Medal\Clips")]);
    assert_no_code(&c, "CFG-S014");
    assert_error_at(&c, "CFG-S011", "storage.libraries[1].path");
}

#[test]
fn cfg_s014_sibling_prefix_is_not_nested() {
    let c = cfg_with(vec![lib(r"D:\ClipCheck2")]);
    assert_no_code(&c, "CFG-S014");
}

#[test]
fn cfg_s011_disabled_duplicate_of_primary_still_reported() {
    let mut l = lib(r"d:/clipcheck/");
    l.enabled = false;
    let c = cfg_with(vec![l]);
    assert_error_at(&c, "CFG-S011", "storage.libraries[0].path");
}

#[test]
fn valid_distinct_unnested_libraries_yield_no_library_diagnostics() {
    let c = cfg_with(vec![
        labelled(r"D:\Medal\Clips", "Medal"),
        LibraryDir {
            path: r"\\server\share\Clips".to_owned(),
            label: String::new(),
            read_only: true,
            enabled: true,
        },
    ]);
    let d = validate(&c);
    let lib_codes = [
        "CFG-S009", "CFG-S010", "CFG-S011", "CFG-S012", "CFG-S013", "CFG-S014",
    ];
    let stray: Vec<_> = d.iter().filter(|x| lib_codes.contains(&x.code)).collect();
    assert!(
        stray.is_empty(),
        "unexpected library diagnostics: {stray:#?}"
    );
}

// ---------------------------------------------------------------------------
// 3. all_library_paths
// ---------------------------------------------------------------------------

#[test]
fn all_library_paths_without_libraries_returns_only_primary() {
    let c = cfg_with(vec![]);
    let items = c.storage.all_library_paths();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, PathBuf::from(r"D:\ClipCheck"));
    assert!(items[0].1);
    assert!(items[0].2.is_none());
}

#[test]
fn all_library_paths_primary_first_enabled_in_order_disabled_omitted() {
    let mut disabled = labelled(r"D:\Off", "Off");
    disabled.enabled = false;
    let c = cfg_with(vec![
        labelled(r"D:\Medal\Clips", "Medal"),
        disabled,
        labelled(r"\\server\share\Clips", "NAS"),
    ]);
    let items = c.storage.all_library_paths();
    assert_eq!(items.len(), 3, "{items:#?}");

    // primary
    assert_eq!(items[0].0, PathBuf::from(r"D:\ClipCheck"));
    assert!(items[0].1);
    assert!(items[0].2.is_none());

    // first enabled extra
    assert_eq!(items[1].0, PathBuf::from(r"D:\Medal\Clips"));
    assert!(!items[1].1);
    let l1 = items[1].2.expect("extra library should be Some");
    assert_eq!(l1.label, "Medal");

    // second enabled extra, disabled one skipped
    assert_eq!(items[2].0, PathBuf::from(r"\\server\share\Clips"));
    assert!(!items[2].1);
    let l2 = items[2].2.expect("extra library should be Some");
    assert_eq!(l2.label, "NAS");

    // only the first item is primary
    assert_eq!(items.iter().filter(|x| x.1).count(), 1);
}

// ---------------------------------------------------------------------------
// 4. sanitize_game_folder
// ---------------------------------------------------------------------------

#[test]
fn sanitize_plain_names_unchanged() {
    assert_eq!(
        sanitize_game_folder("Grand Theft Auto V FiveM"),
        "Grand Theft Auto V FiveM"
    );
    assert_eq!(
        sanitize_game_folder("League of Legends"),
        "League of Legends"
    );
    assert_eq!(sanitize_game_folder("REPO"), "REPO");
}

#[test]
fn sanitize_colon_removed_and_spaces_collapsed() {
    assert_eq!(
        sanitize_game_folder("Call of Duty: Warzone"),
        "Call of Duty Warzone"
    );
}

#[test]
fn sanitize_removes_all_illegal_characters() {
    assert_eq!(
        sanitize_game_folder("What? Why* <a>|b\"c/d\\e"),
        "What Why abcde"
    );
}

#[test]
fn sanitize_reserved_names_get_underscore_prefix() {
    assert_eq!(sanitize_game_folder("CON"), "_CON");
    assert_eq!(sanitize_game_folder("con"), "_con");
    assert_eq!(sanitize_game_folder("con.txt"), "_con.txt");
    assert_eq!(sanitize_game_folder("COM1"), "_COM1");
    assert_eq!(sanitize_game_folder("lpt9"), "_lpt9");
    assert_eq!(sanitize_game_folder("COM9"), "_COM9");
}

#[test]
fn sanitize_non_reserved_device_lookalikes_unchanged() {
    assert_eq!(sanitize_game_folder("COM10"), "COM10");
    assert_eq!(sanitize_game_folder("LPT0"), "LPT0");
    assert_eq!(sanitize_game_folder("NULL"), "NULL");
    assert_eq!(sanitize_game_folder("CONSOLE"), "CONSOLE");
}

#[test]
fn sanitize_empty_results_become_unsorted() {
    for input in ["..", ".", "...", "", "   ", "*?<>|", "\\/:"] {
        assert_eq!(sanitize_game_folder(input), "Unsorted", "input {input:?}");
    }
}

#[test]
fn sanitize_trims_dots_and_spaces_at_ends() {
    assert_eq!(sanitize_game_folder("Game."), "Game");
    assert_eq!(sanitize_game_folder("Game. "), "Game");
    assert_eq!(sanitize_game_folder(".hidden game"), "hidden game");
}

#[test]
fn sanitize_collapses_whitespace_and_controls() {
    assert_eq!(sanitize_game_folder("  spaced   out  "), "spaced out");
    assert_eq!(sanitize_game_folder("A\tB"), "A B");
    assert_eq!(sanitize_game_folder("A\u{0007}B"), "A B");
}

#[test]
fn sanitize_unicode_unchanged() {
    assert_eq!(sanitize_game_folder("日本語ゲーム"), "日本語ゲーム");
    assert_eq!(sanitize_game_folder("🎮 Game"), "🎮 Game");
    assert_eq!(sanitize_game_folder("Ünïcödé Ñame"), "Ünïcödé Ñame");
}

#[test]
fn sanitize_truncates_to_80_characters() {
    let out = sanitize_game_folder(&"a".repeat(100));
    assert_eq!(out.chars().count(), 80);
    assert!(out.chars().all(|c| c == 'a'));
    assert_eq!(out, "a".repeat(80));
}

#[test]
fn sanitize_truncation_trims_trailing_space() {
    // 79 a's, space, b: the cut at 80 leaves a trailing space, which is trimmed.
    let input = format!("{} b", "a".repeat(79));
    let out = sanitize_game_folder(&input);
    assert_eq!(out, "a".repeat(79));
}

#[test]
fn sanitize_output_never_has_illegal_chars_or_bad_ends() {
    let cases = [
        "Grand Theft Auto V FiveM",
        "Call of Duty: Warzone",
        "What? Why* <a>|b\"c/d\\e",
        "CON",
        "con.txt",
        "..",
        "*?<>|",
        "Game. ",
        "A\tB",
        "A\u{0007}B",
        "🎮 Game",
        "  spaced   out  ",
    ];
    let illegal = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    for input in cases {
        let out = sanitize_game_folder(input);
        assert!(!out.is_empty(), "empty output for {input:?}");
        assert!(
            !out.chars().any(|c| illegal.contains(&c) || c.is_control()),
            "illegal char in {out:?} from {input:?}"
        );
        assert!(
            !out.ends_with('.') && !out.ends_with(' '),
            "bad ending in {out:?} from {input:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. layout_dir
// ---------------------------------------------------------------------------

#[test]
fn layout_dir_clips_and_sessions() {
    let c = cfg_with(vec![]);
    assert_eq!(
        c.storage.layout_dir(LayoutKind::Clips, "REPO"),
        r"D:\ClipCheck\Clips\REPO"
    );
    assert_eq!(
        c.storage.layout_dir(LayoutKind::Sessions, "REPO"),
        r"D:\ClipCheck\Sessions\REPO"
    );
}

#[test]
fn layout_dir_trailing_separator_on_root_is_not_doubled() {
    let mut c = cfg_with(vec![]);
    c.storage.root = PathBuf::from(r"D:\ClipCheck\");
    assert_eq!(
        c.storage.layout_dir(LayoutKind::Clips, "REPO"),
        r"D:\ClipCheck\Clips\REPO"
    );
    assert_eq!(
        c.storage.layout_dir(LayoutKind::Sessions, "REPO"),
        r"D:\ClipCheck\Sessions\REPO"
    );
}

#[test]
fn layout_dir_drive_root() {
    let mut c = cfg_with(vec![]);
    c.storage.root = PathBuf::from(r"D:\");
    assert_eq!(
        c.storage.layout_dir(LayoutKind::Clips, "REPO"),
        r"D:\Clips\REPO"
    );
}

#[test]
fn layout_dir_does_not_sanitize_game() {
    let c = cfg_with(vec![]);
    assert_eq!(
        c.storage.layout_dir(LayoutKind::Clips, "a/b"),
        r"D:\ClipCheck\Clips\a/b"
    );
}

// ---------------------------------------------------------------------------
// 6. apply_class for library changes
// ---------------------------------------------------------------------------

#[test]
fn apply_class_adding_library_is_live() {
    let old = Config::default();
    let mut new = Config::default();
    new.storage.libraries.push(lib(r"D:\Medal\Clips"));
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn apply_class_root_change_is_next_part() {
    let old = Config::default();
    let mut new = Config::default();
    new.storage.root = PathBuf::from(r"D:\Other");
    assert_eq!(apply_class(&old, &new), ApplyClass::NextPart);
}

#[test]
fn apply_class_root_and_libraries_change_is_next_part() {
    let old = Config::default();
    let mut new = Config::default();
    new.storage.root = PathBuf::from(r"D:\Other");
    new.storage.libraries.push(lib(r"D:\Medal\Clips"));
    assert_eq!(apply_class(&old, &new), ApplyClass::NextPart);
}

#[test]
fn apply_class_library_label_change_is_live() {
    let mut old = Config::default();
    old.storage.libraries.push(labelled(r"D:\Medal\Clips", ""));
    let mut new = old.clone();
    new.storage.libraries[0].label = "Medal".to_owned();
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn apply_class_identical_configs_is_live() {
    let mut c = Config::default();
    c.storage
        .libraries
        .push(labelled(r"D:\Medal\Clips", "Medal"));
    assert_eq!(apply_class(&c, &c.clone()), ApplyClass::Live);
    assert_eq!(
        apply_class(&Config::default(), &Config::default()),
        ApplyClass::Live
    );
}
