//! Public-API tests for the microphone lane choice in `PlaceMedia`: which audio streams of a video
//! media are placed, given the stream titles and the `ptt_mix_gated` preference. Written from the
//! doc comments in `src/ops_basic.rs` (`place_media_with_mic`, `placed_audio_streams`) and
//! `src/cmd.rs` (`Session::set_ptt_mix_gated`). Helpers are duplicated per test file on purpose.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clipcheck_timeline::cmd::{Command, Session};
use clipcheck_timeline::model::{ClipKind, MediaAudio, MediaKind, MediaRef, Project};
use clipcheck_timeline::ops_basic::{add_media, place_media, place_media_with_mic, PlaceMode};
use clipcheck_timeline::time::Tick;

// ---------- helpers ----------

/// A 10 s video media whose audio streams carry the given titles, in order.
fn titled_media(path: &str, titles: &[&str]) -> MediaRef {
    MediaRef {
        kind: MediaKind::Video,
        name: path.to_string(),
        path: path.to_string(),
        duration: Tick::from_secs(10),
        width: 1920,
        height: 1080,
        fps: 30.0,
        video_codec: "h264".to_string(),
        audio: titles
            .iter()
            .map(|t| MediaAudio {
                title: (*t).to_string(),
                codec: "aac".to_string(),
                ..MediaAudio::default()
            })
            .collect(),
        ..MediaRef::default()
    }
}

/// A project holding one media with the given stream titles. Returns (project, media id).
fn project_with(titles: &[&str]) -> (Project, String) {
    let mut p = Project::new("mic");
    let id = add_media(&mut p, titled_media("C:/m/mic.mp4", titles)).expect("add media");
    (p, id)
}

/// Places `media_id` at zero with the whole source and the given microphone preference.
/// Returns the created clip ids (video first).
fn place_with(p: &mut Project, media_id: &str, gated: bool) -> Vec<String> {
    place_media_with_mic(
        p,
        media_id,
        Tick::ZERO,
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
        gated,
    )
    .expect("place media")
}

/// Original stream indexes (`audio.source_track`) of the audio clips among `ids`, in order.
/// Also checks that the first id is the video clip.
fn placed_sources(p: &Project, ids: &[String]) -> Vec<u32> {
    let clips: Vec<_> = ids
        .iter()
        .map(|id| {
            p.clips
                .iter()
                .find(|c| &c.id == id)
                .expect("created clip exists")
        })
        .collect();
    assert_eq!(clips[0].kind, ClipKind::Video, "first clip is the video");
    clips[1..]
        .iter()
        .map(|c| {
            assert_eq!(c.kind, ClipKind::Audio, "rest are audio clips");
            c.audio.source_track
        })
        .collect()
}

/// Placed stream indexes for a media with `titles`, under the given preference.
fn placed_for(titles: &[&str], gated: bool) -> Vec<u32> {
    let (mut p, m) = project_with(titles);
    let ids = place_with(&mut p, &m, gated);
    placed_sources(&p, &ids)
}

// Stream layout used by most tests: 0 Mix, 1 Game, 2 Microphone, 3 Push to talk, 4 Discord.
const FULL: [&str; 5] = ["Mix", "Game", "Microphone", "Push to talk", "Discord"];

// ---------- the Mix stream ----------

#[test]
fn mix_stream_is_never_placed_under_either_preference() {
    assert!(!placed_for(&FULL, true).contains(&0));
    assert!(!placed_for(&FULL, false).contains(&0));
}

#[test]
fn gated_true_places_push_to_talk_and_drops_microphone() {
    // Game, Push to talk, Discord. The Microphone (index 2) is not placed.
    assert_eq!(placed_for(&FULL, true), vec![1, 3, 4]);
}

#[test]
fn gated_false_places_microphone_and_drops_push_to_talk() {
    // Game, Microphone, Discord. The Push to talk (index 3) is not placed.
    assert_eq!(placed_for(&FULL, false), vec![1, 2, 4]);
}

#[test]
fn other_streams_are_always_placed_under_both_preferences() {
    for gated in [true, false] {
        let placed = placed_for(&FULL, gated);
        assert!(placed.contains(&1), "Game placed (gated={gated})");
        assert!(placed.contains(&4), "Discord placed (gated={gated})");
    }
}

#[test]
fn default_place_media_behaves_as_gated_true() {
    let (mut p, m) = project_with(&FULL);
    let ids = place_media(
        &mut p,
        &m,
        Tick::ZERO,
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .expect("place media");
    assert_eq!(placed_sources(&p, &ids), placed_for(&FULL, true));
    assert_eq!(placed_sources(&p, &ids), vec![1, 3, 4]);
}

// ---------- only one of the two microphones ----------

#[test]
fn only_microphone_is_placed_under_both_preferences() {
    let titles = ["Game", "Microphone"];
    assert_eq!(placed_for(&titles, true), vec![0, 1]);
    assert_eq!(placed_for(&titles, false), vec![0, 1]);
}

#[test]
fn only_push_to_talk_is_placed_under_both_preferences() {
    let titles = ["Game", "Push to talk"];
    assert_eq!(placed_for(&titles, true), vec![0, 1]);
    assert_eq!(placed_for(&titles, false), vec![0, 1]);
}

#[test]
fn neither_microphone_leaves_the_lanes_unchanged() {
    let titles = ["Game", "Discord"];
    assert_eq!(placed_for(&titles, true), vec![0, 1]);
    assert_eq!(placed_for(&titles, false), vec![0, 1]);
}

// ---------- title matching ----------

#[test]
fn titles_compare_trimmed_and_case_insensitively() {
    // Index 0 " Mix " is skipped. Kept: 1 "game", 2 " push TO talk ", 3 "MICROPHONE ".
    let titles = [" Mix ", "game", " push TO talk ", "MICROPHONE "];
    assert_eq!(placed_for(&titles, true), vec![1, 2]);
    assert_eq!(placed_for(&titles, false), vec![1, 3]);
}

#[test]
fn mix_title_match_is_case_insensitive() {
    let titles = ["mIX", "Game"];
    assert_eq!(placed_for(&titles, true), vec![1]);
}

// ---------- fallback: only a Mix ----------

#[test]
fn media_with_only_a_mix_stream_places_it_under_both_preferences() {
    assert_eq!(placed_for(&["Mix"], true), vec![0]);
    assert_eq!(placed_for(&["Mix"], false), vec![0]);
}

// ---------- session ----------

fn session_place(gated: Option<bool>) -> Vec<u32> {
    let (p, m) = project_with(&FULL);
    let mut s = Session::new(p);
    if let Some(g) = gated {
        s.set_ptt_mix_gated(g);
    }
    let applied = s
        .apply(Command::PlaceMedia {
            media_id: m,
            at: Tick::ZERO,
            src_in: Tick::ZERO,
            src_len: Tick::ZERO,
            video_track: None,
            mode: PlaceMode::Overwrite,
        })
        .expect("place through session");
    // The project starts empty, so the created clips are exactly the project's clips, video first.
    let ids: Vec<String> = s
        .project
        .clips
        .iter()
        .filter(|c| applied.created.contains(&c.id))
        .map(|c| c.id.clone())
        .collect();
    placed_sources(&s.project, &ids)
}

#[test]
fn session_default_follows_gated_true() {
    assert_eq!(session_place(None), vec![1, 3, 4]);
}

#[test]
fn session_set_ptt_mix_gated_false_places_microphone() {
    assert_eq!(session_place(Some(false)), vec![1, 2, 4]);
}

#[test]
fn session_set_ptt_mix_gated_true_places_push_to_talk() {
    assert_eq!(session_place(Some(true)), vec![1, 3, 4]);
}
