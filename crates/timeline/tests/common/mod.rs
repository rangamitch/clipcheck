//! Shared fixtures for the timeline tests. Synthetic data only: no files, no clock.
#![allow(dead_code)]

use clipcheck_timeline::model::{
    Clip, ClipKind, MediaAudio, MediaKind, MediaRef, Project, TrackKind,
};
use clipcheck_timeline::ops_basic::{self, PlaceMode};
use clipcheck_timeline::time::Tick;

/// Id of the fixture video media (10 s, two audio streams).
pub const MEDIA_ID: &str = "media-video-1";

/// A 10 s video media with two audio streams ("Game" and "Microphone").
pub fn video_media(id: &str) -> MediaRef {
    MediaRef {
        id: id.to_string(),
        kind: MediaKind::Video,
        name: "synthetic.mp4".to_string(),
        path: "C:/synthetic/synthetic.mp4".to_string(),
        duration: Tick::from_secs(10),
        width: 1920,
        height: 1080,
        fps: 30.0,
        video_codec: "h264".to_string(),
        audio: vec![
            MediaAudio {
                title: "Game".to_string(),
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48_000,
            },
            MediaAudio {
                title: "Microphone".to_string(),
                codec: "aac".to_string(),
                channels: 1,
                sample_rate: 48_000,
            },
        ],
        ..MediaRef::default()
    }
}

/// A default project (V1..V3, A1..A4) holding the fixture media [`MEDIA_ID`].
pub fn project_with_media() -> Project {
    let mut p = Project::new("Fixture");
    p.media.push(video_media(MEDIA_ID));
    p
}

/// Id of the `index`-th track of `kind` (empty string when there is none).
pub fn nth_track(p: &Project, kind: TrackKind, index: usize) -> String {
    p.tracks
        .iter()
        .filter(|t| t.kind == kind)
        .nth(index)
        .map(|t| t.id.clone())
        .unwrap_or_default()
}

/// A clip built directly, with default properties otherwise.
pub fn clip(id: &str, track_id: &str, kind: ClipKind, start: Tick, dur: Tick) -> Clip {
    Clip {
        id: id.to_string(),
        track_id: track_id.to_string(),
        kind,
        start,
        dur,
        ..Clip::default()
    }
}

/// Places the fixture media at 0 with the whole source, through the public `place_media` op.
/// Returns the created clip ids (video first, then one audio clip per stream).
pub fn place_fixture_media(p: &mut Project) -> Vec<String> {
    ops_basic::place_media(
        p,
        MEDIA_ID,
        Tick::ZERO,
        Tick::ZERO,
        Tick::ZERO,
        None,
        PlaceMode::Overwrite,
    )
    .expect("place fixture media")
}
