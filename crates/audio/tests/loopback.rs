//! Process loopback integration tests (Windows only).
//!
//! Plays an 880 Hz tone through `clipcheck_testkit::tone` (a PowerShell
//! `SoundPlayer` process) and captures it with ProcessLoopback in IncludeTree and
//! ExcludeTree modes.
//!
//! Skip policy (see `common/mod.rs`): when process loopback is unsupported the
//! test reports `SKIPPED` through `common::skip_or_fail` and returns. With
//! `CLIPCHECK_REQUIRE_AUDIO=1` the same condition fails the test. A skip is never
//! reported as a pass.

#![cfg(windows)]

mod common;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Barrier, Mutex};
use std::time::{Duration, Instant};

use clipcheck_audio::{is_supported, AudioChunk, AudioError, LoopbackMode, ProcessLoopback};

/// Serialises tests so one tone's audio never leaks into another test's capture.
static SERIAL: Mutex<()> = Mutex::new(());

const CAPTURE_WINDOW: Duration = Duration::from_secs(2);

fn lock_serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug)]
struct Stats {
    chunks: Vec<AudioChunk>,
    total_chunks: usize,
    nonsilent_count: usize,
    /// RMS over non-silent chunks only.
    rms_nonsilent: f64,
    /// RMS over all received samples; silent chunks contribute zeros.
    rms_all: f64,
}

fn rms(sum_sq: f64, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        (sum_sq / n as f64).sqrt()
    }
}

fn stats_of(chunks: Vec<AudioChunk>) -> Stats {
    let mut ns_sum = 0.0f64;
    let mut ns_n = 0usize;
    let mut all_sum = 0.0f64;
    let mut all_n = 0usize;
    let mut nonsilent_count = 0usize;
    for c in &chunks {
        all_n += c.samples.len();
        if c.silent {
            continue;
        }
        nonsilent_count += 1;
        for &x in &c.samples {
            let x = x as f64;
            ns_sum += x * x;
            all_sum += x * x;
            ns_n += 1;
        }
    }
    Stats {
        total_chunks: chunks.len(),
        nonsilent_count,
        rms_nonsilent: rms(ns_sum, ns_n),
        rms_all: rms(all_sum, all_n),
        chunks,
    }
}

/// Collects chunks from `rx` for `CAPTURE_WINDOW`, then stops `lb` and drains the rest.
fn collect(mut lb: ProcessLoopback, rx: Receiver<AudioChunk>) -> Stats {
    let mut chunks = Vec::new();
    let deadline = Instant::now() + CAPTURE_WINDOW;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(c) => chunks.push(c),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    lb.stop();
    chunks.extend(rx.try_iter());
    stats_of(chunks)
}

/// Goertzel magnitude of `freq` Hz in the left channel of interleaved-stereo 48 kHz chunks.
/// Immune to ambient system audio at other frequencies.
fn tone_magnitude(chunks: &[AudioChunk], freq: f64) -> f64 {
    let left: Vec<f64> = chunks
        .iter()
        .flat_map(|c| c.samples.iter().step_by(2).map(|&v| v as f64))
        .collect();
    if left.is_empty() {
        return 0.0;
    }
    let w = 2.0 * std::f64::consts::PI * freq / 48_000.0;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for x in &left {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    power.max(0.0).sqrt() / left.len() as f64
}

fn assert_chunk_shape(label: &str, chunks: &[AudioChunk]) {
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(
            c.samples.len() % 2,
            0,
            "{label}: chunk {i} has odd sample count {}",
            c.samples.len()
        );
        assert!(
            c.samples.iter().all(|s| s.is_finite()),
            "{label}: chunk {i} has non-finite samples"
        );
        assert!(
            c.samples.iter().all(|s| (-1.0..=1.0).contains(s)),
            "{label}: chunk {i} has samples outside [-1,1]"
        );
    }
}

/// Starts a loopback capture on `pid`. An `Unsupported` result is a skip (reported
/// through `common::skip_or_fail`, then `None`). Any other error is a failure.
fn start_or_skip(pid: u32, mode: LoopbackMode) -> Option<(ProcessLoopback, Receiver<AudioChunk>)> {
    match ProcessLoopback::start(pid, mode) {
        Ok(pair) => Some(pair),
        Err(AudioError::Unsupported(why)) => {
            common::skip_or_fail(&format!("process loopback unsupported ({why})"));
            None
        }
        Err(e) => panic!("ProcessLoopback::start({pid}, {mode:?}) failed: {e:?}"),
    }
}

#[test]
fn test_2a_include_tree_captures_tone() {
    let _g = lock_serial();
    if let Err(e) = is_supported() {
        common::skip_or_fail(&format!("process loopback unsupported: {e:?}"));
        return;
    }

    let tone = common::start_tone(880.0, 30);
    let pid = tone.pid();
    std::thread::sleep(Duration::from_millis(500));

    let Some((lb, rx)) = start_or_skip(pid, LoopbackMode::IncludeTree) else {
        return;
    };
    let stats = collect(lb, rx);
    drop(tone);

    println!(
        "2a IncludeTree pid={pid}: total_chunks={} nonsilent={} rms_nonsilent={:.6} rms_all={:.6}",
        stats.total_chunks, stats.nonsilent_count, stats.rms_nonsilent, stats.rms_all
    );

    assert_chunk_shape("2a include", &stats.chunks);
    assert!(
        stats.nonsilent_count > 0,
        "2a: no non-silent chunks from IncludeTree"
    );
    assert!(
        stats.rms_nonsilent > 0.001,
        "2a: rms of non-silent samples {:.6} not > 0.001",
        stats.rms_nonsilent
    );
}

#[test]
fn test_2b_exclude_tree_quieter_than_include() {
    let _g = lock_serial();
    if let Err(e) = is_supported() {
        common::skip_or_fail(&format!("process loopback unsupported: {e:?}"));
        return;
    }

    let tone = common::start_tone(880.0, 30);
    let pid = tone.pid();
    std::thread::sleep(Duration::from_millis(500));

    // Both captures start behind a barrier so they cover the same playback window.
    let barrier = Barrier::new(2);
    let (inc, exc) = std::thread::scope(|s| {
        let h_inc = s.spawn(|| {
            barrier.wait();
            start_or_skip(pid, LoopbackMode::IncludeTree).map(|(lb, rx)| collect(lb, rx))
        });
        let h_exc = s.spawn(|| {
            barrier.wait();
            start_or_skip(pid, LoopbackMode::ExcludeTree).map(|(lb, rx)| collect(lb, rx))
        });
        (
            h_inc.join().expect("include thread panicked"),
            h_exc.join().expect("exclude thread panicked"),
        )
    });
    drop(tone);

    // A missing side was already reported as a skip by `start_or_skip`.
    let (Some(inc), Some(exc)) = (inc, exc) else {
        return;
    };

    println!(
        "2b include pid={pid}: total_chunks={} nonsilent={} rms_all={:.6}",
        inc.total_chunks, inc.nonsilent_count, inc.rms_all
    );
    println!(
        "2b exclude pid={pid}: total_chunks={} nonsilent={} rms_all={:.6}",
        exc.total_chunks, exc.nonsilent_count, exc.rms_all
    );

    assert_chunk_shape("2b include", &inc.chunks);
    assert_chunk_shape("2b exclude", &exc.chunks);
    let inc_tone = tone_magnitude(&inc.chunks, 880.0);
    let exc_tone = tone_magnitude(&exc.chunks, 880.0);
    println!("2b 880Hz magnitude: include={inc_tone:.6} exclude={exc_tone:.6}");
    assert!(
        inc_tone > 0.001 && exc_tone * 5.0 < inc_tone,
        "2b: 880 Hz tone should be present in include ({inc_tone:.6}) and absent from exclude ({exc_tone:.6})"
    );
}
