//! Public-API tests for the range and clipboard operations: paste, remove_range (lift and extract),
//! close_gap, close_all_gaps and freeze_frame. Written from the doc comments in
//! `src/ops_range.rs`. Helpers are duplicated per test file on purpose (no shared module).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clipcheck_timeline::error::EditError;
use clipcheck_timeline::model::{new_id, Clip, ClipKind, Group, Project, TrackKind};
use clipcheck_timeline::ops_basic::PlaceMode;
use clipcheck_timeline::ops_range::{
    close_all_gaps, close_gap, freeze_frame, paste_clips, remove_range,
};
use clipcheck_timeline::time::{Rate, Tick, TICKS_PER_SECOND};

// ---------- helpers ----------

fn s(n: i64) -> Tick {
    Tick::from_secs(n)
}

fn project() -> Project {
    Project::new("range")
}

fn video_tracks(p: &Project) -> Vec<String> {
    p.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .map(|t| t.id.clone())
        .collect()
}

fn audio_tracks(p: &Project) -> Vec<String> {
    p.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id.clone())
        .collect()
}

fn v1(p: &Project) -> String {
    video_tracks(p)[0].clone()
}

fn v2(p: &Project) -> String {
    video_tracks(p)[1].clone()
}

fn v3(p: &Project) -> String {
    video_tracks(p)[2].clone()
}

fn a1(p: &Project) -> String {
    audio_tracks(p)[0].clone()
}

/// Adds a media-less clip straight into the project to set up an exact layout.
fn raw(p: &mut Project, track: &str, kind: ClipKind, start: Tick, dur: Tick) -> String {
    let id = new_id();
    p.clips.push(Clip {
        id: id.clone(),
        track_id: track.to_string(),
        kind,
        start,
        dur,
        ..Clip::default()
    });
    id
}

/// A clip value for the clipboard (not yet in any project).
fn cb(track: &str, kind: ClipKind, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: new_id(),
        track_id: track.to_string(),
        kind,
        start,
        dur,
        ..Clip::default()
    }
}

fn set_link(p: &mut Project, a: &str, b: &str, link: &str) {
    p.clip_mut(a).expect("a").link_id = Some(link.to_string());
    p.clip_mut(b).expect("b").link_id = Some(link.to_string());
}

fn clip<'a>(p: &'a Project, id: &str) -> &'a Clip {
    p.clip(id).expect("clip exists")
}

/// (start, dur) of every clip on a track, sorted by start.
fn layout(p: &Project, track: &str) -> Vec<(Tick, Tick)> {
    p.clips_on_track(track)
        .iter()
        .map(|c| (c.start, c.dur))
        .collect()
}

fn all_tracks(p: &Project) -> Vec<String> {
    p.tracks.iter().map(|t| t.id.clone()).collect()
}

// ---------- paste_clips ----------

#[test]
fn paste_offsets_clips_relative_to_the_earliest_start() {
    let mut p = project();
    let t = v1(&p);
    let clips = vec![
        cb(&t, ClipKind::Video, s(5), s(2)),
        cb(&t, ClipKind::Video, s(7), s(1)),
    ];
    let ids = paste_clips(&mut p, &clips, s(20), None, PlaceMode::Overwrite).unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(clip(&p, &ids[0]).start, s(20));
    assert_eq!(clip(&p, &ids[1]).start, s(22));
    assert_eq!(layout(&p, &t), vec![(s(20), s(2)), (s(22), s(1))]);
}

#[test]
fn paste_returns_fresh_ids_not_the_clipboard_ids() {
    let mut p = project();
    let t = v1(&p);
    let source = cb(&t, ClipKind::Video, s(0), s(2));
    let source_id = source.id.clone();
    let ids = paste_clips(
        &mut p,
        std::slice::from_ref(&source),
        s(4),
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_ne!(ids[0], source_id);
    assert!(
        p.clip(&source_id).is_none(),
        "clipboard id is not added to the project"
    );
}

#[test]
fn paste_goes_back_onto_original_track_when_it_still_exists() {
    let mut p = project();
    let dest = v2(&p);
    let clips = vec![cb(&dest, ClipKind::Video, s(0), s(2))];
    let wanted = v3(&p);
    let ids = paste_clips(&mut p, &clips, s(3), Some(&wanted), PlaceMode::Overwrite).unwrap();
    assert_eq!(clip(&p, &ids[0]).track_id, dest);
}

#[test]
fn paste_uses_wanted_track_when_original_track_is_gone() {
    let mut p = project();
    let wanted = v3(&p);
    let clips = vec![cb("deleted-track", ClipKind::Video, s(0), s(2))];
    let ids = paste_clips(&mut p, &clips, s(3), Some(&wanted), PlaceMode::Overwrite).unwrap();
    assert_eq!(clip(&p, &ids[0]).track_id, wanted);
}

#[test]
fn paste_falls_back_to_first_unlocked_track_of_the_right_kind() {
    let mut p = project();
    p.track_mut(&v1(&p)).unwrap().locked = true;
    let clips = vec![cb("deleted-track", ClipKind::Video, s(0), s(2))];
    let ids = paste_clips(&mut p, &clips, s(3), None, PlaceMode::Overwrite).unwrap();
    assert_eq!(clip(&p, &ids[0]).track_id, v2(&p));
}

#[test]
fn paste_audio_clip_with_missing_track_lands_on_an_audio_track() {
    let mut p = project();
    let clips = vec![cb("deleted-track", ClipKind::Audio, s(0), s(2))];
    let ids = paste_clips(&mut p, &clips, s(3), None, PlaceMode::Overwrite).unwrap();
    assert_eq!(clip(&p, &ids[0]).track_id, a1(&p));
}

#[test]
fn paste_remaps_link_ids_of_a_pair_to_one_new_link() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let mut video = cb(&vt, ClipKind::Video, s(0), s(2));
    let mut audio = cb(&at, ClipKind::Audio, s(0), s(2));
    video.link_id = Some("CLIP-LINK".to_string());
    audio.link_id = Some("CLIP-LINK".to_string());
    let ids = paste_clips(&mut p, &[video, audio], s(10), None, PlaceMode::Overwrite).unwrap();
    let lv = clip(&p, &ids[0]).link_id.clone().expect("video linked");
    let la = clip(&p, &ids[1]).link_id.clone().expect("audio linked");
    assert_eq!(lv, la);
    assert_ne!(lv, "CLIP-LINK");
}

#[test]
fn paste_single_member_of_a_link_drops_the_link() {
    let mut p = project();
    let t = v1(&p);
    let mut lone = cb(&t, ClipKind::Video, s(0), s(2));
    lone.link_id = Some("solo".to_string());
    let ids = paste_clips(&mut p, &[lone], s(10), None, PlaceMode::Overwrite).unwrap();
    assert_eq!(clip(&p, &ids[0]).link_id, None);
}

#[test]
fn paste_remaps_group_ids_and_creates_a_copy_of_the_group() {
    let mut p = project();
    let t = v1(&p);
    p.groups.push(Group {
        id: "G".into(),
        name: "Hero".into(),
        color: None,
    });
    let mut a = cb(&t, ClipKind::Video, s(0), s(1));
    let mut b = cb(&t, ClipKind::Video, s(1), s(1));
    a.group_id = Some("G".into());
    b.group_id = Some("G".into());
    let ids = paste_clips(&mut p, &[a, b], s(10), None, PlaceMode::Overwrite).unwrap();
    let g0 = clip(&p, &ids[0]).group_id.clone().expect("grouped");
    let g1 = clip(&p, &ids[1]).group_id.clone().expect("grouped");
    assert_eq!(g0, g1);
    assert_ne!(g0, "G");
    assert_eq!(
        p.groups.iter().find(|g| g.id == g0).expect("copy").name,
        "Hero"
    );
}

#[test]
fn paste_overwrite_cuts_what_is_under_the_pasted_clip() {
    let mut p = project();
    let t = v1(&p);
    let old = raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let clips = vec![cb(&t, ClipKind::Video, s(0), s(2))];
    paste_clips(&mut p, &clips, s(4), None, PlaceMode::Overwrite).unwrap();
    assert_eq!(
        layout(&p, &t),
        vec![(s(0), s(4)), (s(4), s(2)), (s(6), s(4))]
    );
    assert_eq!(clip(&p, &old).dur, s(4));
}

#[test]
fn paste_insert_splits_at_point_and_pushes_later_clips_by_block_length() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let clips = vec![cb(&t, ClipKind::Video, s(0), s(2))];
    paste_clips(&mut p, &clips, s(2), None, PlaceMode::Insert).unwrap();
    assert_eq!(
        layout(&p, &t),
        vec![(s(0), s(2)), (s(2), s(2)), (s(4), s(8))]
    );
}

#[test]
fn paste_insert_pushes_clip_that_starts_at_the_point() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(4), s(2));
    let clips = vec![cb(&t, ClipKind::Video, s(0), s(3))];
    paste_clips(&mut p, &clips, s(4), None, PlaceMode::Insert).unwrap();
    assert_eq!(layout(&p, &t), vec![(s(4), s(3)), (s(7), s(2))]);
}

#[test]
fn paste_append_lands_after_the_last_clip_of_the_track() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let clips = vec![cb(&t, ClipKind::Video, s(0), s(2))];
    let ids = paste_clips(&mut p, &clips, s(0), None, PlaceMode::Append).unwrap();
    assert_eq!(clip(&p, &ids[0]).start, s(10));
    assert_eq!(layout(&p, &t), vec![(s(0), s(10)), (s(10), s(2))]);
}

#[test]
fn paste_empty_clipboard_is_ok_and_changes_nothing() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let before = p.clone();
    let ids = paste_clips(&mut p, &[], s(5), None, PlaceMode::Overwrite).unwrap();
    assert!(ids.is_empty());
    assert_eq!(p, before);
}

#[test]
fn paste_zero_length_clip_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    let clips = vec![cb(&t, ClipKind::Video, s(0), Tick::ZERO)];
    let err = paste_clips(&mut p, &clips, s(1), None, PlaceMode::Overwrite).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert!(p.clips.is_empty());
}

#[test]
fn paste_onto_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    p.track_mut(&t).unwrap().locked = true;
    let clips = vec![cb(&t, ClipKind::Video, s(0), s(2))];
    let before = p.clone();
    let err = paste_clips(&mut p, &clips, s(1), None, PlaceMode::Overwrite).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
    assert_eq!(p, before);
}

// ---------- remove_range (lift / extract) ----------

#[test]
fn lift_removes_range_and_leaves_a_gap_with_trimmed_edges() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    remove_range(&mut p, s(2), s(4), &[], false).unwrap();
    assert_eq!(layout(&p, &t), vec![(s(0), s(2)), (s(4), s(6))]);
    assert_eq!(clip(&p, &id).dur, s(2), "left piece keeps the id");
    let right = p.clips_on_track(&t)[1].clone();
    assert_eq!(
        right.src_in,
        s(4),
        "right piece source advanced past the cut"
    );
    assert_ne!(right.id, id);
}

#[test]
fn extract_removes_range_and_closes_the_gap() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    remove_range(&mut p, s(2), s(4), &[], true).unwrap();
    assert_eq!(layout(&p, &t), vec![(s(0), s(2)), (s(2), s(6))]);
}

#[test]
fn range_inside_one_clip_splits_it_in_two_with_new_right_id() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    remove_range(&mut p, s(3), s(5), &[], false).unwrap();
    let parts = p.clips_on_track(&t);
    assert_eq!(parts.len(), 2);
    assert_ne!(parts[0].id, parts[1].id);
    assert_eq!(layout(&p, &t), vec![(s(0), s(3)), (s(5), s(5))]);
}

#[test]
fn clips_fully_inside_the_range_are_removed() {
    let mut p = project();
    let t = v1(&p);
    let a = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    raw(&mut p, &t, ClipKind::Video, s(3), s(1));
    let c = raw(&mut p, &t, ClipKind::Video, s(6), s(2));
    remove_range(&mut p, s(2), s(5), &[], false).unwrap();
    assert_eq!(layout(&p, &t), vec![(s(0), s(2)), (s(6), s(2))]);
    assert!(p.clip(&a).is_some());
    assert!(p.clip(&c).is_some());
}

#[test]
fn extract_across_clip_head_moves_the_trimmed_clip_to_zero() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    remove_range(&mut p, s(0), s(3), &[], true).unwrap();
    let c = p.clips_on_track(&t)[0].clone();
    assert_eq!(c.start, s(0));
    assert_eq!(c.dur, s(7));
    assert_eq!(c.src_in, s(3));
}

#[test]
fn lift_trims_clip_tail_without_changing_source_start() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    remove_range(&mut p, s(7), s(12), &[], false).unwrap();
    assert_eq!(layout(&p, &t), vec![(s(0), s(7))]);
    assert_eq!(clip(&p, &id).src_in, Tick::ZERO);
}

#[test]
fn explicit_locked_track_in_list_is_locked() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    p.track_mut(&t).unwrap().locked = true;
    let err = remove_range(&mut p, s(1), s(2), std::slice::from_ref(&t), false).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn all_tracks_mode_skips_locked_tracks() {
    let mut p = project();
    let (t1, t2) = (v1(&p), v2(&p));
    raw(&mut p, &t1, ClipKind::Video, s(0), s(10));
    raw(&mut p, &t2, ClipKind::Video, s(0), s(10));
    p.track_mut(&t2).unwrap().locked = true;
    remove_range(&mut p, s(2), s(4), &[], false).unwrap();
    assert_eq!(layout(&p, &t1), vec![(s(0), s(2)), (s(4), s(6))]);
    assert_eq!(layout(&p, &t2), vec![(s(0), s(10))]);
}

#[test]
fn extract_only_ripples_the_listed_tracks() {
    let mut p = project();
    let (t1, t2) = (v1(&p), v2(&p));
    raw(&mut p, &t1, ClipKind::Video, s(0), s(10));
    raw(&mut p, &t2, ClipKind::Video, s(6), s(4));
    remove_range(&mut p, s(2), s(4), std::slice::from_ref(&t1), true).unwrap();
    assert_eq!(layout(&p, &t1), vec![(s(0), s(2)), (s(2), s(6))]);
    assert_eq!(layout(&p, &t2), vec![(s(6), s(4))]);
}

#[test]
fn remove_range_with_start_not_before_end_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let before = p.clone();
    let err = remove_range(&mut p, s(4), s(4), &[], false).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn remove_range_with_negative_start_is_invalid() {
    let mut p = project();
    let err = remove_range(&mut p, Tick(-1), s(2), &[], false).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
}

#[test]
fn remove_range_unknown_track_is_not_found() {
    let mut p = project();
    let err = remove_range(&mut p, s(0), s(2), &["nowhere".to_string()], false).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

// ---------- close_gap / close_all_gaps ----------

#[test]
fn close_gap_moves_later_clips_left_over_the_gap() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    raw(&mut p, &t, ClipKind::Video, s(5), s(2));
    close_gap(&mut p, &t, s(3)).unwrap();
    assert_eq!(layout(&p, &t), vec![(s(0), s(2)), (s(2), s(2))]);
}

#[test]
fn close_gap_at_a_point_on_a_clip_is_a_no_op() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    raw(&mut p, &t, ClipKind::Video, s(5), s(2));
    let before = p.clone();
    close_gap(&mut p, &t, s(1)).unwrap();
    assert_eq!(p, before);
}

#[test]
fn close_gap_after_the_last_clip_is_a_no_op() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let before = p.clone();
    close_gap(&mut p, &t, s(9)).unwrap();
    assert_eq!(p, before);
}

#[test]
fn close_gap_on_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    raw(&mut p, &t, ClipKind::Video, s(5), s(2));
    p.track_mut(&t).unwrap().locked = true;
    let err = close_gap(&mut p, &t, s(3)).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn close_all_gaps_makes_clips_contiguous_from_first_start() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(1), s(1));
    raw(&mut p, &t, ClipKind::Video, s(5), s(2));
    raw(&mut p, &t, ClipKind::Video, s(9), s(1));
    close_all_gaps(&mut p, &t).unwrap();
    assert_eq!(
        layout(&p, &t),
        vec![(s(1), s(1)), (s(2), s(2)), (s(4), s(1))]
    );
}

#[test]
fn close_all_gaps_on_empty_track_is_ok() {
    let mut p = project();
    let t = a1(&p);
    let before = p.clone();
    close_all_gaps(&mut p, &t).unwrap();
    assert_eq!(p, before);
}

#[test]
fn close_all_gaps_on_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(1));
    raw(&mut p, &t, ClipKind::Video, s(3), s(1));
    p.track_mut(&t).unwrap().locked = true;
    let err = close_all_gaps(&mut p, &t).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn close_gap_on_unknown_track_is_not_found() {
    let mut p = project();
    let err = close_gap(&mut p, "nowhere", s(1)).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

// ---------- freeze_frame ----------

#[test]
fn freeze_splits_clip_and_inserts_a_freeze_clip_with_exact_ticks() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let fid = freeze_frame(&mut p, &orig, s(1), s(2)).unwrap();
    assert_eq!(
        layout(&p, &t),
        vec![(s(0), s(1)), (s(1), s(2)), (s(3), s(3))]
    );
    assert_eq!(clip(&p, &orig).dur, s(1));
    let fz = clip(&p, &fid);
    assert_eq!(fz.name, "Freeze");
    assert_eq!(fz.kind, ClipKind::Video);
    assert_eq!(fz.track_id, t);
    assert_eq!(fz.start, Tick(TICKS_PER_SECOND));
    assert_eq!(fz.dur, Tick(2 * TICKS_PER_SECOND));
}

#[test]
fn freeze_clip_has_zero_speed_and_zero_source_length() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let fid = freeze_frame(&mut p, &orig, s(1), s(2)).unwrap();
    let fz = clip(&p, &fid);
    assert_eq!(fz.speed, 0.0);
    assert_eq!(fz.src_len(), Tick::ZERO);
}

#[test]
fn freeze_source_time_is_the_frame_shown_at_the_point() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let fid = freeze_frame(&mut p, &orig, s(1), s(2)).unwrap();
    assert_eq!(clip(&p, &fid).src_in, s(1));
    let right = p.clips_on_track(&t)[2].clone();
    assert_eq!(
        right.src_in,
        s(1),
        "the right half continues from the same source time"
    );
}

#[test]
fn freeze_at_double_speed_uses_timeline_offset_times_speed() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.clip_mut(&orig).unwrap().speed = 2.0;
    let fid = freeze_frame(&mut p, &orig, s(1), Tick::from_frame(1, Rate::whole(30))).unwrap();
    assert_eq!(clip(&p, &fid).src_in, s(2));
}

#[test]
fn freeze_on_reversed_clip_shows_the_source_frame_from_the_end_side() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.clip_mut(&orig).unwrap().reverse = true;
    let fid = freeze_frame(&mut p, &orig, s(1), Tick::from_frame(1, Rate::whole(30))).unwrap();
    assert_eq!(clip(&p, &fid).src_in, s(3));
}

#[test]
fn freeze_shifts_later_clips_on_the_track_right_by_its_duration() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let later = raw(&mut p, &t, ClipKind::Video, s(6), s(2));
    let t2 = v2(&p);
    let other = raw(&mut p, &t2, ClipKind::Video, s(6), s(2));
    freeze_frame(&mut p, &orig, s(1), s(2)).unwrap();
    assert_eq!(clip(&p, &later).start, s(8));
    assert_eq!(clip(&p, &other).start, s(6), "other tracks do not move");
}

#[test]
fn freeze_with_duration_below_one_frame_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let before = p.clone();
    let err = freeze_frame(&mut p, &orig, s(1), Tick(1)).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn freeze_at_clip_start_or_end_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let before = p.clone();
    let at_start = freeze_frame(&mut p, &orig, s(0), s(1)).unwrap_err();
    assert!(
        matches!(at_start, EditError::Invalid(_)),
        "got {at_start:?}"
    );
    let at_end = freeze_frame(&mut p, &orig, s(4), s(1)).unwrap_err();
    assert!(matches!(at_end, EditError::Invalid(_)), "got {at_end:?}");
    assert_eq!(p, before);
}

#[test]
fn freeze_on_audio_clip_is_invalid() {
    let mut p = project();
    let t = a1(&p);
    let orig = raw(&mut p, &t, ClipKind::Audio, s(0), s(4));
    let err = freeze_frame(&mut p, &orig, s(1), s(1)).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
}

#[test]
fn freeze_on_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    let orig = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.track_mut(&t).unwrap().locked = true;
    let err = freeze_frame(&mut p, &orig, s(1), s(1)).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn freeze_unknown_clip_is_not_found() {
    let mut p = project();
    let err = freeze_frame(&mut p, "nope", s(1), s(1)).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

#[test]
fn freeze_does_not_split_linked_partners() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let v = raw(&mut p, &vt, ClipKind::Video, s(0), s(4));
    let a = raw(&mut p, &at, ClipKind::Audio, s(0), s(4));
    set_link(&mut p, &v, &a, "L");
    freeze_frame(&mut p, &v, s(1), s(1)).unwrap();
    assert_eq!(
        layout(&p, &at),
        vec![(s(0), s(4))],
        "audio partner untouched"
    );
    assert_eq!(clip(&p, &a).link_id.as_deref(), Some("L"));
}

#[test]
fn default_project_has_three_video_and_four_audio_tracks_unlocked() {
    let p = project();
    assert_eq!(all_tracks(&p).len(), 7);
    assert_eq!(video_tracks(&p).len(), 3);
    assert_eq!(audio_tracks(&p).len(), 4);
    assert!(p.tracks.iter().all(|t| !t.locked));
}
