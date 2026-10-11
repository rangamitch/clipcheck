//! Integration tests for `clipcheck_engine::tracker`.
//!
//! Native test media (`clipcheck-testkit`) generates the segments and the in-repo MP4
//! reader measures them. Segments are copied one by one into a tracked directory, the way
//! an encoder would produce them, with polls in between.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipcheck_buffer::{ClipPlanError, RingIndex, TICKS_PER_SEC};
use clipcheck_engine::tracker::{CompletedSegment, SegmentTracker, TrackerThread};

use tempfile::TempDir;

#[macro_use]
mod common;

use clipcheck_testkit::MediaSpec;
use common::wait_until;

/// Upper bound for a copied segment to become visible at its final name.
const VISIBLE_TIMEOUT: Duration = Duration::from_secs(2);

/// Ticks for a probed duration, computed as the tracker does it.
fn secs_to_ticks(secs: f64) -> i64 {
    (secs * TICKS_PER_SEC as f64).round() as i64
}

/// Real mp4 segments generated up front. `ticks[i]` is the probed duration of
/// `files[i]` in 100 ns ticks.
struct Staged {
    _dir: TempDir,
    files: Vec<PathBuf>,
    ticks: Vec<i64>,
}

/// Generates one mp4 per requested duration, then measures each with the MP4 reader.
fn stage(durations: &[f64]) -> Staged {
    let dir = TempDir::new().expect("staging tempdir");
    let mut files = Vec::new();
    let mut ticks = Vec::new();
    for (i, &d) in durations.iter().enumerate() {
        let path = dir.path().join(format!("src-{i}.mp4"));
        let spec = MediaSpec::h264_aac(d).with_video(160, 120, 15);
        clipcheck_testkit::write_mp4(&path, &spec)
            .unwrap_or_else(|e| panic!("write staged segment {i}: {e}"));
        let secs = reader_duration(&path);
        assert!(
            (secs - d).abs() < 0.25,
            "staged segment {i}: probed {secs} s, requested {d} s"
        );
        ticks.push(secs_to_ticks(secs));
        files.push(path);
    }
    Staged {
        _dir: dir,
        files,
        ticks,
    }
}

/// Copies staged segment `i` into `dir` as `seg-00000i.mp4`, via a temp name
/// and rename so the tracker never sees a half-copied file. Then waits, bounded
/// by `VISIBLE_TIMEOUT`, until the file is visible at its final name with the
/// source length.
fn publish(staged: &Staged, dir: &Path, i: usize) {
    let tmp = dir.join(format!("seg-{i:06}.mp4.tmp"));
    let dst = dir.join(format!("seg-{i:06}.mp4"));
    let src_len = fs::metadata(&staged.files[i])
        .expect("staged segment exists")
        .len();
    fs::copy(&staged.files[i], &tmp).expect("copy segment to temp name");
    fs::rename(&tmp, &dst).expect("rename segment into place");
    let visible = wait_until(VISIBLE_TIMEOUT, || {
        fs::metadata(&dst).is_ok_and(|m| m.len() == src_len)
    });
    assert!(
        visible,
        "segment {i} not visible at {} in time",
        dst.display()
    );
}

fn new_ring() -> Arc<Mutex<RingIndex>> {
    Arc::new(Mutex::new(RingIndex::new()))
}

fn tracker_for(dir: &Path, max_secs: f64, ring: &Arc<Mutex<RingIndex>>) -> SegmentTracker {
    SegmentTracker::new(dir, max_secs, Arc::clone(ring))
}

/// Everything produced by copying all staged segments, polling after each
/// copy, then calling finish and polling once more.
struct Run {
    dir: TempDir,
    ring: Arc<Mutex<RingIndex>>,
    tracker: SegmentTracker,
    completed: Vec<CompletedSegment>,
}

fn copy_all_and_finish(staged: &Staged, max_secs: f64) -> Run {
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = new_ring();
    let mut tracker = tracker_for(dir.path(), max_secs, &ring);
    let mut completed = Vec::new();
    for i in 0..staged.files.len() {
        publish(staged, dir.path(), i);
        completed.extend(tracker.poll().expect("poll during copy"));
    }
    tracker.finish();
    completed.extend(tracker.poll().expect("poll after finish"));
    Run {
        dir,
        ring,
        tracker,
        completed,
    }
}

fn seqs(segs: &[CompletedSegment]) -> Vec<u64> {
    segs.iter().map(|s| s.seq).collect()
}

#[test]
fn completion_order_and_continuity() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.5, 2.0, 1.5, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = new_ring();
    let mut tracker = tracker_for(dir.path(), 3600.0, &ring);

    let mut all: Vec<CompletedSegment> = Vec::new();
    for i in 0..staged.files.len() {
        publish(&staged, dir.path(), i);
        let got = tracker.poll().expect("poll during copy");
        // Copying segment i completes exactly segment i-1.
        let expect: Vec<u64> = if i == 0 { vec![] } else { vec![(i - 1) as u64] };
        assert_eq!(seqs(&got), expect, "poll after copying segment {i}");
        all.extend(got);
    }
    // Segment 4 has no successor: nothing else until finish.
    assert!(
        tracker.poll().expect("poll without finish").is_empty(),
        "last segment reported without finish()"
    );
    tracker.finish();
    let last = tracker.poll().expect("poll after finish");
    assert_eq!(seqs(&last), vec![4], "finish() should complete seq 4");
    all.extend(last);

    assert_eq!(seqs(&all), vec![0, 1, 2, 3, 4], "seq order across polls");
    assert_eq!(all[0].start_100ns, 0, "first segment must start at 0");
    for (i, seg) in all.iter().enumerate() {
        assert_eq!(
            seg.path,
            dir.path().join(format!("seg-{i:06}.mp4")),
            "path for seq {i}"
        );
        if i > 0 {
            assert_eq!(
                seg.start_100ns,
                all[i - 1].end_100ns,
                "seq {i} start must equal seq {} end",
                i - 1
            );
        }
        assert_eq!(
            seg.end_100ns - seg.start_100ns,
            staged.ticks[i],
            "seq {i} duration must match the MP4 reader"
        );
        let on_disk = fs::metadata(&seg.path).expect("segment exists").len();
        assert_eq!(seg.bytes, on_disk, "seq {i} bytes must equal file length");
    }
    // Re-read the tracked file itself with the MP4 reader.
    let probed = reader_duration(&all[2].path);
    assert_eq!(
        all[2].end_100ns - all[2].start_100ns,
        secs_to_ticks(probed),
        "seq 2 duration vs MP4 reader"
    );
}

#[test]
fn no_premature_completion() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = new_ring();
    let mut tracker = tracker_for(dir.path(), 3600.0, &ring);
    let seg0 = dir.path().join("seg-000000.mp4");

    // Zero-byte file with no successor and no finish(): not reported.
    fs::write(&seg0, b"").expect("write zero-byte seg 0");
    assert!(
        tracker.poll().expect("poll zero-byte").is_empty(),
        "zero-byte seg 0 with no successor was reported"
    );
    assert!(tracker.poll().expect("poll again").is_empty());

    // Real content, still no successor and no finish(): still not reported.
    fs::copy(&staged.files[0], &seg0).expect("overwrite seg 0 with real data");
    assert!(
        tracker.poll().expect("poll real seg 0").is_empty(),
        "seg 0 with no successor was reported before finish()"
    );
    assert!(
        ring.lock().expect("ring lock").is_empty(),
        "ring must stay empty before completion"
    );
}

#[test]
fn ring_index_contents_and_plan_clip() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.5, 2.0, 1.5, 2.0]);
    let run = copy_all_and_finish(&staged, 3600.0);
    assert_eq!(seqs(&run.completed), vec![0, 1, 2, 3, 4]);

    let ring = run.ring.lock().expect("ring lock");
    assert_eq!(ring.len(), staged.files.len(), "no eviction expected");
    assert!(!ring.is_empty());

    let mut expected_bytes: u64 = 0;
    let mut prev_end: Option<i64> = None;
    for (i, meta) in ring.iter().enumerate() {
        assert_eq!(meta.seq, i as u64, "ring seq order");
        assert!(meta.committed, "seq {} not committed", meta.seq);
        assert_eq!(meta.first_keyframe_100ns, meta.start_100ns);
        if let Some(pe) = prev_end {
            assert_eq!(meta.start_100ns, pe, "ring contiguity at seq {}", meta.seq);
        }
        prev_end = Some(meta.end_100ns);
        let len = fs::metadata(&meta.path).expect("ring file exists").len();
        assert_eq!(meta.bytes, len, "ring bytes for seq {}", meta.seq);
        expected_bytes += len;
    }
    assert_eq!(ring.total_bytes(), expected_bytes, "total_bytes");

    let now = prev_end.expect("ring has segments");
    let total_ticks: i64 = staged.ticks.iter().sum();
    assert_eq!(now, total_ticks, "ring end equals summed durations");

    let plan = ring
        .plan_clip(now, 3, 0)
        .expect("plan_clip(3 s) must succeed");
    assert!(!plan.segments.is_empty(), "plan has no segments");
    assert!(
        (plan.segments[0].seq as usize) < staged.files.len(),
        "plan first seq out of range"
    );
    assert!(plan.effective_secs > 0.0, "effective_secs must be positive");
    assert!(
        (plan.effective_secs - 3.0).abs() < 1e-6,
        "effective_secs {} should be 3.0 when the ring is long enough",
        plan.effective_secs
    );
    assert!(plan.trim_start_offset_100ns >= 0, "negative trim offset");
    let first_dur = plan.segments[0].end_100ns - plan.segments[0].start_100ns;
    assert!(
        plan.trim_start_offset_100ns < first_dur,
        "trim offset {} beyond first segment ({first_dur} ticks)",
        plan.trim_start_offset_100ns
    );
    assert!(!plan.short, "3 s plan from a 9 s ring should not be short");

    assert_eq!(
        ring.plan_clip(now, 0, 0).expect_err("seconds=0 must fail"),
        ClipPlanError::InvalidDuration
    );
    assert_eq!(
        ring.plan_clip(now, 3, -1)
            .expect_err("negative pad must fail"),
        ClipPlanError::InvalidDuration
    );
}

#[test]
fn eviction_deletes_old_files() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0; 7]);
    let max_secs = 3.0;
    let run = copy_all_and_finish(&staged, max_secs);
    assert_eq!(
        seqs(&run.completed),
        (0..7).collect::<Vec<u64>>(),
        "all segments reported once"
    );

    let ring = run.ring.lock().expect("ring lock");
    assert!(ring.len() < staged.files.len(), "ring should have evicted");
    assert!(
        ring.len() <= 4,
        "ring holds {} segments, expected <= 4",
        ring.len()
    );
    assert!(!ring.is_empty());

    let ring_seqs: Vec<u64> = ring.iter().map(|s| s.seq).collect();
    let oldest_start = ring.iter().next().expect("oldest").start_100ns;
    assert!(oldest_start > 0, "oldest ring segment still starts at 0");
    assert_eq!(
        *ring_seqs.last().expect("newest"),
        6,
        "newest ring seq should be the last segment"
    );
    let first = ring_seqs[0];
    assert_eq!(
        ring_seqs,
        (first..=6).collect::<Vec<u64>>(),
        "ring seqs must be contiguous"
    );

    // Every seq 0..7: the file exists on disk iff the ring still holds it.
    for seq in 0..7u64 {
        let path = run.dir.path().join(format!("seg-{seq:06}.mp4"));
        let on_disk = path.exists();
        let in_ring = ring_seqs.contains(&seq);
        assert_eq!(
            on_disk, in_ring,
            "seq {seq}: on_disk={on_disk} in_ring={in_ring}"
        );
    }

    // No stray segment files beyond the seq range.
    let mut names: Vec<String> = fs::read_dir(run.dir.path())
        .expect("read tracked dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("seg-") && n.ends_with(".mp4"))
        .collect();
    names.sort();
    let expected_names: Vec<String> = ring_seqs
        .iter()
        .map(|s| format!("seg-{s:06}.mp4"))
        .collect();
    assert_eq!(names, expected_names, "files on disk vs ring");
}

#[test]
fn finish_completes_last() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0, 2.0, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = new_ring();
    let mut tracker = tracker_for(dir.path(), 3600.0, &ring);

    let mut got_before_finish = Vec::new();
    for i in 0..staged.files.len() {
        publish(&staged, dir.path(), i);
        got_before_finish.extend(tracker.poll().expect("poll during copy"));
    }
    assert_eq!(
        seqs(&got_before_finish),
        vec![0, 1, 2, 3],
        "before finish, N-1 segments complete"
    );

    tracker.finish();
    let last = tracker.poll().expect("poll after finish");
    assert_eq!(seqs(&last), vec![4], "finish() must complete exactly seq 4");

    tracker.finish();
    assert!(
        tracker.poll().expect("second finish poll").is_empty(),
        "second finish+poll must return nothing"
    );
}

#[test]
fn poll_idempotent() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0]);
    let mut run = copy_all_and_finish(&staged, 3600.0);
    assert_eq!(seqs(&run.completed), vec![0, 1, 2]);
    assert!(run.tracker.poll().expect("poll 1").is_empty());
    assert!(run.tracker.poll().expect("poll 2").is_empty());
    assert_eq!(run.ring.lock().expect("ring lock").len(), 3);
}

#[test]
fn thread_spawn_and_stop() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let staged = stage(&[2.0, 2.0, 2.0, 2.0, 2.0]);
    let dir = TempDir::new().expect("tracked tempdir");
    let ring = new_ring();
    let tracker = tracker_for(dir.path(), 3600.0, &ring);

    let collected: Arc<Mutex<Vec<CompletedSegment>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&collected);
    let thread = TrackerThread::spawn(
        tracker,
        Duration::from_millis(50),
        Box::new(move |seg: &CompletedSegment| {
            if let Ok(mut v) = sink.lock() {
                v.push(seg.clone());
            }
        }),
    );

    for i in 0..staged.files.len() {
        publish(&staged, dir.path(), i);
    }
    thread.stop().expect("stop() must return Ok");

    let got = collected.lock().expect("collected lock").clone();
    assert_eq!(seqs(&got), vec![0, 1, 2, 3, 4], "callback seqs in order");
    for (i, seg) in got.iter().enumerate() {
        if i > 0 {
            assert_eq!(seg.start_100ns, got[i - 1].end_100ns, "callback continuity");
        }
        assert_eq!(seg.end_100ns - seg.start_100ns, staged.ticks[i]);
    }
    assert_eq!(
        ring.lock().expect("ring lock").len(),
        5,
        "ring after thread"
    );
}

#[test]
fn nonexistent_dir_poll_is_empty() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let parent = TempDir::new().expect("tempdir");
    let missing = parent.path().join("does-not-exist");
    let ring = new_ring();
    let mut tracker = tracker_for(&missing, 10.0, &ring);
    let got = tracker.poll().expect("poll on missing dir must be Ok");
    assert!(got.is_empty(), "expected no segments, got {got:?}");
}

/// Duration of the movie in seconds, read with the in-repo MP4 reader.
fn reader_duration(path: &Path) -> f64 {
    clipcheck_mp4::reader::read_movie(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .duration_secs
}
