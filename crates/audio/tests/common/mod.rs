//! Shared harness for the audio integration tests.
//!
//! Each test file pulls this in with `mod common;`. Not every file uses every
//! item, so dead code is allowed here and nowhere else.
//!
//! Skip policy: a missing prerequisite (no capture or render endpoint, process
//! loopback unsupported) is reported through [`skip_or_fail`]. That writes a
//! visible `SKIPPED: <test>: <reason>` line to stderr and returns, so the test is
//! shown as skipped in the log. Set `CLIPCHECK_REQUIRE_AUDIO=1` to make every skip
//! a failure instead, which is the setting for a machine that must prove the
//! audio path works.
//!
//! Tone source: [`start_tone`] plays a quiet sine through `clipcheck_testkit::tone`
//! (a hidden `powershell.exe` using `System.Media.SoundPlayer`), so the tests need
//! no external encoder or media binary.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use clipcheck_testkit::tone::TonePlayer;

/// Reports a missing prerequisite. Fails the test when `CLIPCHECK_REQUIRE_AUDIO`
/// is set; otherwise writes a visible `SKIPPED` line to stderr.
///
/// The line goes straight to the process stderr, not through `eprintln!`, so
/// the libtest output capture does not hide it on a passing test.
pub fn skip_or_fail(reason: &str) {
    use std::io::Write;
    let test = thread::current().name().unwrap_or("?").to_owned();
    if std::env::var_os("CLIPCHECK_REQUIRE_AUDIO").is_some() {
        panic!("{test}: required audio prerequisite missing: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED: {test}: {reason}");
}

/// A uniquely named scratch directory under the system temp dir, removed on drop.
pub struct ToneDir(PathBuf);

impl ToneDir {
    fn new() -> std::io::Result<ToneDir> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "clipcheck-audio-tone-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(ToneDir(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ToneDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A looping tone playing in its own PowerShell process. Fields drop in
/// declaration order, so the player is killed before its scratch directory is
/// removed.
pub struct ToneSession {
    pub player: TonePlayer,
    _dir: ToneDir,
}

impl ToneSession {
    /// Process id of the tone player; its audio session belongs to this pid.
    pub fn pid(&self) -> u32 {
        self.player.pid()
    }
}

/// Starts a looping `freq_hz` tone that plays for at most `max_secs` seconds
/// (the player is also killed when the returned session drops). Panics if the
/// PowerShell tone player cannot be started.
pub fn start_tone(freq_hz: f32, max_secs: u32) -> ToneSession {
    let dir = ToneDir::new().expect("create tone scratch directory");
    let player = TonePlayer::spawn(dir.path(), freq_hz, max_secs)
        .expect("spawn tone player (powershell.exe)");
    ToneSession { player, _dir: dir }
}
