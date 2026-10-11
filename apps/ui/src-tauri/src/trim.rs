//! Trim backend (D27, wired in D60). Contract: docs/impl/player-v2.md and docs/impl/features-f1.md.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use clipcheck_clipedit::{
    plan_trim, run_trim, suggest_output_name, TrimInput, TrimMode, TrimPlan, TrimProgress,
    TrimRequest, TrimRunError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::ipc::DaemonClient;

const NEEDS_MP4: &str = "Trimming needs an MP4 recording";
/// Shortest selection that can be trimmed.
pub const MIN_TRIM_MS: i64 = 100;

/// Trim mode, as sent by the UI.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrimModeDto {
    Lossless,
    Precise,
}

/// Request body of `trim_clip` (mirrors `TrimRequest` in trimapi.ts).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimRequestDto {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub mode: TrimModeDto,
    pub overwrite: bool,
    /// The new clip takes the source's date (and favourite flag): used by "Save clip".
    #[serde(default)]
    pub keep_timestamp: bool,
}

/// Result of `trim_clip`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrimStartedDto {
    pub job_id: String,
}

/// Result of `get_trim_preview`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrimPreviewDto {
    pub actual_start_ms: i64,
    pub actual_end_ms: i64,
    pub snap_delta_ms: i64,
}

/// Payload of the `trim-progress` event.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrimProgressEvent {
    pub job_id: String,
    pub percent: f32,
    pub out_time_ms: i64,
}

/// Payload of the `trim-done` event.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrimDoneEvent {
    pub job_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

type ActiveJob = Option<(String, Arc<AtomicBool>)>;

/// The single running job: id and its cancel flag.
static ACTIVE: Mutex<ActiveJob> = Mutex::new(None);
static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);

fn active() -> std::sync::MutexGuard<'static, ActiveJob> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_job_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("trim-{ms}-{}", JOB_COUNTER.fetch_add(1, Ordering::SeqCst))
}

fn to_mode(m: TrimModeDto) -> TrimMode {
    match m {
        TrimModeDto::Lossless => TrimMode::Lossless,
        TrimModeDto::Precise => TrimMode::Precise,
    }
}

/// True when the path has an MP4-family extension.
pub fn is_mp4_path(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "mp4" | "m4v" | "mov"))
}

/// Video keyframe presentation times in ms (sorted, deduplicated) plus the movie duration in ms.
pub fn keyframes_and_duration(path: &Path) -> Result<(Vec<i64>, i64), String> {
    if !is_mp4_path(path) {
        return Err(NEEDS_MP4.into());
    }
    let movie = clipcheck_mp4::reader::read_movie(path).map_err(|_| NEEDS_MP4.to_string())?;
    let track = movie
        .tracks
        .iter()
        .find(|t| t.is_video())
        .ok_or_else(|| NEEDS_MP4.to_string())?;
    let kfs = keyframes_ms(
        track.timescale,
        track.edit_media_time,
        track
            .samples
            .iter()
            .map(|s| (s.dts, s.cts_offset, s.keyframe)),
    );
    let duration = (movie.duration_secs * 1000.0).round() as i64;
    Ok((kfs, duration))
}

/// Pure: keyframe presentation times in ms from `(dts, cts_offset, keyframe)` samples.
pub fn keyframes_ms(
    timescale: u32,
    edit_media_time: i64,
    samples: impl Iterator<Item = (i64, i32, bool)>,
) -> Vec<i64> {
    if timescale == 0 {
        return Vec::new();
    }
    let shift = edit_media_time.max(0);
    let mut out: Vec<i64> = samples
        .filter(|s| s.2)
        .map(|(dts, cts, _)| {
            let pts = dts + i64::from(cts) - shift;
            (pts.max(0) as f64 * 1000.0 / f64::from(timescale)).round() as i64
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Pure: validates a selection against the duration.
pub fn validate_range(start_ms: i64, end_ms: i64, duration_ms: i64) -> Result<(), String> {
    if start_ms < 0 || start_ms >= end_ms || end_ms > duration_ms {
        return Err("Invalid trim range".into());
    }
    if end_ms - start_ms < MIN_TRIM_MS {
        return Err("The selection is too short to trim".into());
    }
    Ok(())
}

/// `dir/name`, or with " (2)", " (3)"... before the extension until unused.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(first)
}

fn source_duration_ms(db: &Path, id: &str, from_movie: i64) -> i64 {
    let row = clipcheck_library::Library::open_readonly(db)
        .ok()
        .and_then(|lib| lib.get_recording(id).ok().flatten())
        .map_or(0, |r| r.duration_ms);
    if row > 0 {
        row
    } else {
        from_movie
    }
}

struct Prepared {
    src: PathBuf,
    dst: PathBuf,
    plan: TrimPlan,
}

fn plan_for(
    src: &Path,
    db: &Path,
    id: &str,
    start_ms: i64,
    end_ms: i64,
    mode: TrimModeDto,
) -> Result<TrimPlan, String> {
    let (kfs, movie_ms) = keyframes_and_duration(src)?;
    let duration_ms = source_duration_ms(db, id, movie_ms);
    validate_range(start_ms, end_ms, duration_ms)?;
    plan_trim(&TrimInput {
        keyframes_ms: &kfs,
        duration_ms,
        start_ms,
        end_ms,
        mode: to_mode(mode),
    })
    .map_err(|e| e.to_string())
}

fn resolve(id: &str) -> Result<(PathBuf, PathBuf), String> {
    let db = crate::commands::try_default_db_path().map_err(|e| e.to_string())?;
    let src = crate::media::resolve_recording_path(&db, id).map_err(|e| format!("{e:?}"))?;
    Ok((db, src))
}

fn prepare(req: &TrimRequestDto) -> Result<Prepared, String> {
    if req.overwrite {
        return Err("Overwriting the original is not supported".into());
    }
    let (db, src) = resolve(&req.id)?;
    let plan = plan_for(&src, &db, &req.id, req.start_ms, req.end_ms, req.mode)?;
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip.mp4".into());
    let dir = src.parent().ok_or("Recording has no folder")?;
    // The planner returns the name without an extension; the output keeps the source container.
    let ext = src.extension().map_or_else(
        || "mp4".to_string(),
        |e| e.to_string_lossy().to_ascii_lowercase(),
    );
    let file = format!(
        "{}.{ext}",
        suggest_output_name(&name, req.start_ms, req.end_ms)
    );
    let dst = unique_path(dir, &file);
    Ok(Prepared { src, dst, plan })
}

fn register(
    client: &DaemonClient,
    source_id: &str,
    dst: &Path,
    keep_source_time: bool,
) -> Result<String, String> {
    let cmd = json!({
        "type": "register_edited_recording",
        "data": {
            "source_recording_id": source_id,
            "path": dst.to_string_lossy(),
            "keep_source_time": keep_source_time,
        }
    });
    let reply = client
        .request_command(cmd)
        .map_err(|e| format!("Could not register the new clip: {e}"))?;
    parse_registered(&reply)
}

/// Pure: new recording id from a `recording_registered` reply, or the daemon error message.
pub fn parse_registered(reply: &Value) -> Result<String, String> {
    let data = reply.get("data");
    match reply.get("type").and_then(Value::as_str) {
        Some("recording_registered") => data
            .and_then(|d| d.get("recording_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "Daemon reply had no recording id".into()),
        Some("error") => Err(data
            .and_then(|d| d.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("The daemon rejected the new clip")
            .to_owned()),
        _ => Err("Unexpected daemon reply".into()),
    }
}

fn run_job(
    app: &AppHandle,
    client: &DaemonClient,
    job_id: &str,
    source_id: &str,
    p: &Prepared,
    cancel: &AtomicBool,
    keep_source_time: bool,
) -> Result<String, String> {
    let treq = TrimRequest {
        src: &p.src,
        dst: &p.dst,
        plan: &p.plan,
    };
    let total = p.plan.output_duration_ms().max(1);
    let mut on_progress = |pr: TrimProgress| {
        let percent = ((pr.out_time_ms as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32;
        let _ = app.emit(
            "trim-progress",
            TrimProgressEvent {
                job_id: job_id.to_string(),
                percent,
                out_time_ms: pr.out_time_ms,
            },
        );
    };
    match run_trim(&treq, cancel, &mut on_progress) {
        Ok(()) => {}
        Err(TrimRunError::Cancelled) => return Err("Cancelled".into()),
        Err(e) => return Err(e.to_string()),
    }
    register(client, source_id, &p.dst, keep_source_time)
}

/// Tauri command: starts a trim job.
#[tauri::command]
pub async fn trim_clip(
    app: AppHandle,
    client: State<'_, Arc<DaemonClient>>,
    req: TrimRequestDto,
) -> Result<TrimStartedDto, String> {
    if active().is_some() {
        return Err("A trim is already running".into());
    }
    let req2 = req.clone();
    let prepared = tauri::async_runtime::spawn_blocking(move || prepare(&req2))
        .await
        .map_err(|e| e.to_string())??;
    let job_id = new_job_id();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = active();
        if slot.is_some() {
            return Err("A trim is already running".into());
        }
        *slot = Some((job_id.clone(), Arc::clone(&cancel)));
    }
    let client = Arc::clone(&client);
    let jid = job_id.clone();
    let spawned = std::thread::Builder::new()
        .name("trim-job".into())
        .spawn(move || {
            let result = run_job(
                &app,
                &client,
                &jid,
                &req.id,
                &prepared,
                &cancel,
                req.keep_timestamp,
            );
            *active() = None;
            let done = match result {
                Ok(new_id) => TrimDoneEvent {
                    job_id: jid,
                    ok: true,
                    new_id: Some(new_id),
                    error: None,
                },
                Err(error) => TrimDoneEvent {
                    job_id: jid,
                    ok: false,
                    new_id: None,
                    error: Some(error),
                },
            };
            let _ = app.emit("trim-done", done);
        });
    if let Err(e) = spawned {
        *active() = None;
        return Err(format!("Could not start the trim: {e}"));
    }
    Ok(TrimStartedDto { job_id })
}

/// Tauri command: cancels a running trim job. Ok even when nothing runs.
#[tauri::command]
pub fn cancel_trim(job_id: String) -> Result<(), String> {
    if let Some((id, flag)) = active().as_ref() {
        if *id == job_id {
            flag.store(true, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// Tauri command: the actual output range for a requested selection.
#[tauri::command]
pub async fn get_trim_preview(
    id: String,
    start_ms: i64,
    end_ms: i64,
    mode: TrimModeDto,
) -> Result<TrimPreviewDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (db, src) = resolve(&id)?;
        let plan = plan_for(&src, &db, &id, start_ms, end_ms, mode)?;
        Ok(TrimPreviewDto {
            actual_start_ms: plan.actual_start_ms,
            actual_end_ms: plan.actual_end_ms,
            snap_delta_ms: plan.snap_delta_ms,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
