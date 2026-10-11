//! Regression: clips came out one segment (about 2 s) long when the index
//! timeline trailed the wall clock by more than the requested length.

use clipcheck_buffer::index::{RingIndex, SegmentMeta, TICKS_PER_SEC};
use std::path::PathBuf;

const SEG: i64 = 2 * TICKS_PER_SEC;

/// Ring of `n` committed 2 s segments, timeline 0..2n seconds.
fn ring(n: u64) -> RingIndex {
    let mut idx = RingIndex::new();
    for i in 0..n {
        let start = i as i64 * SEG;
        idx.push(SegmentMeta {
            seq: i,
            start_100ns: start,
            end_100ns: start + SEG,
            first_keyframe_100ns: start,
            bytes: 1,
            path: PathBuf::from(format!("seg-{i:06}.mp4")),
            committed: true,
        })
        .unwrap();
    }
    idx
}

#[test]
fn wall_anchor_collapses_clip_when_timeline_lags() {
    // 70 segments = 140 s of footage, but 235 s of wall time passed (owner's case).
    let idx = ring(70);
    let wall = 235 * TICKS_PER_SEC;
    let old = idx.plan_clip(wall, 30, 0).unwrap();
    assert_eq!(old.segments.len(), 1, "documents the bug: one segment");
}

#[test]
fn latest_clip_spans_requested_length_when_timeline_lags() {
    let idx = ring(70);
    let wall = 235 * TICKS_PER_SEC;
    let (plan, anchor) = idx.plan_latest_clip(wall, 30, 0).unwrap();
    assert_eq!(anchor, 140 * TICKS_PER_SEC);
    assert_eq!(plan.segments.len(), 15, "30 s = 15 segments of 2 s");
    assert!(!plan.short);
    assert!(
        (plan.effective_secs - 30.0).abs() < 1e-6,
        "{}",
        plan.effective_secs
    );
}

#[test]
fn latest_clip_ten_seconds_spans_five_segments() {
    let idx = ring(10);
    let (plan, _) = idx.plan_latest_clip(1_000 * TICKS_PER_SEC, 10, 0).unwrap();
    assert_eq!(plan.segments.len(), 5);
    let seqs: Vec<u64> = plan.segments.iter().map(|s| s.seq).collect();
    assert_eq!(seqs, vec![5, 6, 7, 8, 9]);
}

#[test]
fn latest_clip_never_anchors_past_wall_clock() {
    let idx = ring(10); // 20 s of footage
    let (_, anchor) = idx.plan_latest_clip(15 * TICKS_PER_SEC, 5, 0).unwrap();
    assert_eq!(anchor, 15 * TICKS_PER_SEC);
}

#[test]
fn latest_clip_reports_short_when_buffer_is_young() {
    let idx = ring(3); // 6 s
    let (plan, _) = idx.plan_latest_clip(10 * TICKS_PER_SEC, 30, 0).unwrap();
    assert!(plan.short);
    assert_eq!(plan.segments.len(), 3);
}
