//! Black-box tests for `clipcheck_render::smart::check_eligibility`. Each test builds a small
//! hand-made project and asserts only the reason that the documented rule order (see the doc
//! comment of `analyse` in smart.rs) yields. Media entries point at paths that do not exist
//! unless the rule under test needs a real file; the rules that come before file probing never
//! touch the disk. No media is decoded and nothing is written except one temp text file.
#![cfg(windows)]

use clipcheck_render::job::JobControl;
use clipcheck_render::smart::{check_eligibility, run_lossless, Eligibility};
use clipcheck_render::RenderError;
use clipcheck_timeline::export::{AudioLayout, ExportKind, ExportRange, ExportSettings};
use clipcheck_timeline::model::{
    Clip, ClipKind, MediaKind, MediaRef, Project, Rgba, TextSpec, Track, TrackKind, Transform,
    Transition, TransitionKind, WorkArea,
};
use clipcheck_timeline::time::Tick;
use std::path::{Path, PathBuf};

/// Reason text for an empty export range.
const NOTHING: &str = "Nothing to export";

/// A project with the default tracks (V1..V3, A1..A4) and a 1280x720 canvas.
fn base_project() -> Project {
    let mut p = Project::new("smart-test");
    p.canvas.width = 1280;
    p.canvas.height = 720;
    p
}

/// Id of the track at `index` among the project's tracks of `kind`.
fn track_id(p: &Project, kind: TrackKind, index: usize) -> String {
    p.tracks
        .iter()
        .filter(|t| t.kind == kind)
        .nth(index)
        .map(|t: &Track| t.id.clone())
        .unwrap_or_default()
}

/// Registers a video media entry with the given id and path (the file need not exist).
fn add_media(p: &mut Project, id: &str, path: &Path) {
    p.media.push(MediaRef {
        id: id.to_string(),
        kind: MediaKind::Video,
        name: format!("{id}.mp4"),
        path: path.to_string_lossy().into_owned(),
        duration: Tick::from_secs(10),
        width: 1280,
        height: 720,
        fps: 30.0,
        video_codec: "h264".to_string(),
        ..MediaRef::default()
    });
}

/// A video clip on the `track_index`-th video track starting at `start_s` for `dur_s` seconds.
fn video_clip(
    p: &Project,
    id: &str,
    media: &str,
    track_index: usize,
    start_s: i64,
    dur_s: i64,
) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: track_id(p, TrackKind::Video, track_index),
        kind: ClipKind::Video,
        media_id: Some(media.to_string()),
        name: id.to_string(),
        start: Tick::from_secs(start_s),
        dur: Tick::from_secs(dur_s),
        src_in: Tick::ZERO,
        ..Clip::default()
    }
}

/// Settings that ask for a lossless video export of the whole timeline.
fn lossless_settings() -> ExportSettings {
    ExportSettings {
        lossless: true,
        width: 1280,
        height: 720,
        ..ExportSettings::default()
    }
}

/// Project with one plain video clip on V1 from 0 s for 2 s, media at a missing path.
fn single_clip_project() -> Project {
    let mut p = base_project();
    add_media(&mut p, "m1", Path::new("C:\\clipcheck-no-such-dir\\m1.mp4"));
    let clip = video_clip(&p, "c1", "m1", 0, 0, 2);
    p.clips.push(clip);
    p
}

/// Asserts the result is ineligible with exactly `reason` and no snap notes.
fn assert_reason(res: &Eligibility, reason: &str) {
    assert!(!res.eligible, "expected ineligible, got {res:?}");
    assert_eq!(res.reason.as_deref(), Some(reason));
    assert!(res.snap_notes.is_empty());
}

#[test]
fn empty_project_is_nothing_to_export() {
    let p = base_project();
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, NOTHING);
}

#[test]
fn selection_of_missing_clips_is_nothing_to_export() {
    let p = single_clip_project();
    let mut s = lossless_settings();
    s.range = ExportRange::SelectedClips {
        ids: vec!["no-such-clip".to_string()],
    };
    let res = check_eligibility(&p, &s);
    assert_reason(&res, NOTHING);
}

#[test]
fn empty_work_area_is_nothing_to_export() {
    let mut p = single_clip_project();
    p.work_area = Some(WorkArea {
        start: Tick::from_secs(1),
        end: Tick::from_secs(1),
    });
    let mut s = lossless_settings();
    s.range = ExportRange::WorkArea;
    let res = check_eligibility(&p, &s);
    assert_reason(&res, NOTHING);
}

#[test]
fn disabled_clip_is_nothing_to_export() {
    let mut p = single_clip_project();
    for c in p.clips.iter_mut() {
        c.enabled = false;
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, NOTHING);
}

#[test]
fn still_kind_is_ineligible() {
    // The kind is checked before any file is inspected, so the missing media does not matter.
    let p = single_clip_project();
    let mut s = lossless_settings();
    s.kind = ExportKind::Still {
        format: "png".to_string(),
        quality: 90,
        at: Tick::ZERO,
    };
    let res = check_eligibility(&p, &s);
    assert_reason(&res, "Only video exports can be lossless");
}

#[test]
fn audio_only_kind_is_ineligible() {
    let p = single_clip_project();
    let mut s = lossless_settings();
    s.kind = ExportKind::AudioOnly;
    let res = check_eligibility(&p, &s);
    assert_reason(&res, "Only video exports can be lossless");
}

#[test]
fn speed_change_is_ineligible() {
    let mut p = single_clip_project();
    if let Some(c) = p.clips.first_mut() {
        c.speed = 2.0;
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Speed changes need re-encoding");
}

#[test]
fn reverse_clip_is_ineligible() {
    let mut p = single_clip_project();
    if let Some(c) = p.clips.first_mut() {
        c.reverse = true;
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Speed changes need re-encoding");
}

#[test]
fn title_clip_is_ineligible() {
    let mut p = base_project();
    p.clips.push(Clip {
        id: "t1".to_string(),
        track_id: track_id(&p, TrackKind::Video, 0),
        kind: ClipKind::Title,
        name: "title".to_string(),
        start: Tick::ZERO,
        dur: Tick::from_secs(2),
        text: Some(TextSpec::default()),
        ..Clip::default()
    });
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Titles, images and solids need re-encoding");
}

#[test]
fn solid_clip_is_ineligible() {
    let mut p = base_project();
    p.clips.push(Clip {
        id: "s1".to_string(),
        track_id: track_id(&p, TrackKind::Video, 0),
        kind: ClipKind::Solid,
        name: "solid".to_string(),
        start: Tick::ZERO,
        dur: Tick::from_secs(2),
        solid: Some(Rgba {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        }),
        ..Clip::default()
    });
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Titles, images and solids need re-encoding");
}

#[test]
fn two_overlapping_video_layers_are_ineligible() {
    let mut p = base_project();
    add_media(&mut p, "m1", Path::new("C:\\clipcheck-no-such-dir\\a.mp4"));
    add_media(&mut p, "m2", Path::new("C:\\clipcheck-no-such-dir\\b.mp4"));
    let a = video_clip(&p, "a", "m1", 0, 0, 3);
    let b = video_clip(&p, "b", "m2", 1, 1, 3);
    p.clips.push(a);
    p.clips.push(b);
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Overlapping video layers");
}

#[test]
fn transform_change_is_ineligible() {
    let mut p = single_clip_project();
    if let Some(c) = p.clips.first_mut() {
        c.transform = Transform {
            pos_x: 25.0,
            ..Transform::default()
        };
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Clip transforms need re-encoding");
}

#[test]
fn opacity_change_is_ineligible() {
    let mut p = single_clip_project();
    if let Some(c) = p.clips.first_mut() {
        c.opacity = 0.5;
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Clip transforms need re-encoding");
}

#[test]
fn gap_at_start_is_ineligible() {
    let mut p = base_project();
    add_media(&mut p, "m1", Path::new("C:\\clipcheck-no-such-dir\\m1.mp4"));
    // Nothing covers 0..1 s, so the plan has a segment with no video.
    let clip = video_clip(&p, "c1", "m1", 0, 1, 2);
    p.clips.push(clip);
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Gaps need re-encoding");
}

#[test]
fn non_mp4_media_names_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file: PathBuf = dir.path().join("notes.txt");
    std::fs::write(&file, b"plain text, not a movie").expect("write text file");

    let mut p = base_project();
    add_media(&mut p, "txt", &file);
    p.media[0].name = "notes.txt".to_string();
    let clip = video_clip(&p, "c1", "txt", 0, 0, 2);
    p.clips.push(clip);

    let res = check_eligibility(&p, &lossless_settings());
    assert!(!res.eligible, "a text file cannot be stream-copied");
    let reason = res.reason.unwrap_or_default();
    assert!(
        reason.contains("notes.txt"),
        "reason should name the file: {reason}"
    );
    assert!(res.snap_notes.is_empty());
}

#[test]
fn ineligible_result_never_reports_snap_notes() {
    let mut p = single_clip_project();
    if let Some(c) = p.clips.first_mut() {
        c.speed = 0.5;
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert!(!res.eligible);
    assert!(res.snap_notes.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Rules that come before file probing (no media is read here)
// ---------------------------------------------------------------------------------------------

#[test]
fn transition_in_is_ineligible_as_a_transform_change() {
    let mut p = single_clip_project();
    if let Some(c) = p.clips.first_mut() {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: Tick::from_secs(1),
        });
    }
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Clip transforms need re-encoding");
}

#[test]
fn gap_between_two_clips_is_ineligible() {
    let mut p = base_project();
    add_media(&mut p, "m1", Path::new("C:\\clipcheck-no-such-dir\\m1.mp4"));
    let first = video_clip(&p, "c1", "m1", 0, 0, 1);
    let second = video_clip(&p, "c2", "m1", 0, 2, 1);
    p.clips.push(first);
    p.clips.push(second);
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Gaps need re-encoding");
}

#[test]
fn audio_speed_change_is_ineligible() {
    let mut p = single_clip_project();
    add_media(&mut p, "a1", Path::new("C:\\clipcheck-no-such-dir\\a1.mp4"));
    let audio = Clip {
        id: "ca1".to_string(),
        track_id: track_id(&p, TrackKind::Audio, 0),
        kind: ClipKind::Audio,
        media_id: Some("a1".to_string()),
        name: "ca1".to_string(),
        start: Tick::ZERO,
        dur: Tick::from_secs(2),
        speed: 2.0,
        ..Clip::default()
    };
    p.clips.push(audio);
    let res = check_eligibility(&p, &lossless_settings());
    assert_reason(&res, "Speed changes need re-encoding");
}

// ---------------------------------------------------------------------------------------------
// run_lossless: nothing is left under the final name or in temp names when it fails
// ---------------------------------------------------------------------------------------------

/// Sorted file names in `dir`.
fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn run_lossless_on_a_file_that_is_not_mp4_writes_no_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.mp4");
    std::fs::write(&bad, b"this is not an mp4 file").expect("write");
    let mut p = base_project();
    add_media(&mut p, "m1", &bad);
    let clip = video_clip(&p, "c1", "m1", 0, 0, 2);
    p.clips.push(clip);

    let dst = dir.path().join("out.mp4");
    let res = run_lossless(
        &p,
        &lossless_settings(),
        &dst,
        &JobControl::new(),
        &mut |_| {},
    );
    assert!(res.is_err(), "a garbage source must fail");
    assert!(!dst.exists(), "no output under the final name");
    assert_eq!(
        dir_names(dir.path()),
        vec!["bad.mp4".to_string()],
        "no .part or piece files"
    );
}

#[test]
fn run_lossless_with_nothing_to_export_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = base_project();
    let dst = dir.path().join("out.mp4");
    let res = run_lossless(
        &p,
        &lossless_settings(),
        &dst,
        &JobControl::new(),
        &mut |_| {},
    );
    assert!(
        matches!(res, Err(RenderError::Invalid(ref m)) if m == NOTHING),
        "{res:?}"
    );
    assert!(dir_names(dir.path()).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Fixture tests: need a real H.264 + AAC MP4 (640x360, 30 fps, 5 s+) in CLIPCHECK_EXPORT_FIXTURE.
// They are ignored by default and never run by the test chain.
// ---------------------------------------------------------------------------------------------

/// The fixture file named by `CLIPCHECK_EXPORT_FIXTURE`.
fn fixture_path() -> PathBuf {
    PathBuf::from(std::env::var("CLIPCHECK_EXPORT_FIXTURE").expect("set CLIPCHECK_EXPORT_FIXTURE"))
}

/// A 640x360 project with one 5 s video clip of the fixture on V1 (media id "f1").
fn fixture_project() -> Project {
    let mut p = Project::new("fixture");
    p.canvas.width = 640;
    p.canvas.height = 360;
    add_media(&mut p, "f1", &fixture_path());
    if let Some(m) = p.media.first_mut() {
        m.width = 640;
        m.height = 360;
    }
    let clip = video_clip(&p, "c1", "f1", 0, 0, 5);
    p.clips.push(clip);
    p
}

/// Lossless settings for the 640x360 fixture, mixing layout off (separate tracks).
fn fixture_settings() -> ExportSettings {
    ExportSettings {
        lossless: true,
        width: 640,
        height: 360,
        ..ExportSettings::default()
    }
}

#[test]
#[ignore = "needs fixture"]
fn fixture_video_without_audio_clip_needs_reencode() {
    // The shown clip's audio stream is not audible in the timeline (detached), so stream copy
    // would change the audio.
    let p = fixture_project();
    let res = check_eligibility(&p, &fixture_settings());
    assert_reason(&res, "Audio changes need re-encoding");
}

#[test]
#[ignore = "needs fixture"]
fn fixture_disabled_audio_clip_needs_reencode() {
    let mut p = fixture_project();
    let mut audio = Clip {
        id: "ca1".to_string(),
        track_id: track_id(&p, TrackKind::Audio, 0),
        kind: ClipKind::Audio,
        media_id: Some("f1".to_string()),
        name: "ca1".to_string(),
        start: Tick::ZERO,
        dur: Tick::from_secs(5),
        ..Clip::default()
    };
    audio.enabled = false;
    p.clips.push(audio);
    let res = check_eligibility(&p, &fixture_settings());
    assert_reason(&res, "Audio changes need re-encoding");
}

#[test]
#[ignore = "needs fixture with two audio streams"]
fn fixture_mix_of_two_source_audio_streams_needs_reencode() {
    let mut p = fixture_project();
    for (i, track) in [(0_u32, 0_usize), (1, 1)] {
        let mut audio = Clip {
            id: format!("ca{i}"),
            track_id: track_id(&p, TrackKind::Audio, track),
            kind: ClipKind::Audio,
            media_id: Some("f1".to_string()),
            name: format!("ca{i}"),
            start: Tick::ZERO,
            dur: Tick::from_secs(5),
            ..Clip::default()
        };
        audio.audio.source_track = i;
        p.clips.push(audio);
    }
    let mut s = fixture_settings();
    s.audio.layout = AudioLayout::Mix;
    let res = check_eligibility(&p, &s);
    assert_reason(&res, "Mixing several audio tracks needs re-encoding");
}

#[test]
#[ignore = "needs fixture"]
fn fixture_cancel_before_run_is_cancelled_and_leaves_no_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dst = dir.path().join("out.mp4");
    let ctl = JobControl::new();
    ctl.cancel();
    let res = run_lossless(
        &fixture_project(),
        &fixture_settings(),
        &dst,
        &ctl,
        &mut |_| {},
    );
    assert!(matches!(res, Err(RenderError::Cancelled)), "{res:?}");
    assert!(dir_names(dir.path()).is_empty());
}
