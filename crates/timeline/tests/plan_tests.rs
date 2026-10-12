//! Render plan tests (public API only): derive_plan segmentation, skipping rules, z order, source
//! time with speed, reverse and freeze, range clipping, and source_time_at exact results.
//! Helpers are inline on purpose (other testers write sibling files at the same time).

use clipcheck_timeline::model::{
    Canvas, Clip, ClipAudio, ClipKind, MediaAudio, MediaKind, MediaRef, Project, Rgba, TextSpec,
    TrackKind, Transform, Transition, TransitionKind,
};
use clipcheck_timeline::ops_props::track_of_kind;
use clipcheck_timeline::plan::{derive_plan, source_time_at, RenderPlan};
use clipcheck_timeline::time::Tick;

// ----- helpers ---------------------------------------------------------------------------------

fn secs(n: i64) -> Tick {
    Tick::from_secs(n)
}

fn tid(p: &Project, kind: TrackKind, idx: usize) -> String {
    track_of_kind(p, kind, idx)
        .expect("track exists")
        .id
        .clone()
}

/// Default project (V1..V3, A1..A4) plus media "m1" (60 s video with 2 audio streams) and
/// "gone" (missing from disk).
fn project() -> Project {
    let mut p = Project::new("plan");
    p.media.push(MediaRef {
        id: "m1".to_string(),
        kind: MediaKind::Video,
        name: "m1.mp4".to_string(),
        duration: secs(60),
        audio: vec![MediaAudio::default(), MediaAudio::default()],
        ..MediaRef::default()
    });
    p.media.push(MediaRef {
        id: "gone".to_string(),
        kind: MediaKind::Video,
        name: "gone.mp4".to_string(),
        duration: secs(60),
        missing: true,
        ..MediaRef::default()
    });
    p
}

fn vclip(id: &str, track: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: track.to_string(),
        kind: ClipKind::Video,
        media_id: Some("m1".to_string()),
        start,
        dur,
        ..Clip::default()
    }
}

fn aclip(id: &str, track: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: track.to_string(),
        kind: ClipKind::Audio,
        media_id: Some("m1".to_string()),
        start,
        dur,
        ..Clip::default()
    }
}

fn full(p: &Project) -> RenderPlan {
    derive_plan(p, None)
}

fn layer_ids(plan: &RenderPlan, seg: usize) -> Vec<String> {
    plan.segments
        .get(seg)
        .map(|s| s.video.iter().map(|v| v.clip_id.clone()).collect())
        .unwrap_or_default()
}

fn audio_ids(plan: &RenderPlan, seg: usize) -> Vec<String> {
    plan.segments
        .get(seg)
        .map(|s| s.audio.iter().map(|a| a.clip_id.clone()).collect())
        .unwrap_or_default()
}

fn assert_gap_free(plan: &RenderPlan) {
    let Some(first) = plan.segments.first() else {
        return;
    };
    assert_eq!(
        first.start, plan.start,
        "first segment starts at range start"
    );
    let last = plan.segments.last().expect("non-empty");
    assert_eq!(last.end, plan.end, "last segment ends at range end");
    for w in plan.segments.windows(2) {
        assert_eq!(w[0].end, w[1].start, "no gap or overlap between segments");
    }
    for s in &plan.segments {
        assert!(s.start < s.end, "segments have positive length");
    }
}

// ----- segmentation ----------------------------------------------------------------------------

#[test]
fn empty_project_gives_no_segments() {
    let p = project();
    let plan = full(&p);
    assert!(plan.segments.is_empty());
    assert_eq!(plan.start, Tick::ZERO);
    assert_eq!(plan.end, Tick::ZERO);
}

#[test]
fn single_clip_gives_one_segment_covering_it() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(2)));
    let plan = full(&p);
    assert_eq!(plan.segments.len(), 1);
    assert_eq!(plan.segments[0].start, secs(0));
    assert_eq!(plan.segments[0].end, secs(2));
    assert_eq!(layer_ids(&plan, 0), ["a"]);
    assert_gap_free(&plan);
}

#[test]
fn segments_are_gap_free_and_cover_the_timeline() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    p.clips.push(vclip("a", &v1, secs(0), secs(3)));
    p.clips.push(vclip("b", &v2, secs(1), secs(4)));
    let plan = full(&p);
    assert_gap_free(&plan);
    assert_eq!(plan.end, secs(5));
}

#[test]
fn cut_points_fall_on_clip_edges() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    p.clips.push(vclip("a", &v1, secs(0), secs(3)));
    p.clips.push(vclip("b", &v2, secs(1), secs(1)));
    let plan = full(&p);
    let bounds: Vec<(Tick, Tick)> = plan.segments.iter().map(|s| (s.start, s.end)).collect();
    assert_eq!(
        bounds,
        vec![(secs(0), secs(1)), (secs(1), secs(2)), (secs(2), secs(3))]
    );
    assert_eq!(layer_ids(&plan, 0), ["a"]);
    assert_eq!(layer_ids(&plan, 1), ["a", "b"]);
    assert_eq!(layer_ids(&plan, 2), ["a"]);
}

#[test]
fn adjacent_identical_content_segments_are_not_merged() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(1)));
    p.clips.push(vclip("b", &v1, secs(1), secs(1)));
    let plan = full(&p);
    assert_eq!(
        plan.segments.len(),
        2,
        "clip boundary creates a cut even with same layer"
    );
    assert_gap_free(&plan);
}

#[test]
fn gap_between_clips_still_gives_contiguous_segments_with_empty_content() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(1)));
    p.clips.push(vclip("b", &v1, secs(3), secs(1)));
    let plan = full(&p);
    assert_gap_free(&plan);
    assert_eq!(plan.end, secs(4));
    let gap = plan
        .segments
        .iter()
        .find(|s| s.start == secs(1) && s.end == secs(3))
        .expect("gap segment exists");
    assert!(gap.video.is_empty() && gap.audio.is_empty());
}

// ----- skipping rules --------------------------------------------------------------------------

#[test]
fn muted_video_track_is_skipped() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.track_mut(&v1).expect("t").muted = true;
    p.clips.push(vclip("a", &v1, secs(0), secs(2)));
    let plan = full(&p);
    assert!(plan.segments.iter().all(|s| s.video.is_empty()));
}

#[test]
fn hidden_video_track_is_skipped() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.track_mut(&v1).expect("t").hidden = true;
    p.clips.push(vclip("a", &v1, secs(0), secs(2)));
    let plan = full(&p);
    assert!(plan.segments.iter().all(|s| s.video.is_empty()));
}

#[test]
fn muted_audio_track_is_skipped_but_video_still_plays() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    let v1 = tid(&p, TrackKind::Video, 0);
    p.track_mut(&a1).expect("t").muted = true;
    p.clips.push(aclip("au", &a1, secs(0), secs(2)));
    p.clips.push(vclip("vi", &v1, secs(0), secs(2)));
    let plan = full(&p);
    assert!(audio_ids(&plan, 0).is_empty());
    assert_eq!(layer_ids(&plan, 0), ["vi"]);
}

#[test]
fn solo_audio_track_excludes_other_audio_tracks() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    let a2 = tid(&p, TrackKind::Audio, 1);
    p.track_mut(&a2).expect("t").solo = true;
    p.clips.push(aclip("one", &a1, secs(0), secs(2)));
    p.clips.push(aclip("two", &a2, secs(0), secs(2)));
    let plan = full(&p);
    assert_eq!(audio_ids(&plan, 0), ["two"]);
}

#[test]
fn solo_video_track_excludes_other_video_tracks() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    p.track_mut(&v2).expect("t").solo = true;
    p.clips.push(vclip("one", &v1, secs(0), secs(2)));
    p.clips.push(vclip("two", &v2, secs(0), secs(2)));
    let plan = full(&p);
    assert_eq!(layer_ids(&plan, 0), ["two"]);
}

#[test]
fn disabled_clip_is_skipped() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let mut c = vclip("a", &v1, secs(0), secs(2));
    c.enabled = false;
    p.clips.push(c);
    let plan = full(&p);
    assert!(plan.segments.iter().all(|s| s.video.is_empty()));
}

#[test]
fn audio_clip_with_audio_muted_is_skipped() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    let mut c = aclip("au", &a1, secs(0), secs(2));
    c.audio = ClipAudio {
        muted: true,
        ..ClipAudio::default()
    };
    p.clips.push(c);
    let plan = full(&p);
    assert!(audio_ids(&plan, 0).is_empty());
}

#[test]
fn clip_using_missing_media_is_skipped() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let mut c = vclip("a", &v1, secs(0), secs(2));
    c.media_id = Some("gone".to_string());
    p.clips.push(c);
    let plan = full(&p);
    assert!(plan.segments.iter().all(|s| s.video.is_empty()));
}

#[test]
fn clip_referencing_unknown_media_is_skipped() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    let mut c = aclip("au", &a1, secs(0), secs(2));
    c.media_id = Some("nope".to_string());
    p.clips.push(c);
    let plan = full(&p);
    assert!(audio_ids(&plan, 0).is_empty());
}

#[test]
fn title_clip_without_media_is_kept() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(Clip {
        id: "t".to_string(),
        track_id: v1,
        kind: ClipKind::Title,
        start: secs(0),
        dur: secs(2),
        text: Some(TextSpec {
            text: "Hi".to_string(),
            ..TextSpec::default()
        }),
        ..Clip::default()
    });
    let plan = full(&p);
    assert_eq!(layer_ids(&plan, 0), ["t"]);
    let layer = plan
        .segments
        .first()
        .and_then(|s| s.video.first())
        .expect("layer");
    assert_eq!(layer.media_id, None);
    assert_eq!(layer.text.as_ref().map(|t| t.text.as_str()), Some("Hi"));
}

#[test]
fn video_clip_placed_on_audio_track_is_not_in_plan() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    p.clips.push(vclip("odd", &a1, secs(0), secs(2)));
    let plan = full(&p);
    assert!(plan
        .segments
        .iter()
        .all(|s| s.video.is_empty() && s.audio.is_empty()));
}

// ----- z order and audio sources ---------------------------------------------------------------

#[test]
fn layers_are_sorted_by_z_with_z_as_video_track_index() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v3 = tid(&p, TrackKind::Video, 2);
    // Push the upper track's clip first to check the plan re-sorts by z.
    p.clips.push(vclip("top", &v3, secs(0), secs(2)));
    p.clips.push(vclip("bottom", &v1, secs(0), secs(2)));
    let plan = full(&p);
    let seg = plan.segments.first().expect("segment");
    let zs: Vec<u32> = seg.video.iter().map(|v| v.z).collect();
    assert_eq!(zs, vec![0, 2]);
    assert_eq!(layer_ids(&plan, 0), ["bottom", "top"]);
}

#[test]
fn audio_source_carries_track_index_and_track_fader() {
    let mut p = project();
    let a2 = tid(&p, TrackKind::Audio, 1);
    p.track_mut(&a2).expect("t").volume_db = -4.5;
    p.clips.push(aclip("au", &a2, secs(0), secs(2)));
    let plan = full(&p);
    let src = plan
        .segments
        .first()
        .and_then(|s| s.audio.first())
        .expect("source");
    assert_eq!(src.track_index, 1);
    assert_eq!(src.track_id, a2);
    assert_eq!(src.track_db, -4.5);
    assert_eq!(src.media_id, "m1");
}

#[test]
fn audio_source_copies_clip_audio_and_source_stream() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    let mut c = aclip("au", &a1, secs(0), secs(2));
    c.audio = ClipAudio {
        source_track: 1,
        gain_db: -3.0,
        pan: 0.5,
        mono: true,
        ..ClipAudio::default()
    };
    p.clips.push(c);
    let plan = full(&p);
    let src = plan
        .segments
        .first()
        .and_then(|s| s.audio.first())
        .expect("source");
    assert_eq!(src.source_track, 1);
    assert_eq!(src.audio.gain_db, -3.0);
    assert_eq!(src.audio.pan, 0.5);
    assert!(src.audio.mono);
    assert_eq!(src.clip_start, secs(0));
    assert_eq!(src.clip_dur, secs(2));
}

// ----- source time -----------------------------------------------------------------------------

#[test]
fn speed_two_advances_source_twice_as_fast() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    let mut fast = vclip("fast", &v1, secs(0), secs(4));
    fast.speed = 2.0;
    p.clips.push(fast);
    // Cut at 1 s so a segment starts there.
    p.clips.push(vclip("cut", &v2, secs(1), secs(1)));
    let plan = full(&p);
    let seg = plan
        .segments
        .iter()
        .find(|s| s.start == secs(1))
        .expect("seg at 1 s");
    let layer = seg
        .video
        .iter()
        .find(|v| v.clip_id == "fast")
        .expect("fast layer");
    assert_eq!(layer.src_time, secs(2));
    assert_eq!(layer.speed, 2.0);
}

#[test]
fn speed_half_advances_source_slowly() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    let mut slow = vclip("slow", &v1, secs(0), secs(4));
    slow.speed = 0.5;
    p.clips.push(slow);
    p.clips.push(vclip("cut", &v2, secs(2), secs(1)));
    let plan = full(&p);
    let seg = plan
        .segments
        .iter()
        .find(|s| s.start == secs(2))
        .expect("seg at 2 s");
    let layer = seg
        .video
        .iter()
        .find(|v| v.clip_id == "slow")
        .expect("slow layer");
    assert_eq!(layer.src_time, secs(1));
}

#[test]
fn reverse_clip_plays_source_backwards_from_src_out() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    let mut rev = vclip("rev", &v1, secs(0), secs(4));
    rev.reverse = true;
    p.clips.push(rev);
    p.clips.push(vclip("cut", &v2, secs(1), secs(1)));
    let plan = full(&p);
    let seg = plan
        .segments
        .iter()
        .find(|s| s.start == secs(1))
        .expect("seg at 1 s");
    let layer = seg
        .video
        .iter()
        .find(|v| v.clip_id == "rev")
        .expect("rev layer");
    // src_in 0, src_out 4 s; one second into the clip the source is at 3 s.
    assert_eq!(layer.src_time, secs(3));
    assert!(layer.reverse);
}

#[test]
fn freeze_clip_always_shows_src_in() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    let mut frozen = vclip("frz", &v1, secs(0), secs(4));
    frozen.speed = 0.0;
    frozen.src_in = secs(7);
    p.clips.push(frozen);
    p.clips.push(vclip("cut", &v2, secs(1), secs(1)));
    let plan = full(&p);
    for seg in &plan.segments {
        if let Some(layer) = seg.video.iter().find(|v| v.clip_id == "frz") {
            assert_eq!(
                layer.src_time,
                secs(7),
                "freeze shows src_in at every segment"
            );
            assert_eq!(layer.speed, 0.0);
        }
    }
}

#[test]
fn segment_source_time_accounts_for_src_in_offset() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let v2 = tid(&p, TrackKind::Video, 1);
    let mut c = vclip("a", &v1, secs(0), secs(4));
    c.src_in = secs(10);
    p.clips.push(c);
    p.clips.push(vclip("cut", &v2, secs(3), secs(1)));
    let plan = full(&p);
    let seg = plan
        .segments
        .iter()
        .find(|s| s.start == secs(3))
        .expect("seg");
    let layer = seg.video.iter().find(|v| v.clip_id == "a").expect("a");
    assert_eq!(layer.src_time, secs(13));
}

#[test]
fn audio_source_src_time_follows_speed() {
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    let v1 = tid(&p, TrackKind::Video, 0);
    let mut au = aclip("au", &a1, secs(0), secs(4));
    au.speed = 2.0;
    p.clips.push(au);
    p.clips.push(vclip("cut", &v1, secs(1), secs(1)));
    let plan = full(&p);
    let seg = plan
        .segments
        .iter()
        .find(|s| s.start == secs(1))
        .expect("seg");
    let src = seg.audio.first().expect("audio");
    assert_eq!(src.src_time, secs(2));
    assert_eq!(src.speed, 2.0);
}

// ----- source_time_at exact results ------------------------------------------------------------

fn clip_at(start: Tick, dur: Tick, src_in: Tick, speed: f64, reverse: bool) -> Clip {
    Clip {
        start,
        dur,
        src_in,
        speed,
        reverse,
        ..Clip::default()
    }
}

#[test]
fn source_time_at_offsets_from_src_in_at_speed_one() {
    let c = clip_at(secs(1), secs(2), secs(5), 1.0, false);
    assert_eq!(source_time_at(&c, secs(2)), secs(6));
    assert_eq!(source_time_at(&c, secs(3)), secs(7));
}

#[test]
fn source_time_at_clamps_time_into_the_clip() {
    let c = clip_at(secs(1), secs(2), secs(5), 1.0, false);
    assert_eq!(
        source_time_at(&c, secs(0)),
        secs(5),
        "before start gives src_in"
    );
    assert_eq!(
        source_time_at(&c, secs(10)),
        secs(7),
        "after end gives src_in + len"
    );
}

#[test]
fn source_time_at_speed_two_is_double_elapsed() {
    let c = clip_at(secs(0), secs(3), secs(0), 2.0, false);
    assert_eq!(source_time_at(&c, secs(2)), secs(4));
}

#[test]
fn source_time_at_reverse_starts_at_src_out() {
    let c = clip_at(secs(0), secs(4), secs(1), 1.0, true);
    // src_out = 1 + 4 = 5 s.
    assert_eq!(source_time_at(&c, secs(0)), secs(5));
    assert_eq!(source_time_at(&c, secs(1)), secs(4));
    assert_eq!(source_time_at(&c, secs(4)), secs(1));
}

#[test]
fn source_time_at_zero_speed_is_freeze() {
    let c = clip_at(secs(0), secs(4), secs(3), 0.0, false);
    assert_eq!(source_time_at(&c, secs(2)), secs(3));
}

#[test]
fn source_time_at_negative_or_nan_speed_is_freeze() {
    let neg = clip_at(secs(0), secs(4), secs(3), -1.0, false);
    assert_eq!(source_time_at(&neg, secs(2)), secs(3));
    let nan = clip_at(secs(0), secs(4), secs(3), f64::NAN, false);
    assert_eq!(source_time_at(&nan, secs(2)), secs(3));
}

#[test]
fn source_time_at_is_never_negative() {
    let c = clip_at(secs(0), secs(4), Tick::ZERO, 1.0, true);
    assert!(source_time_at(&c, secs(3)) >= Tick::ZERO);
}

// ----- range handling --------------------------------------------------------------------------

#[test]
fn explicit_range_limits_segments_and_keeps_src_time_consistent() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(3)));
    let plan = derive_plan(&p, Some((secs(1), secs(2))));
    assert_eq!(plan.start, secs(1));
    assert_eq!(plan.end, secs(2));
    assert_eq!(plan.segments.len(), 1);
    assert_eq!(plan.segments[0].video[0].src_time, secs(1));
    assert_gap_free(&plan);
}

#[test]
fn range_end_beyond_timeline_is_clamped_to_timeline_end() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(2)));
    let plan = derive_plan(&p, Some((Tick::ZERO, secs(100))));
    assert_eq!(plan.end, secs(2));
    assert_gap_free(&plan);
}

#[test]
fn range_start_at_or_after_end_gives_empty_plan() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(2)));
    assert!(derive_plan(&p, Some((secs(2), secs(2))))
        .segments
        .is_empty());
    assert!(derive_plan(&p, Some((secs(1), secs(0))))
        .segments
        .is_empty());
    assert!(derive_plan(&p, Some((secs(5), secs(9))))
        .segments
        .is_empty());
}

#[test]
fn negative_range_start_is_clamped_to_zero() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(2)));
    let plan = derive_plan(&p, Some((Tick(-secs(3).0), secs(1))));
    assert_eq!(plan.start, Tick::ZERO);
    assert_eq!(plan.end, secs(1));
}

#[test]
fn range_is_clipped_so_clip_starting_before_it_still_plays_with_offset_src() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let mut c = vclip("a", &v1, secs(0), secs(4));
    c.src_in = secs(2);
    p.clips.push(c);
    let plan = derive_plan(&p, Some((secs(2), secs(3))));
    assert_eq!(plan.segments.len(), 1);
    assert_eq!(plan.segments[0].video[0].src_time, secs(4));
}

// ----- passthrough fields and canvas -----------------------------------------------------------

#[test]
fn transform_opacity_transition_are_copied_into_layer() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    let mut c = vclip("a", &v1, secs(0), secs(2));
    c.opacity = 0.4;
    c.transform = Transform {
        scale: 1.5,
        rotate_90: 1,
        ..Transform::default()
    };
    let tr = Transition {
        kind: TransitionKind::DipToWhite,
        duration: secs(1),
    };
    c.transition_in = Some(tr);
    p.clips.push(c);
    let plan = full(&p);
    let layer = plan
        .segments
        .first()
        .and_then(|s| s.video.first())
        .expect("layer");
    assert_eq!(layer.opacity, 0.4);
    assert_eq!(layer.transform.scale, 1.5);
    assert_eq!(layer.transform.rotate_90, 1);
    assert_eq!(layer.transition, Some(tr));
    assert_eq!(layer.clip_start, secs(0));
}

#[test]
fn solid_layer_carries_its_colour() {
    let mut p = project();
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(Clip {
        id: "s".to_string(),
        track_id: v1,
        kind: ClipKind::Solid,
        start: secs(0),
        dur: secs(1),
        solid: Some(Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 255,
        }),
        ..Clip::default()
    });
    let plan = full(&p);
    let layer = plan
        .segments
        .first()
        .and_then(|s| s.video.first())
        .expect("layer");
    assert_eq!(
        layer.solid,
        Some(Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 255
        })
    );
    assert_eq!(layer.media_id, None);
}

#[test]
fn plan_carries_a_copy_of_the_canvas() {
    let mut p = project();
    p.canvas = Canvas {
        width: 1280,
        height: 720,
        ..Canvas::default()
    };
    let v1 = tid(&p, TrackKind::Video, 0);
    p.clips.push(vclip("a", &v1, secs(0), secs(1)));
    let plan = full(&p);
    assert_eq!(plan.canvas.width, 1280);
    assert_eq!(plan.canvas.height, 720);
}

#[test]
fn track_struct_identity_is_used_for_audio_index() {
    // Audio index counts audio tracks only, so a video track in front does not shift it.
    let mut p = project();
    let a1 = tid(&p, TrackKind::Audio, 0);
    p.clips.push(aclip("au", &a1, secs(0), secs(1)));
    let plan = full(&p);
    assert_eq!(plan.segments[0].audio[0].track_index, 0);
}
