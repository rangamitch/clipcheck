//! Regression tests for issue #99: `Service::reload_config` must keep applying a config file
//! that contains unknown keys, and must report each unknown key as a CFG-P004 warning.
//!
//! Unknown keys are dropped on load, so the file still applies. A genuinely invalid value is
//! still rejected. The service runs in degraded mode (`no_encoders: true`), so no recorder,
//! capture or game is involved. Storage, logs and the library live in a temp dir, the detector
//! allowlist is empty, and the global hotkeys are removed.
//!
//! Windows only.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use clipcheck_core::config::schema::{Config, DetectionMode, Hotkeys};
use clipcheck_core::config::validate::{Diagnostic, Severity};
use clipcheck_library::Library;
use clipcheckd::events::EventBus;
use clipcheckd::service::{ConfigOutcome, Service, ServiceOptions};

/// A service running in degraded mode with everything outside `dir` disabled.
struct Harness {
    service: Service,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.service.stop();
    }
}

/// Config that cannot detect a game, write outside `dir`, or register a global hotkey.
fn isolated_config(dir: &Path) -> Config {
    let mut config = Config::default();
    config.storage.root = dir.join("recordings");
    config.advanced.log_dir = dir.join("logs");
    config.replay.location = dir.join("replay");
    config.detection.mode = DetectionMode::Allowlist;
    config.detection.allow = Vec::new();
    let mut hotkeys = Hotkeys::default();
    hotkeys.clips.clear();
    hotkeys.session.split = None;
    hotkeys.session.stop_start = None;
    hotkeys.session.toggle_buffer = None;
    hotkeys.session.mute_mic = None;
    config.hotkeys = hotkeys;
    config
}

fn start_degraded() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path().join("library.sqlite")).expect("open library");
    let bus = Arc::new(EventBus::new());
    let service = Service::start(ServiceOptions {
        config: isolated_config(dir.path()),
        data_dir: dir.path().to_path_buf(),
        library: Arc::new(Mutex::new(library)),
        bus,
        no_encoders: true,
    })
    .expect("start service");
    Harness { service, _dir: dir }
}

/// The serialized default-based config text that the tests edit and write to disk.
fn base_toml(dir_config: &Config) -> String {
    dir_config.to_toml_string().expect("serialize config")
}

/// Writes `text` to the config file the service reads, and returns that path.
fn write_config(h: &Harness, text: &str) -> String {
    let view = h.service.config().expect("config view");
    fs::write(Path::new(&view.path), text).expect("write config.toml");
    view.path
}

/// Inserts `line` on its own line directly after the first line equal to `header`.
fn insert_after_header(text: &str, header: &str, line: &str) -> String {
    let mut out = String::new();
    let mut found = false;
    for l in text.lines() {
        out.push_str(l);
        out.push('\n');
        if !found && l.trim() == header {
            out.push_str(line);
            out.push('\n');
            found = true;
        }
    }
    assert!(found, "header {header} not present in serialized config");
    out
}

/// The CFG-P004 warnings in `warnings`.
fn cfg_p004(warnings: &[Diagnostic]) -> Vec<&Diagnostic> {
    warnings.iter().filter(|d| d.code == "CFG-P004").collect()
}

/// Reloads and returns the warnings of an `Applied` outcome; panics on any other outcome.
fn reload_applied_warnings(h: &Harness) -> Vec<Diagnostic> {
    match h.service.reload_config().expect("reload_config answers") {
        ConfigOutcome::Applied { warnings, .. } => warnings,
        other => panic!("expected Applied, got {other:?}"),
    }
}

/// Issue #99: an unknown top-level key must not reject the file. The file applies and the
/// unknown key is reported once as a CFG-P004 warning that names the key.
#[test]
fn issue99_unknown_top_level_key_applies_with_one_cfg_p004_warning() {
    let h = start_degraded();
    let base = base_toml(&isolated_config(h._dir.path()));
    let text = format!("bogus_top_level_key = 1\n{base}");
    write_config(&h, &text);

    let warnings = reload_applied_warnings(&h);
    let p004 = cfg_p004(&warnings);
    assert_eq!(
        p004.len(),
        1,
        "expected exactly one CFG-P004 warning, got {warnings:?}"
    );
    assert_eq!(p004[0].severity, Severity::Warning);
    assert!(
        p004[0].path.contains("bogus_top_level_key"),
        "warning path must name the key, got {:?}",
        p004[0].path
    );
}

/// Issue #99: an unknown key inside the `[audio]` table must not reject the file. It is
/// reported as one CFG-P004 warning.
#[test]
fn issue99_unknown_key_in_audio_table_applies_with_one_cfg_p004_warning() {
    let h = start_degraded();
    let base = base_toml(&isolated_config(h._dir.path()));
    let text = insert_after_header(&base, "[audio]", "bogus_audio = 1");
    write_config(&h, &text);

    let warnings = reload_applied_warnings(&h);
    let p004 = cfg_p004(&warnings);
    assert_eq!(
        p004.len(),
        1,
        "expected exactly one CFG-P004 warning, got {warnings:?}"
    );
    assert_eq!(p004[0].severity, Severity::Warning);
    assert!(
        p004[0].path.contains("bogus_audio"),
        "warning path must name the key, got {:?}",
        p004[0].path
    );
}

/// Issue #99: a file with only known keys has no CFG-P004 warning.
#[test]
fn issue99_clean_file_has_no_cfg_p004_warning() {
    let h = start_degraded();
    let base = base_toml(&isolated_config(h._dir.path()));
    write_config(&h, &base);

    let warnings = reload_applied_warnings(&h);
    assert!(
        cfg_p004(&warnings).is_empty(),
        "a clean file must have no CFG-P004 warning, got {warnings:?}"
    );
}

/// Issue #99: a genuinely invalid value (a string where `general.autostart` needs a bool) is
/// still rejected, so the unknown-key handling does not turn errors into warnings.
#[test]
fn issue99_invalid_value_for_known_key_is_rejected() {
    let h = start_degraded();
    let base = base_toml(&isolated_config(h._dir.path()));
    let before = base.clone();
    let text = insert_after_header(&base, "[general]", "autostart = \"yes\"");
    // The inserted line duplicates the existing `autostart` key in [general], which is also
    // invalid TOML. Remove the original key so the only fault left is the wrong type.
    let text: String = text
        .lines()
        .filter(|l| !(l.trim_start().starts_with("autostart = ") && !l.contains("\"yes\"")))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(text, before, "test edit must change the file");
    write_config(&h, &text);

    match h.service.reload_config().expect("reload_config answers") {
        ConfigOutcome::Rejected(diags) => {
            assert!(
                diags.iter().any(|d| d.severity == Severity::Error),
                "rejection must carry an error diagnostic, got {diags:?}"
            );
        }
        other => panic!("expected Rejected for a wrong-typed value, got {other:?}"),
    }
}
