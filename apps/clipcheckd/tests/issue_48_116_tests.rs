//! Tests for issue #48 (a patch must not destroy a broken config.toml) and issue #116
//! (CLIPCHECK_LOG level filter and log flushing).
//!
//! Each test spawns the real `clipcheckd` binary with a temp data dir and a unique pipe name,
//! and talks to it over the named pipe. The single-instance mutex is session-global, so every
//! test takes `common::lock()`. A watchdog kills any live daemon and exits the process if a test
//! runs longer than 60 seconds.

#![cfg(windows)]

mod common;

use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clipcheck_core::config::schema::Config;
use clipcheck_core::config::validate::{validate, Severity};
use clipcheck_ipc::proto::{Command as Cmd, ErrorCode, Reply};
use clipcheck_ipc::PipeConn;
use clipcheckd::logging::{is_enabled, parse_level, Level};
use serde_json::{json, Value};

use common::{
    assert_pong, call, connect_and_hello, live_pids, lock, spawn_env, unique_pipe, wait_exit,
    Daemon,
};

const INVALID_TOML: &str = "this is = = not toml [[[\n";
const CONFIG: &str = "config.toml";
const BROKEN_PREFIX: &str = "config.toml.broken-";

/// Fails the whole test binary after 60 seconds, killing any live daemon first.
/// Dropping the guard stops the timer.
struct Watchdog {
    _done: mpsc::Sender<()>,
}

fn watchdog(name: &'static str) -> Watchdog {
    let (tx, rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(60)) {
            eprintln!("WATCHDOG: {name} exceeded 60 s; killing daemons and exiting");
            for pid in live_pids() {
                let _ = Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .status();
            }
            std::process::exit(101);
        }
    });
    Watchdog { _done: tx }
}

/// Sends a PatchConfig (not dry run) and returns the reply body.
fn patch_reply(conn: &mut PipeConn, id: u64, patch: Value) -> Reply {
    call(
        conn,
        id,
        Cmd::PatchConfig {
            patch,
            dry_run: false,
        },
    )
    .body
}

fn assert_applied(reply: Reply) {
    assert!(
        matches!(reply, Reply::ConfigApplied { .. }),
        "expected ConfigApplied, got {reply:?}"
    );
}

/// Sends Shutdown over IPC, checks the ack, and waits for a zero exit.
fn shutdown_and_expect_exit(d: &mut Daemon, conn: &mut PipeConn, id: u64) -> ExitStatus {
    let resp = call(
        conn,
        id,
        Cmd::Shutdown {
            reason: None,
            mode: None,
        },
    );
    assert!(
        matches!(resp.body, Reply::ShutdownAck),
        "expected ShutdownAck, got {:?}",
        resp.body
    );
    let status = wait_exit(d, Duration::from_secs(10));
    assert!(status.success(), "daemon exit status was {status:?}");
    status
}

/// Every `config.toml.broken-<ms>` file in `dir`, oldest first, with its ms stamp.
fn broken_backups(dir: &Path) -> Vec<(u128, PathBuf)> {
    let mut found: Vec<(u128, PathBuf)> = fs::read_dir(dir)
        .expect("read data dir")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let ms = path
                .file_name()?
                .to_str()?
                .strip_prefix(BROKEN_PREFIX)?
                .parse::<u128>()
                .ok()?;
            Some((ms, path))
        })
        .collect();
    found.sort();
    found
}

/// Every file in `dir` whose name contains "broken".
fn any_broken_names(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("read data dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("broken"))
        .collect()
}

/// Asserts that `path` loads as a Config with no validation errors.
fn assert_config_healthy(path: &Path) -> Config {
    let text = fs::read_to_string(path).expect("read config.toml");
    let config = Config::from_toml_str(&text)
        .unwrap_or_else(|e| panic!("config.toml is not valid TOML: {e}\n{text}"));
    let errors: Vec<_> = validate(&config)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "config.toml has errors: {errors:?}");
    config
}

fn read_log(data_dir: &Path) -> String {
    fs::read_to_string(data_dir.join("logs").join("clipcheckd.log")).unwrap_or_default()
}

/// The level label of a log line (`<timestamp> <LABEL> pid=... message`).
fn label_of(line: &str) -> Option<&str> {
    line.split_whitespace().nth(1)
}

fn has_label(log: &str, label: &str) -> bool {
    log.lines().any(|line| label_of(line) == Some(label))
}

// ===================================================================================
// Issue #48: a patch over a broken config.toml keeps a backup of the broken file.
// ===================================================================================

#[test]
fn issue48_a_invalid_toml_is_backed_up_before_patch() {
    let _g = lock();
    let _wd = watchdog("issue48_a");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    let garbage = INVALID_TOML.as_bytes().to_vec();
    fs::write(&cfg, &garbage).expect("write invalid config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);
    assert_pong(&mut conn, 2);

    assert_applied(patch_reply(
        &mut conn,
        3,
        json!({"replay": {"max_seconds": 120}}),
    ));

    let backups = broken_backups(dir.path());
    assert_eq!(backups.len(), 1, "expected one backup, found {backups:?}");
    assert_eq!(
        fs::read(&backups[0].1).expect("read backup"),
        garbage,
        "backup bytes differ from the original broken file"
    );
    let saved = assert_config_healthy(&cfg);
    assert_eq!(saved.replay.max_seconds, 120, "patch was not written");
    shutdown_and_expect_exit(&mut d, &mut conn, 4);
}

#[test]
fn issue48_b_validation_failing_config_is_backed_up_before_patch() {
    let _g = lock();
    let _wd = watchdog("issue48_b");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    // The issue's example key `[clips] length_s` does not exist in this schema, so the
    // out-of-range `replay.max_seconds` (valid range 30..=3600) is used instead.
    let failing = "[replay]\nmax_seconds = 99999\n";
    fs::write(&cfg, failing).expect("write failing config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);

    assert_applied(patch_reply(
        &mut conn,
        2,
        json!({"replay": {"max_seconds": 120}}),
    ));

    let backups = broken_backups(dir.path());
    assert_eq!(backups.len(), 1, "expected one backup, found {backups:?}");
    assert_eq!(
        fs::read(&backups[0].1).expect("read backup"),
        failing.as_bytes(),
        "backup bytes differ from the original failing file"
    );
    assert_config_healthy(&cfg);
    shutdown_and_expect_exit(&mut d, &mut conn, 3);
}

#[test]
fn issue48_c_healthy_config_makes_no_backup() {
    let _g = lock();
    let _wd = watchdog("issue48_c");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    fs::write(&cfg, Config::default_toml()).expect("write healthy config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);

    assert_applied(patch_reply(
        &mut conn,
        2,
        json!({"replay": {"max_seconds": 120}}),
    ));

    let names = any_broken_names(dir.path());
    assert!(names.is_empty(), "healthy config produced files: {names:?}");
    assert_config_healthy(&cfg);
    shutdown_and_expect_exit(&mut d, &mut conn, 3);
}

#[test]
fn issue48_d_seven_broken_cycles_keep_newest_five() {
    let _g = lock();
    let _wd = watchdog("issue48_d");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);

    let pipe = unique_pipe();
    fs::write(&cfg, Config::default_toml()).expect("write healthy config");
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);

    // Each cycle re-breaks the file while the daemon runs. Save checks the disk, so no
    // restart is needed.
    let mut contents: Vec<Vec<u8>> = Vec::new();
    for cycle in 1..=7u32 {
        let garbage = format!("# cycle {cycle}\n{INVALID_TOML}");
        fs::write(&cfg, garbage.as_bytes()).expect("re-break config");
        contents.push(garbage.into_bytes());
        assert_applied(patch_reply(
            &mut conn,
            10 + u64::from(cycle),
            // Must stay above the default hotkey clip length (60 s), or validation rejects it.
            json!({"replay": {"max_seconds": 300 + cycle}}),
        ));
    }

    let backups = broken_backups(dir.path());
    assert_eq!(
        backups.len(),
        5,
        "expected exactly 5 backups, found {backups:?}"
    );
    let bodies: Vec<Vec<u8>> = backups
        .iter()
        .map(|(_, p)| fs::read(p).expect("read backup"))
        .collect();
    assert_eq!(
        bodies,
        contents[2..].to_vec(),
        "kept backups are not the newest five (cycles 3..=7)"
    );
    shutdown_and_expect_exit(&mut d, &mut conn, 99);
}

#[test]
fn issue48_e_empty_file_does_not_crash() {
    let _g = lock();
    let _wd = watchdog("issue48_e_empty");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    fs::write(&cfg, b"").expect("write empty config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);
    assert_pong(&mut conn, 2);
    assert!(
        d.child.try_wait().expect("try_wait").is_none(),
        "daemon exited on an empty config"
    );

    assert_applied(patch_reply(
        &mut conn,
        3,
        json!({"replay": {"max_seconds": 120}}),
    ));
    // Observation only: an empty file is valid TOML, so no backup is expected.
    let backups = broken_backups(dir.path());
    eprintln!(
        "OBSERVED issue48_e empty file: {} backup(s) made",
        backups.len()
    );
    assert_config_healthy(&cfg);
    shutdown_and_expect_exit(&mut d, &mut conn, 4);
}

#[test]
fn issue48_e_binary_garbage_is_backed_up_and_does_not_crash() {
    let _g = lock();
    let _wd = watchdog("issue48_e_binary");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    // Not valid UTF-8, so the file cannot be read as text.
    let binary: Vec<u8> = vec![
        0xFF, 0xFE, 0x00, 0x9C, 0xC3, 0x28, 0x80, 0x00, 0x0D, 0x0A, 0xFF,
    ];
    fs::write(&cfg, &binary).expect("write binary config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);
    assert_pong(&mut conn, 2);

    assert_applied(patch_reply(
        &mut conn,
        3,
        json!({"replay": {"max_seconds": 120}}),
    ));
    assert!(
        d.child.try_wait().expect("try_wait").is_none(),
        "daemon exited on binary config"
    );

    let backups = broken_backups(dir.path());
    assert_eq!(backups.len(), 1, "expected one backup, found {backups:?}");
    assert_eq!(
        fs::read(&backups[0].1).expect("read backup"),
        binary,
        "backup bytes differ from the binary original"
    );
    assert_config_healthy(&cfg);
    shutdown_and_expect_exit(&mut d, &mut conn, 4);
}

#[test]
fn issue48_f_config_held_open_refuses_overwrite() {
    let _g = lock();
    let _wd = watchdog("issue48_f_locked");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    let garbage = INVALID_TOML.as_bytes().to_vec();
    fs::write(&cfg, &garbage).expect("write invalid config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);

    // Hold the file with no sharing: the backup copy cannot read it, so nothing may be written.
    let reply = {
        let _hold = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&cfg)
            .expect("hold config file");
        patch_reply(&mut conn, 2, json!({"replay": {"max_seconds": 120}}))
    };
    match &reply {
        Reply::Error { code, .. } => assert_eq!(*code, ErrorCode::Internal, "reply {reply:?}"),
        other => panic!("expected Error(Internal) when backup fails, got {other:?}"),
    }
    assert_eq!(
        fs::read(&cfg).expect("read config"),
        garbage,
        "config.toml was overwritten although the backup failed"
    );
    assert!(
        broken_backups(dir.path()).is_empty(),
        "a backup was reported although the copy failed"
    );
    assert_pong(&mut conn, 3);
    shutdown_and_expect_exit(&mut d, &mut conn, 4);
}

#[test]
fn issue48_f_read_only_config_observed() {
    let _g = lock();
    let _wd = watchdog("issue48_f_readonly");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    let garbage = INVALID_TOML.as_bytes().to_vec();
    fs::write(&cfg, &garbage).expect("write invalid config");
    let mut perms = fs::metadata(&cfg).expect("metadata").permissions();
    perms.set_readonly(true);
    fs::set_permissions(&cfg, perms).expect("set read-only");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);

    let reply = patch_reply(&mut conn, 2, json!({"replay": {"max_seconds": 120}}));
    eprintln!(
        "OBSERVED issue48_f read-only: reply = {reply:?}, backups = {}, stray = {:?}",
        broken_backups(dir.path()).len(),
        any_broken_names(dir.path())
    );
    assert_eq!(
        fs::read(&cfg).expect("read config"),
        garbage,
        "read-only config.toml was overwritten"
    );
    assert_pong(&mut conn, 3);

    let mut perms = fs::metadata(&cfg).expect("metadata").permissions();
    // Windows only: clearing the read-only flag so the temp dir can be removed.
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&cfg, perms).expect("clear read-only");
    shutdown_and_expect_exit(&mut d, &mut conn, 4);
}

// ===================================================================================
// Issue #116: logging level filter, IPC-visible behaviour and flushing.
// ===================================================================================

#[test]
fn issue116_parse_level_table() {
    let cases: [(&str, Option<Level>); 7] = [
        ("debug", Some(Level::Debug)),
        ("INFO", Some(Level::Info)),
        ("Warn", Some(Level::Warn)),
        ("error", Some(Level::Error)),
        (" warn ", Some(Level::Warn)),
        ("", None),
        ("x", None),
    ];
    for (text, expected) in cases {
        assert_eq!(parse_level(text), expected, "parse_level({text:?})");
    }
}

#[test]
fn issue116_level_filter_drops_below_minimum() {
    assert!(is_enabled(Level::Error, Level::Warn));
    assert!(is_enabled(Level::Warn, Level::Warn));
    assert!(!is_enabled(Level::Info, Level::Warn));
    assert!(!is_enabled(Level::Debug, Level::Info));
    assert!(is_enabled(Level::Debug, Level::Debug));
}

#[test]
fn issue116_warn_level_has_no_info_but_has_warn() {
    let _g = lock();
    let _wd = watchdog("issue116_warn");
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(CONFIG);
    fs::write(&cfg, INVALID_TOML).expect("write invalid config");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[("CLIPCHECK_LOG", "warn")], None);
    let mut conn = connect_and_hello(&mut d, &pipe);
    // The patch provokes a WARN that names the broken-config backup.
    assert_applied(patch_reply(
        &mut conn,
        2,
        json!({"replay": {"max_seconds": 120}}),
    ));
    shutdown_and_expect_exit(&mut d, &mut conn, 3);

    let log = read_log(dir.path());
    assert!(
        !has_label(&log, "INFO"),
        "CLIPCHECK_LOG=warn still wrote INFO lines:\n{log}"
    );
    assert!(has_label(&log, "WARN"), "no WARN line in log:\n{log}");
    assert!(
        log.lines()
            .any(|l| label_of(l) == Some("WARN") && l.contains(BROKEN_PREFIX)),
        "no WARN line names the broken-config backup:\n{log}"
    );
}

#[test]
fn issue116_default_level_has_info_and_no_debug() {
    let _g = lock();
    let _wd = watchdog("issue116_default");
    let dir = tempfile::tempdir().expect("tempdir");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);
    shutdown_and_expect_exit(&mut d, &mut conn, 2);

    let log = read_log(dir.path());
    assert!(
        log.lines()
            .any(|l| label_of(l) == Some("INFO") && l.contains("starting")),
        "default level has no INFO 'starting' line:\n{log}"
    );
    assert!(
        !has_label(&log, "DEBUG"),
        "default level wrote DEBUG lines:\n{log}"
    );
}

#[test]
fn issue116_debug_level_observed() {
    let _g = lock();
    let _wd = watchdog("issue116_debug");
    let dir = tempfile::tempdir().expect("tempdir");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[("CLIPCHECK_LOG", "debug")], None);
    // Disconnecting a client logs "IPC client disconnected" at DEBUG.
    {
        let _first = connect_and_hello(&mut d, &pipe);
    }
    let mut conn = connect_and_hello(&mut d, &pipe);
    shutdown_and_expect_exit(&mut d, &mut conn, 2);

    let log = read_log(dir.path());
    eprintln!(
        "OBSERVED issue116 debug: DEBUG lines present = {}",
        has_label(&log, "DEBUG")
    );
    assert!(
        has_label(&log, "DEBUG"),
        "CLIPCHECK_LOG=debug wrote no DEBUG line after a client disconnect:\n{log}"
    );
    assert!(
        log.lines()
            .any(|l| label_of(l) == Some("INFO") && l.contains("starting")),
        "debug level lost the INFO 'starting' line:\n{log}"
    );
}

#[test]
fn issue116_invalid_level_is_ignored_and_daemon_starts() {
    let _g = lock();
    let _wd = watchdog("issue116_loud");
    let dir = tempfile::tempdir().expect("tempdir");
    let stderr_path = dir.path().join("stderr-capture.txt");

    let pipe = unique_pipe();
    let mut d = spawn_env(
        dir.path(),
        &pipe,
        &[("CLIPCHECK_LOG", "loud")],
        Some(&stderr_path),
    );
    let mut conn = connect_and_hello(&mut d, &pipe);
    assert_pong(&mut conn, 2);
    shutdown_and_expect_exit(&mut d, &mut conn, 3);

    let log = read_log(dir.path());
    assert!(
        log.lines()
            .any(|l| label_of(l) == Some("INFO") && l.contains("starting")),
        "invalid CLIPCHECK_LOG did not fall back to the default level:\n{log}"
    );
    let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        stderr.contains("ignoring CLIPCHECK_LOG=loud"),
        "no stderr notice about the invalid value; stderr was:\n{stderr}"
    );
}

#[test]
fn issue116_shutdown_flushes_final_lines() {
    let _g = lock();
    let _wd = watchdog("issue116_flush");
    let dir = tempfile::tempdir().expect("tempdir");

    let pipe = unique_pipe();
    let mut d = spawn_env(dir.path(), &pipe, &[], None);
    let mut conn = connect_and_hello(&mut d, &pipe);
    let status = shutdown_and_expect_exit(&mut d, &mut conn, 2);
    assert!(status.success());

    let log = read_log(dir.path());
    assert!(
        log.contains("shutdown requested by IPC client"),
        "log lacks the IPC shutdown line:\n{log}"
    );
    assert!(
        log.contains("shutdown requested; exiting"),
        "buffered final exit line was not flushed:\n{log}"
    );
}
