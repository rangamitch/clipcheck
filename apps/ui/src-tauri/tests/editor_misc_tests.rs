//! Public-API tests for other pure pieces of the editor backend: DTO serde shapes in `projects`,
//! `export` and `media`, the snap-kind mapping and snapshot helper in `session_cmds`, and the
//! `Session` commands that `snapshot_of` reads. Nothing here touches the disk, the daemon or
//! media files.

use clipcheck_timeline::cmd::{Command, Session};
use clipcheck_timeline::export::{ExportSettings, Preset};
use clipcheck_timeline::model::{MediaKind, MediaRef, Project, TrackKind};
use clipcheck_timeline::ops_basic::PlaceMode;
use clipcheck_timeline::snap::SnapKind;
use clipcheck_timeline::time::Tick;
use clipcheck_ui_lib::editor::export::{
    EstimateReply, ExportDefaults, ExportManager, JobDto, JobState,
};
use clipcheck_ui_lib::editor::media::{LoudnessReport, MediaStatus};
use clipcheck_ui_lib::editor::projects::ProjectSummary;
use clipcheck_ui_lib::editor::session_cmds::{snap_kind_name, snapshot_of, SnapPointDto};
use clipcheck_ui_lib::editor::{EditorSnapshot, EditorState};
use serde_json::{json, Value};

fn sample_job(state: JobState) -> JobDto {
    JobDto {
        id: "job1".into(),
        project_name: "Demo".into(),
        state,
        percent: 42.5,
        elapsed_s: 12.25,
        eta_s: Some(7.0),
        speed_x: 1.5,
        out_path: "D:\\Exports\\Demo.mp4".into(),
        bytes: 1024,
        paused_reason: None,
        error: None,
        warnings: vec!["audio dropped".into()],
        encoder: Some("h264_nvenc".into()),
        recording_id: None,
    }
}

// ---- ProjectSummary -----------------------------------------------------------------------------

fn sample_summary() -> ProjectSummary {
    ProjectSummary {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        name: "Demo".into(),
        modified_ms: 1_700_000_000_000,
        duration_ms: 65_000,
        clip_count: 3,
        width: 1920,
        height: 1080,
        has_autosave: true,
        broken: false,
    }
}

#[test]
fn project_summary_serialises_camel_case_keys() {
    let v = serde_json::to_value(sample_summary()).expect("ser");
    for key in [
        "id",
        "name",
        "modifiedMs",
        "durationMs",
        "clipCount",
        "width",
        "height",
        "hasAutosave",
        "broken",
    ] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    assert!(v.get("modified_ms").is_none());
}

#[test]
fn project_summary_round_trips() {
    let text = serde_json::to_string(&sample_summary()).expect("ser");
    let back: ProjectSummary = serde_json::from_str(&text).expect("de");
    assert_eq!(back.id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(back.duration_ms, 65_000);
    assert_eq!(back.clip_count, 3);
    assert!(back.has_autosave);
    assert!(!back.broken);
}

#[test]
fn project_summary_reads_broken_row() {
    let v = json!({
        "id": "x", "name": "Bad", "modifiedMs": 0, "durationMs": 0, "clipCount": 0,
        "width": 0, "height": 0, "hasAutosave": false, "broken": true
    });
    let s: ProjectSummary = serde_json::from_value(v).expect("de");
    assert!(s.broken);
}

#[test]
fn project_summary_rejects_snake_case_keys() {
    let v = json!({
        "id": "x", "name": "n", "modified_ms": 0, "duration_ms": 0, "clip_count": 0,
        "width": 0, "height": 0, "has_autosave": false, "broken": false
    });
    assert!(serde_json::from_value::<ProjectSummary>(v).is_err());
}

// ---- JobState -----------------------------------------------------------------------------------

#[test]
fn job_state_serialises_snake_case() {
    let cases = [
        (JobState::Queued, "queued"),
        (JobState::Running, "running"),
        (JobState::Paused, "paused"),
        (JobState::Done, "done"),
        (JobState::Failed, "failed"),
        (JobState::Cancelled, "cancelled"),
    ];
    for (state, text) in cases {
        assert_eq!(
            serde_json::to_string(&state).expect("ser"),
            format!("\"{text}\"")
        );
    }
}

#[test]
fn job_state_round_trips_every_variant() {
    for text in ["queued", "running", "paused", "done", "failed", "cancelled"] {
        let state: JobState = serde_json::from_str(&format!("\"{text}\"")).expect("de");
        let back = serde_json::to_string(&state).expect("ser");
        assert_eq!(back, format!("\"{text}\""));
    }
}

#[test]
fn job_state_rejects_pascal_case() {
    assert!(serde_json::from_str::<JobState>("\"Running\"").is_err());
}

#[test]
fn job_state_rejects_unknown_value() {
    assert!(serde_json::from_str::<JobState>("\"exploded\"").is_err());
}

#[test]
fn job_state_equality_and_copy() {
    let a = JobState::Paused;
    let b = a;
    assert_eq!(a, b);
    assert_ne!(JobState::Done, JobState::Failed);
}

// ---- JobDto -------------------------------------------------------------------------------------

#[test]
fn job_dto_serialises_camel_case_keys() {
    let v = serde_json::to_value(sample_job(JobState::Running)).expect("ser");
    for key in [
        "id",
        "projectName",
        "state",
        "percent",
        "elapsedS",
        "etaS",
        "speedX",
        "outPath",
        "bytes",
        "pausedReason",
        "error",
        "warnings",
        "encoder",
        "recordingId",
    ] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    assert_eq!(v["state"], json!("running"));
}

#[test]
fn job_dto_none_fields_serialise_as_null() {
    let v = serde_json::to_value(sample_job(JobState::Queued)).expect("ser");
    assert_eq!(v["pausedReason"], Value::Null);
    assert_eq!(v["error"], Value::Null);
    assert_eq!(v["recordingId"], Value::Null);
}

#[test]
fn job_dto_eta_none_serialises_as_null() {
    let mut job = sample_job(JobState::Running);
    job.eta_s = None;
    let v = serde_json::to_value(job).expect("ser");
    assert_eq!(v["etaS"], Value::Null);
}

#[test]
fn job_dto_round_trip_keeps_values() {
    let job = sample_job(JobState::Failed);
    let text = serde_json::to_string(&job).expect("ser");
    let back: JobDto = serde_json::from_str(&text).expect("de");
    assert_eq!(back.id, "job1");
    assert_eq!(back.state, JobState::Failed);
    assert_eq!(back.bytes, 1024);
    assert_eq!(back.warnings, vec!["audio dropped".to_string()]);
    assert_eq!(back.encoder.as_deref(), Some("h264_nvenc"));
    assert!((back.percent - 42.5).abs() < f32::EPSILON);
}

#[test]
fn job_dto_list_serialises_as_array() {
    let list = vec![sample_job(JobState::Queued), sample_job(JobState::Done)];
    let v = serde_json::to_value(&list).expect("ser");
    assert_eq!(v.as_array().map(Vec::len), Some(2));
    assert_eq!(v[1]["state"], json!("done"));
}

#[test]
fn job_dto_missing_state_is_rejected() {
    let v = json!({
        "id": "j", "projectName": "p", "percent": 0.0, "elapsedS": 0.0, "etaS": null,
        "speedX": 0.0, "outPath": "", "bytes": 0, "pausedReason": null, "error": null,
        "warnings": [], "encoder": null, "recordingId": null
    });
    assert!(serde_json::from_value::<JobDto>(v).is_err());
}

// ---- ExportManager / EditorState defaults -------------------------------------------------------

#[test]
fn export_manager_starts_with_empty_queue() {
    let m = ExportManager::default();
    let jobs = m.jobs.lock().expect("lock");
    assert!(jobs.is_empty());
}

#[test]
fn editor_state_starts_without_session() {
    let st = EditorState::default();
    let session = st.session.lock().expect("lock");
    assert!(session.is_none());
}

// ---- ExportDefaults / EstimateReply -------------------------------------------------------------

#[test]
fn export_defaults_serialises_camel_case() {
    let d = ExportDefaults {
        dir: "D:\\ClipCheck\\Exports".into(),
        free_bytes: 5_000_000_000,
        presets: vec![Preset {
            id: "balanced_1080p30".into(),
            label: "Balanced".into(),
            settings: ExportSettings::default(),
            fixed_size: false,
            max_mb: None,
            builtin: true,
        }],
        settings: ExportSettings::default(),
        always_ask_location: true,
        encoders: vec!["auto".into(), "software".into()],
    };
    let v = serde_json::to_value(&d).expect("ser");
    for key in [
        "dir",
        "freeBytes",
        "presets",
        "settings",
        "alwaysAskLocation",
        "encoders",
    ] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    assert_eq!(v["presets"][0]["builtin"], json!(true));
    // `Preset` (timeline crate) has no rename, so its own fields stay snake_case.
    assert_eq!(v["presets"][0]["max_mb"], Value::Null);
    assert_eq!(v["presets"][0]["fixed_size"], json!(false));
}

#[test]
fn export_defaults_round_trip_preserves_encoders() {
    let d = ExportDefaults {
        dir: "x".into(),
        free_bytes: 1,
        presets: vec![],
        settings: ExportSettings::default(),
        always_ask_location: false,
        encoders: vec!["auto".into()],
    };
    let text = serde_json::to_string(&d).expect("ser");
    let back: ExportDefaults = serde_json::from_str(&text).expect("de");
    assert_eq!(back.encoders, vec!["auto".to_string()]);
    assert_eq!(back.free_bytes, 1);
}

#[test]
fn estimate_reply_has_camel_case_top_level_and_snake_case_estimate() {
    // `SizeEstimate` (timeline crate) has no rename, so its own fields stay snake_case.
    let r = EstimateReply {
        estimate: clipcheck_timeline::export::SizeEstimate {
            bytes: 10_000_000,
            video_kbps: 8000,
            audio_kbps: 192,
        },
        duration_s: 12.5,
        problems: vec![],
        warning: None,
        out_path: "D:\\Exports\\a.mp4".into(),
    };
    let v = serde_json::to_value(&r).expect("ser");
    assert_eq!(v["durationS"], json!(12.5));
    assert_eq!(v["outPath"], json!("D:\\Exports\\a.mp4"));
    assert_eq!(v["warning"], Value::Null);
    assert_eq!(v["estimate"]["video_kbps"], json!(8000));
    assert_eq!(v["estimate"]["audio_kbps"], json!(192));
}

#[test]
fn estimate_reply_keeps_problems_list() {
    let r = EstimateReply {
        estimate: clipcheck_timeline::export::SizeEstimate {
            bytes: 0,
            video_kbps: 0,
            audio_kbps: 0,
        },
        duration_s: 0.0,
        problems: vec!["no clips".into(), "bad range".into()],
        warning: Some("Likely over the 10 MB limit".into()),
        out_path: String::new(),
    };
    let v = serde_json::to_value(&r).expect("ser");
    assert_eq!(v["problems"].as_array().map(Vec::len), Some(2));
    assert_eq!(v["warning"], json!("Likely over the 10 MB limit"));
}

// ---- MediaStatus / LoudnessReport ---------------------------------------------------------------

#[test]
fn media_status_serialises_camel_case() {
    let v = serde_json::to_value(MediaStatus {
        media_id: "m1".into(),
        missing: true,
    })
    .expect("ser");
    assert_eq!(v, json!({"mediaId": "m1", "missing": true}));
}

#[test]
fn media_status_round_trip() {
    let s: MediaStatus = serde_json::from_str(r#"{"mediaId":"m2","missing":false}"#).expect("de");
    assert_eq!(s.media_id, "m2");
    assert!(!s.missing);
}

#[test]
fn loudness_report_silent_clip_has_null_lufs() {
    let v = serde_json::to_value(LoudnessReport {
        lufs: None,
        peak_db: -90.0,
    })
    .expect("ser");
    assert_eq!(v["lufs"], Value::Null);
    assert_eq!(v["peakDb"], json!(-90.0));
}

#[test]
fn loudness_report_measured_clip_keeps_value() {
    let v = serde_json::to_value(LoudnessReport {
        lufs: Some(-14.2),
        peak_db: -1.0,
    })
    .expect("ser");
    assert_eq!(v["lufs"], json!(-14.2));
}

// ---- snap_kind_name -----------------------------------------------------------------------------

#[test]
fn snap_kind_names_match_dto_contract() {
    assert_eq!(snap_kind_name(SnapKind::Origin), "origin");
    assert_eq!(snap_kind_name(SnapKind::Playhead), "playhead");
    assert_eq!(snap_kind_name(SnapKind::ClipEdge), "clip_edge");
    assert_eq!(snap_kind_name(SnapKind::Marker), "marker");
    assert_eq!(snap_kind_name(SnapKind::WorkArea), "work_area");
}

#[test]
fn snap_kind_names_are_all_distinct() {
    let kinds = [
        SnapKind::Origin,
        SnapKind::Playhead,
        SnapKind::ClipEdge,
        SnapKind::Marker,
        SnapKind::WorkArea,
    ];
    let mut names: Vec<&str> = kinds.iter().map(|k| snap_kind_name(*k)).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 5);
}

#[test]
fn snap_point_dto_serialises_time_and_kind() {
    let dto = SnapPointDto {
        time: clipcheck_timeline::time::Tick(254_016_000_000),
        kind: snap_kind_name(SnapKind::Playhead).into(),
    };
    let v = serde_json::to_value(&dto).expect("ser");
    assert_eq!(v["kind"], json!("playhead"));
    assert!(v.get("time").is_some());
}

// ---- snapshot_of / session behaviour ------------------------------------------------------------

fn media(id: &str, path: &str) -> MediaRef {
    MediaRef {
        id: id.into(),
        path: path.into(),
        name: "clip.mp4".into(),
        ..MediaRef::default()
    }
}

#[test]
fn snapshot_of_fresh_project_is_clean() {
    let session = Session::new(Project::new("Demo"));
    let snap = snapshot_of(&session);
    assert_eq!(snap.project.name, "Demo");
    assert!(!snap.can_undo);
    assert!(!snap.can_redo);
    assert!(!snap.dirty);
    assert_eq!(snap.clipboard_len, 0);
}

#[test]
fn new_project_has_three_video_and_four_audio_tracks() {
    let p = Project::new("Demo");
    assert_eq!(p.tracks.len(), 7);
    let names: Vec<&str> = p.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(&names[..3], ["V1", "V2", "V3"]);
    assert_eq!(&names[3..], ["A1", "A2", "A3", "A4"]);
}

#[test]
fn snapshot_of_serialises_camel_case_top_level() {
    let snap = snapshot_of(&Session::new(Project::new("Demo")));
    let v = serde_json::to_value(&snap).expect("ser");
    for key in [
        "project",
        "canUndo",
        "canRedo",
        "dirty",
        "plan",
        "clipboardLen",
    ] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
}

#[test]
fn snapshot_round_trips_through_json() {
    let snap = snapshot_of(&Session::new(Project::new("Demo")));
    let text = serde_json::to_string(&snap).expect("ser");
    let back: EditorSnapshot = serde_json::from_str(&text).expect("de");
    assert_eq!(back.project.name, "Demo");
    assert_eq!(back.clipboard_len, 0);
}

#[test]
fn adding_media_marks_session_dirty_and_undoable() {
    let mut session = Session::new(Project::new("Demo"));
    let applied = session
        .apply(Command::AddMedia {
            media: media("m1", r"D:\clips\a.mp4"),
        })
        .expect("add");
    assert!(applied.created.iter().any(|id| id == "m1"));
    let snap = snapshot_of(&session);
    assert!(snap.dirty);
    assert!(snap.can_undo);
    assert_eq!(snap.project.media.len(), 1);
}

#[test]
fn undo_after_add_restores_empty_bin_and_enables_redo() {
    let mut session = Session::new(Project::new("Demo"));
    session
        .apply(Command::AddMedia {
            media: media("m1", r"D:\clips\a.mp4"),
        })
        .expect("add");
    session.undo().expect("undo");
    let snap = snapshot_of(&session);
    assert!(snap.project.media.is_empty());
    assert!(!snap.can_undo);
    assert!(snap.can_redo);
}

#[test]
fn redo_reapplies_media() {
    let mut session = Session::new(Project::new("Demo"));
    session
        .apply(Command::AddMedia {
            media: media("m1", r"D:\clips\a.mp4"),
        })
        .expect("add");
    session.undo().expect("undo");
    session.redo().expect("redo");
    assert_eq!(snapshot_of(&session).project.media.len(), 1);
}

#[test]
fn failed_command_leaves_session_clean() {
    let mut session = Session::new(Project::new("Demo"));
    let err = session.apply(Command::RemoveMedia {
        media_id: "nope".into(),
    });
    assert!(err.is_err());
    let snap = snapshot_of(&session);
    assert!(!snap.dirty);
    assert!(!snap.can_undo);
}

#[test]
fn duplicate_media_id_with_other_path_is_rejected_and_unchanged() {
    let mut session = Session::new(Project::new("Demo"));
    session
        .apply(Command::AddMedia {
            media: media("m1", r"D:\clips\a.mp4"),
        })
        .expect("first");
    let second = session.apply(Command::AddMedia {
        media: media("m1", r"D:\clips\b.mp4"),
    });
    assert!(second.is_err());
    assert_eq!(snapshot_of(&session).project.media.len(), 1);
}

#[test]
fn copy_clips_with_no_ids_changes_nothing_but_clipboard() {
    let mut session = Session::new(Project::new("Demo"));
    let applied = session
        .apply(Command::CopyClips { ids: vec![] })
        .expect("copy");
    assert!(applied.created.is_empty());
    assert!(applied.removed.is_empty());
    let snap = snapshot_of(&session);
    assert!(!snap.dirty, "copy must not count as an edit");
    assert!(!snap.can_undo);
    assert_eq!(snap.clipboard_len, 0);
}

#[test]
fn mark_saved_clears_dirty_flag() {
    let mut session = Session::new(Project::new("Demo"));
    session
        .apply(Command::AddMedia {
            media: media("m1", r"D:\clips\a.mp4"),
        })
        .expect("add");
    session.mark_saved();
    assert!(!snapshot_of(&session).dirty);
}

#[test]
fn undo_with_nothing_to_undo_is_an_error() {
    let mut session = Session::new(Project::new("Demo"));
    assert!(session.undo().is_err());
    assert!(session.redo().is_err());
}

#[test]
fn snapshot_plan_serialises_as_object() {
    let snap = snapshot_of(&Session::new(Project::new("Demo")));
    let v = serde_json::to_value(&snap.plan).expect("ser");
    assert!(v.is_object());
}

// ---- snapshot_of after edits (the snapshot must follow the session) -----------------------------

/// A video media entry of 10 s at a synthetic Windows path (the file is never opened).
fn video_media(id: &str) -> MediaRef {
    MediaRef {
        id: id.into(),
        kind: MediaKind::Video,
        path: format!(r"D:\clips\{id}.mp4"),
        name: format!("{id}.mp4"),
        duration: Tick::from_secs(10),
        width: 1280,
        height: 720,
        fps: 30.0,
        ..MediaRef::default()
    }
}

/// Id of the first video track of the session's project ("" when there is none).
fn first_video_track(session: &Session) -> String {
    session
        .project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .map(|t| t.id.clone())
        .unwrap_or_default()
}

/// Adds media "m1" and places it on the first video track at `at_secs`, 2 s long.
fn add_and_place(session: &mut Session, at_secs: i64) {
    session
        .apply(Command::AddMedia {
            media: video_media("m1"),
        })
        .expect("add media");
    let video_track = Some(first_video_track(session));
    session
        .apply(Command::PlaceMedia {
            media_id: "m1".into(),
            at: Tick::from_secs(at_secs),
            src_in: Tick::ZERO,
            src_len: Tick::from_secs(2),
            video_track,
            mode: PlaceMode::Overwrite,
        })
        .expect("place media");
}

#[test]
fn snapshot_of_empty_project_has_no_plan_segments() {
    let snap = snapshot_of(&Session::new(Project::new("Demo")));
    assert!(snap.plan.segments.is_empty());
}

#[test]
fn snapshot_after_placing_media_has_the_clip_and_a_plan_segment() {
    let mut session = Session::new(Project::new("Demo"));
    add_and_place(&mut session, 0);
    let snap = snapshot_of(&session);
    assert_eq!(snap.project.clips.len(), 1);
    assert!(snap.plan.segments.iter().any(|s| !s.video.is_empty()));
}

#[test]
fn snapshot_plan_range_spans_the_placed_clip() {
    let mut session = Session::new(Project::new("Demo"));
    add_and_place(&mut session, 0);
    let snap = snapshot_of(&session);
    assert_eq!(snap.plan.start, Tick::ZERO);
    assert_eq!(snap.plan.end, Tick::from_secs(2));
}

#[test]
fn snapshot_after_undo_of_place_has_no_clip_and_can_redo() {
    let mut session = Session::new(Project::new("Demo"));
    add_and_place(&mut session, 0);
    session.undo().expect("undo place");
    let snap = snapshot_of(&session);
    assert!(snap.project.clips.is_empty());
    assert!(snap.plan.segments.is_empty());
    assert!(snap.can_undo, "the media add is still undoable");
    assert!(snap.can_redo);
}

#[test]
fn snapshot_media_follows_insertion_order() {
    let mut session = Session::new(Project::new("Demo"));
    session
        .apply(Command::AddMedia {
            media: video_media("m1"),
        })
        .expect("add m1");
    session
        .apply(Command::AddMedia {
            media: video_media("m2"),
        })
        .expect("add m2");
    let snap = snapshot_of(&session);
    let ids: Vec<&str> = snap.project.media.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2"]);
}

#[test]
fn snapshot_clipboard_len_counts_copied_clips() {
    let mut session = Session::new(Project::new("Demo"));
    add_and_place(&mut session, 0);
    let clip_id = session
        .project
        .clips
        .first()
        .map(|c| c.id.clone())
        .unwrap_or_default();
    session
        .apply(Command::CopyClips { ids: vec![clip_id] })
        .expect("copy");
    let snap = snapshot_of(&session);
    assert_eq!(snap.clipboard_len, 1);
    assert_eq!(
        snap.project.clips.len(),
        1,
        "copying leaves the timeline alone"
    );
}

#[test]
fn snapshot_is_dirty_again_after_an_edit_that_follows_mark_saved() {
    let mut session = Session::new(Project::new("Demo"));
    session
        .apply(Command::AddMedia {
            media: video_media("m1"),
        })
        .expect("add m1");
    session.mark_saved();
    assert!(!snapshot_of(&session).dirty);
    session
        .apply(Command::AddMedia {
            media: video_media("m2"),
        })
        .expect("add m2");
    assert!(snapshot_of(&session).dirty);
}

#[test]
fn snapshot_plan_canvas_matches_the_project_canvas() {
    let mut project = Project::new("Demo");
    project.canvas.width = 1920;
    project.canvas.height = 1080;
    let snap = snapshot_of(&Session::new(project));
    assert_eq!(snap.plan.canvas.width, 1920);
    assert_eq!(snap.plan.canvas.height, 1080);
    assert_eq!(snap.project.canvas.width, 1920);
}

#[test]
fn undo_then_redo_restores_the_same_plan_segment_count() {
    let mut session = Session::new(Project::new("Demo"));
    add_and_place(&mut session, 0);
    let before = snapshot_of(&session).plan.segments.len();
    session.undo().expect("undo");
    session.redo().expect("redo");
    let after = snapshot_of(&session).plan.segments.len();
    assert_eq!(before, after);
    assert!(after > 0);
}
