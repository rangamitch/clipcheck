//! Snapping and audio gain maths tests (public API only).
//! Helpers are inline on purpose (other testers write sibling files at the same time).

use clipcheck_timeline::audiomath::{
    clip_gain_at, clip_gain_at_timeline, db_to_lin, envelope_db_at, fade_factor, lin_to_db,
    pan_gains,
};
use clipcheck_timeline::model::{
    Clip, ClipAudio, ClipKind, EnvPoint, FadeCurve, Marker, Project, WorkArea,
};
use clipcheck_timeline::snap::{snap_block, snap_points, snap_time, SnapKind, SnapPoint};
use clipcheck_timeline::time::Tick;

// ----- helpers ---------------------------------------------------------------------------------

fn secs(n: i64) -> Tick {
    Tick::from_secs(n)
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn sp(time: Tick, kind: SnapKind) -> SnapPoint {
    SnapPoint { time, kind }
}

fn clip(id: &str, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: "t".to_string(),
        kind: ClipKind::Video,
        start,
        dur,
        ..Clip::default()
    }
}

fn audio(gain_db: f64) -> ClipAudio {
    ClipAudio {
        gain_db,
        ..ClipAudio::default()
    }
}

fn env(points: &[(Tick, f64)]) -> Vec<EnvPoint> {
    points.iter().map(|&(t, db)| EnvPoint { t, db }).collect()
}

// ----- snap_points -----------------------------------------------------------------------------

#[test]
fn snap_points_include_origin_playhead_and_clip_edges() {
    let mut p = Project::new("s");
    p.clips.push(clip("a", secs(2), secs(3)));
    let pts = snap_points(&p, &[], secs(7));
    assert!(pts.contains(&sp(Tick::ZERO, SnapKind::Origin)));
    assert!(pts.contains(&sp(secs(7), SnapKind::Playhead)));
    assert!(pts.contains(&sp(secs(2), SnapKind::ClipEdge)));
    assert!(pts.contains(&sp(secs(5), SnapKind::ClipEdge)));
}

#[test]
fn snap_points_are_sorted_by_time() {
    let mut p = Project::new("s");
    p.clips.push(clip("a", secs(9), secs(1)));
    p.clips.push(clip("b", secs(1), secs(1)));
    p.markers.push(Marker {
        time: secs(4),
        label: "m".to_string(),
        ..Marker::default()
    });
    let pts = snap_points(&p, &[], secs(3));
    assert!(pts.windows(2).all(|w| w[0].time <= w[1].time));
}

#[test]
fn snap_points_include_markers_and_work_area_edges() {
    let mut p = Project::new("s");
    p.markers.push(Marker {
        time: secs(4),
        label: "m".to_string(),
        ..Marker::default()
    });
    p.work_area = Some(WorkArea {
        start: secs(1),
        end: secs(8),
    });
    let pts = snap_points(&p, &[], Tick::ZERO);
    assert!(pts.contains(&sp(secs(4), SnapKind::Marker)));
    assert!(pts.contains(&sp(secs(1), SnapKind::WorkArea)));
    assert!(pts.contains(&sp(secs(8), SnapKind::WorkArea)));
}

#[test]
fn snap_points_exclude_listed_clips_only() {
    let mut p = Project::new("s");
    p.clips.push(clip("keep", secs(1), secs(1)));
    p.clips.push(clip("skip", secs(5), secs(1)));
    let pts = snap_points(&p, &["skip".to_string()], Tick::ZERO);
    assert!(pts
        .iter()
        .any(|x| x.time == secs(1) && x.kind == SnapKind::ClipEdge));
    assert!(!pts
        .iter()
        .any(|x| x.time == secs(5) && x.kind == SnapKind::ClipEdge));
    assert!(!pts
        .iter()
        .any(|x| x.time == secs(6) && x.kind == SnapKind::ClipEdge));
}

#[test]
fn snap_points_skip_disabled_clips() {
    let mut p = Project::new("s");
    let mut c = clip("off", secs(3), secs(1));
    c.enabled = false;
    p.clips.push(c);
    let pts = snap_points(&p, &[], Tick::ZERO);
    assert!(!pts.iter().any(|x| x.kind == SnapKind::ClipEdge));
}

#[test]
fn snap_points_dedup_same_time_and_kind() {
    let mut p = Project::new("s");
    // Two clips starting at 2 s produce one ClipEdge point at 2 s.
    p.clips.push(clip("a", secs(2), secs(1)));
    p.clips.push(clip("b", secs(2), secs(4)));
    let pts = snap_points(&p, &[], Tick::ZERO);
    let at_two = pts
        .iter()
        .filter(|x| x.time == secs(2) && x.kind == SnapKind::ClipEdge)
        .count();
    assert_eq!(at_two, 1);
}

#[test]
fn snap_points_keep_different_kinds_at_the_same_time() {
    let mut p = Project::new("s");
    p.markers.push(Marker {
        time: secs(3),
        label: "m".to_string(),
        ..Marker::default()
    });
    p.clips.push(clip("a", secs(3), secs(1)));
    let pts = snap_points(&p, &[], secs(3));
    assert!(pts.contains(&sp(secs(3), SnapKind::Playhead)));
    assert!(pts.contains(&sp(secs(3), SnapKind::ClipEdge)));
    assert!(pts.contains(&sp(secs(3), SnapKind::Marker)));
}

#[test]
fn snap_points_for_empty_project_are_origin_and_playhead() {
    let p = Project::new("s");
    let pts = snap_points(&p, &[], secs(2));
    assert_eq!(pts.len(), 2);
    assert!(pts.contains(&sp(Tick::ZERO, SnapKind::Origin)));
    assert!(pts.contains(&sp(secs(2), SnapKind::Playhead)));
}

// ----- snap_time -------------------------------------------------------------------------------

#[test]
fn snap_time_threshold_is_inclusive() {
    let pts = [sp(secs(100), SnapKind::Marker)];
    let edge = Tick(10);
    let at = snap_time(Tick(secs(100).0 + 10), &pts, edge);
    assert_eq!(at, Some(sp(secs(100), SnapKind::Marker)));
    let beyond = snap_time(Tick(secs(100).0 + 11), &pts, edge);
    assert_eq!(beyond, None);
}

#[test]
fn snap_time_returns_none_when_nothing_is_close() {
    let pts = [sp(secs(10), SnapKind::ClipEdge)];
    assert_eq!(snap_time(secs(50), &pts, Tick(5)), None);
    assert_eq!(snap_time(secs(50), &[], secs(1000)), None);
}

#[test]
fn snap_time_picks_the_nearest_point() {
    let pts = [
        sp(secs(10), SnapKind::ClipEdge),
        sp(secs(13), SnapKind::Marker),
    ];
    let hit = snap_time(secs(12), &pts, secs(5));
    assert_eq!(hit, Some(sp(secs(13), SnapKind::Marker)));
}

#[test]
fn snap_time_equal_distance_prefers_earlier_point() {
    let pts = [
        sp(secs(110), SnapKind::ClipEdge),
        sp(secs(90), SnapKind::ClipEdge),
    ];
    let hit = snap_time(secs(100), &pts, secs(10));
    assert_eq!(hit.map(|x| x.time), Some(secs(90)));
}

#[test]
fn snap_time_with_zero_threshold_only_hits_exact_matches() {
    let pts = [sp(secs(4), SnapKind::Playhead)];
    assert!(snap_time(secs(4), &pts, Tick::ZERO).is_some());
    assert!(snap_time(Tick(secs(4).0 + 1), &pts, Tick::ZERO).is_none());
}

// ----- snap_block ------------------------------------------------------------------------------

#[test]
fn snap_block_snaps_the_closer_edge() {
    // Start is 3 away from 103, end is 2 away from 198: the end edge wins.
    let pts = [
        sp(secs(103), SnapKind::ClipEdge),
        sp(secs(198), SnapKind::Marker),
    ];
    let (delta, hit) = snap_block(secs(100), secs(200), &pts, secs(5)).expect("snaps");
    assert_eq!(hit.time, secs(198));
    assert_eq!(hit.kind, SnapKind::Marker);
    assert_eq!(delta, secs(198) - secs(200));
    assert_eq!(delta, Tick(-secs(2).0));
}

#[test]
fn snap_block_start_edge_delta_is_point_minus_start() {
    let pts = [sp(secs(98), SnapKind::ClipEdge)];
    let (delta, hit) = snap_block(secs(100), secs(150), &pts, secs(5)).expect("snaps");
    assert_eq!(hit.time, secs(98));
    assert_eq!(delta, Tick(-secs(2).0));
}

#[test]
fn snap_block_end_only_snap_gives_end_relative_delta() {
    let pts = [sp(secs(205), SnapKind::ClipEdge)];
    let (delta, _) = snap_block(secs(100), secs(200), &pts, secs(5)).expect("snaps");
    assert_eq!(delta, secs(5));
}

#[test]
fn snap_block_equal_distance_prefers_start_edge() {
    let pts = [
        sp(secs(98), SnapKind::ClipEdge),
        sp(secs(202), SnapKind::ClipEdge),
    ];
    let (delta, hit) = snap_block(secs(100), secs(200), &pts, secs(5)).expect("snaps");
    assert_eq!(hit.time, secs(98), "start edge wins a tie");
    assert_eq!(delta, Tick(-secs(2).0));
}

#[test]
fn snap_block_none_when_neither_edge_is_close() {
    let pts = [sp(secs(50), SnapKind::ClipEdge)];
    assert!(snap_block(secs(100), secs(200), &pts, secs(5)).is_none());
}

// ----- db <-> linear ---------------------------------------------------------------------------

#[test]
fn zero_db_is_unity_gain() {
    assert_eq!(db_to_lin(0.0), 1.0);
}

#[test]
fn minus_six_db_is_about_half_amplitude() {
    assert!(close(db_to_lin(-6.0206), 0.5, 1e-4));
}

#[test]
fn db_at_or_below_minus_120_is_silence() {
    assert_eq!(db_to_lin(-120.0), 0.0);
    assert_eq!(db_to_lin(-200.0), 0.0);
    assert_eq!(db_to_lin(f64::NAN), 0.0);
}

#[test]
fn db_just_above_minus_120_is_not_silence() {
    assert!(db_to_lin(-119.0) > 0.0);
}

#[test]
fn linear_zero_and_negative_map_to_minus_120_db() {
    assert_eq!(lin_to_db(0.0), -120.0);
    assert_eq!(lin_to_db(-1.0), -120.0);
    assert_eq!(lin_to_db(f64::NAN), -120.0);
}

#[test]
fn lin_to_db_of_unity_is_zero_and_half_is_minus_six() {
    assert!(close(lin_to_db(1.0), 0.0, 1e-12));
    assert!(close(lin_to_db(0.5), -6.0206, 1e-3));
}

#[test]
fn db_and_linear_round_trip() {
    for db in [-40.0, -12.0, -3.5, 0.0, 6.0] {
        assert!(close(lin_to_db(db_to_lin(db)), db, 1e-9), "round trip {db}");
    }
}

// ----- fade_factor -----------------------------------------------------------------------------

#[test]
fn linear_fade_midpoint_is_half() {
    let f = fade_factor(FadeCurve::Linear, secs(1), secs(2));
    assert!(close(f, 0.5, 1e-12));
}

#[test]
fn equal_power_fade_midpoint_is_sin_quarter_pi() {
    let f = fade_factor(FadeCurve::EqualPower, secs(1), secs(2));
    assert!(close(f, std::f64::consts::FRAC_1_SQRT_2, 1e-12));
}

#[test]
fn fade_ends_are_zero_and_one() {
    for curve in [FadeCurve::Linear, FadeCurve::EqualPower] {
        assert!(close(fade_factor(curve, Tick::ZERO, secs(2)), 0.0, 1e-12));
        assert!(close(fade_factor(curve, secs(2), secs(2)), 1.0, 1e-12));
    }
}

#[test]
fn fade_with_zero_or_negative_length_gives_unity() {
    assert_eq!(fade_factor(FadeCurve::Linear, secs(1), Tick::ZERO), 1.0);
    assert_eq!(
        fade_factor(FadeCurve::EqualPower, Tick::ZERO, Tick(-5)),
        1.0
    );
}

#[test]
fn fade_position_is_clamped_into_length() {
    assert_eq!(fade_factor(FadeCurve::Linear, secs(9), secs(2)), 1.0);
    assert_eq!(
        fade_factor(FadeCurve::Linear, Tick(-secs(1).0), secs(2)),
        0.0
    );
}

// ----- envelope_db_at --------------------------------------------------------------------------

#[test]
fn empty_envelope_is_zero_db() {
    assert_eq!(envelope_db_at(&[], secs(1)), 0.0);
}

#[test]
fn single_point_envelope_is_flat() {
    let e = env(&[(secs(1), -4.0)]);
    assert_eq!(envelope_db_at(&e, Tick::ZERO), -4.0);
    assert_eq!(envelope_db_at(&e, secs(1)), -4.0);
    assert_eq!(envelope_db_at(&e, secs(9)), -4.0);
}

#[test]
fn envelope_interpolates_linearly_in_db_between_points() {
    let e = env(&[(secs(0), -6.0), (secs(2), 0.0)]);
    assert!(close(envelope_db_at(&e, secs(1)), -3.0, 1e-9));
    assert!(close(envelope_db_at(&e, Tick::from_ms(500)), -4.5, 1e-9));
}

#[test]
fn envelope_is_flat_before_first_and_after_last_point() {
    let e = env(&[(secs(1), -6.0), (secs(3), 2.0)]);
    assert_eq!(envelope_db_at(&e, Tick::ZERO), -6.0);
    assert_eq!(envelope_db_at(&e, secs(10)), 2.0);
}

#[test]
fn envelope_hits_exact_point_values() {
    let e = env(&[(secs(0), -6.0), (secs(2), 0.0), (secs(4), 3.0)]);
    assert!(close(envelope_db_at(&e, secs(2)), 0.0, 1e-12));
    assert!(close(envelope_db_at(&e, secs(3)), 1.5, 1e-9));
}

// ----- clip_gain_at ----------------------------------------------------------------------------

#[test]
fn muted_clip_gives_zero_gain_everywhere() {
    let a = ClipAudio {
        muted: true,
        gain_db: 12.0,
        ..ClipAudio::default()
    };
    assert_eq!(clip_gain_at(&a, secs(4), secs(1)), 0.0);
}

#[test]
fn default_clip_has_unity_gain() {
    let a = ClipAudio::default();
    assert!(close(clip_gain_at(&a, secs(4), secs(2)), 1.0, 1e-12));
}

#[test]
fn clip_gain_db_applies_as_linear_factor() {
    let a = audio(6.0206);
    assert!(close(clip_gain_at(&a, secs(4), secs(2)), 2.0, 1e-3));
}

#[test]
fn linear_fade_in_ramps_from_silence() {
    let a = ClipAudio {
        fade_in: secs(2),
        fade_curve: FadeCurve::Linear,
        ..ClipAudio::default()
    };
    assert!(close(clip_gain_at(&a, secs(4), Tick::ZERO), 0.0, 1e-12));
    assert!(close(clip_gain_at(&a, secs(4), secs(1)), 0.5, 1e-12));
    assert!(close(clip_gain_at(&a, secs(4), secs(2)), 1.0, 1e-12));
}

#[test]
fn linear_fade_out_ramps_to_silence_at_clip_end() {
    let a = ClipAudio {
        fade_out: secs(2),
        fade_curve: FadeCurve::Linear,
        ..ClipAudio::default()
    };
    // 1 s before the end of a 4 s clip is halfway down a 2 s fade.
    assert!(close(clip_gain_at(&a, secs(4), secs(3)), 0.5, 1e-12));
    assert!(close(clip_gain_at(&a, secs(4), secs(4)), 0.0, 1e-12));
}

#[test]
fn equal_power_fade_in_midpoint_is_sin_quarter_pi() {
    let a = ClipAudio {
        fade_in: secs(2),
        fade_curve: FadeCurve::EqualPower,
        ..ClipAudio::default()
    };
    assert!(close(
        clip_gain_at(&a, secs(4), secs(1)),
        std::f64::consts::FRAC_1_SQRT_2,
        1e-12
    ));
}

#[test]
fn envelope_and_clip_gain_multiply_in_db() {
    let a = ClipAudio {
        gain_db: -6.0206,
        envelope: env(&[(Tick::ZERO, -6.0206), (secs(4), -6.0206)]),
        ..ClipAudio::default()
    };
    // -6 dB clip gain plus -6 dB envelope is about -12 dB, a factor of 0.25.
    assert!(close(clip_gain_at(&a, secs(4), secs(1)), 0.25, 1e-3));
}

#[test]
fn clip_gain_time_is_clamped_into_the_clip() {
    let a = ClipAudio {
        fade_in: secs(1),
        fade_curve: FadeCurve::Linear,
        ..ClipAudio::default()
    };
    assert!(close(clip_gain_at(&a, secs(4), secs(-5)), 0.0, 1e-12));
    assert!(close(clip_gain_at(&a, secs(4), secs(99)), 1.0, 1e-12));
}

// ----- clip_gain_at_timeline -------------------------------------------------------------------

#[test]
fn timeline_gain_is_zero_outside_the_clip() {
    let mut c = clip("a", secs(2), secs(2));
    c.kind = ClipKind::Audio;
    c.audio = audio(0.0);
    assert_eq!(clip_gain_at_timeline(&c, secs(1)), 0.0);
    assert_eq!(clip_gain_at_timeline(&c, secs(5)), 0.0);
}

#[test]
fn timeline_gain_end_is_exclusive() {
    let mut c = clip("a", secs(2), secs(2));
    c.kind = ClipKind::Audio;
    assert!(
        close(clip_gain_at_timeline(&c, secs(2)), 1.0, 1e-12),
        "start is inside"
    );
    assert_eq!(clip_gain_at_timeline(&c, secs(4)), 0.0, "end is outside");
}

#[test]
fn timeline_gain_uses_time_from_clip_start() {
    let mut c = clip("a", secs(2), secs(4));
    c.kind = ClipKind::Audio;
    c.audio = ClipAudio {
        fade_in: secs(2),
        fade_curve: FadeCurve::Linear,
        ..ClipAudio::default()
    };
    // Timeline 3 s is 1 s into the clip, halfway through the 2 s fade-in.
    assert!(close(clip_gain_at_timeline(&c, secs(3)), 0.5, 1e-12));
}

// ----- pan_gains -------------------------------------------------------------------------------

#[test]
fn centre_pan_gives_unity_on_both_channels() {
    let (l, r) = pan_gains(0.0);
    assert!(close(l, 1.0, 1e-12));
    assert!(close(r, 1.0, 1e-12));
}

#[test]
fn hard_left_pan_sends_everything_left() {
    let (l, r) = pan_gains(-1.0);
    assert!(close(l, std::f64::consts::SQRT_2, 1e-12));
    assert!(close(r, 0.0, 1e-12));
}

#[test]
fn hard_right_pan_sends_everything_right() {
    let (l, r) = pan_gains(1.0);
    assert!(close(l, 0.0, 1e-12));
    assert!(close(r, std::f64::consts::SQRT_2, 1e-12));
}

#[test]
fn pan_preserves_total_power() {
    for pan in [-1.0, -0.6, -0.3, 0.0, 0.3, 0.6, 1.0] {
        let (l, r) = pan_gains(pan);
        assert!(close(l * l + r * r, 2.0, 1e-9), "power at pan {pan}");
    }
}

#[test]
fn pan_out_of_range_is_clamped_and_nan_is_centre() {
    assert_eq!(pan_gains(5.0), pan_gains(1.0));
    assert_eq!(pan_gains(-5.0), pan_gains(-1.0));
    let (l, r) = pan_gains(f64::NAN);
    assert!(close(l, 1.0, 1e-12) && close(r, 1.0, 1e-12));
}
