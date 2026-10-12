//! Command session, undo/redo, clipboard and command JSON (docs/design/13-video-editor.md, M2).
//! Public API only.

mod common;

use clipcheck_timeline::cmd::{Command, Session, UNDO_DEPTH};
use clipcheck_timeline::error::EditError;
use clipcheck_timeline::model::{
    Canvas, ClipKind, EnvPoint, Project, Rgba, TextSpec, TrackKind, Transition, TransitionKind,
    WorkArea,
};
use clipcheck_timeline::ops_basic::{ClipMove, PlaceMode};
use clipcheck_timeline::ops_props::{ClipPatch, TrackPatch};
use clipcheck_timeline::ops_trim::Edge;
use clipcheck_timeline::time::Tick;
use common::{clip, nth_track, project_with_media, video_media, MEDIA_ID};

fn place_cmd() -> Command {
    Command::PlaceMedia {
        media_id: MEDIA_ID.to_string(),
        at: Tick::ZERO,
        src_in: Tick::ZERO,
        src_len: Tick::ZERO,
        video_track: None,
        mode: PlaceMode::Overwrite,
    }
}

fn solid_cmd(track_id: &str, at: Tick, dur: Tick) -> Command {
    Command::AddSolid {
        track_id: track_id.to_string(),
        at,
        dur,
        color: Rgba::WHITE,
    }
}

fn set_modes(magnetic: bool, snap: bool) -> Command {
    Command::SetModes { magnetic, snap }
}

/// A session on the fixture project with the fixture media placed at zero.
fn placed_session() -> Session {
    let mut s = Session::new(project_with_media());
    s.apply(place_cmd()).expect("place fixture media");
    s
}

fn video_clip_id(p: &Project) -> String {
    p.clips
        .iter()
        .find(|c| c.kind == ClipKind::Video)
        .map(|c| c.id.clone())
        .unwrap_or_default()
}

/// Serialises a command to JSON and back, and checks the result is equal.
fn round_trip(cmd: Command) {
    let text = serde_json::to_string(&cmd).expect("serialise command");
    let back: Command = serde_json::from_str(&text).expect("deserialise command");
    assert_eq!(back, cmd, "json was {text}");
}

// ---------------------------------------------------------------------------------------------
// Session state, undo and redo
// ---------------------------------------------------------------------------------------------

#[test]
fn new_session_starts_clean_with_empty_history_and_clipboard() {
    let s = Session::new(project_with_media());
    assert!(!s.is_dirty());
    assert!(!s.can_undo());
    assert!(!s.can_redo());
    assert!(s.clipboard.is_empty());
}

#[test]
fn place_media_creates_one_video_clip_and_one_audio_clip_per_stream() {
    let mut s = Session::new(project_with_media());
    let applied = s.apply(place_cmd()).expect("place");
    assert_eq!(applied.created.len(), 3);
    assert!(applied.removed.is_empty());
    let videos = s
        .project
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Video)
        .count();
    let audios = s
        .project
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Audio)
        .count();
    assert_eq!((videos, audios), (1, 2));
}

#[test]
fn undo_then_redo_restores_equal_projects_after_place_media() {
    let mut s = Session::new(project_with_media());
    let initial = s.project.clone();
    s.apply(place_cmd()).expect("place");
    let placed = s.project.clone();
    assert_ne!(placed, initial);

    s.undo().expect("undo");
    assert_eq!(s.project, initial);
    s.redo().expect("redo");
    assert_eq!(s.project, placed);
}

#[test]
fn undo_with_no_history_is_nothing_to_undo() {
    let mut s = Session::new(project_with_media());
    assert_eq!(s.undo(), Err(EditError::NothingTo("undo")));
}

#[test]
fn redo_with_no_history_is_nothing_to_redo() {
    let mut s = Session::new(project_with_media());
    assert_eq!(s.redo(), Err(EditError::NothingTo("redo")));
}

#[test]
fn failing_command_leaves_project_and_undo_history_unchanged() {
    let mut s = Session::new(project_with_media());
    let initial = s.project.clone();
    s.apply(place_cmd()).expect("place");
    let before = s.project.clone();
    let result = s.apply(Command::SetReverse {
        clip_id: "no-such-clip".to_string(),
        reverse: true,
    });
    assert!(result.is_err());
    assert_eq!(s.project, before);
    assert!(s.can_undo());

    // Exactly one undo step exists: the place. After it the project is the initial one again.
    s.undo().expect("undo place");
    assert!(!s.can_undo());
    assert_eq!(s.project, initial);
}

#[test]
fn failing_command_on_a_fresh_session_leaves_it_clean() {
    let mut s = Session::new(project_with_media());
    let result = s.apply(Command::SetReverse {
        clip_id: "no-such-clip".to_string(),
        reverse: true,
    });
    assert!(result.is_err());
    assert!(!s.can_undo());
    assert!(!s.is_dirty());
}

#[test]
fn failing_command_keeps_the_redo_stack() {
    let mut s = placed_session();
    s.undo().expect("undo");
    assert!(s.can_redo());
    let result = s.apply(Command::SetReverse {
        clip_id: "no-such-clip".to_string(),
        reverse: true,
    });
    assert!(result.is_err());
    assert!(s.can_redo());
    s.redo().expect("redo still available");
}

#[test]
fn successful_new_command_clears_the_redo_stack() {
    let mut s = placed_session();
    s.undo().expect("undo");
    assert!(s.can_redo());
    // Modes default to magnetic and snap on, so the change must be a real one to count as an edit.
    s.apply(set_modes(false, false)).expect("modes");
    assert!(!s.can_redo());
    assert_eq!(s.redo(), Err(EditError::NothingTo("redo")));
}

#[test]
fn undo_and_redo_walk_through_a_sequence_of_commands() {
    let mut s = Session::new(project_with_media());
    let initial = s.project.clone();
    s.apply(set_modes(true, false)).expect("first");
    let after_first = s.project.clone();
    s.apply(set_modes(false, true)).expect("second");
    let after_second = s.project.clone();

    s.undo().expect("undo second");
    assert_eq!(s.project, after_first);
    s.undo().expect("undo first");
    assert_eq!(s.project, initial);
    s.redo().expect("redo first");
    s.redo().expect("redo second");
    assert_eq!(s.project, after_second);
}

#[test]
fn undo_depth_is_capped_at_the_documented_limit() {
    assert_eq!(UNDO_DEPTH, 200);
    let mut s = Session::new(project_with_media());
    for i in 0..205 {
        s.apply(set_modes(i % 2 == 0, false)).expect("toggle");
    }
    for step in 0..200 {
        assert!(s.undo().is_ok(), "undo {step} should succeed");
    }
    assert_eq!(s.undo(), Err(EditError::NothingTo("undo")));
}

#[test]
fn dirty_flag_follows_edits_undo_redo_and_mark_saved() {
    let mut s = Session::new(project_with_media());
    assert!(!s.is_dirty());
    // Defaults are magnetic and snap on; turning magnetic off is a real edit.
    s.apply(set_modes(false, true)).expect("edit");
    assert!(s.is_dirty());
    s.mark_saved();
    assert!(!s.is_dirty());
    s.undo().expect("undo");
    assert!(s.is_dirty());
    s.mark_saved();
    s.redo().expect("redo");
    assert!(s.is_dirty());
}

// Doc (Session::apply): a command that leaves the project equal to its previous state pushes no
// undo entry, clears no redo stack and does not mark the session dirty. Adding a media whose path
// is already in the bin is such a command.

#[test]
fn no_op_command_pushes_no_undo_entry_and_reports_nothing() {
    let mut s = Session::new(project_with_media());
    let applied = s
        .apply(Command::AddMedia {
            media: video_media("dup-id"),
        })
        .expect("adding an existing path succeeds");
    assert!(applied.created.is_empty());
    assert!(applied.removed.is_empty());
    assert!(!s.can_undo());
    assert!(!s.is_dirty());
}

#[test]
fn no_op_command_does_not_mark_a_saved_session_dirty() {
    let mut s = placed_session();
    s.mark_saved();
    s.apply(Command::AddMedia {
        media: video_media("dup-id"),
    })
    .expect("no-op");
    assert!(!s.is_dirty());
}

#[test]
fn no_op_command_keeps_the_redo_stack() {
    let mut s = placed_session();
    s.undo().expect("undo place");
    assert!(s.can_redo());
    s.apply(Command::AddMedia {
        media: video_media("dup-id"),
    })
    .expect("no-op");
    assert!(s.can_redo(), "a no-op must not clear redo");
    s.redo().expect("redo place");
    assert!(!s.can_redo());
}

#[test]
fn no_op_command_adds_no_step_to_undo_history() {
    let mut s = placed_session();
    s.apply(Command::AddMedia {
        media: video_media("dup-id"),
    })
    .expect("no-op");
    s.undo().expect("undo the place only");
    assert_eq!(s.undo(), Err(EditError::NothingTo("undo")));
}

// ---------------------------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------------------------

#[test]
fn copy_clips_fills_the_clipboard_and_is_not_undoable() {
    let mut s = Session::new(project_with_media());
    let initial = s.project.clone();
    s.apply(place_cmd()).expect("place");
    let vid = video_clip_id(&s.project);

    let applied = s
        .apply(Command::CopyClips {
            ids: vec![vid.clone()],
        })
        .expect("copy");
    assert!(applied.created.is_empty());
    assert!(applied.removed.is_empty());
    assert!(s.clipboard.iter().any(|c| c.id == vid));

    // Copy pushed no snapshot: one undo still returns to the state before the place.
    s.undo().expect("undo place");
    assert!(!s.can_undo());
    assert_eq!(s.project, initial);
}

#[test]
fn copy_clips_does_not_mark_the_session_dirty() {
    let mut s = placed_session();
    s.mark_saved();
    s.apply(Command::CopyClips { ids: vec![] }).expect("copy");
    assert!(!s.is_dirty());
}

#[test]
fn paste_with_empty_clips_uses_the_clipboard() {
    let mut s = placed_session();
    let vid = video_clip_id(&s.project);
    s.apply(Command::CopyClips { ids: vec![vid] })
        .expect("copy");
    let before = s.project.clips.len();

    let applied = s
        .apply(Command::PasteClips {
            clips: vec![],
            at: Tick::from_secs(20),
            track_id: None,
            mode: PlaceMode::Insert,
        })
        .expect("paste from clipboard");
    assert!(!applied.created.is_empty());
    assert_eq!(s.project.clips.len(), before + applied.created.len());
    for id in &applied.created {
        assert!(
            s.project.clip(id).is_some(),
            "created id {id} is in the project"
        );
    }
}

#[test]
fn paste_with_nothing_copied_is_an_error() {
    let mut s = Session::new(project_with_media());
    let result = s.apply(Command::PasteClips {
        clips: vec![],
        at: Tick::ZERO,
        track_id: None,
        mode: PlaceMode::Overwrite,
    });
    assert!(result.is_err());
    assert!(!s.can_undo());
}

#[test]
fn paste_with_explicit_clips_creates_them_regardless_of_clipboard() {
    let mut s = Session::new(project_with_media());
    let v1 = nth_track(&s.project, TrackKind::Video, 0);
    let explicit = clip("src", &v1, ClipKind::Video, Tick::ZERO, Tick::from_secs(1));
    let applied = s
        .apply(Command::PasteClips {
            clips: vec![explicit],
            at: Tick::from_secs(5),
            track_id: Some(v1),
            mode: PlaceMode::Overwrite,
        })
        .expect("paste explicit clip");
    assert_eq!(applied.created.len(), 1);
    assert!(s.clipboard.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Applied: created and removed ids
// ---------------------------------------------------------------------------------------------

#[test]
fn add_solid_reports_the_new_clip_as_created() {
    let mut s = Session::new(project_with_media());
    let v1 = nth_track(&s.project, TrackKind::Video, 0);
    let applied = s
        .apply(solid_cmd(&v1, Tick::ZERO, Tick::from_secs(10)))
        .expect("add solid");
    assert_eq!(applied.created.len(), 1);
    let id = &applied.created[0];
    assert_eq!(s.project.clip(id).map(|c| c.kind), Some(ClipKind::Solid));
}

#[test]
fn split_reports_one_created_id_per_new_right_half() {
    let mut s = Session::new(project_with_media());
    let v1 = nth_track(&s.project, TrackKind::Video, 0);
    let applied = s
        .apply(solid_cmd(&v1, Tick::ZERO, Tick::from_secs(10)))
        .expect("add solid");
    let solid_id = applied.created[0].clone();

    let split = s
        .apply(Command::SplitAt {
            time: Tick::from_secs(5),
            ids: vec![solid_id.clone()],
        })
        .expect("split");
    assert_eq!(split.created.len(), 1);
    assert!(split.removed.is_empty());
    assert!(s.project.clip(&split.created[0]).is_some());
    // The left half keeps the original id and now ends at five seconds.
    let left = s.project.clip(&solid_id).expect("left half keeps its id");
    assert_eq!(left.dur, Tick::from_secs(5));
}

#[test]
fn split_of_placed_media_reports_every_split_piece() {
    let mut s = placed_session();
    let split = s
        .apply(Command::SplitAt {
            time: Tick::from_secs(5),
            ids: vec![],
        })
        .expect("split all");
    // One video clip and two audio clips span five seconds, so three right halves appear.
    assert_eq!(split.created.len(), 3);
    assert_eq!(s.project.clips.len(), 6);
}

#[test]
fn delete_reports_removed_ids() {
    let mut s = Session::new(project_with_media());
    let v1 = nth_track(&s.project, TrackKind::Video, 0);
    let applied = s
        .apply(solid_cmd(&v1, Tick::ZERO, Tick::from_secs(2)))
        .expect("add solid");
    let solid_id = applied.created[0].clone();

    let deleted = s
        .apply(Command::DeleteClips {
            ids: vec![solid_id.clone()],
            ripple: false,
        })
        .expect("delete");
    assert_eq!(deleted.removed, vec![solid_id.clone()]);
    assert!(deleted.created.is_empty());
    assert!(s.project.clip(&solid_id).is_none());
}

// ---------------------------------------------------------------------------------------------
// Command JSON
// ---------------------------------------------------------------------------------------------

#[test]
fn split_at_deserialises_from_its_documented_json_form() {
    let cmd: Command =
        serde_json::from_str(r#"{"type":"split_at","time":5,"ids":[]}"#).expect("parse");
    // Tick is serialised as raw ticks, so the bare number 5 is five ticks.
    assert_eq!(
        cmd,
        Command::SplitAt {
            time: Tick(5),
            ids: vec![]
        }
    );
}

#[test]
fn unknown_command_type_is_rejected() {
    assert!(serde_json::from_str::<Command>(r#"{"type":"explode"}"#).is_err());
}

#[test]
fn edit_group_null_color_means_clear() {
    let cmd: Command = serde_json::from_str(r#"{"type":"edit_group","group_id":"g","color":null}"#)
        .expect("parse");
    match cmd {
        Command::EditGroup {
            group_id,
            name,
            color,
        } => {
            assert_eq!(group_id, "g");
            assert_eq!(name, None);
            assert_eq!(color, Some(None));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn edit_group_absent_color_means_leave_alone() {
    let cmd: Command =
        serde_json::from_str(r#"{"type":"edit_group","group_id":"g"}"#).expect("parse");
    match &cmd {
        Command::EditGroup { color, .. } => assert_eq!(*color, None),
        other => panic!("unexpected command {other:?}"),
    }
    let text = serde_json::to_string(&cmd).expect("serialise");
    assert!(
        !text.contains("color"),
        "absent colour must not be written: {text}"
    );
}

#[test]
fn edit_group_string_color_means_set() {
    let cmd: Command =
        serde_json::from_str(r##"{"type":"edit_group","group_id":"g","color":"#ff0000"}"##)
            .expect("parse");
    match cmd {
        Command::EditGroup { color, .. } => {
            assert_eq!(color, Some(Some("#ff0000".to_string())));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn patch_clips_null_color_clears_and_absent_color_leaves_alone() {
    let cleared: Command =
        serde_json::from_str(r#"{"type":"patch_clips","ids":["a"],"patch":{"color":null}}"#)
            .expect("parse cleared");
    match cleared {
        Command::PatchClips { patch, .. } => assert_eq!(patch.color, Some(None)),
        other => panic!("unexpected command {other:?}"),
    }

    let untouched: Command =
        serde_json::from_str(r#"{"type":"patch_clips","ids":["a"],"patch":{}}"#)
            .expect("parse untouched");
    match untouched {
        Command::PatchClips { patch, .. } => {
            assert_eq!(patch.color, None);
            assert_eq!(patch, ClipPatch::default());
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn edit_marker_null_color_clears_the_marker_colour() {
    let cmd: Command =
        serde_json::from_str(r#"{"type":"edit_marker","marker_id":"m","color":null}"#)
            .expect("parse");
    match cmd {
        Command::EditMarker {
            marker_id,
            time,
            label,
            color,
        } => {
            assert_eq!(marker_id, "m");
            assert_eq!(time, None);
            assert_eq!(label, None);
            assert_eq!(color, Some(None));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn patch_clips_string_color_means_set() {
    let cmd: Command =
        serde_json::from_str(r##"{"type":"patch_clips","ids":["a"],"patch":{"color":"#00ff00"}}"##)
            .expect("parse");
    match cmd {
        Command::PatchClips { patch, .. } => {
            assert_eq!(patch.color, Some(Some("#00ff00".to_string())));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn edit_marker_absent_color_leaves_alone_and_is_not_written() {
    let cmd: Command =
        serde_json::from_str(r#"{"type":"edit_marker","marker_id":"m","label":"x"}"#)
            .expect("parse");
    match &cmd {
        Command::EditMarker { color, .. } => assert_eq!(*color, None),
        other => panic!("unexpected command {other:?}"),
    }
    let text = serde_json::to_string(&cmd).expect("serialise");
    assert!(
        !text.contains("color"),
        "absent colour must not be written: {text}"
    );
}

#[test]
fn edit_group_double_option_survives_a_round_trip() {
    round_trip(Command::EditGroup {
        group_id: "g".to_string(),
        name: None,
        color: Some(None),
    });
    round_trip(Command::EditGroup {
        group_id: "g".to_string(),
        name: Some("Name".to_string()),
        color: None,
    });
}

#[test]
fn place_mode_serialises_in_snake_case() {
    let text = serde_json::to_string(&PlaceMode::Insert).expect("serialise");
    assert_eq!(text, "\"insert\"");
}

#[test]
fn clip_editing_commands_round_trip_through_json() {
    round_trip(Command::MoveClips {
        moves: vec![ClipMove {
            clip_id: "a".to_string(),
            new_start: Tick::from_secs(2),
            new_track: Some("t".to_string()),
        }],
    });
    round_trip(Command::DeleteClips {
        ids: vec!["a".to_string(), "b".to_string()],
        ripple: true,
    });
    round_trip(Command::SplitAt {
        time: Tick::from_secs(5),
        ids: vec!["a".to_string()],
    });
    round_trip(Command::TrimEdge {
        clip_id: "a".to_string(),
        edge: Edge::End,
        new_time: Tick::from_secs(9),
        ripple: false,
    });
    round_trip(Command::SetSpeed {
        clip_id: "a".to_string(),
        speed: 2.5,
        ripple: true,
    });
    round_trip(Command::SetEnvelope {
        clip_id: "a".to_string(),
        points: vec![EnvPoint {
            t: Tick::from_ms(250),
            db: -3.0,
        }],
    });
    round_trip(Command::PatchClips {
        ids: vec!["a".to_string()],
        patch: ClipPatch {
            color: Some(None),
            opacity: Some(0.5),
            ..ClipPatch::default()
        },
    });
    round_trip(Command::AddSolid {
        track_id: "t".to_string(),
        at: Tick::ZERO,
        dur: Tick::from_secs(1),
        color: Rgba::WHITE,
    });
    round_trip(Command::AddTitle {
        track_id: "t".to_string(),
        at: Tick::ZERO,
        dur: Tick::from_secs(1),
        text: TextSpec {
            text: "Title".to_string(),
            ..TextSpec::default()
        },
    });
    round_trip(Command::PasteClips {
        clips: vec![],
        at: Tick::from_secs(3),
        track_id: None,
        mode: PlaceMode::Overwrite,
    });
    round_trip(Command::CopyClips {
        ids: vec!["a".to_string()],
    });
}

#[test]
fn track_project_and_marker_commands_round_trip_through_json() {
    round_trip(set_modes(true, false));
    round_trip(Command::SetCanvas {
        canvas: Canvas::default(),
    });
    round_trip(Command::RenameProject {
        name: "Renamed".to_string(),
    });
    round_trip(Command::SetWorkArea {
        area: Some(WorkArea {
            start: Tick::ZERO,
            end: Tick::from_secs(5),
        }),
    });
    round_trip(Command::SetWorkArea { area: None });
    round_trip(Command::AddTrack {
        kind: TrackKind::Audio,
    });
    round_trip(Command::MoveTrack {
        track_id: "t".to_string(),
        delta: -1,
    });
    round_trip(Command::PatchTrack {
        track_id: "t".to_string(),
        patch: TrackPatch {
            volume_db: Some(-3.0),
            ..TrackPatch::default()
        },
    });
    round_trip(Command::AddMarker {
        time: Tick::from_secs(1),
        label: "Mark".to_string(),
        color: None,
    });
    round_trip(Command::EditMarker {
        marker_id: "m".to_string(),
        time: Some(Tick::from_secs(2)),
        label: None,
        color: Some(Some("#00ff00".to_string())),
    });
    round_trip(Command::RemoveMarker {
        marker_id: "m".to_string(),
    });
}

#[test]
fn media_and_range_commands_round_trip_through_json() {
    round_trip(Command::AddMedia {
        media: video_media("m-json"),
    });
    round_trip(Command::RemoveMedia {
        media_id: "m-json".to_string(),
    });
    round_trip(Command::RelinkMedia {
        media_id: "m-json".to_string(),
        media: video_media("m-json"),
    });
    round_trip(Command::PlaceMedia {
        media_id: "m-json".to_string(),
        at: Tick::from_secs(1),
        src_in: Tick::ZERO,
        src_len: Tick::from_secs(3),
        video_track: Some("v".to_string()),
        mode: PlaceMode::Insert,
    });
    round_trip(Command::RemoveRange {
        start: Tick::ZERO,
        end: Tick::from_secs(2),
        track_ids: vec!["v".to_string()],
        ripple: true,
    });
    round_trip(Command::CloseGap {
        track_id: "v".to_string(),
        at: Tick::from_secs(2),
        all_tracks: true,
    });
    round_trip(Command::CloseAllGaps {
        track_id: "v".to_string(),
    });
    round_trip(Command::FreezeFrame {
        clip_id: "a".to_string(),
        at: Tick::from_secs(1),
        dur: Tick::from_secs(2),
    });
    round_trip(Command::RollEdit {
        left_id: "a".to_string(),
        right_id: "b".to_string(),
        new_cut: Tick::from_secs(4),
    });
    round_trip(Command::Slip {
        clip_id: "a".to_string(),
        delta: Tick::from_ms(-500),
    });
    round_trip(Command::OffsetGain {
        ids: vec!["a".to_string()],
        delta_db: -2.0,
    });
    round_trip(Command::AdjustGroupGain {
        group_id: "g".to_string(),
        delta_db: 1.5,
    });
}

#[test]
fn moving_a_neighbour_away_clears_its_transition_and_undo_restores_it() {
    use clipcheck_timeline::model::{Transition, TransitionKind};
    let mut p = Project::new("transitions");
    let track = nth_track(&p, TrackKind::Video, 0);
    p.clips.push(clip(
        "a",
        &track,
        ClipKind::Video,
        Tick::ZERO,
        Tick::from_secs(5),
    ));
    p.clips.push(clip(
        "b",
        &track,
        ClipKind::Video,
        Tick::from_secs(5),
        Tick::from_secs(5),
    ));
    let mut s = Session::new(p);
    s.apply(Command::PatchClips {
        ids: vec!["b".to_string()],
        patch: ClipPatch {
            transition_in: Some(Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: Tick::from_ms(500),
            })),
            ..ClipPatch::default()
        },
    })
    .expect("transition on b (touching a)");
    assert!(s.project.clip("b").expect("b").transition_in.is_some());

    s.apply(Command::MoveClips {
        moves: vec![ClipMove {
            clip_id: "b".to_string(),
            new_start: Tick::from_secs(20),
            new_track: None,
        }],
    })
    .expect("move b away from a");
    assert!(
        s.project.clip("b").expect("b").transition_in.is_none(),
        "a transition with no neighbour must be cleared"
    );

    s.undo().expect("undo the move");
    assert_eq!(s.project.clip("b").expect("b").start, Tick::from_secs(5));
    assert!(
        s.project.clip("b").expect("b").transition_in.is_some(),
        "undo restores the transition"
    );
}

#[test]
fn trim_shortening_a_clip_clamps_its_transition_and_drops_it_below_the_minimum() {
    let mut p = project_with_media();
    let vt = nth_track(&p, TrackKind::Video, 0);
    let prev = clip("prev", &vt, ClipKind::Video, Tick::ZERO, Tick::from_secs(1));
    let mut v = clip(
        "v",
        &vt,
        ClipKind::Video,
        Tick::from_secs(1),
        Tick::from_ms(600),
    );
    v.media_id = Some(MEDIA_ID.to_string());
    v.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: Tick::from_ms(500),
    });
    p.clips.push(prev);
    p.clips.push(v);
    let mut s = Session::new(p);
    s.apply(Command::TrimEdge {
        clip_id: "v".to_string(),
        edge: Edge::End,
        new_time: Tick::from_ms(1400),
        ripple: false,
    })
    .expect("trim to 0.4 s");
    assert_eq!(
        s.project
            .clip("v")
            .expect("v")
            .transition_in
            .map(|t| t.duration),
        Some(Tick::from_ms(400)),
        "transition clamped to the 0.4 s clip"
    );
    s.apply(Command::TrimEdge {
        clip_id: "v".to_string(),
        edge: Edge::End,
        new_time: Tick::from_ms(1050),
        ripple: false,
    })
    .expect("trim to 0.05 s");
    assert_eq!(
        s.project.clip("v").expect("v").transition_in,
        None,
        "0.05 s is under the 0.1 s minimum, so the transition is dropped"
    );
}
