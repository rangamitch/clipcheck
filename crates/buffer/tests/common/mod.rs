//! Helpers shared by the buffer integration tests. Fixtures come from
//! `clipcheck-testkit` (native H.264/AAC generation) and are read back with
//! `clipcheck-mp4`. Each test binary that declares `mod common;` compiles its
//! own copy, and no binary uses every helper, hence the crate-level `dead_code`
//! allowance.
#![allow(dead_code)]

use clipcheck_buffer::index::{ClipPlan, SegmentMeta, TICKS_PER_SEC};
use std::fs;
use std::path::{Path, PathBuf};

/// Length in seconds of every segment described by [`segment_meta`].
pub const SEG_SECS: i64 = 2;

/// Index entry for the segment at `path`, taken as the `seq`-th
/// [`SEG_SECS`]-long keyframe-aligned segment, starting at `seq * SEG_SECS`.
/// `bytes` is read from the file, so the file must exist.
pub fn segment_meta(seq: u64, path: &Path) -> SegmentMeta {
    let i = seq as i64;
    SegmentMeta {
        seq,
        start_100ns: SEG_SECS * i * TICKS_PER_SEC,
        end_100ns: SEG_SECS * (i + 1) * TICKS_PER_SEC,
        first_keyframe_100ns: SEG_SECS * i * TICKS_PER_SEC,
        bytes: fs::metadata(path)
            .unwrap_or_else(|e| panic!("segment metadata {}: {e}", path.display()))
            .len(),
        path: path.to_path_buf(),
        committed: true,
    }
}

/// Untrimmed `ClipPlan` over `segs` in order, each [`SEG_SECS`] long from 0,
/// with the given `effective_secs`.
pub fn plan_of(segs: &[PathBuf], effective_secs: f64) -> ClipPlan {
    ClipPlan {
        segments: segs
            .iter()
            .enumerate()
            .map(|(i, p)| segment_meta(i as u64, p))
            .collect(),
        trim_start_offset_100ns: 0,
        effective_secs,
        short: false,
        has_gap: false,
    }
}

/// Copies `src` into `dest_dir` under the same file name and returns the new
/// path. Used to give each test its own writable copy of a shared fixture.
pub fn copy_file(src: &Path, dest_dir: &Path) -> PathBuf {
    let name = src
        .file_name()
        .unwrap_or_else(|| panic!("no file name in {}", src.display()));
    let dest = dest_dir.join(name);
    fs::copy(src, &dest)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dest.display()));
    dest
}

/// Directory for fixtures shared by every test in one test binary. It sits in
/// Cargo's per-target temporary directory and is regenerated once per process.
pub fn shared_fixture_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("create fixture dir {}: {e}", dir.display()));
    dir
}
