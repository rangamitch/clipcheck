//! Black-box tests for `clipcheck_render::engine::run_export` argument validation and the still
//! image path. Synthetic projects only: a single solid-colour clip (no media, no encoder for the
//! still path). Temp dirs come from `tempfile`. Only the one ignored test needs a real MP4.
#![cfg(windows)]

use clipcheck_render::engine::{run_export, ExportReport, ExportRequest};
use clipcheck_render::job::JobControl;
use clipcheck_render::still;
use clipcheck_render::RenderError;
use clipcheck_timeline::export::{
    ExportKind, ExportRange, ExportSettings, QualityMode, VideoCodec,
};
use clipcheck_timeline::model::{Clip, ClipKind, MediaKind, MediaRef, Project, Rgba, TrackKind};
use clipcheck_timeline::time::{Rate, Tick};
use std::path::{Path, PathBuf};

/// Canvas and output size of the still tests (even, at least 128).
const SIZE: u32 = 256;
const SIZE_H: u32 = 144;

/// A project with a 1 s solid red clip on the first video track and a canvas of `w` x `h`.
fn solid_project(w: u32, h: u32) -> Project {
    let mut p = Project::new("engine-test");
    p.canvas.width = w;
    p.canvas.height = h;
    let track_id = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .map(|t| t.id.clone())
        .unwrap_or_default();
    p.clips.push(Clip {
        id: "solid".to_string(),
        track_id,
        kind: ClipKind::Solid,
        name: "solid".to_string(),
        start: Tick::ZERO,
        dur: Tick::from_secs(1),
        solid: Some(Rgba {
            r: 220,
            g: 30,
            b: 30,
            a: 255,
        }),
        ..Clip::default()
    });
    p
}

/// Still-image settings at the test size.
fn still_settings(format: &str, quality: u32) -> ExportSettings {
    ExportSettings {
        width: SIZE,
        height: SIZE_H,
        kind: ExportKind::Still {
            format: format.to_string(),
            quality,
            at: Tick::ZERO,
        },
        ..ExportSettings::default()
    }
}

/// `<path>.part`, the temp name the engine writes before the rename.
fn part_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Runs an export with a fresh cancel flag and no progress reporting.
fn run(
    project: &Project,
    settings: &ExportSettings,
    out: &Path,
    work: &Path,
) -> Result<ExportReport, RenderError> {
    let ctl = JobControl::new();
    let req = ExportRequest {
        project,
        settings,
        out_path: out,
        work_dir: work,
        ctl: &ctl,
    };
    run_export(&req, &mut |_| {})
}

/// Fresh temp dir with `out.<ext>` and `work` inside it (neither created).
fn layout(ext: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join(format!("out.{ext}"));
    let work = dir.path().join("work");
    (dir, out, work)
}

#[test]
fn existing_out_path_is_output_error_and_untouched() {
    let (_dir, out, work) = layout("png");
    std::fs::write(&out, b"keep me").expect("seed existing output");
    let res = run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("png", 90),
        &out,
        &work,
    );
    assert!(
        matches!(res, Err(RenderError::Output(_))),
        "expected Output error, got {res:?}"
    );
    assert_eq!(std::fs::read(&out).expect("read seeded file"), b"keep me");
    assert!(!part_of(&out).exists());
}

#[test]
fn audio_only_is_unsupported() {
    let (_dir, out, work) = layout("m4a");
    let s = ExportSettings {
        kind: ExportKind::AudioOnly,
        ..ExportSettings::default()
    };
    let res = run(&solid_project(1280, 720), &s, &out, &work);
    assert!(
        matches!(res, Err(RenderError::Unsupported(_))),
        "expected Unsupported, got {res:?}"
    );
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
    assert!(
        !work.exists(),
        "work dir created by the call must be removed"
    );
}

#[test]
fn odd_width_is_invalid_and_leaves_nothing_on_disk() {
    let (_dir, out, work) = layout("mp4");
    let s = ExportSettings {
        width: 1281,
        ..ExportSettings::default()
    };
    let res = run(&solid_project(1280, 720), &s, &out, &work);
    assert!(
        matches!(res, Err(RenderError::Invalid(_))),
        "expected Invalid, got {res:?}"
    );
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
    assert!(!work.exists());
}

#[test]
fn width_below_minimum_is_invalid() {
    let (_dir, out, work) = layout("mp4");
    let s = ExportSettings {
        width: 64,
        height: 64,
        ..ExportSettings::default()
    };
    let res = run(&solid_project(1280, 720), &s, &out, &work);
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
}

#[test]
fn zero_quality_is_invalid() {
    let (_dir, out, work) = layout("mp4");
    let s = ExportSettings {
        quality: QualityMode::Quality { value: 0 },
        ..ExportSettings::default()
    };
    let res = run(&solid_project(1280, 720), &s, &out, &work);
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
}

#[test]
fn unknown_still_format_is_invalid() {
    let (_dir, out, work) = layout("bmp");
    let res = run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("bmp", 90),
        &out,
        &work,
    );
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
}

#[test]
fn still_quality_out_of_range_is_invalid() {
    let (_dir, out, work) = layout("jpg");
    let res = run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("jpg", 0),
        &out,
        &work,
    );
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
}

#[test]
fn video_export_of_empty_project_is_invalid() {
    let (_dir, out, work) = layout("mp4");
    let s = ExportSettings::default();
    let res = run(&Project::new("empty"), &s, &out, &work);
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
}

#[test]
fn selected_clips_that_do_not_exist_is_invalid() {
    let (_dir, out, work) = layout("mp4");
    let s = ExportSettings {
        range: ExportRange::SelectedClips {
            ids: vec!["ghost".to_string()],
        },
        ..ExportSettings::default()
    };
    let res = run(&solid_project(1280, 720), &s, &out, &work);
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
}

#[test]
fn still_png_has_signature_and_no_part_file() {
    let (_dir, out, work) = layout("png");
    let report = run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("png", 90),
        &out,
        &work,
    )
    .expect("png still export");
    let bytes = std::fs::read(&out).expect("read png");
    assert_eq!(
        bytes.get(..8),
        Some(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..]),
        "missing PNG signature"
    );
    assert!(!part_of(&out).exists(), ".part must be renamed away");
    assert_eq!(report.bytes, bytes.len() as u64);
}

#[test]
fn still_jpg_has_jpeg_start_marker() {
    let (_dir, out, work) = layout("jpg");
    run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("jpg", 85),
        &out,
        &work,
    )
    .expect("jpg still export");
    let bytes = std::fs::read(&out).expect("read jpg");
    assert_eq!(
        bytes.get(..3),
        Some(&[0xFF, 0xD8, 0xFF][..]),
        "missing JPEG SOI"
    );
    assert!(!part_of(&out).exists());
}

#[test]
fn still_report_describes_the_file() {
    let (_dir, out, work) = layout("png");
    let report = run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("png", 90),
        &out,
        &work,
    )
    .expect("png still export");
    assert_eq!(report.out_path, out);
    let len = std::fs::metadata(&out).expect("stat output").len();
    assert_eq!(report.bytes, len);
    assert_eq!(report.duration_secs, 0.0);
    assert!(!report.smart);
    assert_eq!(report.encoder, "WIC png");
    assert!(report.warnings.is_empty());
}

#[test]
fn still_work_dir_is_removed_when_the_export_created_it() {
    let (_dir, out, work) = layout("png");
    assert!(!work.exists());
    run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("png", 90),
        &out,
        &work,
    )
    .expect("png still export");
    assert!(
        !work.exists(),
        "work dir created by the export must be removed"
    );
}

#[test]
fn still_work_dir_is_kept_when_it_already_existed() {
    let (_dir, out, work) = layout("png");
    std::fs::create_dir_all(&work).expect("create work dir");
    std::fs::write(work.join("keep.txt"), b"x").expect("seed work dir");
    run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("png", 90),
        &out,
        &work,
    )
    .expect("png still export");
    assert!(
        work.join("keep.txt").exists(),
        "pre-existing work dir must be left alone"
    );
}

#[test]
fn still_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("a").join("b").join("frame.png");
    let work = dir.path().join("work");
    run(
        &solid_project(SIZE, SIZE_H),
        &still_settings("png", 90),
        &out,
        &work,
    )
    .expect("png still export into new folders");
    assert!(out.is_file());
}

#[test]
fn still_time_outside_the_timeline_is_invalid() {
    let (_dir, out, work) = layout("png");
    let mut s = still_settings("png", 90);
    s.kind = ExportKind::Still {
        format: "png".to_string(),
        quality: 90,
        at: Tick::from_secs(100),
    };
    let res = run(&solid_project(SIZE, SIZE_H), &s, &out, &work);
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
    assert!(!part_of(&out).exists());
}

#[test]
fn still_on_empty_project_is_invalid() {
    let (_dir, out, work) = layout("png");
    let res = run(
        &Project::new("empty"),
        &still_settings("png", 90),
        &out,
        &work,
    );
    assert!(matches!(res, Err(RenderError::Invalid(_))), "got {res:?}");
    assert!(!out.exists());
}

#[test]
fn video_cancelled_before_start_is_cancelled_and_clean() {
    let (_dir, out, work) = layout("mp4");
    let s = ExportSettings {
        width: SIZE,
        height: SIZE_H,
        codec: VideoCodec::H264,
        encoder: "software".to_string(),
        fps: Some(Rate::whole(30)),
        ..ExportSettings::default()
    };
    let project = solid_project(SIZE, SIZE_H);
    let ctl = JobControl::new();
    ctl.cancel();
    let req = ExportRequest {
        project: &project,
        settings: &s,
        out_path: &out,
        work_dir: &work,
        ctl: &ctl,
    };
    let res = run_export(&req, &mut |_| {});
    assert!(
        matches!(res, Err(RenderError::Cancelled)),
        "expected Cancelled, got {res:?}"
    );
    assert!(!out.exists(), "no output after cancel");
    assert!(!part_of(&out).exists(), "no .part after cancel");
    assert!(
        !work.exists(),
        "work dir created by the call must be removed"
    );
}

/// Real-media export: three cuts from one synthetic source, stream-copied (no re-encode).
///
/// Needs a source file that the test cannot generate itself: clipcheck-render has no
/// clipcheck-testkit dev-dependency. Point `CLIPCHECK_EXPORT_FIXTURE` at an H.264 + AAC MP4 that
/// is 640x360, 30 fps, keyframe at least every second, and at least 5 s long (for example one
/// made with `clipcheck_testkit::write_mp4` and a 640x360/30 spec). Run with
/// `cargo test -p clipcheck-render --test engine_tests -- --ignored`.
#[test]
#[ignore = "needs a real H.264+AAC fixture: set CLIPCHECK_EXPORT_FIXTURE (640x360 30 fps, 5 s+)"]
fn export_cuts_only_three_clips() {
    let Some(fixture) = std::env::var_os("CLIPCHECK_EXPORT_FIXTURE") else {
        eprintln!("SKIPPED: CLIPCHECK_EXPORT_FIXTURE is not set");
        return;
    };
    let src = PathBuf::from(fixture);
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("cuts.mp4");
    let work = dir.path().join("work");

    let mut p = Project::new("cuts");
    p.canvas.width = 640;
    p.canvas.height = 360;
    p.canvas.fps = Rate::whole(30);
    p.media.push(MediaRef {
        id: "m1".to_string(),
        kind: MediaKind::Video,
        name: "fixture.mp4".to_string(),
        path: src.to_string_lossy().into_owned(),
        duration: Tick::from_secs(5),
        width: 640,
        height: 360,
        fps: 30.0,
        video_codec: "h264".to_string(),
        ..MediaRef::default()
    });
    let track_id = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .map(|t| t.id.clone())
        .unwrap_or_default();
    // Three one-second cuts from source seconds 0, 2 and 4, placed back to back on V1.
    for (i, src_s) in [0i64, 2, 4].into_iter().enumerate() {
        p.clips.push(Clip {
            id: format!("cut{i}"),
            track_id: track_id.clone(),
            kind: ClipKind::Video,
            media_id: Some("m1".to_string()),
            name: format!("cut{i}"),
            start: Tick::from_secs(i as i64),
            dur: Tick::from_secs(1),
            src_in: Tick::from_secs(src_s),
            ..Clip::default()
        });
    }

    let settings = ExportSettings {
        width: 640,
        height: 360,
        fps: Some(Rate::whole(30)),
        codec: VideoCodec::H264,
        lossless: true,
        ..ExportSettings::default()
    };
    let eligibility = clipcheck_render::smart::check_eligibility(&p, &settings);
    assert!(
        eligibility.eligible,
        "three cuts must be eligible: {eligibility:?}"
    );

    let report = run(&p, &settings, &out, &work).expect("lossless export");
    assert!(report.smart, "the stream-copy path must be used");
    assert_eq!(report.encoder, "stream copy");
    assert!(
        (report.duration_secs - 3.0).abs() < 1e-6,
        "duration {} is not 3 s",
        report.duration_secs
    );
    assert!(report.bytes > 0);
    assert!(out.is_file());
    assert!(!part_of(&out).exists());
}

// ---------------------------------------------------------------------------------------------
// Cancellation of still exports and the still render path (no media, no encoder)
// ---------------------------------------------------------------------------------------------

#[test]
fn still_render_frame_with_cancelled_job_is_cancelled() {
    let p = solid_project(SIZE, SIZE_H);
    let ctl = JobControl::new();
    ctl.cancel();
    let res = still::render_frame_with(&p, Tick::ZERO, &ctl);
    assert!(matches!(res, Err(RenderError::Cancelled)));
}

#[test]
fn still_render_frame_draws_the_solid_clip_colour() {
    let p = solid_project(SIZE, SIZE_H);
    let frame = still::render_frame(&p, Tick::ZERO).expect("render");
    assert_eq!((frame.width, frame.height), (SIZE, SIZE_H));
    // BGRA: the clip colour is r 220, g 30, b 30.
    assert_eq!(frame.pixel(SIZE / 2, SIZE_H / 2), Some([30, 30, 220, 255]));
}

#[test]
fn still_export_with_precancelled_job_is_cancelled_and_leaves_nothing() {
    let (_dir, out, work) = layout("png");
    let p = solid_project(SIZE, SIZE_H);
    let s = still_settings("png", 90);
    let ctl = JobControl::new();
    ctl.cancel();
    let req = ExportRequest {
        project: &p,
        settings: &s,
        out_path: &out,
        work_dir: &work,
        ctl: &ctl,
    };
    let res = run_export(&req, &mut |_| {});
    assert!(matches!(res, Err(RenderError::Cancelled)));
    assert!(!out.exists(), "no output file");
    assert!(!part_of(&out).exists(), "no .part file");
    assert!(
        !work.exists(),
        "a work dir created by the export is removed"
    );
}

#[test]
fn still_export_cancel_keeps_a_work_dir_that_already_existed() {
    let (_dir, out, work) = layout("png");
    std::fs::create_dir_all(&work).expect("mkdir");
    std::fs::write(work.join("keep.txt"), b"keep").expect("write");
    let p = solid_project(SIZE, SIZE_H);
    let s = still_settings("png", 90);
    let ctl = JobControl::new();
    ctl.cancel();
    let req = ExportRequest {
        project: &p,
        settings: &s,
        out_path: &out,
        work_dir: &work,
        ctl: &ctl,
    };
    assert!(matches!(
        run_export(&req, &mut |_| {}),
        Err(RenderError::Cancelled)
    ));
    assert!(
        work.join("keep.txt").is_file(),
        "a pre-existing work dir is not removed"
    );
    assert!(!out.exists());
}
