//! Tests for the pure audio mixing helpers in `clipcheck_engine::audiomix`.
//! These run on every platform and touch no devices.

use clipcheck_engine::audiomix::{
    block_level_db, db_to_gain, mix_blocks, mix_blocks_counted, Mixer, NoiseGate, SourceEdge,
    SourceRing, BACKLOG_SAMPLES, BLOCK_SAMPLES,
};
use clipcheck_engine::procs::plan_scan;

fn block(v: f32) -> Vec<f32> {
    vec![v; BLOCK_SAMPLES]
}

fn edge(source: usize, gain: f32, gate: Option<NoiseGate>) -> SourceEdge {
    SourceEdge {
        source,
        gain,
        gate,
        gate_ctl: None,
    }
}

// ---------------------------------------------------------------------------
// db_to_gain
// ---------------------------------------------------------------------------

#[test]
fn db_to_gain_table() {
    assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6, "0 dB");
    assert!((db_to_gain(-20.0) - 0.1).abs() < 1e-6, "-20 dB");
    assert!((db_to_gain(6.0) - 1.995).abs() < 0.005, "+6 dB");
    assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0, "-inf dB");
    assert_eq!(db_to_gain(f32::NAN), 1.0, "NaN dB");
}

// ---------------------------------------------------------------------------
// SourceRing
// ---------------------------------------------------------------------------

#[test]
fn ring_push_pop_round_trip() {
    let mut r = SourceRing::new();
    assert!(r.is_empty());
    r.push(&[0.1, 0.2, 0.3, 0.4]);
    assert_eq!(r.len(), 4, "len after push");
    let out = r.pop_block(4);
    assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4], "round trip order");
    assert!(r.is_empty(), "empty after full pop");
    assert_eq!(r.underflow(), 0, "no underflow on exact pop");
    assert_eq!(r.dropped(), 0, "no drop");
}

#[test]
fn ring_pop_zero_pads_and_counts_underflow() {
    let mut r = SourceRing::new();
    r.push(&[0.5, 0.25]);
    let out = r.pop_block(5);
    assert_eq!(out, vec![0.5, 0.25, 0.0, 0.0, 0.0], "zero padded");
    assert_eq!(r.underflow(), 3, "padding counted");
    assert!(r.is_empty());

    let out = r.pop_block(2);
    assert_eq!(out, vec![0.0, 0.0], "pop from empty is all zeros");
    assert_eq!(r.underflow(), 5, "underflow accumulates");
}

#[test]
fn ring_overflow_drops_oldest_and_keeps_newest() {
    // 300 ms of stereo ramp: 28800 samples, more than BACKLOG_SAMPLES (24000).
    let total = 28_800usize;
    let ramp: Vec<f32> = (0..total).map(|i| i as f32).collect();
    let mut r = SourceRing::new();
    r.push(&ramp);

    let overflow = total - BACKLOG_SAMPLES;
    assert_eq!(BACKLOG_SAMPLES, 24_000, "backlog size");
    assert_eq!(r.len(), BACKLOG_SAMPLES, "len capped at backlog");
    assert_eq!(r.dropped(), overflow as u64, "dropped count");

    let kept = r.pop_block(BACKLOG_SAMPLES);
    assert_eq!(
        kept[0], overflow as f32,
        "oldest retained sample is the newest 24000 start"
    );
    assert_eq!(
        kept[BACKLOG_SAMPLES - 1],
        (total - 1) as f32,
        "newest sample retained"
    );
    assert_eq!(r.underflow(), 0, "no underflow");
}

#[test]
fn ring_repeated_overflow_counts_cumulatively() {
    let mut r = SourceRing::new();
    r.push(&vec![0.0; BACKLOG_SAMPLES]);
    assert_eq!(r.dropped(), 0);
    r.push(&vec![0.0; 100]);
    assert_eq!(r.dropped(), 100, "first overflow");
    r.push(&[0.0; 50]);
    assert_eq!(r.dropped(), 150, "second overflow accumulates");
    assert_eq!(r.len(), BACKLOG_SAMPLES);
}

// ---------------------------------------------------------------------------
// block_level_db
// ---------------------------------------------------------------------------

#[test]
fn block_level_db_values() {
    assert_eq!(block_level_db(&[]), f32::NEG_INFINITY, "empty");
    assert_eq!(block_level_db(&[0.0; 64]), f32::NEG_INFINITY, "all zero");
    let level = block_level_db(&[0.5; 64]);
    assert!(
        (level - (-6.0206)).abs() < 0.01,
        "constant 0.5 is about -6 dB, got {level}"
    );
}

// ---------------------------------------------------------------------------
// mix_blocks
// ---------------------------------------------------------------------------

#[test]
fn mix_sums_with_gains() {
    let a = [0.2f32, 0.4];
    let b = [0.1f32, -0.1];
    let out = mix_blocks(&[&a, &b], &[1.0, 0.5]);
    assert!((out[0] - 0.25).abs() < 1e-6, "sample 0: {}", out[0]);
    assert!((out[1] - 0.35).abs() < 1e-6, "sample 1: {}", out[1]);
}

#[test]
fn mix_clamps_and_counts_clipped_samples() {
    let (out, clipped) = mix_blocks_counted(&[&[1.0f32], &[1.0f32]], &[1.0, 1.0]);
    assert_eq!(out, vec![1.0], "sum clamped to 1.0");
    assert_eq!(clipped, 1, "one clipped sample");

    let (out, clipped) = mix_blocks_counted(&[&[-1.0f32, -2.0]], &[1.0]);
    assert_eq!(out, vec![-1.0, -1.0], "negative clamp");
    assert_eq!(clipped, 1, "only the overflowing sample is counted");

    let (_, clipped) = mix_blocks_counted(&[&[0.5f32, -0.5]], &[1.0]);
    assert_eq!(clipped, 0, "in range is not clipped");
}

#[test]
fn mix_unequal_lengths_uses_longest_and_zero_past_end() {
    let a = [0.1f32, 0.2, 0.3];
    let b = [0.5f32];
    let out = mix_blocks(&[&a, &b], &[1.0, 1.0]);
    assert_eq!(out.len(), 3, "output length is the longest input");
    assert!((out[0] - 0.6).abs() < 1e-6);
    assert!(
        (out[1] - 0.2).abs() < 1e-6,
        "shorter input counts as zero past its end"
    );
    assert!((out[2] - 0.3).abs() < 1e-6);
}

#[test]
fn mix_nan_becomes_zero_and_is_counted() {
    let a = [f32::NAN, 0.5f32];
    let (out, clipped) = mix_blocks_counted(&[&a], &[1.0]);
    assert_eq!(out[0], 0.0, "NaN sample -> 0");
    assert_eq!(out[1], 0.5, "normal sample kept");
    assert_eq!(clipped, 1, "NaN counted");
}

#[test]
fn mix_empty_input_gives_empty_output() {
    let out = mix_blocks(&[], &[]);
    assert!(out.is_empty());
}

// ---------------------------------------------------------------------------
// NoiseGate
// ---------------------------------------------------------------------------

#[test]
fn gate_starts_closed_and_opens_on_loud_block() {
    let mut gate = NoiseGate::new(-30.0);
    assert_eq!(gate.threshold_db(), -30.0);
    assert_eq!(gate.gain(), 0.0, "starts closed");

    let mut loud = block(0.5);
    let input = loud.clone();
    gate.process(&mut loud);
    assert_eq!(gate.gain(), 1.0, "loud block opens instantly");
    assert_eq!(loud, input, "gate output on a loud block equals input");
}

#[test]
fn gate_hold_then_release_curve() {
    let mut gate = NoiseGate::new(-30.0);
    let mut loud = block(0.5);
    gate.process(&mut loud);

    let mut gains = Vec::new();
    for _ in 0..13 {
        let mut quiet = block(0.0);
        gate.process(&mut quiet);
        gains.push(gate.gain());
    }
    let expected = [
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, // 8 hold blocks (150 ms)
        0.8, 0.6, 0.4, 0.2, 0.0, // release over 100 ms
    ];
    for (i, (g, e)) in gains.iter().zip(expected.iter()).enumerate() {
        assert!(
            (g - e).abs() < 1e-5,
            "quiet block {i}: gain {g}, expected {e}"
        );
    }
    assert_eq!(gains.len(), expected.len());
}

#[test]
fn gate_quiet_but_nonzero_counts_as_quiet() {
    // 0.001 constant is about -60 dB, below the -30 dB threshold.
    let mut gate = NoiseGate::new(-30.0);
    let mut loud = block(0.5);
    gate.process(&mut loud);
    // Burn the 8 hold blocks with quiet input, then one more.
    for _ in 0..9 {
        let mut quiet = block(0.001);
        gate.process(&mut quiet);
    }
    assert!(
        (gate.gain() - 0.8).abs() < 1e-5,
        "release started: gain {}",
        gate.gain()
    );
}

#[test]
fn gate_closed_outputs_zeros() {
    let mut gate = NoiseGate::new(-30.0);
    let mut quiet = block(0.001);
    gate.process(&mut quiet);
    assert_eq!(gate.gain(), 0.0, "closed gate");
    assert!(
        quiet.iter().all(|&s| s == 0.0),
        "closed gate output is zero"
    );
}

// ---------------------------------------------------------------------------
// Mixer
// ---------------------------------------------------------------------------

#[test]
fn mixer_one_source_two_tracks_with_different_gains() {
    let mut m = Mixer::new(1, vec![vec![edge(0, 1.0, None)], vec![edge(0, 0.1, None)]]);
    m.push(0, &block(0.5));
    let out = m.tick();
    assert_eq!(out.len(), 2, "one block per track");
    assert!(
        out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6),
        "track0 ~0.5"
    );
    assert!(
        out[1].iter().all(|&s| (s - 0.05).abs() < 1e-6),
        "track1 ~0.05"
    );
    assert_eq!(out[0].len(), BLOCK_SAMPLES);
    assert_eq!(out[1].len(), BLOCK_SAMPLES);
}

#[test]
fn mixer_empty_track_is_all_zero_block() {
    let mut m = Mixer::new(1, vec![vec![], vec![edge(0, 1.0, None)]]);
    m.push(0, &block(0.3));
    let out = m.tick();
    assert_eq!(
        out[0].len(),
        BLOCK_SAMPLES,
        "empty track still gives a block"
    );
    assert!(out[0].iter().all(|&s| s == 0.0), "empty track is silent");
    assert!(out[1].iter().all(|&s| (s - 0.3).abs() < 1e-6));
}

#[test]
fn mixer_tick_always_one_block_per_track_even_without_input() {
    let mut m = Mixer::new(
        2,
        vec![vec![edge(0, 1.0, None)], vec![edge(1, 1.0, None)], vec![]],
    );
    for _ in 0..5 {
        let out = m.tick();
        assert_eq!(out.len(), 3, "track count");
        for (i, b) in out.iter().enumerate() {
            assert_eq!(b.len(), BLOCK_SAMPLES, "track {i} block length");
        }
    }
}

#[test]
fn mixer_tick_pops_one_block_from_every_ring() {
    let mut m = Mixer::new(2, vec![vec![edge(0, 1.0, None)]]);
    m.push(0, &block(0.1));
    m.push(1, &block(0.1));
    m.tick();
    assert!(m.ring(0).unwrap().is_empty(), "used ring drained");
    assert!(m.ring(1).unwrap().is_empty(), "unused ring also drained");
    assert!(m.ring(2).is_none(), "unknown ring");
}

#[test]
fn mixer_unknown_source_push_is_ignored() {
    let mut m = Mixer::new(1, vec![vec![edge(0, 1.0, None)]]);
    m.push(7, &block(0.5));
    let out = m.tick();
    assert!(out[0].iter().all(|&s| s == 0.0), "unknown source ignored");
}

#[test]
fn mixer_clipped_counts_overflow() {
    let mut m = Mixer::new(1, vec![vec![edge(0, 1.0, None)]]);
    m.push(0, &block(1.5));
    assert_eq!(m.clipped(), 0, "no clipping before tick");
    m.tick();
    assert_eq!(
        m.clipped(),
        BLOCK_SAMPLES as u64,
        "every overflowing sample counted"
    );
    m.push(0, &block(0.5));
    m.tick();
    assert_eq!(
        m.clipped(),
        BLOCK_SAMPLES as u64,
        "in-range block adds nothing"
    );
}

#[test]
fn mixer_gate_applies_only_to_its_edge() {
    // Source 0 at 0.001 (about -60 dB): gated edge closed, ungated edge passes it.
    let mut m = Mixer::new(
        1,
        vec![
            vec![edge(0, 1.0, Some(NoiseGate::new(-30.0)))],
            vec![edge(0, 1.0, None)],
        ],
    );
    m.push(0, &block(0.001));
    let out = m.tick();
    assert!(out[0].iter().all(|&s| s == 0.0), "gated edge silent");
    assert!(
        out[1].iter().all(|&s| (s - 0.001).abs() < 1e-7),
        "ungated edge passes"
    );
}

#[test]
fn mixer_gate_on_shared_source_is_per_edge() {
    // Two edges of one track: one gated (closed), one not. Only the open one sums.
    let mut m = Mixer::new(
        2,
        vec![vec![
            edge(0, 1.0, Some(NoiseGate::new(-30.0))),
            edge(1, 1.0, None),
        ]],
    );
    m.push(0, &block(0.001));
    m.push(1, &block(0.2));
    let out = m.tick();
    assert!(
        out[0].iter().all(|&s| (s - 0.2).abs() < 1e-6),
        "gated source removed, other kept"
    );
}

// ---------------------------------------------------------------------------
// plan_scan (pure)
// ---------------------------------------------------------------------------

#[test]
fn plan_scan_starts_new_and_stops_gone() {
    let (start, stop) = plan_scan(&[1, 2], &[2, 3]);
    assert_eq!(start, vec![3], "to_start");
    assert_eq!(stop, vec![1], "to_stop");
}

#[test]
fn plan_scan_no_change() {
    let (start, stop) = plan_scan(&[4, 5], &[5, 4]);
    assert!(start.is_empty(), "nothing to start");
    assert!(stop.is_empty(), "nothing to stop");
}

#[test]
fn plan_scan_from_empty_and_to_empty() {
    let (start, stop) = plan_scan(&[], &[7, 8]);
    assert_eq!(start, vec![7, 8]);
    assert!(stop.is_empty());

    let (start, stop) = plan_scan(&[7, 8], &[]);
    assert!(start.is_empty());
    assert_eq!(stop, vec![7, 8]);
}
