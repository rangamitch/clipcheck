//! Crash salvage at daemon start (issues #26, #172).
//!
//! Once the encoder probe succeeds, the service concatenates leftover staged session parts from
//! a dead earlier process (`<work_dir>/run-*/stage-s<seq>-p<part>/part-NNNNNN.mp4`) into
//! `<sessions_dir>/recovered-<run>-s<seq>-p<part>.mp4`. Successes are registered as recovered
//! Session rows and publish `SessionPartFinished { recovered: true }`. Failures keep the staged
//! parts and publish a `session_recovery` warning.
//!
//! Windows only. Both tests need native test media (`clipcheck_testkit::available()`) and a
//! working software encoder. Missing media, or an encoder probe that fails, is reported as
//! `SKIPPED:` and the test returns. Set `CLIPCHECK_REQUIRE_CAPTURE` to turn missing test media into a failure.
//!
//! Every test uses a temporary data dir. The config allowlists a game exe that does not exist and
//! turns auto-record off, so no game is detected and nothing is recorded.

#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clipcheck_ipc::proto::{Command as Cmd, Event, RecState, Reply};
use clipcheck_library::{Library, ListQuery, RecordingKind};
use clipcheck_testkit::MediaSpec;
use common::*;

/// How long each test waits for the salvage events.
const SALVAGE_WAIT: Duration = Duration::from_secs(60);

/// Bytes written into every staged part for the unrecoverable case.
const GARBAGE: &[u8] = b"this is not an mp4 file, only garbage bytes\n";

/// Writes `<data_dir>/config.toml`. Replay stays at its defaults, so the work dir is
/// `<data_dir>/buffer`, and the sessions dir is `<data_dir>/recordings/Sessions`.
fn write_config(data_dir: &Path) {
    let recordings = data_dir.join("recordings");
    let logs = data_dir.join("logs");
    let text = format!(
        r#"[storage]
root = '{root}'

[detection]
mode = "allowlist"
allow = ["clipcheck-test-no-such-game.exe"]
auto_record = false

[replay]
enabled = true
max_seconds = 60
location = ""

[video]
encoder = "software"
codec = "h264"
preset = "low"

[advanced]
log_dir = '{logs}'
"#,
        root = recordings.display(),
        logs = logs.display(),
    );
    fs::write(data_dir.join("config.toml"), text).expect("failed to write config.toml");
}

/// Native test media is available, or print the `SKIPPED:` line (or fail when required).
fn media_or_skip(test: &str) -> bool {
    if clipcheck_testkit::available() {
        return true;
    }
    skip_no_encoders(test);
    false
}

/// The stage dir the daemon scans: `<work_dir>/run-1/stage-s1-p1`, with work_dir `<data>/buffer`.
fn stage_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("buffer").join("run-1").join("stage-s1-p1")
}

/// Writes two valid 1 s fragmented parts (`part-NNNNNN.mp4`) with the native test generator.
fn write_valid_parts(stage: &Path) {
    let spec = MediaSpec::h264_aac(2.0).with_video(160, 120, 15);
    let tmp = stage.join("tmp");
    let segs = clipcheck_testkit::write_segments(&tmp, &spec, 1.0).expect("write test segments");
    assert_eq!(segs.len(), 2, "expected two 1 s segments");
    for (n, seg) in segs.iter().enumerate() {
        fs::rename(seg, stage.join(format!("part-{n:06}.mp4"))).expect("move segment to part");
    }
    fs::remove_dir_all(&tmp).expect("remove temp segment dir");
}

/// Creates the stage dir with two valid mp4 parts.
fn stage_with_valid_parts(data_dir: &Path) -> PathBuf {
    let stage = stage_dir(data_dir);
    fs::create_dir_all(&stage).expect("create stage dir");
    write_valid_parts(&stage);
    stage
}

/// Creates the stage dir with two parts that hold garbage bytes.
fn stage_with_garbage_parts(data_dir: &Path) -> PathBuf {
    let stage = stage_dir(data_dir);
    fs::create_dir_all(&stage).expect("create stage dir");
    for n in 0..2 {
        fs::write(stage.join(format!("part-{n:06}.mp4")), GARBAGE).expect("write garbage part");
    }
    stage
}

/// Spawns the daemon on a fresh pipe and subscribes to all topics. Salvage starts only after the
/// probe, so subscribing right away catches every salvage event.
fn start(data_dir: &Path, pipe: &str) -> (Daemon, Client) {
    write_config(data_dir);
    let mut d = spawn_with(data_dir, pipe, &[]);
    let mut c = Client::handshake(&mut d, pipe);
    c.subscribe_all();
    (d, c)
}

/// Waits for the encoder probe to finish. `Ok` once the recorder is up. `Err(reason)` when the
/// probe failed, because salvage never runs without a successful probe.
fn wait_probe(c: &mut Client, data_dir: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let s = c.status();
        match (s.state, s.degraded_reason.as_deref()) {
            (st, None) if st != RecState::Degraded => return Ok(()),
            (_, Some(r)) if r.contains("in progress") => {}
            (_, Some(r)) => return Err(r.to_owned()),
            (_, None) => {}
        }
        if Instant::now() >= deadline {
            panic!(
                "encoder probe did not finish within 90 s; last status: {s:?}\nlog:\n{}",
                log_text(data_dir)
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn crash_salvage_recovers_staged_parts_at_start() {
    let _guard = lock();
    let test = "crash_salvage_recovers_staged_parts_at_start";
    if !media_or_skip(test) {
        return;
    }
    let data = tempfile::tempdir().expect("tempdir");
    let stage = stage_with_valid_parts(data.path());

    let pipe = unique_pipe();
    let (_d, mut c) = start(data.path(), &pipe);
    if let Err(reason) = wait_probe(&mut c, data.path()) {
        eprintln!("SKIPPED: {test}: encoder probe failed, so crash salvage never runs: {reason}");
        return;
    }

    let ev = c.wait_event(
        SALVAGE_WAIT,
        "recovered session_part_finished for part 1",
        |e| {
            matches!(
                e,
                Event::SessionPartFinished {
                    recovered: true,
                    part: 1,
                    ..
                }
            )
        },
    );
    let Event::SessionPartFinished {
        session_id,
        path,
        bytes,
        ..
    } = ev
    else {
        panic!("wait_event returned a non-SessionPartFinished event: {ev:?}");
    };

    // The recovered file exists, is non-empty, lives under the sessions dir and has the agreed name.
    let out = PathBuf::from(&path);
    assert!(out.is_file(), "recovered file missing: {path}");
    let len = fs::metadata(&out).expect("stat recovered file").len();
    assert!(len > 0, "recovered file is empty: {path}");
    assert_eq!(len, bytes, "event bytes do not match the file on disk");
    let sessions = fs::canonicalize(data.path().join("recordings").join("Sessions"))
        .expect("sessions dir exists");
    let out_canon = fs::canonicalize(&out).expect("canonicalize recovered file");
    assert!(
        out_canon.starts_with(&sessions),
        "recovered file {} is not under {}",
        out_canon.display(),
        sessions.display()
    );
    assert_eq!(
        out.file_name().map(|n| n.to_string_lossy().into_owned()),
        Some("recovered-run-1-s1-p1.mp4".to_owned()),
        "recovered file name"
    );

    // The stage dir is gone once its parts are salvaged.
    assert!(
        !stage.exists(),
        "stage dir still exists after a successful salvage: {}",
        stage.display()
    );

    // The library holds a recovered Session row for the part.
    let lib =
        Library::open_readonly(data.path().join("library.db")).expect("open library.db read-only");
    let deadline = Instant::now() + Duration::from_secs(10);
    let row = loop {
        let page = lib
            .list(&ListQuery {
                kind: Some(RecordingKind::Session),
                limit: 200,
                ..Default::default()
            })
            .expect("list session rows");
        if let Some(found) = page
            .items
            .into_iter()
            .find(|r| r.session_id.as_deref() == Some(session_id.as_str()))
        {
            break found;
        }
        if Instant::now() >= deadline {
            panic!(
                "no Session library row for session {session_id}; log:\n{}",
                log_text(data.path())
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        row.recovered,
        "library row is not marked recovered: {row:?}"
    );
    assert_eq!(
        row.end_reason.as_deref(),
        Some("recovered"),
        "library row end_reason"
    );
    assert_eq!(row.part_index, Some(1), "library row part index");
}

#[test]
fn unrecoverable_stage_is_kept_and_daemon_stays_up() {
    let _guard = lock();
    let test = "unrecoverable_stage_is_kept_and_daemon_stays_up";
    if !media_or_skip(test) {
        return;
    }
    let data = tempfile::tempdir().expect("tempdir");
    let stage = stage_with_garbage_parts(data.path());

    let pipe = unique_pipe();
    let (mut d, mut c) = start(data.path(), &pipe);
    if let Err(reason) = wait_probe(&mut c, data.path()) {
        eprintln!("SKIPPED: {test}: encoder probe failed, so crash salvage never runs: {reason}");
        return;
    }

    // The failure warning is published after the salvage attempt has finished.
    c.wait_event(
        SALVAGE_WAIT,
        "session_recovery warning that the parts were kept",
        |e| {
            matches!(e, Event::Warning { code, message }
            if code == "session_recovery" && message.contains("they were kept"))
        },
    );

    // The staged parts are still there and byte-for-byte unchanged.
    for n in 0..2 {
        let part = stage.join(format!("part-{n:06}.mp4"));
        let bytes = fs::read(&part)
            .unwrap_or_else(|e| panic!("staged part {} was removed: {e}", part.display()));
        assert_eq!(bytes, GARBAGE, "staged part {} was changed", part.display());
    }
    assert!(
        stage.is_dir(),
        "stage dir was removed after a failed salvage"
    );

    // No recovered session part was published.
    assert!(
        !c.seen.iter().any(|e| matches!(
            e,
            Event::SessionPartFinished {
                recovered: true,
                ..
            }
        )),
        "a recovered session part was published for garbage input: {:?}",
        c.seen
    );

    // The daemon did not crash and still answers.
    assert!(
        d.child.try_wait().expect("try_wait").is_none(),
        "daemon exited after a failed salvage\nlog:\n{}",
        log_text(data.path())
    );
    match c.call(Cmd::GetStatus) {
        Reply::Status(_) => {}
        other => panic!("expected Status after a failed salvage, got {other:?}"),
    }
}
