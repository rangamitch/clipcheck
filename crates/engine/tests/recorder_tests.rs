//! Integration tests for `clipcheck_engine::recorder`.
//!
//! Idle-only tests run everywhere. Tests that attach to a real window need
//! Windows Graphics Capture. When it is
//! missing, the test prints `SKIPPED: <test>: <reason>` to stderr and returns.
//! Set `CLIPCHECK_REQUIRE_CAPTURE=1` to make every skip a failure instead.
//! Set `CLIPCHECK_TEST_VERBOSE=1` to print unrelated recorder events.
//!
//! Window tests take the shared `common::serial()` lock so that no two
//! recorders capture or encode at the same time.

#[macro_use]
mod common;

use std::path::{Path, PathBuf};
#[cfg(windows)]
#[cfg(windows)]
use std::sync::mpsc;
#[cfg(windows)]
use std::sync::mpsc::Receiver;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use clipcheck_engine::recorder::RecorderEvent;
use clipcheck_engine::recorder::{Recorder, RecorderError};

#[cfg(windows)]
use common::serial;
#[cfg(windows)]
use common::win_window::TestWindow;
#[cfg(windows)]
use common::{make_config, wait_until};

/// Fill colour of the test window (COLORREF 0x00BBGGRR, dark orange-brown).
#[cfg(windows)]
const WINDOW_FILL: u32 = 0x0030_6090;

/// Length of one recorder segment, in seconds. Clip and session lengths are
/// checked against it.
#[cfg(windows)]
const SEGMENT_SECS: f64 = 2.0;

/// Attaches `$hwnd` to `$rec`. `Unsupported` skips the test; any other error,
/// including a failed capture start, fails it.
#[cfg(windows)]
macro_rules! attach_or_skip {
    ($rec:expr, $hwnd:expr, $game:expr) => {
        match $rec.attach($hwnd, $game) {
            Ok(()) => {}
            Err(RecorderError::Unsupported) => {
                skip!("recording is not supported on this platform")
            }
            Err(e) => panic!("attach to the test window failed: {e:?}"),
        }
    };
}

/// Asserts that a clip requested for `secs` seconds has a matching length.
///
/// The buffer trims the clip to the requested start, so the length should be
/// `secs`. The lower bound allows half a segment for keyframe alignment, and the
/// upper bound allows half a second for container rounding.
#[cfg(windows)]
fn assert_clip_len(path: &Path, secs: f64) {
    let d = reader_duration(path);
    let (lo, hi) = (secs - SEGMENT_SECS / 2.0, secs + 0.5);
    assert!(
        (lo..=hi).contains(&d),
        "clip {} is {d:.3} s, expected {lo:.3}..={hi:.3} s for a {secs} s request",
        path.display()
    );
}

/// Waits for the first event satisfying `pred`. Other events are printed only
/// when `CLIPCHECK_TEST_VERBOSE` is set.
#[cfg(windows)]
fn wait_event<F>(rx: &Receiver<RecorderEvent>, timeout: Duration, pred: F) -> Option<RecorderEvent>
where
    F: Fn(&RecorderEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(ev) if pred(&ev) => return Some(ev),
            Ok(ev) => vlog!("  (other event: {ev:?})"),
            Err(_) => return None,
        }
    }
}

/// Waits for the ClipSaved or ClipFailed event of `job`. Returns the saved path
/// and the event's `seconds_actual`; panics on ClipFailed or timeout.
#[cfg(windows)]
fn wait_clip_saved(
    rx: &Receiver<RecorderEvent>,
    job: clipcheck_engine::recorder::JobId,
    timeout: Duration,
) -> (PathBuf, f64, bool) {
    let ev = wait_event(rx, timeout, |e| match e {
        RecorderEvent::ClipSaved { job: j, .. } | RecorderEvent::ClipFailed { job: j, .. } => {
            *j == job
        }
        _ => false,
    })
    .unwrap_or_else(|| panic!("no ClipSaved/ClipFailed for {job:?} within {timeout:?}"));
    match ev {
        RecorderEvent::ClipSaved {
            path,
            seconds_actual,
            short,
            ..
        } => (path, seconds_actual, short),
        RecorderEvent::ClipFailed { error, .. } => panic!("clip {job:?} failed: {error}"),
        other => panic!("unexpected event {other:?}"),
    }
}

/// Waits until the rolling buffer spans at least `secs`, so a clip of that
/// length can be cut. Polls the recorder status rather than sleeping a guessed time.
#[cfg(windows)]
fn wait_buffer(rec: &Recorder, secs: f64) {
    let reached = wait_until(Duration::from_secs(30), || rec.status().buffer_secs >= secs);
    assert!(
        reached,
        "buffer reached {:.2} s, wanted {secs} s",
        rec.status().buffer_secs
    );
}

/// Waits until the replay buffer span has grown by `secs` past `base`. Buffer
/// growth while a session runs is session footage, so this replaces a fixed
/// sleep. Panics on timeout.
#[cfg(windows)]
fn wait_footage_past(rec: &Recorder, base: f64, secs: f64, timeout: Duration) {
    let want = base + secs;
    let reached = wait_until(timeout, || rec.status().buffer_secs >= want);
    assert!(
        reached,
        "buffer reached {:.2} s, wanted {want:.2} s (base {base:.2} s + {secs} s)",
        rec.status().buffer_secs
    );
}

/// `yyyy-MM-dd HH-mm-ss`, 19 chars.
#[cfg(windows)]
fn stamp_ok(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 19
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 | 13 | 16 => c == b'-',
            10 => c == b' ',
            _ => c.is_ascii_digit(),
        })
}

/// True when `path` sits in `<base>/Test Game/` and is named
/// `"Test Game <stamp> <suffix>"`. `base` is the clips or sessions base folder.
#[cfg(windows)]
fn name_ok(path: &Path, base: &Path, suffix: &str) -> bool {
    let game_dir = path.parent();
    if game_dir.and_then(Path::file_name).and_then(|n| n.to_str()) != Some("Test Game")
        || game_dir.and_then(Path::parent) != Some(base)
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(rest) = name.strip_prefix("Test Game ") else {
        return false;
    };
    let Some(stamp) = rest.strip_suffix(&format!(" {suffix}")) else {
        return false;
    };
    stamp_ok(stamp)
}

#[cfg(windows)]
fn run_dirs(work: &Path) -> Vec<String> {
    std::fs::read_dir(work)
        .expect("read work_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("run-"))
        .collect()
}

// ---------------------------------------------------------------------------
// idle tests (all platforms)
// ---------------------------------------------------------------------------

#[test]
fn status_default_idle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(dir.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let rec = Recorder::new(cfg, tx);
    let st = rec.status();
    assert!(st.attached.is_none(), "idle attached = {:?}", st.attached);
    assert!(!st.session_active, "idle session_active = true");
    assert_eq!(st.part_index, 0, "idle part_index");
    assert_eq!(st.buffer_secs, 0.0, "idle buffer_secs");
    assert_eq!(st.frames_dropped, 0, "idle frames_dropped");
}

#[test]
fn calls_when_idle_return_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(dir.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);

    assert_eq!(
        rec.clip(4).err(),
        Some(RecorderError::NotAttached),
        "clip(4)"
    );
    assert_eq!(
        rec.session_start(),
        Err(RecorderError::NotAttached),
        "session_start"
    );
    assert_eq!(
        rec.session_stop(),
        Err(RecorderError::NotAttached),
        "session_stop"
    );
    assert_eq!(
        rec.session_split(),
        Err(RecorderError::NotAttached),
        "session_split"
    );
    assert!(rec.status().attached.is_none(), "attached after idle calls");
}

#[test]
fn detach_twice_is_fine() {
    // Never attached: detach is a no-op, twice.
    let dir = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(dir.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    rec.detach();
    rec.detach();
    assert!(rec.status().attached.is_none(), "never-attached detach");
}

// ---------------------------------------------------------------------------
// window tests (Windows only)
// ---------------------------------------------------------------------------

/// `clip(0)` on an attached recorder is an invalid request, not a buffer error.
#[cfg(windows)]
#[test]
fn clip_zero_after_attach_is_invalid_request() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let dir = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(dir.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    match rec.clip(0) {
        Err(RecorderError::InvalidRequest(_)) => {}
        other => panic!("clip(0) after attach: expected InvalidRequest, got {other:?}"),
    }
    rec.detach();
}

/// Attached half of the detach check: a second detach after a real attach is a no-op.
#[cfg(windows)]
#[test]
fn detach_twice_after_attach_is_fine() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let dir = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(dir.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    rec.detach();
    rec.detach();
    assert!(rec.status().attached.is_none(), "attached after detach x2");
}

#[cfg(windows)]
#[test]
fn end_to_end_clip_split_stop_detach() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, work, clips, sessions) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);

    let st = rec.status();
    assert!(
        st.attached.is_none(),
        "attached before attach: {:?}",
        st.attached
    );
    assert!(!st.session_active, "session active before attach");

    attach_or_skip!(rec, win.hwnd, "Test Game");
    assert_eq!(
        rec.status().attached.as_deref(),
        Some("Test Game"),
        "attached name"
    );

    // Clip.
    wait_buffer(&rec, 4.0 + SEGMENT_SECS);
    let job = rec.clip(4).expect("clip(4) returned error");
    let (path, _actual, _short) = wait_clip_saved(&rx, job, Duration::from_secs(20));
    assert!(path.is_file(), "clip file missing: {}", path.display());
    assert!(
        name_ok(&path, &clips, "clip-4s.mp4"),
        "clip name does not match pattern: {}",
        path.display()
    );
    assert_clip_len(&path, 4.0);

    // Session: start, split, stop.
    let start = Instant::now();
    rec.session_start().expect("session_start");
    assert!(
        rec.status().session_active,
        "session not active after start"
    );
    // The split needs a completed session segment: wait for 5 s of session footage.
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(15));

    rec.session_split().expect("session_split");
    let ev = wait_event(&rx, Duration::from_secs(5), |e| {
        matches!(e, RecorderEvent::SessionPartSaved { .. })
    })
    .expect("no SessionPartSaved after split");
    let part1 = match ev {
        RecorderEvent::SessionPartSaved {
            path,
            part_index: 1,
            final_part: false,
        } => path,
        other => panic!("split event mismatch: {other:?}"),
    };
    assert!(part1.is_file(), "part1 missing: {}", part1.display());
    assert!(
        name_ok(&part1, &sessions, "session-part1.mp4"),
        "part1 name: {}",
        part1.display()
    );
    let d1 = reader_duration(&part1);
    assert!(d1 > 0.0, "part1 duration {d1} not > 0");

    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 5.0, Duration::from_secs(15));
    let stop_at = Instant::now();
    rec.session_stop().expect("session_stop");
    let ev = wait_event(&rx, Duration::from_secs(5), |e| {
        matches!(e, RecorderEvent::SessionPartSaved { .. })
    })
    .expect("no SessionPartSaved after stop");
    let part2 = match ev {
        RecorderEvent::SessionPartSaved {
            path,
            part_index: 2,
            final_part: true,
        } => path,
        other => panic!("stop event mismatch: {other:?}"),
    };
    assert!(part2.is_file(), "part2 missing: {}", part2.display());
    assert!(
        name_ok(&part2, &sessions, "session-part2.mp4"),
        "part2 name: {}",
        part2.display()
    );
    let d2 = reader_duration(&part2);

    // The parts cover the session wall time to within one segment. The lower bound
    // allows one segment of keyframe-alignment slack. The upper bound allows half a
    // second for stop-to-event timing. Wall time is measured with Instant, so the
    // footage waits above do not change the expected total.
    let wall = stop_at.duration_since(start).as_secs_f64();
    let total = d1 + d2;
    assert!(
        total >= wall - SEGMENT_SECS && total <= wall + 0.5,
        "session parts total {total:.3} s not within [{:.3}, {:.3}] (wall {wall:.3} s)",
        wall - SEGMENT_SECS,
        wall + 0.5
    );
    assert!(
        !rec.status().session_active,
        "session still active after stop"
    );

    // Detach returns before the run directory is removed: the finalizer thread
    // does the cleanup (D48). Wait for queued jobs to finish, then check.
    rec.detach();
    assert!(rec.status().attached.is_none(), "attached after detach");
    assert!(
        rec.wait_idle(Duration::from_secs(60)),
        "finalizer did not finish within 60 s"
    );
    let leftover = run_dirs(&work);
    assert!(
        leftover.is_empty(),
        "run dirs left after detach: {leftover:?}"
    );
}

#[cfg(windows)]
#[test]
fn clip_with_buffer_disabled_errors() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(root.path(), "libx264", false, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    assert_eq!(
        rec.clip(4).err(),
        Some(RecorderError::BufferDisabled),
        "clip(4)"
    );
    rec.detach();
}

#[cfg(windows)]
#[test]
fn clip_works_while_session_off_and_on() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, clips, _) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    // Session on.
    rec.session_start().expect("session_start");
    wait_buffer(&rec, 2.0 + SEGMENT_SECS);
    let job = rec.clip(2).expect("clip(2) with session on");
    let (path, _, _) = wait_clip_saved(&rx, job, Duration::from_secs(20));
    assert!(
        name_ok(&path, &clips, "clip-2s.mp4"),
        "name: {}",
        path.display()
    );
    assert_clip_len(&path, 2.0);

    // Session off, buffer still running.
    rec.session_stop().expect("session_stop");
    let job = rec.clip(2).expect("clip(2) with session off");
    let (path, _, _) = wait_clip_saved(&rx, job, Duration::from_secs(20));
    assert!(
        path.is_file(),
        "session-off clip missing: {}",
        path.display()
    );
    assert_clip_len(&path, 2.0);

    rec.detach();
}

#[cfg(windows)]
#[test]
fn end_to_end_nvenc_clip() {
    let _g = serial();
    if !nvenc_available() {
        skip!("h264_nvenc encode probe failed");
    }
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(root.path(), "h264_nvenc", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    wait_buffer(&rec, 2.0 + SEGMENT_SECS);
    let job = rec.clip(2).expect("clip(2) with nvenc");
    let (path, _, _) = wait_clip_saved(&rx, job, Duration::from_secs(20));
    assert_clip_len(&path, 2.0);
    rec.detach();
}

#[cfg(windows)]
#[test]
fn second_attach_is_rejected() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(root.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    assert_eq!(
        rec.attach(win.hwnd, "Test Game").err(),
        Some(RecorderError::AlreadyAttached),
        "second attach"
    );
    rec.detach();
}

#[cfg(windows)]
#[test]
fn clip_with_empty_buffer_is_error() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(root.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    assert_eq!(
        rec.clip(4).err(),
        Some(RecorderError::BufferEmpty),
        "clip right after attach"
    );
    rec.detach();
}

#[cfg(windows)]
#[test]
fn session_split_without_segments_is_error() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, _) = make_config(root.path(), "libx264", true, false);
    let (tx, _rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    rec.session_start().expect("session_start");
    assert_eq!(
        rec.session_split().err(),
        Some(RecorderError::NoSegments),
        "split with no completed segments"
    );
    rec.session_stop().expect("session_stop");
    assert_eq!(
        rec.session_split().err(),
        Some(RecorderError::NoSession),
        "split with no session"
    );
    rec.detach();
}

#[cfg(windows)]
#[test]
fn session_auto_start_begins_session() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, _, sessions) = make_config(root.path(), "libx264", true, true);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");
    let st = rec.status();
    assert!(st.session_active, "session not auto-started");
    assert_eq!(st.part_index, 1, "part_index after auto start");

    wait_footage_past(&rec, st.buffer_secs, 5.0, Duration::from_secs(15));
    rec.detach();

    let ev = wait_event(&rx, Duration::from_secs(5), |e| {
        matches!(
            e,
            RecorderEvent::SessionPartSaved {
                final_part: true,
                ..
            }
        )
    })
    .expect("no final SessionPartSaved after detach");
    let RecorderEvent::SessionPartSaved { path, .. } = ev else {
        unreachable!("predicate only matches SessionPartSaved")
    };
    assert!(path.is_file(), "final part missing: {}", path.display());
    assert_eq!(
        path.parent().and_then(Path::parent),
        Some(sessions.as_path()),
        "part dir"
    );
    assert!(
        name_ok(&path, &sessions, "session-part1.mp4"),
        "part name: {}",
        path.display()
    );
}

/// Describes the tracks of `file` as `(key, value)` lists, read with the
/// in-repo MP4 reader. One list per track, in track order. Keys: `codec_type`,
/// `codec_name`, `width`, `height`, `channels`, `sample_rate` and `TAG:title`.
#[cfg(windows)]
fn probe_streams(file: &Path) -> Vec<Vec<(String, String)>> {
    let movie = clipcheck_mp4::reader::read_movie(file)
        .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
    movie
        .tracks
        .iter()
        .map(|t| {
            let mut kv: Vec<(String, String)> = Vec::new();
            match &t.kind {
                clipcheck_mp4::reader::TrackKind::Video {
                    codec,
                    width,
                    height,
                } => {
                    kv.push(("codec_type".into(), "video".into()));
                    kv.push(("codec_name".into(), codec.clone()));
                    kv.push(("width".into(), width.to_string()));
                    kv.push(("height".into(), height.to_string()));
                }
                clipcheck_mp4::reader::TrackKind::Audio {
                    codec,
                    channels,
                    sample_rate,
                } => {
                    kv.push(("codec_type".into(), "audio".into()));
                    kv.push(("codec_name".into(), codec.clone()));
                    kv.push(("channels".into(), channels.to_string()));
                    kv.push(("sample_rate".into(), sample_rate.to_string()));
                }
                clipcheck_mp4::reader::TrackKind::Other { handler } => {
                    kv.push(("codec_type".into(), handler.clone()));
                }
            }
            if let Some(title) = &t.title {
                kv.push(("TAG:title".into(), title.clone()));
            }
            kv
        })
        .collect()
}

/// True when a hardware H.264 encoder from the NVIDIA vendor is enumerable. The
/// NVENC path is only exercised when this holds.
#[cfg(windows)]
fn nvenc_available() -> bool {
    clipcheck_hwenc::enumerate(clipcheck_hwenc::Codec::H264)
        .map(|v| {
            v.iter()
                .any(|d| d.hardware && matches!(d.vendor, clipcheck_hwenc::Vendor::Nvidia))
        })
        .unwrap_or(false)
}

/// Value of `key` in one stream's `probe_streams` entry.
#[cfg(windows)]
fn stream_field<'a>(stream: &'a [(String, String)], key: &str) -> Option<&'a str> {
    stream
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// tuning and multi-track audio (Windows only)
// ---------------------------------------------------------------------------

/// A session recorded with explicit encoder tuning and two audio tracks writes
/// one 1280x720 H.264 video stream and two AAC streams titled "Mix" and "Game".
#[cfg(windows)]
#[test]
fn recorder_applies_tuning_and_two_audio_tracks() {
    use clipcheck_encode::tuning::{EncoderTuning, RateMode, Speed};
    use clipcheck_engine::recorder::{AudioSourceKind, AudioTrackConfig, TrackSource};

    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (mut cfg, _, _, sessions) = make_config(root.path(), "libx264", true, false);
    cfg.canvas = (1280, 720);
    cfg.fps = 30;
    cfg.tuning = EncoderTuning {
        rate: RateMode::Quality { cq: 23 },
        speed: Speed::Fastest,
        b_frames: 0,
        gpu_index: None,
    };
    cfg.audio_tracks = vec![
        AudioTrackConfig {
            title: "Mix".to_owned(),
            sources: vec![TrackSource {
                kind: AudioSourceKind::SystemMix,
                gain_db: 0.0,
                gate_db: None,
            }],
            bitrate_kbps: 128,
        },
        AudioTrackConfig {
            title: "Game".to_owned(),
            sources: vec![TrackSource {
                kind: AudioSourceKind::GameProcess,
                gain_db: 0.0,
                gate_db: None,
            }],
            bitrate_kbps: 128,
        },
    ];
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    rec.session_start().expect("session_start");
    let base = rec.status().buffer_secs;
    wait_footage_past(&rec, base, 6.0, Duration::from_secs(16));
    rec.session_stop().expect("session_stop");
    let ev = wait_event(&rx, Duration::from_secs(30), |e| {
        matches!(
            e,
            RecorderEvent::SessionPartSaved {
                final_part: true,
                ..
            }
        )
    })
    .expect("no final SessionPartSaved within 30 s");
    let RecorderEvent::SessionPartSaved { path, .. } = ev else {
        unreachable!("predicate only matches SessionPartSaved")
    };
    assert!(path.is_file(), "part missing: {}", path.display());
    assert_eq!(
        path.parent().and_then(Path::parent),
        Some(sessions.as_path()),
        "part dir"
    );

    let streams = probe_streams(&path);
    let kind = |want: &str| -> Vec<&Vec<(String, String)>> {
        streams
            .iter()
            .filter(|s| stream_field(s, "codec_type") == Some(want))
            .collect()
    };
    let video = kind("video");
    let audio = kind("audio");
    assert_eq!(video.len(), 1, "video stream count in {}", path.display());
    assert_eq!(audio.len(), 2, "audio stream count in {}", path.display());
    assert_eq!(
        stream_field(video[0], "codec_name"),
        Some("h264"),
        "video codec"
    );
    // The canvas follows the attached window's aspect (D54). The test window is 800x600,
    // so a 720-high canvas is 960 wide, not the 16:9 1280.
    assert_eq!(stream_field(video[0], "width"), Some("960"), "video width");
    assert_eq!(
        stream_field(video[0], "height"),
        Some("720"),
        "video height"
    );
    for (i, a) in audio.iter().enumerate() {
        assert_eq!(
            stream_field(a, "codec_name"),
            Some("aac"),
            "audio {i} codec"
        );
    }
    // MP4 stores a track name in the `name` tag (the title is the same atom).
    let title = |s: &[(String, String)]| {
        stream_field(s, "TAG:title")
            .or_else(|| stream_field(s, "TAG:name"))
            .map(str::to_owned)
    };
    assert_eq!(title(audio[0]).as_deref(), Some("Mix"), "track 1 title");
    assert_eq!(title(audio[1]).as_deref(), Some("Game"), "track 2 title");

    let d = reader_duration(&path);
    assert!(
        (4.0..=9.0).contains(&d),
        "part duration {d:.3} s not within 6 +/- 2 s"
    );

    rec.detach();
    assert!(rec.status().attached.is_none(), "attached after detach");
}

// ---------------------------------------------------------------------------
// clip folder layout (Windows only)
// ---------------------------------------------------------------------------

/// With per_game_folders on, a clip sits in `<clips>\Test Game\` and is named
/// `Test Game <stamp> clip-4s.mp4`.
#[cfg(windows)]
#[test]
fn clip_saved_in_per_game_folder_named_after_label() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (cfg, _, clips, _) = make_config(root.path(), "libx264", true, false);
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    wait_buffer(&rec, 4.0 + SEGMENT_SECS);
    let job = rec.clip(4).expect("clip(4) returned error");
    let (path, _, _) = wait_clip_saved(&rx, job, Duration::from_secs(20));
    assert!(path.is_file(), "clip file missing: {}", path.display());
    let parent = path.parent().expect("clip has no parent");
    assert_eq!(
        parent.file_name().and_then(|n| n.to_str()),
        Some("Test Game"),
        "clip folder: {}",
        path.display()
    );
    assert_eq!(
        parent.parent(),
        Some(clips.as_path()),
        "clip base folder: {}",
        path.display()
    );
    assert!(
        name_ok(&path, &clips, "clip-4s.mp4"),
        "clip name does not match pattern: {}",
        path.display()
    );
    rec.detach();
}

/// With per_game_folders off, a clip sits directly in the clips base folder and
/// no per-game folder is created.
#[cfg(windows)]
#[test]
fn clip_saved_flat_when_per_game_folders_off() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (mut cfg, _, clips, _) = make_config(root.path(), "libx264", true, false);
    cfg.per_game_folders = false;
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip!(rec, win.hwnd, "Test Game");

    wait_buffer(&rec, 4.0 + SEGMENT_SECS);
    let job = rec.clip(4).expect("clip(4) returned error");
    let (path, _, _) = wait_clip_saved(&rx, job, Duration::from_secs(20));
    assert!(path.is_file(), "clip file missing: {}", path.display());
    assert_eq!(
        path.parent(),
        Some(clips.as_path()),
        "clip not directly in clips base: {}",
        path.display()
    );
    assert!(
        !clips.join("Test Game").exists(),
        "per-game folder created while per_game_folders is off"
    );
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    assert!(
        name.starts_with("Test Game ") && name.ends_with(" clip-4s.mp4"),
        "clip name: {name}"
    );
    rec.detach();
}

/// The per-game folder is the sanitized label. "Odd: Name/?*" becomes "Odd Name"
/// and the reserved device name "CON" becomes "_CON". Each folder must exist.
#[cfg(windows)]
#[test]
fn clip_folder_uses_sanitized_label() {
    let _g = serial();
    let win = TestWindow::create(WINDOW_FILL);
    for (label, folder) in [("Odd: Name/?*", "Odd Name"), ("CON", "_CON")] {
        let root = tempfile::tempdir().expect("tempdir");
        let (cfg, _, clips, _) = make_config(root.path(), "libx264", true, false);
        let (tx, rx) = mpsc::channel();
        let mut rec = Recorder::new(cfg, tx);
        attach_or_skip!(rec, win.hwnd, label);

        wait_buffer(&rec, 4.0 + SEGMENT_SECS);
        let job = rec.clip(4).expect("clip(4) returned error");
        let (path, _, _) = wait_clip_saved(&rx, job, Duration::from_secs(20));
        assert!(path.is_file(), "{label}: clip missing: {}", path.display());
        let parent = path.parent().expect("clip has no parent");
        assert_eq!(
            parent.file_name().and_then(|n| n.to_str()),
            Some(folder),
            "{label}: folder of {}",
            path.display()
        );
        assert!(
            parent.is_dir(),
            "{label}: folder missing: {}",
            parent.display()
        );
        assert_eq!(
            parent.parent(),
            Some(clips.as_path()),
            "{label}: clip base folder"
        );
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert!(name.ends_with(" clip-4s.mp4"), "{label}: clip name {name}");
        rec.detach();
    }
}

/// Duration of the movie in seconds, read with the in-repo MP4 reader.
#[allow(dead_code)]
fn reader_duration(path: &std::path::Path) -> f64 {
    clipcheck_mp4::reader::read_movie(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .duration_secs
}
