//! Project files and the open session lifecycle. Owner: worker `ui-projects`.
//! DTOs are the final CONTRACT.
//!
//! Storage: `<editor_root>\projects\<id>.ccproj` (pretty JSON via `timeline::json`), autosave
//! `<id>.autosave`. Writes are atomic (`.part` + rename). Deleting a project moves the files to
//! the Recycle Bin via `SHFileOperationW`; media is never touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use tauri::State;

use clipcheck_timeline::cmd::{Command, Session};
use clipcheck_timeline::json;
use clipcheck_timeline::model::{new_id, Canvas, EditModes, MediaRef, Project};
use clipcheck_timeline::ops_basic::PlaceMode;
use clipcheck_timeline::time::{Rate, Tick};

use super::media::editor_media_from_recording;
use super::settings::{self, EditorSettings};
use super::{paths, session_cmds, EditorSnapshot, EditorState};
use crate::mediaserver;

/// Aspect presets: (id, width, height) at the standard 1080 px size.
const PRESETS: [(&str, u32, u32); 4] = [
    ("16:9", 1920, 1080),
    ("9:16", 1080, 1920),
    ("1:1", 1080, 1080),
    ("4:5", 1080, 1350),
];

/// Row of the Editor home screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    /// Project id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Modified, ms since epoch.
    pub modified_ms: i64,
    /// Timeline length in ms.
    pub duration_ms: i64,
    /// Number of clips.
    pub clip_count: u32,
    /// Canvas width.
    pub width: u32,
    /// Canvas height.
    pub height: u32,
    /// True when a newer `.autosave` exists (crash recovery).
    pub has_autosave: bool,
    /// True when the file could not be parsed.
    pub broken: bool,
}

/// `%LOCALAPPDATA%\ClipCheck\editor`.
fn editor_root() -> PathBuf {
    paths::editor_root(std::env::var("LOCALAPPDATA").ok().as_deref())
}

/// `<editor_root>\projects`.
fn projects_dir() -> PathBuf {
    paths::projects_dir(&editor_root())
}

/// `<editor_root>\settings.json`.
fn settings_path() -> PathBuf {
    editor_root().join("settings.json")
}

/// `.ccproj` path of a project id, rejecting ids that could escape the folder.
fn ccproj_path(id: &str) -> Result<PathBuf, String> {
    paths::project_file(&projects_dir(), id).ok_or_else(|| format!("invalid project id \"{id}\""))
}

/// `.autosave` path of a project id, rejecting ids that could escape the folder.
fn autosave_path(id: &str) -> Result<PathBuf, String> {
    paths::autosave_file(&projects_dir(), id).ok_or_else(|| format!("invalid project id \"{id}\""))
}

/// Milliseconds since the Unix epoch, clamped into `i64`.
fn millis_i64(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// Current time, ms since the Unix epoch (0 if the clock is before 1970).
fn now_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => millis_i64(d),
        Err(_) => 0,
    }
}

/// Modified time of a file in ms since the Unix epoch, None when unavailable.
fn file_mtime_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(millis_i64(since))
}

/// Loads a project from an arbitrary file and sets its id to `id`.
fn load_project_file(path: &Path, id: &str) -> Result<Project, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut project =
        json::from_json(&text).map_err(|e| format!("project {id} could not be loaded: {e}"))?;
    project.id = id.to_string();
    Ok(project)
}

/// Reads the saved `<id>.ccproj`.
fn read_project(id: &str) -> Result<Project, String> {
    let path = ccproj_path(id)?;
    load_project_file(&path, id)
}

/// Writes `text` to `path` through `<name>.part` and a rename, creating the folder if needed.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("bad file name {}", path.display()))?;
    let tmp = path.with_file_name(format!("{name}.part"));
    fs::write(&tmp, text).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("could not replace {}: {e}", path.display())
    })
}

/// Deletes a file, treating "already gone" as success.
fn remove_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not delete {}: {e}", path.display())),
    }
}

/// True when the autosave of `id` exists and is newer than the saved file.
fn autosave_newer(projects_dir: &Path, id: &str, ccproj: &Path) -> bool {
    let Some(auto) = paths::autosave_file(projects_dir, id) else {
        return false;
    };
    let Some(auto_ms) = file_mtime_ms(&auto) else {
        return false;
    };
    match file_mtime_ms(ccproj) {
        Some(saved_ms) => auto_ms > saved_ms,
        None => true,
    }
}

/// Builds the list row of a loaded project.
fn summary_of(project: &Project, id: &str, has_autosave: bool) -> ProjectSummary {
    ProjectSummary {
        id: id.to_string(),
        name: project.name.clone(),
        modified_ms: project.modified_ms,
        duration_ms: project.timeline_end().as_ms(),
        clip_count: u32::try_from(project.clips.len()).unwrap_or(u32::MAX),
        width: project.canvas.width,
        height: project.canvas.height,
        has_autosave,
        broken: false,
    }
}

/// Marks each media missing when its file is gone or no longer matches its fingerprint. A media
/// whose fingerprint size is 0 (unknown) is only checked for existence. Does not change anything
/// else.
pub(crate) fn refresh_media_status(project: &mut Project) {
    for media in project.media.iter_mut() {
        media.missing = media_is_missing(media);
    }
}

/// True when the media file is absent or differs from its fingerprint.
pub(crate) fn media_is_missing(media: &MediaRef) -> bool {
    let path = Path::new(&media.path);
    let Ok(meta) = fs::metadata(path) else {
        return true;
    };
    if !meta.is_file() {
        return true;
    }
    if media.fingerprint.size == 0 {
        return false;
    }
    let mtime = file_mtime_ms(path).unwrap_or(0);
    meta.len() != media.fingerprint.size || mtime != media.fingerprint.mtime_ms
}

/// Locks the open-session slot. The guard must not be held across an `.await`.
fn lock_session(state: &EditorState) -> Result<MutexGuard<'_, Option<Session>>, String> {
    // A panic while the lock was held leaves the session in its last consistent state; recover it
    // like the other editor modules do instead of failing every later command.
    Ok(state.session.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Applies one command to a session, turning the error into text.
fn apply(session: &mut Session, cmd: Command) -> Result<(), String> {
    session.apply(cmd).map(|_| ()).map_err(|e| e.to_string())
}

/// Checks a name for rename: trimmed, 1 to 120 characters.
fn checked_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    let len = trimmed.chars().count();
    if (1..=120).contains(&len) {
        Ok(trimmed.to_string())
    } else {
        Err("the project name must be 1 to 120 characters".to_string())
    }
}

/// Name for a new project: trimmed, "Untitled project" when empty, at most 120 characters.
fn new_project_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Untitled project".to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

/// Name of the preset whose aspect ratio matches `width` x `height`, else "custom".
fn aspect_name(width: u32, height: u32) -> String {
    PRESETS
        .iter()
        .find(|(_, pw, ph)| u64::from(width) * u64::from(*ph) == u64::from(height) * u64::from(*pw))
        .map_or_else(|| "custom".to_string(), |(name, _, _)| (*name).to_string())
}

/// Canvas chosen by the settings' default when the caller gave none. `match_first_clip` uses the
/// first clip's size (even, 128..=8192) and frame rate (at most 120, 30 when unknown).
fn default_canvas_for(settings: &EditorSettings, first: Option<&MediaRef>) -> Canvas {
    if settings.default_canvas == "match_first_clip" {
        if let Some(m) = first.filter(|m| m.width > 0 && m.height > 0) {
            let width = (m.width & !1).clamp(128, 8192);
            let height = (m.height & !1).clamp(128, 8192);
            let fps = if m.fps.is_finite() && m.fps > 0.0 {
                Rate::from_f64(f64::from(m.fps).min(120.0))
            } else {
                Rate::whole(30)
            };
            return Canvas {
                width,
                height,
                fps,
                aspect_preset: aspect_name(width, height),
                ..Canvas::default()
            };
        }
        return Canvas::default();
    }
    match PRESETS
        .iter()
        .find(|(name, _, _)| *name == settings.default_canvas)
    {
        Some((name, width, height)) => Canvas {
            width: *width,
            height: *height,
            fps: Rate::whole(30),
            aspect_preset: (*name).to_string(),
            ..Canvas::default()
        },
        None => Canvas::default(),
    }
}

/// Opens a project file as the active session. With `make_dirty`, a no-op `SetModes` is applied so
/// the session counts as unsaved (used for autosave recovery).
fn open_file(
    state: &EditorState,
    path: &Path,
    id: &str,
    make_dirty: bool,
) -> Result<EditorSnapshot, String> {
    let mut project = load_project_file(path, id)?;
    refresh_media_status(&mut project);
    let modes = project.modes;
    let mut session = Session::new(project);
    let same_project = lock_session(state)?
        .as_ref()
        .is_some_and(|s| s.project.id == id);
    if !same_project {
        mediaserver::clear_imports();
    }
    if make_dirty {
        apply(
            &mut session,
            Command::SetModes {
                magnetic: modes.magnetic,
                snap: modes.snap,
            },
        )?;
    }
    let snapshot = session_cmds::snapshot_of(&session);
    *lock_session(state)? = Some(session);
    Ok(snapshot)
}

/// Deletes the given files by moving them to the Recycle Bin (undoable from there).
#[cfg(windows)]
fn recycle(files: &[PathBuf]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };

    // pFrom is a list of NUL-terminated paths followed by one extra NUL.
    let mut wide: Vec<u16> = Vec::new();
    for file in files {
        wide.extend(file.as_os_str().encode_wide());
        wide.push(0);
    }
    wide.push(0);
    let flags = FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0;
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(wide.as_ptr()),
        fFlags: flags as u16,
        ..Default::default()
    };
    // SAFETY: `op` is a valid, exclusively borrowed struct for the duration of the call, and
    // `pFrom` points into `wide`, which lives until the call returns and is double-NUL terminated.
    let code = unsafe { SHFileOperationW(&mut op) };
    // Copy the flag out first: the struct is packed on 32-bit builds.
    let aborted = op.fAnyOperationsAborted;
    if code != 0 || aborted.as_bool() {
        return Err(format!(
            "could not move the project to the Recycle Bin (code {code})"
        ));
    }
    Ok(())
}

/// Deletes the given files (no Recycle Bin on this platform).
#[cfg(not(windows))]
fn recycle(files: &[PathBuf]) -> Result<(), String> {
    for file in files {
        remove_if_exists(file)?;
    }
    Ok(())
}

/// Lists projects, newest modified first. Unreadable files are listed with `broken = true`.
#[tauri::command(async)]
pub fn editor_list_projects() -> Result<Vec<ProjectSummary>, String> {
    let dir = projects_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("could not read {}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ccproj") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let has_autosave = autosave_newer(&dir, &id, &path);
        let summary = match load_project_file(&path, &id) {
            Ok(project) => summary_of(&project, &id, has_autosave),
            Err(_) => ProjectSummary {
                id: id.clone(),
                name: id.clone(),
                modified_ms: file_mtime_ms(&path).unwrap_or(0),
                duration_ms: 0,
                clip_count: 0,
                width: 0,
                height: 0,
                has_autosave,
                broken: true,
            },
        };
        out.push(summary);
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified_ms));
    Ok(out)
}

/// Creates a project and opens it as the active session. `recording_ids` (library recording ids)
/// are appended in order to V1/A1 (via `media::media_from_recording` + `PlaceMedia` Append). The
/// canvas is `canvas` if given, else per `EditorSettings.default_canvas` (match first clip:
/// the first clip's size snapped to the nearest preset, fps = nearest standard rate, capped 120).
/// The new project is saved immediately.
#[tauri::command]
pub async fn editor_new_project(
    state: State<'_, EditorState>,
    name: String,
    canvas: Option<Canvas>,
    recording_ids: Vec<String>,
) -> Result<EditorSnapshot, String> {
    let settings = settings::load(&settings_path());
    let name = new_project_name(&name);

    // Probe every recording first: the canvas decision needs the first clip. A recording that
    // cannot be probed is skipped; only when none can be added is the command an error.
    let mut medias: Vec<MediaRef> = Vec::with_capacity(recording_ids.len());
    let mut skipped: Vec<String> = Vec::new();
    let mut first_failure: Option<String> = None;
    for recording_id in &recording_ids {
        match editor_media_from_recording(recording_id.clone()).await {
            Ok(media) => medias.push(media),
            Err(e) => {
                skipped.push(recording_id.clone());
                first_failure.get_or_insert(e);
            }
        }
    }
    if medias.is_empty() {
        if let Some(first) = first_failure {
            return Err(format!(
                "none of the recordings could be added to the project; first failure: {first}"
            ));
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "editor_new_project: skipped recordings that could not be probed: {}",
            skipped.join(", ")
        );
    }

    let mut project = Project::new(&name);
    let now = now_ms();
    project.created_ms = now;
    project.modified_ms = now;
    project.modes = EditModes {
        magnetic: settings.magnetic_default,
        snap: settings.snap_default,
    };
    let mut session = Session::new(project);

    let canvas = canvas.unwrap_or_else(|| default_canvas_for(&settings, medias.first()));
    apply(&mut session, Command::SetCanvas { canvas })?;

    for media in medias {
        let path = media.path.clone();
        apply(&mut session, Command::AddMedia { media })?;
        // AddMedia returns an existing entry when the same file was added before, so look the
        // id up by path instead of trusting the probed id.
        let media_id = session
            .project
            .media
            .iter()
            .find(|m| m.path == path)
            .map(|m| m.id.clone())
            .ok_or_else(|| format!("could not add {path} to the media bin"))?;
        apply(
            &mut session,
            Command::PlaceMedia {
                media_id,
                at: Tick::ZERO,
                src_in: Tick::ZERO,
                src_len: Tick::ZERO,
                video_track: None,
                mode: PlaceMode::Append,
            },
        )?;
    }

    // Setup steps (canvas, media, placement) are the project's content, not undoable history.
    let mut session = Session::new(session.project.clone());
    session.project.modified_ms = now_ms();
    let path = ccproj_path(&session.project.id)?;
    write_atomic(&path, &json::to_json(&session.project))?;
    session.mark_saved();

    let snapshot = session_cmds::snapshot_of(&session);
    mediaserver::clear_imports();
    *lock_session(&state)? = Some(session);
    Ok(snapshot)
}

/// Opens a saved project as the active session (replacing any open one without saving).
/// Refreshes each media's `missing` flag from the file system and fingerprint.
#[tauri::command]
pub async fn editor_open_project(
    state: State<'_, EditorState>,
    id: String,
) -> Result<EditorSnapshot, String> {
    let path = ccproj_path(&id)?;
    open_file(&state, &path, &id, false)
}

/// Opens the autosave of a project instead of the saved file (crash recovery). The session is
/// dirty afterwards.
#[tauri::command]
pub async fn editor_recover_autosave(
    state: State<'_, EditorState>,
    id: String,
) -> Result<EditorSnapshot, String> {
    let path = autosave_path(&id)?;
    open_file(&state, &path, &id, true)
}

/// Saves the active session to its `.ccproj`, updates `modified_ms`, deletes the autosave, marks
/// the session clean.
#[tauri::command]
pub fn editor_save_project(state: State<'_, EditorState>) -> Result<(), String> {
    let mut guard = lock_session(&state)?;
    let session = guard
        .as_mut()
        .ok_or_else(|| "no project open".to_string())?;
    session.project.modified_ms = now_ms();
    let path = ccproj_path(&session.project.id)?;
    write_atomic(&path, &json::to_json(&session.project))?;
    remove_if_exists(&autosave_path(&session.project.id)?)?;
    session.mark_saved();
    Ok(())
}

/// Writes the active session to `.autosave` (does not touch the saved file or the dirty flag).
/// No-op when the session is clean.
#[tauri::command]
pub fn editor_autosave(state: State<'_, EditorState>) -> Result<(), String> {
    let guard = lock_session(&state)?;
    let Some(session) = guard.as_ref() else {
        return Ok(());
    };
    if !session.is_dirty() {
        return Ok(());
    }
    let path = autosave_path(&session.project.id)?;
    write_atomic(&path, &json::to_json(&session.project))
}

/// Closes the active session. `discard_autosave` deletes the `.autosave`.
#[tauri::command]
pub fn editor_close_project(
    state: State<'_, EditorState>,
    discard_autosave: bool,
) -> Result<(), String> {
    let mut guard = lock_session(&state)?;
    if discard_autosave {
        if let Some(session) = guard.as_ref() {
            remove_if_exists(&autosave_path(&session.project.id)?)?;
        }
    }
    *guard = None;
    mediaserver::clear_imports();
    Ok(())
}

/// Renames a saved project (and the open session if it is the same one).
#[tauri::command]
pub fn editor_rename_project(
    state: State<'_, EditorState>,
    id: String,
    name: String,
) -> Result<(), String> {
    let name = checked_name(&name)?;
    let path = ccproj_path(&id)?;
    let mut guard = lock_session(&state)?;
    if let Some(session) = guard.as_mut() {
        if session.project.id == id {
            apply(session, Command::RenameProject { name: name.clone() })?;
        }
    }
    if path.exists() {
        let mut project = load_project_file(&path, &id)?;
        project.name = name;
        write_atomic(&path, &json::to_json(&project))?;
    }
    Ok(())
}

/// Duplicates a saved project under a new id and name "<name> copy"; returns the new id.
#[tauri::command]
pub fn editor_duplicate_project(id: String) -> Result<String, String> {
    let mut project = read_project(&id)?;
    let copy_id = new_id();
    let now = now_ms();
    project.name = format!("{} copy", project.name);
    project.id = copy_id.clone();
    project.created_ms = now;
    project.modified_ms = now;
    let path = ccproj_path(&copy_id)?;
    write_atomic(&path, &json::to_json(&project))?;
    Ok(copy_id)
}

/// Deletes a project's files (Recycle Bin). Never touches media. Refused for the open project.
#[tauri::command]
pub fn editor_delete_project(state: State<'_, EditorState>, id: String) -> Result<(), String> {
    let ccproj = ccproj_path(&id)?;
    let autosave = autosave_path(&id)?;
    let guard = lock_session(&state)?;
    if guard.as_ref().is_some_and(|s| s.project.id == id) {
        return Err("this project is open; close it before deleting".to_string());
    }
    let files: Vec<PathBuf> = [ccproj, autosave]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    recycle(&files)
}
