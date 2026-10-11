//! Microphone and system-mix capture tests (Windows only).
//!
//! Skip policy (see `common/mod.rs`): a missing capture or render endpoint is a
//! skip, reported through `common::skip_or_fail`. It is never a pass. With
//! `CLIPCHECK_REQUIRE_AUDIO=1` the skip becomes a failure. A `NoDevice` result
//! from an unknown endpoint id (3c) is the API contract itself, so it is
//! asserted, not skipped.

#![cfg(windows)]

mod common;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use clipcheck_audio::{AudioChunk, AudioError, Microphone, SystemMix};

const WINDOW: Duration = Duration::from_secs(1);

/// Collects chunks for `WINDOW`, returning them. Receiver is consumed.
fn collect_window(rx: &Receiver<AudioChunk>) -> Vec<AudioChunk> {
    let mut out = Vec::new();
    let deadline = Instant::now() + WINDOW;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(c) => out.push(c),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    out.extend(rx.try_iter());
    out
}

fn check_chunks(label: &str, chunks: &[AudioChunk], require_range: bool) {
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
        if require_range {
            assert!(
                c.samples.iter().all(|s| (-1.0..=1.0).contains(s)),
                "{label}: chunk {i} has samples outside [-1,1]"
            );
        }
    }
}

#[test]
fn test_3a_microphone_default_endpoint() {
    match Microphone::start(None) {
        Err(AudioError::NoDevice) => {
            common::skip_or_fail("3a: no default capture endpoint (NoDevice)");
        }
        Err(other) => panic!("3a: Microphone::start(None) failed: {other:?}"),
        Ok((mut mic, rx)) => {
            let chunks = collect_window(&rx);
            mic.stop();
            let total_samples: usize = chunks.iter().map(|c| c.samples.len()).sum();
            let silent = chunks.iter().filter(|c| c.silent).count();
            println!(
                "3a mic Ok: chunks={} silent={} samples={}",
                chunks.len(),
                silent,
                total_samples
            );
            assert!(
                !chunks.is_empty(),
                "3a: no chunk arrived within 1 s from an open mic"
            );
            check_chunks("3a mic", &chunks, true);
        }
    }
}

#[test]
fn test_3b_system_mix_default_render() {
    match SystemMix::start() {
        Err(AudioError::NoDevice) => {
            common::skip_or_fail("3b: no default render endpoint (NoDevice)");
        }
        Err(other) => panic!("3b: SystemMix::start() failed: {other:?}"),
        Ok((mut mix, rx)) => {
            let chunks = collect_window(&rx);
            mix.stop();
            let silent = chunks.iter().filter(|c| c.silent).count();
            println!("3b system mix: chunks={} silent={}", chunks.len(), silent);
            assert!(
                !chunks.is_empty(),
                "3b: no chunk arrived within 1 s from SystemMix"
            );
            check_chunks("3b system mix", &chunks, false);
        }
    }
}

#[test]
fn test_3c_microphone_unknown_endpoint_id() {
    match Microphone::start(Some("this-endpoint-id-does-not-exist".into())) {
        Err(AudioError::NoDevice) => {}
        Err(other) => panic!("3c: expected NoDevice, got {other:?}"),
        Ok((mut mic, _rx)) => {
            mic.stop();
            panic!("3c: unknown endpoint id unexpectedly opened a stream");
        }
    }
}
