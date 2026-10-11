//! Black-box tests for `clipcheck_clipedit::run` (decision D26).
//!
//! Fixtures are generated natively with `clipcheck-testkit` and probed with
//! `clipcheck-mp4`. If the testkit cannot open its encoders the tests print
//! `SKIPPED: native test media unavailable` and return. All files live under
//! the OS temp dir.

use clipcheck_clipedit::run::{run_trim, TrimProgress, TrimRequest, TrimRunError};
use clipcheck_clipedit::{plan_trim, TrimInput, TrimMode, TrimPlan};
use clipcheck_mp4::reader::{read_movie, Movie, Track, TrackKind};
use clipcheck_testkit::{write_mp4, AudioSpec, MediaSpec};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SKIP_MSG: &str = "SKIPPED: native test media unavailable";

// ---------------------------------------------------------------------------
// Probing (clipcheck-mp4 reader) and fixtures
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq)]
struct StreamInfo {
    codec_type: String,
    codec_name: String,
    title: Option<String>,
}

fn read(file: &Path) -> Movie {
    read_movie(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
}

fn first_video(m: &Movie) -> &Track {
    m.tracks
        .iter()
        .find(|t| matches!(t.kind, TrackKind::Video { .. }))
        .expect("video track")
}

/// Presentation time of a sample in ms.
fn pts_ms(t: &Track, dts: i64, cts_offset: i32) -> i64 {
    ((dts + i64::from(cts_offset)) as f64 * 1000.0 / f64::from(t.timescale)).round() as i64
}

fn probe_streams(file: &Path) -> Vec<StreamInfo> {
    read(file)
        .tracks
        .iter()
        .map(|t| {
            let (codec_type, codec_name) = match &t.kind {
                TrackKind::Video { codec, .. } => ("video", codec.clone()),
                TrackKind::Audio { codec, .. } => ("audio", codec.clone()),
                TrackKind::Other { handler } => ("other", handler.clone()),
            };
            StreamInfo {
                codec_type: codec_type.to_string(),
                codec_name,
                title: t.title.clone(),
            }
        })
        .collect()
}

/// Keyframe presentation times in ms, ascending.
fn keyframes_ms(file: &Path) -> Vec<i64> {
    let m = read(file);
    let v = first_video(&m);
    v.samples
        .iter()
        .filter(|s| s.keyframe)
        .map(|s| pts_ms(v, s.dts, s.cts_offset))
        .collect()
}

fn duration_ms(file: &Path) -> i64 {
    (read(file).duration_secs * 1000.0).round() as i64
}

/// (pts seconds, key_frame flag) of the first video sample.
fn first_video_frame(file: &Path) -> (f64, bool) {
    let m = read(file);
    let v = first_video(&m);
    let s = v.samples.first().expect("first video sample");
    (pts_ms(v, s.dts, s.cts_offset) as f64 / 1000.0, s.keyframe)
}

struct Fixtures {
    src: PathBuf,
    keyframes: Vec<i64>,
    duration_ms: i64,
}

static FIXTURES: OnceLock<Option<Fixtures>> = OnceLock::new();
static LONG_SRC: OnceLock<(PathBuf, Vec<i64>, i64)> = OnceLock::new();

fn fixture_root() -> PathBuf {
    std::env::temp_dir()
        .join(format!("clipedit-run-tests-{}", std::process::id()))
        .join("fixtures")
}

fn build_fixtures() -> Option<Fixtures> {
    if !clipcheck_testkit::available() {
        eprintln!("{SKIP_MSG}");
        return None;
    }
    let root = fixture_root();
    fs::create_dir_all(&root).expect("create fixture dir");
    let src = root.join("src.mp4");
    let spec = MediaSpec::h264_aac(10.0)
        .with_video(320, 180, 30)
        .with_audio(vec![
            AudioSpec::titled(440.0, "Mix"),
            AudioSpec::titled(880.0, "Mic"),
        ]);
    write_mp4(&src, &spec).expect("generate 10 s source");
    let keyframes = keyframes_ms(&src);
    let duration_ms = duration_ms(&src);
    Some(Fixtures {
        src,
        keyframes,
        duration_ms,
    })
}

fn fixtures() -> Option<&'static Fixtures> {
    FIXTURES.get_or_init(build_fixtures).as_ref()
}

fn long_source() -> &'static (PathBuf, Vec<i64>, i64) {
    LONG_SRC.get_or_init(|| {
        let root = fixture_root();
        fs::create_dir_all(&root).expect("create fixture dir");
        let path = root.join("long60.mp4");
        let spec = MediaSpec::h264_aac(60.0).with_video(640, 360, 30);
        write_mp4(&path, &spec).expect("generate 60 s source");
        let kf = keyframes_ms(&path);
        let dur = duration_ms(&path);
        (path, kf, dur)
    })
}

// ---------------------------------------------------------------------------
// Scratch directories and helpers
// ---------------------------------------------------------------------------

/// Per-test temp directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir()
            .join(format!("clipedit-run-tests-{}", std::process::id()))
            .join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("create scratch dir");
        Scratch(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn part_of(dst: &Path) -> PathBuf {
    let mut s = dst.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn lossless_plan(keyframes: &[i64], duration: i64) -> TrimPlan {
    plan_trim(&TrimInput {
        keyframes_ms: keyframes,
        duration_ms: duration,
        start_ms: 2500,
        end_ms: 6200,
        mode: TrimMode::Lossless,
    })
    .expect("lossless plan")
}

fn precise_plan(keyframes: &[i64], duration: i64) -> TrimPlan {
    plan_trim(&TrimInput {
        keyframes_ms: keyframes,
        duration_ms: duration,
        start_ms: 2500,
        end_ms: 6200,
        mode: TrimMode::Precise,
    })
    .expect("precise plan")
}

// ---------------------------------------------------------------------------
// 1. Lossless trim
// ---------------------------------------------------------------------------

#[test]
fn t1_lossless_trim_preserves_streams_and_snaps_to_keyframe() {
    let Some(fx) = fixtures() else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    let scratch = Scratch::new("t1_lossless");
    let dst = scratch.path().join("out.mp4");
    let plan = lossless_plan(&fx.keyframes, fx.duration_ms);
    assert_eq!(plan.actual_start_ms, 2000, "planner should snap to 2000 ms");

    let before = fs::read(&fx.src).expect("read src");
    let req = TrimRequest {
        src: &fx.src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
    assert_eq!(res, Ok(()), "lossless trim should succeed");

    assert!(dst.is_file(), "output should exist");
    assert!(!part_of(&dst).exists(), ".part should be gone");

    let streams = probe_streams(&dst);
    let video = streams.iter().filter(|s| s.codec_type == "video").count();
    let audio: Vec<&StreamInfo> = streams.iter().filter(|s| s.codec_type == "audio").collect();
    assert_eq!(video, 1, "one video stream expected, got {streams:?}");
    assert_eq!(
        audio.len(),
        2,
        "two audio streams expected, got {streams:?}"
    );
    assert_eq!(audio[0].title.as_deref(), Some("Mix"), "audio 0 title");
    assert_eq!(audio[1].title.as_deref(), Some("Mic"), "audio 1 title");

    let dur = duration_ms(&dst);
    let expected = plan.output_duration_ms();
    assert!(
        dur >= expected - 100 && dur <= expected + 600,
        "duration {dur} ms not within [{}, {}]",
        expected - 100,
        expected + 600
    );

    let (_pts, key) = first_video_frame(&dst);
    assert!(
        key,
        "first video frame of lossless output should be a keyframe"
    );

    let after = fs::read(&fx.src).expect("re-read src");
    assert!(before == after, "source bytes changed during trim");
}

// ---------------------------------------------------------------------------
// 2. Precise trim
// ---------------------------------------------------------------------------

#[test]
fn t2_precise_trim_duration_and_audio_copy() {
    let Some(fx) = fixtures() else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    let scratch = Scratch::new("t2_precise");
    let dst = scratch.path().join("out.mp4");
    let plan = precise_plan(&fx.keyframes, fx.duration_ms);
    assert_eq!(plan.output_duration_ms(), 3700);

    let req = TrimRequest {
        src: &fx.src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
    assert_eq!(res, Ok(()), "precise trim should succeed");
    assert!(dst.is_file(), "output should exist");
    assert!(!part_of(&dst).exists(), ".part should be gone");

    let dur = duration_ms(&dst);
    assert!(
        (dur - 3700).abs() <= 150,
        "precise duration {dur} ms not within 3700 +/- 150"
    );

    let streams = probe_streams(&dst);
    let audio: Vec<&StreamInfo> = streams.iter().filter(|s| s.codec_type == "audio").collect();
    assert_eq!(
        audio.len(),
        2,
        "two audio streams expected, got {streams:?}"
    );
    for a in &audio {
        assert_eq!(a.codec_name, "aac", "audio should be copied as aac");
    }
    if audio.iter().any(|a| a.title.is_none()) {
        eprintln!("OBSERVATION: precise output audio titles: {:?}", audio);
    }

    let (pts, _key) = first_video_frame(&dst);
    assert!(pts < 0.1, "first frame pts {pts} s should be near 0");
}

// ---------------------------------------------------------------------------
// 3. Progress
// ---------------------------------------------------------------------------

#[test]
fn t3_progress_is_monotonic_and_ends_at_100() {
    let Some(fx) = fixtures() else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    let scratch = Scratch::new("t3_progress");
    let dst = scratch.path().join("out.mp4");
    let plan = precise_plan(&fx.keyframes, fx.duration_ms);
    let req = TrimRequest {
        src: &fx.src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let mut percents: Vec<f32> = Vec::new();
    let mut out_times: Vec<i64> = Vec::new();
    let mut cb = |p: TrimProgress| {
        percents.push(p.percent);
        out_times.push(p.out_time_ms);
    };
    let res = run_trim(&req, &cancel, &mut cb);
    assert_eq!(res, Ok(()), "trim should succeed");

    assert!(!percents.is_empty(), "progress callback never invoked");
    for p in &percents {
        assert!((0.0..=100.0).contains(p), "percent {p} out of 0..=100");
    }
    for w in percents.windows(2) {
        assert!(w[0] <= w[1], "percent decreased: {} -> {}", w[0], w[1]);
    }
    assert_eq!(
        *percents.last().unwrap(),
        100.0,
        "final callback should be 100"
    );
    eprintln!(
        "OBSERVATION: {} progress callbacks, out_time_ms {:?}",
        percents.len(),
        out_times
    );
}

// ---------------------------------------------------------------------------
// 4. Cancel
// ---------------------------------------------------------------------------

#[test]
fn t4_cancel_kills_long_job_quickly_and_cleans_up() {
    if fixtures().is_none() {
        eprintln!("{SKIP_MSG}");
        return;
    }
    let (long_src, long_kf, long_dur) = long_source();
    let scratch = Scratch::new("t4_cancel");
    let dst = scratch.path().join("cancelled.mp4");
    let plan = plan_trim(&TrimInput {
        keyframes_ms: long_kf,
        duration_ms: *long_dur,
        start_ms: 5000,
        end_ms: 55000,
        mode: TrimMode::Precise,
    })
    .expect("long plan");
    let req = TrimRequest {
        src: long_src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let mut set_at: Option<Instant> = None;
    let mut cb = |_p: TrimProgress| {
        if set_at.is_none() {
            cancel.store(true, Ordering::SeqCst);
            set_at = Some(Instant::now());
        }
    };
    let res = run_trim(&req, &cancel, &mut cb);
    let elapsed = set_at.map(|t| t.elapsed());

    let elapsed = elapsed.expect("cancel flag was never set (no progress before job ended?)");
    assert_eq!(res, Err(TrimRunError::Cancelled), "expected Cancelled");
    assert!(
        elapsed < Duration::from_millis(1000),
        "cancel took {elapsed:?} after the flag was set"
    );
    eprintln!("OBSERVATION: cancel returned {elapsed:?} after flag set");
    assert!(!part_of(&dst).exists(), ".part should be removed");
    assert!(!dst.exists(), "dst should be absent");
}

// ---------------------------------------------------------------------------
// 5. Error paths
// ---------------------------------------------------------------------------

fn dummy_plan() -> TrimPlan {
    TrimPlan {
        mode: TrimMode::Lossless,
        requested_start_ms: 0,
        requested_end_ms: 1000,
        actual_start_ms: 0,
        actual_end_ms: 1000,
        snap_delta_ms: 0,
        reencode_from_keyframe_ms: None,
        discard_head_ms: 0,
    }
}

#[test]
fn t5a_existing_dst_is_refused_and_untouched() {
    let scratch = Scratch::new("t5a_exists");
    let dst = scratch.path().join("out.mp4");
    fs::write(&dst, b"keep me").unwrap();
    let plan = dummy_plan();
    let src = scratch.path().join("src.mp4");
    fs::write(&src, b"src bytes").unwrap();
    let req = TrimRequest {
        src: &src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
    match res {
        Err(TrimRunError::Io(msg)) => assert!(
            msg.to_lowercase().contains("exists"),
            "Io message should mention 'exists': {msg}"
        ),
        other => panic!("expected Err(Io(..)), got {other:?}"),
    }
    assert_eq!(fs::read(&dst).unwrap(), b"keep me", "dst modified");
    assert!(!part_of(&dst).exists(), ".part should not be created");
    assert_eq!(fs::read(&src).unwrap(), b"src bytes", "src modified");
}

#[test]
fn t5b_bad_plans_are_rejected_without_output() {
    let scratch = Scratch::new("t5b_badplan");
    let dst = scratch.path().join("out.mp4");
    let src = scratch.path().join("src.mp4");
    fs::write(&src, b"x").unwrap();

    let mut zero = dummy_plan();
    zero.actual_end_ms = zero.actual_start_ms;
    zero.requested_end_ms = zero.requested_start_ms;

    let mut negative = dummy_plan();
    negative.actual_start_ms = 2000;
    negative.actual_end_ms = 1000;

    for (label, plan) in [("zero", zero), ("negative", negative)] {
        let req = TrimRequest {
            src: &src,
            dst: &dst,
            plan: &plan,
        };
        let cancel = AtomicBool::new(false);
        let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
        assert!(
            matches!(res, Err(TrimRunError::BadPlan(_))),
            "{label} duration: expected BadPlan, got {res:?}"
        );
        assert!(!dst.exists(), "{label}: dst created");
        assert!(!part_of(&dst).exists(), "{label}: .part created");
    }
}

#[test]
fn t5c_missing_src_is_an_error_and_leaves_no_part() {
    let Some(fx) = fixtures() else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    let scratch = Scratch::new("t5c_missing");
    let dst = scratch.path().join("out.mp4");
    let missing = scratch.path().join("does-not-exist.mp4");
    let plan = lossless_plan(&fx.keyframes, fx.duration_ms);
    let req = TrimRequest {
        src: &missing,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
    match &res {
        Err(TrimRunError::Io(_)) => {}
        other => panic!("expected Err(Io), got {other:?}"),
    }
    eprintln!("OBSERVATION: missing src gave {res:?}");
    assert!(!part_of(&dst).exists(), ".part should be removed");
    assert!(!dst.exists(), "dst should be absent");
}

#[test]
fn t5e_dst_in_nonexistent_directory_is_error() {
    let Some(fx) = fixtures() else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    let scratch = Scratch::new("t5e_nodir");
    let dst = scratch.path().join("no such dir").join("out.mp4");
    let plan = lossless_plan(&fx.keyframes, fx.duration_ms);
    let req = TrimRequest {
        src: &fx.src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
    assert!(res.is_err(), "expected Err, got Ok");
    eprintln!("OBSERVATION: nonexistent dst dir gave {res:?}");
    assert!(!dst.exists(), "dst should be absent");
}

// ---------------------------------------------------------------------------
// 7. Unicode and spaces in paths
// ---------------------------------------------------------------------------

#[test]
fn t7_paths_with_spaces_and_unicode_work() {
    let Some(fx) = fixtures() else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    let scratch = Scratch::new("t7_unicode");
    let src_dir = scratch.path().join("sp ace \u{e9} \u{65e5}\u{672c}");
    let dst_dir = scratch.path().join("out dir \u{f1}");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dst_dir).unwrap();
    let src = src_dir.join("src file \u{fc}.mp4");
    fs::copy(&fx.src, &src).expect("copy source");
    let dst = dst_dir.join("clip 1 \u{fc}.mp4");

    let plan = lossless_plan(&fx.keyframes, fx.duration_ms);
    let req = TrimRequest {
        src: &src,
        dst: &dst,
        plan: &plan,
    };
    let cancel = AtomicBool::new(false);
    let res = run_trim(&req, &cancel, &mut |_: TrimProgress| {});
    assert_eq!(res, Ok(()), "unicode/space paths should succeed");
    assert!(dst.is_file(), "output should exist");
    assert!(!part_of(&dst).exists(), ".part should be gone");
    let streams = probe_streams(&dst);
    assert_eq!(streams.len(), 3, "expected 3 streams, got {streams:?}");
}
