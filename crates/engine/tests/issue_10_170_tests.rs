//! Integration tests for issue #170: the segment tracker skips unreadable
//! segments instead of stopping.
//!
//! `clipcheck-testkit` generates the valid segments. Unreadable segments are random
//! bytes written under the normal segment names, so the mp4 reader fails on them.
//!
//! Issue #10 (`unique_path`) is not covered here. The naming module is
//! `pub(crate)` (`recorder.rs`: `pub(crate) mod naming`), so an integration
//! test cannot reach it. Clip names are only produced by `Recorder::clip`,
//! which needs a captured window and buffered segments. Its unit tests in
//! `crates/engine/src/recorder/naming.rs` cover the behaviour.

use clipcheck_testkit::MediaSpec;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clipcheck_buffer::{RingIndex, TICKS_PER_SEC};
use clipcheck_engine::tracker::{CompletedSegment, SegmentTracker, TrackerThread};
use tempfile::TempDir;

/// Pause between segment placements, long enough for a poll to observe each.
const SEG_SLEEP: Duration = Duration::from_millis(150);
/// Nominal encoder segment length in 100 ns ticks (the tracker's gap span).
const NOMINAL_100NS: i64 = 2 * TICKS_PER_SEC;

/// Real 2-second mp4 segments, generated once per test.
struct Staged {
    _dir: TempDir,
    files: Vec<PathBuf>,
}

/// Generates one mp4 per requested duration with clipcheck-testkit.
fn stage(durations: &[f64]) -> Staged {
    let dir = TempDir::new().expect("staging tempdir");
    let mut files = Vec::new();
    for (i, &d) in durations.iter().enumerate() {
        let path = dir.path().join(format!("src-{i}.mp4"));
        let spec = MediaSpec::h264_aac(d)
            .with_video(160, 120, 15)
            .with_audio(Vec::new());
        clipcheck_testkit::write_mp4(&path, &spec).expect("testkit could not write segment");
        files.push(path);
    }
    Staged { _dir: dir, files }
}

/// Deterministic pseudo-random bytes (xorshift32). Not a valid mp4.
fn garbage(seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..4096)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 0xff) as u8
        })
        .collect()
}

/// Writes `bytes` as segment `i` via a temp name and rename, so the tracker
/// never sees a half-written file.
fn place_bytes(dir: &Path, i: usize, bytes: &[u8]) {
    let tmp = dir.join(format!("seg-{i:06}.mp4.tmp"));
    let dst = dir.join(format!("seg-{i:06}.mp4"));
    fs::write(&tmp, bytes).expect("write temp segment");
    fs::rename(&tmp, &dst).expect("rename segment into place");
}

/// Places the valid staged segment `i` as segment `i` of the tracked dir.
fn place_valid(staged: &Staged, dir: &Path, i: usize) {
    let bytes = fs::read(&staged.files[i]).expect("read staged segment");
    place_bytes(dir, i, &bytes);
}

/// Polls once. Returns the completed segments, or an empty list when the poll
/// returned an error (a transient probe failure). Errors are not fatal here.
fn poll_tolerant(tracker: &mut SegmentTracker) -> Vec<CompletedSegment> {
    tracker.poll().unwrap_or_default()
}

#[test]
fn corrupt_segment_is_skipped_and_tracking_continues() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0, 2.0, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = Arc::new(Mutex::new(RingIndex::new()));
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    let mut done: Vec<CompletedSegment> = Vec::new();
    for i in 0..staged.files.len() {
        if i == 1 {
            place_bytes(dir.path(), 1, &garbage(1));
        } else {
            place_valid(&staged, dir.path(), i);
        }
        done.extend(poll_tolerant(&mut tracker));
    }
    tracker.finish();
    for _ in 0..5 {
        done.extend(poll_tolerant(&mut tracker));
    }

    let seqs: Vec<u64> = done.iter().map(|s| s.seq).collect();
    assert_eq!(
        seqs,
        vec![0, 2, 3, 4],
        "valid segments after the corrupt one must still complete"
    );

    let quarantined = tracker.quarantined();
    assert_eq!(quarantined.len(), 1, "quarantined: {quarantined:?}");
    assert_eq!(quarantined[0].seq, 1, "quarantined seq");
    assert_eq!(
        quarantined[0].path,
        dir.path().join("seg-000001.mp4"),
        "quarantined path"
    );
    assert!(
        !quarantined[0].reason.is_empty(),
        "quarantine must carry the probe error"
    );

    assert!(
        !dir.path().join("seg-000001.mp4").exists(),
        "unreadable file must be deleted after quarantine"
    );

    // The timeline skips one nominal segment: seq 2 starts where the
    // quarantined span ends, and the gap is recorded at that boundary.
    assert_eq!(
        done[1].start_100ns,
        done[0].end_100ns + NOMINAL_100NS,
        "seq 2 must start one nominal segment after seq 0 ends"
    );
    let gap = ring.lock().expect("ring lock").gap_100ns();
    assert_eq!(gap, Some(done[1].start_100ns), "timeline gap");

    let indexed: Vec<u64> = ring
        .lock()
        .expect("ring lock")
        .iter()
        .map(|m| m.seq)
        .collect();
    assert_eq!(indexed, vec![0, 2, 3, 4], "ring index contents");
}

#[test]
fn transient_probe_failure_is_retried_not_quarantined() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = Arc::new(Mutex::new(RingIndex::new()));
    let mut tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    // Segment 0 exists but is not readable yet (empty file, still being written).
    place_bytes(dir.path(), 0, &[]);
    place_valid(&staged, dir.path(), 1);
    assert!(
        poll_tolerant(&mut tracker).is_empty(),
        "unreadable seq 0 must not complete"
    );
    assert!(
        poll_tolerant(&mut tracker).is_empty(),
        "unreadable seq 0 must not complete on the second poll"
    );
    assert!(
        tracker.quarantined().is_empty(),
        "two failed probes must not quarantine: {:?}",
        tracker.quarantined()
    );

    // The encoder finishes writing segment 0.
    place_valid(&staged, dir.path(), 0);
    let first = tracker
        .poll()
        .expect("poll after segment 0 becomes readable");
    assert_eq!(
        first.iter().map(|s| s.seq).collect::<Vec<_>>(),
        vec![0],
        "segment 0 must complete on retry"
    );

    place_valid(&staged, dir.path(), 2);
    let mut done = first;
    done.extend(tracker.poll().expect("poll after seq 2 placed"));
    tracker.finish();
    done.extend(tracker.poll().expect("poll after finish"));

    let seqs: Vec<u64> = done.iter().map(|s| s.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2], "all segments complete in order");
    assert!(tracker.quarantined().is_empty(), "nothing quarantined");
    assert_eq!(ring.lock().expect("ring lock").gap_100ns(), None, "no gap");
    for pair in done.windows(2) {
        assert_eq!(
            pair[1].start_100ns, pair[0].end_100ns,
            "continuity across the retried segment"
        );
    }
}

#[test]
fn thread_stop_is_ok_when_nothing_skipped() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = Arc::new(Mutex::new(RingIndex::new()));
    let tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let thread = TrackerThread::spawn(
        tracker,
        Duration::from_millis(50),
        Box::new(move |s: &CompletedSegment| sink.lock().expect("sink lock").push(s.seq)),
    );
    for i in 0..staged.files.len() {
        place_valid(&staged, dir.path(), i);
        thread::sleep(SEG_SLEEP);
    }
    let result = thread.stop();
    assert!(
        result.is_ok(),
        "stop() with no skipped segments: {result:?}"
    );
    assert_eq!(
        *seen.lock().expect("sink lock"),
        vec![0, 1, 2],
        "every segment reaches on_complete"
    );
}

#[test]
fn thread_stop_errors_and_names_skipped_sequences() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0, 2.0, 2.0, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = Arc::new(Mutex::new(RingIndex::new()));
    let tracker = SegmentTracker::new(dir.path(), 3600.0, Arc::clone(&ring));

    let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let thread = TrackerThread::spawn(
        tracker,
        Duration::from_millis(50),
        Box::new(move |s: &CompletedSegment| sink.lock().expect("sink lock").push(s.seq)),
    );
    // Valid 0, corrupt 1, valid 2, corrupt 3, valid 4 and 5.
    for i in 0..staged.files.len() {
        if i == 1 || i == 3 {
            place_bytes(dir.path(), i, &garbage(i as u32 + 7));
        } else {
            place_valid(&staged, dir.path(), i);
        }
        thread::sleep(SEG_SLEEP);
    }
    let result = thread.stop();
    let err = result.expect_err("stop() must fail when segments were skipped");
    let msg = err.to_string();
    assert!(
        msg.contains("2 segment(s)") && msg.contains("seq 1, 3"),
        "error must name skipped seqs 1 and 3: {msg}"
    );
    assert_eq!(
        *seen.lock().expect("sink lock"),
        vec![0, 2, 4, 5],
        "readable segments still reach on_complete"
    );
    assert!(
        !dir.path().join("seg-000001.mp4").exists() && !dir.path().join("seg-000003.mp4").exists(),
        "skipped files must be deleted"
    );
}
