use clipcheck_buffer::{ClipPlanError, IndexError, RingIndex, SegmentMeta, Window, TICKS_PER_SEC};
use std::path::PathBuf;

const S: i64 = TICKS_PER_SEC;

fn seg(seq: u64, start: i64, end: i64, kf: i64, bytes: u64, committed: bool) -> SegmentMeta {
    SegmentMeta {
        seq,
        start_100ns: start,
        end_100ns: end,
        first_keyframe_100ns: kf,
        bytes,
        path: PathBuf::from(format!("seg{seq}.ts")),
        committed,
    }
}

/// Committed 2-second segments, seq i covering [2i s, 2(i+1) s), with the
/// given first-keyframe times, 100 bytes each.
fn ring(kfs: &[i64]) -> RingIndex {
    let mut r = RingIndex::new();
    for (i, kf) in kfs.iter().enumerate() {
        let i = i as i64;
        r.push(seg(i as u64, 2 * S * i, 2 * S * (i + 1), *kf, 100, true))
            .unwrap();
    }
    r
}

/// Five 2 s segments, keyframe at each segment start (0, 2, 4, 6, 8 s).
fn std_ring() -> RingIndex {
    ring(&[0, 2 * S, 4 * S, 6 * S, 8 * S])
}

fn seqs(v: &[SegmentMeta]) -> Vec<u64> {
    v.iter().map(|s| s.seq).collect()
}

// ---------- plan_clip: errors ----------

#[test]
fn plan_empty_index_is_empty_error() {
    let r = RingIndex::new();
    assert_eq!(r.plan_clip(10 * S, 5, 0), Err(ClipPlanError::Empty));
}

#[test]
fn plan_zero_seconds_is_invalid() {
    assert_eq!(
        std_ring().plan_clip(10 * S, 0, 0),
        Err(ClipPlanError::InvalidDuration)
    );
    assert_eq!(
        RingIndex::new().plan_clip(10 * S, 0, 0),
        Err(ClipPlanError::InvalidDuration)
    );
}

#[test]
fn plan_negative_pad_is_invalid() {
    assert_eq!(
        std_ring().plan_clip(10 * S, 5, -1),
        Err(ClipPlanError::InvalidDuration)
    );
}

#[test]
fn plan_huge_seconds_is_invalid_not_panic() {
    assert_eq!(
        std_ring().plan_clip(10 * S, u64::MAX, 0),
        Err(ClipPlanError::InvalidDuration)
    );
    assert_eq!(
        std_ring().plan_clip(10 * S, (i64::MAX / S + 1) as u64, 0),
        Err(ClipPlanError::InvalidDuration)
    );
}

// ---------- plan_clip: keyframe snapping ----------

#[test]
fn plan_target_exactly_on_keyframe() {
    // now 10 s, want 4 s -> target 6 s == keyframe of seq 3.
    let p = std_ring().plan_clip(10 * S, 4, 0).unwrap();
    assert_eq!(p.trim_start_offset_100ns, 0);
    assert_eq!(seqs(&p.segments), vec![3, 4]);
    assert_eq!(p.segments[0].first_keyframe_100ns, 6 * S);
    assert!((p.effective_secs - 4.0).abs() < 1e-9);
    assert!(!p.short);
    assert!(!p.has_gap);
}

#[test]
fn plan_target_between_keyframes_snaps_back() {
    // target 5 s lies between kf 4 s (seq 2) and kf 6 s (seq 3).
    let p = std_ring().plan_clip(10 * S, 5, 0).unwrap();
    assert_eq!(seqs(&p.segments), vec![2, 3, 4]);
    assert_eq!(p.segments[0].first_keyframe_100ns, 4 * S);
    assert_eq!(p.trim_start_offset_100ns, S);
    assert!((p.effective_secs - 5.0).abs() < 1e-9);
    assert!(!p.short);
}

#[test]
fn plan_target_before_all_keyframes_uses_earliest() {
    // kfs at 1 s, 3 s, 5 s; now 6 s; want 5 s, pad 0.5 s -> target 0.5 s,
    // before every keyframe. Buffer (from 1 s) is 5 s, exactly the request.
    let r = ring(&[S, 3 * S, 5 * S]);
    let p = r.plan_clip(6 * S, 5, S / 2).unwrap();
    assert_eq!(p.trim_start_offset_100ns, 0);
    assert_eq!(p.segments[0].first_keyframe_100ns, S);
    assert_eq!(seqs(&p.segments), vec![0, 1, 2]);
    assert!((p.effective_secs - 5.0).abs() < 1e-9);
    assert!(!p.short);
    assert!(!p.has_gap);
}

#[test]
fn plan_short_buffer_flags_short_and_effective_from_earliest_kf() {
    // Only 4 s of data (kf 0), request 10 s.
    let r = ring(&[0, 2 * S]);
    let p = r.plan_clip(4 * S, 10, 0).unwrap();
    assert!(p.short);
    assert_eq!(p.trim_start_offset_100ns, 0);
    assert_eq!(p.segments[0].first_keyframe_100ns, 0);
    assert!((p.effective_secs - 4.0).abs() < 1e-9);
}

#[test]
fn plan_short_when_earliest_kf_is_late() {
    // Data starts at kf 3 s; now 6 s; request 5 s -> only 3 s available.
    let r = ring(&[3 * S, 5 * S]);
    let p = r.plan_clip(6 * S, 5, 0).unwrap();
    assert!(p.short);
    assert!((p.effective_secs - 3.0).abs() < 1e-9);
}

#[test]
fn plan_pad_moves_target_earlier() {
    // Without pad target = 6 s (origin seq 3, trim 0).
    // With 1.5 s pad target = 4.5 s (origin seq 2, trim 0.5 s).
    let r = std_ring();
    let no_pad = r.plan_clip(10 * S, 4, 0).unwrap();
    assert_eq!(no_pad.segments[0].first_keyframe_100ns, 6 * S);
    let padded = r.plan_clip(10 * S, 4, 3 * S / 2).unwrap();
    assert_eq!(padded.segments[0].first_keyframe_100ns, 4 * S);
    assert_eq!(padded.trim_start_offset_100ns, S / 2);
    assert_eq!(seqs(&padded.segments), vec![2, 3, 4]);
    assert!((padded.effective_secs - 5.5).abs() < 1e-9);
}

// ---------- plan_clip: gaps ----------

#[test]
fn gap_clamps_clip_and_excludes_earlier_segments() {
    let mut r = std_ring();
    r.mark_gap(5 * S);
    // want 8 s from now 10 s -> raw target 2 s < gap 5 s.
    let p = r.plan_clip(10 * S, 8, 0).unwrap();
    assert!(p.has_gap);
    assert!(p.segments.iter().all(|s| s.start_100ns >= 5 * S));
    assert_eq!(seqs(&p.segments), vec![3, 4]);
    assert_eq!(p.trim_start_offset_100ns, 0);
    assert!(p.short);
}

#[test]
fn gap_not_reached_has_gap_false() {
    let mut r = std_ring();
    r.mark_gap(S);
    let p = r.plan_clip(10 * S, 2, 0).unwrap();
    assert!(!p.has_gap);
}

#[test]
fn gap_after_all_data_is_empty() {
    let mut r = std_ring();
    r.mark_gap(20 * S);
    assert_eq!(r.plan_clip(10 * S, 5, 0), Err(ClipPlanError::Empty));
}

#[test]
fn gap_latest_wins() {
    let mut r = RingIndex::new();
    assert_eq!(r.gap_100ns(), None);
    r.mark_gap(3 * S);
    assert_eq!(r.gap_100ns(), Some(3 * S));
    r.mark_gap(5 * S);
    assert_eq!(r.gap_100ns(), Some(5 * S));
    r.mark_gap(4 * S);
    assert_eq!(r.gap_100ns(), Some(5 * S));
}

// ---------- open segment ----------

#[test]
fn open_segment_included_in_plan() {
    let mut r = ring(&[0, 2 * S]);
    r.push(seg(2, 4 * S, 4 * S, 4 * S, 50, false)).unwrap();
    // now 5 s, want 1 s -> target 4 s, origin = open seg's kf.
    let p = r.plan_clip(5 * S, 1, 0).unwrap();
    assert_eq!(seqs(&p.segments), vec![2]);
    assert!(!p.segments[0].committed);
    assert_eq!(p.trim_start_offset_100ns, 0);
}

#[test]
fn open_segment_never_evicted() {
    let mut r = ring(&[0, 2 * S]);
    r.push(seg(2, 4 * S, 4 * S, 4 * S, 50, false)).unwrap();
    let ev = r.evict_to(0, 0);
    assert_eq!(seqs(&ev), vec![0, 1]);
    assert_eq!(r.len(), 1);
    assert_eq!(r.iter().next().unwrap().seq, 2);
    assert_eq!(r.total_bytes(), 50);
    // Still nothing more to evict.
    assert!(r.evict_to(0, 0).is_empty());
}

// ---------- push / commit errors ----------

#[test]
fn push_after_open_segment_fails() {
    let mut r = RingIndex::new();
    r.push(seg(0, 0, 0, 0, 10, false)).unwrap();
    assert_eq!(
        r.push(seg(1, 0, 0, 0, 10, false)),
        Err(IndexError::OpenSegmentPending(0))
    );
    assert_eq!(r.len(), 1);
    assert_eq!(r.total_bytes(), 10);
}

#[test]
fn push_equal_or_lower_seq_is_out_of_order() {
    let mut r = RingIndex::new();
    r.push(seg(5, 0, S, 0, 10, true)).unwrap();
    assert_eq!(
        r.push(seg(5, S, 2 * S, S, 10, true)),
        Err(IndexError::OutOfOrder(5))
    );
    assert_eq!(
        r.push(seg(3, S, 2 * S, S, 10, true)),
        Err(IndexError::OutOfOrder(3))
    );
    assert_eq!(r.len(), 1);
    assert_eq!(r.total_bytes(), 10);
    r.push(seg(6, S, 2 * S, S, 10, true)).unwrap();
    assert_eq!(r.len(), 2);
}

#[test]
fn commit_unknown_seq() {
    let mut r = RingIndex::new();
    assert_eq!(r.commit(0, S, 10), Err(IndexError::UnknownSeq(0)));
    r.push(seg(0, 0, 0, 0, 10, false)).unwrap();
    assert_eq!(r.commit(7, S, 10), Err(IndexError::UnknownSeq(7)));
}

#[test]
fn commit_twice_is_already_committed() {
    let mut r = RingIndex::new();
    r.push(seg(0, 0, 0, 0, 10, false)).unwrap();
    r.commit(0, 2 * S, 30).unwrap();
    assert_eq!(r.commit(0, 3 * S, 40), Err(IndexError::AlreadyCommitted(0)));
    let s = r.iter().next().unwrap();
    assert_eq!(s.end_100ns, 2 * S);
    assert_eq!(s.bytes, 30);
    assert!(s.committed);
}

// ---------- total_bytes / commit bookkeeping ----------

#[test]
fn total_bytes_tracks_push_commit_evict() {
    let mut r = RingIndex::new();
    r.push(seg(0, 0, 0, 0, 10, false)).unwrap();
    assert_eq!(r.total_bytes(), 10);
    r.commit(0, 2 * S, 30).unwrap();
    assert_eq!(r.total_bytes(), 30);
    r.push(seg(1, 2 * S, 2 * S, 2 * S, 20, false)).unwrap();
    assert_eq!(r.total_bytes(), 50);
    r.commit(1, 4 * S, 25).unwrap();
    assert_eq!(r.total_bytes(), 55);
    let ev = r.evict_to(i64::MAX, 30);
    assert_eq!(seqs(&ev), vec![0]);
    assert_eq!(r.total_bytes(), 25);
    let ev = r.evict_to(i64::MAX, 0);
    assert_eq!(seqs(&ev), vec![1]);
    assert_eq!(r.total_bytes(), 0);
    assert!(r.is_empty());
}

#[test]
fn evicted_segments_keep_path() {
    let mut r = std_ring();
    let ev = r.evict_to(i64::MAX, 0);
    assert_eq!(ev.len(), 5);
    for (i, s) in ev.iter().enumerate() {
        assert_eq!(s.path, PathBuf::from(format!("seg{i}.ts")));
    }
}

// ---------- evict_to ----------

#[test]
fn evict_by_bytes_only() {
    let mut r = std_ring(); // 5 x 100 bytes
    let ev = r.evict_to(i64::MAX, 250);
    assert_eq!(seqs(&ev), vec![0, 1, 2]);
    assert_eq!(r.len(), 2);
    assert_eq!(r.total_bytes(), 200);
}

#[test]
fn evict_by_duration_keeps_one_segment_slack() {
    // Span 0..10 s. max 6 s + oldest 2 s slack = 8 s allowed.
    let mut r = std_ring();
    let ev = r.evict_to(6 * S, u64::MAX);
    assert_eq!(seqs(&ev), vec![0]);
    assert_eq!(r.len(), 4);
    let first = r.iter().next().unwrap();
    let last = r.iter().last().unwrap();
    assert_eq!(last.end_100ns - first.start_100ns, 8 * S);
    // Stable on repeat.
    assert!(r.evict_to(6 * S, u64::MAX).is_empty());
}

#[test]
fn evict_no_limits_evicts_nothing() {
    let mut r = std_ring();
    assert!(r.evict_to(i64::MAX, u64::MAX).is_empty());
    assert_eq!(r.len(), 5);
}

#[test]
fn leased_oldest_blocks_eviction() {
    let mut r = std_ring();
    let lease = r.lease(Window {
        start_100ns: 0,
        end_100ns: S,
    });
    assert_eq!(lease.seqs(), &[0]);
    assert!(r.evict_to(i64::MAX, 0).is_empty());
    assert_eq!(r.len(), 5);
    r.release(lease);
    assert_eq!(seqs(&r.evict_to(i64::MAX, 0)), vec![0, 1, 2, 3, 4]);
}

#[test]
fn leased_middle_stops_eviction_at_it() {
    let mut r = std_ring();
    // Window [5 s, 5.5 s) overlaps only seq 2 ([4 s, 6 s)).
    let lease = r.lease(Window {
        start_100ns: 5 * S,
        end_100ns: 11 * S / 2,
    });
    assert_eq!(lease.seqs(), &[2]);
    let ev = r.evict_to(i64::MAX, 0);
    assert_eq!(seqs(&ev), vec![0, 1]);
    assert_eq!(r.len(), 3);
    r.release(lease);
    assert_eq!(seqs(&r.evict_to(i64::MAX, 0)), vec![2, 3, 4]);
}

// ---------- lease / release ----------

#[test]
fn lease_window_boundaries_are_half_open() {
    let mut r = std_ring();
    // [4 s, 6 s) overlaps seq 2 only: seq 1 ends at 4 s (not > 4 s),
    // seq 3 starts at 6 s (not < 6 s).
    let lease = r.lease(Window {
        start_100ns: 4 * S,
        end_100ns: 6 * S,
    });
    assert_eq!(lease.seqs(), &[2]);
    r.release(lease);
}

#[test]
fn lease_no_overlap_takes_nothing() {
    let mut r = std_ring();
    let lease = r.lease(Window {
        start_100ns: 100 * S,
        end_100ns: 200 * S,
    });
    assert!(lease.seqs().is_empty());
    r.release(lease);
    assert_eq!(seqs(&r.evict_to(i64::MAX, 0)), vec![0, 1, 2, 3, 4]);
}

#[test]
fn double_lease_needs_both_released() {
    let mut r = std_ring();
    let w = Window {
        start_100ns: 0,
        end_100ns: S,
    };
    let a = r.lease(w);
    let b = r.lease(w);
    r.release(a);
    assert!(
        r.evict_to(i64::MAX, 0).is_empty(),
        "one lease still outstanding"
    );
    r.release(b);
    assert_eq!(seqs(&r.evict_to(i64::MAX, 0)), vec![0, 1, 2, 3, 4]);
}

#[test]
fn lease_release_restores_evictability() {
    let mut r = std_ring();
    let lease = r.lease(Window {
        start_100ns: 0,
        end_100ns: 3 * S,
    });
    assert_eq!(lease.seqs(), &[0, 1]);
    assert!(r.evict_to(i64::MAX, 0).is_empty());
    r.release(lease);
    assert_eq!(seqs(&r.evict_to(i64::MAX, 0)), vec![0, 1, 2, 3, 4]);
    assert!(r.is_empty());
    assert_eq!(r.total_bytes(), 0);
}

// ---------- push_committed ----------

/// Five 2 s segments built with `push_committed`, keyframe at each segment
/// start, 100 bytes each. Same shape as `std_ring`.
fn std_ring_committed() -> RingIndex {
    let mut r = RingIndex::new();
    for i in 0..5i64 {
        r.push_committed(
            seg(i as u64, 2 * S * i, 0, 2 * S * i, 0, false),
            2 * S * (i + 1),
            100,
        )
        .unwrap();
    }
    r
}

#[test]
fn push_committed_stores_committed_segment() {
    let mut r = RingIndex::new();
    // The meta passed in has stale end/bytes/committed values; they must be
    // replaced by the arguments.
    r.push_committed(seg(0, 0, 0, 0, 999, false), 2 * S, 30)
        .unwrap();
    assert_eq!(r.len(), 1);
    let s = r.iter().next().unwrap();
    assert!(s.committed);
    assert_eq!(s.start_100ns, 0);
    assert_eq!(s.end_100ns, 2 * S);
    assert_eq!(s.bytes, 30);
    assert_eq!(r.total_bytes(), 30);
}

#[test]
fn push_committed_matches_push_then_commit() {
    let mut a = RingIndex::new();
    a.push(seg(0, 0, 0, 0, 10, false)).unwrap();
    a.commit(0, 2 * S, 30).unwrap();
    a.push(seg(1, 2 * S, 4 * S, 2 * S, 20, false)).unwrap();
    a.commit(1, 4 * S, 25).unwrap();

    let mut b = RingIndex::new();
    b.push_committed(seg(0, 0, 0, 0, 10, false), 2 * S, 30)
        .unwrap();
    b.push_committed(seg(1, 2 * S, 4 * S, 2 * S, 20, false), 4 * S, 25)
        .unwrap();

    let sa: Vec<SegmentMeta> = a.iter().cloned().collect();
    let sb: Vec<SegmentMeta> = b.iter().cloned().collect();
    assert_eq!(sa, sb);
    assert_eq!(a.len(), b.len());
    assert_eq!(a.total_bytes(), b.total_bytes());
    assert_eq!(a.total_bytes(), 55);
    assert_eq!(a.timeline_end_100ns(), b.timeline_end_100ns());
    assert_eq!(a.plan_clip(4 * S, 3, 0), b.plan_clip(4 * S, 3, 0));
    assert!(a.plan_clip(4 * S, 3, 0).is_ok());
}

#[test]
fn push_committed_after_open_push_fails() {
    let mut r = RingIndex::new();
    r.push(seg(0, 0, 0, 0, 10, false)).unwrap();
    assert_eq!(
        r.push_committed(seg(1, 2 * S, 4 * S, 2 * S, 20, false), 4 * S, 25),
        Err(IndexError::OpenSegmentPending(0))
    );
    assert_eq!(r.len(), 1);
    assert_eq!(r.total_bytes(), 10);
    assert!(!r.iter().next().unwrap().committed);
}

#[test]
fn push_committed_non_increasing_seq_is_out_of_order() {
    let mut r = RingIndex::new();
    r.push_committed(seg(5, 0, 0, 0, 0, false), S, 10).unwrap();
    assert_eq!(
        r.push_committed(seg(5, S, 0, S, 0, false), 2 * S, 10),
        Err(IndexError::OutOfOrder(5))
    );
    assert_eq!(
        r.push_committed(seg(3, S, 0, S, 0, false), 2 * S, 10),
        Err(IndexError::OutOfOrder(3))
    );
    // Same error as push for the same input.
    let mut p = RingIndex::new();
    p.push(seg(5, 0, S, 0, 10, true)).unwrap();
    assert_eq!(
        p.push(seg(3, S, 2 * S, S, 10, true)),
        Err(IndexError::OutOfOrder(3))
    );
    assert_eq!(r.len(), 1);
    assert_eq!(r.total_bytes(), 10);
}

#[test]
fn push_after_push_committed_succeeds() {
    let mut r = RingIndex::new();
    r.push_committed(seg(0, 0, 0, 0, 0, false), 2 * S, 30)
        .unwrap();
    r.push(seg(1, 2 * S, 2 * S, 2 * S, 20, false)).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r.total_bytes(), 50);
    assert!(!r.last().unwrap().committed);
    r.commit(1, 4 * S, 25).unwrap();
    assert_eq!(r.total_bytes(), 55);
    r.push_committed(seg(2, 4 * S, 0, 4 * S, 0, false), 6 * S, 5)
        .unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r.total_bytes(), 60);
}

#[test]
fn push_committed_ring_evicts_by_duration_and_bytes() {
    // Same expectations as `evict_by_duration_keeps_one_segment_slack` and
    // `evict_by_bytes_only`, on a ring built only with push_committed.
    let mut r = std_ring_committed();
    assert_eq!(r.len(), 5);
    assert_eq!(r.total_bytes(), 500);
    let ev = r.evict_to(6 * S, u64::MAX);
    assert_eq!(seqs(&ev), vec![0]);
    assert_eq!(r.len(), 4);
    assert!(r.iter().all(|s| s.committed));
    let first = r.iter().next().unwrap();
    let last = r.iter().last().unwrap();
    assert_eq!(last.end_100ns - first.start_100ns, 8 * S);

    let mut r = std_ring_committed();
    let ev = r.evict_to(i64::MAX, 250);
    assert_eq!(seqs(&ev), vec![0, 1, 2]);
    assert_eq!(r.len(), 2);
    assert_eq!(r.total_bytes(), 200);
}
