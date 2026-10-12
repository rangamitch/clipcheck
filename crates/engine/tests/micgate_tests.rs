//! Tests for the push-to-talk / mute gate `clipcheck_engine::audiomix::MicGate` and its
//! effect on a `Mixer` edge. Public API only; no devices.

use std::sync::Arc;

use clipcheck_engine::audiomix::{MicGate, Mixer, SourceEdge, BLOCK_SAMPLES};

fn edge_with(gate: Option<Arc<MicGate>>) -> SourceEdge {
    SourceEdge {
        source: 0,
        gain: 1.0,
        gate: None,
        gate_ctl: gate,
    }
}

fn full_block(v: f32) -> Vec<f32> {
    vec![v; BLOCK_SAMPLES]
}

// ---------------------------------------------------------------------------
// defaults
// ---------------------------------------------------------------------------

#[test]
fn new_gate_starts_in_toggle_mode_unmuted() {
    let g = MicGate::new();
    assert!(!g.is_ptt_mode(), "toggle mode by default");
    assert!(!g.is_held(), "nothing held");
    assert!(!g.is_muted(), "unmuted");
    assert!(g.wants_open(), "open by default");
    assert!(!g.is_silenced(), "not silenced by default");
    assert_eq!(g.tail_blocks(), 0, "no tail outside PTT mode");
}

#[test]
fn default_tail_in_ptt_mode_is_250ms_as_13_blocks() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    // 250 ms at 20 ms per block, rounded up.
    assert_eq!(g.tail_blocks(), 13, "250 ms -> 13 blocks");
}

// ---------------------------------------------------------------------------
// tail conversion
// ---------------------------------------------------------------------------

#[test]
fn tail_blocks_rounds_up_and_is_zero_outside_ptt() {
    let g = MicGate::new();
    g.set_ptt_mode(true);

    g.set_tail_ms(0);
    assert_eq!(g.tail_blocks(), 0, "0 ms");
    g.set_tail_ms(20);
    assert_eq!(g.tail_blocks(), 1, "20 ms exactly one block");
    g.set_tail_ms(21);
    assert_eq!(g.tail_blocks(), 2, "21 ms rounds up to two");
    g.set_tail_ms(2000);
    assert_eq!(g.tail_blocks(), 100, "2000 ms");

    g.set_ptt_mode(false);
    assert_eq!(g.tail_blocks(), 0, "no tail in toggle mode");
}

// ---------------------------------------------------------------------------
// press / release counter
// ---------------------------------------------------------------------------

#[test]
fn two_presses_stay_held_until_both_released() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    g.press();
    g.press();
    assert!(g.is_held(), "held after two presses");
    assert!(g.wants_open(), "open while held");

    g.release();
    assert!(g.is_held(), "still held with one key down");
    assert!(g.wants_open(), "still open with one key down");

    g.release();
    assert!(!g.is_held(), "released after both keys up");
    assert!(!g.wants_open(), "closed once both released");
}

#[test]
fn extra_releases_do_not_underflow() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    g.release();
    g.release();
    g.release();
    assert!(!g.is_held(), "extra releases leave nothing held");

    // A later press must count from zero: one press -> held, one release -> not held.
    g.press();
    assert!(g.is_held(), "one press after underflow attempts is held");
    g.release();
    assert!(!g.is_held(), "one release clears it, no wrap-around");
}

#[test]
fn release_all_clears_every_held_key() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    g.press();
    g.press();
    g.press();
    g.release_all();
    assert!(!g.is_held(), "release_all clears");
    assert!(!g.wants_open(), "closed after release_all");
    g.release();
    assert!(!g.is_held(), "release after release_all stays at zero");
}

#[test]
fn set_ptt_mode_clears_held() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    g.press();
    assert!(g.is_held());
    g.set_ptt_mode(true);
    assert!(!g.is_held(), "set_ptt_mode clears held keys");
}

// ---------------------------------------------------------------------------
// mute and toggle
// ---------------------------------------------------------------------------

#[test]
fn toggle_muted_returns_new_state() {
    let g = MicGate::new();
    assert!(g.toggle_muted(), "unmuted -> muted returns true");
    assert!(g.is_muted());
    assert!(!g.toggle_muted(), "muted -> unmuted returns false");
    assert!(!g.is_muted());
}

#[test]
fn set_muted_round_trip() {
    let g = MicGate::new();
    g.set_muted(true);
    assert!(g.is_muted());
    assert!(!g.wants_open(), "muted closes the gate");
    g.set_muted(false);
    assert!(!g.is_muted());
    assert!(g.wants_open(), "unmuted reopens in toggle mode");
}

// ---------------------------------------------------------------------------
// truth table: wants_open == !muted && (!ptt || held); is_silenced == !wants_open
// ---------------------------------------------------------------------------

#[test]
fn wants_open_truth_table() {
    for &muted in &[false, true] {
        for &ptt in &[false, true] {
            for &held in &[false, true] {
                let g = MicGate::new();
                g.set_ptt_mode(ptt);
                if held {
                    g.press();
                }
                g.set_muted(muted);
                let expected = !muted && (!ptt || held);
                assert_eq!(
                    g.wants_open(),
                    expected,
                    "muted={muted} ptt={ptt} held={held}"
                );
                assert_eq!(
                    g.is_silenced(),
                    !expected,
                    "is_silenced muted={muted} ptt={ptt} held={held}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// mute_only_view
// ---------------------------------------------------------------------------

#[test]
fn mute_only_view_ignores_ptt_when_nothing_held() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let view = g.mute_only_view();
    assert!(!g.wants_open(), "original is closed in PTT with no keys");
    assert!(view.wants_open(), "view is open ignoring PTT");
    assert_eq!(view.tail_blocks(), 0, "view has no release tail");
}

#[test]
fn mute_only_view_follows_mute_of_original() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let view = g.mute_only_view();
    assert!(view.wants_open());
    g.set_muted(true);
    assert!(!view.wants_open(), "view closes when original is muted");
    assert!(view.is_muted(), "view sees the shared mute flag");
}

#[test]
fn toggle_on_original_is_visible_through_view() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let view = g.mute_only_view();
    let now_muted = g.toggle_muted();
    assert!(now_muted, "toggle mutes");
    assert!(view.is_muted(), "view sees the toggle");
    assert!(!view.wants_open(), "view closed after toggle mute");
    assert!(!g.toggle_muted(), "second toggle unmutes");
    assert!(view.wants_open(), "view open again after unmute");
}

#[test]
fn view_is_unaffected_by_press_and_release() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let view = g.mute_only_view();
    g.press();
    assert!(view.wants_open());
    g.release();
    assert!(view.wants_open(), "view still open after release");
}

// ---------------------------------------------------------------------------
// mixer integration
// ---------------------------------------------------------------------------

/// Pushes a full block of `v` and ticks, returning track 0's block.
fn push_and_tick(m: &mut Mixer, v: f32) -> Vec<f32> {
    m.push(0, &full_block(v));
    let mut out = m.tick();
    assert_eq!(out.len(), 1, "one track");
    out.remove(0)
}

fn all_close(block: &[f32], want: f32) -> bool {
    block.iter().all(|s| (s - want).abs() < 1e-6)
}

#[test]
fn ptt_gate_silences_source_when_nothing_held() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let mut m = Mixer::new(1, vec![vec![edge_with(Some(g.clone()))]]);

    // First tick ramps the gain down from 1.0 to 0.0 across the block.
    let _ramp = push_and_tick(&mut m, 0.5);
    // Once the ramp has settled, the block is silent.
    let silent_1 = push_and_tick(&mut m, 0.5);
    let silent_2 = push_and_tick(&mut m, 0.5);
    assert!(all_close(&silent_1, 0.0), "silent after ramp");
    assert!(all_close(&silent_2, 0.0), "still silent");
}

#[test]
fn ptt_gate_passes_source_after_press() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let mut m = Mixer::new(1, vec![vec![edge_with(Some(g.clone()))]]);

    let _ = push_and_tick(&mut m, 0.5);
    let _ = push_and_tick(&mut m, 0.5);

    g.press();
    // Give it two ticks to settle: the first ramps up, the second is steady.
    let _ramp = push_and_tick(&mut m, 0.5);
    let open_1 = push_and_tick(&mut m, 0.5);
    let open_2 = push_and_tick(&mut m, 0.5);
    assert!(all_close(&open_1, 0.5), "full 0.5 after press");
    assert!(all_close(&open_2, 0.5), "stays 0.5 while held");
}

#[test]
fn ptt_gate_closes_again_after_release_tail() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    g.set_tail_ms(20); // one block of tail, to keep the test short
    let mut m = Mixer::new(1, vec![vec![edge_with(Some(g.clone()))]]);

    g.press();
    let _ = push_and_tick(&mut m, 0.5);
    let _ = push_and_tick(&mut m, 0.5);
    g.release();
    // First block after release is still inside the one-block tail, so it stays open.
    let tail = push_and_tick(&mut m, 0.5);
    assert!(all_close(&tail, 0.5), "tail block still open");
    // Tail spent: the block after that is closed.
    let _ramp = push_and_tick(&mut m, 0.5);
    let closed = push_and_tick(&mut m, 0.5);
    assert!(all_close(&closed, 0.0), "closed after tail");
}

#[test]
fn mute_only_view_edge_stays_open_while_ptt_not_held() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let view = g.mute_only_view();
    let mut m = Mixer::new(1, vec![vec![edge_with(Some(view.clone()))]]);

    for _ in 0..4 {
        let block = push_and_tick(&mut m, 0.5);
        assert!(all_close(&block, 0.5), "view edge stays 0.5 with PTT up");
    }
}

#[test]
fn mute_only_view_edge_is_cut_by_mute() {
    let g = MicGate::new();
    g.set_ptt_mode(true);
    let view = g.mute_only_view();
    let mut m = Mixer::new(1, vec![vec![edge_with(Some(view))]]);

    let _ = push_and_tick(&mut m, 0.5);
    g.set_muted(true);
    let _ramp = push_and_tick(&mut m, 0.5);
    let muted = push_and_tick(&mut m, 0.5);
    assert!(all_close(&muted, 0.0), "mute silences the view edge");
}
