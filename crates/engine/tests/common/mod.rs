//! Shared harness for the engine integration tests.
//!
//! Each test file pulls this in with `#[macro_use] mod common;`. Not every file
//! uses every item, so dead code is allowed here and nowhere else.
//!
//! Skip policy: a missing prerequisite (native test media, WGC, NVENC) skips the
//! test by printing `SKIPPED: <test>: <reason>` to stderr. Set
//! `CLIPCHECK_REQUIRE_CAPTURE=1` to make every skip a failure instead.
//! Test media is generated natively by `clipcheck-testkit`; no external tools are used.
#![allow(dead_code)]

#[cfg(windows)]
pub mod win_window;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_engine::recorder::RecorderConfig;

/// Serialises every test that attaches to a real window or runs a real encoder.
/// One lock for the whole binary, so two recorders never capture at once.
static SERIAL: Mutex<()> = Mutex::new(());

/// Locks the serialisation mutex, recovering from poisoning.
pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reports a missing prerequisite. Fails the test when `CLIPCHECK_REQUIRE_CAPTURE`
/// is set; otherwise writes a visible `SKIPPED` line to stderr.
///
/// The line goes straight to the process stderr, not through `eprintln!`, so
/// the libtest output capture does not hide it on a passing test.
pub fn skip_or_fail(reason: &str) {
    use std::io::Write;
    let test = thread::current().name().unwrap_or("?").to_owned();
    if std::env::var_os("CLIPCHECK_REQUIRE_CAPTURE").is_some() {
        panic!("{test}: required prerequisite missing: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED: {test}: {reason}");
}

/// Skips the enclosing test (returns from it) after reporting the reason.
#[allow(unused_macros)]
macro_rules! skip {
    ($($arg:tt)+) => {{
        common::skip_or_fail(&format!($($arg)+));
        return;
    }};
}

/// Yields the placeholder `(empty, empty)` path pair, or skips
/// the test when native test media cannot be generated.
#[allow(unused_macros)]
macro_rules! tools_or_skip {
    () => {
        match common::vendored_tools() {
            Some(tools) => tools,
            None => skip!("native test media unavailable"),
        }
    };
}

/// Gate for tests that need native test media (`clipcheck-testkit`).
///
/// Historically this located vendored external binaries. Test media is now
/// generated natively, so `Some` means "native media available" and the paths are
/// empty placeholders: the recorder encodes natively and never spawns them.
/// Kept under this name because many test files call it.
pub fn vendored_tools() -> Option<(PathBuf, PathBuf)> {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    let ok = *AVAILABLE.get_or_init(clipcheck_testkit::available);
    ok.then(|| (PathBuf::new(), PathBuf::new()))
}

/// No vendored binaries exist any more. Always `None`; kept only so existing
/// callers compile. Do not add new callers.
pub fn vendor_bin(_name: &str) -> Option<PathBuf> {
    None
}

/// `name` with the platform executable suffix: `.exe` on Windows, none elsewhere.
pub fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Duration of `file` in seconds, read from its MP4 container with `clipcheck_mp4`.
/// The first argument is unused and kept for existing callers.
pub fn probe_duration(_unused: &Path, file: &Path) -> f64 {
    clipcheck_mp4::reader::read_movie(file)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()))
        .duration_secs
}

/// Track title of a JSON stream entry (`tags.title`, then `tags.name`; the mp4
/// muxer stores the `title` metadata as the QuickTime `name` tag), empty when
/// neither is set. Works on any JSON stream object with a `tags` map.
pub fn stream_title(stream: &serde_json::Value) -> String {
    ["title", "name"]
        .iter()
        .find_map(|k| stream["tags"][*k].as_str().filter(|t| !t.is_empty()))
        .unwrap_or("")
        .to_string()
}

/// Builds a recorder config with every directory under `root`, created up
/// front. `per_game_folders` is on and `audio_tracks` empty; callers that need
/// other values overwrite the fields on the returned config.
/// Returns `(config, work_dir, clips_dir, sessions_dir)`.
pub fn make_config(
    root: &Path,
    encoder: &str,
    buffer_enabled: bool,
    session_auto_start: bool,
) -> (RecorderConfig, PathBuf, PathBuf, PathBuf) {
    let (work, clips, sessions) = session_dirs(root);
    let cfg = RecorderConfig {
        encoder_name: encoder.to_string(),
        fps: 30,
        canvas: (1280, 720),
        canvas_height: 720,
        bitrate_kbps: 4000,
        buffer_enabled,
        buffer_max_secs: 60.0,
        work_dir: work.clone(),
        clips_dir: clips.clone(),
        sessions_dir: sessions.clone(),
        per_game_folders: true,
        session_auto_start,
        audio_tracks: Vec::new(),
        tuning: clipcheck_encode::tuning::EncoderTuning::default(),
        no_border_capture: false,
    };
    (cfg, work, clips, sessions)
}

/// Polls `cond` every 20 ms until it returns true or `timeout` elapses.
/// Returns whether the condition became true. Use this for every wait on a
/// state change; a fixed sleep only belongs in a negative check.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// NVENC availability probe. Not implemented natively yet, so this always
/// returns false and NVENC-only tests skip. The argument is unused.
pub fn nvenc_works(_unused: &Path) -> bool {
    false
}

/// Stops a child process from opening a console window (Windows only).
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        clipcheck_util::proc::no_window(cmd);
    }
    cmd
}

/// Creates and returns the `(work, clips, sessions)` folders under `root`, the
/// layout every recorder config in the tests uses.
pub fn session_dirs(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let dirs = (root.join("work"), root.join("clips"), root.join("sessions"));
    for d in [&dirs.0, &dirs.1, &dirs.2] {
        std::fs::create_dir_all(d).expect("create dir");
    }
    dirs
}

/// Per-process directory for generated fixtures, under `CARGO_TARGET_TMPDIR`.
/// Tests never write into it: they copy fixtures out.
pub fn fixture_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fixtures-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        dir
    })
}

/// Number of 2 s segments in the 12 s session fixture.
pub const SEGMENT_COUNT: usize = 6;

/// Six 2 s keyframe-aligned segments of a 12 s test pattern, generated natively
/// by `clipcheck-testkit` (the same segment layout the native recorder writes).
/// Encoded once per test binary; callers copy them with [`copy_into`]. Only call
/// after checking [`vendored_tools`].
pub fn session_segments() -> &'static [PathBuf] {
    static SEGS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    SEGS.get_or_init(|| {
        let dir = fixture_dir().join("segments");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create segments dir");
        let spec = clipcheck_testkit::MediaSpec::h264_aac(12.0).with_video(320, 240, 30);
        let mut segs = clipcheck_testkit::write_segments(&dir, &spec, 2.0)
            .expect("testkit could not generate fixture segments");
        segs.sort();
        assert_eq!(
            segs.len(),
            SEGMENT_COUNT,
            "expected {SEGMENT_COUNT} fixture segments, got {}: {segs:?}",
            segs.len()
        );
        segs
    })
}

/// Copies each file of `srcs` into `dir` under the same name and returns the
/// copies in the same order. A copy, not a link: callers may modify theirs.
pub fn copy_into(srcs: &[PathBuf], dir: &Path) -> Vec<PathBuf> {
    srcs.iter()
        .map(|src| {
            let dst = dir.join(src.file_name().expect("fixture has a file name"));
            std::fs::copy(src, &dst)
                .unwrap_or_else(|e| panic!("copy {} failed: {e}", src.display()));
            dst
        })
        .collect()
}

/// Sorted list of `.mp4` files directly inside `dir`.
pub fn list_mp4(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "mp4"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// True when `CLIPCHECK_TEST_VERBOSE` is set. Diagnostic output that is only
/// useful while debugging a test is printed through [`vlog!`] behind this switch.
pub fn verbose() -> bool {
    std::env::var_os("CLIPCHECK_TEST_VERBOSE").is_some()
}

/// `eprintln!` that only prints when `CLIPCHECK_TEST_VERBOSE` is set.
#[allow(unused_macros)]
macro_rules! vlog {
    ($($arg:tt)+) => {{
        if common::verbose() {
            eprintln!($($arg)+);
        }
    }};
}
