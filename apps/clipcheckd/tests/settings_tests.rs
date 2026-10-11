//! Settings tests: spawn the real `clipcheckd` and drive `PatchConfig`, `GetConfig`,
//! `ReloadConfig` and `GetStatus` over the named pipe.
//!
//! Windows only. The config tests need neither test media nor a desktop.
//! `status_tracks_game_and_session_split` and `hotkey_change_applies_live` need native test media
//! (`clipcheck_testkit::available()`) and an interactive desktop for their test window. Missing test media is a
//! `SKIPPED:` report (`common::skip_no_encoders`); a window that cannot be created is a failure.
//!
//! The Win32 window, SendInput helpers and the IPC `Client` live in `tests/common`.

#![cfg(windows)]

mod common;

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_core::config::schema::Config;
use clipcheck_ipc::proto::{
    Command as Cmd, DiagSeverity, DiagnosticDto, ErrorCode, Event, RecState, Reply, StatusDto,
    Topic,
};
use serde_json::{json, Value};

use common::window::{press_chord, TestWindow, VK_CONTROL, VK_F20, VK_MENU, VK_SHIFT};
use common::*;

/// Writes `<data_dir>/config.toml` for the recording tests. The test exe is the only allowlisted
/// game, and `chord` is the single clip hotkey. Output stays under `data_dir`.
fn write_config(data_dir: &Path, exe: &str, chord: &str) {
    let recordings = data_dir.join("recordings");
    let logs = data_dir.join("logs");
    let text = format!(
        r#"[storage]
root = '{root}'

[detection]
mode = "allowlist"
allow = ["{exe}"]
auto_record = true
stop_grace_s = 3

[replay]
enabled = true
max_seconds = 60
location = ""

[video]
encoder = "software"
codec = "h264"
preset = "low"

[[hotkeys.clips]]
name = "test"
keys = "{chord}"
seconds = 5

[advanced]
detect_poll_ms = 250
log_dir = '{logs}'
"#,
        root = recordings.display(),
        logs = logs.display(),
        exe = exe,
        chord = chord,
    );
    fs::write(data_dir.join("config.toml"), text).expect("failed to write config.toml");
}

/// Polls status until `pred` holds or `timeout` expires.
fn wait_status(
    c: &mut Client,
    timeout: Duration,
    what: &str,
    pred: impl Fn(&StatusDto) -> bool,
) -> Box<StatusDto> {
    let deadline = Instant::now() + timeout;
    loop {
        let s = c.status();
        if pred(&s) {
            return s;
        }
        if Instant::now() >= deadline {
            panic!("timed out after {timeout:?} waiting for {what}; last status: {s:?}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Waits until the encoder probe has finished and the recorder is up. Any degraded reason other
/// than "encoder probe in progress" is a failure.
fn wait_ready(c: &mut Client, data_dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = c.status();
        let reason = s.degraded_reason.clone();
        match reason.as_deref() {
            Some(r) if !r.contains("in progress") => panic!(
                "recorder did not start: degraded_reason = {r:?}\nlog:\n{}",
                log_text(data_dir)
            ),
            None if s.state != RecState::Degraded => return,
            _ => {}
        }
        if Instant::now() >= deadline {
            panic!(
                "encoder probe did not finish within 30 s; last status: {s:?}\nlog:\n{}",
                log_text(data_dir)
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Reads `config.toml` bytes from the data directory.
fn config_bytes(data_dir: &Path) -> Vec<u8> {
    fs::read(data_dir.join("config.toml")).expect("read config.toml")
}

/// Parses `config.toml` from the data directory. Panics if it is not a valid config.
fn parse_file(data_dir: &Path) -> Config {
    let text = fs::read_to_string(data_dir.join("config.toml")).expect("read config.toml");
    Config::from_toml_str(&text)
        .unwrap_or_else(|e| panic!("config.toml does not parse: {e}\n{text}"))
}

// ---------------------------------------------------------------------------
// Reply helpers
// ---------------------------------------------------------------------------

const VALID_CLASSES: [&str; 6] = [
    "live",
    "next_part",
    "pipeline_reinit",
    "audio_reinit",
    "replay_flush",
    "restart",
];

fn patch(c: &mut Client, patch: Value) -> Reply {
    c.call(Cmd::PatchConfig {
        patch,
        dry_run: false,
    })
}

fn dry_run(c: &mut Client, patch: Value) -> Reply {
    c.call(Cmd::PatchConfig {
        patch,
        dry_run: true,
    })
}

/// `(apply_class, restart_required, diagnostics)` of a `ConfigApplied` reply.
fn expect_applied(reply: Reply, what: &str) -> (String, bool, Vec<DiagnosticDto>) {
    match reply {
        Reply::ConfigApplied {
            apply_class,
            restart_required,
            diagnostics,
        } => (apply_class, restart_required, diagnostics),
        other => panic!("{what}: expected ConfigApplied, got {other:?}"),
    }
}

/// Diagnostics of a `ConfigRejected` reply.
fn expect_rejected(reply: Reply, what: &str) -> Vec<DiagnosticDto> {
    match reply {
        Reply::ConfigRejected { diagnostics } => diagnostics,
        other => panic!("{what}: expected ConfigRejected, got {other:?}"),
    }
}

/// Message of a `BadRequest` error that is not retryable.
fn expect_bad_request(reply: Reply, what: &str) -> String {
    match reply {
        Reply::Error {
            code,
            message,
            retryable,
        } => {
            assert_eq!(
                code,
                ErrorCode::BadRequest,
                "{what}: wrong code, message {message:?}"
            );
            assert!(!retryable, "{what}: BadRequest must not be retryable");
            message
        }
        other => panic!("{what}: expected Error(BadRequest), got {other:?}"),
    }
}

/// True when `diags` holds an error whose path is `path`.
fn has_error_at(diags: &[DiagnosticDto], path: &str) -> bool {
    diags
        .iter()
        .any(|d| d.path == path && d.severity == DiagSeverity::Error)
}

const VK_F21: u16 = 0x84;

/// Polls status until a game is attached. Returns the game name.
fn wait_for_game(c: &mut Client, data: &Path) -> String {
    let s = wait_status(c, Duration::from_secs(30), "game_info", |s| {
        s.game_info.is_some()
    });
    let name = s
        .game_info
        .as_ref()
        .map(|g| g.name.clone())
        .unwrap_or_default();
    assert!(
        !name.is_empty(),
        "game_info.name is empty: {s:?}\nlog:\n{}",
        log_text(data)
    );
    name
}

// ---------------------------------------------------------------------------
// Config tests
// ---------------------------------------------------------------------------

/// A valid change is applied, saved, read back by GetConfig, and announced on the Settings topic.
#[test]
fn patch_applies_saves_and_emits_event() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Settings]);

    let (_, version, path) = c.get_config();
    assert_eq!(version, 1, "config_version");
    assert!(path.ends_with("config.toml"), "GetConfig path was {path:?}");

    let (class, restart, _) = expect_applied(
        patch(&mut c, json!({"replay": {"max_seconds": 120}})),
        "patch max_seconds 120",
    );
    assert!(
        VALID_CLASSES.contains(&class.as_str()),
        "apply_class {class:?} is not a known class"
    );
    assert!(!restart, "restart_required is true with no game attached");

    assert_eq!(
        parse_file(dir.path()).replay.max_seconds,
        120,
        "config.toml on disk after the patch"
    );

    let (cfg, version, _) = c.get_config();
    assert_eq!(
        cfg["replay"]["max_seconds"],
        json!(120),
        "GetConfig after patch"
    );
    assert_eq!(version, 1, "config_version after patch");

    let ev = c.wait_event(Duration::from_secs(5), "config_changed event", |e| {
        matches!(e, Event::ConfigChanged { .. })
    });
    match ev {
        Event::ConfigChanged {
            apply_class,
            restart_required,
        } => {
            assert_eq!(apply_class, class, "event apply_class differs from reply");
            assert!(!restart_required, "event restart_required is true");
        }
        other => panic!("expected ConfigChanged, got {other:?}"),
    }
}

/// An out-of-range value is rejected with a diagnostic on its key; the file is byte-identical.
#[test]
fn patch_out_of_range_is_rejected_and_file_unchanged() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    let before_value = c.config()["replay"]["max_seconds"].clone();
    let before_bytes = config_bytes(dir.path());

    let diags = expect_rejected(
        patch(&mut c, json!({"replay": {"max_seconds": 5}})),
        "patch max_seconds 5",
    );
    assert!(
        has_error_at(&diags, "replay.max_seconds"),
        "no error diagnostic at replay.max_seconds: {diags:?}"
    );
    assert_eq!(
        config_bytes(dir.path()),
        before_bytes,
        "config.toml changed after a rejected patch"
    );
    assert_eq!(
        c.config()["replay"]["max_seconds"],
        before_value,
        "GetConfig changed after a rejected patch"
    );
}

/// dry_run validates only: a valid change returns ConfigChecked, an invalid one ConfigRejected.
/// Neither writes the file nor changes GetConfig.
#[test]
fn dry_run_writes_nothing() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    let before_value = c.config()["replay"]["max_seconds"].clone();
    let before_bytes = config_bytes(dir.path());

    match dry_run(&mut c, json!({"replay": {"max_seconds": 120}})) {
        Reply::ConfigChecked { apply_class, .. } => {
            assert!(
                VALID_CLASSES.contains(&apply_class.as_str()),
                "apply_class {apply_class:?} is not a known class"
            );
        }
        other => panic!("dry_run 120: expected ConfigChecked, got {other:?}"),
    }
    assert_eq!(
        config_bytes(dir.path()),
        before_bytes,
        "dry_run 120 changed config.toml"
    );
    assert_eq!(
        c.config()["replay"]["max_seconds"],
        before_value,
        "dry_run 120 changed GetConfig"
    );

    let diags = expect_rejected(
        dry_run(&mut c, json!({"replay": {"max_seconds": 5}})),
        "dry_run 5",
    );
    assert!(
        has_error_at(&diags, "replay.max_seconds"),
        "dry_run 5: no error diagnostic at replay.max_seconds: {diags:?}"
    );
    assert_eq!(
        config_bytes(dir.path()),
        before_bytes,
        "dry_run 5 changed config.toml"
    );
}

/// An unknown key and a type mismatch are BadRequest errors, not retryable, and name the key.
/// The file is unchanged.
#[test]
fn unknown_key_and_type_mismatch_are_bad_requests() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    let before_bytes = config_bytes(dir.path());

    let msg = expect_bad_request(
        patch(&mut c, json!({"replay": {"no_such_key": 1}})),
        "unknown key",
    );
    assert!(
        msg.contains("replay.no_such_key"),
        "message does not name the key: {msg:?}"
    );

    let msg = expect_bad_request(
        patch(&mut c, json!({"replay": {"max_seconds": "long"}})),
        "type mismatch",
    );
    assert!(
        msg.contains("replay.max_seconds"),
        "message does not name the key: {msg:?}"
    );

    assert_eq!(
        config_bytes(dir.path()),
        before_bytes,
        "config.toml changed after a bad request"
    );
}

/// Patches merge deeply: sibling keys survive. Arrays replace. `null` clears an optional field.
#[test]
fn deep_merge_arrays_replace_and_null_clears() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    // Deep merge: change one nested key, siblings keep their values.
    let before = c.config();
    let _ = expect_applied(
        patch(&mut c, json!({"replay": {"max_seconds": 120}})),
        "deep merge patch",
    );
    let after = c.config();
    assert_eq!(after["replay"]["max_seconds"], json!(120), "changed key");
    assert_eq!(
        after["replay"]["enabled"], before["replay"]["enabled"],
        "sibling replay.enabled changed"
    );
    assert_eq!(
        after["storage"], before["storage"],
        "untouched section storage changed"
    );

    // Arrays replace, they do not append.
    let _ = expect_applied(
        patch(&mut c, json!({"detection": {"allow": ["a.exe"]}})),
        "allow [a.exe]",
    );
    assert_eq!(c.config()["detection"]["allow"], json!(["a.exe"]));
    let _ = expect_applied(
        patch(&mut c, json!({"detection": {"allow": ["b.exe"]}})),
        "allow [b.exe]",
    );
    assert_eq!(
        c.config()["detection"]["allow"],
        json!(["b.exe"]),
        "array did not replace: a.exe is still there"
    );

    // null clears an optional field. video.bitrate_kbps is a plain u32 in the schema, so the
    // check uses the per-game override games."cs2.exe".video.bitrate_kbps, which is an Option.
    let _ = expect_applied(
        patch(
            &mut c,
            json!({"games": {"cs2.exe": {"video": {"bitrate_kbps": 9000}}}}),
        ),
        "game bitrate 9000",
    );
    assert_eq!(
        c.config()["games"]["cs2.exe"]["video"]["bitrate_kbps"],
        json!(9000)
    );
    let _ = expect_applied(
        patch(
            &mut c,
            json!({"games": {"cs2.exe": {"video": {"bitrate_kbps": null}}}}),
        ),
        "game bitrate null",
    );
    let v = c.config();
    let video = &v["games"]["cs2.exe"]["video"];
    assert!(
        video.get("bitrate_kbps").is_some(),
        "bitrate_kbps key is missing after null: {video}"
    );
    assert!(
        video["bitrate_kbps"].is_null(),
        "bitrate_kbps is not null after the null patch: {video}"
    );
}

/// The games table takes new entries; a null entry removes the game.
#[test]
fn games_table_add_and_remove() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    let _ = expect_applied(
        patch(
            &mut c,
            json!({"games": {"cs2.exe": {"display_name": "CS2"}}}),
        ),
        "add cs2.exe",
    );
    assert_eq!(
        c.config()["games"]["cs2.exe"]["display_name"],
        json!("CS2"),
        "display_name after add"
    );

    let _ = expect_applied(
        patch(&mut c, json!({"games": {"cs2.exe": null}})),
        "remove cs2.exe",
    );
    assert!(
        c.config()["games"].get("cs2.exe").is_none(),
        "games.cs2.exe still present after null: {}",
        c.config()["games"]
    );
}

/// Each save keeps the previous file as config.toml.bak.
#[test]
fn save_keeps_previous_file_as_backup() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    let bak = dir.path().join("config.toml.bak");

    let _ = expect_applied(
        patch(&mut c, json!({"replay": {"max_seconds": 120}})),
        "first patch",
    );
    assert!(
        bak.is_file(),
        "config.toml.bak missing after the first patch"
    );

    let before_second = config_bytes(dir.path());
    let _ = expect_applied(
        patch(&mut c, json!({"replay": {"max_seconds": 150}})),
        "second patch",
    );
    assert_eq!(
        fs::read(&bak).expect("read config.toml.bak"),
        before_second,
        "config.toml.bak is not the file from before the second patch"
    );
    assert_eq!(parse_file(dir.path()).replay.max_seconds, 150);
}

/// A stray config.toml.tmp with garbage in it does not block a save.
#[test]
fn stray_tmp_file_does_not_block_save() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    let _ = c.config();

    fs::write(dir.path().join("config.toml.tmp"), "garbage [[[").expect("write stray tmp");
    let _ = expect_applied(
        patch(&mut c, json!({"replay": {"max_seconds": 120}})),
        "patch with stray tmp",
    );
    assert_eq!(parse_file(dir.path()).replay.max_seconds, 120);
}

/// ReloadConfig picks up hand edits: valid is applied, invalid and garbage are rejected and the
/// previous value stays in effect.
#[test]
fn reload_applies_valid_and_rejects_bad_hand_edits() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    let _ = c.config();
    let path = dir.path().join("config.toml");

    fs::write(&path, "config_version = 1\n[replay]\nmax_seconds = 150\n").expect("hand edit 150");
    let _ = expect_applied(c.call(Cmd::ReloadConfig), "reload valid edit");
    assert_eq!(
        c.config()["replay"]["max_seconds"],
        json!(150),
        "valid reload not applied"
    );

    fs::write(&path, "config_version = 1\n[replay]\nmax_seconds = 5\n").expect("hand edit 5");
    let diags = expect_rejected(c.call(Cmd::ReloadConfig), "reload max_seconds 5");
    assert!(
        has_error_at(&diags, "replay.max_seconds"),
        "reload 5: no error diagnostic at replay.max_seconds: {diags:?}"
    );
    assert_eq!(
        c.config()["replay"]["max_seconds"],
        json!(150),
        "rejected reload changed the value in effect"
    );

    fs::write(&path, "this is = = not toml [[[\n").expect("hand edit garbage");
    let diags = expect_rejected(c.call(Cmd::ReloadConfig), "reload garbage");
    assert!(
        diags.iter().any(|d| d.code == "CFG-P001"),
        "reload garbage: no CFG-P001 diagnostic: {diags:?}"
    );
    assert_eq!(
        c.config()["replay"]["max_seconds"],
        json!(150),
        "garbage reload changed the value in effect"
    );
}

/// GetStatus on an idle daemon: the clock is current, nothing waits for a restart, and the
/// buffer figure is a number.
#[test]
fn status_idle_fields() {
    let _g = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pipe = unique_pipe();
    let mut d = spawn(dir.path(), &pipe);
    let mut c = Client::handshake(&mut d, &pipe);

    let s = c.status();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_millis() as i64;
    assert!(
        (now_ms - s.server_unix_ms).abs() < 60_000,
        "server_unix_ms {} is more than 60 s from local clock {now_ms}",
        s.server_unix_ms
    );
    assert!(
        !s.settings_pending_restart,
        "settings_pending_restart is true when idle"
    );
    assert!(
        s.buffer_secs.is_finite() && s.buffer_secs >= 0.0,
        "buffer_secs is not a finite non-negative number: {}",
        s.buffer_secs
    );
}

/// Needs native test media and a desktop. A game window attaches, a session starts, and a split saves the
/// first part. `game_info` is set. The session's `started_unix_ms` does not change across the
/// split. Then `parts_saved` reaches 1 within 30 s.
#[test]
fn status_tracks_game_and_session_split() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("status_tracks_game_and_session_split");
        return;
    }
    let window = TestWindow::create().unwrap_or_else(|e| {
        panic!("status_tracks_game_and_session_split: cannot create the test window: {e}")
    });
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Sessions, Topic::State]);
    wait_ready(&mut c, data);

    let game = wait_for_game(&mut c, data);
    eprintln!("note: game_info.name = {game:?}");

    // With auto_record = true the daemon may already have started a session for the game.
    // Start one only when none is active.
    if c.status().session.is_none() {
        match c.call(Cmd::StartSession) {
            Reply::Ok => {}
            other => panic!(
                "start_session: expected Ok, got {other:?}\nlog:\n{}",
                log_text(data)
            ),
        }
    }
    let s = wait_status(
        &mut c,
        Duration::from_secs(15),
        "session after start",
        |s| s.session.is_some(),
    );
    let started = s.session.as_ref().expect("session").started_unix_ms;
    assert_eq!(
        s.session.as_ref().map(|x| x.part),
        Some(1),
        "part before split"
    );

    // Let part 1 record a few segments before the split.
    c.pump_for(Duration::from_secs(4));

    match c.call(Cmd::SplitSession) {
        Reply::Ok => {}
        other => panic!(
            "split_session: expected Ok, got {other:?}\nlog:\n{}",
            log_text(data)
        ),
    }
    let s = wait_status(&mut c, Duration::from_secs(15), "part 2 after split", |s| {
        s.session.as_ref().is_some_and(|x| x.part == 2)
    });
    let after = s.session.as_ref().expect("session after split");
    assert_eq!(
        after.started_unix_ms, started,
        "started_unix_ms changed across the split"
    );
    assert!(
        s.game_info.is_some(),
        "game_info cleared during the session: {s:?}"
    );

    let s = wait_status(&mut c, Duration::from_secs(30), "parts_saved >= 1", |s| {
        s.session.as_ref().is_some_and(|x| x.parts_saved >= 1)
    });
    assert_eq!(
        s.session.as_ref().map(|x| x.started_unix_ms),
        Some(started),
        "started_unix_ms changed after part 1 finished"
    );

    match c.call(Cmd::StopSession) {
        Reply::Ok => {}
        other => panic!("stop_session: expected Ok, got {other:?}"),
    }
    drop(window);
}

/// Needs native test media and a desktop. A hotkey chord changed by PatchConfig takes effect at once: the
/// new chord saves a clip, and the old chord no longer does.
#[test]
fn hotkey_change_applies_live() {
    let _g = lock();
    if !clipcheck_testkit::available() {
        skip_no_encoders("hotkey_change_applies_live");
        return;
    }
    let window = TestWindow::create().unwrap_or_else(|e| {
        panic!("hotkey_change_applies_live: cannot create the test window: {e}")
    });
    let exe = test_exe_name();
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();
    write_config(data, &exe, "Ctrl+Alt+Shift+F20");
    let pipe = unique_pipe();
    let mut d = spawn(data, &pipe);
    let mut c = Client::handshake(&mut d, &pipe);
    c.subscribe(vec![Topic::Clips, Topic::Settings]);
    wait_ready(&mut c, data);
    wait_for_game(&mut c, data);

    // Fill the replay buffer a little before the first clip.
    c.pump_for(Duration::from_secs(6));

    let (class, _, _) = expect_applied(
        patch(
            &mut c,
            json!({"hotkeys": {"clips": [
                {"name": "test", "keys": "Ctrl+Alt+Shift+F21", "seconds": 5}
            ]}}),
        ),
        "change clip hotkey to F21",
    );
    assert!(
        VALID_CLASSES.contains(&class.as_str()),
        "apply_class {class:?}"
    );

    // The old chord must no longer save a clip.
    let before_old = c.clip_saved_count();
    press_chord(&[VK_CONTROL, VK_MENU, VK_SHIFT, VK_F20]);
    c.pump_for(Duration::from_secs(4));
    assert_eq!(
        c.clip_saved_count(),
        before_old,
        "old chord Ctrl+Alt+Shift+F20 still saved a clip after the change\nlog:\n{}",
        log_text(data)
    );

    // The new chord must save one.
    press_chord(&[VK_CONTROL, VK_MENU, VK_SHIFT, VK_F21]);
    let ev = c.wait_event(
        Duration::from_secs(15),
        "clip_saved from the new chord",
        |e| matches!(e, Event::ClipSaved { .. }),
    );
    eprintln!("note: new chord produced {ev:?}");

    drop(window);
}
