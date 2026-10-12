//! Regression tests for issues #86, #93 and #161 (clipcheckd).
//!
//! - #86: startup config loading and reload share `ConfigStore::load_checked`.
//! - #93: the tray menu is typed (`MenuItem`), and an unknown command id is not a menu item.
//! - #161: rename, delete and register-edited recording commands are answered by the daemon
//!   (never `not_implemented`). The daemon tests are Windows only and use a temp data dir.

use std::fs;
use std::path::PathBuf;

use clipcheck_core::config::schema::Config;
use clipcheck_core::config::validate::Severity;
use clipcheckd::app::load_config;
use clipcheckd::configstore::ConfigStore;
use clipcheckd::tray::{menu_items, MenuItem, MENU_ID_EXIT, MENU_ID_OPEN};

/// Writes `text` to `config.toml` in a fresh temp dir and returns the dir guard and path.
fn config_file(text: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, text).expect("write config");
    (dir, path)
}

// ---- #86: one load and validate path for startup and reload ----

#[test]
fn issue_86_valid_file_loads_the_same_at_startup_and_on_reload() {
    let (_dir, path) = config_file(Config::default_toml());
    let store = ConfigStore::new(path.clone());

    let (checked, diagnostics) = store.load_checked();
    assert!(
        !diagnostics.iter().any(|d| d.severity == Severity::Error),
        "default config must have no error diagnostics: {diagnostics:?}"
    );
    let checked = checked.expect("valid file must load");
    assert_eq!(
        load_config(&path),
        checked,
        "startup load must return the config that load_checked accepts"
    );
    assert_eq!(
        store.read().expect("read succeeds for a valid file"),
        checked,
        "ConfigStore::read and load_checked must agree"
    );
}

#[test]
fn issue_86_syntax_error_is_rejected_by_both_paths() {
    let original = "this is = not [valid toml";
    let (_dir, path) = config_file(original);
    let store = ConfigStore::new(path.clone());

    let (config, diagnostics) = store.load_checked();
    assert!(config.is_none(), "a file with a syntax error must not load");
    assert_eq!(
        diagnostics.len(),
        1,
        "one CFG-P001 diagnostic: {diagnostics:?}"
    );
    assert_eq!(diagnostics[0].code, "CFG-P001");
    assert_eq!(diagnostics[0].severity, Severity::Error);

    assert_eq!(
        load_config(&path),
        Config::default(),
        "startup must fall back to defaults on a syntax error"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read back"),
        original,
        "startup must leave a broken file untouched"
    );
}

#[test]
fn issue_86_validation_error_is_rejected_by_both_paths() {
    // config_version above the supported maximum is error CFG-P003.
    let (_dir, path) = config_file("config_version = 999\n");
    let store = ConfigStore::new(path.clone());

    let (config, diagnostics) = store.load_checked();
    assert!(
        config.is_none(),
        "a file with validation errors must not load"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "CFG-P003" && d.severity == Severity::Error),
        "expected a CFG-P003 error: {diagnostics:?}"
    );
    assert_eq!(
        load_config(&path),
        Config::default(),
        "startup must fall back to defaults when validation has errors"
    );
}

#[test]
fn issue_86_missing_file_is_a_cfg_p001_error_from_load_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConfigStore::new(dir.path().join("absent.toml"));

    let (config, diagnostics) = store.load_checked();
    assert!(config.is_none(), "a missing file has no config to load");
    assert_eq!(diagnostics.len(), 1, "one read diagnostic: {diagnostics:?}");
    assert_eq!(diagnostics[0].code, "CFG-P001");
    assert_eq!(diagnostics[0].severity, Severity::Error);
}

#[test]
fn issue_86_load_checked_and_read_agree_on_syntax_errors() {
    let (_dir, path) = config_file("[general\n");
    let store = ConfigStore::new(path);

    let read_diags = store.read().expect_err("read must fail on bad TOML");
    let (config, checked_diags) = store.load_checked();
    assert!(config.is_none());
    assert_eq!(
        checked_diags, read_diags,
        "load_checked must return exactly the read diagnostics on a read failure"
    );
}

// ---- #93: typed tray menu ----

#[test]
fn issue_93_menu_items_are_typed_in_display_order() {
    assert_eq!(
        menu_items(),
        [(MenuItem::Open, "Open ClipCheck"), (MenuItem::Exit, "Exit")]
    );
    assert_eq!(MenuItem::ALL, [MenuItem::Open, MenuItem::Exit]);
}

#[test]
fn issue_93_menu_item_ids_match_the_command_constants() {
    assert_eq!(MenuItem::Open.id(), MENU_ID_OPEN);
    assert_eq!(MenuItem::Exit.id(), MENU_ID_EXIT);
    assert_ne!(
        MenuItem::Open.id(),
        MenuItem::Exit.id(),
        "Open and Exit must not share a command id"
    );
}

#[test]
fn issue_93_from_id_round_trips_every_item() {
    for item in MenuItem::ALL {
        assert_eq!(
            MenuItem::from_id(item.id()),
            Some(item),
            "{item:?} must map back from its own id"
        );
    }
}

#[test]
fn issue_93_unknown_ids_are_not_menu_items() {
    // None of these may resolve to Exit, the action that can stop recording.
    for id in [0u16, 3, 100, u16::MAX] {
        assert_eq!(
            MenuItem::from_id(id),
            None,
            "command id {id} must not resolve to a menu item"
        );
    }
}

// ---- #161: recording management commands are implemented ----

#[cfg(windows)]
mod common;

mod recording_commands {
    use crate::common::*;

    use clipcheck_ipc::proto::{Command as Cmd, ErrorCode, Reply, Response};

    /// Asserts the reply is an error with `code`, and that it is not `not_implemented`.
    fn assert_error_code(resp: &Response, code: ErrorCode, what: &str) {
        match &resp.body {
            Reply::Error {
                code: got, message, ..
            } => {
                assert_ne!(
                    *got,
                    ErrorCode::NotImplemented,
                    "{what} must be implemented (got not_implemented: {message:?})"
                );
                assert_eq!(
                    *got, code,
                    "{what}: wrong error code, message was {message:?}"
                );
            }
            other => panic!("{what}: expected an error reply, got {other:?}"),
        }
    }

    #[test]
    fn issue_161_rename_unknown_recording_is_bad_request() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
        let mut conn = connect_and_hello(&mut d, &pipe);

        let resp = call(
            &mut conn,
            2,
            Cmd::RenameRecording {
                recording_id: "no-such-recording".into(),
                name: "New name".into(),
            },
        );
        assert_error_code(
            &resp,
            ErrorCode::BadRequest,
            "RenameRecording with an unknown id",
        );
    }

    #[test]
    fn issue_161_delete_unknown_recording_is_bad_request() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
        let mut conn = connect_and_hello(&mut d, &pipe);

        let resp = call(
            &mut conn,
            2,
            Cmd::DeleteRecording {
                recording_id: "no-such-recording".into(),
            },
        );
        assert_error_code(
            &resp,
            ErrorCode::BadRequest,
            "DeleteRecording with an unknown id",
        );
    }

    #[test]
    fn issue_161_register_edited_relative_path_is_bad_request() {
        let _g = lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pipe = unique_pipe();
        let mut d = spawn_with(dir.path(), &pipe, &["--no-encoders"]);
        let mut conn = connect_and_hello(&mut d, &pipe);

        let resp = call(
            &mut conn,
            2,
            Cmd::RegisterEditedRecording {
                source_recording_id: None,
                path: "relative\\edited.mp4".into(),
                keep_source_time: false,
                game: None,
            },
        );
        assert_error_code(
            &resp,
            ErrorCode::BadRequest,
            "RegisterEditedRecording with a relative path",
        );

        // The daemon must stay up after the refused commands.
        match call(&mut conn, 3, Cmd::Ping).body {
            Reply::Pong { .. } => {}
            other => panic!("expected Pong after the refused commands, got {other:?}"),
        }
    }
}
