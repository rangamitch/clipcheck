//! Whole-app cache: the player's audio sidecars, the editor cache, and old recording-buffer runs.
//! Owner: cache maintenance.
//!
//! The player cache root is `player::cache_dir_from` (the `CLIPCHECK_CACHE_DIR` override or
//! `%LOCALAPPDATA%\ClipCheck\cache`). Its `audio` folder holds the sidecars written by
//! `player::ensure_sidecar`. Old builds wrote sidecars that were far too large, under the plain
//! `track{n}.m4a` name; those are deleted here, and the player never reuses them. Symlinks and
//! reparse points are never followed or deleted.
//!
//! The recording buffer (`replay.location`, default `%LOCALAPPDATA%\ClipCheck\buffer`) holds one
//! `run-<nanos>` folder per session. Runs left by crashed sessions are removed by
//! [`clear_stale_buffer`] when the user clears the cache. The live run is never stale because it
//! is still being written. Background maintenance leaves the buffer to the daemon's own sweep.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::editor::cache as editor_cache;
use crate::editor::settings;
use crate::editor::EditorState;
use crate::ipc::DaemonClient;

/// Interval between two background maintenance passes.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);

/// Age after which an in-progress (`.part`) sidecar is considered abandoned.
const PARTIAL_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// A buffer run is stale once nothing inside it has changed for this long.
pub const BUFFER_IDLE: Duration = Duration::from_secs(30 * 60);

/// Most entries (files and folders) counted while sizing one buffer run. Larger runs are kept.
const BUFFER_WALK_LIMIT: usize = 20_000;

/// Sizes of the whole app cache, as the Settings page shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppCacheInfo {
    /// Player cache root folder.
    pub dir: String,
    /// Bytes under the player cache root (its `audio` sidecars).
    pub player_bytes: u64,
    /// Bytes under the editor cache root. 0 when it is the same folder as the player cache.
    pub editor_bytes: u64,
    /// Old recording-buffer folders left by crashed sessions; the live buffer is never counted.
    pub buffer_bytes: u64,
    /// Sum of the three.
    pub total_bytes: u64,
}

/// Player cache root: the `CLIPCHECK_CACHE_DIR` override, else `%LOCALAPPDATA%\ClipCheck\cache`.
pub fn player_cache_root() -> PathBuf {
    crate::player::cache_dir_from(
        std::env::var("CLIPCHECK_CACHE_DIR").ok().as_deref(),
        std::env::var("LOCALAPPDATA").ok().as_deref(),
    )
}

/// Editor cache root, from the current editor settings.
fn editor_cache_root() -> PathBuf {
    editor_cache::cache_root(&settings::load(&settings::settings_path()))
}

/// Expands `%VAR%` pairs. `LOCALAPPDATA` comes from `local_app_data`, other names from the
/// environment. Unknown names and an unmatched `%` are kept as typed.
fn expand_location(input: &str, local_app_data: Option<&str>) -> String {
    let lookup = |name: &str| -> Option<String> {
        if name.eq_ignore_ascii_case("LOCALAPPDATA") {
            local_app_data.map(str::to_string)
        } else {
            std::env::var(name).ok()
        }
    };
    let segments: Vec<&str> = input.split('%').collect();
    let mut out = String::with_capacity(input.len());
    for (i, segment) in segments.iter().enumerate() {
        // Even segments are literal text; odd ones sit between a pair of `%` signs.
        if i % 2 == 0 {
            out.push_str(segment);
            continue;
        }
        let closed = i + 1 < segments.len();
        match lookup(segment) {
            Some(value) if closed && !segment.is_empty() => out.push_str(&value),
            _ => {
                out.push('%');
                out.push_str(segment);
                if closed {
                    out.push('%');
                }
            }
        }
    }
    out
}

/// Default buffer folder: `<LOCALAPPDATA>\ClipCheck\buffer`, or under the temp dir when
/// `LOCALAPPDATA` is unset.
fn default_buffer_dir(local_app_data: Option<&str>) -> PathBuf {
    match local_app_data.filter(|s| !s.trim().is_empty()) {
        Some(base) => PathBuf::from(base).join("ClipCheck").join("buffer"),
        None => std::env::temp_dir().join("ClipCheck").join("buffer"),
    }
}

/// Buffer folder for a `replay.location` value (None or blank means the default folder).
/// `%VAR%` pairs in the location are expanded.
pub fn buffer_dir_from(location: Option<&str>, local_app_data: Option<&str>) -> PathBuf {
    match location.map(str::trim).filter(|s| !s.is_empty()) {
        Some(loc) => PathBuf::from(expand_location(loc, local_app_data)),
        None => default_buffer_dir(local_app_data),
    }
}

/// Pulls `replay.location` out of a `get_config` reply. None when the reply is not a config reply.
fn buffer_location(body: &Value) -> Option<String> {
    if body.get("type").and_then(Value::as_str) != Some("config") {
        return None;
    }
    body.pointer("/data/config/replay/location")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The recording buffer folder, from the daemon's `replay.location` (asked with `client`). Without
/// a daemon or a usable reply, the default folder is used.
pub fn buffer_dir(client: Option<&DaemonClient>) -> PathBuf {
    let location = client
        .and_then(|c| c.request_command(json!({ "type": "get_config" })).ok())
        .and_then(|body| buffer_location(&body));
    buffer_dir_from(
        location.as_deref(),
        std::env::var("LOCALAPPDATA").ok().as_deref(),
    )
}

/// True when both paths name the same folder (compared case-insensitively, as Windows does).
fn same_root(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Sizes for the player root, the editor root and the stale buffer runs. The editor root is
/// counted only when it is a different folder.
fn build_info(player: &Path, editor: &Path, buffer_bytes: u64) -> AppCacheInfo {
    let player_bytes = editor_cache::info_at(player).total_bytes;
    let editor_bytes = if same_root(player, editor) {
        0
    } else {
        editor_cache::info_at(editor).total_bytes
    };
    AppCacheInfo {
        dir: player.to_string_lossy().into_owned(),
        player_bytes,
        editor_bytes,
        buffer_bytes,
        total_bytes: player_bytes
            .saturating_add(editor_bytes)
            .saturating_add(buffer_bytes),
    }
}

/// Subfolders of `dir` that are real folders (links skipped). Empty when `dir` is missing.
fn real_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|path| {
            fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.is_dir() && !editor_cache::is_link(&meta))
        })
        .collect()
}

/// Deletes the legacy sidecars and stale `.part` files directly inside one `<id>\<stamp>` folder.
/// Returns how many files were deleted.
fn prune_stamp_dir(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut deleted = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if editor_cache::is_link(&meta) || !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = if name.contains(".part") {
            meta.modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= PARTIAL_MAX_AGE)
        } else {
            name.ends_with(".m4a") && !name.ends_with(".v2.m4a")
        };
        if stale && fs::remove_file(&path).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// Deletes legacy sidecars (`.m4a` without the `.v2` marker) and `.part` files older than an
/// hour, under `root\audio\<id>\<stamp>`. Then removes the folders left empty. Best effort.
/// Returns how many files were deleted.
pub fn prune_legacy_sidecars(root: &Path) -> usize {
    let mut deleted = 0;
    for id_dir in real_dirs(&root.join("audio")) {
        for stamp_dir in real_dirs(&id_dir) {
            deleted += prune_stamp_dir(&stamp_dir);
        }
    }
    editor_cache::remove_empty_dirs(root);
    deleted
}

/// Total bytes and newest modification time under one buffer run, the run folder included.
/// Links are never followed and their targets are not counted. None when the walk is unreadable,
/// has an entry without a modification time, or passes [`BUFFER_WALK_LIMIT`] entries.
fn scan_run(run: &Path) -> Option<(SystemTime, u64)> {
    let mut newest = fs::symlink_metadata(run).ok()?.modified().ok()?;
    let mut bytes = 0u64;
    let mut entries_seen = 0usize;
    let mut pending = vec![run.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).ok()? {
            let entry = entry.ok()?;
            entries_seen += 1;
            if entries_seen > BUFFER_WALK_LIMIT {
                return None;
            }
            // DirEntry::metadata does not follow links.
            let meta = entry.metadata().ok()?;
            newest = newest.max(meta.modified().ok()?);
            if editor_cache::is_link(&meta) {
                continue;
            }
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    Some((newest, bytes))
}

/// The `run-*` folders directly inside `buffer` that have been idle for at least [`BUFFER_IDLE`]
/// at `now`, each with its total bytes. Links, files, other folders and runs that cannot be
/// fully read are never returned.
pub fn stale_buffer_runs(buffer: &Path, now: SystemTime) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(buffer) else {
        return Vec::new();
    };
    let mut stale = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("run-") {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if editor_cache::is_link(&meta) || !meta.is_dir() {
            continue;
        }
        let Some((newest, bytes)) = scan_run(&path) else {
            continue;
        };
        let idle = now
            .duration_since(newest)
            .is_ok_and(|age| age >= BUFFER_IDLE);
        if idle {
            stale.push((path, bytes));
        }
    }
    stale
}

/// Total bytes of the stale buffer runs at the current time.
fn stale_buffer_bytes(buffer: &Path) -> u64 {
    stale_buffer_runs(buffer, SystemTime::now())
        .iter()
        .map(|(_, bytes)| *bytes)
        .fold(0u64, u64::saturating_add)
}

/// Removes the stale buffer runs (see [`stale_buffer_runs`]). Best effort. Returns the bytes
/// freed by the runs that were removed.
pub fn clear_stale_buffer(buffer: &Path, now: SystemTime) -> u64 {
    let mut freed = 0u64;
    for (path, bytes) in stale_buffer_runs(buffer, now) {
        if fs::remove_dir_all(&path).is_ok() {
            freed = freed.saturating_add(bytes);
        }
    }
    freed
}

/// One background maintenance pass: prune the player's legacy sidecars, then enforce the editor's
/// `cache_max_gb` limit on the player cache. The recording buffer is left to the daemon's sweep.
pub fn maintain_cache() {
    let root = player_cache_root();
    prune_legacy_sidecars(&root);
    let settings = settings::load(&settings::settings_path());
    editor_cache::enforce_limit(&root, editor_cache::limit_bytes(settings.cache_max_gb));
}

/// Starts the maintenance thread (named `clipcheck-cache-maint`). It runs one pass at once and then
/// one every 10 minutes for the life of the app. Call once at startup.
pub fn spawn_maintenance() {
    let spawned = std::thread::Builder::new()
        .name("clipcheck-cache-maint".into())
        .spawn(|| loop {
            maintain_cache();
            std::thread::sleep(MAINTENANCE_INTERVAL);
        });
    if let Err(e) = spawned {
        eprintln!("app cache: cannot start maintenance: {e}");
    }
}

/// Tauri: sizes of the player and editor caches and of the stale buffer runs.
#[tauri::command]
pub async fn app_cache_info(client: State<'_, Arc<DaemonClient>>) -> Result<AppCacheInfo, String> {
    let daemon = Arc::clone(&*client);
    tauri::async_runtime::spawn_blocking(move || {
        let buffer_bytes = stale_buffer_bytes(&buffer_dir(Some(&*daemon)));
        build_info(&player_cache_root(), &editor_cache_root(), buffer_bytes)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Tauri: deletes the player and editor caches (thumbs, peaks, audio subfolders only) and the
/// stale recording-buffer runs, then returns the fresh sizes. Refused while an export is queued
/// or running.
#[tauri::command]
pub async fn app_clear_cache(
    state: State<'_, EditorState>,
    client: State<'_, Arc<DaemonClient>>,
) -> Result<AppCacheInfo, String> {
    if editor_cache::export_busy(&state.exports) {
        return Err(
            "An export is queued or running. Clear the cache when the exports have finished."
                .to_string(),
        );
    }
    let daemon = Arc::clone(&*client);
    tauri::async_runtime::spawn_blocking(move || {
        let player = player_cache_root();
        let editor = editor_cache_root();
        editor_cache::clear_at(&player);
        if !same_root(&player, &editor) {
            editor_cache::clear_at(&editor);
        }
        let buffer = buffer_dir(Some(&*daemon));
        clear_stale_buffer(&buffer, SystemTime::now());
        build_info(&player, &editor, stale_buffer_bytes(&buffer))
    })
    .await
    .map_err(|e| e.to_string())
}
