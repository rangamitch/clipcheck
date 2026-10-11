//! Integration tests for retired config keys (D63): `advanced.ffmpeg_dir` and
//! `video.native_encoder` were removed from the schema. Old files must keep loading,
//! the retired keys must be dropped on save, and only the exact section+key pairs
//! are retired.

use clipcheck_core::config::schema::Config;
use clipcheck_core::config::validate::{Diagnostic, Severity};

/// A realistic config written by an older version: copied sections from the default
/// template plus the two retired keys.
const OLD_CONFIG: &str = r#"config_version = 1

[general]
autostart = true
start_minimized = false

[storage]
root = 'D:\Clips'
per_game_folders = false
quota_gb = 5

[video]
native_encoder = true

[advanced]
ffmpeg_dir = 'C:\Tools\ffmpeg'
"#;

/// The same file with the retired keys removed. Loading it must give an equal Config.
const OLD_CONFIG_WITHOUT_RETIRED: &str = r#"config_version = 1

[general]
autostart = true
start_minimized = false

[storage]
root = 'D:\Clips'
per_game_folders = false
quota_gb = 5

[video]

[advanced]
"#;

fn cfg004(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.code == "CFG-P004").collect()
}

fn has_unknown_path(diags: &[Diagnostic], path: &str) -> bool {
    cfg004(diags).iter().any(|d| d.path == path)
}

// 1. A realistic old file loads with both entry points.
#[test]
fn old_file_with_retired_keys_loads_plain() {
    let loaded = Config::from_toml_str(OLD_CONFIG);
    assert!(
        loaded.is_ok(),
        "old file should load, got {:?}",
        loaded.err()
    );
}

#[test]
fn old_file_with_retired_keys_loads_checked_without_cfg_p004_for_them() {
    let (_cfg, diags) = Config::from_toml_str_checked(OLD_CONFIG)
        .expect("old file should load through the checked variant");
    assert!(
        !has_unknown_path(&diags, "advanced.ffmpeg_dir"),
        "advanced.ffmpeg_dir must not warn: {diags:?}"
    );
    assert!(
        !has_unknown_path(&diags, "video.native_encoder"),
        "video.native_encoder must not warn: {diags:?}"
    );
    assert!(
        cfg004(&diags).is_empty(),
        "no CFG-P004 expected for a file with only retired keys: {diags:?}"
    );
}

#[test]
fn old_file_keeps_the_other_values() {
    let old = Config::from_toml_str(OLD_CONFIG).expect("old file should load");
    let stripped =
        Config::from_toml_str(OLD_CONFIG_WITHOUT_RETIRED).expect("stripped file should load");
    // Equal configs prove the retired keys did not change anything else and
    // that the surviving values (autostart, quota_gb, root) were read.
    assert_eq!(old, stripped);
    assert_ne!(
        old,
        Config::default(),
        "file values must not be dropped to defaults"
    );

    let text = old.to_toml_string().expect("should serialize");
    assert!(text.contains("autostart = true"), "autostart kept:\n{text}");
    assert!(text.contains("quota_gb = 5"), "quota_gb kept:\n{text}");
}

// 2. Re-serializing drops the retired keys and round-trips.
#[test]
fn re_serialized_old_config_drops_retired_keys() {
    let old = Config::from_toml_str(OLD_CONFIG).expect("old file should load");
    let text = old.to_toml_string().expect("should serialize");
    assert!(
        !text.contains("ffmpeg_dir"),
        "ffmpeg_dir leaked into save:\n{text}"
    );
    assert!(
        !text.contains("native_encoder"),
        "native_encoder leaked into save:\n{text}"
    );
}

#[test]
fn re_serialized_old_config_parses_back_equal() {
    let old = Config::from_toml_str(OLD_CONFIG).expect("old file should load");
    let text = old.to_toml_string().expect("should serialize");
    let back = Config::from_toml_str(&text).expect("saved text should parse");
    assert_eq!(back, old);
}

// 3. The shipped template no longer mentions the retired keys.
#[test]
fn default_toml_has_no_retired_keys() {
    let t = Config::default_toml();
    assert!(!t.contains("ffmpeg"), "default_toml mentions ffmpeg");
    assert!(
        !t.contains("native_encoder"),
        "default_toml mentions native_encoder"
    );
}

// 4. Genuinely unknown keys still warn, with the dotted path.
#[test]
fn unknown_key_next_to_retired_keys_still_warns() {
    let text = r#"
[advanced]
ffmpeg_dir = 'C:\Tools\ffmpeg'
something_else = 1

[video]
native_encoder = true
native_encoderx = true
"#;
    let (_cfg, diags) = Config::from_toml_str_checked(text).expect("should load");
    assert!(
        has_unknown_path(&diags, "advanced.something_else"),
        "advanced.something_else should warn: {diags:?}"
    );
    assert!(
        has_unknown_path(&diags, "video.native_encoderx"),
        "video.native_encoderx should warn: {diags:?}"
    );
    assert!(
        !has_unknown_path(&diags, "advanced.ffmpeg_dir"),
        "retired advanced.ffmpeg_dir must not warn: {diags:?}"
    );
    assert!(
        !has_unknown_path(&diags, "video.native_encoder"),
        "retired video.native_encoder must not warn: {diags:?}"
    );
    for d in cfg004(&diags) {
        assert_eq!(
            d.severity,
            Severity::Warning,
            "CFG-P004 should be a warning"
        );
    }
}

// 5. Only the exact section+key pairs are retired.
#[test]
fn retired_names_in_wrong_section_still_warn() {
    let text = "[general]\nffmpeg_dir = ''\n";
    let (_cfg, diags) = Config::from_toml_str_checked(text).expect("should load");
    assert!(
        has_unknown_path(&diags, "general.ffmpeg_dir"),
        "general.ffmpeg_dir should warn: {diags:?}"
    );
}

#[test]
fn native_encoder_in_advanced_still_warns() {
    let text = "[advanced]\nnative_encoder = true\n";
    let (_cfg, diags) = Config::from_toml_str_checked(text).expect("should load");
    assert!(
        has_unknown_path(&diags, "advanced.native_encoder"),
        "advanced.native_encoder should warn: {diags:?}"
    );
}

// 6. Retired keys of other TOML types still load without error.
#[test]
fn retired_keys_with_other_types_load() {
    let text = "[video]\nnative_encoder = \"yes\"\n\n[advanced]\nffmpeg_dir = 5\n";
    assert!(
        Config::from_toml_str(text).is_ok(),
        "wrong-typed retired keys must load"
    );
    let checked = Config::from_toml_str_checked(text);
    assert!(
        checked.is_ok(),
        "wrong-typed retired keys must load checked: {:?}",
        checked.err()
    );
}
