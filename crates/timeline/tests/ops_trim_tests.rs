//! Tester tests for `ops_trim`: trim edge, roll, slip, slide, speed, reverse, through-edit join and
//! work area. Written from the doc comments in `src/ops_trim.rs` (public API only).
//!
//! Synthetic data only: media `m1` and `m2` are 10 s long, 30 fps canvas (default), tracks come
//! from `Project::new` (V1, V2, V3, A1..A4). Helpers are inline on purpose (no shared module).

use clipcheck_timeline::error::EditError;
use clipcheck_timeline::model::{
    Clip, ClipKind, EnvPoint, MediaRef, Project, Transition, TransitionKind, WorkArea,
};
use clipcheck_timeline::ops_trim::{
    join_through_edit, roll_edit, set_reverse, set_speed, set_work_area, slide, slip, trim_edge,
    Edge,
};
use clipcheck_timeline::time::{Tick, TICKS_PER_SECOND};

/// One frame at 30 fps (exact: 254_016_000_000 / 30).
const FRAME: i64 = TICKS_PER_SECOND / 30;

fn s(n: i64) -> Tick {
    Tick::from_secs(n)
}

fn ms(n: i64) -> Tick {
    Tick::from_ms(n)
}

fn project() -> Project {
    let mut p = Project::new("trim-tests");
    for id in ["m1", "m2"] {
        p.media.push(MediaRef {
            id: id.to_string(),
            name: format!("{id}.mp4"),
            path: format!("C:/synthetic/{id}.mp4"),
            duration: s(10),
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".to_string(),
            ..MediaRef::default()
        });
    }
    p
}

fn tid(p: &Project, name: &str) -> String {
    p.tracks
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.id.clone())
        .expect("track exists")
}

fn video(p: &Project, id: &str, track: &str, start: Tick, dur: Tick, src_in: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: tid(p, track),
        kind: ClipKind::Video,
        media_id: Some("m1".to_string()),
        start,
        dur,
        src_in,
        ..Clip::default()
    }
}

fn audio(p: &Project, id: &str, track: &str, start: Tick, dur: Tick, src_in: Tick) -> Clip {
    Clip {
        kind: ClipKind::Audio,
        ..video(p, id, track, start, dur, src_in)
    }
}

fn get(p: &Project, id: &str) -> Clip {
    p.clip(id).cloned().expect("clip exists")
}

fn linked(mut c: Clip, link: &str) -> Clip {
    c.link_id = Some(link.to_string());
    c
}

fn grouped(mut c: Clip, group: &str) -> Clip {
    c.group_id = Some(group.to_string());
    c
}

fn with_speed(mut c: Clip, value: f64) -> Clip {
    c.speed = value;
    c
}

/// V1: a = 0..4 (src_in 1 s), b = 4..7 (src_in 2 s). Both on media m1.
fn ab_project() -> Project {
    let mut p = project();
    let a = video(&p, "a", "V1", s(0), s(4), s(1));
    let b = video(&p, "b", "V1", s(4), s(3), s(2));
    p.clips.push(a);
    p.clips.push(b);
    p
}

/// V1: a = 0..4, b = 4..7, c = 7..10 (src_in 1 s).
fn abc_project() -> Project {
    let mut p = ab_project();
    let c = video(&p, "c", "V1", s(7), s(3), s(1));
    p.clips.push(c);
    p
}

// ---------------------------------------------------------------------------------------------
// trim_edge
// ---------------------------------------------------------------------------------------------

#[test]
fn trim_start_speed1_moves_start_dur_and_src_in() {
    let mut p = ab_project();
    trim_edge(&mut p, "a", Edge::Start, s(1), false).expect("trim ok");
    let a = get(&p, "a");
    assert_eq!(a.start, s(1));
    assert_eq!(a.dur, s(3));
    assert_eq!(a.src_in, s(2));
}

#[test]
fn trim_start_speed2_advances_src_in_by_delta_times_speed() {
    let mut p = ab_project();
    p.clip_mut("a").expect("a").speed = 2.0;
    trim_edge(&mut p, "a", Edge::Start, s(1), false).expect("trim ok");
    let a = get(&p, "a");
    assert_eq!(a.dur, s(3));
    assert_eq!(a.src_in, s(3));
    assert_eq!(a.src_out(), s(9));
}

#[test]
fn trim_start_speed_half_advances_src_in_by_half_delta() {
    let mut p = ab_project();
    p.clip_mut("a").expect("a").speed = 0.5;
    trim_edge(&mut p, "a", Edge::Start, s(2), false).expect("trim ok");
    let a = get(&p, "a");
    assert_eq!(a.start, s(2));
    assert_eq!(a.dur, s(2));
    assert_eq!(a.src_in, s(2));
}

#[test]
fn trim_end_changes_dur_only() {
    let mut p = ab_project();
    trim_edge(&mut p, "a", Edge::End, s(3), false).expect("trim ok");
    let a = get(&p, "a");
    assert_eq!(a.start, s(0));
    assert_eq!(a.dur, s(3));
    assert_eq!(a.src_in, s(1));
}

#[test]
fn trim_end_into_neighbour_overwrites_its_head() {
    let mut p = ab_project();
    trim_edge(&mut p, "a", Edge::End, s(5), false).expect("the trimmed clip goes on top");
    assert_eq!(get(&p, "a").dur, s(5));
    // b [4, 7) loses [4, 5): it now starts at 5 and its source moves on by one second.
    let b = get(&p, "b");
    assert_eq!(b.start, s(5));
    assert_eq!(b.dur, s(2));
    assert_eq!(b.src_in, s(3));
}

#[test]
fn trim_start_into_previous_neighbour_overwrites_its_tail() {
    let mut p = ab_project();
    trim_edge(&mut p, "b", Edge::Start, s(3), false).expect("the trimmed clip goes on top");
    // b now covers [3, 7); a [0, 4) keeps [0, 3).
    let a = get(&p, "a");
    assert_eq!(a.start, s(0));
    assert_eq!(a.dur, s(3));
    let b = get(&p, "b");
    assert_eq!(b.start, s(3));
    assert_eq!(b.dur, s(4));
}

#[test]
fn trim_end_to_exact_media_end_is_ok_one_more_is_invalid() {
    let mut p = project();
    let c = video(&p, "c", "V2", s(0), s(2), s(6));
    p.clips.push(c);
    trim_edge(&mut p, "c", Edge::End, s(4), false).expect("src_out == media duration is allowed");
    assert_eq!(get(&p, "c").src_out(), s(10));
    let before = p.clone();
    let r = trim_edge(&mut p, "c", Edge::End, s(5), false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn trim_start_before_zero_is_invalid() {
    let mut p = project();
    let c = video(&p, "c", "V2", s(0), s(4), s(5));
    p.clips.push(c);
    let r = trim_edge(&mut p, "c", Edge::Start, ms(-1000), false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn trim_start_before_media_start_is_invalid() {
    let mut p = project();
    let c = video(&p, "c", "V2", s(2), s(4), s(0));
    p.clips.push(c);
    let r = trim_edge(&mut p, "c", Edge::Start, s(1), false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn trim_end_below_one_frame_is_invalid() {
    let mut p = ab_project();
    let r = trim_edge(&mut p, "a", Edge::End, Tick(FRAME - 1), false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn trim_end_to_exactly_one_frame_is_ok() {
    let mut p = ab_project();
    trim_edge(&mut p, "a", Edge::End, Tick(FRAME), false).expect("one frame allowed");
    assert_eq!(get(&p, "a").dur, Tick(FRAME));
}

#[test]
fn trim_start_onto_end_leaves_zero_length_and_is_invalid() {
    let mut p = ab_project();
    let r = trim_edge(&mut p, "a", Edge::Start, s(4), false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn trim_on_locked_track_is_locked_and_atomic() {
    let mut p = ab_project();
    let v1 = tid(&p, "V1");
    p.track_mut(&v1).expect("V1").locked = true;
    let before = p.clone();
    let r = trim_edge(&mut p, "a", Edge::End, s(3), false);
    assert_eq!(r, Err(EditError::Locked(v1)));
    assert_eq!(p, before);
}

#[test]
fn trim_clamps_fades_to_new_length_shrinking_fade_in_then_out() {
    let mut p = ab_project();
    {
        let a = p.clip_mut("a").expect("a");
        a.audio.fade_in = s(3);
        a.audio.fade_out = s(0);
    }
    trim_edge(&mut p, "a", Edge::End, s(2), false).expect("trim ok");
    let a = get(&p, "a");
    assert_eq!(a.audio.fade_in, s(2));
    assert_eq!(a.audio.fade_out, s(0));
}

#[test]
fn trim_fade_out_shrinks_when_fades_exceed_length() {
    let mut p = ab_project();
    {
        let a = p.clip_mut("a").expect("a");
        a.audio.fade_in = s(1);
        a.audio.fade_out = s(3);
    }
    trim_edge(&mut p, "a", Edge::End, s(2), false).expect("trim ok");
    let a = get(&p, "a");
    assert_eq!(a.audio.fade_in, s(1));
    assert_eq!(a.audio.fade_out, s(1));
}

#[test]
fn trim_drops_envelope_points_after_new_end_and_keeps_the_level_there() {
    let mut p = ab_project();
    p.clip_mut("a").expect("a").audio.envelope = vec![
        EnvPoint { t: s(1), db: -3.0 },
        EnvPoint { t: s(2), db: -6.0 },
        EnvPoint {
            t: ms(3500),
            db: -9.0,
        },
    ];
    trim_edge(&mut p, "a", Edge::End, s(2), false).expect("trim ok");
    let env = get(&p, "a").audio.envelope;
    assert!(env.iter().all(|e| e.t <= s(2)), "nothing past the end");
    let last = env.last().expect("envelope kept");
    assert_eq!(last.t, s(2));
    assert!((last.db + 6.0).abs() < 1e-9, "level at the end is -6 dB");
}

/// An audio clip on A1 at `start` (media m1, source from 0).
fn audio_on_a1(p: &Project, id: &str, start: Tick, dur: Tick, src_in: Tick) -> Clip {
    audio(p, id, "A1", start, dur, src_in)
}

/// Zig-zag envelope (-3 dB, -9 dB alternating every 70 ms) with `n` points, so no point is
/// close to the line through its neighbours.
fn zigzag(n: i64) -> Vec<EnvPoint> {
    (0..n)
        .map(|k| EnvPoint {
            t: ms(k * 70),
            db: if k % 2 == 0 { -3.0 } else { -9.0 },
        })
        .collect()
}

#[test]
fn shortening_an_end_keeps_the_interpolated_level_at_the_new_end() {
    // Envelope -20 dB at 0 rising to 0 dB at 10 s. Trimmed to 5 s the end is -10 dB, not the
    // -20 dB held from the last point that survived.
    let mut p = project();
    let x = audio_on_a1(&p, "x", s(0), s(10), s(0));
    p.clips.push(x);
    p.clip_mut("x").expect("x").audio.envelope = vec![
        EnvPoint { t: s(0), db: -20.0 },
        EnvPoint { t: s(10), db: 0.0 },
    ];
    trim_edge(&mut p, "x", Edge::End, s(5), false).expect("trim ok");
    let env = get(&p, "x").audio.envelope;
    let last = env.last().expect("envelope kept");
    assert_eq!(last.t, s(5));
    assert!((last.db + 10.0).abs() < 1e-9, "got {} dB", last.db);
}

#[test]
fn roll_shortening_the_left_clip_keeps_the_interpolated_end_level() {
    let mut p = project();
    let mut x = audio_on_a1(&p, "x", s(0), s(10), s(0));
    x.audio.envelope = vec![
        EnvPoint { t: s(0), db: -20.0 },
        EnvPoint { t: s(10), db: 0.0 },
    ];
    let y = audio_on_a1(&p, "y", s(10), s(5), s(5));
    p.clips.push(x);
    p.clips.push(y);
    roll_edit(&mut p, "x", "y", s(5)).expect("roll ok");
    let env = get(&p, "x").audio.envelope;
    let last = env.last().expect("envelope kept");
    assert_eq!(last.t, s(5));
    assert!((last.db + 10.0).abs() < 1e-9, "got {} dB", last.db);
}

#[test]
fn slide_shortening_the_left_neighbour_keeps_the_interpolated_end_level() {
    let mut p = project();
    let mut x = audio_on_a1(&p, "x", s(0), s(10), s(0));
    x.audio.envelope = vec![
        EnvPoint { t: s(0), db: -20.0 },
        EnvPoint { t: s(10), db: 0.0 },
    ];
    let y = audio_on_a1(&p, "y", s(10), s(5), s(5));
    p.clips.push(x);
    p.clips.push(y);
    slide(&mut p, "y", s(-5)).expect("slide ok");
    let env = get(&p, "x").audio.envelope;
    let last = env.last().expect("envelope kept");
    assert_eq!(get(&p, "x").dur, s(5));
    assert_eq!(last.t, s(5));
    assert!((last.db + 10.0).abs() < 1e-9, "got {} dB", last.db);
}

#[test]
fn join_caps_the_envelope_at_64_points_and_keeps_the_ends() {
    let mut p = project();
    let mut l = audio_on_a1(&p, "l", s(0), s(5), s(0));
    l.audio.envelope = zigzag(64);
    let mut r = audio_on_a1(&p, "r", s(5), s(5), s(5));
    r.audio.envelope = zigzag(64);
    p.clips.push(l);
    p.clips.push(r);
    join_through_edit(&mut p, "l", "r").expect("join ok");
    let env = get(&p, "l").audio.envelope;
    assert_eq!(env.len(), 64, "capped at MAX_ENV_POINTS");
    assert_eq!(env.first().map(|e| e.t), Some(Tick::ZERO));
    assert_eq!(env.last().map(|e| e.t), Some(s(5) + ms(63 * 70)));
}

#[test]
fn join_with_a_left_point_at_the_seam_gives_no_two_points_the_same_time() {
    let mut p = project();
    // The joined 12 s clip reads 0..12 s of source, so the media must be longer than the fixture's 10 s.
    for m in p.media.iter_mut() {
        m.duration = s(20);
    }
    let mut l = audio_on_a1(&p, "l", s(0), s(10), s(0));
    l.audio.envelope = vec![
        EnvPoint { t: s(0), db: 0.0 },
        EnvPoint { t: s(10), db: -6.0 },
    ];
    let mut r = audio_on_a1(&p, "r", s(10), s(2), s(10));
    r.audio.envelope = vec![
        EnvPoint { t: s(0), db: -3.0 },
        EnvPoint { t: s(1), db: -9.0 },
    ];
    p.clips.push(l);
    p.clips.push(r);
    join_through_edit(&mut p, "l", "r").expect("join ok");
    let env = get(&p, "l").audio.envelope;
    assert!(
        env.windows(2).all(|w| w[0].t < w[1].t),
        "times must be strictly increasing: {env:?}"
    );
    let at_seam: Vec<&EnvPoint> = env.iter().filter(|e| e.t == s(10)).collect();
    assert_eq!(at_seam.len(), 1, "one point at the seam");
    assert!(
        (at_seam[0].db + 6.0).abs() < 1e-9,
        "seam keeps the left level"
    );
}

#[test]
fn trim_reverse_clip_start_edge_keeps_src_in() {
    let mut p = project();
    let mut r = video(&p, "r", "V2", s(0), s(4), s(5));
    r.reverse = true;
    p.clips.push(r);
    trim_edge(&mut p, "r", Edge::Start, s(1), false).expect("trim ok");
    let r = get(&p, "r");
    assert_eq!(r.start, s(1));
    assert_eq!(r.dur, s(3));
    assert_eq!(r.src_in, s(5));
}

#[test]
fn trim_ripple_end_extend_shifts_all_later_clips() {
    let mut p = abc_project();
    trim_edge(&mut p, "a", Edge::End, s(6), true).expect("ripple ok");
    assert_eq!(get(&p, "a").dur, s(6));
    assert_eq!(get(&p, "b").start, s(6));
    assert_eq!(get(&p, "c").start, s(9));
}

#[test]
fn trim_ripple_end_shrink_pulls_later_clips_back() {
    let mut p = abc_project();
    trim_edge(&mut p, "a", Edge::End, s(2), true).expect("ripple ok");
    assert_eq!(get(&p, "b").start, s(2));
    assert_eq!(get(&p, "c").start, s(5));
}

#[test]
fn trim_ripple_start_keeps_clip_start_and_shifts_later_clips_by_minus_delta() {
    let mut p = abc_project();
    trim_edge(&mut p, "a", Edge::Start, s(1), true).expect("ripple ok");
    let a = get(&p, "a");
    assert_eq!(a.start, s(0));
    assert_eq!(a.dur, s(3));
    assert_eq!(a.src_in, s(2));
    assert_eq!(get(&p, "b").start, s(3));
    assert_eq!(get(&p, "c").start, s(6));
}

#[test]
fn trim_without_ripple_leaves_later_clips_in_place() {
    let mut p = abc_project();
    trim_edge(&mut p, "a", Edge::End, s(2), false).expect("trim ok");
    assert_eq!(get(&p, "b").start, s(4));
    assert_eq!(get(&p, "c").start, s(7));
}

#[test]
fn trim_end_moves_linked_audio_partner_by_same_delta() {
    let mut p = project();
    let a = video(&p, "a", "V1", s(0), s(4), s(1));
    let aa = audio(&p, "aa", "A1", s(0), s(4), s(1));
    p.clips.push(linked(a, "L"));
    p.clips.push(linked(aa, "L"));
    trim_edge(&mut p, "a", Edge::End, s(3), false).expect("trim ok");
    let aa = get(&p, "aa");
    assert_eq!(aa.dur, s(3));
    assert_eq!(aa.src_in, s(1));
}

#[test]
fn trim_start_moves_linked_audio_partner_start_edge() {
    let mut p = project();
    let a = video(&p, "a", "V1", s(0), s(4), s(1));
    let aa = audio(&p, "aa", "A1", s(0), s(4), s(1));
    p.clips.push(linked(a, "L"));
    p.clips.push(linked(aa, "L"));
    trim_edge(&mut p, "a", Edge::Start, s(1), false).expect("trim ok");
    let aa = get(&p, "aa");
    assert_eq!(aa.start, s(1));
    assert_eq!(aa.dur, s(3));
    assert_eq!(aa.src_in, s(2));
}

#[test]
fn trim_end_moves_group_member_partner_too() {
    let mut p = project();
    let a = grouped(video(&p, "a", "V1", s(0), s(4), s(1)), "G");
    let aa = grouped(audio(&p, "aa", "A1", s(0), s(4), s(1)), "G");
    p.clips.push(a);
    p.clips.push(aa);
    trim_edge(&mut p, "a", Edge::End, s(2), false).expect("trim ok");
    assert_eq!(get(&p, "aa").dur, s(2));
}

#[test]
fn trim_with_locked_partner_track_is_atomic_for_whole_edit() {
    let mut p = project();
    let a = video(&p, "a", "V1", s(0), s(4), s(1));
    let aa = audio(&p, "aa", "A1", s(0), s(4), s(1));
    p.clips.push(linked(a, "L"));
    p.clips.push(linked(aa, "L"));
    let a1 = tid(&p, "A1");
    p.track_mut(&a1).expect("A1").locked = true;
    let before = p.clone();
    let r = trim_edge(&mut p, "a", Edge::End, s(3), false);
    assert_eq!(r, Err(EditError::Locked(a1)));
    assert_eq!(get(&p, "a").dur, s(4));
    assert_eq!(p, before);
}

#[test]
fn trim_freeze_clip_start_edge_is_refused() {
    let mut p = project();
    let f = with_speed(video(&p, "f", "V2", s(0), s(4), s(1)), 0.0);
    p.clips.push(f);
    let before = p.clone();
    let r = trim_edge(&mut p, "f", Edge::Start, s(1), false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn trim_freeze_clip_end_edge_is_allowed_and_keeps_speed_zero() {
    let mut p = project();
    let f = with_speed(video(&p, "f", "V2", s(0), s(4), s(1)), 0.0);
    p.clips.push(f);
    trim_edge(&mut p, "f", Edge::End, s(2), false).expect("end trim ok");
    let f = get(&p, "f");
    assert_eq!(f.dur, s(2));
    assert_eq!(f.speed, 0.0);
}

#[test]
fn trim_unknown_clip_is_not_found() {
    let mut p = ab_project();
    let r = trim_edge(&mut p, "nope", Edge::End, s(2), false);
    assert_eq!(r, Err(EditError::NotFound("nope".to_string())));
}

#[test]
fn trim_to_same_edge_position_is_a_no_op() {
    let mut p = ab_project();
    let before = p.clone();
    trim_edge(&mut p, "a", Edge::Start, s(0), false).expect("no-op ok");
    assert_eq!(p, before);
}

// ---------------------------------------------------------------------------------------------
// roll_edit
// ---------------------------------------------------------------------------------------------

#[test]
fn roll_moves_cut_right_lengthens_left_shortens_right() {
    let mut p = ab_project();
    roll_edit(&mut p, "a", "b", s(5)).expect("roll ok");
    assert_eq!(get(&p, "a").dur, s(5));
    let b = get(&p, "b");
    assert_eq!(b.start, s(5));
    assert_eq!(b.dur, s(2));
    assert_eq!(b.src_in, s(3));
}

#[test]
fn roll_moves_cut_left_shortens_left_lengthens_right() {
    let mut p = ab_project();
    roll_edit(&mut p, "a", "b", s(3)).expect("roll ok");
    assert_eq!(get(&p, "a").dur, s(3));
    let b = get(&p, "b");
    assert_eq!(b.start, s(3));
    assert_eq!(b.dur, s(4));
    assert_eq!(b.src_in, s(1));
}

#[test]
fn roll_moves_linked_partners_of_both_sides() {
    let mut p = project();
    let a = linked(video(&p, "a", "V1", s(0), s(4), s(1)), "LA");
    let b = linked(video(&p, "b", "V1", s(4), s(3), s(2)), "LB");
    let aa = linked(audio(&p, "aa", "A1", s(0), s(4), s(1)), "LA");
    let bb = linked(audio(&p, "bb", "A1", s(4), s(3), s(2)), "LB");
    for c in [a, b, aa, bb] {
        p.clips.push(c);
    }
    roll_edit(&mut p, "a", "b", s(5)).expect("roll ok");
    assert_eq!(get(&p, "aa").dur, s(5));
    let bb = get(&p, "bb");
    assert_eq!(bb.start, s(5));
    assert_eq!(bb.dur, s(2));
    assert_eq!(bb.src_in, s(3));
}

#[test]
fn roll_across_different_tracks_is_invalid() {
    let mut p = ab_project();
    let b2 = video(&p, "b2", "V2", s(4), s(3), s(2));
    p.clips.push(b2);
    let before = p.clone();
    let r = roll_edit(&mut p, "a", "b2", s(5));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn roll_non_touching_clips_is_invalid() {
    let mut p = abc_project();
    let r = roll_edit(&mut p, "a", "c", s(5));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn roll_same_clip_is_invalid() {
    let mut p = ab_project();
    let r = roll_edit(&mut p, "a", "a", s(3));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn roll_cut_before_left_start_plus_one_frame_is_invalid() {
    let mut p = ab_project();
    let r = roll_edit(&mut p, "a", "b", Tick(FRAME - 1));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn roll_cut_past_right_end_minus_one_frame_is_invalid() {
    let mut p = ab_project();
    let r = roll_edit(&mut p, "a", "b", s(7));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn roll_exhausting_right_handle_is_invalid_and_atomic() {
    let mut p = ab_project();
    let before = p.clone();
    // b would need src_in = 2 + (1 - 4) = -1 s.
    let r = roll_edit(&mut p, "a", "b", s(1));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn roll_on_locked_track_is_locked() {
    let mut p = ab_project();
    let v1 = tid(&p, "V1");
    p.track_mut(&v1).expect("V1").locked = true;
    let r = roll_edit(&mut p, "a", "b", s(5));
    assert_eq!(r, Err(EditError::Locked(v1)));
}

// ---------------------------------------------------------------------------------------------
// slip
// ---------------------------------------------------------------------------------------------

#[test]
fn slip_shifts_src_in_and_keeps_place_and_length() {
    let mut p = ab_project();
    slip(&mut p, "a", s(2)).expect("slip ok");
    let a = get(&p, "a");
    assert_eq!(a.src_in, s(3));
    assert_eq!(a.start, s(0));
    assert_eq!(a.dur, s(4));
}

#[test]
fn slip_scales_delta_by_speed() {
    let mut p = ab_project();
    p.clip_mut("a").expect("a").speed = 2.0;
    p.clip_mut("a").expect("a").dur = s(2);
    slip(&mut p, "a", s(1)).expect("slip ok");
    assert_eq!(get(&p, "a").src_in, s(3));
}

#[test]
fn slip_before_media_start_is_invalid_and_atomic() {
    let mut p = ab_project();
    let before = p.clone();
    let r = slip(&mut p, "a", s(-2));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn slip_past_media_end_is_invalid() {
    let mut p = ab_project();
    // src_out would become 8 + 4 = 12 s, media is 10 s.
    let r = slip(&mut p, "a", s(7));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn slip_moves_linked_partner_by_same_delta() {
    let mut p = project();
    let a = linked(video(&p, "a", "V1", s(0), s(4), s(1)), "L");
    let aa = linked(audio(&p, "aa", "A1", s(0), s(4), s(1)), "L");
    p.clips.push(a);
    p.clips.push(aa);
    slip(&mut p, "a", s(1)).expect("slip ok");
    assert_eq!(get(&p, "aa").src_in, s(2));
}

#[test]
fn slip_on_locked_track_is_locked() {
    let mut p = ab_project();
    let v1 = tid(&p, "V1");
    p.track_mut(&v1).expect("V1").locked = true;
    assert_eq!(slip(&mut p, "a", s(1)), Err(EditError::Locked(v1)));
}

#[test]
fn slip_unknown_clip_is_not_found() {
    let mut p = ab_project();
    assert_eq!(
        slip(&mut p, "zz", s(1)),
        Err(EditError::NotFound("zz".to_string()))
    );
}

// ---------------------------------------------------------------------------------------------
// slide
// ---------------------------------------------------------------------------------------------

#[test]
fn slide_right_moves_clip_and_neighbours_absorb_change() {
    let mut p = abc_project();
    slide(&mut p, "b", s(1)).expect("slide ok");
    assert_eq!(get(&p, "a").dur, s(5));
    let b = get(&p, "b");
    assert_eq!(b.start, s(5));
    assert_eq!(b.dur, s(3));
    let c = get(&p, "c");
    assert_eq!(c.start, s(8));
    assert_eq!(c.dur, s(2));
    assert_eq!(c.src_in, s(2));
}

#[test]
fn slide_left_moves_clip_and_neighbours_absorb_change() {
    let mut p = abc_project();
    slide(&mut p, "b", s(-1)).expect("slide ok");
    assert_eq!(get(&p, "a").dur, s(3));
    assert_eq!(get(&p, "b").start, s(3));
    let c = get(&p, "c");
    assert_eq!(c.start, s(6));
    assert_eq!(c.dur, s(4));
    assert_eq!(c.src_in, s(0));
}

#[test]
fn slide_would_vanish_left_neighbour_is_invalid_and_atomic() {
    let mut p = abc_project();
    let before = p.clone();
    let r = slide(&mut p, "b", s(-4));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn slide_would_vanish_right_neighbour_is_invalid() {
    let mut p = abc_project();
    let r = slide(&mut p, "b", s(3));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn slide_exhausting_right_neighbour_handle_is_invalid() {
    let mut p = abc_project();
    // c src_in would become 1 + (-2) = -1 s.
    let r = slide(&mut p, "b", s(-2));
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn slide_zero_delta_is_a_no_op() {
    let mut p = abc_project();
    let before = p.clone();
    slide(&mut p, "b", Tick::ZERO).expect("no-op ok");
    assert_eq!(p, before);
}

#[test]
fn slide_clip_without_neighbours_moves_alone() {
    let mut p = project();
    let z = video(&p, "z", "V3", s(0), s(2), s(0));
    p.clips.push(z);
    slide(&mut p, "z", s(1)).expect("slide ok");
    let z = get(&p, "z");
    assert_eq!(z.start, s(1));
    assert_eq!(z.dur, s(2));
}

#[test]
fn slide_moves_linked_partner_with_clip() {
    let mut p = abc_project();
    let bb = linked(audio(&p, "bb", "A1", s(4), s(3), s(2)), "LB");
    p.clip_mut("b").expect("b").link_id = Some("LB".to_string());
    p.clips.push(bb);
    slide(&mut p, "b", s(1)).expect("slide ok");
    let bb = get(&p, "bb");
    assert_eq!(bb.start, s(5));
    assert_eq!(bb.dur, s(3));
}

// ---------------------------------------------------------------------------------------------
// set_speed
// ---------------------------------------------------------------------------------------------

#[test]
fn set_speed_two_halves_duration_exactly() {
    let mut p = ab_project();
    set_speed(&mut p, "a", 2.0, false).expect("speed ok");
    let a = get(&p, "a");
    assert_eq!(a.speed, 2.0);
    assert_eq!(a.dur, s(2));
    assert_eq!(a.src_in, s(1));
    assert_eq!(a.src_len(), s(4));
}

#[test]
fn set_speed_half_without_ripple_overwrites_the_neighbour() {
    let mut p = ab_project();
    set_speed(&mut p, "a", 0.5, false).expect("the slowed clip goes on top");
    // a grows to [0, 8), which covers b [4, 7) completely.
    assert_eq!(get(&p, "a").dur, s(8));
    assert!(p.clip("b").is_none());
    assert_eq!(p.clips.len(), 1);
}

#[test]
fn set_speed_half_with_ripple_shifts_later_clips() {
    let mut p = ab_project();
    set_speed(&mut p, "a", 0.5, true).expect("ripple speed ok");
    assert_eq!(get(&p, "a").dur, s(8));
    assert_eq!(get(&p, "b").start, s(8));
    assert_eq!(get(&p, "b").dur, s(3));
}

#[test]
fn set_speed_four_gives_exact_quarter_length() {
    let mut p = ab_project();
    set_speed(&mut p, "a", 4.0, false).expect("speed ok");
    assert_eq!(get(&p, "a").dur, s(1));
}

#[test]
fn set_speed_eight_gives_exact_eighth_length() {
    let mut p = ab_project();
    set_speed(&mut p, "a", 8.0, false).expect("speed ok");
    assert_eq!(get(&p, "a").dur, ms(500));
}

#[test]
fn set_speed_lower_bound_0_1_is_accepted_within_one_tick() {
    let mut p = project();
    let c = video(&p, "c", "V2", s(0), s(4), s(0));
    p.clips.push(c);
    set_speed(&mut p, "c", 0.1, false).expect("0.1 is allowed");
    let c = get(&p, "c");
    assert_eq!(c.speed, 0.1);
    assert!((c.dur.0 - s(40).0).abs() <= 1, "dur {:?}", c.dur);
}

#[test]
fn set_speed_outside_range_is_invalid() {
    for bad in [0.09, 8.01, f64::NAN, f64::INFINITY, -1.0] {
        let mut p = project();
        let c = video(&p, "c", "V2", s(0), s(4), s(0));
        p.clips.push(c);
        let before = p.clone();
        let r = set_speed(&mut p, "c", bad, false);
        assert!(
            matches!(r, Err(EditError::Invalid(_))),
            "speed {bad}: got {r:?}"
        );
        assert_eq!(p, before);
    }
}

#[test]
fn set_speed_that_makes_clip_shorter_than_a_frame_is_invalid() {
    let mut p = project();
    let c = video(&p, "c", "V2", s(0), Tick(2 * FRAME), s(0));
    p.clips.push(c);
    let r = set_speed(&mut p, "c", 8.0, false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn set_speed_on_freeze_clip_is_invalid() {
    let mut p = project();
    let f = with_speed(video(&p, "f", "V2", s(0), s(4), s(1)), 0.0);
    p.clips.push(f);
    let r = set_speed(&mut p, "f", 2.0, false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn set_speed_applies_to_linked_partner_too() {
    let mut p = project();
    let a = linked(video(&p, "a", "V1", s(0), s(4), s(1)), "L");
    let aa = linked(audio(&p, "aa", "A1", s(0), s(4), s(1)), "L");
    p.clips.push(a);
    p.clips.push(aa);
    set_speed(&mut p, "a", 2.0, false).expect("speed ok");
    let aa = get(&p, "aa");
    assert_eq!(aa.speed, 2.0);
    assert_eq!(aa.dur, s(2));
}

#[test]
fn set_speed_rescales_fades_and_envelope_proportionally() {
    // Doc: "Envelope point times and fade lengths are rescaled/clamped." The clip halves in
    // length, so proportional rescaling gives fade_in 1 s, fade_out 0.5 s, envelope t 0.5 s.
    let mut p = ab_project();
    {
        let a = p.clip_mut("a").expect("a");
        a.audio.fade_in = s(2);
        a.audio.fade_out = s(1);
        a.audio.envelope = vec![EnvPoint { t: s(1), db: -3.0 }];
    }
    set_speed(&mut p, "a", 2.0, false).expect("speed ok");
    let a = get(&p, "a");
    assert_eq!(a.audio.fade_in, s(1));
    assert_eq!(a.audio.fade_out, ms(500));
    assert_eq!(a.audio.envelope.len(), 1);
    assert_eq!(a.audio.envelope[0].t, ms(500));
}

#[test]
fn set_speed_fades_scale_by_current_timeline_length_across_repeated_changes() {
    // Each change scales fades by new_dur / old_dur, so the steps compose: 4 s -> 2 s -> 1 s -> 4 s.
    let mut p = ab_project();
    p.clip_mut("a").expect("a").audio.fade_in = s(2);
    set_speed(&mut p, "a", 2.0, false).expect("2x");
    assert_eq!(get(&p, "a").audio.fade_in, s(1));
    set_speed(&mut p, "a", 4.0, false).expect("4x");
    assert_eq!(get(&p, "a").dur, s(1));
    assert_eq!(get(&p, "a").audio.fade_in, ms(500));
    set_speed(&mut p, "a", 1.0, true).expect("1x with ripple");
    assert_eq!(get(&p, "a").dur, s(4));
    assert_eq!(get(&p, "a").audio.fade_in, s(2));
}

#[test]
fn set_speed_slowdown_moves_envelope_points_out_with_the_clip() {
    // Doc: "Envelope point times ... are rescaled". Half speed doubles the timeline length, so
    // points at 1 s and 2 s move to 2 s and 4 s. (A point at the old end would land exactly on the
    // new end, which is dropped as past the end, so the fixture keeps points inside the clip.)
    let mut p = ab_project();
    {
        let a = p.clip_mut("a").expect("a");
        a.audio.envelope = vec![
            EnvPoint { t: s(1), db: -6.0 },
            EnvPoint { t: s(2), db: 0.0 },
        ];
    }
    set_speed(&mut p, "a", 0.5, true).expect("half speed with ripple");
    let a = get(&p, "a");
    assert_eq!(a.dur, s(8));
    assert_eq!(a.audio.envelope.len(), 2);
    assert_eq!(a.audio.envelope[0].t, s(2));
    assert_eq!(a.audio.envelope[1].t, s(4));
}

// ---------------------------------------------------------------------------------------------
// set_reverse
// ---------------------------------------------------------------------------------------------

#[test]
fn set_reverse_sets_flag_on_clip_and_partner_and_keeps_src_in() {
    let mut p = project();
    let a = linked(video(&p, "a", "V1", s(0), s(4), s(1)), "L");
    let aa = linked(audio(&p, "aa", "A1", s(0), s(4), s(1)), "L");
    p.clips.push(a);
    p.clips.push(aa);
    set_reverse(&mut p, "a", true).expect("reverse ok");
    assert!(get(&p, "a").reverse);
    assert!(get(&p, "aa").reverse);
    assert_eq!(get(&p, "a").src_in, s(1));
}

#[test]
fn set_reverse_false_clears_flag_on_clip_and_partner() {
    let mut p = project();
    let a = linked(video(&p, "a", "V1", s(0), s(4), s(1)), "L");
    let mut aa = linked(audio(&p, "aa", "A1", s(0), s(4), s(1)), "L");
    aa.reverse = true;
    p.clips.push(a);
    p.clips.push(aa);
    set_reverse(&mut p, "aa", false).expect("clear ok");
    assert!(!get(&p, "a").reverse);
    assert!(!get(&p, "aa").reverse);
}

#[test]
fn set_reverse_unknown_clip_is_not_found() {
    let mut p = ab_project();
    assert_eq!(
        set_reverse(&mut p, "nope", true),
        Err(EditError::NotFound("nope".to_string()))
    );
}

#[test]
fn set_reverse_on_locked_track_is_locked_and_atomic() {
    let mut p = ab_project();
    let v1 = tid(&p, "V1");
    p.track_mut(&v1).expect("V1").locked = true;
    let before = p.clone();
    assert_eq!(set_reverse(&mut p, "a", true), Err(EditError::Locked(v1)));
    assert_eq!(p, before);
}

// ---------------------------------------------------------------------------------------------
// join_through_edit
// ---------------------------------------------------------------------------------------------

/// L = 0..4 (src 1..5), R = 4..6 (src 5..7). Contiguous on media m1.
fn join_project() -> Project {
    let mut p = project();
    let l = video(&p, "L", "V1", s(0), s(4), s(1));
    let r = video(&p, "R", "V1", s(4), s(2), s(5));
    p.clips.push(l);
    p.clips.push(r);
    p
}

#[test]
fn join_merges_contiguous_clips_and_returns_left_id() {
    let mut p = join_project();
    let kept = join_through_edit(&mut p, "L", "R").expect("join ok");
    assert_eq!(kept, "L");
    assert_eq!(p.clips.len(), 1);
    assert!(p.clip("R").is_none());
    let l = get(&p, "L");
    assert_eq!(l.start, s(0));
    assert_eq!(l.dur, s(6));
    assert_eq!(l.src_in, s(1));
}

#[test]
fn join_keeps_left_fade_in_and_takes_right_fade_out() {
    let mut p = join_project();
    p.clip_mut("L").expect("L").audio.fade_in = ms(500);
    p.clip_mut("R").expect("R").audio.fade_out = s(1);
    join_through_edit(&mut p, "L", "R").expect("join ok");
    let l = get(&p, "L");
    assert_eq!(l.audio.fade_in, ms(500));
    assert_eq!(l.audio.fade_out, s(1));
}

#[test]
fn join_appends_right_envelope_shifted_by_left_length() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").audio.envelope = vec![EnvPoint {
        t: ms(500),
        db: -2.0,
    }];
    join_through_edit(&mut p, "L", "R").expect("join ok");
    // Justification: the left clip has no points, so its audio stays at 0 dB; the old code held
    // the right's -2 dB over the whole left part. The right's point moves to 4.5 s.
    let env = get(&p, "L").audio.envelope;
    assert_eq!(
        env.first(),
        Some(&EnvPoint {
            t: Tick::ZERO,
            db: 0.0
        })
    );
    assert!(env.contains(&EnvPoint {
        t: ms(4500),
        db: -2.0
    }));
    let at_3s = env.iter().filter(|e| e.t <= s(3)).all(|e| e.db == 0.0);
    assert!(at_3s, "left part must stay at 0 dB: {env:?}");
}

#[test]
fn join_non_contiguous_source_is_invalid_and_atomic() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").src_in = s(6);
    let before = p.clone();
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn join_different_media_is_invalid() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").media_id = Some("m2".to_string());
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_different_track_is_invalid() {
    let mut p = join_project();
    let v2 = tid(&p, "V2");
    p.clip_mut("R").expect("R").track_id = v2;
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_not_touching_is_invalid() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").start = s(5);
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_different_speed_is_invalid() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").speed = 2.0;
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_forward_and_reversed_mix_is_invalid() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").reverse = true;
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_right_with_transition_is_invalid() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: ms(500),
    });
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_clip_with_itself_is_invalid() {
    let mut p = join_project();
    let r = join_through_edit(&mut p, "L", "L");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_different_kind_is_invalid() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").kind = ClipKind::Audio;
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn join_reversed_pair_with_contiguous_source_is_ok() {
    // Reversed: the left clip covers the later source (src 5..9), the right covers 2..5.
    let mut p = project();
    let mut l = video(&p, "L", "V1", s(0), s(4), s(5));
    l.reverse = true;
    let mut r = video(&p, "R", "V1", s(4), s(3), s(2));
    r.reverse = true;
    p.clips.push(l);
    p.clips.push(r);
    join_through_edit(&mut p, "L", "R").expect("join ok");
    let l = get(&p, "L");
    assert_eq!(l.dur, s(7));
    // The joined range is src 2..9, played backwards, so its start is the right clip's src_in.
    assert_eq!(l.src_in, s(2));
    assert!(l.reverse);
}

#[test]
fn join_reversed_pair_with_gap_in_source_is_invalid() {
    let mut p = project();
    let mut l = video(&p, "L", "V1", s(0), s(4), s(5));
    l.reverse = true;
    let mut r = video(&p, "R", "V1", s(4), s(3), s(6));
    r.reverse = true;
    p.clips.push(l);
    p.clips.push(r);
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

// ---------------------------------------------------------------------------------------------
// set_work_area
// ---------------------------------------------------------------------------------------------

/// Timeline 0..10 s on V2 (one clip), so the work area may end at most at 10 s (frame-aligned).
fn ten_second_project() -> Project {
    let mut p = project();
    let z = video(&p, "z", "V2", s(0), s(10), s(0));
    p.clips.push(z);
    p
}

#[test]
fn work_area_valid_range_is_stored() {
    let mut p = ten_second_project();
    let area = WorkArea {
        start: s(1),
        end: s(5),
    };
    set_work_area(&mut p, Some(area)).expect("valid area");
    assert_eq!(p.work_area, Some(area));
}

#[test]
fn work_area_end_equal_to_timeline_end_is_ok() {
    let mut p = ten_second_project();
    set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(0),
            end: s(10),
        }),
    )
    .expect("ok");
}

#[test]
fn work_area_end_beyond_timeline_by_a_frame_is_invalid() {
    let mut p = ten_second_project();
    let r = set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(0),
            end: Tick(s(10).0 + FRAME),
        }),
    );
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn work_area_timeline_end_rounds_up_to_next_frame() {
    // Timeline end is 10 s + 1 tick, which rounds up to 10 s + one frame.
    let mut p = project();
    let z = video(&p, "z", "V2", s(0), Tick(s(10).0 + 1), s(0));
    p.clips.push(z);
    set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(0),
            end: Tick(s(10).0 + FRAME),
        }),
    )
    .expect("rounded end is allowed");
    let r = set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(0),
            end: Tick(s(10).0 + FRAME + 1),
        }),
    );
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn work_area_start_not_before_end_is_invalid() {
    let mut p = ten_second_project();
    let r1 = set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(3),
            end: s(3),
        }),
    );
    assert!(matches!(r1, Err(EditError::Invalid(_))), "got {r1:?}");
    let r2 = set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(4),
            end: s(3),
        }),
    );
    assert!(matches!(r2, Err(EditError::Invalid(_))), "got {r2:?}");
}

#[test]
fn work_area_negative_start_is_invalid() {
    let mut p = ten_second_project();
    let r = set_work_area(
        &mut p,
        Some(WorkArea {
            start: Tick(-1),
            end: s(3),
        }),
    );
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn work_area_none_clears_the_area() {
    let mut p = ten_second_project();
    set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(1),
            end: s(2),
        }),
    )
    .expect("ok");
    set_work_area(&mut p, None).expect("clear ok");
    assert_eq!(p.work_area, None);
}

#[test]
fn work_area_on_empty_timeline_allows_one_frame_only() {
    let mut p = project();
    set_work_area(
        &mut p,
        Some(WorkArea {
            start: Tick::ZERO,
            end: Tick(FRAME),
        }),
    )
    .expect("one frame");
    let r = set_work_area(
        &mut p,
        Some(WorkArea {
            start: Tick::ZERO,
            end: Tick(2 * FRAME),
        }),
    );
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
}

#[test]
fn work_area_rejected_value_keeps_previous_area() {
    let mut p = ten_second_project();
    let good = WorkArea {
        start: s(1),
        end: s(2),
    };
    set_work_area(&mut p, Some(good)).expect("ok");
    let r = set_work_area(
        &mut p,
        Some(WorkArea {
            start: s(9),
            end: s(20),
        }),
    );
    assert!(r.is_err());
    assert_eq!(p.work_area, Some(good));
}

// ---------------------------------------------------------------------------------------------
// Review fixes: reversed source, grouped ripple, envelope rebase, join properties, speed bounds
// ---------------------------------------------------------------------------------------------

/// Project whose media are 30 s long, so reversed clips can sit away from the start of the file.
fn long_media_project() -> Project {
    let mut p = project();
    for m in p.media.iter_mut() {
        m.duration = s(30);
    }
    p
}

#[test]
fn reversed_end_trim_advances_src_in_by_removed_source() {
    let mut p = long_media_project();
    let mut r = video(&p, "r", "V2", s(0), s(10), s(10));
    r.reverse = true;
    p.clips.push(r);
    trim_edge(&mut p, "r", Edge::End, s(8), false).expect("trim ok");
    let r = get(&p, "r");
    assert_eq!(r.dur, s(8));
    assert_eq!(r.src_in, s(12));
    assert_eq!(r.src_out(), s(20));
}

#[test]
fn reversed_end_lengthen_moves_src_in_back() {
    let mut p = long_media_project();
    let mut r = video(&p, "r", "V2", s(0), s(10), s(10));
    r.reverse = true;
    p.clips.push(r);
    trim_edge(&mut p, "r", Edge::End, s(12), false).expect("trim ok");
    let r = get(&p, "r");
    assert_eq!(r.dur, s(12));
    assert_eq!(r.src_in, s(8));
}

#[test]
fn roll_left_reversed_clip_moves_src_in_with_its_cut() {
    let mut p = long_media_project();
    let mut l = video(&p, "L", "V1", s(0), s(4), s(10));
    l.reverse = true;
    let r = video(&p, "R", "V1", s(4), s(3), s(2));
    p.clips.push(l);
    p.clips.push(r);
    roll_edit(&mut p, "L", "R", s(3)).expect("roll ok");
    let l = get(&p, "L");
    assert_eq!(l.dur, s(3));
    assert_eq!(l.src_in, s(11));
    let r = get(&p, "R");
    assert_eq!(r.start, s(3));
    assert_eq!(r.dur, s(4));
    assert_eq!(r.src_in, s(1));
}

#[test]
fn slide_reversed_left_neighbour_moves_src_in_with_its_end() {
    let mut p = long_media_project();
    let mut l = video(&p, "L", "V1", s(0), s(4), s(10));
    l.reverse = true;
    let c = video(&p, "C", "V1", s(4), s(3), s(2));
    p.clips.push(l);
    p.clips.push(c);
    slide(&mut p, "C", s(1)).expect("slide ok");
    let l = get(&p, "L");
    assert_eq!(l.dur, s(5));
    assert_eq!(l.src_in, s(9));
}

#[test]
fn ripple_end_trim_with_grouped_partner_on_same_track_keeps_partner_length() {
    let mut p = project();
    let a = grouped(video(&p, "a", "V1", s(0), s(5), s(0)), "g");
    let b = grouped(video(&p, "b", "V1", s(10), s(5), s(2)), "g");
    p.clips.push(a);
    p.clips.push(b);
    trim_edge(&mut p, "a", Edge::End, s(4), true).expect("ripple ok");
    assert_eq!(get(&p, "a").dur, s(4));
    let b = get(&p, "b");
    assert_eq!(b.start, s(9));
    assert_eq!(b.dur, s(5));
}

#[test]
fn ripple_start_trim_with_grouped_partner_on_same_track_trims_partner_once() {
    let mut p = project();
    let a = grouped(video(&p, "a", "V1", s(0), s(5), s(0)), "g");
    let b = grouped(video(&p, "b", "V1", s(10), s(5), s(2)), "g");
    p.clips.push(a);
    p.clips.push(b);
    trim_edge(&mut p, "a", Edge::Start, s(1), true).expect("ripple ok");
    let a = get(&p, "a");
    assert_eq!(a.start, s(0));
    assert_eq!(a.dur, s(4));
    // b is first pulled left by the ripple (to 9 s), then trimmed by the same 1 s on its start.
    let b = get(&p, "b");
    assert_eq!(b.start, s(9));
    assert_eq!(b.dur, s(4));
    assert_eq!(b.src_in, s(3));
}

#[test]
fn start_trim_rebases_envelope_points_with_the_source() {
    let mut p = project();
    let mut c = video(&p, "c", "V2", s(0), s(8), s(0));
    c.audio.envelope = vec![EnvPoint { t: s(5), db: -4.0 }];
    p.clips.push(c);
    trim_edge(&mut p, "c", Edge::Start, s(2), false).expect("trim ok");
    let c = get(&p, "c");
    assert_eq!(c.start, s(2));
    assert_eq!(c.dur, s(6));
    assert!(
        c.audio.envelope.contains(&EnvPoint { t: s(3), db: -4.0 }),
        "envelope {:?}",
        c.audio.envelope
    );
}

#[test]
fn roll_right_clip_rebases_its_envelope() {
    let mut p = long_media_project();
    let l = video(&p, "L", "V1", s(0), s(4), s(0));
    let mut r = video(&p, "R", "V1", s(4), s(4), s(4));
    r.audio.envelope = vec![EnvPoint { t: s(2), db: -6.0 }];
    p.clips.push(l);
    p.clips.push(r);
    roll_edit(&mut p, "L", "R", s(5)).expect("roll ok");
    let r = get(&p, "R");
    assert_eq!(r.src_in, s(5));
    assert!(
        r.audio.envelope.contains(&EnvPoint { t: s(1), db: -6.0 }),
        "envelope {:?}",
        r.audio.envelope
    );
}

#[test]
fn join_with_different_gain_is_invalid_and_atomic() {
    let mut p = join_project();
    p.clip_mut("R").expect("R").audio.gain_db = -3.0;
    let before = p.clone();
    let r = join_through_edit(&mut p, "L", "R");
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}

#[test]
fn join_with_different_pan_mono_muted_or_enabled_is_invalid() {
    let cases: [fn(&mut Clip); 4] = [
        |c| c.audio.pan = 0.5,
        |c| c.audio.mono = true,
        |c| c.audio.muted = true,
        |c| c.enabled = false,
    ];
    for tweak in cases {
        let mut p = join_project();
        tweak(p.clip_mut("R").expect("R"));
        let r = join_through_edit(&mut p, "L", "R");
        assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    }
}

#[test]
fn join_with_only_left_envelope_keeps_right_at_zero_db() {
    let mut p = join_project();
    p.clip_mut("L").expect("L").audio.envelope = vec![EnvPoint { t: s(1), db: -6.0 }];
    join_through_edit(&mut p, "L", "R").expect("join ok");
    let env = get(&p, "L").audio.envelope;
    // The left's own point is kept; the seam at 4 s holds its level, and everything after the
    // seam's first tick (the right clip's part) is at 0 dB.
    assert!(env.contains(&EnvPoint { t: s(1), db: -6.0 }), "{env:?}");
    let after_seam: Vec<f64> = env.iter().filter(|e| e.t > s(4)).map(|e| e.db).collect();
    assert!(!after_seam.is_empty());
    assert!(after_seam.iter().all(|db| *db == 0.0), "{env:?}");
}

#[test]
fn set_speed_untrimmed_clip_to_three_and_point_seven_succeeds_within_media() {
    for speed in [3.0, 0.7] {
        let mut p = project();
        let c = video(&p, "c", "V2", s(0), s(10), s(0));
        p.clips.push(c);
        set_speed(&mut p, "c", speed, false).expect("speed ok");
        let c = get(&p, "c");
        assert_eq!(c.speed, speed);
        assert!(
            c.src_out() <= s(10),
            "speed {speed}: src_out {:?}",
            c.src_out()
        );
    }
}

#[test]
fn set_speed_on_image_clip_is_invalid_and_atomic() {
    let mut p = project();
    let c = Clip {
        id: "img".to_string(),
        track_id: tid(&p, "V2"),
        kind: ClipKind::Image,
        media_id: None,
        start: s(0),
        dur: s(4),
        ..Clip::default()
    };
    p.clips.push(c);
    let before = p.clone();
    let r = set_speed(&mut p, "img", 2.0, false);
    assert!(matches!(r, Err(EditError::Invalid(_))), "got {r:?}");
    assert_eq!(p, before);
}
