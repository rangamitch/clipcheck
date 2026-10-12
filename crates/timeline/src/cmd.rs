//! Command layer (M2): every edit is a serialisable [`Command`] applied through
//! [`Session::apply`], with snapshot-based undo/redo. Owner: worker `tl-session`.
//! `Command` is the final CONTRACT; `Session` applies it through the `ops_*` modules.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{EditError, EditResult};
use crate::model::WorkArea;
use crate::model::{Canvas, Clip, EnvPoint, MediaRef, Project, Rgba, TextSpec, TrackKind};
use crate::ops_basic::{self, ClipMove, PlaceMode};
use crate::ops_group;
use crate::ops_props::{self, ClipPatch, TrackPatch};
use crate::ops_range;
use crate::ops_trim::{self, Edge};
use crate::time::Tick;

/// Maximum undo depth.
pub const UNDO_DEPTH: usize = 200;

/// Every edit the UI can request. JSON form: `{"type": "split_at", ...fields}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Add media to the bin.
    AddMedia { media: MediaRef },
    /// Remove media from the bin.
    RemoveMedia { media_id: String },
    /// Relink media.
    RelinkMedia { media_id: String, media: MediaRef },
    /// Place media on the timeline.
    PlaceMedia {
        media_id: String,
        at: Tick,
        src_in: Tick,
        src_len: Tick,
        video_track: Option<String>,
        mode: PlaceMode,
    },
    /// Move clips.
    MoveClips { moves: Vec<ClipMove> },
    /// Delete clips.
    DeleteClips { ids: Vec<String>, ripple: bool },
    /// Split at a time.
    SplitAt { time: Tick, ids: Vec<String> },
    /// Duplicate clips.
    DuplicateClips { ids: Vec<String>, offset: Tick },
    /// Paste clips (the session clipboard is used when `clips` is empty).
    PasteClips {
        clips: Vec<Clip>,
        at: Tick,
        track_id: Option<String>,
        mode: PlaceMode,
    },
    /// Copy clips to the session clipboard (not undoable, not a project change).
    CopyClips { ids: Vec<String> },
    /// Lift or extract a range.
    RemoveRange {
        start: Tick,
        end: Tick,
        track_ids: Vec<String>,
        ripple: bool,
    },
    /// Close the gap at `at` on `track_id`. `all_tracks` (default true) ripples every unlocked
    /// track so linked A/V stay in sync; `false` shifts only `track_id`.
    CloseGap {
        track_id: String,
        at: Tick,
        #[serde(default = "default_true")]
        all_tracks: bool,
    },
    /// Close all gaps on a track.
    CloseAllGaps { track_id: String },
    /// Freeze frame.
    FreezeFrame {
        clip_id: String,
        at: Tick,
        dur: Tick,
    },
    /// Trim an edge.
    TrimEdge {
        clip_id: String,
        edge: Edge,
        new_time: Tick,
        ripple: bool,
    },
    /// Roll edit.
    RollEdit {
        left_id: String,
        right_id: String,
        new_cut: Tick,
    },
    /// Slip.
    Slip { clip_id: String, delta: Tick },
    /// Slide.
    Slide { clip_id: String, delta: Tick },
    /// Set speed.
    SetSpeed {
        clip_id: String,
        speed: f64,
        ripple: bool,
    },
    /// Set reverse.
    SetReverse { clip_id: String, reverse: bool },
    /// Join through edit.
    JoinThroughEdit { left_id: String, right_id: String },
    /// Set work area.
    SetWorkArea { area: Option<WorkArea> },
    /// Group clips.
    GroupClips { ids: Vec<String> },
    /// Ungroup.
    Ungroup { ids: Vec<String> },
    /// Link clips.
    LinkClips { ids: Vec<String> },
    /// Unlink clips.
    UnlinkClips { ids: Vec<String> },
    /// Detach audio of a video clip.
    DetachAudio { clip_id: String },
    /// Edit a group.
    EditGroup {
        group_id: String,
        name: Option<String>,
        #[serde(
            default,
            with = "crate::serde_util::double_option",
            skip_serializing_if = "Option::is_none"
        )]
        color: Option<Option<String>>,
    },
    /// Group gain offset.
    AdjustGroupGain { group_id: String, delta_db: f64 },
    /// Patch clips.
    PatchClips { ids: Vec<String>, patch: ClipPatch },
    /// Replace an envelope.
    SetEnvelope {
        clip_id: String,
        points: Vec<EnvPoint>,
    },
    /// Offset gain.
    OffsetGain { ids: Vec<String>, delta_db: f64 },
    /// Add a track.
    AddTrack { kind: TrackKind },
    /// Remove a track.
    RemoveTrack { track_id: String },
    /// Patch a track.
    PatchTrack { track_id: String, patch: TrackPatch },
    /// Move a track.
    MoveTrack { track_id: String, delta: i32 },
    /// Set the canvas.
    SetCanvas { canvas: Canvas },
    /// Rename the project.
    RenameProject { name: String },
    /// Set editing modes.
    SetModes { magnetic: bool, snap: bool },
    /// Add a marker.
    AddMarker {
        time: Tick,
        label: String,
        color: Option<String>,
    },
    /// Edit a marker.
    EditMarker {
        marker_id: String,
        time: Option<Tick>,
        label: Option<String>,
        #[serde(
            default,
            with = "crate::serde_util::double_option",
            skip_serializing_if = "Option::is_none"
        )]
        color: Option<Option<String>>,
    },
    /// Remove a marker.
    RemoveMarker { marker_id: String },
    /// Add a title clip.
    AddTitle {
        track_id: String,
        at: Tick,
        dur: Tick,
        text: TextSpec,
    },
    /// Add a solid clip.
    AddSolid {
        track_id: String,
        at: Tick,
        dur: Tick,
        color: Rgba,
    },
}

/// Serde default for boolean command flags that default to `true`.
fn default_true() -> bool {
    true
}

/// What a successful command produced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    /// Ids created by the command (new clips, tracks, markers, groups, media).
    pub created: Vec<String>,
    /// Ids removed.
    pub removed: Vec<String>,
}

/// An editing session: the project plus undo/redo stacks and a clipboard.
#[derive(Debug, Clone)]
pub struct Session {
    /// The live project.
    pub project: Project,
    undo: Vec<Project>,
    redo: Vec<Project>,
    /// Clipboard filled by `CopyClips`.
    pub clipboard: Vec<Clip>,
    /// Number of commands applied since the last [`Session::mark_saved`] (0 = clean).
    dirty: bool,
}

impl Session {
    /// Starts a session on a project.
    pub fn new(project: Project) -> Session {
        Session {
            project,
            undo: Vec::new(),
            redo: Vec::new(),
            clipboard: Vec::new(),
            dirty: false,
        }
    }

    /// Applies a command. On `Err` the project and the stacks are unchanged. On success pushes a
    /// snapshot (depth [`UNDO_DEPTH`], oldest dropped), clears redo, sets `modified_ms` (via the
    /// caller; this crate has no clock), marks dirty. A command that leaves the project equal to
    /// its previous state pushes nothing and is not dirty. `CopyClips` changes only the clipboard.
    pub fn apply(&mut self, cmd: Command) -> EditResult<Applied> {
        if let Command::CopyClips { ids } = &cmd {
            let expanded = ops_group::expand_selection(&self.project, ids);
            let mut clips: Vec<Clip> = self
                .project
                .clips
                .iter()
                .filter(|c| expanded.contains(&c.id))
                .cloned()
                .collect();
            clips.sort_by_key(|c| c.start);
            self.clipboard = clips;
            return Ok(Applied::default());
        }
        let before = self.project.clone();
        let before_ids = ids_of(&before);
        if let Err(e) = run(&mut self.project, cmd, &self.clipboard) {
            self.project = before;
            return Err(e);
        }
        // Part of the same command: a neighbour that moved or went away takes its transition with it.
        ops_props::normalise_transitions(&mut self.project);
        if self.project == before {
            // A no-op (for example adding media that is already present): no undo entry, not dirty.
            return Ok(Applied::default());
        }
        let after_ids = ids_of(&self.project);
        let applied = Applied {
            created: after_ids.difference(&before_ids).cloned().collect(),
            removed: before_ids.difference(&after_ids).cloned().collect(),
        };
        push_bounded(&mut self.undo, before);
        self.redo.clear();
        self.dirty = true;
        Ok(applied)
    }

    /// Undoes the last command.
    pub fn undo(&mut self) -> EditResult<()> {
        let prev = self.undo.pop().ok_or(EditError::NothingTo("undo"))?;
        let current = std::mem::replace(&mut self.project, prev);
        push_bounded(&mut self.redo, current);
        self.dirty = true;
        Ok(())
    }

    /// Redoes the last undone command.
    pub fn redo(&mut self) -> EditResult<()> {
        let next = self.redo.pop().ok_or(EditError::NothingTo("redo"))?;
        let current = std::mem::replace(&mut self.project, next);
        push_bounded(&mut self.undo, current);
        self.dirty = true;
        Ok(())
    }

    /// Whether undo is possible.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether redo is possible.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Marks the current state as saved.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }
}

/// Pushes onto an undo or redo stack, dropping the oldest entry beyond [`UNDO_DEPTH`].
fn push_bounded(stack: &mut Vec<Project>, item: Project) {
    stack.push(item);
    if stack.len() > UNDO_DEPTH {
        stack.remove(0);
    }
}

/// Every id of a clip, track, marker, group or media entry in the project.
fn ids_of(p: &Project) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    out.extend(p.clips.iter().map(|c| c.id.clone()));
    out.extend(p.tracks.iter().map(|t| t.id.clone()));
    out.extend(p.markers.iter().map(|m| m.id.clone()));
    out.extend(p.groups.iter().map(|g| g.id.clone()));
    out.extend(p.media.iter().map(|m| m.id.clone()));
    out
}

/// Runs one non-clipboard command against the project. Ops are atomic; the caller restores the
/// snapshot on error. `clipboard` is used by `PasteClips` when its own clip list is empty.
fn run(p: &mut Project, cmd: Command, clipboard: &[Clip]) -> EditResult<()> {
    match cmd {
        Command::AddMedia { media } => ops_basic::add_media(p, media).map(|_| ()),
        Command::RemoveMedia { media_id } => ops_basic::remove_media(p, &media_id),
        Command::RelinkMedia { media_id, media } => ops_basic::relink_media(p, &media_id, media),
        Command::PlaceMedia {
            media_id,
            at,
            src_in,
            src_len,
            video_track,
            mode,
        } => ops_basic::place_media(
            p,
            &media_id,
            at,
            src_in,
            src_len,
            video_track.as_deref(),
            mode,
        )
        .map(|_| ()),
        Command::MoveClips { moves } => ops_basic::move_clips(p, &moves),
        Command::DeleteClips { ids, ripple } => {
            ops_basic::delete_clips(p, &ids, ripple).map(|_| ())
        }
        Command::SplitAt { time, ids } => ops_basic::split_at(p, time, &ids).map(|_| ()),
        Command::DuplicateClips { ids, offset } => {
            ops_basic::duplicate_clips(p, &ids, offset).map(|_| ())
        }
        Command::PasteClips {
            clips,
            at,
            track_id,
            mode,
        } => {
            let src: &[Clip] = if clips.is_empty() { clipboard } else { &clips };
            if src.is_empty() {
                return Err(EditError::Invalid("clipboard is empty".to_string()));
            }
            ops_range::paste_clips(p, src, at, track_id.as_deref(), mode).map(|_| ())
        }
        Command::CopyClips { .. } => Ok(()),
        Command::RemoveRange {
            start,
            end,
            track_ids,
            ripple,
        } => ops_range::remove_range(p, start, end, &track_ids, ripple),
        Command::CloseGap {
            track_id,
            at,
            all_tracks,
        } => {
            if all_tracks {
                ops_range::close_gap_all(p, &track_id, at)
            } else {
                ops_range::close_gap(p, &track_id, at)
            }
        }
        Command::CloseAllGaps { track_id } => ops_range::close_all_gaps(p, &track_id),
        Command::FreezeFrame { clip_id, at, dur } => {
            ops_range::freeze_frame(p, &clip_id, at, dur).map(|_| ())
        }
        Command::TrimEdge {
            clip_id,
            edge,
            new_time,
            ripple,
        } => ops_trim::trim_edge(p, &clip_id, edge, new_time, ripple),
        Command::RollEdit {
            left_id,
            right_id,
            new_cut,
        } => ops_trim::roll_edit(p, &left_id, &right_id, new_cut),
        Command::Slip { clip_id, delta } => ops_trim::slip(p, &clip_id, delta),
        Command::Slide { clip_id, delta } => ops_trim::slide(p, &clip_id, delta),
        Command::SetSpeed {
            clip_id,
            speed,
            ripple,
        } => ops_trim::set_speed(p, &clip_id, speed, ripple),
        Command::SetReverse { clip_id, reverse } => ops_trim::set_reverse(p, &clip_id, reverse),
        Command::JoinThroughEdit { left_id, right_id } => {
            ops_trim::join_through_edit(p, &left_id, &right_id).map(|_| ())
        }
        Command::SetWorkArea { area } => ops_trim::set_work_area(p, area),
        Command::GroupClips { ids } => ops_group::group_clips(p, &ids).map(|_| ()),
        Command::Ungroup { ids } => ops_group::ungroup(p, &ids).map(|_| ()),
        Command::LinkClips { ids } => ops_group::link_clips(p, &ids),
        Command::UnlinkClips { ids } => ops_group::unlink_clips(p, &ids),
        Command::DetachAudio { clip_id } => ops_group::detach_audio(p, &clip_id).map(|_| ()),
        Command::EditGroup {
            group_id,
            name,
            color,
        } => ops_group::edit_group(p, &group_id, name, color),
        Command::AdjustGroupGain { group_id, delta_db } => {
            ops_group::adjust_group_gain(p, &group_id, delta_db)
        }
        Command::PatchClips { ids, patch } => ops_props::patch_clips(p, &ids, &patch),
        Command::SetEnvelope { clip_id, points } => ops_props::set_envelope(p, &clip_id, points),
        Command::OffsetGain { ids, delta_db } => ops_props::offset_gain(p, &ids, delta_db),
        Command::AddTrack { kind } => ops_props::add_track(p, kind).map(|_| ()),
        Command::RemoveTrack { track_id } => ops_props::remove_track(p, &track_id),
        Command::PatchTrack { track_id, patch } => ops_props::patch_track(p, &track_id, &patch),
        Command::MoveTrack { track_id, delta } => ops_props::move_track(p, &track_id, delta),
        Command::SetCanvas { canvas } => ops_props::set_canvas(p, canvas),
        Command::RenameProject { name } => ops_props::rename_project(p, &name),
        Command::SetModes { magnetic, snap } => ops_props::set_modes(p, magnetic, snap),
        Command::AddMarker { time, label, color } => {
            ops_props::add_marker(p, time, &label, color).map(|_| ())
        }
        Command::EditMarker {
            marker_id,
            time,
            label,
            color,
        } => ops_props::edit_marker(p, &marker_id, time, label, color),
        Command::RemoveMarker { marker_id } => ops_props::remove_marker(p, &marker_id),
        Command::AddTitle {
            track_id,
            at,
            dur,
            text,
        } => ops_props::add_title(p, &track_id, at, dur, text).map(|_| ()),
        Command::AddSolid {
            track_id,
            at,
            dur,
            color,
        } => ops_props::add_solid(p, &track_id, at, dur, color).map(|_| ()),
    }
}
