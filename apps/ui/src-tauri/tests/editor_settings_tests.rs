//! Public-API tests for `clipcheck_ui_lib::editor::settings` (UI-owned `settings.json`, D72).
//!
//! All file work happens in tempdirs. `settings_path()` reads LOCALAPPDATA and is not used here.

use std::fs;
use std::path::Path;

use clipcheck_timeline::export::{ExportSettings, Preset};
use clipcheck_ui_lib::editor::settings::{load, normalize, save, EditorSettings};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_TEMPLATE: &str = "{project} {yyyy-MM-dd HH-mm-ss}";

fn preset(id: &str, label: &str) -> Preset {
    Preset {
        id: id.to_string(),
        label: label.to_string(),
        settings: ExportSettings::default(),
        fixed_size: false,
        max_mb: None,
        builtin: false,
    }
}

fn normalized(mut s: EditorSettings) -> EditorSettings {
    normalize(&mut s);
    s
}

// ---- defaults -----------------------------------------------------------------------------------

#[test]
fn defaults_match_documented_values() {
    let d = EditorSettings::default();
    assert_eq!(d.autosave_s, 30);
    assert!(d.magnetic_default);
    assert!(d.snap_default);
    assert_eq!(d.default_canvas, "match_first_clip");
    assert_eq!(d.preview_quality, "auto");
    assert_eq!(d.cache_dir, "");
    assert_eq!(d.cache_max_gb, 5);
    assert_eq!(d.decode_cache_mb, 512);
    assert_eq!(d.default_preset, "balanced_1080p30");
    assert_eq!(d.encoder, "auto");
    assert!(d.pause_while_recording);
    assert!(!d.open_folder_when_done);
    assert_eq!(d.filename_template, DEFAULT_TEMPLATE);
    assert!(d.low_priority);
    assert!(!d.always_ask_location);
    assert_eq!(d.last_export_dir, "");
    assert!(d.protect_exports_from_quota);
    assert!(d.custom_presets.is_empty());
}

#[test]
fn defaults_are_already_normalised() {
    let d = EditorSettings::default();
    assert_eq!(normalized(d.clone()), d);
}

#[test]
fn normalize_is_idempotent() {
    let mut s = EditorSettings {
        autosave_s: 0,
        preview_quality: "x".into(),
        ..Default::default()
    };
    normalize(&mut s);
    let once = s.clone();
    normalize(&mut s);
    assert_eq!(s, once);
}

// ---- normalize: numeric clamps ------------------------------------------------------------------

#[test]
fn autosave_clamps_low_to_10() {
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: 0,
            ..Default::default()
        })
        .autosave_s,
        10
    );
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: 9,
            ..Default::default()
        })
        .autosave_s,
        10
    );
}

#[test]
fn autosave_clamps_high_to_600() {
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: 601,
            ..Default::default()
        })
        .autosave_s,
        600
    );
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: u32::MAX,
            ..Default::default()
        })
        .autosave_s,
        600
    );
}

#[test]
fn autosave_bounds_are_kept() {
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: 10,
            ..Default::default()
        })
        .autosave_s,
        10
    );
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: 600,
            ..Default::default()
        })
        .autosave_s,
        600
    );
    assert_eq!(
        normalized(EditorSettings {
            autosave_s: 45,
            ..Default::default()
        })
        .autosave_s,
        45
    );
}

#[test]
fn cache_max_gb_clamps_to_1_and_100() {
    assert_eq!(
        normalized(EditorSettings {
            cache_max_gb: 0,
            ..Default::default()
        })
        .cache_max_gb,
        1
    );
    assert_eq!(
        normalized(EditorSettings {
            cache_max_gb: 500,
            ..Default::default()
        })
        .cache_max_gb,
        100
    );
    assert_eq!(
        normalized(EditorSettings {
            cache_max_gb: 7,
            ..Default::default()
        })
        .cache_max_gb,
        7
    );
}

#[test]
fn decode_cache_mb_clamps_to_128_and_4096() {
    assert_eq!(
        normalized(EditorSettings {
            decode_cache_mb: 1,
            ..Default::default()
        })
        .decode_cache_mb,
        128
    );
    assert_eq!(
        normalized(EditorSettings {
            decode_cache_mb: 9999,
            ..Default::default()
        })
        .decode_cache_mb,
        4096
    );
    assert_eq!(
        normalized(EditorSettings {
            decode_cache_mb: 1024,
            ..Default::default()
        })
        .decode_cache_mb,
        1024
    );
}

// ---- normalize: enums ---------------------------------------------------------------------------

#[test]
fn preview_quality_accepts_known_values() {
    for q in ["auto", "full", "half", "quarter"] {
        let s = normalized(EditorSettings {
            preview_quality: q.into(),
            ..Default::default()
        });
        assert_eq!(s.preview_quality, q);
    }
}

#[test]
fn preview_quality_unknown_becomes_auto() {
    for q in ["FULL", "bogus", "", "1/2"] {
        let s = normalized(EditorSettings {
            preview_quality: q.into(),
            ..Default::default()
        });
        assert_eq!(s.preview_quality, "auto", "input {q:?}");
    }
}

#[test]
fn preview_quality_is_trimmed_before_check() {
    let s = normalized(EditorSettings {
        preview_quality: " quarter ".into(),
        ..Default::default()
    });
    assert_eq!(s.preview_quality, "quarter");
}

#[test]
fn default_canvas_accepts_known_presets() {
    for c in ["match_first_clip", "16:9", "9:16", "1:1", "4:5"] {
        let s = normalized(EditorSettings {
            default_canvas: c.into(),
            ..Default::default()
        });
        assert_eq!(s.default_canvas, c);
    }
}

#[test]
fn default_canvas_unknown_becomes_match_first_clip() {
    for c in ["21:9", "", "square", "16x9"] {
        let s = normalized(EditorSettings {
            default_canvas: c.into(),
            ..Default::default()
        });
        assert_eq!(s.default_canvas, "match_first_clip", "input {c:?}");
    }
}

#[test]
fn default_canvas_is_trimmed() {
    let s = normalized(EditorSettings {
        default_canvas: " 9:16 ".into(),
        ..Default::default()
    });
    assert_eq!(s.default_canvas, "9:16");
}

// ---- normalize: strings -------------------------------------------------------------------------

#[test]
fn cache_dir_and_last_export_dir_are_trimmed() {
    let s = normalized(EditorSettings {
        cache_dir: "  D:\\Cache  ".into(),
        last_export_dir: "\tE:\\Out\n".into(),
        ..Default::default()
    });
    assert_eq!(s.cache_dir, "D:\\Cache");
    assert_eq!(s.last_export_dir, "E:\\Out");
}

#[test]
fn blank_filename_template_gets_default() {
    let s = normalized(EditorSettings {
        filename_template: "   ".into(),
        ..Default::default()
    });
    assert_eq!(s.filename_template, DEFAULT_TEMPLATE);
}

#[test]
fn filename_template_is_trimmed() {
    let s = normalized(EditorSettings {
        filename_template: "  {project}  ".into(),
        ..Default::default()
    });
    assert_eq!(s.filename_template, "{project}");
}

#[test]
fn filename_template_is_capped_at_200_chars() {
    let long = "x".repeat(300);
    let s = normalized(EditorSettings {
        filename_template: long,
        ..Default::default()
    });
    assert_eq!(s.filename_template.chars().count(), 200);
}

#[test]
fn filename_template_cap_counts_chars_not_bytes() {
    let long = "é".repeat(250);
    let s = normalized(EditorSettings {
        filename_template: long,
        ..Default::default()
    });
    assert_eq!(s.filename_template.chars().count(), 200);
}

#[test]
fn blank_encoder_becomes_auto() {
    let s = normalized(EditorSettings {
        encoder: "  ".into(),
        ..Default::default()
    });
    assert_eq!(s.encoder, "auto");
}

#[test]
fn named_encoder_is_kept_and_trimmed() {
    let s = normalized(EditorSettings {
        encoder: " nvenc ".into(),
        ..Default::default()
    });
    assert_eq!(s.encoder, "nvenc");
}

#[test]
fn blank_default_preset_gets_builtin_default() {
    let s = normalized(EditorSettings {
        default_preset: "".into(),
        ..Default::default()
    });
    assert_eq!(s.default_preset, "balanced_1080p30");
}

#[test]
fn default_preset_is_trimmed() {
    let s = normalized(EditorSettings {
        default_preset: " discord_10mb ".into(),
        ..Default::default()
    });
    assert_eq!(s.default_preset, "discord_10mb");
}

// ---- normalize: custom presets ------------------------------------------------------------------

#[test]
fn builtin_presets_are_dropped() {
    let mut builtin = preset("shipped", "Shipped");
    builtin.builtin = true;
    let s = normalized(EditorSettings {
        custom_presets: vec![builtin],
        ..Default::default()
    });
    assert!(s.custom_presets.is_empty());
}

#[test]
fn presets_with_empty_or_blank_id_are_dropped() {
    let s = normalized(EditorSettings {
        custom_presets: vec![preset("", "a"), preset("   ", "b"), preset("ok", "c")],
        ..Default::default()
    });
    assert_eq!(s.custom_presets.len(), 1);
    assert_eq!(s.custom_presets[0].id, "ok");
}

#[test]
fn preset_ids_are_trimmed() {
    let s = normalized(EditorSettings {
        custom_presets: vec![preset("  mine  ", "Mine")],
        ..Default::default()
    });
    assert_eq!(s.custom_presets[0].id, "mine");
}

#[test]
fn duplicate_preset_ids_keep_last_occurrence() {
    let s = normalized(EditorSettings {
        custom_presets: vec![preset("a", "first"), preset("b", "B"), preset("a", "last")],
        ..Default::default()
    });
    let ids: Vec<&str> = s.custom_presets.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);
    assert_eq!(s.custom_presets[1].label, "last");
}

#[test]
fn duplicate_ids_after_trim_are_collapsed() {
    let s = normalized(EditorSettings {
        custom_presets: vec![preset("a", "one"), preset(" a ", "two")],
        ..Default::default()
    });
    assert_eq!(s.custom_presets.len(), 1);
    assert_eq!(s.custom_presets[0].label, "two");
}

#[test]
fn preset_order_is_preserved_for_unique_ids() {
    let s = normalized(EditorSettings {
        custom_presets: vec![preset("x", "1"), preset("y", "2"), preset("z", "3")],
        ..Default::default()
    });
    let ids: Vec<&str> = s.custom_presets.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["x", "y", "z"]);
}

#[test]
fn custom_presets_are_capped_at_50() {
    let many: Vec<Preset> = (0..60).map(|i| preset(&format!("p{i}"), "x")).collect();
    let s = normalized(EditorSettings {
        custom_presets: many,
        ..Default::default()
    });
    assert_eq!(s.custom_presets.len(), 50);
}

#[test]
fn exactly_50_custom_presets_are_all_kept() {
    let many: Vec<Preset> = (0..50).map(|i| preset(&format!("p{i}"), "x")).collect();
    let s = normalized(EditorSettings {
        custom_presets: many,
        ..Default::default()
    });
    assert_eq!(s.custom_presets.len(), 50);
}

#[test]
fn cap_keeps_first_fifty_in_file_order_current_behaviour() {
    // Observed behaviour, documented here so a change is noticed: with 60 unique presets the
    // first 50 (p0..p49) are kept and the newest entries at the end are dropped.
    let many: Vec<Preset> = (0..60).map(|i| preset(&format!("p{i}"), "x")).collect();
    let s = normalized(EditorSettings {
        custom_presets: many,
        ..Default::default()
    });
    assert_eq!(s.custom_presets.first().map(|p| p.id.as_str()), Some("p0"));
    assert_eq!(s.custom_presets.last().map(|p| p.id.as_str()), Some("p49"));
}

// ---- load ---------------------------------------------------------------------------------------

#[test]
fn load_missing_file_gives_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let s = load(&dir.path().join("settings.json"));
    assert_eq!(s, EditorSettings::default());
}

#[test]
fn load_invalid_json_gives_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, "{ this is not json").expect("write");
    assert_eq!(load(&p), EditorSettings::default());
}

#[test]
fn load_wrong_field_type_gives_whole_defaults() {
    // One bad field makes the whole file invalid, so every field falls back to its default.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, r#"{"autosaveS": 20, "snapDefault": "yes"}"#).expect("write");
    assert_eq!(load(&p).autosave_s, 30);
}

#[test]
fn load_negative_autosave_gives_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, r#"{"autosaveS": -5}"#).expect("write");
    assert_eq!(load(&p), EditorSettings::default());
}

#[test]
fn load_directory_path_gives_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(load(dir.path()), EditorSettings::default());
}

#[test]
fn load_oversized_file_gives_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    let mut body = String::from(r#"{"autosaveS": 15}"#);
    body.push_str(&" ".repeat(MAX_BYTES + 1 - body.len()));
    assert!(body.len() > MAX_BYTES);
    fs::write(&p, body).expect("write");
    // Valid JSON, but over 2 MiB: rejected, so the stored 15 is ignored.
    assert_eq!(load(&p).autosave_s, 30);
}

#[test]
fn load_file_exactly_at_limit_is_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    let mut body = String::from(r#"{"autosaveS": 15}"#);
    body.push_str(&" ".repeat(MAX_BYTES - body.len()));
    assert_eq!(body.len(), MAX_BYTES);
    fs::write(&p, body).expect("write");
    assert_eq!(load(&p).autosave_s, 15);
}

#[test]
fn load_partial_file_fills_missing_fields_with_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, r#"{"autosaveS": 120}"#).expect("write");
    let s = load(&p);
    assert_eq!(s.autosave_s, 120);
    assert_eq!(s.cache_max_gb, 5);
    assert!(s.snap_default);
}

#[test]
fn load_ignores_unknown_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, r#"{"autosaveS": 20, "futureThing": true, "x": [1,2]}"#).expect("write");
    assert_eq!(load(&p).autosave_s, 20);
}

#[test]
fn load_normalises_out_of_range_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(
        &p,
        r#"{"autosaveS": 1, "cacheMaxGb": 999, "previewQuality": "nope"}"#,
    )
    .expect("write");
    let s = load(&p);
    assert_eq!(s.autosave_s, 10);
    assert_eq!(s.cache_max_gb, 100);
    assert_eq!(s.preview_quality, "auto");
}

#[test]
fn load_empty_object_gives_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, "{}").expect("write");
    assert_eq!(load(&p), EditorSettings::default());
}

// ---- save / round trip --------------------------------------------------------------------------

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    let s = EditorSettings {
        autosave_s: 90,
        magnetic_default: false,
        snap_default: false,
        default_canvas: "9:16".into(),
        preview_quality: "half".into(),
        cache_dir: "D:\\Cache".into(),
        cache_max_gb: 42,
        decode_cache_mb: 2048,
        default_preset: "discord_10mb".into(),
        encoder: "software".into(),
        pause_while_recording: false,
        open_folder_when_done: true,
        filename_template: "{project} {n}".into(),
        low_priority: false,
        always_ask_location: true,
        last_export_dir: "E:\\Out".into(),
        protect_exports_from_quota: false,
        custom_presets: vec![preset("mine", "Mine")],
    };
    save(&p, &s).expect("save");
    assert_eq!(load(&p), s);
}

#[test]
fn save_creates_missing_parent_folders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("a").join("b").join("settings.json");
    save(&p, &EditorSettings::default()).expect("save");
    assert!(p.is_file());
}

#[test]
fn save_leaves_no_part_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    save(&p, &EditorSettings::default()).expect("save");
    assert!(!dir.path().join("settings.json.part").exists());
}

#[test]
fn save_overwrites_existing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    save(
        &p,
        &EditorSettings {
            autosave_s: 60,
            ..Default::default()
        },
    )
    .expect("first");
    save(
        &p,
        &EditorSettings {
            autosave_s: 200,
            ..Default::default()
        },
    )
    .expect("second");
    assert_eq!(load(&p).autosave_s, 200);
}

#[test]
fn save_normalises_before_writing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    save(
        &p,
        &EditorSettings {
            autosave_s: 1,
            ..Default::default()
        },
    )
    .expect("save");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&p).expect("read")).expect("json");
    assert_eq!(v["autosaveS"], serde_json::json!(10));
}

#[test]
fn save_does_not_mutate_callers_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    let input = EditorSettings {
        autosave_s: 1,
        ..Default::default()
    };
    save(&p, &input).expect("save");
    assert_eq!(input.autosave_s, 1);
}

#[test]
fn save_writes_camel_case_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    save(&p, &EditorSettings::default()).expect("save");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&p).expect("read")).expect("json");
    let obj = v.as_object().expect("object");
    for key in [
        "autosaveS",
        "magneticDefault",
        "snapDefault",
        "defaultCanvas",
        "previewQuality",
        "cacheDir",
        "cacheMaxGb",
        "decodeCacheMb",
        "defaultPreset",
        "encoder",
        "pauseWhileRecording",
        "openFolderWhenDone",
        "filenameTemplate",
        "lowPriority",
        "alwaysAskLocation",
        "lastExportDir",
        "protectExportsFromQuota",
        "customPresets",
    ] {
        assert!(obj.contains_key(key), "missing key {key}");
    }
    assert!(!obj.keys().any(|k| k.contains('_')), "snake_case key found");
}

#[test]
fn camel_case_json_is_read_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(
        &p,
        r#"{"pauseWhileRecording": false, "openFolderWhenDone": true}"#,
    )
    .expect("write");
    let s = load(&p);
    assert!(!s.pause_while_recording);
    assert!(s.open_folder_when_done);
}

#[test]
fn snake_case_json_keys_are_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(&p, r#"{"autosave_s": 20, "pause_while_recording": false}"#).expect("write");
    let s = load(&p);
    assert_eq!(s.autosave_s, 30);
    assert!(s.pause_while_recording);
}

#[test]
fn save_into_path_whose_parent_is_a_file_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, "x").expect("write");
    let p = blocker.join("settings.json");
    assert!(save(&p, &EditorSettings::default()).is_err());
}

#[test]
fn save_onto_existing_directory_fails_and_removes_part() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("settings.json");
    fs::create_dir(&target).expect("mkdir");
    assert!(save(&target, &EditorSettings::default()).is_err());
    assert!(!dir.path().join("settings.json.part").exists());
}

#[test]
fn save_error_names_the_failing_step() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, "x").expect("write");
    let err = save(&blocker.join("s.json"), &EditorSettings::default()).expect_err("fails");
    assert!(err.starts_with("create settings folder"), "got {err}");
}

#[test]
fn load_uses_path_as_given_without_part_fallback() {
    // A leftover .part file is never read as the settings file.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("settings.json");
    fs::write(format!("{}.part", p.display()), r#"{"autosaveS": 15}"#).expect("write");
    assert_eq!(load(&p), EditorSettings::default());
}

#[test]
fn settings_serde_accepts_empty_object() {
    let s: EditorSettings = serde_json::from_str("{}").expect("parse");
    assert_eq!(s, EditorSettings::default());
}

#[test]
fn settings_serde_json_round_trip_is_stable() {
    let s = EditorSettings {
        custom_presets: vec![preset("a", "A")],
        ..Default::default()
    };
    let text = serde_json::to_string(&s).expect("ser");
    let back: EditorSettings = serde_json::from_str(&text).expect("de");
    assert_eq!(back, s);
}

#[test]
fn normalize_does_not_touch_valid_values() {
    let s = EditorSettings {
        autosave_s: 300,
        cache_max_gb: 10,
        decode_cache_mb: 256,
        preview_quality: "full".into(),
        default_canvas: "1:1".into(),
        ..Default::default()
    };
    let n = normalized(s.clone());
    assert_eq!(n, s);
}

#[test]
fn tempdir_paths_are_absolute() {
    // Sanity: the other tests rely on absolute tempdir paths.
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(Path::new(dir.path()).is_absolute());
}
