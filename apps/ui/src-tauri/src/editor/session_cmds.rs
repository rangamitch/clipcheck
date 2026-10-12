//! Editing commands against the open session. Owner: worker `ui-session`.
//! DTOs final; commands operate on the open session under its mutex.

use std::sync::MutexGuard;
use std::time::{SystemTime, UNIX_EPOCH};

use clipcheck_timeline::cmd::{Applied, Command, Session};
use clipcheck_timeline::model::{Project, UiState};
use clipcheck_timeline::plan::{derive_plan, RenderPlan};
use clipcheck_timeline::snap::{self, SnapKind};
use clipcheck_timeline::time::Tick;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{EditorSnapshot, EditorState};

/// Error returned when no project is open.
const NO_PROJECT: &str = "no project open";

/// Reply of `editor_apply`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReply {
    /// What the command created and removed.
    pub applied: Applied,
    /// State after the command.
    pub snapshot: EditorSnapshot,
}

/// One snap point for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapPointDto {
    /// Time.
    pub time: Tick,
    /// "origin", "playhead", "clip_edge", "marker" or "work_area".
    pub kind: String,
}

/// Maps a [`SnapKind`] to its DTO string.
pub fn snap_kind_name(k: SnapKind) -> &'static str {
    match k {
        SnapKind::Origin => "origin",
        SnapKind::Playhead => "playhead",
        SnapKind::ClipEdge => "clip_edge",
        SnapKind::Marker => "marker",
        SnapKind::WorkArea => "work_area",
    }
}

/// Builds a snapshot of a session (project clone, undo/redo flags, whole-timeline plan).
pub fn snapshot_of(session: &clipcheck_timeline::cmd::Session) -> EditorSnapshot {
    finish_snapshot(snapshot_parts(session))
}

/// The session data a snapshot needs, copied while the session lock is held. The plan is derived
/// afterwards by [`finish_snapshot`], so the lock is not held during that work.
struct SnapshotParts {
    project: Project,
    can_undo: bool,
    can_redo: bool,
    dirty: bool,
    clipboard_len: usize,
}

/// Copies the snapshot data out of a session (cheap apart from the project clone).
fn snapshot_parts(session: &Session) -> SnapshotParts {
    SnapshotParts {
        project: session.project.clone(),
        can_undo: session.can_undo(),
        can_redo: session.can_redo(),
        dirty: session.is_dirty(),
        clipboard_len: session.clipboard.len(),
    }
}

/// Derives the plan and assembles the snapshot. Call without holding the session lock.
fn finish_snapshot(parts: SnapshotParts) -> EditorSnapshot {
    let plan = derive_plan(&parts.project, None);
    EditorSnapshot {
        project: parts.project,
        can_undo: parts.can_undo,
        can_redo: parts.can_redo,
        dirty: parts.dirty,
        plan,
        clipboard_len: parts.clipboard_len,
    }
}

/// Locks the session slot, recovering the guard from a poisoned mutex.
fn lock_session(state: &EditorState) -> MutexGuard<'_, Option<Session>> {
    state.session.lock().unwrap_or_else(|e| e.into_inner())
}

/// Current time in milliseconds since the Unix epoch (0 if the clock is before the epoch).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Applies a command to the open session, stamps `modified_ms` with the current time on success,
/// returns the new snapshot. Error string = the `EditError` display text. `Err("no project open")`
/// when there is no session.
#[tauri::command(async)]
pub fn editor_apply(state: State<'_, EditorState>, command: Command) -> Result<ApplyReply, String> {
    let (applied, parts) = {
        let mut guard = lock_session(&state);
        let session = guard.as_mut().ok_or_else(|| NO_PROJECT.to_string())?;
        let applied = session.apply(command).map_err(|e| e.to_string())?;
        session.project.modified_ms = now_ms();
        (applied, snapshot_parts(session))
    };
    let snapshot = finish_snapshot(parts);
    Ok(ApplyReply { applied, snapshot })
}

/// Snapshot of the open session, or `None` when no project is open (not an error).
#[tauri::command]
pub fn editor_current_snapshot(
    state: State<'_, EditorState>,
) -> Result<Option<EditorSnapshot>, String> {
    let parts = {
        let guard = lock_session(&state);
        guard.as_ref().map(snapshot_parts)
    };
    Ok(parts.map(finish_snapshot))
}

/// Undo.
#[tauri::command]
pub fn editor_undo(state: State<'_, EditorState>) -> Result<EditorSnapshot, String> {
    let mut guard = lock_session(&state);
    let session = guard.as_mut().ok_or_else(|| NO_PROJECT.to_string())?;
    session.undo().map_err(|e| e.to_string())?;
    session.project.modified_ms = now_ms();
    Ok(snapshot_of(session))
}

/// Redo.
#[tauri::command]
pub fn editor_redo(state: State<'_, EditorState>) -> Result<EditorSnapshot, String> {
    let mut guard = lock_session(&state);
    let session = guard.as_mut().ok_or_else(|| NO_PROJECT.to_string())?;
    session.redo().map_err(|e| e.to_string())?;
    session.project.modified_ms = now_ms();
    Ok(snapshot_of(session))
}

/// Stores cosmetic UI state (zoom, scroll, playhead) without touching undo or the dirty flag.
#[tauri::command]
pub fn editor_set_ui_state(state: State<'_, EditorState>, ui: UiState) -> Result<(), String> {
    let mut guard = lock_session(&state);
    let session = guard.as_mut().ok_or_else(|| NO_PROJECT.to_string())?;
    session.project.ui_state = ui;
    Ok(())
}

/// Render plan for a range (None = whole timeline).
#[tauri::command]
pub fn editor_plan(
    state: State<'_, EditorState>,
    start: Option<Tick>,
    end: Option<Tick>,
) -> Result<RenderPlan, String> {
    let guard = lock_session(&state);
    let session = guard.as_ref().ok_or_else(|| NO_PROJECT.to_string())?;
    let range = match (start, end) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    };
    Ok(derive_plan(&session.project, range))
}

/// Snap points for drags (clips in `exclude` omitted).
#[tauri::command]
pub fn editor_snap_points(
    state: State<'_, EditorState>,
    exclude: Vec<String>,
    playhead: Tick,
) -> Result<Vec<SnapPointDto>, String> {
    let guard = lock_session(&state);
    let session = guard.as_ref().ok_or_else(|| NO_PROJECT.to_string())?;
    let points = snap::snap_points(&session.project, &exclude, playhead);
    Ok(points
        .into_iter()
        .map(|sp| SnapPointDto {
            time: sp.time,
            kind: snap_kind_name(sp.kind).to_string(),
        })
        .collect())
}

/// Clips currently on the session clipboard (for the UI to enable Paste).
#[tauri::command]
pub fn editor_clipboard_len(state: State<'_, EditorState>) -> Result<usize, String> {
    let guard = lock_session(&state);
    let session = guard.as_ref().ok_or_else(|| NO_PROJECT.to_string())?;
    Ok(session.clipboard.len())
}
