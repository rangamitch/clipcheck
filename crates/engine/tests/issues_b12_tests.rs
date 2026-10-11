//! Regression tests for the engine fixes in issues #11, #13, #19, #24, #27,
//! #171 and #172, through the public API.
//!
//! Probes run in-process (D58), so no external tool runs here. Tests that need a valid MP4
//! generate one with clipcheck-testkit, and skip when it is unavailable. The concat and
//! recovery paths are native (D58). Other media files are plain bytes.
//!
//! Not covered, with the reason:
//! - #14 (dead branches): removes unreachable code, no behaviour to observe.
//! - #12 (concat dedupe): the concat list format is covered by buffer's tests.
//! - #22 (status walk): needs an attached recorder and a live capture.
//! - #13 `unique_path` and #11 shared helpers: `pub(crate)`, not reachable here.
//! - #19 allocation counts: not observable; the mixed output is checked instead.

mod common;

use clipcheck_testkit::MediaSpec;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use clipcheck_buffer::RingIndex;
use clipcheck_engine::audiomix::{Mixer, NoiseGate, SourceEdge, BLOCK_SAMPLES};
use clipcheck_engine::catalog::{find_registered, register_recording, CatalogError, RegisterMeta};
use clipcheck_engine::sessionfile::{recover, SessionAssembler};
use clipcheck_engine::tracker::SegmentTracker;
use clipcheck_library::db::{NewRecording, RecordingSource};
use clipcheck_library::{Library, RecStatus, RecordingKind};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn meta() -> RegisterMeta<'static> {
    RegisterMeta {
        kind: RecordingKind::Clip,
        game: "exe:test.exe",
        session_id: None,
        part_index: None,
        created_ms: 1_000,
    }
}

fn seg_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("seg-{seq:06}.mp4"))
}

/// Writes segment `seq` as a copy of `clip`, a real MP4 the tracker can read.
fn place_seg(dir: &Path, seq: u64, clip: &[u8]) {
    fs::write(seg_path(dir, seq), clip).expect("write segment");
}

/// A real 3 s MP4 generated once with clipcheck-testkit. The tracker and the catalog read
/// it natively (D58), so tests that need a valid file use this. `None` when native test media
/// is unavailable, and the caller skips.
fn real_clip() -> Option<&'static [u8]> {
    static CLIP: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    CLIP.get_or_init(|| {
        if !clipcheck_testkit::available() {
            return None;
        }
        let out = common::fixture_dir().join("tracker_clip.mp4");
        let spec = MediaSpec::h264_aac(3.0)
            .with_video(320, 240, 25)
            .with_audio(Vec::new());
        clipcheck_testkit::write_mp4(&out, &spec).ok()?;
        fs::read(&out).ok()
    })
    .as_deref()
}

/// A real 2 s fragmented MP4 part, shaped like the recorder's staged segments. Encoded once with
/// clipcheck-testkit as a fixture generator. `None` when native test media is unavailable.
fn real_fragmented_part() -> Option<&'static [u8]> {
    static PART: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    PART.get_or_init(|| {
        if !clipcheck_testkit::available() {
            return None;
        }
        let dir = common::fixture_dir().join("session_part_frag");
        let _ = fs::remove_dir_all(&dir);
        let spec = MediaSpec::h264_aac(2.0)
            .with_video(320, 240, 25)
            .with_audio(Vec::new());
        let segs = clipcheck_testkit::write_segments(&dir, &spec, 2.0).ok()?;
        fs::read(segs.first()?).ok()
    })
    .as_deref()
}

fn new_ring() -> Arc<Mutex<RingIndex>> {
    Arc::new(Mutex::new(RingIndex::new()))
}

/// Constant block of `v`, `BLOCK_SAMPLES` long.
fn block(v: f32) -> Vec<f32> {
    vec![v; BLOCK_SAMPLES]
}

fn edge(source: usize, gain: f32, gate: Option<NoiseGate>) -> SourceEdge {
    SourceEdge { source, gain, gate }
}

// ---------------------------------------------------------------------------
// #13: probe fields that are missing or malformed fail the probe
// ---------------------------------------------------------------------------

#[test]
fn issue_13_register_pre_1970_mtime_is_an_error_not_zero() {
    let root = TempDir::new().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    let file = root.path().join("clip.mp4");
    let Some(clip) = real_clip() else {
        eprintln!("SKIPPED: pre-1970 mtime: native test media unavailable");
        return;
    };
    fs::write(&file, clip).expect("write media");
    let before_epoch = UNIX_EPOCH - Duration::from_secs(10);
    fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .expect("open for mtime")
        .set_modified(before_epoch)
        .expect("set mtime before 1970");

    let thumbs = root.path().join("thumbs");
    let got = register_recording(&lib, &thumbs, root.path(), &file, meta());

    match got {
        Err(CatalogError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}"),
        other => panic!("pre-1970 mtime must fail registration, got {other:?}"),
    }
    let root_id = lib
        .ensure_storage_root(&root.path().to_string_lossy(), 1_000)
        .expect("root");
    assert!(
        lib.list_paths_for_root(root_id).expect("list").is_empty(),
        "no row may be written with a fake mtime"
    );
}

#[test]
fn issue_13_register_without_poster_still_registers_the_recording() {
    let Some(clip) = real_clip() else {
        eprintln!("SKIPPED: poster-less registration: native test media unavailable");
        return;
    };
    let root = TempDir::new().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    let file = root.path().join("clip.mp4");
    fs::write(&file, clip).expect("write media");
    // A regular file where the thumbnails directory should be makes the poster write fail.
    // The recording must still register.
    let thumbs = root.path().join("thumbs");
    fs::write(&thumbs, b"not a directory").expect("block thumbs dir");

    let id = register_recording(&lib, &thumbs, root.path(), &file, meta())
        .expect("registration succeeds without a poster");
    assert!(
        !thumbs.join(&id).join("poster.jpg").exists(),
        "no poster was written"
    );
    let root_id = lib
        .ensure_storage_root(&root.path().to_string_lossy(), 1_000)
        .expect("root");
    let rows = lib.list_paths_for_root(root_id).expect("list");
    assert_eq!(rows.len(), 1, "one row registered");
    assert_eq!(rows[0].1, "clip.mp4", "row names the file");
}

// #24: registration looks the file up by key, past the first library page
// ---------------------------------------------------------------------------

fn new_row(id: &str, root_id: i64, rel: &str) -> NewRecording {
    NewRecording {
        id: id.to_string(),
        kind: RecordingKind::Clip,
        session_id: None,
        part_index: None,
        game_id: None,
        root_id,
        file_name: rel.to_string(),
        rel_path: rel.to_string(),
        ext: "mp4".to_string(),
        size_bytes: 1024,
        file_mtime_ms: 1_000,
        created_at_ms: 1_000,
        added_at_ms: 1_000,
        duration_ms: 5_000,
        status: RecStatus::Ready,
        error_detail: None,
        end_reason: None,
        recovered: false,
        degraded: false,
        dropped_packets: 0,
        gap_ms: 0,
        needs_faststart: false,
    }
}

#[test]
fn issue_24_find_registered_finds_a_row_past_the_first_page() {
    let root = TempDir::new().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    let root_id = lib
        .ensure_storage_root(&root.path().to_string_lossy(), 1_000)
        .expect("root");
    // More rows than one 200-row page of the library listing.
    for i in 0..250u32 {
        let rel = format!("clip-{i:03}.mp4");
        fs::write(root.path().join(&rel), b"x").expect("write media");
        let id = format!("01HZ{i:022}");
        lib.insert_recording_sourced(
            &new_row(&id, root_id, &rel),
            RecordingSource::Recorded,
            true,
        )
        .expect("insert");
    }

    let target = root.path().join("clip-240.mp4");
    let found = find_registered(&lib, root.path(), &target).expect("lookup");
    assert_eq!(
        found,
        Some(format!("01HZ{:022}", 240)),
        "row 240 found by key"
    );

    let first = find_registered(&lib, root.path(), &root.path().join("clip-000.mp4"))
        .expect("lookup first");
    assert_eq!(first, Some(format!("01HZ{:022}", 0)), "row 0 found");
}

#[test]
fn issue_24_find_registered_is_none_for_an_unregistered_file() {
    let root = TempDir::new().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    let root_id = lib
        .ensure_storage_root(&root.path().to_string_lossy(), 1_000)
        .expect("root");
    lib.insert_recording_sourced(
        &new_row("01HZ0000000000000000000R01", root_id, "clip.mp4"),
        RecordingSource::Recorded,
        true,
    )
    .expect("insert");
    fs::write(root.path().join("extra.mp4"), b"x").expect("write extra");

    let got = find_registered(&lib, root.path(), &root.path().join("extra.mp4")).expect("lookup");
    assert_eq!(got, None, "unregistered file has no id");
}

#[test]
fn issue_24_find_registered_rejects_a_file_outside_the_root() {
    let root = TempDir::new().expect("tempdir");
    let elsewhere = TempDir::new().expect("tempdir");
    let lib = Library::open_in_memory().expect("open");
    let outside = elsewhere.path().join("clip.mp4");
    fs::write(&outside, b"x").expect("write outside");

    let err = find_registered(&lib, root.path(), &outside).expect_err("outside root must fail");
    assert!(
        matches!(err, CatalogError::NotUnderRoot { .. }),
        "expected NotUnderRoot, got {err}"
    );
}

// ---------------------------------------------------------------------------
// #172: recover() does nothing for a missing or empty stage directory
// ---------------------------------------------------------------------------

#[test]
fn issue_172_recover_missing_dir_is_none_and_not_created() {
    let tmp = TempDir::new().expect("tempdir");
    let missing = tmp.path().join("no-such-stage");
    let out = tmp.path().join("out.mp4");
    let got = recover(missing.clone(), &out).expect("recover on a missing dir is Ok");
    assert!(got.is_none(), "missing dir yields None");
    assert!(!missing.exists(), "recover must not create the stage dir");
    assert!(!out.exists(), "no output written");
}

#[test]
fn issue_172_recover_empty_dir_is_none() {
    let tmp = TempDir::new().expect("tempdir");
    let stage = tmp.path().join("stage");
    fs::create_dir_all(&stage).expect("mkdir");
    let out = tmp.path().join("out.mp4");
    // Recovery is native; an empty stage dir must yield None without writing output.
    let got = recover(stage, &out).expect("empty stage dir must recover to None");
    assert!(got.is_none(), "no parts yields None");
    assert!(!out.exists(), "no output written");
}

#[test]
fn issue_172_recover_keeps_unrelated_files_and_returns_none() {
    let tmp = TempDir::new().expect("tempdir");
    let stage = tmp.path().join("stage");
    fs::create_dir_all(&stage).expect("mkdir");
    fs::write(stage.join("notes.txt"), b"keep me").expect("write notes");
    let out = tmp.path().join("out.mp4");

    let got = recover(stage.clone(), &out).expect("recover with only stray files is Ok");
    assert!(got.is_none(), "no part files yields None");
    assert_eq!(
        fs::read(stage.join("notes.txt")).expect("notes still there"),
        b"keep me",
        "stray file untouched"
    );
    assert!(!out.exists(), "no output written");
}

#[test]
fn issue_172_recover_path_that_is_a_file_is_none() {
    let tmp = TempDir::new().expect("tempdir");
    let not_a_dir = tmp.path().join("stage-is-a-file");
    fs::write(&not_a_dir, b"x").expect("write file");
    let out = tmp.path().join("out.mp4");

    let got =
        recover(not_a_dir.clone(), &out).expect("a file where the stage dir should be is Ok(None)");
    assert!(got.is_none(), "not a directory yields None");
    assert!(not_a_dir.is_file(), "file left alone");
}

#[test]
fn issue_172_recover_with_parts_reaches_finalize_and_keeps_parts_on_failure() {
    let Some(part) = real_fragmented_part() else {
        eprintln!("SKIPPED: recover with parts: native test media unavailable");
        return;
    };
    let tmp = TempDir::new().expect("tempdir");
    let seg = tmp.path().join("seg.mp4");
    fs::write(&seg, part).expect("write seg");

    // Success path: two real fragmented parts staged (a simulated crash), then recovered.
    let good_stage = tmp.path().join("good-stage");
    {
        let mut asm = SessionAssembler::new(good_stage.clone()).expect("new");
        asm.add_segment(&seg).expect("add");
        asm.add_segment(&seg).expect("add");
    }
    let good_out = tmp.path().join("good.mp4");
    let done = recover(good_stage, &good_out)
        .expect("recover with real parts must succeed")
        .expect("staged parts must reach finalize");
    assert_eq!(done.segments, 2);
    assert!(done.bytes > 0, "finalized output has bytes");
    assert!(good_out.exists(), "output written");

    // Failure path: one real part and one corrupt part. Concat fails, parts survive, no output.
    let bad = tmp.path().join("bad.mp4");
    fs::write(&bad, b"not an mp4 at all").expect("write bad");
    let bad_stage = tmp.path().join("bad-stage");
    {
        let mut asm = SessionAssembler::new(bad_stage.clone()).expect("new");
        asm.add_segment(&seg).expect("add");
        asm.add_segment(&bad).expect("add");
    }
    let bad_out = tmp.path().join("bad-out.mp4");
    let err = recover(bad_stage.clone(), &bad_out).expect_err("a corrupt part must fail recover");
    let msg = err.to_string();
    assert!(
        msg.len() < 2_048,
        "bounded message, got {} bytes",
        msg.len()
    );
    let parts = fs::read_dir(&bad_stage)
        .expect("read stage")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("part-"))
        .count();
    assert_eq!(parts, 2, "parts survive a failed recovery");
    assert!(!bad_out.exists(), "no output on failure");
}

// ---------------------------------------------------------------------------
// #11: a failed concat names the problem, stays bounded, and leaves no output (D58: native concat)
// ---------------------------------------------------------------------------

#[test]
fn issue_11_concat_failure_message_names_the_problem_and_is_bounded() {
    let tmp = TempDir::new().expect("tempdir");
    let bad = tmp.path().join("bad.mp4");
    fs::write(&bad, vec![b'A'; 5_000]).expect("write bad");
    let stage = tmp.path().join("stage");
    let mut asm = SessionAssembler::new(stage.clone()).expect("new");
    asm.add_segment(&bad).expect("add");
    asm.add_segment(&bad).expect("add");
    let out = tmp.path().join("out.mp4");
    let err = asm
        .finalize(&out)
        .expect_err("concat of corrupt parts must fail");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("mp4"),
        "message names the problem: {msg}"
    );
    assert!(
        msg.len() < 2_048,
        "message bounded, got {} bytes",
        msg.len()
    );
    assert!(!out.exists(), "no output left");
    let leftovers: Vec<String> = fs::read_dir(tmp.path())
        .expect("read tmp")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("out."))
        .collect();
    assert!(leftovers.is_empty(), "no temp output left: {leftovers:?}");
}

// ---------------------------------------------------------------------------
// #19: the mixer's sums, gains and gates are unchanged by buffer reuse
// ---------------------------------------------------------------------------

/// A ramp of exact binary fractions in `[-1, 1]`, so gain-of-0.5 results are
/// exact and unity gain does not clip. (`BLOCK_SAMPLES` is 1920, so the range
/// must be centred on zero to stay in bounds.)
fn ramp() -> Vec<f32> {
    (0..BLOCK_SAMPLES)
        .map(|i| (i as f32 - 960.0) / 1024.0)
        .collect()
}

#[test]
fn issue_19_unity_gain_edge_passes_samples_unchanged() {
    let src = ramp();
    let mut m = Mixer::new(1, vec![vec![edge(0, 1.0, None)]]);
    m.push(0, &src);
    let out = m.tick();
    assert_eq!(out[0], src, "unity gain must reproduce the source exactly");
    assert_eq!(m.clipped(), 0, "no clipping in range");
}

#[test]
fn issue_19_half_gain_edge_is_an_exact_half() {
    let src = ramp();
    let expect: Vec<f32> = src.iter().map(|s| s * 0.5).collect();
    let mut m = Mixer::new(1, vec![vec![edge(0, 0.5, None)]]);
    m.push(0, &src);
    let out = m.tick();
    assert_eq!(out[0], expect, "gain 0.5 output");
}

#[test]
fn issue_19_sum_is_clamped_once_after_all_edges_are_added() {
    // 0.9 + 0.9 - 0.9 is 0.9. Clamping after each add would give 0.1.
    let mut m = Mixer::new(
        3,
        vec![vec![
            edge(0, 1.0, None),
            edge(1, 1.0, None),
            edge(2, 1.0, None),
        ]],
    );
    m.push(0, &block(0.9));
    m.push(1, &block(0.9));
    m.push(2, &block(-0.9));
    let out = m.tick();
    assert!(
        out[0].iter().all(|&s| (s - 0.9).abs() < 1e-6),
        "sum must be 0.9, not clamped mid-sum"
    );
    assert_eq!(m.clipped(), 0, "no clipping for an in-range sum");
}

#[test]
fn issue_19_closed_gate_on_shared_source_leaves_the_other_edge_exact() {
    let mut m = Mixer::new(
        1,
        vec![
            vec![edge(0, 1.0, Some(NoiseGate::new(-30.0)))],
            vec![edge(0, 1.0, None)],
        ],
    );
    m.push(0, &block(0.001));
    let out = m.tick();
    assert!(out[0].iter().all(|&s| s == 0.0), "gated edge is silent");
    assert!(out[1].iter().all(|&s| s == 0.001), "ungated edge is exact");

    // The next tick reuses the same buffers. Nothing from the last tick may leak.
    m.push(0, &block(0.002));
    let out = m.tick();
    assert!(out[0].iter().all(|&s| s == 0.0), "gated edge still silent");
    assert!(out[1].iter().all(|&s| s == 0.002), "second tick exact");
}

#[test]
fn issue_19_open_gate_then_gain_multiplies_after_the_gate() {
    // Loud source: the gate opens at once, so the gated edge passes 0.5 * 0.5.
    // The ungated edge adds 0.5 * 0.25. The total is exactly 0.375.
    let mut m = Mixer::new(
        1,
        vec![vec![
            edge(0, 0.5, Some(NoiseGate::new(-30.0))),
            edge(0, 0.25, None),
        ]],
    );
    m.push(0, &block(0.5));
    let out = m.tick();
    assert!(
        out[0].iter().all(|&s| s == 0.375),
        "gate then gain per edge"
    );
}

#[test]
fn issue_19_tick_without_input_does_not_repeat_the_previous_block() {
    let mut m = Mixer::new(1, vec![vec![edge(0, 1.0, None)]]);
    m.push(0, &block(0.5));
    let first = m.tick();
    assert!(
        first[0].iter().all(|&s| s == 0.5),
        "first tick passes input"
    );

    // Nothing pushed: the ring underflows to zeros. The old block must not repeat.
    let second = m.tick();
    assert!(second[0].iter().all(|&s| s == 0.0), "no stale samples");
}

#[test]
fn issue_19_missing_source_is_silent_with_or_without_a_gate() {
    let mut m = Mixer::new(
        1,
        vec![vec![
            edge(7, 1.0, Some(NoiseGate::new(-30.0))),
            edge(7, 0.5, None),
        ]],
    );
    m.push(0, &block(0.5));
    let out = m.tick();
    assert_eq!(out[0].len(), BLOCK_SAMPLES, "block length");
    assert!(
        out[0].iter().all(|&s| s == 0.0),
        "unknown source adds nothing"
    );
}

// ---------------------------------------------------------------------------
// #171: one locked evicted file does not hold up the others (Windows only: the
// lock is an open handle without FILE_SHARE_DELETE, which blocks deletion)
// ---------------------------------------------------------------------------

/// Opens `path` so that it cannot be deleted while the handle is open.
#[cfg(windows)]
fn open_locked(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    /// FILE_SHARE_READ only. FILE_SHARE_DELETE is left out on purpose.
    const FILE_SHARE_READ: u32 = 0x1;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .expect("open locked segment")
}

/// Sequence number of the oldest segment still in the ring, if any.
#[cfg(windows)]
fn oldest_seq(ring: &Arc<Mutex<RingIndex>>) -> Option<u64> {
    ring.lock().expect("ring").iter().next().map(|m| m.seq)
}

/// Places and polls segments until the oldest ring entry is past `seq`. Returns
/// the next unused segment number.
#[cfg(windows)]
fn fill_until_oldest_passes(
    tracker: &mut SegmentTracker,
    dir: &Path,
    ring: &Arc<Mutex<RingIndex>>,
    mut next: u64,
    seq: u64,
    clip: &[u8],
) -> u64 {
    for _ in 0..40 {
        if oldest_seq(ring).is_some_and(|s| s > seq) {
            return next;
        }
        place_seg(dir, next, clip);
        tracker.poll().expect("poll");
        next += 1;
    }
    panic!("oldest ring seq never passed {seq}");
}

#[cfg(windows)]
#[test]
fn issue_171_locked_evicted_file_does_not_block_other_deletes() {
    let dir = TempDir::new().expect("tempdir");
    let Some(clip) = real_clip() else {
        eprintln!("SKIPPED: lock test: native test media unavailable");
        return;
    };
    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3.0, Arc::clone(&ring));

    place_seg(dir.path(), 0, clip);
    let locked = open_locked(&seg_path(dir.path(), 0));
    // Seg 0 is evicted while it is locked. The next evicted file is not locked.
    let next = fill_until_oldest_passes(&mut tracker, dir.path(), &ring, 1, 0, clip);
    assert!(
        seg_path(dir.path(), 0).exists(),
        "locked file stays on disk"
    );

    fill_until_oldest_passes(&mut tracker, dir.path(), &ring, next, 1, clip);
    assert!(
        !seg_path(dir.path(), 1).exists(),
        "seg 1 is deletable and must go although seg 0 is still locked"
    );
    assert!(seg_path(dir.path(), 0).exists(), "seg 0 still locked");

    drop(locked);
    tracker.poll().expect("poll after unlock");
    assert!(
        !seg_path(dir.path(), 0).exists(),
        "seg 0 is deleted on a later poll once unlocked"
    );
}

#[cfg(windows)]
#[test]
fn issue_171_undeletable_file_is_abandoned_after_the_retry_limit() {
    let dir = TempDir::new().expect("tempdir");
    let Some(clip) = real_clip() else {
        eprintln!("SKIPPED: lock test: native test media unavailable");
        return;
    };
    let ring = new_ring();
    let mut tracker = SegmentTracker::new(dir.path(), 3.0, Arc::clone(&ring));

    place_seg(dir.path(), 0, clip);
    let locked = open_locked(&seg_path(dir.path(), 0));
    fill_until_oldest_passes(&mut tracker, dir.path(), &ring, 1, 0, clip);

    // Each poll retries the deletion. Six polls exceed the retry limit of five
    // attempts, counting the attempt made at eviction.
    for _ in 0..6 {
        tracker.poll().expect("poll while locked");
    }
    drop(locked);
    tracker.poll().expect("poll after unlock");
    assert!(
        seg_path(dir.path(), 0).exists(),
        "after the retry limit the file is no longer retried and stays on disk"
    );
}
