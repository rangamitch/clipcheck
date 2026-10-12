//! Project file load, save, sanitise and validate (docs/design/13-video-editor.md). Public API only.

mod common;

use clipcheck_timeline::json::{from_json, migrate, sanitize, to_json, validate, LoadError};
use clipcheck_timeline::model::{
    Canvas, Clip, ClipAudio, ClipKind, EditModes, Group, Marker, MediaAudio, MediaKind, MediaRef,
    Project, Rgba, TextSpec, TrackKind, Transition, TransitionKind, UiState, WorkArea,
};
use clipcheck_timeline::time::{Rate, Tick, MAX_PROJECT_TICKS, MAX_TICK};
use common::{clip, nth_track, project_with_media, MEDIA_ID};

/// A project that exercises most fields: media, linked and grouped clips, a title, a solid,
/// a transition, a marker, a work area, non-default canvas and UI state.
fn populated_project() -> Project {
    let mut p = project_with_media();
    p.name = "Populated".to_string();
    p.canvas = Canvas {
        width: 1280,
        height: 720,
        fps: Rate {
            num: 24_000,
            den: 1001,
        },
        background: Rgba::BLACK,
        aspect_preset: "16:9".to_string(),
        blur_background: true,
    };
    p.media.push(MediaRef {
        id: "media-audio-1".to_string(),
        kind: MediaKind::Audio,
        name: "voice.wav".to_string(),
        path: "C:/synthetic/voice.wav".to_string(),
        duration: Tick::from_secs(8),
        audio: vec![MediaAudio {
            title: "VO".to_string(),
            codec: "pcm".to_string(),
            channels: 1,
            sample_rate: 48_000,
        }],
        ..MediaRef::default()
    });
    let v1 = nth_track(&p, TrackKind::Video, 0);
    let v2 = nth_track(&p, TrackKind::Video, 1);
    let a1 = nth_track(&p, TrackKind::Audio, 0);
    let a2 = nth_track(&p, TrackKind::Audio, 1);
    p.tracks[4].volume_db = -6.0;
    p.groups.push(Group {
        id: "grp-1".to_string(),
        name: "Group A".to_string(),
        color: Some("#ff8800".to_string()),
    });
    p.clips.push(Clip {
        name: "Intro".to_string(),
        media_id: Some(MEDIA_ID.to_string()),
        link_id: Some("link-1".to_string()),
        dur: Tick::from_secs(4),
        ..clip("c-video-1", &v1, ClipKind::Video, Tick::ZERO, Tick::ZERO)
    });
    p.clips.push(Clip {
        media_id: Some(MEDIA_ID.to_string()),
        src_in: Tick::from_secs(1),
        speed: 2.0,
        group_id: Some("grp-1".to_string()),
        transition_in: Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: Tick::from_ms(500),
        }),
        opacity: 0.75,
        ..clip(
            "c-video-2",
            &v1,
            ClipKind::Video,
            Tick::from_secs(4),
            Tick::from_secs(3),
        )
    });
    p.clips.push(Clip {
        media_id: Some(MEDIA_ID.to_string()),
        link_id: Some("link-1".to_string()),
        audio: ClipAudio {
            source_track: 1,
            gain_db: -3.0,
            pan: 0.25,
            fade_in: Tick::from_ms(500),
            fade_out: Tick::from_secs(1),
            ..ClipAudio::default()
        },
        ..clip(
            "c-audio-1",
            &a1,
            ClipKind::Audio,
            Tick::ZERO,
            Tick::from_secs(4),
        )
    });
    p.clips.push(Clip {
        media_id: Some("media-audio-1".to_string()),
        audio: ClipAudio {
            gain_db: 2.0,
            ..ClipAudio::default()
        },
        ..clip(
            "c-audio-2",
            &a2,
            ClipKind::Audio,
            Tick::from_secs(1),
            Tick::from_secs(5),
        )
    });
    p.clips.push(Clip {
        solid: Some(Rgba {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        }),
        ..clip(
            "c-solid",
            &v2,
            ClipKind::Solid,
            Tick::ZERO,
            Tick::from_secs(2),
        )
    });
    p.clips.push(Clip {
        text: Some(TextSpec {
            text: "Hello".to_string(),
            ..TextSpec::default()
        }),
        ..clip(
            "c-title",
            &v2,
            ClipKind::Title,
            Tick::from_secs(6),
            Tick::from_secs(2),
        )
    });
    p.markers.push(Marker {
        id: "mk-1".to_string(),
        time: Tick::from_secs(2),
        label: "Beat".to_string(),
        color: Some("#00ff00".to_string()),
    });
    p.work_area = Some(WorkArea {
        start: Tick::ZERO,
        end: Tick::from_secs(5),
    });
    p.modes = EditModes {
        magnetic: true,
        snap: false,
    };
    p.ui_state = UiState {
        zoom_px_per_sec: 80.0,
        scroll_x: 12.5,
        playhead: Tick::from_secs(3),
    };
    p
}

/// Minimal sanitised-clean project with one video and one audio track.
fn bare_project() -> Project {
    Project::new("Bare")
}

fn video_track_id(p: &Project) -> String {
    nth_track(p, TrackKind::Video, 0)
}

fn audio_track_id(p: &Project) -> String {
    nth_track(p, TrackKind::Audio, 0)
}

/// Small deterministic generator (no rand crate). Returns the high bits of a 64-bit LCG.
struct Lcg(u64);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

// ---------------------------------------------------------------------------------------------
// Round trip and defaults
// ---------------------------------------------------------------------------------------------

#[test]
fn populated_project_round_trips_to_an_equal_project() {
    let p = populated_project();
    let back = from_json(&to_json(&p)).expect("load saved project");
    assert_eq!(back, p);
}

#[test]
fn to_json_ends_with_a_newline() {
    assert!(to_json(&bare_project()).ends_with('\n'));
}

#[test]
fn unknown_top_level_fields_are_kept_in_extra_and_written_back() {
    let text = r#"{"schema_version":1,"name":"x","future_thing":{"a":[1,2]}}"#;
    let p = from_json(text).expect("load");
    assert_eq!(
        p.extra.get("future_thing"),
        Some(&serde_json::json!({"a": [1, 2]}))
    );
    let saved = to_json(&p);
    assert!(saved.contains("future_thing"));
    let again = from_json(&saved).expect("reload");
    assert_eq!(again.extra, p.extra);
}

#[test]
fn missing_fields_take_their_defaults() {
    let p = from_json("{}").expect("empty object is a valid project");
    assert_eq!(p.name, "Untitled");
    assert_eq!(p.canvas.width, 1920);
    assert_eq!(p.canvas.height, 1080);
    assert!(p.tracks.iter().any(|t| t.kind == TrackKind::Video));
    assert!(p.tracks.iter().any(|t| t.kind == TrackKind::Audio));
}

#[test]
fn missing_clip_fields_default_to_neutral_values() {
    let text = r#"{
        "tracks":[{"id":"v","kind":"video"}],
        "clips":[{"id":"c","track_id":"v","kind":"video","start":0,"dur":1000}]
    }"#;
    let p = from_json(text).expect("load");
    let c = p.clip("c").expect("clip kept");
    assert_eq!(c.speed, 1.0);
    assert_eq!(c.opacity, 1.0);
    assert!(c.enabled);
    assert_eq!(c.src_in, Tick::ZERO);
}

#[test]
fn missing_schema_version_is_read_as_version_one() {
    let p = from_json(r#"{"name":"x"}"#).expect("load");
    assert_eq!(p.schema_version, 1);
}

#[test]
fn migrate_stamps_a_missing_version_as_one() {
    let mut value = serde_json::json!({});
    migrate(&mut value).expect("migrate");
    assert_eq!(value["schema_version"], 1);
}

#[test]
fn schema_version_99_is_too_new() {
    let err = from_json(r#"{"schema_version":99}"#).expect_err("too new");
    assert_eq!(
        err,
        LoadError::TooNew {
            found: 99,
            supported: 1
        }
    );
}

#[test]
fn schema_version_two_is_too_new() {
    let err = from_json(r#"{"schema_version":2}"#).expect_err("too new");
    assert!(matches!(err, LoadError::TooNew { found: 2, .. }));
}

#[test]
fn non_object_top_level_is_a_shape_error() {
    for text in ["[1,2]", "\"text\"", "42", "null"] {
        let err = from_json(text).expect_err("not an object");
        assert!(matches!(err, LoadError::Shape(_)), "input {text}: {err:?}");
    }
}

#[test]
fn wrong_field_type_is_a_shape_error() {
    let err = from_json(r#"{"clips": 5}"#).expect_err("clips must be an array");
    assert!(matches!(err, LoadError::Shape(_)), "{err:?}");
}

#[test]
fn garbage_and_empty_input_are_json_errors() {
    assert!(matches!(from_json("{not json"), Err(LoadError::Json(_))));
    assert!(matches!(from_json(""), Err(LoadError::Json(_))));
}

#[test]
fn input_over_64_mib_is_too_large() {
    let text = " ".repeat(64 * 1024 * 1024 + 1);
    assert_eq!(from_json(&text), Err(LoadError::TooLarge));
}

// ---------------------------------------------------------------------------------------------
// Sanitising
// ---------------------------------------------------------------------------------------------

#[test]
fn sanitize_clamps_negative_and_huge_ticks() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(clip(
        "neg",
        &v1,
        ClipKind::Video,
        Tick(-5),
        Tick::from_secs(1),
    ));
    p.clips.push(clip(
        "huge",
        &v1,
        ClipKind::Video,
        Tick(i64::MAX),
        Tick(i64::MAX),
    ));
    sanitize(&mut p);
    assert_eq!(p.clip("neg").expect("kept").start, Tick::ZERO);
    let huge = p.clip("huge").expect("kept");
    assert_eq!(huge.start, Tick(MAX_TICK));
    assert_eq!(huge.dur, Tick(MAX_TICK));
}

#[test]
fn sanitize_drops_clips_with_zero_or_negative_length() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips
        .push(clip("zero", &v1, ClipKind::Video, Tick::ZERO, Tick::ZERO));
    p.clips
        .push(clip("neg", &v1, ClipKind::Video, Tick::ZERO, Tick(-1)));
    sanitize(&mut p);
    assert!(p.clip("zero").is_none());
    assert!(p.clip("neg").is_none());
}

#[test]
fn sanitize_clamps_video_speed_and_keeps_freeze_frames() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    for (id, speed) in [
        ("nan", f64::NAN),
        ("neg", -1.0),
        ("fast", 100.0),
        ("freeze", 0.0),
    ] {
        p.clips.push(Clip {
            speed,
            ..clip(id, &v1, ClipKind::Video, Tick::ZERO, Tick::from_secs(1))
        });
    }
    sanitize(&mut p);
    assert_eq!(p.clip("nan").expect("kept").speed, 1.0);
    assert_eq!(p.clip("neg").expect("kept").speed, 1.0);
    assert_eq!(p.clip("fast").expect("kept").speed, 8.0);
    assert_eq!(p.clip("freeze").expect("kept").speed, 0.0);
}

#[test]
fn sanitize_raises_audio_speed_of_zero_to_the_minimum() {
    let mut p = bare_project();
    let a1 = audio_track_id(&p);
    p.clips.push(Clip {
        speed: 0.0,
        ..clip("a", &a1, ClipKind::Audio, Tick::ZERO, Tick::from_secs(1))
    });
    sanitize(&mut p);
    let speed = p.clip("a").expect("kept").speed;
    assert!((speed - 0.1).abs() < 1e-12, "speed {speed}");
}

#[test]
fn sanitize_clamps_gain_and_pan() {
    let mut p = bare_project();
    let a1 = audio_track_id(&p);
    let audio = |gain: f64, pan: f64| ClipAudio {
        gain_db: gain,
        pan,
        ..ClipAudio::default()
    };
    p.clips.push(Clip {
        audio: audio(500.0, 9.0),
        ..clip("hi", &a1, ClipKind::Audio, Tick::ZERO, Tick::from_secs(1))
    });
    p.clips.push(Clip {
        audio: audio(-500.0, -9.0),
        ..clip(
            "lo",
            &a1,
            ClipKind::Audio,
            Tick::from_secs(2),
            Tick::from_secs(1),
        )
    });
    sanitize(&mut p);
    let hi = p.clip("hi").expect("kept");
    assert_eq!(hi.audio.gain_db, 12.0);
    assert_eq!(hi.audio.pan, 1.0);
    let lo = p.clip("lo").expect("kept");
    assert_eq!(lo.audio.gain_db, -60.0);
    assert_eq!(lo.audio.pan, -1.0);
}

#[test]
fn sanitize_clamps_track_volume() {
    let mut p = bare_project();
    p.tracks[0].volume_db = 500.0;
    sanitize(&mut p);
    assert_eq!(p.tracks[0].volume_db, 12.0);
}

#[test]
fn sanitize_keeps_opacity_finite_and_within_zero_to_one() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(Clip {
        opacity: f64::NAN,
        ..clip("nan", &v1, ClipKind::Video, Tick::ZERO, Tick::from_secs(1))
    });
    p.clips.push(Clip {
        opacity: 7.0,
        ..clip(
            "over",
            &v1,
            ClipKind::Video,
            Tick::from_secs(2),
            Tick::from_secs(1),
        )
    });
    sanitize(&mut p);
    let nan = p.clip("nan").expect("kept").opacity;
    assert!(
        nan.is_finite() && (0.0..=1.0).contains(&nan),
        "opacity {nan}"
    );
    assert_eq!(p.clip("over").expect("kept").opacity, 1.0);
}

#[test]
fn sanitize_makes_canvas_dimensions_even_and_within_limits() {
    let mut p = bare_project();
    p.canvas.width = 1921;
    p.canvas.height = 9999;
    sanitize(&mut p);
    assert_eq!(p.canvas.width, 1920);
    assert_eq!(p.canvas.height, 8192);

    let mut q = bare_project();
    q.canvas.width = 0;
    q.canvas.height = 1081;
    sanitize(&mut q);
    assert_eq!(q.canvas.width, 128);
    assert_eq!(q.canvas.height, 1080);
}

#[test]
fn sanitize_replaces_a_zero_frame_rate_with_30() {
    let mut p = bare_project();
    p.canvas.fps = Rate { num: 0, den: 0 };
    sanitize(&mut p);
    assert_eq!(p.canvas.fps, Rate::whole(30));
}

#[test]
fn sanitize_regenerates_duplicate_clip_ids() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(clip(
        "dup",
        &v1,
        ClipKind::Video,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    p.clips.push(clip(
        "dup",
        &v1,
        ClipKind::Video,
        Tick::from_secs(2),
        Tick::from_secs(1),
    ));
    sanitize(&mut p);
    assert_eq!(p.clips.len(), 2);
    assert_eq!(p.clips[0].id, "dup");
    assert!(!p.clips[1].id.is_empty());
    assert_ne!(p.clips[1].id, "dup");
}

#[test]
fn sanitize_drops_clips_on_missing_tracks() {
    let mut p = bare_project();
    p.clips.push(clip(
        "ghost",
        "no-such-track",
        ClipKind::Video,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    sanitize(&mut p);
    assert!(p.clip("ghost").is_none());
}

#[test]
fn sanitize_drops_clips_of_the_wrong_track_kind() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    let a1 = audio_track_id(&p);
    p.clips.push(clip(
        "audio-on-video",
        &v1,
        ClipKind::Audio,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    p.clips.push(clip(
        "video-on-audio",
        &a1,
        ClipKind::Video,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    p.clips.push(clip(
        "title-on-audio",
        &a1,
        ClipKind::Title,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    p.clips.push(clip(
        "good-video",
        &v1,
        ClipKind::Video,
        Tick::from_secs(2),
        Tick::from_secs(1),
    ));
    sanitize(&mut p);
    assert!(p.clip("audio-on-video").is_none());
    assert!(p.clip("video-on-audio").is_none());
    assert!(p.clip("title-on-audio").is_none());
    assert!(p.clip("good-video").is_some());
}

#[test]
fn sanitize_clears_dangling_group_and_link_ids() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.groups.push(Group {
        id: "g-ok".to_string(),
        name: "ok".to_string(),
        color: None,
    });
    p.clips.push(Clip {
        group_id: Some("nope".to_string()),
        link_id: Some("lonely".to_string()),
        ..clip("solo", &v1, ClipKind::Video, Tick::ZERO, Tick::from_secs(1))
    });
    p.clips.push(Clip {
        group_id: Some("g-ok".to_string()),
        link_id: Some("pair".to_string()),
        ..clip(
            "pair-a",
            &v1,
            ClipKind::Video,
            Tick::from_secs(2),
            Tick::from_secs(1),
        )
    });
    p.clips.push(Clip {
        link_id: Some("pair".to_string()),
        ..clip(
            "pair-b",
            &v1,
            ClipKind::Video,
            Tick::from_secs(4),
            Tick::from_secs(1),
        )
    });
    sanitize(&mut p);
    let solo = p.clip("solo").expect("kept");
    assert_eq!(solo.group_id, None);
    assert_eq!(solo.link_id, None);
    let a = p.clip("pair-a").expect("kept");
    assert_eq!(a.group_id.as_deref(), Some("g-ok"));
    assert_eq!(a.link_id.as_deref(), Some("pair"));
    assert_eq!(
        p.clip("pair-b").expect("kept").link_id.as_deref(),
        Some("pair")
    );
}

#[test]
fn sanitize_gives_a_project_with_no_tracks_a_v1_and_an_a1() {
    let mut p = bare_project();
    p.tracks.clear();
    sanitize(&mut p);
    let v = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .expect("video track");
    let a = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .expect("audio track");
    assert_eq!(v.name, "V1");
    assert_eq!(a.name, "A1");
}

#[test]
fn sanitize_is_idempotent_on_a_populated_project() {
    let mut p = populated_project();
    sanitize(&mut p);
    let once = p.clone();
    sanitize(&mut p);
    assert_eq!(p, once);
}

// ---------------------------------------------------------------------------------------------
// Validation (reports only, never changes the project)
// ---------------------------------------------------------------------------------------------

fn codes(p: &Project) -> Vec<String> {
    validate(p).into_iter().map(|i| i.code).collect()
}

#[test]
fn validate_reports_no_issues_for_a_clean_project() {
    assert!(validate(&populated_project()).is_empty());
}

#[test]
fn validate_reports_overlapping_clips_on_one_track() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(clip(
        "a",
        &v1,
        ClipKind::Video,
        Tick::ZERO,
        Tick::from_secs(4),
    ));
    p.clips.push(clip(
        "b",
        &v1,
        ClipKind::Video,
        Tick::from_secs(2),
        Tick::from_secs(4),
    ));
    assert!(codes(&p).iter().any(|c| c == "overlap"));
}

#[test]
fn validate_reports_a_clip_using_missing_media() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(Clip {
        media_id: Some("missing".to_string()),
        ..clip("m", &v1, ClipKind::Video, Tick::ZERO, Tick::from_secs(1))
    });
    assert!(codes(&p).iter().any(|c| c == "dangling_media"));
}

#[test]
fn validate_reports_a_clip_on_a_missing_track() {
    let mut p = bare_project();
    p.clips.push(clip(
        "x",
        "no-such-track",
        ClipKind::Video,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    assert!(codes(&p).iter().any(|c| c == "dangling_track"));
}

#[test]
fn validate_reports_wrong_track_kind_without_removing_the_clip() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(clip(
        "wrong",
        &v1,
        ClipKind::Audio,
        Tick::ZERO,
        Tick::from_secs(1),
    ));
    assert!(codes(&p).iter().any(|c| c == "wrong_track_kind"));
    assert!(p.clip("wrong").is_some());
}

#[test]
fn validate_reports_out_of_range_canvas() {
    let mut p = bare_project();
    p.canvas.width = 1921;
    assert!(codes(&p).iter().any(|c| c == "range"));
}

#[test]
fn validate_reports_a_timeline_longer_than_four_hours() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips.push(clip(
        "far",
        &v1,
        ClipKind::Video,
        Tick(MAX_PROJECT_TICKS),
        Tick::from_secs(1),
    ));
    assert!(codes(&p).iter().any(|c| c == "too_long"));
}

#[test]
fn validate_does_not_change_the_project() {
    let mut p = bare_project();
    let v1 = video_track_id(&p);
    p.clips
        .push(clip("a", &v1, ClipKind::Audio, Tick::ZERO, Tick(-3)));
    p.canvas.width = 9999;
    let before = p.clone();
    let _ = validate(&p);
    assert_eq!(p, before);
}

// ---------------------------------------------------------------------------------------------
// Hostile input: mutations and truncations must never panic
// ---------------------------------------------------------------------------------------------

#[test]
fn byte_mutations_of_a_saved_project_never_panic() {
    let base = to_json(&populated_project()).into_bytes();
    let mut rng = Lcg(0x5EED_0001);
    for _ in 0..200 {
        let mut bytes = base.clone();
        let pos = rng.next_u32() as usize % bytes.len();
        let value = rng.next_u32() as u8;
        match rng.next_u32() % 3 {
            0 => {
                if let Some(b) = bytes.get_mut(pos) {
                    *b = value;
                }
            }
            1 => {
                bytes.remove(pos);
            }
            _ => bytes.insert(pos, value),
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if let Ok(p) = from_json(&text) {
            let _ = validate(&p);
            let _ = to_json(&p);
        }
    }
}

#[test]
fn truncations_of_a_saved_project_never_panic() {
    let base = to_json(&populated_project());
    let mut rng = Lcg(0x5EED_0002);
    for _ in 0..200 {
        let cut = rng.next_u32() as usize % base.len();
        let Some(prefix) = base.get(..cut) else {
            continue;
        };
        if let Ok(p) = from_json(prefix) {
            let _ = validate(&p);
        }
    }
}
