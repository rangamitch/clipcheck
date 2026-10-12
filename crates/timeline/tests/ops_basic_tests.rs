//! Public-API tests for the basic timeline operations: media bin, place, move, delete, split and
//! duplicate. Written from the doc comments in `src/ops_basic.rs` and `src/json.rs`.
//! Helpers are duplicated per test file on purpose (no shared module).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clipcheck_timeline::error::EditError;
use clipcheck_timeline::json;
use clipcheck_timeline::model::{
    new_id, Clip, ClipKind, EnvPoint, FadeCurve, Group, MediaAudio, MediaKind, MediaRef, Project,
    TrackKind,
};
use clipcheck_timeline::ops_basic::{
    add_media, delete_clips, duplicate_clips, move_clips, place_media, relink_media, remove_media,
    split_at, ClipMove, PlaceMode,
};
use clipcheck_timeline::time::{Rate, Tick, MAX_PROJECT_TICKS, TICKS_PER_SECOND};

// ---------- helpers ----------

fn s(n: i64) -> Tick {
    Tick::from_secs(n)
}

fn ms(n: i64) -> Tick {
    Tick::from_ms(n)
}

fn project() -> Project {
    Project::new("basic")
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

fn media(kind: MediaKind, path: &str, dur: Tick, streams: usize) -> MediaRef {
    MediaRef {
        kind,
        name: path.to_string(),
        path: path.to_string(),
        duration: dur,
        width: 1920,
        height: 1080,
        fps: 30.0,
        video_codec: "h264".to_string(),
        audio: (0..streams)
            .map(|i| MediaAudio {
                title: format!("S{i}"),
                codec: "aac".to_string(),
                ..MediaAudio::default()
            })
            .collect(),
        ..MediaRef::default()
    }
}

fn video_media(path: &str, dur_s: i64, streams: usize) -> MediaRef {
    media(MediaKind::Video, path, s(dur_s), streams)
}

fn audio_media(path: &str, dur_s: i64) -> MediaRef {
    media(MediaKind::Audio, path, s(dur_s), 1)
}

fn image_media(path: &str) -> MediaRef {
    media(MediaKind::Image, path, Tick::ZERO, 0)
}

fn add(p: &mut Project, m: MediaRef) -> String {
    add_media(p, m).expect("add media")
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

fn frame() -> Tick {
    Tick::from_frame(1, Rate::whole(30))
}

/// Video track 0 of a fresh project as a string id.
fn v1(p: &Project) -> String {
    video_tracks(p)[0].clone()
}

fn v2(p: &Project) -> String {
    video_tracks(p)[1].clone()
}

fn a1(p: &Project) -> String {
    audio_tracks(p)[0].clone()
}

fn a2(p: &Project) -> String {
    audio_tracks(p)[1].clone()
}

/// Deterministic LCG (no rand crate).
struct Lcg(u64);

impl Lcg {
    fn next_u(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u() % n
    }
}

// ---------- media bin: add / remove / relink ----------

#[test]
fn add_media_assigns_id_and_stores_media() {
    let mut p = project();
    let id = add(&mut p, video_media("C:/m/a.mp4", 10, 2));
    assert!(!id.is_empty());
    let m = p.media_ref(&id).expect("stored");
    assert_eq!(m.path, "C:/m/a.mp4");
    assert_eq!(m.duration, s(10));
    assert_eq!(p.media.len(), 1);
}

#[test]
fn add_media_empty_id_gets_fresh_unique_id() {
    let mut p = project();
    let a = add(&mut p, video_media("C:/m/a.mp4", 1, 0));
    let b = add(&mut p, video_media("C:/m/b.mp4", 1, 0));
    assert_ne!(a, b);
    assert!(p.media_ref(&a).is_some());
    assert!(p.media_ref(&b).is_some());
}

#[test]
fn add_media_same_path_returns_existing_id_without_duplicate() {
    let mut p = project();
    let first = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let second = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    assert_eq!(first, second);
    assert_eq!(p.media.len(), 1);
}

#[test]
fn add_media_explicit_id_already_used_is_invalid() {
    let mut p = project();
    let existing = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let mut other = video_media("C:/m/b.mp4", 10, 0);
    other.id = existing;
    let err = add_media(&mut p, other).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(p.media.len(), 1);
}

#[test]
fn add_media_negative_duration_is_invalid() {
    let mut p = project();
    let m = media(MediaKind::Video, "C:/m/neg.mp4", Tick(-1), 0);
    let err = add_media(&mut p, m).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert!(p.media.is_empty());
}

#[test]
fn remove_media_unknown_is_not_found() {
    let mut p = project();
    let err = remove_media(&mut p, "nope").unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

#[test]
fn remove_media_in_use_reports_clip_count() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let err = remove_media(&mut p, &m).unwrap_err();
    assert_eq!(err, EditError::InUse(1));
    assert!(p.media_ref(&m).is_some());
}

#[test]
fn remove_media_unused_is_removed() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    remove_media(&mut p, &m).unwrap();
    assert!(p.media_ref(&m).is_none());
}

#[test]
fn relink_keeps_clip_ranges_and_clears_missing() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/old.mp4", 10, 0));
    let id =
        place_media(&mut p, &m, s(0), s(2), s(4), None, PlaceMode::Overwrite).unwrap()[0].clone();
    for item in p.media.iter_mut() {
        item.missing = true;
    }
    let before = clip(&p, &id).clone();
    relink_media(&mut p, &m, video_media("C:/m/new.mp4", 20, 0)).unwrap();
    let rel = p.media_ref(&m).expect("media");
    assert_eq!(rel.path, "C:/m/new.mp4");
    assert!(!rel.missing);
    assert_eq!(rel.id, m);
    assert_eq!(*clip(&p, &id), before);
}

#[test]
fn relink_with_other_kind_is_invalid() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let err = relink_media(&mut p, &m, audio_media("C:/m/a.wav", 10)).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(p.media_ref(&m).expect("media").path, "C:/m/a.mp4");
}

#[test]
fn relink_unknown_media_is_not_found() {
    let mut p = project();
    let err = relink_media(&mut p, "nope", video_media("C:/m/a.mp4", 10, 0)).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

// ---------- place_media ----------

#[test]
fn place_video_with_two_streams_creates_one_video_and_two_linked_audio() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 2));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(ids.len(), 3);
    assert_eq!(p.clips.len(), 3);
    let video = clip(&p, &ids[0]);
    assert_eq!(video.kind, ClipKind::Video);
    assert_eq!(video.track_id, v1(&p));
    assert_eq!(video.dur, s(10));
    let link = video.link_id.clone().expect("video is linked");
    for id in &ids[1..] {
        let a = clip(&p, id);
        assert_eq!(a.kind, ClipKind::Audio);
        assert_eq!(a.link_id.as_deref(), Some(link.as_str()));
        assert_eq!(a.dur, s(10));
    }
}

#[test]
fn place_video_stream_zero_audible_and_stream_one_muted() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 2));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let first = clip(&p, &ids[1]);
    let second = clip(&p, &ids[2]);
    assert!(!first.audio.muted);
    assert_eq!(first.audio.source_track, 0);
    assert!(second.audio.muted);
    assert_eq!(second.audio.source_track, 1);
}

#[test]
fn place_video_audio_clips_go_on_consecutive_audio_tracks() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 2));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(clip(&p, &ids[1]).track_id, a1(&p));
    assert_eq!(clip(&p, &ids[2]).track_id, a2(&p));
}

#[test]
fn place_video_skips_locked_first_audio_track_and_uses_next_ones() {
    let mut p = project();
    let a1_id = a1(&p);
    p.track_mut(&a1_id).unwrap().locked = true;
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 2));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let audio = audio_tracks(&p);
    assert_eq!(clip(&p, &ids[1]).track_id, audio[1]);
    assert_eq!(clip(&p, &ids[2]).track_id, audio[2]);
}

#[test]
fn place_video_with_every_audio_track_locked_is_locked() {
    let mut p = project();
    for t in audio_tracks(&p) {
        p.track_mut(&t).unwrap().locked = true;
    }
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 1));
    let err = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
    assert!(p.clips.is_empty());
}

#[test]
fn place_default_video_track_skips_locked_first_video_track() {
    let mut p = project();
    p.track_mut(&v1(&p)).unwrap().locked = true;
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(clip(&p, &ids[0]).track_id, v2(&p));
}

#[test]
fn place_on_explicit_locked_video_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    p.track_mut(&t).unwrap().locked = true;
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let err = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        Some(&t),
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn place_overwrite_trims_existing_clip_exactly() {
    let mut p = project();
    let t = v1(&p);
    let old = raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let m = add(&mut p, video_media("C:/m/b.mp4", 20, 0));
    let ids = place_media(
        &mut p,
        &m,
        s(2),
        Tick::ZERO,
        s(4),
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(
        layout(&p, &t),
        vec![(s(0), s(2)), (s(2), s(4)), (s(6), s(4))]
    );
    assert_eq!(clip(&p, &old).dur, s(2), "left piece keeps the id");
    assert_eq!(clip(&p, &ids[0]).start, s(2));
    let right = p.clips_on_track(&t)[2].clone();
    assert_eq!(
        right.src_in,
        s(6),
        "right piece source advanced by the offset"
    );
    assert_ne!(right.id, old);
}

#[test]
fn place_insert_pushes_later_clips_right_exactly() {
    let mut p = project();
    let t = v1(&p);
    let old = raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let m = add(&mut p, video_media("C:/m/b.mp4", 20, 0));
    place_media(&mut p, &m, s(5), Tick::ZERO, s(3), None, PlaceMode::Insert).unwrap();
    assert_eq!(
        layout(&p, &t),
        vec![(s(0), s(5)), (s(5), s(3)), (s(8), s(5))]
    );
    assert_eq!(clip(&p, &old).dur, s(5));
}

#[test]
fn place_insert_moves_clip_that_starts_after_point() {
    let mut p = project();
    let t = v1(&p);
    let later = raw(&mut p, &t, ClipKind::Video, s(10), s(2));
    let m = add(&mut p, video_media("C:/m/b.mp4", 20, 0));
    place_media(&mut p, &m, s(0), Tick::ZERO, s(5), None, PlaceMode::Insert).unwrap();
    assert_eq!(clip(&p, &later).start, s(15));
}

#[test]
fn place_append_lands_at_end_of_track() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let m = add(&mut p, video_media("C:/m/b.mp4", 20, 0));
    let ids = place_media(&mut p, &m, s(2), Tick::ZERO, s(3), None, PlaceMode::Append).unwrap();
    assert_eq!(clip(&p, &ids[0]).start, s(10));
    assert_eq!(clip(&p, &ids[0]).dur, s(3));
}

#[test]
fn place_append_occupied_audio_lane_at_video_end_is_overlap() {
    // Doc: Append places at the end of the video track. A long clip on the audio lane the new
    // media needs at that start is refused with Overlap, never shifted, and nothing changes.
    let mut p = project();
    let long_audio = add(&mut p, audio_media("C:/m/long-audio.wav", 20));
    place_media(
        &mut p,
        &long_audio,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let m = add(&mut p, video_media("C:/m/two-streams.mp4", 2, 2));
    let before = p.clone();
    let err = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Append,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Overlap(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn place_append_ignores_audio_lane_length_when_the_lane_is_free() {
    // Doc: Append places at the end of the video track; audio lanes do not push it. The audio
    // lane is only 4 s long, so the audio clip lands at the same 10 s start as the video.
    let mut p = project();
    let short_audio = add(&mut p, audio_media("C:/m/short-audio.wav", 4));
    place_media(
        &mut p,
        &short_audio,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(10));
    let m = add(&mut p, video_media("C:/m/two-streams.mp4", 2, 2));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Append,
    )
    .unwrap();
    assert_eq!(
        clip(&p, &ids[0]).start,
        s(10),
        "video starts after the video track ends"
    );
    assert_eq!(
        clip(&p, &ids[1]).start,
        s(10),
        "audio stream 0 starts at the same point"
    );
    assert_eq!(layout(&p, &a1(&p)), vec![(s(0), s(4)), (s(10), s(2))]);
}

#[test]
fn place_append_audio_only_media_goes_after_the_first_audio_track() {
    // Doc: audio-only media appends after the first audio track, not after the video track.
    let mut p = project();
    let first = add(&mut p, audio_media("C:/m/a-first.wav", 3));
    place_media(
        &mut p,
        &first,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let second = add(&mut p, audio_media("C:/m/a-second.wav", 2));
    let ids = place_media(
        &mut p,
        &second,
        s(9),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Append,
    )
    .unwrap();
    assert_eq!(clip(&p, &ids[0]).track_id, a1(&p));
    assert_eq!(clip(&p, &ids[0]).start, s(3));
}

#[test]
fn place_append_uses_only_the_chosen_video_track() {
    // Doc: Append places at the end of the chosen video track. A longer clip on V2 is ignored
    // when V1 is chosen, and V2's own end is used when V2 is chosen.
    let mut p = project();
    let v2_id = v2(&p);
    raw(&mut p, &v2_id, ClipKind::Video, s(0), s(30));
    let m = add(&mut p, video_media("C:/m/c.mp4", 2, 0));
    let v1_id = v1(&p);
    let on_v1 = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        Some(&v1_id),
        PlaceMode::Append,
    )
    .unwrap();
    assert_eq!(clip(&p, &on_v1[0]).track_id, v1_id);
    assert_eq!(clip(&p, &on_v1[0]).start, Tick::ZERO);
    let on_v2 = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        Some(&v2_id),
        PlaceMode::Append,
    )
    .unwrap();
    assert_eq!(clip(&p, &on_v2[0]).start, s(30));
}

#[test]
fn place_zero_src_len_means_to_end_of_media() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        s(3),
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    let c = clip(&p, &ids[0]);
    assert_eq!(c.dur, s(7));
    assert_eq!(c.src_in, s(3));
    assert_eq!(c.src_out(), s(10));
}

#[test]
fn place_src_range_past_media_end_is_invalid() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let err = place_media(&mut p, &m, s(0), s(9), s(2), None, PlaceMode::Overwrite).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert!(p.clips.is_empty());
}

#[test]
fn place_src_in_at_media_end_with_zero_len_is_invalid() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let err = place_media(
        &mut p,
        &m,
        s(0),
        s(10),
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
}

#[test]
fn place_image_default_length_is_five_seconds() {
    let mut p = project();
    let m = add(&mut p, image_media("C:/m/still.png"));
    let ids = place_media(
        &mut p,
        &m,
        s(1),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(ids.len(), 1);
    let c = clip(&p, &ids[0]);
    assert_eq!(c.kind, ClipKind::Image);
    assert_eq!(c.dur, s(5));
    assert_eq!(c.track_id, v1(&p));
}

#[test]
fn place_image_explicit_length_is_used() {
    let mut p = project();
    let m = add(&mut p, image_media("C:/m/still.png"));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        s(2),
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(clip(&p, &ids[0]).dur, s(2));
}

#[test]
fn place_audio_only_media_creates_one_audio_clip_and_no_video() {
    let mut p = project();
    let m = add(&mut p, audio_media("C:/m/voice.wav", 6));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(ids.len(), 1);
    let c = clip(&p, &ids[0]);
    assert_eq!(c.kind, ClipKind::Audio);
    assert_eq!(c.track_id, a1(&p));
    assert_eq!(p.clips.len(), 1);
}

#[test]
fn place_more_than_eight_audio_streams_is_capped_at_eight_tracks() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/many.mkv", 10, 10));
    let ids = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(ids.len(), 9, "one video and eight audio clips");
    assert_eq!(audio_tracks(&p).len(), 8);
}

#[test]
fn place_with_video_track_of_audio_kind_is_wrong_kind() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let audio = a1(&p);
    let err = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        Some(&audio),
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::WrongTrackKind(_)), "got {err:?}");
}

#[test]
fn place_unknown_media_is_not_found() {
    let mut p = project();
    let err = place_media(
        &mut p,
        "missing",
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

#[test]
fn place_zero_duration_video_is_invalid() {
    let mut p = project();
    let m = add(
        &mut p,
        media(MediaKind::Video, "C:/m/empty.mp4", Tick::ZERO, 0),
    );
    let err = place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
}

#[test]
fn place_negative_at_is_invalid() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    let err = place_media(
        &mut p,
        &m,
        Tick(-1),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
}

#[test]
fn place_is_atomic_on_error() {
    let mut p = project();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 2));
    place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        s(4),
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    for t in audio_tracks(&p) {
        p.track_mut(&t).unwrap().locked = true;
    }
    let before = p.clone();
    let err = place_media(
        &mut p,
        &m,
        s(4),
        Tick::ZERO,
        s(2),
        None,
        PlaceMode::Overwrite,
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn place_does_not_change_canvas() {
    let mut p = project();
    let canvas_before = p.canvas.clone();
    let m = add(&mut p, video_media("C:/m/a.mp4", 10, 0));
    place_media(
        &mut p,
        &m,
        s(0),
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .unwrap();
    assert_eq!(p.canvas, canvas_before);
}

// ---------- move_clips ----------

#[test]
fn move_clip_to_free_space_changes_only_start() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id.clone(),
            new_start: s(10),
            new_track: None,
        }],
    )
    .unwrap();
    assert_eq!(clip(&p, &id).start, s(10));
    assert_eq!(clip(&p, &id).track_id, t);
    assert_eq!(clip(&p, &id).dur, s(2));
}

#[test]
fn move_into_unmoved_clip_is_overlap() {
    let mut p = project();
    let t = v1(&p);
    let a = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    raw(&mut p, &t, ClipKind::Video, s(5), s(2));
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: a,
            new_start: s(4),
            new_track: None,
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Overlap(_)), "got {err:?}");
}

#[test]
fn move_video_moves_linked_audio_partner_by_same_delta() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let v = raw(&mut p, &vt, ClipKind::Video, s(0), s(4));
    let a = raw(&mut p, &at, ClipKind::Audio, s(0), s(4));
    set_link(&mut p, &v, &a, "L");
    move_clips(
        &mut p,
        &[ClipMove {
            clip_id: v.clone(),
            new_start: s(10),
            new_track: None,
        }],
    )
    .unwrap();
    assert_eq!(clip(&p, &v).start, s(10));
    assert_eq!(clip(&p, &a).start, s(10));
    assert_eq!(clip(&p, &a).track_id, at);
}

#[test]
fn move_linked_pair_keeps_relative_offset_when_audio_is_moved() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let v = raw(&mut p, &vt, ClipKind::Video, s(2), s(4));
    let a = raw(&mut p, &at, ClipKind::Audio, s(2), s(4));
    set_link(&mut p, &v, &a, "L");
    move_clips(
        &mut p,
        &[ClipMove {
            clip_id: a.clone(),
            new_start: s(7),
            new_track: None,
        }],
    )
    .unwrap();
    assert_eq!(clip(&p, &v).start, s(7));
    assert_eq!(clip(&p, &a).start, s(7));
}

#[test]
fn move_video_clip_to_audio_track_is_wrong_kind() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let audio = a1(&p);
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id,
            new_start: s(0),
            new_track: Some(audio),
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::WrongTrackKind(_)), "got {err:?}");
}

#[test]
fn move_to_other_video_track_of_same_kind_works() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let dest = v2(&p);
    move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id.clone(),
            new_start: s(3),
            new_track: Some(dest.clone()),
        }],
    )
    .unwrap();
    assert_eq!(clip(&p, &id).track_id, dest);
    assert_eq!(clip(&p, &id).start, s(3));
}

#[test]
fn move_to_negative_start_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(1), s(2));
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id.clone(),
            new_start: Tick(-1),
            new_track: None,
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(clip(&p, &id).start, s(1));
}

#[test]
fn move_locked_source_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    p.track_mut(&t).unwrap().locked = true;
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id,
            new_start: s(5),
            new_track: None,
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn move_to_locked_destination_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let dest = v2(&p);
    p.track_mut(&dest).unwrap().locked = true;
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id,
            new_start: s(0),
            new_track: Some(dest),
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn move_unknown_clip_is_not_found() {
    let mut p = project();
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: "nope".into(),
            new_start: s(0),
            new_track: None,
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

#[test]
fn move_unknown_destination_track_is_not_found() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let err = move_clips(
        &mut p,
        &[ClipMove {
            clip_id: id,
            new_start: s(0),
            new_track: Some("nowhere".into()),
        }],
    )
    .unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

#[test]
fn move_empty_list_is_ok_and_changes_nothing() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let before = p.clone();
    move_clips(&mut p, &[]).unwrap();
    assert_eq!(p, before);
}

// ---------- delete_clips ----------

#[test]
fn delete_without_ripple_leaves_a_gap() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let b = raw(&mut p, &t, ClipKind::Video, s(2), s(2));
    let c = raw(&mut p, &t, ClipKind::Video, s(4), s(2));
    let deleted = delete_clips(&mut p, std::slice::from_ref(&b), false).unwrap();
    assert_eq!(deleted, vec![b]);
    assert_eq!(clip(&p, &c).start, s(4));
    assert_eq!(layout(&p, &t), vec![(s(0), s(2)), (s(4), s(2))]);
}

#[test]
fn delete_with_ripple_closes_gap_on_same_track() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let b = raw(&mut p, &t, ClipKind::Video, s(2), s(2));
    let c = raw(&mut p, &t, ClipKind::Video, s(4), s(2));
    delete_clips(&mut p, std::slice::from_ref(&b), true).unwrap();
    assert_eq!(clip(&p, &c).start, s(2));
}

#[test]
fn delete_with_ripple_does_not_shift_other_tracks() {
    let mut p = project();
    let t = v1(&p);
    let other = v2(&p);
    let b = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let d = raw(&mut p, &other, ClipKind::Video, s(4), s(2));
    delete_clips(&mut p, std::slice::from_ref(&b), true).unwrap();
    assert_eq!(clip(&p, &d).start, s(4));
}

#[test]
fn delete_linked_clip_also_deletes_its_partners() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let v = raw(&mut p, &vt, ClipKind::Video, s(0), s(4));
    let a = raw(&mut p, &at, ClipKind::Audio, s(0), s(4));
    let a2_id = a2(&p);
    let a_second = raw(&mut p, &a2_id, ClipKind::Audio, s(0), s(4));
    set_link(&mut p, &v, &a, "L");
    p.clip_mut(&a_second).unwrap().link_id = Some("L".to_string());
    let mut deleted = delete_clips(&mut p, std::slice::from_ref(&v), false).unwrap();
    deleted.sort();
    let mut expected = vec![v, a, a_second];
    expected.sort();
    assert_eq!(deleted, expected);
    assert!(p.clips.is_empty());
}

#[test]
fn delete_removes_groups_no_clip_references_any_more() {
    let mut p = project();
    let t = v1(&p);
    p.groups.push(Group {
        id: "orphan".into(),
        name: "Old".into(),
        color: None,
    });
    let b = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    delete_clips(&mut p, std::slice::from_ref(&b), false).unwrap();
    assert!(p.groups.iter().all(|g| g.id != "orphan"));
}

#[test]
fn delete_keeps_group_that_still_has_members() {
    let mut p = project();
    let t = v1(&p);
    p.groups.push(Group {
        id: "g".into(),
        name: "Keep".into(),
        color: None,
    });
    let b = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let keep = raw(&mut p, &t, ClipKind::Video, s(5), s(2));
    p.clip_mut(&keep).unwrap().group_id = Some("g".into());
    delete_clips(&mut p, std::slice::from_ref(&b), false).unwrap();
    assert!(p.groups.iter().any(|g| g.id == "g"));
}

#[test]
fn delete_unknown_clip_is_not_found() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let before = p.clone();
    let err = delete_clips(&mut p, &["nope".to_string()], false).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn delete_on_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    p.track_mut(&t).unwrap().locked = true;
    let err = delete_clips(&mut p, std::slice::from_ref(&id), false).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
    assert!(p.clip(&id).is_some());
}

#[test]
fn delete_empty_list_returns_nothing() {
    let mut p = project();
    let deleted = delete_clips(&mut p, &[], true).unwrap();
    assert!(deleted.is_empty());
}

// ---------- split_at ----------

#[test]
fn split_in_middle_gives_two_clips_whose_durations_sum_to_original() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let new_ids = split_at(&mut p, s(1), std::slice::from_ref(&id)).unwrap();
    assert_eq!(new_ids.len(), 1);
    let left = clip(&p, &id);
    let right = clip(&p, &new_ids[0]);
    assert_eq!(left.start, s(0));
    assert_eq!(left.dur, s(1));
    assert_eq!(right.start, s(1));
    assert_eq!(right.dur, s(3));
    assert_eq!(left.dur + right.dur, s(4));
    assert_ne!(right.id, id);
}

#[test]
fn split_advances_source_in_by_offset_times_speed_at_speed_two() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    {
        let c = p.clip_mut(&id).unwrap();
        c.speed = 2.0;
        c.src_in = s(1);
    }
    let orig_src_out = clip(&p, &id).src_out();
    let new_ids = split_at(&mut p, s(1), std::slice::from_ref(&id)).unwrap();
    let right = clip(&p, &new_ids[0]);
    assert_eq!(right.src_in, s(3), "1 s + 1 s timeline offset * speed 2");
    assert_eq!(right.src_out(), orig_src_out, "source end is unchanged");
    assert_eq!(clip(&p, &id).src_out(), s(3));
}

#[test]
fn split_advances_source_in_by_offset_times_speed_at_half_speed() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    {
        let c = p.clip_mut(&id).unwrap();
        c.speed = 0.5;
        c.src_in = Tick::ZERO;
    }
    let new_ids = split_at(&mut p, s(2), std::slice::from_ref(&id)).unwrap();
    let right = clip(&p, &new_ids[0]);
    assert_eq!(right.src_in, s(1), "2 s * speed 0.5");
    assert_eq!(right.dur, s(2));
    assert_eq!(right.src_len(), s(1));
}

#[test]
fn split_reverse_clip_gives_left_piece_the_advanced_source() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.clip_mut(&id).unwrap().reverse = true;
    let new_ids = split_at(&mut p, s(1), std::slice::from_ref(&id)).unwrap();
    let left = clip(&p, &id);
    let right = clip(&p, &new_ids[0]);
    assert_eq!(left.dur, s(1));
    assert_eq!(
        left.src_in,
        s(3),
        "reversed: the left piece plays the end of the source"
    );
    assert_eq!(right.src_in, Tick::ZERO);
    assert_eq!(right.dur, s(3));
    assert!(right.reverse && left.reverse);
}

#[test]
fn split_divides_fades_fade_in_left_and_fade_out_right() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    {
        let c = p.clip_mut(&id).unwrap();
        c.audio.fade_in = s(1);
        c.audio.fade_out = s(1);
        c.audio.fade_curve = FadeCurve::EqualPower;
    }
    let new_ids = split_at(&mut p, s(1), std::slice::from_ref(&id)).unwrap();
    let left = clip(&p, &id);
    let right = clip(&p, &new_ids[0]);
    assert_eq!(left.audio.fade_in, s(1));
    assert_eq!(left.audio.fade_out, Tick::ZERO);
    assert_eq!(right.audio.fade_in, Tick::ZERO);
    assert_eq!(right.audio.fade_out, s(1));
    assert_eq!(right.audio.fade_curve, FadeCurve::EqualPower);
}

#[test]
fn split_divides_envelope_points_and_rebases_right_piece() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.clip_mut(&id).unwrap().audio.envelope = vec![
        EnvPoint {
            t: ms(500),
            db: -3.0,
        },
        EnvPoint {
            t: ms(2500),
            db: -6.0,
        },
    ];
    let new_ids = split_at(&mut p, s(2), std::slice::from_ref(&id)).unwrap();
    let left = &clip(&p, &id).audio.envelope;
    let right = &clip(&p, &new_ids[0]).audio.envelope;
    assert_eq!(
        left,
        &vec![EnvPoint {
            t: ms(500),
            db: -3.0
        }]
    );
    assert_eq!(
        right,
        &vec![EnvPoint {
            t: ms(500),
            db: -6.0
        }]
    );
}

#[test]
fn split_linked_pair_gives_right_halves_a_new_shared_link() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let v = raw(&mut p, &vt, ClipKind::Video, s(0), s(4));
    let a = raw(&mut p, &at, ClipKind::Audio, s(0), s(4));
    set_link(&mut p, &v, &a, "OLD");
    let new_ids = split_at(&mut p, s(1), std::slice::from_ref(&v)).unwrap();
    assert_eq!(new_ids.len(), 2, "the audio partner is split too");
    let right_v = clip(&p, &new_ids[0]).clone();
    let right_a = clip(&p, &new_ids[1]).clone();
    assert_eq!(clip(&p, &v).link_id.as_deref(), Some("OLD"));
    assert_eq!(clip(&p, &a).link_id.as_deref(), Some("OLD"));
    let shared = right_v.link_id.clone().expect("right video linked");
    assert_ne!(shared, "OLD");
    assert_eq!(right_a.link_id.as_deref(), Some(shared.as_str()));
}

#[test]
fn split_grouped_clip_gives_right_half_a_new_copy_of_the_group() {
    let mut p = project();
    let t = v1(&p);
    p.groups.push(Group {
        id: "g1".into(),
        name: "Intro".into(),
        color: Some("#fff".into()),
    });
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.clip_mut(&id).unwrap().group_id = Some("g1".into());
    let new_ids = split_at(&mut p, s(1), std::slice::from_ref(&id)).unwrap();
    let right_group = clip(&p, &new_ids[0]).group_id.clone().expect("grouped");
    assert_ne!(right_group, "g1");
    let g = p
        .groups
        .iter()
        .find(|g| g.id == right_group)
        .expect("new group stored");
    assert_eq!(g.name, "Intro");
    assert_eq!(g.color.as_deref(), Some("#fff"));
    assert_eq!(clip(&p, &id).group_id.as_deref(), Some("g1"));
}

#[test]
fn split_at_clip_start_is_a_no_op() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(2), s(4));
    let before = p.clone();
    let new_ids = split_at(&mut p, s(2), std::slice::from_ref(&id)).unwrap();
    assert!(new_ids.is_empty());
    assert_eq!(p, before);
}

#[test]
fn split_at_clip_end_is_a_no_op() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(2), s(4));
    let before = p.clone();
    let new_ids = split_at(&mut p, s(6), std::slice::from_ref(&id)).unwrap();
    assert!(new_ids.is_empty());
    assert_eq!(p, before);
}

#[test]
fn split_with_no_ids_splits_only_targeted_unlocked_spanning_clips() {
    let mut p = project();
    let (t1, t2, t3) = (v1(&p), v2(&p), video_tracks(&p)[2].clone());
    let a = raw(&mut p, &t1, ClipKind::Video, s(0), s(4));
    let b = raw(&mut p, &t2, ClipKind::Video, s(0), s(4));
    let c = raw(&mut p, &t3, ClipKind::Video, s(0), s(4));
    p.track_mut(&t2).unwrap().targeted = false;
    p.track_mut(&t3).unwrap().locked = true;
    let new_ids = split_at(&mut p, s(2), &[]).unwrap();
    assert_eq!(new_ids.len(), 1);
    assert_eq!(clip(&p, &a).dur, s(2));
    assert_eq!(clip(&p, &b).dur, s(4), "untargeted track skipped");
    assert_eq!(clip(&p, &c).dur, s(4), "locked track skipped");
}

#[test]
fn split_listed_clip_on_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    p.track_mut(&t).unwrap().locked = true;
    let before = p.clone();
    let err = split_at(&mut p, s(1), std::slice::from_ref(&id)).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn split_listed_piece_shorter_than_a_frame_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let end = s(4);
    let err = split_at(&mut p, end - Tick(1), std::slice::from_ref(&id)).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(clip(&p, &id).dur, s(4));
}

#[test]
fn split_listed_piece_of_exactly_one_frame_is_allowed() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let new_ids = split_at(&mut p, frame(), std::slice::from_ref(&id)).unwrap();
    assert_eq!(clip(&p, &id).dur, frame());
    assert_eq!(clip(&p, &new_ids[0]).dur, s(4) - frame());
}

#[test]
fn split_time_outside_tick_range_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    raw(&mut p, &t, ClipKind::Video, s(0), s(4));
    let err = split_at(&mut p, Tick(-5), &[]).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
}

#[test]
fn split_unknown_clip_is_not_found() {
    let mut p = project();
    let err = split_at(&mut p, s(1), &["nope".to_string()]).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

// ---------- duplicate_clips ----------

#[test]
fn duplicate_shifts_copy_by_offset_and_gives_new_ids() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let new_ids = duplicate_clips(&mut p, std::slice::from_ref(&id), s(10)).unwrap();
    assert_eq!(new_ids.len(), 1);
    assert_ne!(new_ids[0], id);
    assert_eq!(clip(&p, &new_ids[0]).start, s(10));
    assert_eq!(clip(&p, &new_ids[0]).track_id, t);
    assert_eq!(clip(&p, &id).start, s(0));
    assert_eq!(p.clips.len(), 2);
}

#[test]
fn duplicate_onto_existing_clip_is_overlap_and_leaves_project_unchanged() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let before = p.clone();
    let err = duplicate_clips(&mut p, std::slice::from_ref(&id), s(1)).unwrap_err();
    assert!(matches!(err, EditError::Overlap(_)), "got {err:?}");
    assert_eq!(p, before);
}

#[test]
fn duplicate_negative_result_start_is_invalid() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let err =
        duplicate_clips(&mut p, std::slice::from_ref(&id), Tick(-TICKS_PER_SECOND)).unwrap_err();
    assert!(matches!(err, EditError::Invalid(_)), "got {err:?}");
    assert_eq!(p.clips.len(), 1);
}

#[test]
fn duplicate_linked_pair_copies_partner_with_new_shared_link() {
    let mut p = project();
    let (vt, at) = (v1(&p), a1(&p));
    let v = raw(&mut p, &vt, ClipKind::Video, s(0), s(4));
    let a = raw(&mut p, &at, ClipKind::Audio, s(0), s(4));
    set_link(&mut p, &v, &a, "OLD");
    let new_ids = duplicate_clips(&mut p, std::slice::from_ref(&v), s(10)).unwrap();
    assert_eq!(new_ids.len(), 2, "partner duplicated with the clip");
    let copy_v = clip(&p, &new_ids[0]).clone();
    let copy_a = clip(&p, &new_ids[1]).clone();
    assert_eq!(copy_v.start, s(10));
    assert_eq!(copy_a.start, s(10));
    let link = copy_v.link_id.clone().expect("copy linked");
    assert_ne!(link, "OLD");
    assert_eq!(copy_a.link_id.as_deref(), Some(link.as_str()));
    assert_eq!(clip(&p, &a).link_id.as_deref(), Some("OLD"));
}

#[test]
fn duplicate_grouped_clip_gets_new_group_copy() {
    let mut p = project();
    let t = v1(&p);
    p.groups.push(Group {
        id: "g".into(),
        name: "Scene".into(),
        color: None,
    });
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    p.clip_mut(&id).unwrap().group_id = Some("g".into());
    let new_ids = duplicate_clips(&mut p, std::slice::from_ref(&id), s(5)).unwrap();
    let gid = clip(&p, &new_ids[0]).group_id.clone().expect("grouped");
    assert_ne!(gid, "g");
    assert_eq!(
        p.groups.iter().find(|g| g.id == gid).expect("group").name,
        "Scene"
    );
}

#[test]
fn duplicate_empty_list_returns_nothing() {
    let mut p = project();
    let before = p.clone();
    let ids = duplicate_clips(&mut p, &[], s(3)).unwrap();
    assert!(ids.is_empty());
    assert_eq!(p, before);
}

#[test]
fn duplicate_unknown_clip_is_not_found() {
    let mut p = project();
    let err = duplicate_clips(&mut p, &["nope".to_string()], s(1)).unwrap_err();
    assert!(matches!(err, EditError::NotFound(_)), "got {err:?}");
}

#[test]
fn duplicate_clip_on_locked_track_is_locked() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    p.track_mut(&t).unwrap().locked = true;
    let err = duplicate_clips(&mut p, std::slice::from_ref(&id), s(5)).unwrap_err();
    assert!(matches!(err, EditError::Locked(_)), "got {err:?}");
}

#[test]
fn duplicate_past_four_hour_limit_is_limit_error() {
    let mut p = project();
    let t = v1(&p);
    let id = raw(&mut p, &t, ClipKind::Video, s(0), s(2));
    let offset = Tick(MAX_PROJECT_TICKS);
    let err = duplicate_clips(&mut p, std::slice::from_ref(&id), offset).unwrap_err();
    assert!(matches!(err, EditError::Limit(_)), "got {err:?}");
}

// ---------- property-style: random operations keep the timeline valid ----------

fn run_random_ops(seed: u64, steps: usize) -> usize {
    let mut p = project();
    let pool = [
        add(&mut p, video_media("C:/m/prop1.mp4", 20, 2)),
        add(&mut p, video_media("C:/m/prop2.mp4", 8, 1)),
        add(&mut p, image_media("C:/m/prop3.png")),
    ];
    let mut rng = Lcg(seed);
    let mut succeeded = 0usize;
    for _ in 0..steps {
        let before = p.clone();
        let ids: Vec<String> = p.clips.iter().map(|c| c.id.clone()).collect();
        let result: Result<(), EditError> = match rng.below(4) {
            0 => {
                let idx = rng.below(pool.len() as u64) as usize;
                let media_id = pool.get(idx).cloned().unwrap_or_default();
                let at = ms(rng.below(60_000) as i64);
                let mode = match rng.below(3) {
                    0 => PlaceMode::Overwrite,
                    1 => PlaceMode::Insert,
                    _ => PlaceMode::Append,
                };
                let len = ms(500 + rng.below(4000) as i64);
                place_media(&mut p, &media_id, at, Tick::ZERO, len, None, mode).map(|_| ())
            }
            1 => {
                let at = ms(rng.below(60_000) as i64);
                split_at(&mut p, at, &[]).map(|_| ())
            }
            2 => match ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                Some(id) => {
                    let new_start = ms(rng.below(60_000) as i64);
                    move_clips(
                        &mut p,
                        &[ClipMove {
                            clip_id: id.clone(),
                            new_start,
                            new_track: None,
                        }],
                    )
                }
                None => Ok(()),
            },
            _ => match ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                Some(id) => {
                    let ripple = rng.below(2) == 0;
                    delete_clips(&mut p, std::slice::from_ref(id), ripple).map(|_| ())
                }
                None => Ok(()),
            },
        };
        match result {
            Ok(()) => succeeded += 1,
            Err(_) => assert!(
                p == before,
                "a refused edit must leave the project unchanged"
            ),
        }
        let issues = json::validate(&p);
        assert!(
            issues.iter().all(|i| i.code != "overlap"),
            "overlap reported after an accepted edit: {issues:?}"
        );
        for t in video_tracks(&p).iter().chain(audio_tracks(&p).iter()) {
            let total: i64 = p.clips_on_track(t).iter().map(|c| c.dur.0).sum();
            assert!(
                total <= MAX_PROJECT_TICKS,
                "track {t} longer than the 4 hour limit"
            );
        }
        for c in &p.clips {
            assert!(
                c.end().0 <= MAX_PROJECT_TICKS,
                "clip {} ends past the limit",
                c.id
            );
        }
    }
    succeeded
}

#[test]
fn property_300_random_ops_seed_42_never_overlap() {
    let ok = run_random_ops(42, 300);
    assert!(
        ok >= 60,
        "only {ok} of 300 random ops were accepted; the walk is too weak"
    );
}

#[test]
fn property_300_random_ops_seed_7_never_overlap() {
    let ok = run_random_ops(7, 300);
    assert!(
        ok >= 60,
        "only {ok} of 300 random ops were accepted; the walk is too weak"
    );
}

#[test]
fn property_seconds_constant_matches_docs() {
    // One second is 254,016,000,000 ticks (docs/design/13-video-editor.md section 14.2).
    assert_eq!(TICKS_PER_SECOND, 254_016_000_000);
    assert_eq!(s(1), Tick(TICKS_PER_SECOND));
}
