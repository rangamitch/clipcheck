//! In-app updates (doc 11, sections 6 and 8), on top of the Tauri updater plugin.
//!
//! The UI process makes the update requests. The daemon never touches the network.
//! Flow of [`install_update`]: re-check the manifest, refuse while a session is recording,
//! download and verify the signed installer (the old version stays untouched on failure),
//! ask the daemon to stop with `shutdown` in mode `update` (a `busy` reply refuses the
//! update), then hand over to the passive NSIS installer, which exits this process.
//!
//! Commands: [`check_for_update`], [`install_update`], [`skip_update_version`],
//! [`get_update_state`]. Events: `update-progress` and `update-available` (payloads are
//! documented on each item).
//!
//! Endpoint: `updates.endpoint` when set, else [`DEFAULT_UPDATE_ENDPOINT`]. Plain http is
//! accepted only for 127.0.0.1 and localhost, and only in debug builds, because the
//! plugin rejects non-https endpoints in release builds.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use url::Url;

use crate::cli::{
    classify_shutdown_reply, forced_update_shutdown_command, update_shutdown_command, ShutdownReply,
};
use crate::closing::session_active_now;
use crate::ipc::{timeout_for, DaemonClient};
use crate::update_state::{self, PendingUpdate, UpdateState};

/// Manifest URL used when `updates.endpoint` is empty.
///
/// The repository owner can change this constant. Forks and tests override it through
/// `updates.endpoint` in config.toml, or by calling `app.updater_builder().endpoints(..)`.
pub const DEFAULT_UPDATE_ENDPOINT: &str =
    "https://github.com/rangamitch/clipcheck/releases/latest/download/latest.json";

/// Event: download, verification and install progress.
/// Payload: `{"phase": "downloading" | "verifying" | "installing", "done": u64, "total": u64 | null}`.
pub const PROGRESS_EVENT: &str = "update-progress";

/// Event: a background check found a version that is not skipped.
/// Payload: an [`UpdateInfoDto`] with `skipped: false`.
pub const AVAILABLE_EVENT: &str = "update-available";

/// Longest accepted endpoint, in characters (matches the config limit).
const MAX_ENDPOINT_CHARS: usize = 300;
/// Delay between launch and the first background check.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
/// While `updates.check = never`, the loop only re-reads the setting at this interval.
const NEVER_RECHECK_DELAY: Duration = Duration::from_secs(60);
/// Default interval between background checks.
const DEFAULT_INTERVAL_HOURS: u64 = 12;
/// Bounds for `updates.interval_hours`.
const INTERVAL_HOURS_MIN: u64 = 1;
const INTERVAL_HOURS_MAX: u64 = 168;
/// Progress events are sent at most once per this many bytes (and at the end).
const PROGRESS_STEP: u64 = 256 * 1024;
/// Time allowed for the daemon's `shutdown` reply during an update.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether background checks run (`updates.check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    /// No background network access. A manual check still works.
    Never,
    /// Check on launch and then on the configured interval.
    Notify,
}

/// The update settings read from the daemon's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSettings {
    /// `updates.check`.
    pub check: CheckMode,
    /// `updates.interval_hours`, clamped to 1..=168.
    pub interval_hours: u64,
    /// `updates.endpoint`, trimmed. Empty means the built-in default.
    pub endpoint: String,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check: CheckMode::Notify,
            interval_hours: DEFAULT_INTERVAL_HOURS,
            endpoint: String::new(),
        }
    }
}

impl UpdateSettings {
    /// Reads the settings from the `config` object of a `get_config` reply (the `updates`
    /// table). Missing or invalid values fall back to the defaults.
    pub fn from_config(config: &Value) -> Self {
        let updates = config.get("updates");
        let check = match updates.and_then(|u| u.get("check")).and_then(Value::as_str) {
            Some("never") => CheckMode::Never,
            _ => CheckMode::Notify,
        };
        let interval_hours = updates
            .and_then(|u| u.get("interval_hours"))
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_INTERVAL_HOURS)
            .clamp(INTERVAL_HOURS_MIN, INTERVAL_HOURS_MAX);
        let endpoint = updates
            .and_then(|u| u.get("endpoint"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        Self {
            check,
            interval_hours,
            endpoint,
        }
    }

    /// Reads the settings from a `get_config` reply body. `None` when it is not a config reply.
    pub fn from_config_reply(body: &Value) -> Option<Self> {
        if body.get("type").and_then(Value::as_str) != Some("config") {
            return None;
        }
        let config = body.get("data")?.get("config")?;
        Some(Self::from_config(config))
    }
}

/// Checks an endpoint: `https`, or `http` for 127.0.0.1 and localhost in debug builds.
pub fn validate_endpoint(raw: &str) -> Result<Url, String> {
    if raw.chars().count() > MAX_ENDPOINT_CHARS {
        return Err("update endpoint is too long".into());
    }
    let url = Url::parse(raw).map_err(|e| format!("invalid update endpoint: {e}"))?;
    let local_host = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    let allowed = match url.scheme() {
        "https" => true,
        "http" => local_host && cfg!(debug_assertions),
        _ => false,
    };
    if allowed {
        Ok(url)
    } else {
        Err("update endpoint must use https".into())
    }
}

/// The endpoint to use: `configured` when set, else [`DEFAULT_UPDATE_ENDPOINT`]. Validated.
pub fn resolve_endpoint(configured: &str) -> Result<Url, String> {
    let raw = if configured.trim().is_empty() {
        DEFAULT_UPDATE_ENDPOINT
    } else {
        configured.trim()
    };
    validate_endpoint(raw)
}

/// An available update, as sent to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateInfoDto {
    /// Version of the update.
    pub version: String,
    /// Version running now.
    pub current_version: String,
    /// Release notes from the manifest.
    pub notes: String,
    /// Publication date from the manifest (RFC 3339), if present.
    pub pub_date: Option<String>,
    /// True when the user chose to skip this version.
    pub skipped: bool,
}

impl UpdateInfoDto {
    fn from_update(update: &Update, skipped_version: Option<&str>) -> Self {
        Self {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            notes: update.body.clone().unwrap_or_default(),
            pub_date: update
                .raw_json
                .get("pub_date")
                .and_then(Value::as_str)
                .map(str::to_owned),
            skipped: skipped_version == Some(update.version.as_str()),
        }
    }
}

/// Managed state: whether an install is running. Background checks pause meanwhile.
#[derive(Debug, Default)]
pub struct UpdateRuntime {
    installing: AtomicBool,
}

/// Clears [`UpdateRuntime::installing`] when dropped.
struct InstallFlag(Arc<UpdateRuntime>);

impl Drop for InstallFlag {
    fn drop(&mut self) {
        self.0.installing.store(false, Ordering::SeqCst);
    }
}

/// Registers the update state and starts the background check loop. Call from `setup`.
pub fn setup(app: &AppHandle) {
    app.manage(Arc::new(UpdateRuntime::default()));
    let handle = app.clone();
    let _ = thread::Builder::new()
        .name("update-loop".into())
        .spawn(move || update_loop(handle));
}

/// Background loop: first check after [`FIRST_CHECK_DELAY`], then every `interval_hours`.
/// The settings are re-read on every pass. Nothing is requested while `check = never`.
fn update_loop(app: AppHandle) {
    let mut wait = FIRST_CHECK_DELAY;
    loop {
        thread::sleep(wait);
        let settings = read_settings_blocking(&app);
        wait = match settings.check {
            CheckMode::Never => NEVER_RECHECK_DELAY,
            CheckMode::Notify => Duration::from_secs(settings.interval_hours * 3600),
        };
        if settings.check == CheckMode::Never || is_installing(&app) {
            continue;
        }
        match tauri::async_runtime::block_on(run_check(&app, false)) {
            Ok(Some(info)) if !info.skipped => {
                let _ = app.emit(AVAILABLE_EVENT, info);
            }
            Ok(_) => {}
            Err(e) => eprintln!("clipcheck-ui: background update check failed: {e}"),
        }
    }
}

/// Checks for an update.
///
/// With `manual == false` and `updates.check = never`, returns `Ok(None)` without any
/// network request. If the daemon is offline, the default (`notify`) applies.
/// Records `last_checked_unix_ms` after a successful check.
#[tauri::command]
pub async fn check_for_update(
    app: AppHandle,
    manual: bool,
) -> Result<Option<UpdateInfoDto>, String> {
    run_check(&app, manual).await
}

/// Downloads, verifies and installs the update, then starts the installer.
///
/// On success the process exits (the installer takes over), so `Ok` is not normally seen.
/// Errors are short codes or messages:
/// - `"recording"`: a session is recording, or the daemon refused the shutdown. Nothing changed.
///   With `stop_recording` the session is not refused: it is saved and ended right before the
///   installer starts (the download runs while it keeps recording).
/// - `"signature"`: the download did not verify. The old version is intact.
/// - `"no-update"`: the manifest has no newer version.
/// - anything else: a download, install or manifest error.
#[tauri::command]
pub async fn install_update(app: AppHandle, stop_recording: Option<bool>) -> Result<(), String> {
    let runtime = Arc::clone(&*app.state::<Arc<UpdateRuntime>>());
    if runtime.installing.swap(true, Ordering::SeqCst) {
        return Err("already in progress".into());
    }
    let _flag = InstallFlag(runtime);
    install_inner(&app, stop_recording.unwrap_or(false)).await
}

/// Returns the update found by an earlier check (this run or a previous one), if it is still
/// newer than the running version and not skipped. Needs no network access.
#[tauri::command]
pub fn get_pending_update(app: AppHandle) -> Option<UpdateInfoDto> {
    let state = update_state::load();
    let pending = state.pending?;
    let current = app.package_info().version.to_string();
    if !is_newer(&pending.version, &current) {
        return None;
    }
    Some(UpdateInfoDto {
        skipped: state.skipped_version.as_deref() == Some(pending.version.as_str()),
        version: pending.version,
        current_version: current,
        notes: pending.notes,
        pub_date: pending.pub_date,
    })
}

/// True when dotted numeric version `candidate` is newer than `current` (pre-release and
/// build suffixes are ignored; anything unparsable is not newer).
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u64>> {
        let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse().ok()).collect()
    }
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Remembers (or forgets) the newest update a check found.
fn record_pending(found: Option<&UpdateInfoDto>) {
    let pending = found.map(|i| PendingUpdate {
        version: i.version.clone(),
        notes: i.notes.clone(),
        pub_date: i.pub_date.clone(),
    });
    let _ = update_state::update(|s| s.pending = pending);
}

/// Remembers that the user chose to skip `version`. Background checks then stay quiet about it.
#[tauri::command]
pub fn skip_update_version(version: String) -> Result<(), String> {
    let version = version.trim().to_owned();
    if version.is_empty() {
        return Err("version is empty".into());
    }
    update_state::update(|s| s.skipped_version = Some(version))
        .map_err(|e| format!("cannot save update state: {e}"))
}

/// Returns `update-state.json` (defaults when missing or corrupt).
#[tauri::command]
pub fn get_update_state() -> UpdateState {
    update_state::load()
}

async fn run_check(app: &AppHandle, manual: bool) -> Result<Option<UpdateInfoDto>, String> {
    let settings = load_settings(app.clone()).await;
    if !manual && settings.check == CheckMode::Never {
        return Ok(None);
    }
    let endpoint = resolve_endpoint(&settings.endpoint)?;
    let updater = build_updater(app, endpoint)?;
    let found = updater
        .check()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
    record_checked();
    let skipped = update_state::load().skipped_version;
    let info = found.map(|u| UpdateInfoDto::from_update(&u, skipped.as_deref()));
    record_pending(info.as_ref());
    Ok(info)
}

async fn install_inner(app: &AppHandle, stop_recording: bool) -> Result<(), String> {
    let settings = load_settings(app.clone()).await;
    let endpoint = resolve_endpoint(&settings.endpoint)?;
    let updater = build_updater(app, endpoint)?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("update check failed: {e}"))?
        .ok_or_else(|| "no-update".to_owned())?;

    // Read-only check first, so nothing is downloaded while a session records.
    if !stop_recording && session_active_blocking(app).await {
        return Err("recording".into());
    }

    let (done, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let (done_chunks, total_chunks, app_chunks) = (done.clone(), total.clone(), app.clone());
    let mut last_emit = 0u64;
    let on_chunk = move |chunk: usize, content_length: Option<u64>| {
        let now = done_chunks.fetch_add(chunk as u64, Ordering::SeqCst) + chunk as u64;
        if let Some(len) = content_length {
            total_chunks.store(len, Ordering::SeqCst);
        }
        if now - last_emit >= PROGRESS_STEP || content_length.is_some_and(|len| now >= len) {
            last_emit = now;
            emit_progress(&app_chunks, "downloading", now, content_length);
        }
    };
    let (done_end, total_end, app_end) = (done.clone(), total.clone(), app.clone());
    let on_finish = move || {
        emit_progress(
            &app_end,
            "verifying",
            done_end.load(Ordering::SeqCst),
            known_total(&total_end),
        );
    };
    let bytes = update
        .download(on_chunk, on_finish)
        .await
        .map_err(map_download_error)?;

    // Verified. Only now stop the daemon, so a failed download leaves recording running.
    stop_daemon_for_update(app, stop_recording).await?;
    let from = update.current_version.clone();
    let _ = update_state::update(|s| s.last_installed_from = Some(from));

    let size = done.load(Ordering::SeqCst);
    emit_progress(app, "installing", size, known_total(&total));
    // On Windows this launches the passive installer and exits the process.
    update
        .install(bytes)
        .map_err(|e| format!("install failed: {e}"))?;
    Ok(())
}

/// Maps a download error. A signature mismatch becomes `"signature"`.
fn map_download_error(error: UpdaterError) -> String {
    match error {
        UpdaterError::Minisign(_) => "signature".into(),
        other => format!("download failed: {other}"),
    }
}

/// Asks the daemon to shut down for the update. A `busy` reply becomes `"recording"`.
/// If the daemon does not answer (not running, or unresponsive), the update continues:
/// the installer stops its own copy of the daemon.
async fn stop_daemon_for_update(app: &AppHandle, force: bool) -> Result<(), String> {
    let Some(client) = client_of(app) else {
        return Ok(());
    };
    let reply = tauri::async_runtime::spawn_blocking(move || {
        let command = if force {
            forced_update_shutdown_command()
        } else {
            update_shutdown_command()
        };
        client.request(command, SHUTDOWN_TIMEOUT)
    })
    .await;
    match reply {
        Ok(Ok(body)) => match classify_shutdown_reply(&body) {
            ShutdownReply::Busy => Err("recording".into()),
            ShutdownReply::Ack | ShutdownReply::Other => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Reads the settings from the daemon. Falls back to the defaults when it is offline.
async fn load_settings(app: AppHandle) -> UpdateSettings {
    tauri::async_runtime::spawn_blocking(move || read_settings_blocking(&app))
        .await
        .unwrap_or_default()
}

/// Blocking form of [`load_settings`]. Call off the UI thread.
fn read_settings_blocking(app: &AppHandle) -> UpdateSettings {
    let Some(client) = client_of(app) else {
        return UpdateSettings::default();
    };
    let command = json!({ "type": "get_config" });
    let timeout = timeout_for(&command);
    match client.request(command, timeout) {
        Ok(body) => UpdateSettings::from_config_reply(&body).unwrap_or_default(),
        Err(_) => UpdateSettings::default(),
    }
}

/// True while a recording session is active (read-only query).
async fn session_active_blocking(app: &AppHandle) -> bool {
    match client_of(app) {
        Some(client) => tauri::async_runtime::spawn_blocking(move || session_active_now(&client))
            .await
            .unwrap_or(false),
        None => false,
    }
}

/// The managed daemon client, if it is in Tauri state.
fn client_of(app: &AppHandle) -> Option<Arc<DaemonClient>> {
    app.try_state::<Arc<DaemonClient>>()
        .map(|state| Arc::clone(&*state))
}

/// Builds the plugin updater with `endpoint` in place of the configured one.
fn build_updater(app: &AppHandle, endpoint: Url) -> Result<tauri_plugin_updater::Updater, String> {
    app.updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| format!("update endpoint rejected: {e}"))?
        .build()
        .map_err(|e| format!("cannot start the updater: {e}"))
}

/// Records the time of a successful check. Failures are logged, not fatal.
fn record_checked() {
    if let Err(e) = update_state::update(|s| s.last_checked_unix_ms = Some(now_unix_ms())) {
        eprintln!("clipcheck-ui: cannot record update check time: {e}");
    }
}

fn is_installing(app: &AppHandle) -> bool {
    app.try_state::<Arc<UpdateRuntime>>()
        .is_some_and(|rt| rt.installing.load(Ordering::SeqCst))
}

/// Content length as an `Option`. Zero means unknown.
fn known_total(total: &AtomicU64) -> Option<u64> {
    match total.load(Ordering::SeqCst) {
        0 => None,
        len => Some(len),
    }
}

/// Sends one `update-progress` event.
fn emit_progress(app: &AppHandle, phase: &str, done: u64, total: Option<u64>) {
    let _ = app.emit(
        PROGRESS_EVENT,
        json!({ "phase": phase, "done": done, "total": total }),
    );
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_defaults_when_config_is_empty() {
        assert_eq!(
            UpdateSettings::from_config(&json!({})),
            UpdateSettings::default()
        );
    }

    #[test]
    fn never_and_interval_clamp() {
        let cfg =
            json!({ "updates": { "check": "never", "interval_hours": 9999, "endpoint": " " } });
        let s = UpdateSettings::from_config(&cfg);
        assert_eq!(s.check, CheckMode::Never);
        assert_eq!(s.interval_hours, INTERVAL_HOURS_MAX);
        assert_eq!(s.endpoint, "");

        let cfg = json!({ "updates": { "interval_hours": 0 } });
        assert_eq!(
            UpdateSettings::from_config(&cfg).interval_hours,
            INTERVAL_HOURS_MIN
        );
    }

    #[test]
    fn unknown_check_value_means_notify() {
        let cfg = json!({ "updates": { "check": "always" } });
        assert_eq!(UpdateSettings::from_config(&cfg).check, CheckMode::Notify);
    }

    #[test]
    fn config_reply_is_parsed_only_when_it_is_a_config() {
        let reply =
            json!({ "type": "config", "data": { "config": { "updates": { "check": "never" } } } });
        assert_eq!(
            UpdateSettings::from_config_reply(&reply).map(|s| s.check),
            Some(CheckMode::Never)
        );
        assert!(UpdateSettings::from_config_reply(&json!({ "type": "error" })).is_none());
    }

    #[test]
    fn endpoint_rules() {
        assert!(validate_endpoint("https://example.com/latest.json").is_ok());
        assert!(validate_endpoint("http://example.com/latest.json").is_err());
        assert!(validate_endpoint("ftp://example.com/latest.json").is_err());
        assert!(validate_endpoint("not a url").is_err());
        if cfg!(debug_assertions) {
            assert!(validate_endpoint("http://127.0.0.1:8080/latest.json").is_ok());
            assert!(validate_endpoint("http://localhost:8080/latest.json").is_ok());
        }
        let long = format!("https://example.com/{}", "a".repeat(300));
        assert!(validate_endpoint(&long).is_err());
    }

    #[test]
    fn empty_endpoint_resolves_to_default() {
        assert_eq!(
            resolve_endpoint("  ").map(|u| u.to_string()),
            Ok(DEFAULT_UPDATE_ENDPOINT.to_owned())
        );
    }

    #[test]
    fn newer_version_comparison() {
        assert!(is_newer("0.3.2", "0.3.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.0", "0.3.1"));
        assert!(!is_newer("junk", "0.3.1"));
    }

    #[test]
    fn known_total_treats_zero_as_unknown() {
        assert_eq!(known_total(&AtomicU64::new(0)), None);
        assert_eq!(known_total(&AtomicU64::new(42)), Some(42));
    }
}
