//! Export job manager and commands. Owner: worker `ui-export`.
//!
//! Behaviour (docs/design/13-video-editor.md sections 8 and 9):
//! - A job snapshots the project at start (clone) so later edits never affect it.
//! - Jobs run one at a time in a worker thread (`std::thread`, name `clipcheck-export`), below
//!   normal priority when `EditorSettings.low_priority`, each under `catch_unwind` (a panic makes
//!   the job `failed`, never crashes the app). The worker exits when the queue is empty.
//! - While `pause_while_recording` is true and the daemon reports a game recording, a monitor
//!   thread (`clipcheck-export-monitor`, one per running job) pauses the job with reason
//!   "recording" and resumes it when the recording ends. `editor_export_pause_override` lets the
//!   user export anyway.
//! - On success: the file is registered with the daemon (`register_edited_recording`, game
//!   "Exports", no source id, `keep_source_time = false`), then `export-done` is emitted.
//! - Events (all `serde(rename_all = "camelCase")` payloads): `export-progress` ([`JobDto`]),
//!   `export-done` ([`JobDto`]), `export-failed` ([`JobDto`]), `export-queue` (Vec<JobDto>).
//!
//! Lock order: `ExportManager::inner` before `ExportManager::jobs`. No lock is held across a
//! render call, a daemon request or an event emit.

use std::collections::HashSet;
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use clipcheck_hwenc::Codec;
use clipcheck_render::engine::{run_export, ExportReport, ExportRequest};
use clipcheck_render::{still, JobControl, Progress, RenderError};
use clipcheck_timeline::export::{
    apply_preset, builtin_presets, estimate_size, expand_name, find_preset, validate_settings,
    ExportKind, ExportRange, ExportSettings, Preset, SizeEstimate,
};
use clipcheck_timeline::model::{Project, TrackKind};
use clipcheck_timeline::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
};

use super::{paths, settings, EditorState};
use crate::fileops;
use crate::ipc::DaemonClient;
use crate::trim::parse_registered;

/// Folder used when the daemon config cannot be read.
const FALLBACK_ROOT: &str = "%USERPROFILE%\\Videos\\ClipCheck";

/// Minimum time between two `export-progress` events for one job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// How often the recording monitor asks the daemon for its status.
const MONITOR_INTERVAL: Duration = Duration::from_secs(2);

/// Most user presets the editor keeps (matches the limit in `settings::normalize`).
const MAX_USER_PRESETS: usize = 50;

/// State of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting its turn.
    Queued,
    /// Rendering.
    Running,
    /// Paused (user or recording).
    Paused,
    /// Finished.
    Done,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
}

/// A job as shown in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDto {
    /// Job id.
    pub id: String,
    /// Project name at start.
    pub project_name: String,
    /// State.
    pub state: JobState,
    /// 0..=100.
    pub percent: f32,
    /// Seconds elapsed (running time, not counting pauses).
    pub elapsed_s: f64,
    /// Estimated seconds left (None when unknown).
    pub eta_s: Option<f64>,
    /// Speed as a multiple of real time.
    pub speed_x: f64,
    /// Output path (final once Done).
    pub out_path: String,
    /// Bytes written so far or final size.
    pub bytes: u64,
    /// Why paused ("user", "recording") when paused.
    pub paused_reason: Option<String>,
    /// Error text when Failed.
    pub error: Option<String>,
    /// Warnings.
    pub warnings: Vec<String>,
    /// Encoder name when known.
    pub encoder: Option<String>,
    /// New library recording id once registered.
    pub recording_id: Option<String>,
}

/// Owns the queue and the worker thread.
#[derive(Default)]
pub struct ExportManager {
    /// Jobs in order (the DTO list the UI reads; mirrors `inner.entries`).
    pub jobs: Mutex<Vec<JobDto>>,
    /// Queue entries with their private state.
    inner: Mutex<Inner>,
}

/// Mutable queue state, guarded by `ExportManager::inner`.
#[derive(Default)]
struct Inner {
    /// Last job sequence number handed out.
    next_seq: u64,
    /// Every job still in the list (queued, running, paused, or finished but not dismissed).
    entries: Vec<JobEntry>,
    /// True while a worker thread is alive and will look at the queue again.
    worker_running: bool,
}

/// Flags copied from the editor settings when a job is queued.
#[derive(Debug, Clone, Copy)]
struct JobFlags {
    /// Pause while a game recording is active.
    pause_while_recording: bool,
    /// Run the render thread at below-normal priority.
    low_priority: bool,
    /// Open the export folder when the job is Done.
    open_folder: bool,
}

/// One job's private state.
struct JobEntry {
    /// What the UI sees.
    dto: JobDto,
    /// Project snapshot; taken by the worker when the job starts.
    project: Option<Project>,
    /// Settings of the job.
    settings: ExportSettings,
    /// Flags from the editor settings.
    flags: JobFlags,
    /// Cancel and pause flags shared with the engine.
    ctl: JobControl,
    /// Set by "export anyway": ignore the recording rule for this job.
    override_pause: bool,
    /// When the job started running.
    started: Option<Instant>,
    /// When the current pause began, if paused.
    paused_since: Option<Instant>,
    /// Total time spent paused before `paused_since`.
    paused_total: Duration,
    /// Scratch folder for the engine.
    work_dir: PathBuf,
}

/// A job handed from the queue to the worker.
struct Job {
    /// Job id.
    id: String,
    /// Project snapshot.
    project: Project,
    /// Settings.
    settings: ExportSettings,
    /// Flags.
    flags: JobFlags,
    /// Cancel and pause flags.
    ctl: JobControl,
    /// Output path.
    out_path: PathBuf,
    /// Scratch folder.
    work_dir: PathBuf,
}

/// Defaults shown when the export dialog opens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDefaults {
    /// Resolved export folder (configured, else `<root>\Exports`).
    pub dir: String,
    /// Free bytes on that drive.
    pub free_bytes: u64,
    /// Built-in plus user presets.
    pub presets: Vec<Preset>,
    /// Settings to start from (project's `export_defaults` adapted to the canvas).
    pub settings: ExportSettings,
    /// Always-ask-location flag.
    pub always_ask_location: bool,
    /// Hardware encoders available: names for the encoder dropdown (always includes "auto" and
    /// "software").
    pub encoders: Vec<String>,
}

/// Estimate for the dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateReply {
    /// Size estimate.
    pub estimate: SizeEstimate,
    /// Output duration in seconds.
    pub duration_s: f64,
    /// Settings problems (empty = OK).
    pub problems: Vec<String>,
    /// Warning chip text, e.g. "Likely over the 10 MB limit".
    pub warning: Option<String>,
    /// Final file path that would be used (`dir\name.ext`, collision-safe).
    pub out_path: String,
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

/// Locks a mutex, recovering the data from a poisoned lock.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs blocking work on the blocking pool and returns its result.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

/// Clones the open project out of the editor session.
fn project_snapshot(state: &EditorState) -> Result<Project, String> {
    let guard = lock(&state.session);
    guard
        .as_ref()
        .map(|session| session.project.clone())
        .ok_or_else(|| "No project is open".to_string())
}

/// Expands `%VAR%` pairs with environment values. Unknown names are kept as typed.
fn expand_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(rest.get(..start).unwrap_or(""));
        let after = rest.get(start..).unwrap_or("");
        let body = after.get(1..).unwrap_or("");
        match body.find('%') {
            Some(end) => {
                let name = body.get(..end).unwrap_or("");
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = body.get(end + 1..).unwrap_or("");
            }
            None => {
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Pulls `(root, export_dir)` out of a `get_config` reply. A missing root falls back to
/// [`FALLBACK_ROOT`]. None when the reply is not a config reply.
fn storage_paths(body: &Value) -> Option<(String, String)> {
    if body.get("type").and_then(Value::as_str) != Some("config") {
        return None;
    }
    let storage = body.pointer("/data/config/storage")?;
    let root = storage
        .get("root")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(FALLBACK_ROOT);
    let export = storage
        .get("export_dir")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some((root.to_string(), export.to_string()))
}

/// The export folder: `storage.export_dir` or `<storage.root>\Exports` from the daemon config,
/// with `%VARS%` expanded. Without a daemon, `%USERPROFILE%\Videos\ClipCheck\Exports`.
fn default_export_dir(client: Option<&DaemonClient>) -> PathBuf {
    let (root, export) = client
        .and_then(|c| c.request_command(json!({ "type": "get_config" })).ok())
        .and_then(|body| storage_paths(&body))
        .unwrap_or_else(|| (FALLBACK_ROOT.to_string(), String::new()));
    let root = expand_vars(&root);
    let export = expand_vars(&export);
    paths::export_dir(Path::new(&root), &export)
}

/// Folder for a new export: the settings' `dir` when set, else the default folder.
fn output_dir(settings: &ExportSettings, client: Option<&DaemonClient>) -> PathBuf {
    match settings
        .dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(dir) => PathBuf::from(expand_vars(dir)),
        None => default_export_dir(client),
    }
}

/// File extension for an output kind.
fn out_ext(kind: &ExportKind) -> &'static str {
    match kind {
        ExportKind::Video => "mp4",
        ExportKind::AudioOnly => "m4a",
        ExportKind::Still { format, .. } => {
            if format.eq_ignore_ascii_case("jpg") {
                "jpg"
            } else {
                "png"
            }
        }
    }
}

/// Local time as (year, month, day, hour, minute, second).
fn local_now() -> (i32, u32, u32, u32, u32, u32) {
    // SAFETY: GetLocalTime has no preconditions and only fills the struct it returns.
    let t = unsafe { GetLocalTime() };
    (
        i32::from(t.wYear),
        u32::from(t.wMonth),
        u32::from(t.wDay),
        u32::from(t.wHour),
        u32::from(t.wMinute),
        u32::from(t.wSecond),
    )
}

/// Free bytes on the drive of `dir` (or of its nearest existing parent). 0 on failure.
fn free_bytes(dir: &Path) -> u64 {
    let existing = dir.ancestors().find(|p| p.exists()).unwrap_or(dir);
    let wide = HSTRING::from(existing.to_string_lossy().as_ref());
    let mut avail: u64 = 0;
    // SAFETY: `wide` is a valid wide string for the call, and `avail` outlives it. The other
    // out parameters are optional and left as None.
    let ok = unsafe { GetDiskFreeSpaceExW(&wide, Some(&mut avail as *mut u64), None, None) };
    if ok.is_ok() {
        avail
    } else {
        0
    }
}

/// When true, the encoder dropdown also lists the hardware encoders of H.264 and HEVC. The
/// engine can use hardware encoders (with a software fallback), so they are listed.
/// Set this to true when the engine can use hardware encoders again.
const LIST_HARDWARE_ENCODERS: bool = true;

/// Encoder names for the dropdown: "auto" and "software" only, unless
/// [`LIST_HARDWARE_ENCODERS`] is set. Then the hardware encoders of H.264 and HEVC follow.
/// Enumeration errors are ignored (the list then has only the fixed entries).
fn encoder_names() -> Vec<String> {
    let mut names = vec!["auto".to_string(), "software".to_string()];
    if LIST_HARDWARE_ENCODERS {
        // Media Foundation must be started before any enumeration. Failure only hides hardware.
        let _ = clipcheck_hwenc::mf_startup();
        for codec in [Codec::H264, Codec::Hevc] {
            if let Ok(list) = clipcheck_hwenc::enumerate(codec) {
                for desc in list.into_iter().filter(|d| d.hardware) {
                    if !names.contains(&desc.name) {
                        names.push(desc.name);
                    }
                }
            }
        }
    }
    names
}

/// Time span covered by a range, as (start, end). None when the range is empty or names
/// clips that do not exist.
fn export_span(project: &Project, range: &ExportRange) -> Option<(Tick, Tick)> {
    match range {
        ExportRange::Entire => {
            let end = project
                .clips
                .iter()
                .fold(Tick::ZERO, |acc, clip| acc.max(clip.end()));
            Some((Tick::ZERO, end))
        }
        ExportRange::WorkArea => match &project.work_area {
            Some(area) => Some((area.start, area.end)),
            None => export_span(project, &ExportRange::Entire),
        },
        ExportRange::SelectedClips { ids } => {
            let mut span: Option<(Tick, Tick)> = None;
            for clip in project.clips.iter().filter(|c| ids.contains(&c.id)) {
                span = Some(match span {
                    Some((start, end)) => (start.min(clip.start), end.max(clip.end())),
                    None => (clip.start, clip.end()),
                });
            }
            span
        }
    }
}

/// Number of audio tracks in the project (for the size estimate).
fn audio_track_count(project: &Project) -> u32 {
    let n = project
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Audio))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Builds a free output path `dir\stem.ext` for a job or still.
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    paths::unique_output_path(dir, stem, ext, &|p: &Path| p.exists())
}

/// Like [`unique_path`], but also skips paths already reserved by queued or running jobs, so
/// two jobs never get the same output file before the first one has written it.
fn unique_path_avoiding(dir: &Path, stem: &str, ext: &str, reserved: &[PathBuf]) -> PathBuf {
    paths::unique_output_path(dir, stem, ext, &|p: &Path| {
        p.exists() || reserved.iter().any(|r| r.as_path() == p)
    })
}

/// Size of the `<out>.part` file the engine writes before the rename. None when it cannot be
/// read (for example before the first frame is written).
fn part_size(out: &Path) -> Option<u64> {
    let mut name = out.as_os_str().to_owned();
    name.push(".part");
    fs::metadata(PathBuf::from(name)).ok().map(|m| m.len())
}

/// Finds a job entry by id.
fn entry_mut<'a>(inner: &'a mut Inner, job_id: &str) -> Option<&'a mut JobEntry> {
    inner.entries.iter_mut().find(|e| e.dto.id == job_id)
}

/// Copies the DTO list into the `jobs` mirror and returns it for emitting.
fn publish(manager: &ExportManager, inner: &Inner) -> Vec<JobDto> {
    let list: Vec<JobDto> = inner.entries.iter().map(|e| e.dto.clone()).collect();
    *lock(&manager.jobs) = list.clone();
    list
}

/// Emits the whole queue.
fn emit_queue(app: &AppHandle, list: Vec<JobDto>) {
    let _ = app.emit("export-queue", list);
}

/// Changes a job under the lock, publishes the list and returns the job plus the list. None
/// when the editor state or the job is missing.
fn update_job(
    app: &AppHandle,
    job_id: &str,
    change: impl FnOnce(&mut JobEntry),
) -> Option<(JobDto, Vec<JobDto>)> {
    let st = app.try_state::<EditorState>()?;
    let mut inner = lock(&st.exports.inner);
    let entry = entry_mut(&mut inner, job_id)?;
    change(entry);
    let dto = entry.dto.clone();
    let list = publish(&st.exports, &inner);
    Some((dto, list))
}

/// Elapsed running time of a job, not counting pauses.
fn elapsed(entry: &JobEntry, now: Instant) -> Duration {
    let Some(start) = entry.started else {
        return Duration::ZERO;
    };
    let total = now.saturating_duration_since(start);
    let paused_now = entry
        .paused_since
        .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
    total.saturating_sub(entry.paused_total + paused_now)
}

/// Starts or ends a pause. `Some(reason)` pauses (or changes the reason), `None` resumes.
fn set_pause(entry: &mut JobEntry, reason: Option<&str>, now: Instant) {
    match reason {
        Some(reason) => {
            if entry.paused_since.is_none() {
                entry.paused_since = Some(now);
            }
            entry.ctl.set_paused(true);
            entry.dto.state = JobState::Paused;
            entry.dto.paused_reason = Some(reason.to_string());
        }
        None => {
            if let Some(since) = entry.paused_since.take() {
                entry.paused_total += now.saturating_duration_since(since);
            }
            entry.ctl.set_paused(false);
            if entry.dto.state == JobState::Paused {
                entry.dto.state = JobState::Running;
            }
            entry.dto.paused_reason = None;
        }
    }
}

/// Decides the recording rule for one job. Returns None for no change, `Some(Some(reason))` to
/// pause with that reason, `Some(None)` to resume. A user pause is never changed here.
fn pause_action(
    reason: Option<&str>,
    recording: bool,
    pause_while_recording: bool,
    override_pause: bool,
) -> Option<Option<&'static str>> {
    if reason == Some("user") {
        return None;
    }
    let want = recording && pause_while_recording && !override_pause;
    match (want, reason == Some("recording")) {
        (true, false) => Some(Some("recording")),
        (false, true) => Some(None),
        _ => None,
    }
}

/// Folds one engine progress report into the job's DTO.
fn apply_progress(entry: &mut JobEntry, p: &Progress, now: Instant) {
    let elapsed_s = elapsed(entry, now).as_secs_f64();
    let percent = p.percent.clamp(0.0, 100.0);
    entry.dto.percent = percent;
    entry.dto.elapsed_s = elapsed_s;
    entry.dto.eta_s = if percent > 0.0 && elapsed_s > 0.0 {
        Some(elapsed_s * f64::from(100.0 - percent) / f64::from(percent))
    } else {
        None
    };
    entry.dto.speed_x = if elapsed_s > 0.0 {
        p.out_secs / elapsed_s
    } else {
        0.0
    };
}

/// Sets the calling thread to below-normal or normal priority. A failure only costs speed, so
/// it is ignored.
fn set_thread_priority(low: bool) {
    let level = if low {
        THREAD_PRIORITY_BELOW_NORMAL
    } else {
        THREAD_PRIORITY_NORMAL
    };
    // SAFETY: GetCurrentThread returns a pseudo-handle valid for the calling thread.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), level);
    }
}

// ---------------------------------------------------------------------------------------------
// Daemon interaction
// ---------------------------------------------------------------------------------------------

/// True when the daemon reports a game recording (or a part split of one). Any failure counts as
/// not recording.
fn recording_now(client: &DaemonClient) -> bool {
    let Ok(body) = client.request_command(json!({ "type": "get_status" })) else {
        return false;
    };
    body.get("type").and_then(Value::as_str) == Some("status")
        && matches!(
            body.pointer("/data/state").and_then(Value::as_str),
            Some("recording" | "splitting" | "buffer_only" | "stopping")
        )
}

/// Registers a finished export in the library as game "Exports" and returns the new id.
fn register_export(client: &DaemonClient, path: &Path) -> Result<String, String> {
    let cmd = json!({
        "type": "register_edited_recording",
        "data": {
            "source_recording_id": null,
            "path": path.to_string_lossy().into_owned(),
            "keep_source_time": false,
            "game": "Exports",
        }
    });
    let reply = client.request_command(cmd).map_err(|e| e.to_string())?;
    parse_registered(&reply)
}

// ---------------------------------------------------------------------------------------------
// Worker and monitor threads
// ---------------------------------------------------------------------------------------------

/// Takes the first queued job and marks it running. Jobs without a project snapshot are failed
/// and skipped. None when nothing is queued.
fn take_next(inner: &mut Inner, now: Instant) -> Option<Job> {
    loop {
        let entry = inner
            .entries
            .iter_mut()
            .find(|e| e.dto.state == JobState::Queued)?;
        match entry.project.take() {
            Some(project) => {
                entry.dto.state = JobState::Running;
                entry.started = Some(now);
                return Some(Job {
                    id: entry.dto.id.clone(),
                    project,
                    settings: entry.settings.clone(),
                    flags: entry.flags,
                    ctl: entry.ctl.clone(),
                    out_path: PathBuf::from(&entry.dto.out_path),
                    work_dir: entry.work_dir.clone(),
                });
            }
            None => {
                entry.dto.state = JobState::Failed;
                entry.dto.error = Some("The project snapshot is missing".to_string());
            }
        }
    }
}

/// Clears `worker_running` when the worker thread ends by panic or early exit, so the next queued
/// job starts a new worker (and restarts one at once when jobs are still waiting). Disarmed on
/// the normal exit, which clears the flag itself under the lock.
struct WorkerGuard {
    app: AppHandle,
    client: Arc<DaemonClient>,
    armed: bool,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(st) = self.app.try_state::<EditorState>() else {
            return;
        };
        let restart = {
            let mut inner = lock(&st.exports.inner);
            inner.worker_running = false;
            inner
                .entries
                .iter()
                .any(|e| e.dto.state == JobState::Queued)
        };
        if restart {
            let mut inner = lock(&st.exports.inner);
            if !inner.worker_running {
                inner.worker_running = true;
                if start_worker(self.app.clone(), Arc::clone(&self.client)).is_err() {
                    inner.worker_running = false;
                }
            }
        }
    }
}

/// Body of the `clipcheck-export` thread: runs queued jobs in order until the queue is empty.
fn worker_loop(app: AppHandle, client: Arc<DaemonClient>) {
    let mut guard = WorkerGuard {
        app: app.clone(),
        client: Arc::clone(&client),
        armed: true,
    };
    sweep_leftovers(&app, &client);
    loop {
        let Some(st) = app.try_state::<EditorState>() else {
            return;
        };
        let (job, list) = {
            let mut inner = lock(&st.exports.inner);
            match take_next(&mut inner, Instant::now()) {
                Some(job) => {
                    let list = publish(&st.exports, &inner);
                    (job, list)
                }
                None => {
                    inner.worker_running = false;
                    guard.armed = false;
                    return;
                }
            }
        };
        emit_queue(&app, list);
        run_job(&app, &client, job);
    }
}

/// Age after which a `*.part` file in the export folder counts as a leftover.
const PART_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Removes leftovers that no queued, running or paused job uses: folders in `export-work` whose
/// name is not a live job id, and `*.part` files older than [`PART_MAX_AGE`] in the default export
/// folder. Only those two places are touched. Runs once when a worker starts.
fn sweep_leftovers(app: &AppHandle, client: &DaemonClient) {
    let Some(st) = app.try_state::<EditorState>() else {
        return;
    };
    let (live_ids, live_part_names) = {
        let inner = lock(&st.exports.inner);
        let live: Vec<&JobEntry> = inner
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e.dto.state,
                    JobState::Queued | JobState::Running | JobState::Paused
                )
            })
            .collect();
        let ids: HashSet<String> = live.iter().map(|e| e.dto.id.clone()).collect();
        let parts: HashSet<std::ffi::OsString> = live
            .iter()
            .filter_map(|e| {
                let mut name = PathBuf::from(&e.dto.out_path).into_os_string();
                name.push(".part");
                PathBuf::from(name).file_name().map(|n| n.to_os_string())
            })
            .collect();
        (ids, parts)
    };

    let work_root =
        paths::editor_root(std::env::var("LOCALAPPDATA").ok().as_deref()).join("export-work");
    if let Ok(read) = fs::read_dir(&work_root) {
        for item in read.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            if path.is_dir() && !live_ids.contains(&name) {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }

    let folder = default_export_dir(Some(client));
    if let Ok(read) = fs::read_dir(&folder) {
        for item in read.flatten() {
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("part") || !path.is_file() {
                continue;
            }
            if live_part_names.contains(&item.file_name()) {
                continue;
            }
            let old = item
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= PART_MAX_AGE);
            if old {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Starts the worker thread.
fn start_worker(app: AppHandle, client: Arc<DaemonClient>) -> Result<(), String> {
    std::thread::Builder::new()
        .name("clipcheck-export".to_string())
        .spawn(move || worker_loop(app, client))
        .map(|_| ())
        .map_err(|e| format!("Could not start the export worker: {e}"))
}

/// Applies the recording rule to one job, emitting the queue only when something changed.
fn apply_recording_rule(app: &AppHandle, job_id: &str, recording: bool) {
    let Some(st) = app.try_state::<EditorState>() else {
        return;
    };
    let list = {
        let mut inner = lock(&st.exports.inner);
        let Some(entry) = entry_mut(&mut inner, job_id) else {
            return;
        };
        if !matches!(entry.dto.state, JobState::Running | JobState::Paused) {
            return;
        }
        let action = pause_action(
            entry.dto.paused_reason.as_deref(),
            recording,
            entry.flags.pause_while_recording,
            entry.override_pause,
        );
        let Some(reason) = action else {
            return;
        };
        set_pause(entry, reason, Instant::now());
        publish(&st.exports, &inner)
    };
    emit_queue(app, list);
}

/// Starts the recording monitor for one running job. It polls the daemon every
/// [`MONITOR_INTERVAL`] until `done` is set.
fn spawn_monitor(
    app: AppHandle,
    client: Arc<DaemonClient>,
    job_id: String,
    done: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("clipcheck-export-monitor".to_string())
        .spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let recording = recording_now(&client);
                apply_recording_rule(&app, &job_id, recording);
                std::thread::sleep(MONITOR_INTERVAL);
            }
        })
        .map(|_| ())
        .map_err(|e| format!("Could not start the recording check: {e}"))
}

/// Adds a warning to a job and emits the queue.
fn add_warning(app: &AppHandle, job_id: &str, message: String) {
    if let Some((_, list)) = update_job(app, job_id, |entry| entry.dto.warnings.push(message)) {
        emit_queue(app, list);
    }
}

/// Records a progress report, publishes the list and returns the job DTO to emit. `bytes` is
/// the current size of the partial file, when known.
fn record_progress(
    app: &AppHandle,
    job_id: &str,
    p: &Progress,
    bytes: Option<u64>,
    now: Instant,
) -> Option<JobDto> {
    update_job(app, job_id, |entry| {
        apply_progress(entry, p, now);
        if let Some(b) = bytes {
            entry.dto.bytes = b;
        }
    })
    .map(|(dto, _)| dto)
}

/// Marks a job Done, registers it in the library (only when `register` is true, that is for
/// video exports) and emits the events.
fn finish_done(
    app: &AppHandle,
    client: &DaemonClient,
    job_id: &str,
    flags: JobFlags,
    register: bool,
    report: ExportReport,
) {
    let mut notes: Vec<String> = Vec::new();
    let recording_id = if register {
        match register_export(client, &report.out_path) {
            Ok(id) => Some(id),
            Err(msg) => {
                notes.push(format!("Saved, but it was not added to the library: {msg}"));
                None
            }
        }
    } else {
        None
    };
    if flags.open_folder {
        if let Some(dir) = report.out_path.parent() {
            if let Err(msg) = fileops::open_folder(dir.to_string_lossy().into_owned()) {
                notes.push(msg);
            }
        }
    }
    let ExportReport {
        out_path,
        bytes,
        encoder,
        warnings,
        ..
    } = report;
    let outcome = update_job(app, job_id, move |entry| {
        entry.dto.state = JobState::Done;
        entry.dto.percent = 100.0;
        entry.dto.eta_s = Some(0.0);
        entry.dto.bytes = bytes;
        entry.dto.encoder = Some(encoder);
        entry.dto.out_path = out_path.to_string_lossy().into_owned();
        entry.dto.recording_id = recording_id;
        entry.dto.paused_reason = None;
        entry.paused_since = None;
        entry.dto.warnings.extend(warnings);
        entry.dto.warnings.extend(notes);
    });
    if let Some((dto, list)) = outcome {
        let _ = app.emit("export-done", dto);
        emit_queue(app, list);
    }
}

/// Marks a job Failed and emits the events.
fn finish_failed(app: &AppHandle, job_id: &str, error: String) {
    let outcome = update_job(app, job_id, move |entry| {
        entry.dto.state = JobState::Failed;
        entry.dto.paused_reason = None;
        entry.paused_since = None;
        entry.dto.error = Some(error);
    });
    if let Some((dto, list)) = outcome {
        let _ = app.emit("export-failed", dto);
        emit_queue(app, list);
    }
}

/// Marks a job Cancelled and emits the queue.
fn finish_cancelled(app: &AppHandle, job_id: &str) {
    let outcome = update_job(app, job_id, |entry| {
        entry.dto.state = JobState::Cancelled;
        entry.dto.paused_reason = None;
        entry.paused_since = None;
    });
    if let Some((_, list)) = outcome {
        emit_queue(app, list);
    }
}

/// Runs one job to its end: render under `catch_unwind`, then the matching finish step.
fn run_job(app: &AppHandle, client: &Arc<DaemonClient>, job: Job) {
    set_thread_priority(job.flags.low_priority);

    let done = Arc::new(AtomicBool::new(false));
    if job.flags.pause_while_recording {
        if let Err(msg) = spawn_monitor(
            app.clone(),
            Arc::clone(client),
            job.id.clone(),
            Arc::clone(&done),
        ) {
            add_warning(app, &job.id, msg);
        }
    }

    let register = matches!(job.settings.kind, ExportKind::Video);
    let outcome = {
        let mut last_emit: Option<Instant> = None;
        let mut on_progress = |p: Progress| {
            let now = Instant::now();
            // The final 100 % report is always sent, even inside the throttle window.
            let finished = p.percent >= 100.0;
            if !finished
                && last_emit.is_some_and(|t| now.saturating_duration_since(t) < PROGRESS_INTERVAL)
            {
                return;
            }
            last_emit = Some(now);
            let bytes = part_size(&job.out_path);
            if let Some(dto) = record_progress(app, &job.id, &p, bytes, now) {
                let _ = app.emit("export-progress", dto);
            }
        };
        let req = ExportRequest {
            project: &job.project,
            settings: &job.settings,
            out_path: &job.out_path,
            work_dir: &job.work_dir,
            ctl: &job.ctl,
        };
        std::panic::catch_unwind(AssertUnwindSafe(|| run_export(&req, &mut on_progress)))
    };

    done.store(true, Ordering::SeqCst);
    let _ = fs::remove_dir_all(&job.work_dir);

    match outcome {
        Ok(Ok(report)) => finish_done(app, client, &job.id, job.flags, register, report),
        Ok(Err(RenderError::Cancelled)) => finish_cancelled(app, &job.id),
        Ok(Err(e)) => finish_failed(app, &job.id, e.to_string()),
        Err(_) => finish_failed(app, &job.id, "The export stopped unexpectedly".to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

/// Dialog defaults for the open project.
#[tauri::command]
pub async fn editor_export_defaults(
    state: State<'_, EditorState>,
    client: State<'_, Arc<DaemonClient>>,
) -> Result<ExportDefaults, String> {
    let project = project_snapshot(&state)?;
    let daemon = Arc::clone(&*client);
    blocking(move || {
        let ed = settings::load(&settings::settings_path());
        let dir = default_export_dir(Some(&*daemon));
        let canvas = project.canvas.clone();

        let mut presets = builtin_presets();
        presets.extend(ed.custom_presets.iter().cloned());

        // A project with no stored settings starts from the app defaults. Once an export has
        // run, the settings it used are stored in the project and become the starting point.
        let start = if project.export_defaults == ExportSettings::default() {
            let base = ExportSettings {
                width: canvas.width,
                height: canvas.height,
                fps: Some(canvas.fps),
                ..project.export_defaults.clone()
            };
            let mut start = match find_preset(&ed.default_preset, &ed.custom_presets) {
                Some(preset) => apply_preset(&base, &preset, canvas.width, canvas.height),
                None => base,
            };
            start.encoder = ed.encoder.clone();
            start.name_template = ed.filename_template.clone();
            start
        } else {
            project.export_defaults.clone()
        };

        Ok(ExportDefaults {
            dir: dir.to_string_lossy().into_owned(),
            free_bytes: free_bytes(&dir),
            presets,
            settings: start,
            always_ask_location: ed.always_ask_location,
            encoders: encoder_names(),
        })
    })
    .await
}

/// Computes the estimate, validation, warning and output path for `settings`.
fn estimate_for(
    project: &Project,
    settings: &ExportSettings,
    client: &DaemonClient,
) -> Result<EstimateReply, String> {
    let (start, end) = export_span(project, &settings.range).unwrap_or((Tick::ZERO, Tick::ZERO));
    let duration = end.saturating_sub(start);
    let estimate = estimate_size(settings, duration, audio_track_count(project));
    let problems = validate_settings(settings);

    let user_presets = settings::load(&settings::settings_path()).custom_presets;
    let warning = find_preset(&settings.preset, &user_presets)
        .and_then(|preset| preset.max_mb)
        .and_then(|mb| {
            (estimate.bytes as f64 > mb * 1_000_000.0)
                .then(|| format!("Likely over the {mb} MB limit"))
        });

    let dir = output_dir(settings, Some(client));
    let stem = expand_name(
        &settings.name_template,
        &project.name,
        local_now(),
        settings.width,
        settings.height,
        &settings.preset,
        0,
    );
    let out = unique_path(&dir, &stem, out_ext(&settings.kind));

    Ok(EstimateReply {
        estimate,
        duration_s: duration.as_secs_f64(),
        problems,
        warning,
        out_path: out.to_string_lossy().into_owned(),
    })
}

/// Size/duration estimate and validation.
#[tauri::command]
pub async fn editor_estimate_export(
    state: State<'_, EditorState>,
    client: State<'_, Arc<DaemonClient>>,
    settings: ExportSettings,
) -> Result<EstimateReply, String> {
    let project = project_snapshot(&state)?;
    let daemon = Arc::clone(&*client);
    blocking(move || estimate_for(&project, &settings, &daemon)).await
}

/// Smart-lossless eligibility.
#[tauri::command]
pub async fn editor_check_smart_export(
    state: State<'_, EditorState>,
    settings: ExportSettings,
) -> Result<serde_json::Value, String> {
    let project = project_snapshot(&state)?;
    blocking(move || {
        let eligibility = clipcheck_render::smart::check_eligibility(&project, &settings);
        Ok(json!({
            "eligible": eligibility.eligible,
            "reason": eligibility.reason,
            "snapNotes": eligibility.snap_notes,
        }))
    })
    .await
}

/// Starts (or queues) an export of the open project; returns the job.
#[tauri::command]
pub async fn editor_export_start(
    app: AppHandle,
    state: State<'_, EditorState>,
    client: State<'_, Arc<DaemonClient>>,
    settings: ExportSettings,
) -> Result<JobDto, String> {
    let problems = validate_settings(&settings);
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let project = project_snapshot(&state)?;
    let is_still = matches!(settings.kind, ExportKind::Still { .. });
    if !is_still {
        match export_span(&project, &settings.range) {
            Some((start, end)) if end > start => {}
            _ => return Err("The export range is empty".to_string()),
        }
    }

    let editor = settings::load(&settings::settings_path());
    // The folder comes from the daemon config (a blocking request), so it runs off the runtime.
    let daemon = Arc::clone(&*client);
    let dir_settings = settings.clone();
    let dir = blocking(move || {
        let dir = output_dir(&dir_settings, Some(&*daemon));
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Could not create the export folder {}: {e}", dir.display()))?;
        Ok(dir)
    })
    .await?;
    let stem = expand_name(
        &settings.name_template,
        &project.name,
        local_now(),
        settings.width,
        settings.height,
        &settings.preset,
        0,
    );
    let flags = JobFlags {
        pause_while_recording: editor.pause_while_recording,
        low_priority: editor.low_priority,
        open_folder: editor.open_folder_when_done,
    };
    let manager = &state.exports;

    let mut inner = lock(&manager.inner);
    // Names of unfinished jobs are reserved too: two queued jobs must not share one file.
    let reserved: Vec<PathBuf> = inner
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.dto.state,
                JobState::Queued | JobState::Running | JobState::Paused
            )
        })
        .map(|e| PathBuf::from(&e.dto.out_path))
        .collect();
    let out = unique_path_avoiding(&dir, &stem, out_ext(&settings.kind), &reserved);
    inner.next_seq = inner.next_seq.saturating_add(1);
    let id = format!("export-{}", inner.next_seq);
    let dto = JobDto {
        id: id.clone(),
        project_name: project.name.clone(),
        state: JobState::Queued,
        percent: 0.0,
        elapsed_s: 0.0,
        eta_s: None,
        speed_x: 0.0,
        out_path: out.to_string_lossy().into_owned(),
        bytes: 0,
        paused_reason: None,
        error: None,
        warnings: Vec::new(),
        encoder: None,
        recording_id: None,
    };
    let work_dir = paths::editor_root(std::env::var("LOCALAPPDATA").ok().as_deref())
        .join("export-work")
        .join(&id);
    let saved_settings = settings.clone();
    inner.entries.push(JobEntry {
        dto: dto.clone(),
        project: Some(project),
        settings,
        flags,
        ctl: JobControl::new(),
        override_pause: false,
        started: None,
        paused_since: None,
        paused_total: Duration::ZERO,
        work_dir,
    });
    if !inner.worker_running {
        inner.worker_running = true;
        let daemon = Arc::clone(&*client);
        if let Err(e) = start_worker(app.clone(), daemon) {
            inner.worker_running = false;
            inner.entries.pop();
            return Err(e);
        }
    }
    let list = publish(manager, &inner);
    drop(inner);
    // Remember the settings in the project for the next dialog. Assigned directly: no undo
    // entry and no dirty flag.
    if let Some(session) = lock(&state.session).as_mut() {
        session.project.export_defaults = saved_settings;
    }
    emit_queue(&app, list);
    Ok(dto)
}

/// Cancels a job (running: within 500 ms; queued: removes it). Partial files are removed by the
/// engine. Finished jobs are left alone.
#[tauri::command]
pub fn editor_export_cancel(
    app: AppHandle,
    state: State<'_, EditorState>,
    job_id: String,
) -> Result<(), String> {
    let manager = &state.exports;
    let list = {
        let mut inner = lock(&manager.inner);
        let job_state = entry_mut(&mut inner, &job_id)
            .map(|e| e.dto.state)
            .ok_or_else(|| "No such job".to_string())?;
        match job_state {
            JobState::Queued => inner.entries.retain(|e| e.dto.id != job_id),
            JobState::Running | JobState::Paused => {
                if let Some(entry) = entry_mut(&mut inner, &job_id) {
                    entry.ctl.cancel();
                }
            }
            JobState::Done | JobState::Failed | JobState::Cancelled => {}
        }
        publish(manager, &inner)
    };
    emit_queue(&app, list);
    Ok(())
}

/// User pause/resume of a job. Resuming a job that was paused for a recording counts as
/// "export anyway" for that job.
#[tauri::command]
pub fn editor_export_pause(
    app: AppHandle,
    state: State<'_, EditorState>,
    job_id: String,
    paused: bool,
) -> Result<(), String> {
    let manager = &state.exports;
    let list = {
        let mut inner = lock(&manager.inner);
        let entry = entry_mut(&mut inner, &job_id).ok_or_else(|| "No such job".to_string())?;
        if !matches!(entry.dto.state, JobState::Running | JobState::Paused) {
            return Err("Only a running job can be paused".to_string());
        }
        let now = Instant::now();
        if paused {
            set_pause(entry, Some("user"), now);
        } else {
            let reason = entry.dto.paused_reason.clone();
            match reason.as_deref() {
                Some("user") => set_pause(entry, None, now),
                Some("recording") => {
                    entry.override_pause = true;
                    set_pause(entry, None, now);
                }
                _ => {}
            }
        }
        publish(manager, &inner)
    };
    emit_queue(&app, list);
    Ok(())
}

/// "Export anyway": ignore the pause-while-recording rule for a job.
#[tauri::command]
pub fn editor_export_pause_override(
    app: AppHandle,
    state: State<'_, EditorState>,
    job_id: String,
) -> Result<(), String> {
    let manager = &state.exports;
    let list = {
        let mut inner = lock(&manager.inner);
        let entry = entry_mut(&mut inner, &job_id).ok_or_else(|| "No such job".to_string())?;
        entry.override_pause = true;
        if entry.dto.paused_reason.as_deref() == Some("recording") {
            set_pause(entry, None, Instant::now());
        }
        publish(manager, &inner)
    };
    emit_queue(&app, list);
    Ok(())
}

/// Current queue.
#[tauri::command]
pub fn editor_export_queue(state: State<'_, EditorState>) -> Result<Vec<JobDto>, String> {
    Ok(lock(&state.exports.jobs).clone())
}

/// Removes a finished/failed/cancelled job from the list.
#[tauri::command]
pub fn editor_export_dismiss(
    app: AppHandle,
    state: State<'_, EditorState>,
    job_id: String,
) -> Result<(), String> {
    let manager = &state.exports;
    let list = {
        let mut inner = lock(&manager.inner);
        let job_state = entry_mut(&mut inner, &job_id)
            .map(|e| e.dto.state)
            .ok_or_else(|| "No such job".to_string())?;
        match job_state {
            JobState::Done | JobState::Failed | JobState::Cancelled => {
                inner.entries.retain(|e| e.dto.id != job_id);
            }
            _ => return Err("Only a finished job can be removed from the list".to_string()),
        }
        publish(manager, &inner)
    };
    emit_queue(&app, list);
    Ok(())
}

/// Moves a queued job to `index` in the queue. The index counts queued jobs only (index among
/// QUEUED jobs, measured after removing the moved job), so a running job is never moved and
/// never counts. An index past the last queued job moves the job to the end.
#[tauri::command]
pub fn editor_export_reorder(
    app: AppHandle,
    state: State<'_, EditorState>,
    job_id: String,
    index: u32,
) -> Result<(), String> {
    let manager = &state.exports;
    let list = {
        let mut inner = lock(&manager.inner);
        let from = inner
            .entries
            .iter()
            .position(|e| e.dto.id == job_id)
            .ok_or_else(|| "No such job".to_string())?;
        let is_queued = inner
            .entries
            .get(from)
            .is_some_and(|e| e.dto.state == JobState::Queued);
        if !is_queued {
            return Err("Only a queued job can be moved".to_string());
        }
        let job = inner.entries.remove(from);
        let slots: Vec<usize> = inner
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.dto.state == JobState::Queued)
            .map(|(i, _)| i)
            .collect();
        let k = (index as usize).min(slots.len());
        let pos = slots.get(k).copied().unwrap_or(inner.entries.len());
        inner.entries.insert(pos, job);
        publish(manager, &inner)
    };
    emit_queue(&app, list);
    Ok(())
}

/// Exports one still frame of the open project at `at` right away (PNG/JPG); returns the file
/// path. Not queued, not registered in the library.
///
/// The folder is `dir` when given and non-empty, else the last export folder (unless the user
/// is asked for a location every time), else the default export folder. The name uses
/// `name_template` when given and non-empty, else the template from the editor settings.
#[tauri::command]
pub async fn editor_export_still(
    state: State<'_, EditorState>,
    client: State<'_, Arc<DaemonClient>>,
    at: Tick,
    format: String,
    quality: u32,
    dir: Option<String>,
    name_template: Option<String>,
) -> Result<String, String> {
    let ext: &'static str = if format.eq_ignore_ascii_case("jpg") {
        "jpg"
    } else if format.eq_ignore_ascii_case("png") {
        "png"
    } else {
        return Err(format!(
            "Still image format must be png or jpg (got {format})"
        ));
    };
    let project = project_snapshot(&state)?;
    let daemon = Arc::clone(&*client);
    blocking(move || {
        let ed = settings::load(&settings::settings_path());
        let requested_dir = dir.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let last_dir = ed.last_export_dir.trim();
        let dir = match requested_dir {
            Some(d) => PathBuf::from(expand_vars(d)),
            None if !last_dir.is_empty() && !ed.always_ask_location => {
                PathBuf::from(expand_vars(last_dir))
            }
            None => default_export_dir(Some(&*daemon)),
        };
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Could not create the export folder {}: {e}", dir.display()))?;
        let frame = still::render_frame(&project, at).map_err(|e| e.to_string())?;
        let template = name_template
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .map_or_else(|| ed.filename_template.clone(), str::to_string);
        let stem = expand_name(
            &template,
            &project.name,
            local_now(),
            project.canvas.width,
            project.canvas.height,
            "still",
            0,
        );
        let out = unique_path(&dir, &stem, ext);
        still::write_image(&frame, &out, ext, quality.clamp(1, 100)).map_err(|e| e.to_string())?;
        Ok(out.to_string_lossy().into_owned())
    })
    .await
}

/// Saves a user preset (replacing the same id) in the editor settings; returns all user presets.
#[tauri::command]
pub fn editor_save_preset(mut preset: Preset) -> Result<Vec<Preset>, String> {
    preset.id = preset.id.trim().to_string();
    if preset.id.is_empty() {
        return Err("A preset needs an id".to_string());
    }
    if builtin_presets().iter().any(|p| p.id == preset.id) {
        return Err(format!("\"{}\" is the id of a built-in preset", preset.id));
    }
    preset.builtin = false;

    let path = settings::settings_path();
    let mut s = settings::load(&path);
    match s.custom_presets.iter_mut().find(|p| p.id == preset.id) {
        Some(slot) => *slot = preset,
        None => {
            if s.custom_presets.len() >= MAX_USER_PRESETS {
                return Err(format!(
                    "At most {MAX_USER_PRESETS} user presets can be saved"
                ));
            }
            s.custom_presets.push(preset);
        }
    }
    settings::normalize(&mut s);
    settings::save(&path, &s)?;
    Ok(s.custom_presets)
}

/// Deletes a user preset by id; returns the remaining user presets.
#[tauri::command]
pub fn editor_delete_preset(id: String) -> Result<Vec<Preset>, String> {
    let path = settings::settings_path();
    let mut s = settings::load(&path);
    s.custom_presets.retain(|p| p.id != id);
    settings::save(&path, &s)?;
    Ok(s.custom_presets)
}

/// File extensions `editor_open_export` is willing to open (lower case).
const OPENABLE_EXTS: [&str; 8] = ["mp4", "mov", "mkv", "webm", "m4a", "png", "jpg", "jpeg"];

/// Win32 `SW_SHOWNORMAL` show command.
const SW_SHOWNORMAL: i32 = 1;

// Declared directly so no extra `windows` crate feature is needed (ShellExecuteW lives behind
// Win32_UI_WindowsAndMessaging, which the crate does not enable).
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: isize,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> isize;
}

/// Drops the Windows verbatim prefix that `canonicalize` adds, so the shell gets a plain path:
/// `\\?\C:\x` becomes `C:\x` and `\\?\UNC\srv\share\x` becomes `\\srv\share\x`. Other paths, and
/// paths that are not valid UTF-8, are returned unchanged.
fn strip_verbatim(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// Opens an exported file in the user's default player (or viewer for stills). Only files that
/// exist, have a video, audio or image extension, and lie inside the export folder (the default
/// one or the last export folder chosen) are opened.
#[tauri::command]
pub async fn editor_open_export(
    client: State<'_, Arc<DaemonClient>>,
    path: String,
) -> Result<(), String> {
    let daemon = Arc::clone(&*client);
    blocking(move || {
        use std::os::windows::ffi::OsStrExt;

        let requested = PathBuf::from(path.trim());
        let file = fs::canonicalize(&requested)
            .map_err(|_| format!("The export file is missing: {}", requested.display()))?;
        if !file.is_file() {
            return Err(format!("The export file is missing: {}", file.display()));
        }
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !OPENABLE_EXTS.contains(&ext.as_str()) {
            return Err("Only video, audio and image exports can be opened".to_string());
        }

        let ed = settings::load(&settings::settings_path());
        let last_dir = ed.last_export_dir.trim();
        let mut roots = vec![default_export_dir(Some(&*daemon))];
        if !last_dir.is_empty() {
            roots.push(PathBuf::from(expand_vars(last_dir)));
        }
        let inside = roots
            .iter()
            .filter_map(|root| fs::canonicalize(root).ok())
            .any(|root| file.starts_with(&root));
        if !inside {
            return Err("That file is not in the export folder".to_string());
        }

        let op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
        // The containment check above uses the canonical path; the shell gets the plain form.
        let shell_path = strip_verbatim(&file);
        let wide: Vec<u16> = shell_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that outlives the call; null
        // parameters and directory are allowed for the "open" verb.
        let code = unsafe {
            ShellExecuteW(
                0,
                op.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if code > 32 {
            Ok(())
        } else {
            Err(format!("Windows could not open the file (code {code})"))
        }
    })
    .await
}

/// One encoder for the Export dialog's Encoder dropdown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderInfo {
    /// Friendly name, e.g. "NVIDIA H.264 Encoder MFT". The render engine accepts it as the
    /// `encoder` setting.
    pub name: String,
    /// True for a GPU/ASIC encoder, false for the Microsoft software encoder.
    pub hardware: bool,
}

/// Lists the Media Foundation encoders for `codec` ("h264" or "hevc", case-insensitive), hardware
/// first. Other codecs (such as "av1") give an empty list.
#[tauri::command]
pub async fn editor_list_encoders(codec: String) -> Result<Vec<EncoderInfo>, String> {
    let codec = match codec.trim().to_ascii_lowercase().as_str() {
        "h264" => Codec::H264,
        "hevc" => Codec::Hevc,
        _ => return Ok(Vec::new()),
    };
    blocking(move || {
        clipcheck_hwenc::mf_startup().map_err(|e| e.to_string())?;
        let list = clipcheck_hwenc::enumerate(codec).map_err(|e| e.to_string())?;
        Ok(list
            .into_iter()
            .map(|desc| EncoderInfo {
                name: desc.name,
                hardware: desc.hardware,
            })
            .collect())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefix_is_stripped_for_the_shell() {
        assert_eq!(
            strip_verbatim(Path::new("\\\\?\\C:\\Videos\\a.mp4")),
            PathBuf::from("C:\\Videos\\a.mp4")
        );
        assert_eq!(
            strip_verbatim(Path::new("\\\\?\\UNC\\server\\share\\a.mp4")),
            PathBuf::from("\\\\server\\share\\a.mp4")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"C:\plain.mp4")),
            PathBuf::from(r"C:\plain.mp4")
        );
    }
}
