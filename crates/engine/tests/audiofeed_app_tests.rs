//! End-to-end check of per-application capture: a `TonePlayer` (a hidden PowerShell
//! process playing an 880 Hz tone) runs, and the feeder's `App` sources must pick it up.
//!
//! Windows only. When the tone player cannot start the test prints
//! `SKIPPED: <reason>` and returns; `CLIPCHECK_REQUIRE_CAPTURE` turns that into a
//! failure. `CLIPCHECK_TEST_VERBOSE` enables diagnostic output. The test does
//! not depend on a microphone: a missing mic only produces a warning.

#![cfg(windows)]

/// Prints `SKIPPED: <reason>` and returns from the test. With
/// `CLIPCHECK_REQUIRE_CAPTURE` set, it panics instead.
macro_rules! skip {
    ($($arg:tt)+) => {{
        let reason = format!($($arg)+);
        if std::env::var_os("CLIPCHECK_REQUIRE_CAPTURE").is_some() {
            panic!("CLIPCHECK_REQUIRE_CAPTURE is set, so a skip is a failure: {reason}");
        }
        eprintln!("SKIPPED: {reason}");
        return;
    }};
}

/// Prints only when `CLIPCHECK_TEST_VERBOSE` is set.
macro_rules! verbose {
    ($($arg:tt)+) => {
        if std::env::var_os("CLIPCHECK_TEST_VERBOSE").is_some() {
            eprintln!($($arg)+);
        }
    };
}

use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_engine::audiofeed::{AudioFeeder, AudioSourceKind, AudioTrackConfig, TrackSource};
use clipcheck_testkit::tone::TONE_PLAYER_EXE;

/// Samples in one 20 ms stereo block at 48 kHz.
const BLOCK_LEN: usize = 1920;
const SAMPLE_RATE: f64 = 48_000.0;
const TONE_HZ: f64 = 880.0;
/// A frequency with no tone in it, used to estimate the noise floor.
const OFF_TONE_HZ: f64 = 1234.5;
/// Time to let the feeder capture the app before blocks are judged.
const WARM_UP: Duration = Duration::from_secs(2);
/// Total collection time, warm-up included.
const RUN: Duration = Duration::from_secs(5);

/// Normalised magnitude of `freq` in `samples`: 2 |X(f)| / N. A pure sine of
/// amplitude A at exactly `freq` gives about A.
fn goertzel_mag(samples: &[f64], freq: f64, rate: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * freq / rate;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &x) in samples.iter().enumerate() {
        let phase = w * i as f64;
        re += x * phase.cos();
        im -= x * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / samples.len() as f64
}

/// Left channel of interleaved stereo blocks, as f64.
fn left_channel(blocks: &[Vec<f32>]) -> Vec<f64> {
    blocks
        .iter()
        .flat_map(|b| b.iter().step_by(2).map(|&s| f64::from(s)))
        .collect()
}

fn app_source(exe: &str, gain_db: f32) -> TrackSource {
    TrackSource {
        kind: AudioSourceKind::App {
            exe: exe.to_string(),
        },
        gain_db,
        gate_db: None,
    }
}

#[test]
fn app_sources_capture_tone_player() {
    let test = "app_sources_capture_tone_player";
    let dir = tempfile::tempdir().expect("tempdir");
    let _tone = match clipcheck_testkit::tone::TonePlayer::spawn(dir.path(), TONE_HZ as f32, 10) {
        Ok(tone) => tone,
        Err(e) => skip!("{test}: could not start the tone player: {e}"),
    };

    // Let the tone player open its audio device before the feeder's first scan.
    thread::sleep(Duration::from_secs(1));

    let tracks = vec![
        AudioTrackConfig {
            title: "Mix".to_string(),
            sources: vec![
                app_source(TONE_PLAYER_EXE, 0.0),
                TrackSource {
                    kind: AudioSourceKind::Microphone { device: None },
                    gain_db: 0.0,
                    gate_db: None,
                },
            ],
            bitrate_kbps: 128,
        },
        AudioTrackConfig {
            title: "AppOnly".to_string(),
            sources: vec![app_source(TONE_PLAYER_EXE, -20.0)],
            bitrate_kbps: 128,
        },
        AudioTrackConfig {
            title: "Silent".to_string(),
            sources: vec![app_source("definitely-not-running.exe", 0.0)],
            bitrate_kbps: 128,
        },
    ];

    let (mut feeder, rxs) = AudioFeeder::start(&tracks, 0).expect("AudioFeeder::start");
    assert_eq!(rxs.len(), tracks.len(), "one receiver per track");

    // Collect for RUN. Blocks received after WARM_UP are kept.
    let t0 = Instant::now();
    let mut kept: Vec<Vec<Vec<f32>>> = vec![Vec::new(); tracks.len()];
    let mut closed = vec![false; tracks.len()];
    while t0.elapsed() < RUN {
        for (i, rx) in rxs.iter().enumerate() {
            if closed[i] {
                continue;
            }
            drain(rx, &mut kept[i], &mut closed[i], t0, &tracks[i].title);
        }
        thread::sleep(Duration::from_millis(5));
    }

    let warnings = feeder.warnings();
    verbose!("{test}: feeder warnings: {warnings:?}");
    feeder.stop();

    for (t, blocks) in tracks.iter().zip(&kept) {
        assert!(
            blocks.len() >= 120,
            "track {:?}: {} blocks in the 3 s window, expected >= 120",
            t.title,
            blocks.len()
        );
    }

    let mix = left_channel(&kept[0]);
    let app = left_channel(&kept[1]);
    let silent = &kept[2];

    let mix_mag = goertzel_mag(&mix, TONE_HZ, SAMPLE_RATE);
    let app_mag = goertzel_mag(&app, TONE_HZ, SAMPLE_RATE);
    verbose!("{test}: 880 Hz magnitude: Mix {mix_mag:.5}, AppOnly {app_mag:.5}");

    // The tone player is kept quiet, so the
    // noise floor is measured at an off-tone bin and the tone must stand
    // clearly above it.
    let mix_floor = goertzel_mag(&mix, OFF_TONE_HZ, SAMPLE_RATE);
    let app_floor = goertzel_mag(&app, OFF_TONE_HZ, SAMPLE_RATE);
    verbose!("{test}: off-tone {OFF_TONE_HZ} Hz floor: Mix {mix_floor:.5}, AppOnly {app_floor:.5}");
    assert!(
        mix_mag > 0.005 && mix_mag > 5.0 * mix_floor,
        "{test}: Mix tone magnitude {mix_mag} not above noise floor {mix_floor}"
    );
    assert!(
        app_mag > 5.0 * app_floor,
        "{test}: AppOnly tone magnitude {app_mag} not above noise floor {app_floor}"
    );

    let ratio = app_mag / mix_mag;
    verbose!("{test}: AppOnly / Mix ratio {ratio:.4} (expected about 0.1)");
    assert!(
        (0.06..=0.14).contains(&ratio),
        "{test}: AppOnly/Mix ratio {ratio:.4} outside [0.06, 0.14] for a -20 dB track"
    );

    let peak = silent
        .iter()
        .flat_map(|b| b.iter())
        .fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(
        silent.iter().all(|b| b.iter().all(|&s| s == 0.0)),
        "{test}: Silent track has non-zero samples (peak {peak})"
    );
}

/// Moves every block ready on `rx` into `kept` (after warm-up) and checks the
/// block size of each one. Sets `closed` when the feeder has disconnected.
fn drain(
    rx: &Receiver<Vec<f32>>,
    kept: &mut Vec<Vec<f32>>,
    closed: &mut bool,
    t0: Instant,
    title: &str,
) {
    loop {
        match rx.try_recv() {
            Ok(block) => {
                assert_eq!(block.len(), BLOCK_LEN, "track {title:?}: block length");
                if t0.elapsed() >= WARM_UP {
                    kept.push(block);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                *closed = true;
                break;
            }
        }
    }
}
