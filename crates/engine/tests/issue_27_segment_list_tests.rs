//! Tests for the segment list (`segments.csv`) path of `SegmentTracker` (D52,
//! issue #27), through the public API only.
//!
//! The segment writer appends one `name,start_secs,end_secs` row per closed
//! segment. The tracker reads complete `\n`-terminated rows, takes durations
//! from them without probing, and only probes when a segment is missing from
//! the list after the tracker has been finished. Probe tools here are either
//! bogus paths (any probe fails) or a fake `.cmd`/`sh` script.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clipcheck_buffer::RingIndex;
use clipcheck_engine::tracker::{CompletedSegment, SegmentTracker};
use tempfile::TempDir;

/// 100 ns ticks per second (`clipcheck_buffer::index::TICKS_PER_SEC`).
const TICKS: i64 = 10_000_000;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn seg_name(seq: u64) -> String {
    format!("seg-{seq:06}.mp4")
}

fn seg_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(seg_name(seq))
}

/// Writes segment `seq` as a complete 64-byte file.
fn place_seg(dir: &Path, seq: u64) {
    fs::write(seg_path(dir, seq), [7u8; 64]).expect("write segment");
}

/// Writes a valid 2 s mp4 as segment `seq`, for tests that reach the probe fallback.
fn place_real_seg(dir: &Path, seq: u64) {
    let spec = clipcheck_testkit::MediaSpec::h264_aac(2.0)
        .with_video(160, 120, 15)
        .with_audio(Vec::new());
    clipcheck_testkit::write_mp4(&seg_path(dir, seq), &spec).expect("testkit segment");
}

fn csv_path(dir: &Path) -> PathBuf {
    dir.join("segments.csv")
}

/// Appends raw text to `segments.csv`, creating it when missing.
fn append_csv(dir: &Path, text: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(csv_path(dir))
        .expect("open segments.csv");
    f.write_all(text.as_bytes()).expect("append csv");
}

/// One complete row, `name,start,end` with six decimals, ending in `\n`.
fn csv_row(seq: u64, start_ticks: i64, end_ticks: i64) -> String {
    format!(
        "{},{:.6},{:.6}\n",
        seg_name(seq),
        start_ticks as f64 / TICKS as f64,
        end_ticks as f64 / TICKS as f64
    )
}

/// Ticks of a duration written with six decimals, as the tracker should see it.
fn dur_ticks(secs: f64) -> i64 {
    (secs * TICKS as f64).round() as i64
}

fn new_ring() -> Arc<Mutex<RingIndex>> {
    Arc::new(Mutex::new(RingIndex::new()))
}

fn ring_seqs(ring: &Arc<Mutex<RingIndex>>) -> Vec<u64> {
    ring.lock()
        .expect("ring lock")
        .iter()
        .map(|m| m.seq)
        .collect()
}

fn seqs(segs: &[CompletedSegment]) -> Vec<u64> {
    segs.iter().map(|s| s.seq).collect()
}

/// Polls `poll_nowait` (or blocking `poll`) until `want` segments are gathered
/// or 50 calls have been made.
fn drain(tracker: &mut SegmentTracker, blocking: bool, want: usize) -> Vec<CompletedSegment> {
    let mut out = Vec::new();
    for _ in 0..50 {
        let batch = if blocking {
            tracker.poll()
        } else {
            tracker.poll_nowait()
        }
        .expect("poll");
        out.extend(batch);
        if out.len() >= want {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// T1: ten segments from the list, no probe needed
// ---------------------------------------------------------------------------

#[test]
fn t1_ten_segments_from_list_without_probe() {
    let dir = TempDir::new().expect("tempdir");
    // First 2.033333, middle 2.0, last 1.333333 seconds.
    let durs: [f64; 10] = [2.033333, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 1.333333];

    let mut csv = String::new();
    let mut start: i64 = 0;
    let mut expected: Vec<(i64, i64)> = Vec::new();
    for (i, d) in durs.iter().enumerate() {
        let seq = i as u64;
        let end = start + dur_ticks(*d);
        place_seg(dir.path(), seq);
        csv.push_str(&csv_row(seq, start, end));
        expected.push((start, end));
        start = end;
    }
    append_csv(dir.path(), &csv);

    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));
    tracker.finish();
    let done = drain(&mut tracker, true, 10);

    assert_eq!(seqs(&done), (0..10).collect::<Vec<u64>>(), "seq order");
    assert!(tracker.quarantined().is_empty(), "nothing quarantined");
    for (seg, (s, e)) in done.iter().zip(expected.iter()) {
        assert_eq!(seg.start_100ns, *s, "start of seq {}", seg.seq);
        assert_eq!(seg.end_100ns, *e, "end of seq {}", seg.seq);
        assert_eq!(
            seg.end_100ns - seg.start_100ns,
            dur_ticks(durs[seg.seq as usize])
        );
        assert_eq!(seg.bytes, 64, "file size of seq {}", seg.seq);
    }
    assert_eq!(
        ring_seqs(&ring),
        (0..10).collect::<Vec<u64>>(),
        "ring index"
    );
}

// ---------------------------------------------------------------------------
// T2: live rows, partial row without newline
// ---------------------------------------------------------------------------

#[test]
fn t2_live_rows_and_partial_row_not_consumed() {
    let dir = TempDir::new().expect("tempdir");
    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    // Rows 0..3, each 2 s, contiguous.
    let mut csv = String::new();
    for seq in 0..3u64 {
        place_seg(dir.path(), seq);
        csv.push_str(&csv_row(
            seq,
            seq as i64 * 2 * TICKS,
            (seq as i64 + 1) * 2 * TICKS,
        ));
    }
    append_csv(dir.path(), &csv);

    let mut got: Vec<CompletedSegment> = Vec::new();
    for _ in 0..20 {
        let batch = tracker.poll_nowait().expect("poll_nowait");
        assert!(
            batch.iter().all(|s| s.seq < 3),
            "nothing beyond the rows may appear: {:?}",
            seqs(&batch)
        );
        got.extend(batch);
    }
    assert_eq!(seqs(&got), vec![0, 1, 2], "three rows indexed");

    // Partial row for seq 3: no trailing newline, file present.
    place_seg(dir.path(), 3);
    append_csv(dir.path(), &format!("{},6.000000,8", seg_name(3)));
    for _ in 0..20 {
        let batch = tracker.poll_nowait().expect("poll_nowait");
        assert!(
            batch.is_empty(),
            "partial row must not be consumed: {:?}",
            seqs(&batch)
        );
    }

    // Complete the row with the rest of the number and the newline.
    append_csv(dir.path(), ".000000\n");
    let more = drain(&mut tracker, false, 1);
    assert_eq!(seqs(&more), vec![3], "completed row is consumed");
    assert_eq!(more[0].start_100ns, 6 * TICKS, "starts at previous end");
    assert_eq!(more[0].end_100ns, 8 * TICKS, "end from the row");
    assert!(tracker.quarantined().is_empty());
    assert_eq!(ring_seqs(&ring), vec![0, 1, 2, 3], "ring index");
}

// ---------------------------------------------------------------------------
// T3: file present, row missing, not finished: waits, never probes
// ---------------------------------------------------------------------------

#[test]
fn t3_missing_row_waits_and_never_probes() {
    let dir = TempDir::new().expect("tempdir");
    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    // Row for seg 0 only; seg 1 has its file but no row yet.
    place_seg(dir.path(), 0);
    place_seg(dir.path(), 1);
    append_csv(dir.path(), &csv_row(0, 0, 2 * TICKS));

    let mut got: Vec<CompletedSegment> = Vec::new();
    for _ in 0..20 {
        got.extend(tracker.poll_nowait().expect("poll_nowait"));
    }
    assert_eq!(seqs(&got), vec![0], "only the row-backed segment completes");
    assert!(
        tracker.quarantined().is_empty(),
        "a missing row before finish() must not quarantine: {:?}",
        tracker.quarantined()
    );
    assert_eq!(ring_seqs(&ring), vec![0]);
}

// ---------------------------------------------------------------------------
// T4: missing final row after finish() falls back to probing the file once
// ---------------------------------------------------------------------------

#[test]
fn t4_missing_row_after_finish_uses_probe_fallback() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let dir = TempDir::new().expect("tempdir");
    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    place_seg(dir.path(), 0);
    place_seg(dir.path(), 1);
    // Seg 2 has a valid file but no row, so the fallback must probe it.
    place_real_seg(dir.path(), 2);
    // Rows for 0 and 1 (2 s each).
    append_csv(
        dir.path(),
        &format!(
            "{}{}",
            csv_row(0, 0, 2 * TICKS),
            csv_row(1, 2 * TICKS, 4 * TICKS)
        ),
    );

    let before = drain(&mut tracker, false, 2);
    assert_eq!(seqs(&before), vec![0, 1], "rows 0 and 1 before finish");
    for _ in 0..5 {
        let batch = tracker.poll_nowait().expect("poll_nowait");
        assert!(batch.is_empty(), "seg 2 waits for its row before finish");
    }

    tracker.finish();
    let after = drain(&mut tracker, true, 1);
    assert_eq!(seqs(&after), vec![2], "seg 2 indexed via fallback");
    let seg2 = &after[0];
    assert_eq!(seg2.start_100ns, 4 * TICKS, "starts at seg 1 end");
    let dur = seg2.end_100ns - seg2.start_100ns;
    assert!(
        (dur - 2 * TICKS).abs() <= TICKS / 100,
        "probe fallback gives about 2 s, got {dur} ticks"
    );
    assert!(tracker.quarantined().is_empty());
    assert_eq!(ring_seqs(&ring), vec![0, 1, 2]);
}

// ---------------------------------------------------------------------------
// T6: malformed row quarantines its segment; later rows still index
// ---------------------------------------------------------------------------

#[test]
fn t6_malformed_row_quarantined_later_rows_index() {
    let dir = TempDir::new().expect("tempdir");
    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    for seq in 0..3u64 {
        place_seg(dir.path(), seq);
    }
    // Seg 0 has a malformed row. Segs 1 and 2 are valid, 2 s each.
    append_csv(
        dir.path(),
        &format!(
            "{},abc,2.0\n{}{}",
            seg_name(0),
            csv_row(1, 2 * TICKS, 4 * TICKS),
            csv_row(2, 4 * TICKS, 6 * TICKS)
        ),
    );

    // A malformed row makes poll() return an error (a probe-style failure) for
    // each attempt; callers tolerate it and keep polling, as the other tracker
    // tests do. The first poll must report it.
    let first = tracker.poll();
    assert!(first.is_err(), "malformed row is reported by poll()");
    let mut got: Vec<CompletedSegment> = Vec::new();
    for _ in 0..10 {
        got.extend(tracker.poll().unwrap_or_default());
    }
    tracker.finish();
    for _ in 0..10 {
        got.extend(tracker.poll().unwrap_or_default());
    }

    assert_eq!(seqs(&got), vec![1, 2], "segments 1 and 2 indexed");
    let q = tracker.quarantined();
    assert_eq!(q.len(), 1, "exactly one quarantined segment: {q:?}");
    assert_eq!(q[0].seq, 0);
    assert_eq!(q[0].path, seg_path(dir.path(), 0));
    assert!(!q[0].reason.is_empty(), "quarantine records a reason");

    // The quarantined seg 0 keeps the timeline: seg 1 starts at the nominal
    // 2 s span of the skipped segment, and its row says 2 s long.
    assert_eq!(
        got[0].start_100ns,
        2 * TICKS,
        "gap span of the quarantined segment"
    );
    assert_eq!(got[0].end_100ns, 4 * TICKS);
    assert_eq!(ring_seqs(&ring), vec![1, 2], "ring index");
}
