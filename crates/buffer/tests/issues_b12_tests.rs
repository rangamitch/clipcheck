//! Regression tests for clipcheck-buffer issue #53.
//!
//! Windows only: the deletion test uses a Windows share mode.
//! Nothing touches the user's recordings or the real ClipCheck install.

#![cfg(windows)]

use clipcheck_buffer::{SegmentDir, SegmentMeta};
use std::fs;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;

// ---- shared helpers -------------------------------------------------------

/// Writes `data` as a committed segment `seq` and returns its final path.
fn write_committed(dir: &SegmentDir, seq: u64, data: &[u8]) -> PathBuf {
    let mut w = dir.open_segment(seq).expect("open_segment");
    w.write_all(data).expect("write");
    w.commit().expect("commit");
    dir.segment_path(seq)
}

fn meta_at(seq: u64, path: PathBuf) -> SegmentMeta {
    SegmentMeta {
        seq,
        start_100ns: 0,
        end_100ns: 0,
        first_keyframe_100ns: 0,
        bytes: 0,
        path,
        committed: true,
    }
}

// ---- #53: delete_evicted attempts every file ------------------------------

#[test]
fn issue_53_delete_evicted_attempts_every_file_after_an_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = SegmentDir::new(tmp.path());
    let locked = write_committed(&dir, 0, b"a");
    let p1 = write_committed(&dir, 1, b"b");
    let p2 = write_committed(&dir, 2, b"c");
    let evicted = vec![
        meta_at(0, locked.clone()),
        meta_at(1, p1.clone()),
        meta_at(2, p2.clone()),
    ];

    let hold = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&locked)
        .expect("hold segment 0 open");

    assert!(dir.delete_evicted(&evicted).is_err());
    assert!(!p1.exists(), "segment 1 must be removed despite the error");
    assert!(!p2.exists(), "segment 2 must be removed despite the error");

    drop(hold);
    assert_eq!(dir.delete_evicted(&evicted[..1]).expect("retry"), 1);
}
