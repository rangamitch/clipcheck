//! Public-API tests for the audiomix level meters and a single-source mixer path.

use std::sync::atomic::{AtomicUsize, Ordering};

use clipcheck_engine::audiomix::{
    block_peak_rms_db, LevelTap, Mixer, SourceEdge, BLOCK_SAMPLES, SILENCE_DB,
};

const EPS: f32 = 0.01;

fn db(x: f32) -> f32 {
    20.0 * x.log10()
}

#[test]
fn empty_zero_and_nan_inputs_are_floored() {
    assert_eq!(block_peak_rms_db(&[]), (SILENCE_DB, SILENCE_DB));
    assert_eq!(block_peak_rms_db(&[0.0; 64]), (SILENCE_DB, SILENCE_DB));
    assert_eq!(block_peak_rms_db(&[f32::NAN; 8]), (SILENCE_DB, SILENCE_DB));
}

#[test]
fn nan_samples_count_as_zero_in_mixed_block() {
    // NaN counts as zero, so the 0.5 samples set both peak and rms.
    let (peak, rms) = block_peak_rms_db(&[f32::NAN, 0.5]);
    assert!(!peak.is_nan() && !rms.is_nan());
    assert!((peak - db(0.5)).abs() < EPS, "peak {peak}");
    assert!((rms - db((0.25f32 / 2.0).sqrt())).abs() < EPS, "rms {rms}");
}

#[test]
fn infinite_input_never_yields_nan_or_neg_inf() {
    let (peak, rms) = block_peak_rms_db(&[f32::INFINITY, 0.1]);
    assert!(!peak.is_nan() && peak != f32::NEG_INFINITY, "peak {peak}");
    assert!(!rms.is_nan() && rms != f32::NEG_INFINITY, "rms {rms}");
}

#[test]
fn full_scale_constant_block_is_zero_db() {
    let (peak, rms) = block_peak_rms_db(&[1.0; 480]);
    assert!(peak.abs() < 1e-4, "peak {peak}");
    assert!(rms.abs() < 1e-4, "rms {rms}");
}

#[test]
fn half_amplitude_block_is_minus_six_db() {
    let (peak, rms) = block_peak_rms_db(&[0.5; 480]);
    assert!((peak - (-6.0206)).abs() < EPS, "peak {peak}");
    assert!((rms - (-6.0206)).abs() < EPS, "rms {rms}");
}

#[test]
fn tap_peak_holds_max_then_take_resets_while_rms_keeps_last_block() {
    let tap = LevelTap::new(1);
    tap.record(0, &[0.2; 4]);
    tap.record(0, &[0.5; 4]);
    tap.record(0, &[0.3; 4]);

    let first = tap.take();
    assert_eq!(first.len(), 1);
    assert!(
        (first[0].peak_db - db(0.5)).abs() < EPS,
        "peak {}",
        first[0].peak_db
    );
    assert!(
        (first[0].rms_db - db(0.3)).abs() < EPS,
        "rms {}",
        first[0].rms_db
    );

    let second = tap.take();
    assert_eq!(second[0].peak_db, SILENCE_DB, "peak must reset");
    assert!(
        (second[0].rms_db - db(0.3)).abs() < EPS,
        "rms must persist, got {}",
        second[0].rms_db
    );
}

#[test]
fn two_tracks_are_independent() {
    let tap = LevelTap::new(2);
    tap.record(0, &[0.5; 4]);
    tap.record(1, &[0.1; 4]);
    let first = tap.take();
    assert!((first[0].peak_db - db(0.5)).abs() < EPS);
    assert!((first[1].peak_db - db(0.1)).abs() < EPS);

    tap.record(1, &[0.8; 4]);
    let second = tap.take();
    assert_eq!(second[0].peak_db, SILENCE_DB);
    assert!(
        (second[0].rms_db - db(0.5)).abs() < EPS,
        "track 0 rms {}",
        second[0].rms_db
    );
    assert!((second[1].peak_db - db(0.8)).abs() < EPS);
}

#[test]
fn out_of_range_track_is_ignored_without_panic() {
    let tap = LevelTap::new(2);
    tap.record(5, &[1.0; 4]);
    tap.record(usize::MAX, &[1.0; 4]);
    let snaps = tap.take();
    assert_eq!(snaps.len(), 2);
    assert!(snaps
        .iter()
        .all(|s| s.peak_db == SILENCE_DB && s.rms_db == SILENCE_DB));

    tap.record(0, &[0.5; 4]);
    tap.record(9, &[1.0; 4]);
    let snaps = tap.take();
    assert!((snaps[0].peak_db - db(0.5)).abs() < EPS);
    assert_eq!(snaps[1].peak_db, SILENCE_DB);
}

#[test]
fn zero_track_tap_is_empty_and_safe() {
    let tap = LevelTap::new(0);
    assert_eq!(tap.tracks(), 0);
    tap.record(0, &[1.0; 4]);
    assert!(tap.take().is_empty());
}

#[test]
fn concurrent_record_and_take_keep_the_global_max() {
    const THREADS: usize = 4;
    const ITERS: usize = 1000;
    // Amplitudes are unique and increasing in (0, 1); the largest is
    // THREADS*ITERS / (THREADS*ITERS + 1).
    let max_amp = (THREADS * ITERS) as f32 / (THREADS * ITERS + 1) as f32;

    let tap = LevelTap::new(1);
    let done = AtomicUsize::new(0);
    let mut seen_max = SILENCE_DB;

    std::thread::scope(|scope| {
        for t in 0..THREADS {
            let tap = &tap;
            let done = &done;
            scope.spawn(move || {
                for i in 0..ITERS {
                    let amp = (t * ITERS + i + 1) as f32 / (THREADS * ITERS + 1) as f32;
                    tap.record(0, &[amp; 4]);
                }
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        while done.load(Ordering::SeqCst) < THREADS {
            let snap = tap.take();
            seen_max = seen_max.max(snap[0].peak_db);
            std::thread::yield_now();
        }
    });
    // Peaks recorded after the last in-loop take are still in the tap.
    seen_max = seen_max.max(tap.take()[0].peak_db);

    let expected = db(max_amp);
    assert!(
        (seen_max - expected).abs() < 0.01,
        "max seen {seen_max} dB, expected {expected} dB"
    );
}

#[test]
fn mixer_single_source_unity_gain_reaches_meter_at_minus_12_db() {
    let edges = vec![vec![SourceEdge {
        source: 0,
        gain: 1.0,
        gate: None,
        gate_ctl: None,
    }]];
    let mut mixer = Mixer::new(1, edges);
    mixer.push(0, &vec![0.25f32; BLOCK_SAMPLES]);
    let out = mixer.tick();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].len(), BLOCK_SAMPLES);

    let tap = LevelTap::new(1);
    tap.record(0, &out[0]);
    let snap = tap.take();
    assert!(
        (snap[0].peak_db - db(0.25)).abs() < EPS,
        "peak {} expected {}",
        snap[0].peak_db,
        db(0.25)
    );
    assert!(
        (snap[0].peak_db - (-12.04)).abs() < EPS,
        "peak {}",
        snap[0].peak_db
    );
}
