//! Video editor backend (docs/design/13-video-editor.md, docs/impl/editor.md).
//!
//! The editing `Session` lives here (single source of truth, D69); the UI sends
//! `timeline::Command`s through `editor_apply` and receives full snapshots back. Submodules, each
//! owned by one worker: `paths` (pure helpers), `settings` (editor settings file), `projects`
//! (project files), `session_cmds` (apply/undo/plan), `media` (probe/waveform/thumbnails/preview
//! URLs), `export` (job manager).

use std::sync::Mutex;

use clipcheck_timeline::cmd::Session;
use serde::{Deserialize, Serialize};

pub mod dialogs;
pub mod export;
pub mod media;
pub mod paths;
pub mod projects;
pub mod session_cmds;
pub mod settings;

/// Tauri-managed editor state.
#[derive(Default)]
pub struct EditorState {
    /// The open editing session, if any.
    pub session: Mutex<Option<Session>>,
    /// Export job manager.
    pub exports: export::ExportManager,
}

/// What the UI receives after opening a project or applying an edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorSnapshot {
    /// The whole project.
    pub project: clipcheck_timeline::Project,
    /// Undo possible.
    pub can_undo: bool,
    /// Redo possible.
    pub can_redo: bool,
    /// Unsaved changes.
    pub dirty: bool,
    /// Flattened plan of the whole timeline (preview reads this).
    pub plan: clipcheck_timeline::plan::RenderPlan,
    /// Number of clips on the clipboard.
    pub clipboard_len: usize,
}
