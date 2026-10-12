//! Integration tests for `storage.export_dir` (video editor export folder): default value,
//! resolution to `<root>\Exports`, TOML load and round trip, the default template, and
//! validation CFG-S015.

use std::path::PathBuf;
use std::process;

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A config whose `export_dir` is `dir` and whose primary root is `D:\ClipCheck`.
fn with_export(dir: &str) -> Config {
    let mut c = Config::default();
    c.storage.root = PathBuf::from(r"D:\ClipCheck");
    c.storage.export_dir = dir.to_owned();
    c
}

/// Every CFG-S015 diagnostic for `c`.
fn export_errors(c: &Config) -> Vec<Diagnostic> {
    validate(c)
        .into_iter()
        .filter(|d| d.code == "CFG-S015")
        .collect()
}

fn assert_export_rejected(dir: &str) {
    let hits = export_errors(&with_export(dir));
    assert_eq!(
        hits.len(),
        1,
        "expected one CFG-S015 for {dir:?}, got {hits:#?}"
    );
    assert_eq!(hits[0].severity, Severity::Error, "{dir:?}: {:?}", hits[0]);
    assert_eq!(hits[0].path, "storage.export_dir", "{dir:?}: {:?}", hits[0]);
}

fn assert_export_accepted(dir: &str) {
    let hits = export_errors(&with_export(dir));
    assert!(
        hits.is_empty(),
        "expected no CFG-S015 for {dir:?}, got {hits:#?}"
    );
}

/// A fresh, empty directory under the system temp dir unique to this test process.
fn unique_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("clipcheck-export-dir-{}-{tag}", process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// ---------------------------------------------------------------------------
// defaults and resolution
// ---------------------------------------------------------------------------

// 1. The default export_dir is empty.
#[test]
fn default_export_dir_is_empty() {
    assert_eq!(Config::default().storage.export_dir, "");
}

// 2. An empty export_dir resolves to <root>\Exports.
#[test]
fn empty_export_dir_resolves_to_root_exports() {
    let s = Storage {
        root: PathBuf::from(r"D:\ClipCheck"),
        ..Storage::default()
    };
    assert_eq!(
        s.export_dir_resolved(),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

// 3. A root with a trailing backslash does not produce a doubled separator.
#[test]
fn root_with_trailing_backslash_resolves_without_double_separator() {
    let s = Storage {
        root: PathBuf::from(r"D:\ClipCheck\"),
        ..Storage::default()
    };
    assert_eq!(
        s.export_dir_resolved(),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

// 4. Several trailing separators (and forward slashes) are all dropped.
#[test]
fn root_with_many_trailing_separators_is_trimmed() {
    let s = Storage {
        root: PathBuf::from(r"D:\ClipCheck\\/"),
        ..Storage::default()
    };
    assert_eq!(
        s.export_dir_resolved(),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

// 5. A blank (whitespace only) export_dir means the default <root>\Exports.
#[test]
fn whitespace_export_dir_uses_default() {
    let c = with_export("   \t ");
    assert_eq!(
        c.storage.export_dir_resolved(),
        PathBuf::from(r"D:\ClipCheck\Exports")
    );
}

// 6. An explicit export_dir is used as given, with no Exports suffix added.
#[test]
fn explicit_export_dir_is_used_as_given() {
    let s = Storage {
        export_dir: r"E:\Videos\Edits".to_owned(),
        ..Storage::default()
    };
    assert_eq!(s.export_dir_resolved(), PathBuf::from(r"E:\Videos\Edits"));
}

// 7. Surrounding whitespace in an explicit export_dir is trimmed.
#[test]
fn explicit_export_dir_is_trimmed() {
    let s = Storage {
        export_dir: r"  E:\Out  ".to_owned(),
        ..Storage::default()
    };
    assert_eq!(s.export_dir_resolved(), PathBuf::from(r"E:\Out"));
}

// 8. The explicit value does not depend on the root.
#[test]
fn explicit_export_dir_ignores_root() {
    let s = Storage {
        root: PathBuf::from(r"D:\ClipCheck"),
        export_dir: r"E:\Out".to_owned(),
        ..Storage::default()
    };
    assert_eq!(s.export_dir_resolved(), PathBuf::from(r"E:\Out"));
}

// 9. A %VAR% in the default root expands from the environment (USERPROFILE is read only, never set).
#[test]
fn default_root_variable_expands_when_env_is_set() {
    let Ok(profile) = std::env::var("USERPROFILE") else {
        eprintln!("USERPROFILE is not set; skipping variable expansion check");
        return;
    };
    let s = Storage::default();
    let expected = format!(r"{profile}\Videos\ClipCheck\Exports");
    assert_eq!(s.export_dir_resolved(), PathBuf::from(expected));
}

// 10. A %VAR% in an explicit export_dir expands from the environment.
#[test]
fn explicit_variable_expands_when_env_is_set() {
    let Ok(profile) = std::env::var("USERPROFILE") else {
        eprintln!("USERPROFILE is not set; skipping variable expansion check");
        return;
    };
    let s = Storage {
        export_dir: r"%USERPROFILE%\Videos\Edits".to_owned(),
        ..Storage::default()
    };
    assert_eq!(
        s.export_dir_resolved(),
        PathBuf::from(format!(r"{profile}\Videos\Edits"))
    );
}

// 11. An unknown %VAR% is kept literally during resolution.
#[test]
fn unknown_variable_is_kept_literally_in_resolution() {
    let s = Storage {
        export_dir: r"%CLIPCHECK_NO_SUCH_VAR_EXPORT%\Exports".to_owned(),
        ..Storage::default()
    };
    assert_eq!(
        s.export_dir_resolved(),
        PathBuf::from(r"%CLIPCHECK_NO_SUCH_VAR_EXPORT%\Exports")
    );
}

// ---------------------------------------------------------------------------
// TOML load and round trip
// ---------------------------------------------------------------------------

// 12. A config file written before the key existed loads with an empty export_dir.
#[test]
fn old_toml_without_export_dir_loads_empty() {
    let cfg = Config::from_toml_str("[storage]\nroot = 'D:\\ClipCheck'\nquota_gb = 5\n")
        .expect("old file loads");
    assert_eq!(cfg.storage.export_dir, "");
    assert_eq!(cfg.storage.quota_gb, 5);
}

// 13. The key is read from [storage].
#[test]
fn export_dir_loads_from_storage_table() {
    let cfg = Config::from_toml_str("[storage]\nexport_dir = 'D:\\Out'\n").expect("loads");
    assert_eq!(cfg.storage.export_dir, r"D:\Out");
}

// 14. A set export_dir survives a TOML round trip.
#[test]
fn set_export_dir_round_trips_through_toml() {
    let c = with_export(r"E:\Videos\Exports");
    let text = c.to_toml_string().expect("serialize");
    let back = Config::from_toml_str(&text).expect("parse");
    assert_eq!(back.storage.export_dir, r"E:\Videos\Exports");
    assert_eq!(back, c);
}

// 15. The default config (empty export_dir) round trips.
#[test]
fn default_config_round_trips_with_empty_export_dir() {
    let text = Config::default().to_toml_string().expect("serialize");
    let back = Config::from_toml_str(&text).expect("parse");
    assert_eq!(back.storage.export_dir, "");
    assert_eq!(back, Config::default());
}

// 16. A %VAR% value is stored as written, not expanded, by load.
#[test]
fn export_dir_variable_is_stored_unexpanded() {
    let cfg = Config::from_toml_str("[storage]\nexport_dir = '%USERPROFILE%\\Clips\\Out'\n")
        .expect("loads");
    assert_eq!(cfg.storage.export_dir, r"%USERPROFILE%\Clips\Out");
}

// 17. The commented template documents the key with an empty default.
#[test]
fn default_template_declares_empty_export_dir() {
    let text = Config::default_toml();
    assert!(
        text.lines()
            .any(|l| l.trim_start().starts_with("export_dir = \"\"")),
        "template should contain export_dir = \"\""
    );
}

// 18. The default template parses to a config whose export_dir is empty.
#[test]
fn default_template_parses_with_empty_export_dir() {
    let cfg = Config::from_toml_str(Config::default_toml()).expect("template parses");
    assert_eq!(cfg.storage.export_dir, "");
}

// 19. The default template validates with no CFG-S015 and no CFG-P004 for the key.
#[test]
fn default_template_has_no_export_dir_diagnostics() {
    let (cfg, warnings) =
        Config::from_toml_str_checked(Config::default_toml()).expect("template loads");
    assert!(export_errors(&cfg).is_empty());
    assert!(
        warnings.iter().all(|w| !w.path.contains("export_dir")),
        "export_dir should be a known key: {warnings:#?}"
    );
}

// ---------------------------------------------------------------------------
// validation CFG-S015
// ---------------------------------------------------------------------------

// 20. A blank export_dir is valid (it means the default).
#[test]
fn blank_export_dir_is_valid() {
    assert_export_accepted("");
    assert_export_accepted("   ");
}

// 21. A valid absolute drive path is accepted.
#[test]
fn absolute_drive_path_is_accepted() {
    assert_export_accepted(r"D:\Videos\ClipCheck\Exports");
}

// 22. A drive path written with forward slashes is accepted.
#[test]
fn forward_slash_drive_path_is_accepted() {
    assert_export_accepted("D:/Videos/Exports");
}

// 23. A UNC path is accepted.
#[test]
fn unc_path_is_accepted() {
    assert_export_accepted(r"\\server\share\Exports");
}

// 24. A relative path is rejected.
#[test]
fn relative_path_is_rejected() {
    assert_export_rejected("Exports");
    assert_export_rejected(r"..\Exports");
}

// 25. A rooted path without a drive letter is rejected.
#[test]
fn rooted_path_without_drive_is_rejected() {
    assert_export_rejected(r"\Exports");
}

// 26. Each Windows-illegal character (* ? " < > |) is rejected.
#[test]
fn illegal_characters_are_rejected() {
    for bad in [
        r"D:\Out*",
        r"D:\Out?",
        "D:\\Out\"x",
        r"D:\Out<x",
        r"D:\Out>x",
        r"D:\Out|x",
    ] {
        assert_export_rejected(bad);
    }
}

// 27. A path exactly 240 characters long is accepted.
#[test]
fn path_of_240_characters_is_accepted() {
    let dir = format!(r"D:\{}", "a".repeat(237));
    assert_eq!(dir.chars().count(), 240);
    assert_export_accepted(&dir);
}

// 28. A path of 241 characters is rejected.
#[test]
fn path_of_241_characters_is_rejected() {
    let dir = format!(r"D:\{}", "a".repeat(238));
    assert_eq!(dir.chars().count(), 241);
    assert_export_rejected(&dir);
}

// 29. A path that names an existing file is rejected.
#[test]
fn existing_file_path_is_rejected() {
    let dir = unique_temp_dir("file-target");
    let file = dir.join("not-a-folder.txt");
    std::fs::write(&file, b"synthetic").expect("write temp file");
    let text = file.to_string_lossy().into_owned();
    let result = std::panic::catch_unwind(|| assert_export_rejected(&text));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

// 30. A path that names an existing directory is accepted.
#[test]
fn existing_directory_path_is_accepted() {
    let dir = unique_temp_dir("dir-target");
    let text = dir.to_string_lossy().into_owned();
    let result = std::panic::catch_unwind(|| assert_export_accepted(&text));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

// 31. The CFG-S015 diagnostic is an error at storage.export_dir and names the expanded value.
#[test]
fn diagnostic_names_the_key_and_value() {
    let hits = export_errors(&with_export("Exports"));
    let d = hits.first().expect("one CFG-S015");
    assert!(d.message.contains("Exports"), "message: {}", d.message);
    assert!(
        d.message.contains("storage.export_dir"),
        "message: {}",
        d.message
    );
}

// 32. An unknown %VAR% stays literal during validation, so the path is still relative and rejected.
#[test]
fn unknown_variable_fails_absolute_check() {
    assert_export_rejected(r"%CLIPCHECK_NO_SUCH_VAR_EXPORT%\Exports");
}

// 33. Validation reports one CFG-S015 per bad value, not one per rule.
#[test]
fn one_bad_value_yields_one_diagnostic() {
    let hits = export_errors(&with_export(r"relative\Out*|bad"));
    assert_eq!(hits.len(), 1, "{hits:#?}");
}

// 34. A valid root with a valid explicit export_dir yields no diagnostics for the key.
#[test]
fn valid_root_and_export_dir_yield_no_export_errors() {
    let mut c = with_export(r"E:\Exports");
    c.storage.root = PathBuf::from(r"E:\ClipCheck");
    assert!(export_errors(&c).is_empty());
}
