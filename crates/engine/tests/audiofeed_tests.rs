//! Tests for `clipcheck_engine::audiofeed` and the recorder's audio tracks.
//!
//! Unit-level tests run everywhere. Tests that record a real window need
//! Windows Graphics Capture and WASAPI. When a prerequisite
//! is missing the test calls `skip!`, which prints `SKIPPED: <reason>` and
//! returns. Setting `CLIPCHECK_REQUIRE_CAPTURE` turns every skip into a failure.
//! `CLIPCHECK_TEST_VERBOSE` enables diagnostic output. Timing-sensitive and

#[macro_use]
mod common;

use common::serial;
#[cfg(windows)]
use common::win_window::TestWindow;

/// Fill colour of the test window (COLORREF 0x00BBGGRR).
#[cfg(windows)]
const WINDOW_FILL: u32 = 0x0030_6090;
#[cfg(windows)]
use clipcheck_testkit::tone::TonePlayer;
#[cfg(windows)]
use common::{make_config, stream_title};

/// Unwraps an `Ok` value, or skips the test with the `Err` text as the reason.
macro_rules! or_skip {
    ($res:expr, $test:expr) => {
        match $res {
            Ok(v) => v,
            Err(reason) => skip!("{}: {}", $test, reason),
        }
    };
}

use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_engine::audiofeed::{
    apply_gain, db_to_linear, silence_block, AudioFeeder, AudioSourceKind, AudioTrackConfig,
    TrackSource, SILENCE_BLOCK_MS,
};
#[cfg(windows)]
use clipcheck_engine::recorder::{Recorder, RecorderError, RecorderEvent};
#[cfg(windows)]
#[cfg(windows)]
use std::sync::mpsc::{self, Receiver};

/// Samples in one 20 ms stereo block at 48 kHz.
const BLOCK_LEN: usize = 1920;

/// Describes the tracks of `file` as the `streams` entries the checks below read: `codec_type`,
/// `duration` (seconds, as a string) and `tags.title`. Read with the in-repo MP4 reader.
fn probe_streams(file: &Path) -> Vec<serde_json::Value> {
    let movie = clipcheck_mp4::reader::read_movie(file)
        .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
    movie
        .tracks
        .iter()
        .map(|t| {
            let kind = match &t.kind {
                clipcheck_mp4::reader::TrackKind::Video { .. } => "video",
                clipcheck_mp4::reader::TrackKind::Audio { .. } => "audio",
                clipcheck_mp4::reader::TrackKind::Other { handler } => handler.as_str(),
            };
            let mut stream = serde_json::json!({
                "codec_type": kind,
                "duration": format!("{:.6}", t.duration_secs()),
            });
            if let Some(title) = &t.title {
                stream["tags"] = serde_json::json!({ "title": title });
            }
            stream
        })
        .collect()
}

fn streams_of<'a>(streams: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
    streams
        .iter()
        .filter(|s| s["codec_type"].as_str() == Some(kind))
        .collect()
}

fn duration_of(s: &serde_json::Value) -> Option<f64> {
    s["duration"].as_str()?.parse().ok()
}

fn track(title: &str, source: AudioSourceKind) -> AudioTrackConfig {
    AudioTrackConfig {
        title: title.to_string(),
        sources: vec![TrackSource {
            kind: source,
            gain_db: 0.0,
            gate_db: None,
        }],
        bitrate_kbps: 128,
    }
}

/// A microphone that does not exist, so the track must fall back to silence.
fn missing_mic() -> AudioSourceKind {
    AudioSourceKind::Microphone {
        device: Some("no-such-device-id-xyz".to_string()),
    }
}

/// Reads receiver blocks until it reports `Disconnected`. Panics on a 2 s timeout.
fn expect_disconnect(rx: &mpsc::Receiver<Vec<f32>>, what: &str) {
    for _ in 0..10_000 {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(_) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                panic!("{what}: receiver still connected 2 s after stop")
            }
        }
    }
    panic!("{what}: receiver never disconnected");
}

// ---------------------------------------------------------------------------
// pure functions (all platforms)
// ---------------------------------------------------------------------------

#[test]
fn db_to_linear_known_values() {
    assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6, "0 dB");
    assert!((db_to_linear(-6.0) - 0.501).abs() < 0.002, "-6 dB");
    assert!((db_to_linear(20.0) - 10.0).abs() < 1e-4, "+20 dB");
    assert_eq!(db_to_linear(f32::NEG_INFINITY), 0.0, "-inf dB");
    assert_eq!(db_to_linear(f32::NAN), 1.0, "NaN dB");
}

#[test]
fn apply_gain_scales_and_clamps() {
    assert_eq!(apply_gain(&[0.5, -0.5], 4.0), vec![1.0, -1.0], "x4 clamp");
    assert_eq!(apply_gain(&[0.25, -0.75], 0.0), vec![0.0, 0.0], "gain 0");
    assert_eq!(
        apply_gain(&[2.0, -3.0, 0.3], 1.0),
        vec![1.0, -1.0, 0.3],
        "out of range input"
    );
}

#[test]
fn apply_gain_maps_nan_to_zero_and_keeps_length() {
    let out = apply_gain(&[f32::NAN, 0.5], 1.0);
    assert_eq!(out[0], 0.0, "NaN sample");
    assert_eq!(out[1], 0.5, "normal sample");
    let out = apply_gain(&[0.5, -0.5], f32::NAN);
    assert_eq!(out, vec![0.0, 0.0], "NaN gain");
    assert_eq!(apply_gain(&[0.1; 7], 1.0).len(), 7, "length");
    assert!(apply_gain(&[], 2.0).is_empty(), "empty input");
}

#[test]
fn silence_block_is_20ms_of_stereo_zeros() {
    assert_eq!(SILENCE_BLOCK_MS, 20, "block duration");
    let b = silence_block();
    assert_eq!(b.len(), BLOCK_LEN, "block length");
    assert!(b.iter().all(|&s| s == 0.0), "block not all zero");
}

// ---------------------------------------------------------------------------
// AudioFeeder without any real source (silence fallback, all platforms)
// ---------------------------------------------------------------------------

#[test]
fn feeder_empty_tracks_is_ok_and_empty() {
    let _g = serial();
    let (mut feeder, rxs) = AudioFeeder::start(&[], 0).expect("start with no tracks");
    assert!(rxs.is_empty(), "receivers for zero tracks: {}", rxs.len());
    assert!(feeder.warnings().is_empty(), "warnings for zero tracks");
    feeder.stop();
}

#[test]
fn feeder_fallback_tracks_feed_silence_and_warn() {
    let _g = serial();
    let tracks = vec![
        track("Game", AudioSourceKind::GameProcess),
        track("Other", AudioSourceKind::AllExceptGame),
        track("Mic", missing_mic()),
    ];
    let (mut feeder, rxs) = AudioFeeder::start(&tracks, 0).expect("AudioFeeder::start");
    assert_eq!(rxs.len(), 3, "receiver count");

    for (rx, t) in rxs.iter().zip(&tracks) {
        let block = rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_else(|e| panic!("track {:?}: no block within 1 s: {e:?}", t.title));
        assert_eq!(block.len(), BLOCK_LEN, "track {:?} block length", t.title);
        assert!(
            block.iter().all(|&s| s == 0.0),
            "track {:?} fallback block not silent",
            t.title
        );
    }

    let warnings = feeder.warnings();
    assert!(
        warnings.len() >= 3,
        "expected >= 3 warnings, got {warnings:?}"
    );
    for t in &tracks {
        let quoted = format!("\"{}\"", t.title);
        assert!(
            warnings.iter().any(|w| w.contains(&quoted)),
            "no warning names track {quoted}: {warnings:?}"
        );
    }
    assert!(
        feeder.warnings().is_empty(),
        "second warnings() call not empty"
    );

    let t0 = Instant::now();
    feeder.stop();
    let stop_took = t0.elapsed();
    assert!(
        stop_took < Duration::from_secs(2),
        "stop took {stop_took:?}"
    );
    for (rx, t) in rxs.iter().zip(&tracks) {
        expect_disconnect(rx, &t.title);
    }
    feeder.stop(); // idempotent

    // Drop also stops and disconnects.
    let (dropped, rxs) = AudioFeeder::start(&tracks[..1], 0).expect("second start");
    let rx = &rxs[0];
    rx.recv_timeout(Duration::from_secs(1))
        .expect("block before drop");
    drop(dropped);
    expect_disconnect(rx, "dropped feeder");
}

#[test]
fn feeder_silence_is_paced_in_real_time() {
    let _g = serial();
    let tracks = [track("Paced", AudioSourceKind::AllExceptGame)];
    let (mut feeder, rxs) = AudioFeeder::start(&tracks, 0).expect("AudioFeeder::start");
    let rx = &rxs[0];

    // 100 blocks at 20 ms each: 99 intervals, about 1980 ms nominal. The lower
    // bound (15 ms average per interval) rejects a burst. The upper bound is
    // deliberately generous (60 ms average) so scheduler jitter on a loaded
    // machine does not fail the test; a stall is still caught by the 2 s
    // per-block receive timeout above.
    const BLOCKS: usize = 100;
    let start = Instant::now();
    for i in 0..BLOCKS {
        let block = rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|e| panic!("block {i}: {e:?}"));
        assert_eq!(block.len(), BLOCK_LEN, "block {i} length");
    }
    let elapsed = start.elapsed();
    feeder.stop();

    let intervals = (BLOCKS - 1) as u32;
    let (lo, hi) = (
        Duration::from_millis(15) * intervals,
        Duration::from_millis(60) * intervals,
    );
    assert!(
        elapsed >= lo,
        "{BLOCKS} silence blocks in {elapsed:?}: too fast, under {lo:?} (burst?)"
    );
    assert!(
        elapsed <= hi,
        "{BLOCKS} silence blocks in {elapsed:?}: too slow, over {hi:?}"
    );
}

// ---------------------------------------------------------------------------
// AudioFeeder with a real source (Windows)
// ---------------------------------------------------------------------------

/// Plays a known 880 Hz tone through a TonePlayer while two system-mix tracks record:
/// a control at 0 dB and a copy at -200 dB. The control proves that the tone
/// reached loopback, so the muted copy's silence is caused by its gain.
#[cfg(windows)]
#[test]
fn feeder_gain_minus_200_db_silences_system_mix() {
    let test = "feeder_gain_minus_200_db_silences_system_mix";
    let _g = serial();
    let control = track("Control", AudioSourceKind::SystemMix);
    let mut muted = track("Muted", AudioSourceKind::SystemMix);
    muted.sources[0].gain_db = -200.0;
    let tracks = [control, muted];
    let (mut feeder, rxs) = AudioFeeder::start(&tracks, 0).expect("AudioFeeder::start");

    let tone_dir = tempfile::tempdir().expect("tempdir");
    let tone = match TonePlayer::spawn(tone_dir.path(), 880.0, 6) {
        Ok(tone) => tone,
        Err(e) => skip!("{test}: could not start the tone player: {e}"),
    };

    let mut peak = [0f32; 2];
    let mut blocks = [0usize; 2];
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        for (i, rx) in rxs.iter().enumerate() {
            while let Ok(b) = rx.try_recv() {
                blocks[i] += 1;
                for s in b {
                    peak[i] = peak[i].max(s.abs());
                }
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    drop(tone);
    feeder.stop();
    vlog!("{test}: blocks {blocks:?}, peak {peak:?} (control, muted)");

    // Without blocks and a clearly audible signal on the control track the
    // muted check would pass trivially, so the test skips in that case rather
    // than passing. A silent muted track is only meaningful when the control
    // track proves that the tone reached loopback.
    if blocks[0] == 0 {
        skip!("{test}: control track delivered no blocks; no loopback capture?");
    }
    if peak[0] <= 1e-3 {
        skip!(
            "{test}: no system audio reached loopback (control peak {:e}); no render device?",
            peak[0]
        );
    }
    assert!(blocks[1] > 0, "{test}: muted track delivered no blocks");
    assert!(
        peak[1] < 1e-6,
        "{test}: muted peak {:e} after -200 dB gain (control peak {:e})",
        peak[1],
        peak[0]
    );
}

// ---------------------------------------------------------------------------
// recorder end-to-end (Windows)
// ---------------------------------------------------------------------------

/// Waits for the final session part, collecting every event on the way.
#[cfg(windows)]
fn collect_until_final_part(
    rx: &Receiver<RecorderEvent>,
    timeout: Duration,
) -> (Option<PathBuf>, Vec<RecorderEvent>) {
    let deadline = Instant::now() + timeout;
    let mut all = Vec::new();
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return (None, all);
        };
        match rx.recv_timeout(left) {
            Ok(ev) => {
                let final_path = match &ev {
                    RecorderEvent::SessionPartSaved {
                        path,
                        final_part: true,
                        ..
                    } => Some(path.clone()),
                    _ => None,
                };
                all.push(ev);
                if let Some(p) = final_path {
                    return (Some(p), all);
                }
            }
            Err(_) => return (None, all),
        }
    }
}

#[cfg(windows)]
struct Recording {
    part: PathBuf,
    events: Vec<RecorderEvent>,
    _root: tempfile::TempDir,
}

/// Attaches to a fresh test window, records one session for `secs` seconds, and
/// detaches. Returns `Err(reason)` when the window or the
/// capture cannot be set up (a skip); real failures panic.
#[cfg(windows)]
fn record_window(
    test: &str,
    tracks: Vec<AudioTrackConfig>,
    secs: u64,
) -> Result<Recording, String> {
    let win = TestWindow::create(WINDOW_FILL);
    let root = tempfile::tempdir().expect("tempdir");
    let (mut cfg, _, _, sessions) = make_config(root.path(), "libx264", true, false);
    cfg.per_game_folders = false;
    cfg.audio_tracks = tracks;
    let (tx, rx) = mpsc::channel();
    let mut rec = Recorder::new(cfg, tx);
    attach_or_skip(&mut rec, win.hwnd, test)?;
    rec.session_start().expect("session_start");
    let start = Instant::now();

    if let Some(left) = (start + Duration::from_secs(secs)).checked_duration_since(Instant::now()) {
        thread::sleep(left);
    }
    rec.session_stop().expect("session_stop");
    let (part, mut events) = collect_until_final_part(&rx, Duration::from_secs(30));
    let part = part.unwrap_or_else(|| panic!("{test}: no final SessionPartSaved within 30 s"));
    rec.detach();
    events.extend(rx.try_iter());

    assert!(part.is_file(), "{test}: part missing: {}", part.display());
    assert_eq!(
        part.parent(),
        Some(sessions.as_path()),
        "{test}: part not in sessions dir"
    );
    Ok(Recording {
        part,
        events,
        _root: root,
    })
}

/// Attach failures caused by the environment (no WGC, no device) are skips.
/// Anything else is a real failure.
#[cfg(windows)]
fn attach_or_skip(rec: &mut Recorder, hwnd: isize, test: &str) -> Result<(), String> {
    match rec.attach(hwnd, "Test Game") {
        Ok(()) => Ok(()),
        Err(RecorderError::Unsupported) => Err(format!(
            "{test}: recording is not supported on this platform"
        )),
        Err(e) => panic!("{test}: attach to the test window failed: {e:?}"),
    }
}

#[cfg(windows)]
fn three_tracks() -> Vec<AudioTrackConfig> {
    vec![
        track("Mix", AudioSourceKind::SystemMix),
        track("Game", AudioSourceKind::GameProcess),
        AudioTrackConfig {
            title: "Mic".to_string(),
            sources: vec![TrackSource {
                kind: AudioSourceKind::Microphone { device: None },
                gain_db: 0.0,
                gate_db: None,
            }],
            bitrate_kbps: 128,
        },
    ]
}

#[cfg(windows)]
#[test]
fn recorder_three_audio_tracks_structure() {
    let test = "recorder_three_audio_tracks_structure";
    let _g = serial();
    let rec = or_skip!(record_window(test, three_tracks(), 8), test);

    let streams = probe_streams(&rec.part);
    let video = streams_of(&streams, "video");
    let audio = streams_of(&streams, "audio");
    assert_eq!(video.len(), 1, "video streams");
    assert_eq!(audio.len(), 3, "audio streams");
    let vd = duration_of(video[0]).expect("video stream duration");
    for (i, a) in audio.iter().enumerate() {
        let ad = duration_of(a).unwrap_or_else(|| panic!("audio {i} has no duration"));
        assert!(
            (ad - vd).abs() <= 1.0,
            "audio {i} duration {ad:.3} s vs video {vd:.3} s"
        );
    }
    // Checked last so the other assertions still run while titles are broken.
    let titles: Vec<String> = audio.iter().map(|s| stream_title(s)).collect();
    assert_eq!(titles, ["Mix", "Game", "Mic"], "audio titles in order");
}

#[cfg(windows)]
#[test]
fn recorder_missing_mic_device_warns_and_still_records() {
    let test = "recorder_missing_mic_device_warns_and_still_records";
    let _g = serial();
    let tracks = vec![
        track("Mix", AudioSourceKind::SystemMix),
        AudioTrackConfig {
            title: "Mic".to_string(),
            sources: vec![TrackSource {
                kind: missing_mic(),
                gain_db: 0.0,
                gate_db: None,
            }],
            bitrate_kbps: 128,
        },
        track("Game", AudioSourceKind::GameProcess),
    ];
    let rec = or_skip!(record_window(test, tracks, 5), test);

    let streams = probe_streams(&rec.part);
    assert_eq!(streams_of(&streams, "video").len(), 1, "video streams");
    let audio = streams_of(&streams, "audio");
    assert_eq!(audio.len(), 3, "audio streams");
    let warned = rec.events.iter().any(|e| match e {
        RecorderEvent::Warning(text) => text.contains("Mic"),
        _ => false,
    });
    assert!(
        warned,
        "no Warning mentioning \"Mic\"; events: {:?}",
        rec.events
    );
    // Checked last so the other assertions still run while titles are broken.
    let titles: Vec<String> = audio.iter().map(|s| stream_title(s)).collect();
    assert_eq!(titles, ["Mix", "Mic", "Game"], "audio titles in order");
}

#[cfg(windows)]
#[test]
fn recorder_without_audio_tracks_is_video_only() {
    let test = "recorder_without_audio_tracks_is_video_only";
    let _g = serial();
    let rec = or_skip!(record_window(test, Vec::new(), 5), test);

    let streams = probe_streams(&rec.part);
    assert_eq!(streams_of(&streams, "video").len(), 1, "video streams");
    assert_eq!(streams_of(&streams, "audio").len(), 0, "audio streams");
    let warnings: Vec<&RecorderEvent> = rec
        .events
        .iter()
        .filter(|e| matches!(e, RecorderEvent::Warning(_)))
        .collect();
    assert!(
        warnings.is_empty(),
        "unexpected Warning events: {warnings:?}"
    );
}
