//! Property edit tests (public API only): clip patches, envelopes, gain offsets, tracks, canvas,
//! project name, modes, markers, generators and audio clip construction.
//! Helpers are inline on purpose (other testers write sibling files at the same time).

use clipcheck_timeline::model::{
    Canvas, Clip, ClipAudio, ClipKind, EnvPoint, FadeCurve, Group, MediaAudio, MediaKind, MediaRef,
    Project, Rgba, TextSpec, Track, TrackKind, Transform, Transition, TransitionKind,
};
use clipcheck_timeline::ops_props::{
    add_marker, add_solid, add_title, add_track, audio_clip_for, edit_marker, move_track,
    offset_gain, patch_clips, patch_track, remove_marker, remove_track, rename_project, set_canvas,
    set_envelope, set_modes, track_of_kind, ClipPatch, TrackPatch,
};
use clipcheck_timeline::time::{Rate, Tick, MAX_PROJECT_TICKS, TICKS_PER_SECOND};
use clipcheck_timeline::EditError;

// ----- helpers -------------------------------------------------------------------------------

fn secs(n: i64) -> Tick {
    Tick::from_secs(n)
}

/// Id of the `idx`-th track of `kind` in a project.
fn tid(p: &Project, kind: TrackKind, idx: usize) -> String {
    track_of_kind(p, kind, idx)
        .expect("track exists")
        .id
        .clone()
}

/// A project with media "m1" (video with two audio streams, 60 s long) and default tracks.
fn project() -> Project {
    let mut p = Project::new("Test");
    p.media.push(MediaRef {
        id: "m1".to_string(),
        kind: MediaKind::Video,
        name: "clip.mp4".to_string(),
        duration: secs(60),
        audio: vec![
            MediaAudio {
                title: "Game".to_string(),
                ..MediaAudio::default()
            },
            MediaAudio {
                title: "  ".to_string(),
                ..MediaAudio::default()
            },
        ],
        ..MediaRef::default()
    });
    p
}

fn video_clip(id: &str, track_id: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: track_id.to_string(),
        kind: ClipKind::Video,
        media_id: Some("m1".to_string()),
        start,
        dur,
        ..Clip::default()
    }
}

fn audio_clip(id: &str, track_id: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: track_id.to_string(),
        kind: ClipKind::Audio,
        media_id: Some("m1".to_string()),
        start,
        dur,
        audio: ClipAudio::default(),
        ..Clip::default()
    }
}

fn clip_ref<'a>(p: &'a Project, id: &str) -> &'a Clip {
    p.clip(id).expect("clip exists")
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn gain_patch(db: f64) -> ClipPatch {
    ClipPatch {
        gain_db: Some(db),
        ..ClipPatch::default()
    }
}

fn assert_invalid<T: std::fmt::Debug>(r: Result<T, EditError>) {
    match r {
        Err(EditError::Invalid(_)) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

/// Project with one audio clip "a" (2 s) and one video clip "v" (2 s) on the first tracks.
fn mixed_project() -> Project {
    let mut p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let vv = tid(&p, TrackKind::Video, 0);
    p.clips.push(audio_clip("a", &va, secs(0), secs(2)));
    p.clips.push(video_clip("v", &vv, secs(0), secs(2)));
    p
}

// ----- patch_clips: ranges --------------------------------------------------------------------

#[test]
fn gain_above_12_db_is_invalid_and_nothing_changes() {
    let mut p = mixed_project();
    let before = p.clip("a").cloned();
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &gain_patch(13.0)));
    assert_eq!(p.clip("a").cloned(), before);
}

#[test]
fn gain_at_bounds_is_accepted() {
    let mut p = mixed_project();
    patch_clips(&mut p, &ids(&["a"]), &gain_patch(12.0)).expect("12 dB ok");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, 12.0);
    patch_clips(&mut p, &ids(&["a"]), &gain_patch(-60.0)).expect("-60 dB ok");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, -60.0);
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &gain_patch(-60.5)));
}

#[test]
fn pan_of_two_is_invalid() {
    let mut p = mixed_project();
    let patch = ClipPatch {
        pan: Some(2.0),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &patch));
    assert_eq!(clip_ref(&p, "a").audio.pan, 0.0);
}

#[test]
fn pan_nan_is_invalid() {
    let mut p = mixed_project();
    let patch = ClipPatch {
        pan: Some(f64::NAN),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &patch));
}

#[test]
fn opacity_above_one_is_invalid_and_valid_opacity_applies() {
    let mut p = mixed_project();
    let bad = ClipPatch {
        opacity: Some(1.5),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &bad));
    assert_eq!(clip_ref(&p, "v").opacity, 1.0);
    let good = ClipPatch {
        opacity: Some(0.25),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["v"]), &good).expect("opacity 0.25 ok");
    assert_eq!(clip_ref(&p, "v").opacity, 0.25);
}

#[test]
fn fades_longer_than_clip_are_invalid() {
    let mut p = mixed_project();
    // Clip is 2 s; 1.5 s + 1 s exceeds it.
    let patch = ClipPatch {
        fade_in: Some(Tick::from_ms(1500)),
        fade_out: Some(secs(1)),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &patch));
    assert_eq!(clip_ref(&p, "a").audio.fade_in, Tick::ZERO);
}

#[test]
fn fade_in_alone_is_checked_against_existing_fade_out() {
    let mut p = mixed_project();
    let set_out = ClipPatch {
        fade_out: Some(secs(1)),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["a"]), &set_out).expect("fade out 1 s ok");
    let set_in = ClipPatch {
        fade_in: Some(Tick::from_ms(1100)),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &set_in));
}

#[test]
fn negative_fade_is_invalid() {
    let mut p = mixed_project();
    let patch = ClipPatch {
        fade_in: Some(Tick(-1)),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &patch));
}

#[test]
fn transition_longer_than_five_seconds_is_invalid() {
    let mut p = mixed_project();
    let t = Transition {
        kind: TransitionKind::Crossfade,
        duration: Tick(5 * TICKS_PER_SECOND + 1),
    };
    let patch = ClipPatch {
        transition_in: Some(Some(t)),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &patch));
}

#[test]
fn transition_shorter_than_point_one_second_is_invalid() {
    let mut p = mixed_project();
    let t = Transition {
        kind: TransitionKind::DipToBlack,
        duration: Tick(25_401_600_000 - 1),
    };
    let patch = ClipPatch {
        transition_in: Some(Some(t)),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &patch));
}

/// `mixed_project` with "v" moved to 2 s and a video clip "prev" (0..2 s) before it on the same
/// track, so a transition on "v" has the neighbour it needs.
fn project_with_video_predecessor() -> Project {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    p.clip_mut("v").expect("v").start = secs(2);
    p.clips.push(video_clip("prev", &vv, secs(0), secs(2)));
    p
}

#[test]
fn transition_of_exactly_point_one_second_is_accepted() {
    let mut p = project_with_video_predecessor();
    let t = Transition {
        kind: TransitionKind::DipToWhite,
        duration: Tick(25_401_600_000),
    };
    let patch = ClipPatch {
        transition_in: Some(Some(t)),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["v"]), &patch).expect("0.1 s transition ok");
    assert_eq!(clip_ref(&p, "v").transition_in, Some(t));
}

#[test]
fn transition_longer_than_the_clip_is_clamped_to_the_clip() {
    let mut p = Project::new("t");
    let vv = tid(&p, TrackKind::Video, 0);
    p.clips.push(video_clip("prev", &vv, secs(0), secs(1)));
    p.clips
        .push(video_clip("v", &vv, secs(1), Tick::from_ms(500)));
    let t = Transition {
        kind: TransitionKind::Crossfade,
        duration: secs(1),
    };
    let patch = ClipPatch {
        transition_in: Some(Some(t)),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["v"]), &patch).expect("clamped, not refused");
    assert_eq!(
        clip_ref(&p, "v").transition_in.map(|t| t.duration),
        Some(Tick::from_ms(500))
    );
}

#[test]
fn transition_without_an_adjacent_clip_before_it_is_invalid_and_nothing_changes() {
    // "v" starts at 0 with nothing before it.
    let mut p = mixed_project();
    let t = Transition {
        kind: TransitionKind::Crossfade,
        duration: secs(1),
    };
    let patch = ClipPatch {
        transition_in: Some(Some(t)),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &patch));
    assert_eq!(clip_ref(&p, "v").transition_in, None);

    // A gap before the clip (predecessor ends at 2 s, "v" starts at 3 s) is not adjacent either.
    let mut gap = project_with_video_predecessor();
    gap.clip_mut("v").expect("v").start = secs(3);
    assert_invalid(patch_clips(&mut gap, &ids(&["v"]), &patch));
    assert_eq!(clip_ref(&gap, "v").transition_in, None);

    // Clearing a transition never needs a neighbour.
    let clear = ClipPatch {
        transition_in: Some(None),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["v"]), &clear).expect("clear ok");
}

#[test]
fn transition_in_null_clears_an_existing_transition() {
    let mut p = project_with_video_predecessor();
    let t = Transition {
        kind: TransitionKind::Crossfade,
        duration: secs(1),
    };
    let set = ClipPatch {
        transition_in: Some(Some(t)),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["v"]), &set).expect("set ok");
    let clear = ClipPatch {
        transition_in: Some(None),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["v"]), &clear).expect("clear ok");
    assert_eq!(clip_ref(&p, "v").transition_in, None);
}

#[test]
fn rotate_90_above_three_is_invalid() {
    let mut p = mixed_project();
    let t = Transform {
        rotate_90: 4,
        ..Transform::default()
    };
    let patch = ClipPatch {
        transform: Some(t),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &patch));
}

#[test]
fn crop_left_plus_right_at_or_above_point_nine_five_is_invalid() {
    let mut p = mixed_project();
    let mut t = Transform::default();
    t.crop.left = 0.5;
    t.crop.right = 0.45;
    let patch = ClipPatch {
        transform: Some(t),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &patch));
}

#[test]
fn scale_outside_range_is_invalid_and_valid_transform_applies() {
    let mut p = mixed_project();
    let bad = Transform {
        scale: 9.0,
        ..Transform::default()
    };
    assert_invalid(patch_clips(
        &mut p,
        &ids(&["v"]),
        &ClipPatch {
            transform: Some(bad),
            ..ClipPatch::default()
        },
    ));
    let good = Transform {
        scale: 2.0,
        ..Transform::default()
    };
    patch_clips(
        &mut p,
        &ids(&["v"]),
        &ClipPatch {
            transform: Some(good),
            ..ClipPatch::default()
        },
    )
    .expect("scale 2 ok");
    assert_eq!(clip_ref(&p, "v").transform.scale, 2.0);
}

// ----- patch_clips: kind mismatch, selection, locks --------------------------------------------

#[test]
fn audio_field_on_video_clip_is_invalid_with_nothing_changed() {
    let mut p = mixed_project();
    let before = p.clip("v").cloned();
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &gain_patch(-3.0)));
    assert_eq!(p.clip("v").cloned(), before);
}

#[test]
fn visual_field_on_audio_clip_is_invalid() {
    let mut p = mixed_project();
    let patch = ClipPatch {
        opacity: Some(0.5),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["a"]), &patch));
}

#[test]
fn text_on_video_clip_is_invalid_and_solid_on_video_is_invalid() {
    let mut p = mixed_project();
    let text = ClipPatch {
        text: Some(TextSpec::default()),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &text));
    let solid = ClipPatch {
        solid: Some(Rgba::WHITE),
        ..ClipPatch::default()
    };
    assert_invalid(patch_clips(&mut p, &ids(&["v"]), &solid));
}

#[test]
fn mixed_selection_with_one_mismatch_changes_nothing() {
    // The gain applies to audio but the video clip rejects it, so the whole edit is refused.
    let mut p = mixed_project();
    let before_a = p.clip("a").cloned();
    assert_invalid(patch_clips(&mut p, &ids(&["a", "v"]), &gain_patch(-6.0)));
    assert_eq!(p.clip("a").cloned(), before_a);
}

#[test]
fn name_and_enabled_apply_to_video_and_audio_together() {
    let mut p = mixed_project();
    let patch = ClipPatch {
        name: Some("renamed".to_string()),
        enabled: Some(false),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["a", "v"]), &patch).expect("common fields ok");
    assert_eq!(clip_ref(&p, "a").name, "renamed");
    assert_eq!(clip_ref(&p, "v").name, "renamed");
    assert!(!clip_ref(&p, "a").enabled);
    assert!(!clip_ref(&p, "v").enabled);
}

#[test]
fn unknown_clip_id_gives_not_found() {
    let mut p = mixed_project();
    let r = patch_clips(&mut p, &ids(&["nope"]), &gain_patch(0.0));
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

#[test]
fn clip_on_locked_track_gives_locked_error() {
    let mut p = mixed_project();
    let va = tid(&p, TrackKind::Audio, 0);
    patch_track(
        &mut p,
        &va,
        &TrackPatch {
            locked: Some(true),
            ..TrackPatch::default()
        },
    )
    .expect("locking is allowed");
    let r = patch_clips(&mut p, &ids(&["a"]), &gain_patch(-1.0));
    assert!(matches!(r, Err(EditError::Locked(_))), "got {r:?}");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, 0.0);
}

#[test]
fn empty_selection_is_a_no_op_ok() {
    let mut p = mixed_project();
    let before = p.clips.clone();
    patch_clips(&mut p, &[], &gain_patch(-1.0)).expect("empty ok");
    assert_eq!(p.clips, before);
}

#[test]
fn duplicate_ids_in_selection_apply_once() {
    let mut p = mixed_project();
    offset_gain(&mut p, &ids(&["a", "a"]), 3.0).expect("duplicates ok");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, 3.0);
}

// ----- ClipPatch serde -------------------------------------------------------------------------

#[test]
fn clip_patch_color_null_is_some_none() {
    let p: ClipPatch = serde_json::from_str(r#"{"color": null}"#).expect("parse");
    assert_eq!(p.color, Some(None));
}

#[test]
fn clip_patch_color_absent_is_none() {
    let p: ClipPatch = serde_json::from_str(r#"{}"#).expect("parse");
    assert_eq!(p.color, None);
}

#[test]
fn clip_patch_color_string_is_some_some() {
    let p: ClipPatch = serde_json::from_str(r##"{"color": "#ff0000"}"##).expect("parse");
    assert_eq!(p.color, Some(Some("#ff0000".to_string())));
}

#[test]
fn clip_patch_transition_in_null_is_some_none_and_absent_is_none() {
    let cleared: ClipPatch = serde_json::from_str(r#"{"transition_in": null}"#).expect("parse");
    assert_eq!(cleared.transition_in, Some(None));
    let absent: ClipPatch = serde_json::from_str(r#"{"gain_db": 1.0}"#).expect("parse");
    assert_eq!(absent.transition_in, None);
}

#[test]
fn clip_patch_round_trip_keeps_double_options() {
    let original = ClipPatch {
        color: Some(None),
        transition_in: Some(None),
        gain_db: Some(-2.0),
        ..ClipPatch::default()
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let back: ClipPatch = serde_json::from_str(&json).expect("parse");
    assert_eq!(back.color, Some(None));
    assert_eq!(back.transition_in, Some(None));
    assert_eq!(back.gain_db, Some(-2.0));
    assert_eq!(back.opacity, None);
}

#[test]
fn clip_patch_absent_color_serialises_without_key() {
    let json = serde_json::to_value(ClipPatch::default()).expect("serialize");
    assert!(json.get("color").is_none());
    assert!(json.get("transition_in").is_none());
}

// ----- set_envelope ----------------------------------------------------------------------------

#[test]
fn envelope_is_sorted_and_deduplicated_last_wins() {
    let mut p = mixed_project();
    let pts = vec![
        EnvPoint {
            t: secs(1),
            db: -3.0,
        },
        EnvPoint {
            t: Tick::from_ms(500),
            db: -1.0,
        },
        EnvPoint {
            t: secs(1),
            db: -2.0,
        },
    ];
    set_envelope(&mut p, "a", pts).expect("envelope ok");
    let env = &clip_ref(&p, "a").audio.envelope;
    assert_eq!(env.len(), 2);
    assert_eq!(env[0].t, Tick::from_ms(500));
    assert_eq!(env[1].t, secs(1));
    assert_eq!(env[1].db, -2.0);
}

#[test]
fn envelope_db_is_clamped_to_range() {
    let mut p = mixed_project();
    let pts = vec![
        EnvPoint {
            t: secs(0),
            db: -100.0,
        },
        EnvPoint {
            t: secs(1),
            db: 20.0,
        },
    ];
    set_envelope(&mut p, "a", pts).expect("envelope ok");
    let env = &clip_ref(&p, "a").audio.envelope;
    assert_eq!(env[0].db, -60.0);
    assert_eq!(env[1].db, 12.0);
}

#[test]
fn envelope_points_beyond_clip_are_not_kept() {
    // Contract ambiguity: the doc says "clamped to the clip's length"; the implementation drops
    // points outside 0..=dur. Either way no kept point may lie outside the clip.
    let mut p = mixed_project();
    let pts = vec![
        EnvPoint {
            t: secs(-1),
            db: 0.0,
        },
        EnvPoint {
            t: secs(1),
            db: -1.0,
        },
        EnvPoint {
            t: secs(9),
            db: -2.0,
        },
    ];
    set_envelope(&mut p, "a", pts).expect("envelope ok");
    let env = &clip_ref(&p, "a").audio.envelope;
    assert!(env.iter().all(|pt| pt.t >= Tick::ZERO && pt.t <= secs(2)));
    assert!(env.iter().any(|pt| pt.t == secs(1)));
}

#[test]
fn envelope_with_more_than_64_points_gives_limit() {
    let mut p = mixed_project();
    let pts: Vec<EnvPoint> = (0..65)
        .map(|i| EnvPoint {
            t: Tick::from_ms(i),
            db: 0.0,
        })
        .collect();
    let r = set_envelope(&mut p, "a", pts);
    assert!(matches!(r, Err(EditError::Limit(_))), "got {r:?}");
}

#[test]
fn envelope_with_exactly_64_points_is_accepted() {
    let mut p = mixed_project();
    let pts: Vec<EnvPoint> = (0..64)
        .map(|i| EnvPoint {
            t: Tick::from_ms(i),
            db: 0.0,
        })
        .collect();
    set_envelope(&mut p, "a", pts).expect("64 points ok");
    assert_eq!(clip_ref(&p, "a").audio.envelope.len(), 64);
}

#[test]
fn envelope_on_video_clip_is_invalid() {
    let mut p = mixed_project();
    let r = set_envelope(
        &mut p,
        "v",
        vec![EnvPoint {
            t: secs(0),
            db: 0.0,
        }],
    );
    assert_invalid(r);
}

#[test]
fn envelope_with_nan_db_is_invalid() {
    let mut p = mixed_project();
    let r = set_envelope(
        &mut p,
        "a",
        vec![EnvPoint {
            t: secs(0),
            db: f64::NAN,
        }],
    );
    assert_invalid(r);
}

#[test]
fn envelope_on_locked_track_gives_locked() {
    let mut p = mixed_project();
    let va = tid(&p, TrackKind::Audio, 0);
    patch_track(
        &mut p,
        &va,
        &TrackPatch {
            locked: Some(true),
            ..TrackPatch::default()
        },
    )
    .expect("lock ok");
    let r = set_envelope(&mut p, "a", vec![]);
    assert!(matches!(r, Err(EditError::Locked(_))), "got {r:?}");
}

#[test]
fn envelope_on_unknown_clip_is_not_found() {
    let mut p = mixed_project();
    let r = set_envelope(&mut p, "ghost", vec![]);
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

// ----- offset_gain -----------------------------------------------------------------------------

#[test]
fn offset_gain_adds_delta_to_each_clip() {
    let mut p = mixed_project();
    offset_gain(&mut p, &ids(&["a"]), -6.0).expect("ok");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, -6.0);
}

#[test]
fn offset_gain_clamps_result_to_range() {
    let mut p = mixed_project();
    patch_clips(&mut p, &ids(&["a"]), &gain_patch(-58.0)).expect("set");
    offset_gain(&mut p, &ids(&["a"]), -10.0).expect("ok");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, -60.0);
    patch_clips(&mut p, &ids(&["a"]), &gain_patch(10.0)).expect("set");
    offset_gain(&mut p, &ids(&["a"]), 5.0).expect("ok");
    assert_eq!(clip_ref(&p, "a").audio.gain_db, 12.0);
}

#[test]
fn offset_gain_non_finite_delta_is_invalid() {
    let mut p = mixed_project();
    assert_invalid(offset_gain(&mut p, &ids(&["a"]), f64::INFINITY));
    assert_eq!(clip_ref(&p, "a").audio.gain_db, 0.0);
}

#[test]
fn offset_gain_with_video_clip_is_invalid_and_atomic() {
    let mut p = mixed_project();
    assert_invalid(offset_gain(&mut p, &ids(&["a", "v"]), 2.0));
    assert_eq!(clip_ref(&p, "a").audio.gain_db, 0.0);
}

// ----- add_track / remove_track ----------------------------------------------------------------

#[test]
fn default_project_has_three_video_and_four_audio_tracks() {
    let p = Project::new("x");
    let video = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .count();
    let audio = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count();
    assert_eq!((video, audio), (3, 4));
}

#[test]
fn add_video_track_is_named_after_count_and_sits_after_last_video() {
    let mut p = Project::new("x");
    let id = add_track(&mut p, TrackKind::Video).expect("room for more");
    let t = p.track(&id).expect("exists");
    assert_eq!(t.name, "V4");
    assert_eq!(t.kind, TrackKind::Video);
    let pos_new = p.tracks.iter().position(|x| x.id == id).expect("pos");
    let pos_last_v3 = p.tracks.iter().position(|x| x.name == "V3").expect("V3");
    assert_eq!(pos_new, pos_last_v3 + 1);
}

#[test]
fn add_audio_track_is_named_a_n_and_sits_after_last_audio() {
    let mut p = Project::new("x");
    let id = add_track(&mut p, TrackKind::Audio).expect("room");
    assert_eq!(p.track(&id).expect("exists").name, "A5");
    assert_eq!(p.tracks.last().map(|t| t.id.clone()), Some(id));
}

#[test]
fn video_track_limit_is_six() {
    let mut p = Project::new("x");
    for _ in 0..3 {
        add_track(&mut p, TrackKind::Video).expect("up to six");
    }
    let r = add_track(&mut p, TrackKind::Video);
    assert!(matches!(r, Err(EditError::Limit(_))), "got {r:?}");
}

#[test]
fn audio_track_limit_is_eight() {
    let mut p = Project::new("x");
    for _ in 0..4 {
        add_track(&mut p, TrackKind::Audio).expect("up to eight");
    }
    let r = add_track(&mut p, TrackKind::Audio);
    assert!(matches!(r, Err(EditError::Limit(_))), "got {r:?}");
}

#[test]
fn remove_track_removes_its_clips() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    remove_track(&mut p, &vv).expect("removed");
    assert!(p.clip("v").is_none());
    assert!(p.clip("a").is_some());
}

#[test]
fn remove_last_track_of_a_kind_is_invalid() {
    let mut p = Project::new("x");
    // Remove V1 and V2, leaving V3 as the only video track; it may not go.
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    remove_track(&mut p, &v1).expect("V1 removable");
    remove_track(&mut p, &v2).expect("V2 removable");
    let last = tid(&p, TrackKind::Video, 0);
    assert_invalid(remove_track(&mut p, &last));
    assert_eq!(
        p.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .count(),
        1
    );
}

#[test]
fn remove_locked_track_gives_locked() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    patch_track(
        &mut p,
        &vv,
        &TrackPatch {
            locked: Some(true),
            ..TrackPatch::default()
        },
    )
    .expect("lock");
    let r = remove_track(&mut p, &vv);
    assert!(matches!(r, Err(EditError::Locked(_))), "got {r:?}");
    assert!(p.clip("v").is_some());
}

#[test]
fn remove_track_unknown_id_is_not_found() {
    let mut p = mixed_project();
    let r = remove_track(&mut p, "nope");
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

#[test]
fn removing_track_clears_link_held_by_one_remaining_clip() {
    let mut p = mixed_project();
    // A second video clip on V2 shares link id "L" with audio clip "a".
    let vv2 = tid(&p, TrackKind::Video, 1);
    p.clips.push(video_clip("v2", &vv2, secs(0), secs(2)));
    p.clip_mut("a").expect("a").link_id = Some("L".to_string());
    p.clip_mut("v2").expect("v2").link_id = Some("L".to_string());
    remove_track(&mut p, &vv2).expect("removed");
    assert!(p.clip("v2").is_none());
    assert_eq!(
        clip_ref(&p, "a").link_id,
        None,
        "dangling link must be cleared"
    );
}

#[test]
fn removing_track_dissolves_group_left_with_one_member() {
    let mut p = mixed_project();
    let vv2 = tid(&p, TrackKind::Video, 1);
    p.clips.push(video_clip("v2", &vv2, secs(0), secs(2)));
    p.groups.push(Group {
        id: "G".to_string(),
        ..Group::default()
    });
    p.clip_mut("a").expect("a").group_id = Some("G".to_string());
    p.clip_mut("v2").expect("v2").group_id = Some("G".to_string());
    remove_track(&mut p, &vv2).expect("removed");
    assert!(p.groups.is_empty(), "group with one member must be removed");
    assert_eq!(clip_ref(&p, "a").group_id, None);
}

#[test]
fn group_with_two_remaining_members_survives_track_removal() {
    let mut p = mixed_project();
    let vv2 = tid(&p, TrackKind::Video, 1);
    let va2 = tid(&p, TrackKind::Audio, 1);
    p.clips.push(video_clip("v2", &vv2, secs(0), secs(2)));
    p.clips.push(audio_clip("a2", &va2, secs(0), secs(2)));
    p.groups.push(Group {
        id: "G".to_string(),
        ..Group::default()
    });
    for id in ["a", "a2", "v2"] {
        p.clip_mut(id).expect("clip").group_id = Some("G".to_string());
    }
    remove_track(&mut p, &vv2).expect("removed");
    assert_eq!(p.groups.len(), 1);
    assert_eq!(clip_ref(&p, "a").group_id.as_deref(), Some("G"));
}

// ----- patch_track -----------------------------------------------------------------------------

#[test]
fn track_name_is_trimmed_and_length_checked() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    patch_track(
        &mut p,
        &vv,
        &TrackPatch {
            name: Some("  Main  ".to_string()),
            ..TrackPatch::default()
        },
    )
    .expect("ok");
    assert_eq!(p.track(&vv).expect("t").name, "Main");
    let empty = TrackPatch {
        name: Some("   ".to_string()),
        ..TrackPatch::default()
    };
    assert_invalid(patch_track(&mut p, &vv, &empty));
    let long = TrackPatch {
        name: Some("x".repeat(41)),
        ..TrackPatch::default()
    };
    assert_invalid(patch_track(&mut p, &vv, &long));
    assert_eq!(p.track(&vv).expect("t").name, "Main");
}

#[test]
fn track_volume_range_is_minus_60_to_12() {
    let mut p = mixed_project();
    let va = tid(&p, TrackKind::Audio, 0);
    let ok = TrackPatch {
        volume_db: Some(12.0),
        ..TrackPatch::default()
    };
    patch_track(&mut p, &va, &ok).expect("12 ok");
    assert_eq!(p.track(&va).expect("t").volume_db, 12.0);
    let bad = TrackPatch {
        volume_db: Some(12.5),
        ..TrackPatch::default()
    };
    assert_invalid(patch_track(&mut p, &va, &bad));
    let low = TrackPatch {
        volume_db: Some(-61.0),
        ..TrackPatch::default()
    };
    assert_invalid(patch_track(&mut p, &va, &low));
}

#[test]
fn track_height_range_is_24_to_200_inclusive() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    for h in [24u32, 200] {
        patch_track(
            &mut p,
            &vv,
            &TrackPatch {
                height: Some(h),
                ..TrackPatch::default()
            },
        )
        .expect("bound ok");
        assert_eq!(p.track(&vv).expect("t").height, h);
    }
    for h in [23u32, 201] {
        assert_invalid(patch_track(
            &mut p,
            &vv,
            &TrackPatch {
                height: Some(h),
                ..TrackPatch::default()
            },
        ));
    }
}

#[test]
fn locked_track_still_accepts_track_patch_so_it_can_be_unlocked() {
    let mut p = mixed_project();
    let va = tid(&p, TrackKind::Audio, 0);
    patch_track(
        &mut p,
        &va,
        &TrackPatch {
            locked: Some(true),
            ..TrackPatch::default()
        },
    )
    .expect("lock");
    patch_track(
        &mut p,
        &va,
        &TrackPatch {
            locked: Some(false),
            ..TrackPatch::default()
        },
    )
    .expect("unlock while locked is a track setting");
    assert!(!p.track(&va).expect("t").locked);
}

#[test]
fn track_flags_apply_independently() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    let patch = TrackPatch {
        muted: Some(true),
        solo: Some(true),
        hidden: Some(true),
        targeted: Some(false),
        ..TrackPatch::default()
    };
    patch_track(&mut p, &vv, &patch).expect("ok");
    let t = p.track(&vv).expect("t");
    assert!(t.muted && t.solo && t.hidden && !t.targeted);
    assert!(!t.locked);
}

#[test]
fn patch_track_unknown_id_is_not_found() {
    let mut p = mixed_project();
    let r = patch_track(&mut p, "nope", &TrackPatch::default());
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

// ----- move_track ------------------------------------------------------------------------------

fn video_names(p: &Project) -> Vec<String> {
    p.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .map(|t| t.name.clone())
        .collect()
}

#[test]
fn move_track_positive_delta_moves_later() {
    let mut p = Project::new("x");
    let v1 = tid(&p, TrackKind::Video, 0);
    move_track(&mut p, &v1, 1).expect("ok");
    assert_eq!(video_names(&p), ["V2", "V1", "V3"]);
}

#[test]
fn move_track_clamps_at_ends() {
    let mut p = Project::new("x");
    let v1 = tid(&p, TrackKind::Video, 0);
    move_track(&mut p, &v1, 100).expect("clamped");
    assert_eq!(video_names(&p), ["V2", "V3", "V1"]);
    move_track(&mut p, &v1, -100).expect("clamped");
    assert_eq!(video_names(&p), ["V1", "V2", "V3"]);
}

#[test]
fn move_track_does_not_disturb_other_kind() {
    let mut p = Project::new("x");
    let audio_before: Vec<String> = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id.clone())
        .collect();
    let v1 = tid(&p, TrackKind::Video, 0);
    move_track(&mut p, &v1, 2).expect("ok");
    let audio_after: Vec<String> = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(audio_before, audio_after);
}

#[test]
fn move_track_unknown_id_is_not_found() {
    let mut p = Project::new("x");
    let r = move_track(&mut p, "nope", 1);
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

// ----- set_canvas ------------------------------------------------------------------------------

fn canvas(w: u32, h: u32, fps: Rate) -> Canvas {
    Canvas {
        width: w,
        height: h,
        fps,
        ..Canvas::default()
    }
}

#[test]
fn canvas_dimensions_must_be_even_and_in_range() {
    let mut p = Project::new("x");
    assert_invalid(set_canvas(&mut p, canvas(127, 720, Rate::whole(30))));
    assert_invalid(set_canvas(&mut p, canvas(1281, 720, Rate::whole(30))));
    assert_invalid(set_canvas(&mut p, canvas(1280, 8194, Rate::whole(30))));
    assert_invalid(set_canvas(&mut p, canvas(1280, 127, Rate::whole(30))));
}

#[test]
fn canvas_dimension_bounds_are_inclusive() {
    let mut p = Project::new("x");
    set_canvas(&mut p, canvas(128, 128, Rate::whole(30))).expect("min ok");
    set_canvas(&mut p, canvas(8192, 8192, Rate::whole(30))).expect("max ok");
    assert_eq!(p.canvas.width, 8192);
}

#[test]
fn canvas_fps_above_120_is_invalid_and_120_is_ok() {
    let mut p = Project::new("x");
    assert_invalid(set_canvas(&mut p, canvas(1280, 720, Rate::whole(121))));
    set_canvas(&mut p, canvas(1280, 720, Rate::whole(120))).expect("120 ok");
}

#[test]
fn canvas_fps_zero_is_invalid() {
    let mut p = Project::new("x");
    assert_invalid(set_canvas(
        &mut p,
        canvas(1280, 720, Rate { num: 0, den: 1 }),
    ));
    assert_invalid(set_canvas(
        &mut p,
        canvas(1280, 720, Rate { num: 30, den: 0 }),
    ));
}

#[test]
fn canvas_fps_below_one_is_invalid_per_doc() {
    // Doc on set_canvas says "fps 1..=120"; 0.5 fps must therefore be refused.
    let mut p = Project::new("x");
    assert_invalid(set_canvas(
        &mut p,
        canvas(1280, 720, Rate { num: 1, den: 2 }),
    ));
}

#[test]
fn canvas_fps_of_exactly_one_and_ntsc_rate_are_ok() {
    let mut p = Project::new("x");
    set_canvas(&mut p, canvas(1280, 720, Rate::whole(1))).expect("1 fps is the lower bound");
    set_canvas(
        &mut p,
        canvas(
            1280,
            720,
            Rate {
                num: 30_000,
                den: 1_001,
            },
        ),
    )
    .expect("NTSC ok");
    assert_eq!(
        p.canvas.fps,
        Rate {
            num: 30_000,
            den: 1_001
        }
    );
}

#[test]
fn canvas_aspect_preset_longer_than_32_chars_is_invalid() {
    let mut p = Project::new("x");
    let mut c = canvas(1280, 720, Rate::whole(30));
    c.aspect_preset = "a".repeat(33);
    assert_invalid(set_canvas(&mut p, c));
}

#[test]
fn canvas_change_does_not_touch_clips() {
    let mut p = mixed_project();
    let before = p.clips.clone();
    set_canvas(&mut p, canvas(1280, 720, Rate::whole(60))).expect("ok");
    assert_eq!(p.clips, before);
    assert_eq!(p.canvas.fps, Rate::whole(60));
}

// ----- rename_project / set_modes --------------------------------------------------------------

#[test]
fn project_name_is_trimmed() {
    let mut p = Project::new("x");
    rename_project(&mut p, "  Trip  ").expect("ok");
    assert_eq!(p.name, "Trip");
}

#[test]
fn project_name_must_have_1_to_120_chars_after_trimming() {
    let mut p = Project::new("x");
    assert_invalid(rename_project(&mut p, ""));
    assert_invalid(rename_project(&mut p, "   "));
    assert_invalid(rename_project(&mut p, &"a".repeat(121)));
    rename_project(&mut p, &"a".repeat(120)).expect("120 ok");
    assert_eq!(p.name.chars().count(), 120);
    assert_eq!(p.name.len(), 120);
}

#[test]
fn set_modes_sets_both_flags() {
    let mut p = Project::new("x");
    set_modes(&mut p, false, true).expect("ok");
    assert!(!p.modes.magnetic);
    assert!(p.modes.snap);
    set_modes(&mut p, true, false).expect("ok");
    assert!(p.modes.magnetic);
    assert!(!p.modes.snap);
}

// ----- markers ---------------------------------------------------------------------------------

#[test]
fn markers_are_kept_sorted_by_time() {
    let mut p = Project::new("x");
    add_marker(&mut p, secs(5), "b", None).expect("add");
    add_marker(&mut p, secs(2), "a", None).expect("add");
    add_marker(&mut p, secs(9), "c", Some("#fff".to_string())).expect("add");
    let times: Vec<Tick> = p.markers.iter().map(|m| m.time).collect();
    assert_eq!(times, vec![secs(2), secs(5), secs(9)]);
}

#[test]
fn add_marker_returns_id_matching_stored_marker() {
    let mut p = Project::new("x");
    let id = add_marker(&mut p, secs(1), "hi", None).expect("add");
    assert!(p.markers.iter().any(|m| m.id == id && m.label == "hi"));
}

#[test]
fn marker_negative_time_is_invalid() {
    let mut p = Project::new("x");
    assert_invalid(add_marker(&mut p, Tick(-1), "x", None));
    assert!(p.markers.is_empty());
}

#[test]
fn marker_label_over_80_chars_is_invalid_and_80_is_ok() {
    let mut p = Project::new("x");
    assert_invalid(add_marker(&mut p, secs(1), &"m".repeat(81), None));
    add_marker(&mut p, secs(1), &"m".repeat(80), None).expect("80 ok");
}

#[test]
fn edit_marker_moves_and_resorts() {
    let mut p = Project::new("x");
    let first = add_marker(&mut p, secs(1), "first", None).expect("add");
    add_marker(&mut p, secs(5), "second", None).expect("add");
    edit_marker(&mut p, &first, Some(secs(10)), None, None).expect("edit");
    let labels: Vec<String> = p.markers.iter().map(|m| m.label.clone()).collect();
    assert_eq!(labels, ["second", "first"]);
}

#[test]
fn edit_marker_label_and_color_clear() {
    let mut p = Project::new("x");
    let id = add_marker(&mut p, secs(1), "old", Some("#123456".to_string())).expect("add");
    edit_marker(&mut p, &id, None, Some("new".to_string()), Some(None)).expect("edit");
    let m = p.markers.iter().find(|m| m.id == id).expect("exists");
    assert_eq!(m.label, "new");
    assert_eq!(m.color, None);
}

#[test]
fn edit_marker_unknown_id_is_not_found() {
    let mut p = Project::new("x");
    let r = edit_marker(&mut p, "nope", Some(secs(1)), None, None);
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

#[test]
fn edit_marker_negative_time_is_invalid_and_unchanged() {
    let mut p = Project::new("x");
    let id = add_marker(&mut p, secs(3), "m", None).expect("add");
    assert_invalid(edit_marker(&mut p, &id, Some(Tick(-5)), None, None));
    assert_eq!(p.markers[0].time, secs(3));
}

#[test]
fn remove_marker_removes_and_unknown_is_not_found() {
    let mut p = Project::new("x");
    let id = add_marker(&mut p, secs(3), "m", None).expect("add");
    remove_marker(&mut p, &id).expect("removed");
    assert!(p.markers.is_empty());
    let r = remove_marker(&mut p, &id);
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

// ----- add_title / add_solid -------------------------------------------------------------------

#[test]
fn add_title_creates_title_clip_on_video_track() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let text = TextSpec {
        text: "Hello".to_string(),
        ..TextSpec::default()
    };
    let id = add_title(&mut p, &vv, secs(1), secs(2), text).expect("ok");
    let c = clip_ref(&p, &id);
    assert_eq!(c.kind, ClipKind::Title);
    assert_eq!(c.start, secs(1));
    assert_eq!(c.dur, secs(2));
    assert_eq!(c.track_id, vv);
    assert_eq!(c.name, "Hello");
}

#[test]
fn title_name_is_first_trimmed_line_capped_at_30_chars() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let long_first = format!("   {}\nsecond line", "w".repeat(40));
    let text = TextSpec {
        text: long_first,
        ..TextSpec::default()
    };
    let id = add_title(&mut p, &vv, secs(0), secs(1), text).expect("ok");
    assert_eq!(clip_ref(&p, &id).name.chars().count(), 30);
}

#[test]
fn empty_title_text_gets_default_name() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let text = TextSpec {
        text: "   ".to_string(),
        ..TextSpec::default()
    };
    let id = add_title(&mut p, &vv, secs(0), secs(1), text).expect("ok");
    assert_eq!(clip_ref(&p, &id).name, "Title");
}

#[test]
fn title_text_over_500_chars_is_invalid() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let text = TextSpec {
        text: "t".repeat(501),
        ..TextSpec::default()
    };
    assert_invalid(add_title(&mut p, &vv, secs(0), secs(1), text));
    assert!(p.clips.is_empty());
}

#[test]
fn title_size_range_is_8_to_1000() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let small = TextSpec {
        size: 7.0,
        ..TextSpec::default()
    };
    assert_invalid(add_title(&mut p, &vv, secs(0), secs(1), small));
    let big = TextSpec {
        size: 1001.0,
        ..TextSpec::default()
    };
    assert_invalid(add_title(&mut p, &vv, secs(0), secs(1), big));
}

#[test]
fn title_on_audio_track_is_wrong_kind() {
    let mut p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let r = add_title(&mut p, &va, secs(0), secs(1), TextSpec::default());
    assert!(matches!(r, Err(EditError::WrongTrackKind(_))), "got {r:?}");
}

#[test]
fn title_overlapping_existing_clip_overwrites_it() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    add_title(&mut p, &vv, secs(1), secs(2), TextSpec::default()).expect("title goes on top");
    // The video clip [0, 2) keeps only [0, 1); the title covers [1, 3).
    assert_eq!(clip_ref(&p, "v").dur, secs(1));
    assert_eq!(p.clips.len(), 3);
}

#[test]
fn title_touching_existing_clip_end_does_not_overlap() {
    let mut p = mixed_project();
    let vv = tid(&p, TrackKind::Video, 0);
    add_title(&mut p, &vv, secs(2), secs(1), TextSpec::default()).expect("abutting is fine");
}

#[test]
fn title_on_locked_track_is_locked() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    patch_track(
        &mut p,
        &vv,
        &TrackPatch {
            locked: Some(true),
            ..TrackPatch::default()
        },
    )
    .expect("lock");
    let r = add_title(&mut p, &vv, secs(0), secs(1), TextSpec::default());
    assert!(matches!(r, Err(EditError::Locked(_))), "got {r:?}");
}

#[test]
fn generator_shorter_than_one_frame_is_invalid() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    // Default canvas is 30 fps; one frame is about 0.033 s.
    assert_invalid(add_title(
        &mut p,
        &vv,
        secs(0),
        Tick::from_ms(10),
        TextSpec::default(),
    ));
}

#[test]
fn generator_negative_start_is_invalid() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    assert_invalid(add_title(
        &mut p,
        &vv,
        Tick(-1),
        secs(1),
        TextSpec::default(),
    ));
}

#[test]
fn generator_extending_past_four_hours_is_limit() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let at = Tick(MAX_PROJECT_TICKS - 1);
    let r = add_title(&mut p, &vv, at, secs(10), TextSpec::default());
    assert!(matches!(r, Err(EditError::Limit(_))), "got {r:?}");
}

#[test]
fn add_solid_names_clip_with_uppercase_hex_and_stores_colour() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let color = Rgba {
        r: 0x1a,
        g: 0xbc,
        b: 0x05,
        a: 255,
    };
    let id = add_solid(&mut p, &vv, secs(0), secs(1), color).expect("ok");
    let c = clip_ref(&p, &id);
    assert_eq!(c.kind, ClipKind::Solid);
    assert_eq!(c.name, "Solid #1ABC05");
    assert_eq!(c.solid, Some(color));
}

#[test]
fn add_solid_on_audio_track_is_wrong_kind() {
    let mut p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let r = add_solid(&mut p, &va, secs(0), secs(1), Rgba::BLACK);
    assert!(matches!(r, Err(EditError::WrongTrackKind(_))), "got {r:?}");
}

#[test]
fn add_solid_overlap_overwrites_the_first_solid() {
    let mut p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let first = add_solid(&mut p, &vv, secs(0), secs(2), Rgba::WHITE).expect("first");
    let second = add_solid(&mut p, &vv, secs(1), secs(1), Rgba::BLACK).expect("second goes on top");
    assert_eq!(clip_ref(&p, &first).dur, secs(1));
    assert_eq!(clip_ref(&p, &second).start, secs(1));
    assert_eq!(p.clips.len(), 2);
}

// ----- audio_clip_for --------------------------------------------------------------------------

#[test]
fn audio_clip_for_builds_audio_clip_with_title_in_name() {
    let p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let c = audio_clip_for(&p, "m1", 0, &va, secs(3), secs(1), secs(4)).expect("ok");
    assert_eq!(c.kind, ClipKind::Audio);
    assert_eq!(c.name, "clip.mp4 - Game");
    assert_eq!(c.start, secs(3));
    assert_eq!(c.src_in, secs(1));
    assert_eq!(c.dur, secs(4));
    assert_eq!(c.speed, 1.0);
    assert_eq!(c.audio.source_track, 0);
    assert!(!c.audio.muted, "stream 0 is audible");
}

#[test]
fn audio_clip_for_stream_other_than_zero_starts_muted() {
    let p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let c = audio_clip_for(&p, "m1", 1, &va, secs(0), secs(0), secs(1)).expect("ok");
    assert!(c.audio.muted);
    assert_eq!(c.audio.source_track, 1);
    // Blank title falls back to the media name alone.
    assert_eq!(c.name, "clip.mp4");
}

#[test]
fn audio_clip_for_missing_stream_is_invalid() {
    let p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let r = audio_clip_for(&p, "m1", 5, &va, secs(0), secs(0), secs(1));
    assert_invalid(r);
}

#[test]
fn audio_clip_for_image_media_is_invalid() {
    let mut p = project();
    p.media.push(MediaRef {
        id: "img".to_string(),
        kind: MediaKind::Image,
        name: "still.png".to_string(),
        ..MediaRef::default()
    });
    let va = tid(&p, TrackKind::Audio, 0);
    assert_invalid(audio_clip_for(&p, "img", 0, &va, secs(0), secs(0), secs(1)));
}

#[test]
fn audio_clip_for_video_track_is_wrong_kind() {
    let p = project();
    let vv = tid(&p, TrackKind::Video, 0);
    let r = audio_clip_for(&p, "m1", 0, &vv, secs(0), secs(0), secs(1));
    assert!(matches!(r, Err(EditError::WrongTrackKind(_))), "got {r:?}");
}

#[test]
fn audio_clip_for_zero_or_negative_length_is_invalid() {
    let p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    assert_invalid(audio_clip_for(
        &p,
        "m1",
        0,
        &va,
        secs(0),
        secs(0),
        Tick::ZERO,
    ));
    assert_invalid(audio_clip_for(&p, "m1", 0, &va, Tick(-1), secs(0), secs(1)));
}

#[test]
fn audio_clip_for_unknown_media_is_not_found() {
    let p = project();
    let va = tid(&p, TrackKind::Audio, 0);
    let r = audio_clip_for(&p, "ghost", 0, &va, secs(0), secs(0), secs(1));
    assert!(matches!(r, Err(EditError::NotFound(_))), "got {r:?}");
}

// ----- misc: fade curve and clip helpers used through the public patch API ---------------------

#[test]
fn fade_curve_patch_applies_and_fades_inside_clip_are_accepted() {
    let mut p = mixed_project();
    let patch = ClipPatch {
        fade_in: Some(secs(1)),
        fade_out: Some(secs(1)),
        fade_curve: Some(FadeCurve::EqualPower),
        ..ClipPatch::default()
    };
    patch_clips(&mut p, &ids(&["a"]), &patch).expect("1 s + 1 s fits in 2 s");
    assert_eq!(clip_ref(&p, "a").audio.fade_curve, FadeCurve::EqualPower);
}

#[test]
fn track_struct_default_is_video_unlocked_height_56() {
    let t = Track::default();
    assert_eq!(t.kind, TrackKind::Video);
    assert!(!t.locked);
    assert_eq!(t.height, 56);
    assert_eq!(t.volume_db, 0.0);
}

#[test]
fn envelope_point_at_the_clip_end_is_dropped_like_a_trimmed_fade() {
    let mut p = mixed_project();
    let dur = clip_ref(&p, "a").dur;
    let pts = vec![
        EnvPoint {
            t: Tick::ZERO,
            db: -6.0,
        },
        EnvPoint { t: dur, db: -9.0 },
    ];
    set_envelope(&mut p, "a", pts).expect("envelope ok");
    let env = &clip_ref(&p, "a").audio.envelope;
    assert_eq!(env.len(), 1, "a point at t == dur must go: {env:?}");
    assert_eq!(env[0].t, Tick::ZERO);
}
