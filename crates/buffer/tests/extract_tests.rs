//! Integration tests for `clipcheck_buffer::extract`.
//!
//! The source video is generated once per process with `clipcheck-testkit`
//! into a shared fixture directory. Each test copies the segments it needs into
//! its own tempdir, so tests never write into the shared copies. Clip checks
//! read the output back with `clipcheck-mp4`.

mod common;

use clipcheck_buffer::index::{ClipPlan, RingIndex, SegmentMeta, TICKS_PER_SEC};
use clipcheck_buffer::{build_concat_list, extract_clip};
use clipcheck_mp4::reader::{read_movie, TrackKind};
use clipcheck_testkit::{write_segments, MediaSpec};
use common::{copy_file, segment_meta, shared_fixture_dir};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SEG_COUNT: u64 = 5;
const NOW_100NS: i64 = 10 * TICKS_PER_SEC;

/// Segments cut from one generated source video.
struct Source {
    segs: Vec<PathBuf>,
}

/// Returns the shared 10 s source, generating it on first use in this process.
fn shared_source() -> &'static Source {
    static SOURCE: OnceLock<Source> = OnceLock::new();
    SOURCE.get_or_init(generate_source)
}

/// Writes a 10 s 1280x720 30 fps H.264 + AAC video as five 2 s
/// keyframe-aligned segments in a shared fixture directory.
fn generate_source() -> Source {
    let dir = shared_fixture_dir("extract-src");
    let spec = MediaSpec::h264_aac(10.0).with_video(1280, 720, 30);
    let segs =
        write_segments(&dir, &spec, 2.0).unwrap_or_else(|e| panic!("generate test segments: {e}"));
    assert_eq!(
        segs.len() as u64,
        SEG_COUNT,
        "expected {SEG_COUNT} segments, got {}",
        segs.len()
    );
    for s in &segs {
        assert!(
            s.is_file(),
            "segment missing after generation: {}",
            s.display()
        );
    }
    Source { segs }
}

struct Fixture {
    dir: tempfile::TempDir,
    segs: Vec<PathBuf>,
    index: RingIndex,
}

impl Fixture {
    fn clips_dir(&self) -> PathBuf {
        let d = self.dir.path().join("clips");
        fs::create_dir_all(&d).expect("create clips dir");
        d
    }
}

/// Five-segment fixture with a `RingIndex` over the copies.
fn fixture() -> Fixture {
    let src = shared_source();
    let dir = tempfile::tempdir().expect("tempdir");
    let segs: Vec<PathBuf> = src.segs.iter().map(|s| copy_file(s, dir.path())).collect();

    let mut index = RingIndex::new();
    for (i, p) in segs.iter().enumerate() {
        index.push(segment_meta(i as u64, p)).expect("push segment");
    }

    Fixture { dir, segs, index }
}

/// Asserts that `dir` holds no `*.tmp.mp4` or `*.concat.txt` leftovers.
fn assert_no_leftovers(dir: &Path) {
    for entry in fs::read_dir(dir).expect("read output dir") {
        let name = entry
            .expect("dir entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(
            !name.ends_with(".tmp.mp4") && !name.ends_with(".concat.txt"),
            "leftover file in {}: {name}",
            dir.display()
        );
    }
}

/// Extracts `plan` to `out` and checks the full set of clip assertions.
fn assert_valid_clip(plan: &ClipPlan, out: &Path) {
    let report = extract_clip(plan, out)
        .unwrap_or_else(|e| panic!("extract_clip failed for {}: {e}", out.display()));

    assert_eq!(report.out, out, "report.out mismatch");
    assert_eq!(
        report.duration_secs_expected, plan.effective_secs,
        "duration_secs_expected must equal plan.effective_secs"
    );
    assert!(out.is_file(), "clip not written: {}", out.display());
    let len = fs::metadata(out).expect("clip metadata").len();
    assert!(len > 0, "clip is empty");
    assert_eq!(report.bytes, len, "report.bytes must equal file length");

    let movie = read_movie(out).unwrap_or_else(|e| panic!("read clip {}: {e}", out.display()));
    let dur = movie.duration_secs;
    assert!(
        (dur - plan.effective_secs).abs() <= 0.25,
        "duration {dur} differs from effective_secs {} by more than 0.25 ({})",
        plan.effective_secs,
        out.display()
    );

    let video = movie
        .tracks
        .iter()
        .find(|t| t.is_video())
        .unwrap_or_else(|| panic!("no video track in {}", out.display()));
    match &video.kind {
        TrackKind::Video { codec, .. } => assert_eq!(codec, "h264", "video codec"),
        other => panic!("video track has kind {other:?}"),
    }
    let first = video
        .samples
        .first()
        .unwrap_or_else(|| panic!("no video samples in {}", out.display()));
    assert!(
        first.keyframe,
        "first video packet is not a keyframe ({})",
        out.display()
    );

    assert_no_leftovers(out.parent().expect("out has parent"));
}

#[test]
fn clip_4s_is_playable_and_starts_on_keyframe() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let fx = fixture();
    let plan = fx.index.plan_clip(NOW_100NS, 4, 0).expect("plan 4s");
    assert!(!plan.short, "4s plan should not be short");
    assert!(
        plan.effective_secs >= 4.0,
        "effective {} < 4.0",
        plan.effective_secs
    );
    let out = fx.clips_dir().join("clip_4.mp4");
    assert_valid_clip(&plan, &out);
}

#[test]
fn clip_6s_is_playable_and_starts_on_keyframe() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let fx = fixture();
    let plan = fx.index.plan_clip(NOW_100NS, 6, 0).expect("plan 6s");
    assert!(!plan.short, "6s plan should not be short");
    assert!(
        plan.effective_secs >= 6.0,
        "effective {} < 6.0",
        plan.effective_secs
    );
    let out = fx.clips_dir().join("clip_6.mp4");
    assert_valid_clip(&plan, &out);
}

#[test]
fn clip_30s_clamps_to_buffer_and_is_short() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let fx = fixture();
    let plan = fx.index.plan_clip(NOW_100NS, 30, 0).expect("plan 30s");
    assert!(plan.short, "30s plan from a 10s buffer must be short");
    assert_eq!(
        plan.trim_start_offset_100ns, 0,
        "earliest keyframe clamp expected"
    );
    let out = fx.clips_dir().join("clip_30.mp4");
    assert_valid_clip(&plan, &out);
}

#[test]
fn empty_plan_is_invalid_input() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let fx = fixture();
    let plan = ClipPlan {
        segments: vec![],
        trim_start_offset_100ns: 0,
        effective_secs: 0.0,
        short: false,
        has_gap: false,
    };
    let out = fx.clips_dir().join("clip_empty.mp4");
    let err = extract_clip(&plan, &out).expect_err("empty plan must fail");
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "got {err}");
    assert_no_leftovers(&fx.clips_dir());
    assert!(!out.exists(), "no output expected for empty plan");
}

#[test]
fn missing_segment_is_not_found_and_names_path() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let fx = fixture();
    let plan = fx.index.plan_clip(NOW_100NS, 6, 0).expect("plan 6s");
    let missing = fx.segs[2].clone();
    assert!(
        plan.segments.iter().any(|s| s.path == missing),
        "test precondition: missing segment must be in plan"
    );
    fs::remove_file(&missing).expect("delete segment");

    let out = fx.clips_dir().join("clip_missing.mp4");
    let err = extract_clip(&plan, &out).expect_err("missing segment must fail");
    assert_eq!(err.kind(), ErrorKind::NotFound, "got {err}");
    let msg = err.to_string();
    assert!(
        msg.contains(&missing.display().to_string()),
        "error {msg:?} does not name {}",
        missing.display()
    );
    assert_no_leftovers(&fx.clips_dir());
    assert!(!out.exists(), "no output expected on missing segment");
}

/// A garbage (non-MP4) segment makes the native remuxer fail with InvalidData.
/// No output, no `.part`, `.tmp.mp4` or concat list is left in the output dir.
#[test]
fn garbage_segment_is_invalid_data_and_leaves_no_output() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let fx = fixture();
    let bad = fx.dir.path().join("bad.mp4");
    let garbage = vec![0xABu8; 4096];
    fs::write(&bad, &garbage).expect("write garbage");
    let plan = ClipPlan {
        segments: vec![SegmentMeta {
            seq: 0,
            start_100ns: 0,
            end_100ns: 2 * TICKS_PER_SEC,
            first_keyframe_100ns: 0,
            bytes: garbage.len() as u64,
            path: bad.clone(),
            committed: true,
        }],
        trim_start_offset_100ns: 0,
        effective_secs: 2.0,
        short: true,
        has_gap: false,
    };

    let clips = fx.clips_dir();
    let out = clips.join("clip_bad.mp4");
    let err = extract_clip(&plan, &out).expect_err("garbage input must fail");
    assert_eq!(err.kind(), ErrorKind::InvalidData, "got {err:?}");
    assert!(!out.exists(), "clip_bad.mp4 must not exist after failure");
    assert!(
        !clips.join("clip_bad.mp4.tmp.mp4").exists(),
        "tmp file left behind"
    );
    assert!(
        !clips.join("clip_bad.mp4.part").exists(),
        "part file left behind"
    );
    let remaining: Vec<_> = fs::read_dir(&clips)
        .expect("read clips dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert!(remaining.is_empty(), "clips dir not empty: {remaining:?}");
    assert_no_leftovers(&clips);
}

#[test]
fn build_concat_list_escapes_windows_path_and_quote() {
    let list = build_concat_list(&[PathBuf::from(r"C:\a b\it's.mp4")]);
    assert_eq!(list, "file 'C:/a b/it'\\''s.mp4'\n");
}
