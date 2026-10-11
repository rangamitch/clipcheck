//! Lists behind the Settings pickers: audio apps, audio devices, encoders,
//! running windows, and the microphone meter.
//!
//! The pure parts (`build_*`, `vendor_name`, `detect_config_from`) take plain
//! data and are unit-tested without the OS. The glue functions at the bottom
//! read the OS through the crates' safe wrappers and keep daemon-wide state in
//! `OnceLock`s, because the IPC layer hands each connection its own thread and
//! the poller and meter outlive any single connection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_audio::{EndpointKind, SeenApp, SeenApps};
use clipcheck_detect::library::LibraryCache;
use clipcheck_detect::score::{evaluate_with, Verdict};
use clipcheck_detect::{known_games, modules, AllowRule, DetectConfig, Lists};
use clipcheck_encode::probe::{Backend, CodecKind, ProbeReport};
use clipcheck_ipc::proto::{
    AudioAppDto, DeviceDto, EncoderDto, ErrorCode, Event, GpuDto, Reply, RunningAppDto,
};
use clipcheck_util::sync::lock_unpoisoned as lock;
use serde_json::Value;

use crate::app::now_unix_ms;
use crate::appinfo;
use crate::events::EventBus;
use crate::logging;

/// Score at or above which a window is suggested as a game.
pub const SUGGEST_SCORE: u32 = 30;
/// Windows narrower or shorter than this (pixels) are not offered.
pub const MIN_WINDOW_PX: i64 = 100;
/// How long an app stays in the audio list after its last sound.
pub const AUDIO_MEMORY_MS: u64 = 24 * 60 * 60 * 1000;
/// Audio poll interval.
pub const AUDIO_POLL: Duration = Duration::from_secs(2);
/// The meter stops this long after its last refresh.
pub const METER_IDLE: Duration = Duration::from_secs(30);
/// Minimum spacing between published meter samples (about 10 Hz).
pub const METER_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on running-app rows.
pub const MAX_RUNNING_APPS: usize = 60;

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// One visible window, reduced to the fields the picker needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCandidate {
    /// Executable file name.
    pub exe: String,
    /// Window title (may be empty).
    pub title: String,
    /// Owning process id.
    pub pid: u32,
    /// Window width in pixels.
    pub width: i64,
    /// Window height in pixels.
    pub height: i64,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// DWM-cloaked.
    pub cloaked: bool,
    /// `WS_EX_TOOLWINDOW`.
    pub tool_window: bool,
    /// On the built-in or user blocklist.
    pub blocked: bool,
    /// A Windows shell process (explorer, shell experience host, ...).
    pub system_shell: bool,
    /// A launcher (Steam, Epic, ...).
    pub launcher: bool,
    /// This daemon or ClipCheck's own UI.
    pub is_self: bool,
    /// Detector score, 0 when a gate rejected the window.
    pub score: u32,
    /// Exe is in the known-games database.
    pub known: bool,
    /// Install directory is inside a library scan.
    pub in_library: bool,
}

/// Builds the running-app rows from window candidates.
///
/// Candidates arrive in z-order (topmost first). Non-game windows are dropped,
/// windows of the same exe are merged into the first (topmost) one, and exes in
/// `excluded` (lowercase; allow list and attached game) are removed. A row is a
/// game when its score reaches `accept_score` or it is known or in a library; it
/// is suggested at `SUGGEST_SCORE` or the same known/library rule. Suggested rows
/// come first, then rows sort by name (case-insensitive) and exe.
pub fn build_running_apps(
    cands: &[WindowCandidate],
    excluded: &[String],
    accept_score: u32,
) -> Vec<RunningAppDto> {
    let mut seen: Vec<String> = Vec::new();
    let mut rows: Vec<RunningAppDto> = Vec::new();
    for c in cands {
        if c.exe.is_empty()
            || !c.visible
            || c.cloaked
            || c.tool_window
            || c.blocked
            || c.system_shell
            || c.launcher
            || c.is_self
            || c.width < MIN_WINDOW_PX
            || c.height < MIN_WINDOW_PX
        {
            continue;
        }
        let key = c.exe.to_lowercase();
        if excluded.iter().any(|e| e.eq_ignore_ascii_case(&key)) || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let known_or_lib = c.known || c.in_library;
        let suggested = c.score >= SUGGEST_SCORE || known_or_lib;
        let is_game = c.score >= accept_score || known_or_lib;
        let name = if c.title.trim().is_empty() {
            stem(&c.exe)
        } else {
            c.title.trim().to_owned()
        };
        rows.push(RunningAppDto {
            exe: c.exe.clone(),
            name,
            icon_png_b64: None,
            pid: c.pid,
            is_game,
            in_allow_list: false,
            suggested,
        });
    }
    rows.sort_by(|a, b| {
        b.suggested
            .cmp(&a.suggested)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.exe.to_lowercase().cmp(&b.exe.to_lowercase()))
    });
    rows.truncate(MAX_RUNNING_APPS);
    rows
}

/// Live state of one session: `(active, peak 0..1)`, keyed by lowercase exe.
pub type LiveMap = HashMap<String, (bool, f32)>;

/// Builds audio-app rows from the remembered apps and their live state.
///
/// Remembered apps appear even when silent. ClipCheck's own processes are
/// excluded. Rows are ordered most recent sound first. Icons are added by the caller.
pub fn build_audio_apps(seen: &[SeenApp], live: &LiveMap) -> Vec<AudioAppDto> {
    seen.iter()
        .filter(|a| !is_self_exe(&a.exe_name) && !a.exe_name.is_empty())
        .map(|a| {
            let key = a.exe_name.to_lowercase();
            let (active, peak) = live.get(&key).copied().unwrap_or((false, 0.0));
            let name = if a.display_name.trim().is_empty() {
                stem(&a.exe_name)
            } else {
                a.display_name.trim().to_owned()
            };
            AudioAppDto {
                exe: a.exe_name.clone(),
                name,
                icon_png_b64: None,
                active,
                peak: if peak.is_finite() {
                    peak.clamp(0.0, 1.0)
                } else {
                    0.0
                },
                last_active_unix_ms: i64::try_from(a.last_seen_ms).unwrap_or(i64::MAX),
            }
        })
        .collect()
}

/// Maps a PCI vendor id to the vendor label used by the UI.
pub fn vendor_name(vendor_id: u32) -> &'static str {
    match vendor_id {
        0x10DE => "nvidia",
        0x1002 | 0x1022 => "amd",
        0x8086 => "intel",
        _ => "other",
    }
}

/// Maps an encoder backend to its label.
pub fn backend_name(b: Backend) -> &'static str {
    match b {
        Backend::Nvenc => "nvenc",
        Backend::Amf => "amf",
        Backend::Qsv => "qsv",
        Backend::Software => "software",
    }
}

/// Maps a codec to its label.
pub fn codec_label(c: CodecKind) -> &'static str {
    match c {
        CodecKind::H264 => "h264",
        CodecKind::H265 => "h265",
        CodecKind::Av1 => "av1",
    }
}

/// Builds the encoder rows from a probe report. `None` (probe pending or failed)
/// yields no encoders and `encoder_ready` false.
pub fn encoders_reply(report: Option<&ProbeReport>, gpus: Vec<GpuDto>) -> Reply {
    match report {
        None => Reply::Encoders {
            encoder_ready: false,
            gpus,
            encoders: Vec::new(),
        },
        Some(r) => Reply::Encoders {
            encoder_ready: true,
            gpus,
            encoders: r
                .encoders
                .iter()
                .map(|e| EncoderDto {
                    backend: backend_name(e.backend).to_owned(),
                    codec: codec_label(e.codec).to_owned(),
                    encoder_name: e.encoder_name.clone(),
                    works: e.works,
                })
                .collect(),
        },
    }
}

/// Reads an exe entry that is either a plain string or `{ "exe": ... }`.
fn exe_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("exe").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

fn str_list(config: &Value, section: &str, key: &str) -> Vec<String> {
    config
        .get(section)
        .and_then(|s| s.get(key))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(exe_of).collect())
        .unwrap_or_default()
}

/// Detection settings the pickers need, read from the configuration JSON
/// (`detection.allow` and `detection.block`). `base` supplies the thresholds.
pub fn detect_config_from(config: &Value, base: DetectConfig) -> DetectConfig {
    let allow = str_list(config, "detection", "allow")
        .into_iter()
        .map(|exe| AllowRule {
            exe,
            display_name: None,
        })
        .collect();
    DetectConfig {
        allow,
        block_extra: str_list(config, "detection", "block"),
        ..base
    }
}

/// Lowercase exes that are already recorded as games: the allow list plus the attached game.
pub fn excluded_exes(config: &Value, attached: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = str_list(config, "detection", "allow")
        .into_iter()
        .map(|e| e.to_lowercase())
        .collect();
    if let Some(exe) = attached {
        out.push(exe.to_lowercase());
    }
    out
}

fn is_self_exe(exe: &str) -> bool {
    exe.to_lowercase().starts_with("clipcheck")
}

/// File name without its extension, for display when no better name exists.
fn stem(exe: &str) -> String {
    match exe.rfind('.') {
        Some(i) if i > 0 => exe[..i].to_owned(),
        _ => exe.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Audio apps (background poller)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AudioState {
    seen: SeenApps,
    live: LiveMap,
    started: bool,
    polled: bool,
}

fn audio_state() -> &'static Mutex<AudioState> {
    static STATE: OnceLock<Mutex<AudioState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(AudioState::default()))
}

/// Starts the background audio poller once. Called when the first client
/// subscribes and on the first audio-app request.
pub fn ensure_audio_poller() {
    {
        let mut st = lock(audio_state());
        if st.started {
            return;
        }
        st.started = true;
    }
    let spawned = thread::Builder::new()
        .name("audio-poll".to_owned())
        .spawn(|| loop {
            poll_audio_once();
            thread::sleep(AUDIO_POLL);
        });
    if let Err(e) = spawned {
        logging::warn(&format!("cannot start audio app poller: {e}"));
        lock(audio_state()).started = false;
    }
}

/// One poll of the audio sessions. Failures are ignored; the next poll retries.
fn poll_audio_once() {
    let Ok(infos) = clipcheck_audio::list_sessions() else {
        return;
    };
    let now = u64::try_from(now_unix_ms()).unwrap_or(0);
    let mut st = lock(audio_state());
    st.seen.observe(&infos, now);
    st.live = infos
        .iter()
        .filter(|i| !i.exe_name.is_empty())
        .map(|i| {
            (
                i.exe_name.to_lowercase(),
                (
                    i.state == clipcheck_audio::SessionState::Active,
                    i.peak.clamp(0.0, 1.0),
                ),
            )
        })
        .collect();
    st.polled = true;
}

/// Reply for `list_audio_apps`.
pub fn audio_apps_reply() -> Reply {
    ensure_audio_poller();
    // The first request after startup must not return an empty list just because
    // the poller has not run yet.
    let needs_poll = !lock(audio_state()).polled;
    if needs_poll {
        poll_audio_once();
    }
    let now = u64::try_from(now_unix_ms()).unwrap_or(0);
    let (seen, live) = {
        let st = lock(audio_state());
        (st.seen.list(now, AUDIO_MEMORY_MS), st.live.clone())
    };
    let paths: HashMap<String, PathBuf> = seen
        .iter()
        .filter_map(|a| Some((a.exe_name.to_lowercase(), a.exe_path.clone()?)))
        .collect();
    let mut apps = build_audio_apps(&seen, &live);
    for app in &mut apps {
        if let Some(path) = paths.get(&app.exe.to_lowercase()) {
            app.icon_png_b64 = appinfo::icon_png_b64(path);
        }
    }
    Reply::AudioApps { apps }
}

// ---------------------------------------------------------------------------
// Devices and encoders
// ---------------------------------------------------------------------------

/// Reply for `list_audio_devices`.
pub fn audio_devices_reply() -> Reply {
    let list = |kind: EndpointKind| -> Result<Vec<DeviceDto>, String> {
        clipcheck_audio::devices::list_endpoints(kind)
            .map(|eps| {
                eps.into_iter()
                    .map(|e| DeviceDto {
                        id: e.id,
                        name: e.name,
                        is_default: e.is_default,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    };
    match (list(EndpointKind::Capture), list(EndpointKind::Render)) {
        (Ok(capture), Ok(render)) => Reply::AudioDevices { capture, render },
        (Err(e), _) | (_, Err(e)) => Reply::Error {
            code: ErrorCode::Internal,
            message: format!("cannot list audio devices: {e}"),
            retryable: true,
        },
    }
}

/// Reply for `list_encoders`. `report` is the service's cached probe, if any.
pub fn encoders_reply_for(report: Option<&ProbeReport>) -> Reply {
    let gpus = appinfo::enumerate_gpus()
        .into_iter()
        .map(|g| GpuDto {
            name: g.name,
            vendor: vendor_name(g.vendor_id).to_owned(),
            index: g.index,
            dedicated_mb: g.dedicated_mb,
        })
        .collect();
    encoders_reply(report, gpus)
}

// ---------------------------------------------------------------------------
// Running windows
// ---------------------------------------------------------------------------

fn library_cache() -> &'static Mutex<LibraryCache> {
    static CACHE: OnceLock<Mutex<LibraryCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(LibraryCache::new()))
}

/// Reply for `list_running_apps`. `config` is the configuration JSON and
/// `attached` the exe of the game being recorded, if any.
pub fn running_apps_reply(config: &Value, attached: Option<&str>) -> Reply {
    let cfg = detect_config_from(config, DetectConfig::default());
    let lists = Lists::from_config(&cfg);
    let own_pid = std::process::id();
    let now = u64::try_from(now_unix_ms()).unwrap_or(0);

    let snaps = clipcheck_detect::win::enumerate_candidates();
    let mut paths: HashMap<String, PathBuf> = HashMap::new();
    let mut cands = Vec::with_capacity(snaps.len());
    {
        let mut cache = lock(library_cache());
        let scan = cache.get(now);
        for snap in &snaps {
            if snap.exe_name.is_empty() {
                continue;
            }
            let exe = snap.exe_name.clone();
            let score = match evaluate_with(&lists, snap, &cfg) {
                Verdict::Scored { score, .. } => score,
                Verdict::Rejected(_) => 0,
            };
            let exe_path = modules::exe_path(snap.target.pid);
            let in_library = exe_path
                .as_ref()
                .is_some_and(|p| scan.find_for_exe(p).is_some());
            if let Some(p) = &exe_path {
                paths.entry(exe.to_lowercase()).or_insert_with(|| p.clone());
            }
            cands.push(WindowCandidate {
                exe: exe.clone(),
                title: snap.window_title.clone(),
                pid: snap.target.pid,
                width: snap.window_rect.width(),
                height: snap.window_rect.height(),
                visible: snap.is_visible,
                cloaked: snap.is_cloaked,
                tool_window: snap.is_tool_window,
                blocked: lists.is_blocked(&exe),
                system_shell: lists.is_system_shell(&exe),
                launcher: lists.is_launcher(&exe),
                is_self: snap.target.pid == own_pid || is_self_exe(&exe),
                score,
                known: known_games::is_known(&exe).is_some(),
                in_library,
            });
        }
    }

    let excluded = excluded_exes(config, attached);
    let mut apps = build_running_apps(&cands, &excluded, u32::from(cfg.min_score));
    for app in &mut apps {
        if let Some(path) = paths.get(&app.exe.to_lowercase()) {
            app.icon_png_b64 = appinfo::icon_png_b64(path);
        }
    }
    Reply::RunningApps { apps }
}

// ---------------------------------------------------------------------------
// Microphone meter
// ---------------------------------------------------------------------------

struct MeterSlot {
    owner: u64,
    device: Option<String>,
    stop: Arc<AtomicBool>,
    /// Unix ms of the last `mic_meter{enable:true}`.
    refreshed_ms: Arc<AtomicU64>,
}

fn meter_slot() -> &'static Mutex<Option<MeterSlot>> {
    static SLOT: OnceLock<Mutex<Option<MeterSlot>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Handles `mic_test`: records `MIC_TEST_SECONDS` from a capture endpoint (`None` =
/// default) and returns it as a base64 WAV. Takes about 5 seconds.
pub fn mic_test_reply(device: Option<String>) -> Reply {
    #[cfg(windows)]
    {
        use base64::Engine;

        match clipcheck_audio::record_mic_clip(device, clipcheck_audio::MIC_TEST_SECONDS) {
            Ok(clip) => Reply::MicTest {
                wav_base64: base64::engine::general_purpose::STANDARD.encode(&clip.wav),
                duration_ms: clip.duration_ms,
                peak_db: clip.peak_db,
            },
            Err(e) => Reply::Error {
                code: ErrorCode::WorkerUnavailable,
                message: format!("the microphone test failed: {e}"),
                retryable: true,
            },
        }
    }
    #[cfg(not(windows))]
    {
        let _ = device;
        Reply::Error {
            code: ErrorCode::WorkerUnavailable,
            message: "the microphone test needs Windows".to_owned(),
            retryable: true,
        }
    }
}

/// Handles `mic_meter`. `owner` identifies the connection; at most one meter
/// runs and a new enable replaces it. Refreshing with the same owner and device
/// keeps the running meter.
pub fn mic_meter(
    owner: u64,
    enable: bool,
    device: Option<String>,
    bus: Arc<EventBus>,
) -> Result<(), String> {
    let mut slot = lock(meter_slot());
    if !enable {
        if let Some(current) = slot.as_ref().filter(|s| s.owner == owner) {
            current.stop.store(true, Ordering::Relaxed);
        }
        if slot.as_ref().is_some_and(|s| s.owner == owner) {
            *slot = None;
        }
        return Ok(());
    }
    let now = u64::try_from(now_unix_ms()).unwrap_or(0);
    if let Some(current) = slot.as_ref() {
        if current.owner == owner
            && current.device == device
            && !current.stop.load(Ordering::Relaxed)
        {
            current.refreshed_ms.store(now, Ordering::Relaxed);
            return Ok(());
        }
        current.stop.store(true, Ordering::Relaxed);
    }
    *slot = None;

    let stop = Arc::new(AtomicBool::new(false));
    let refreshed_ms = Arc::new(AtomicU64::new(now));
    let (started_tx, started_rx) = mpsc::channel::<Result<(), String>>();
    let thread_stop = Arc::clone(&stop);
    let thread_refresh = Arc::clone(&refreshed_ms);
    let thread_device = device.clone();
    thread::Builder::new()
        .name("mic-meter".to_owned())
        .spawn(move || {
            run_meter(
                thread_device,
                &bus,
                &thread_stop,
                &thread_refresh,
                started_tx,
            );
        })
        .map_err(|e| format!("cannot start the meter thread: {e}"))?;

    match started_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            *slot = Some(MeterSlot {
                owner,
                device,
                stop,
                refreshed_ms,
            });
            Ok(())
        }
        Ok(Err(msg)) => Err(msg),
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            stop.store(true, Ordering::Relaxed);
            Err("the microphone meter did not start in time".to_owned())
        }
    }
}

/// Stops the meter if `owner` (a disconnected client) started it.
pub fn meter_release(owner: u64) {
    let mut slot = lock(meter_slot());
    if let Some(current) = slot.as_ref().filter(|s| s.owner == owner) {
        current.stop.store(true, Ordering::Relaxed);
    }
    if slot.as_ref().is_some_and(|s| s.owner == owner) {
        *slot = None;
    }
}

#[cfg(windows)]
fn run_meter(
    device: Option<String>,
    bus: &EventBus,
    stop: &AtomicBool,
    refreshed_ms: &AtomicU64,
    started: mpsc::Sender<Result<(), String>>,
) {
    use clipcheck_audio::MicMeter;

    let (mut meter, readings) = match MicMeter::start(device) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = started.send(Err(format!("no capture device for the meter: {e}")));
            return;
        }
    };
    let _ = started.send(Ok(()));
    let mut last_publish = Instant::now();
    let mut latest = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let idle_ms = u64::try_from(now_unix_ms())
            .unwrap_or(0)
            .saturating_sub(refreshed_ms.load(Ordering::Relaxed));
        if idle_ms > METER_IDLE.as_millis() as u64 {
            break;
        }
        match readings.recv_timeout(METER_INTERVAL) {
            Ok(r) => latest = Some(r),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_publish.elapsed() >= METER_INTERVAL {
            if let Some(r) = latest.take() {
                bus.publish(Event::MicLevel {
                    rms_db: r.rms_db,
                    peak_db: r.peak_db,
                });
            }
            last_publish = Instant::now();
        }
    }
    meter.stop();
}

#[cfg(not(windows))]
fn run_meter(
    _device: Option<String>,
    _bus: &EventBus,
    _stop: &AtomicBool,
    _refreshed_ms: &AtomicU64,
    started: mpsc::Sender<Result<(), String>>,
) {
    let _ = started.send(Err("the microphone meter needs Windows".to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(exe: &str, score: u32) -> WindowCandidate {
        WindowCandidate {
            exe: exe.to_owned(),
            title: format!("{exe} window"),
            pid: 100,
            width: 1280,
            height: 720,
            visible: true,
            cloaked: false,
            tool_window: false,
            blocked: false,
            system_shell: false,
            launcher: false,
            is_self: false,
            score,
            known: false,
            in_library: false,
        }
    }

    #[test]
    fn running_apps_sort_suggested_first_then_name() {
        let cands = vec![
            cand("notepad.exe", 0),
            cand("zeta.exe", 40),
            cand("alpha.exe", 10),
        ];
        let apps = build_running_apps(&cands, &[], 60);
        let order: Vec<&str> = apps.iter().map(|a| a.exe.as_str()).collect();
        assert_eq!(order, ["zeta.exe", "alpha.exe", "notepad.exe"]);
        assert!(apps[0].suggested && !apps[0].is_game);
        assert!(!apps[1].suggested && !apps[1].is_game);
    }

    #[test]
    fn known_or_library_is_game_and_suggested() {
        let mut known = cand("cs2.exe", 0);
        known.known = true;
        let mut lib = cand("lib.exe", 5);
        lib.in_library = true;
        let apps = build_running_apps(&[known, lib], &[], 60);
        assert!(apps.iter().all(|a| a.is_game && a.suggested));
    }

    #[test]
    fn score_at_accept_threshold_is_game() {
        let apps = build_running_apps(&[cand("g.exe", 60)], &[], 60);
        assert!(apps[0].is_game);
        let apps = build_running_apps(&[cand("g.exe", 59)], &[], 60);
        assert!(!apps[0].is_game && apps[0].suggested);
    }

    #[test]
    fn duplicates_merge_into_topmost_window() {
        let mut top = cand("app.exe", 0);
        top.title = "Top".to_owned();
        let mut below = cand("APP.exe", 0);
        below.title = "Below".to_owned();
        let apps = build_running_apps(&[top, below], &[], 60);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Top");
    }

    #[test]
    fn filters_drop_unwanted_windows() {
        let mut hidden = cand("a.exe", 50);
        hidden.visible = false;
        let mut cloaked = cand("b.exe", 50);
        cloaked.cloaked = true;
        let mut tool = cand("c.exe", 50);
        tool.tool_window = true;
        let mut blocked = cand("d.exe", 50);
        blocked.blocked = true;
        let mut shell = cand("e.exe", 50);
        shell.system_shell = true;
        let mut launcher = cand("f.exe", 50);
        launcher.launcher = true;
        let mut me = cand("g.exe", 50);
        me.is_self = true;
        let mut tiny = cand("h.exe", 50);
        tiny.width = 99;
        let mut empty = cand("", 50);
        empty.exe.clear();
        let apps = build_running_apps(
            &[
                hidden, cloaked, tool, blocked, shell, launcher, me, tiny, empty,
            ],
            &[],
            60,
        );
        assert!(apps.is_empty());
    }

    #[test]
    fn excluded_exes_are_removed_case_insensitively() {
        let cands = vec![cand("Game.exe", 90), cand("other.exe", 90)];
        let apps = build_running_apps(&cands, &["game.exe".to_owned()], 60);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].exe, "other.exe");
    }

    #[test]
    fn running_name_falls_back_to_stem() {
        let mut c = cand("tool.exe", 0);
        c.title = "   ".to_owned();
        let apps = build_running_apps(&[c], &[], 60);
        assert_eq!(apps[0].name, "tool");
    }

    #[test]
    fn detect_config_reads_allow_and_block() {
        let cfg = serde_json::json!({
            "detection": {
                "allow": ["Foo.exe", {"exe": "Bar.exe"}],
                "block": ["Baz.exe"]
            }
        });
        let d = detect_config_from(&cfg, DetectConfig::default());
        let allow: Vec<&str> = d.allow.iter().map(|a| a.exe.as_str()).collect();
        assert_eq!(allow, ["Foo.exe", "Bar.exe"]);
        assert_eq!(d.block_extra, ["Baz.exe"]);
        assert_eq!(d.min_score, DetectConfig::default().min_score);
    }

    #[test]
    fn excluded_includes_allow_list_and_attached() {
        let cfg = serde_json::json!({"detection": {"allow": ["A.exe"]}});
        let ex = excluded_exes(&cfg, Some("Live.exe"));
        assert_eq!(ex, ["a.exe", "live.exe"]);
        assert_eq!(
            excluded_exes(&serde_json::json!({}), None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn audio_apps_keep_silent_apps_and_exclude_self() {
        let seen = vec![
            SeenApp {
                exe_name: "Discord.exe".to_owned(),
                display_name: "Discord".to_owned(),
                exe_path: None,
                first_seen_ms: 1,
                last_seen_ms: 500,
            },
            SeenApp {
                exe_name: "ClipCheck.exe".to_owned(),
                display_name: String::new(),
                exe_path: None,
                first_seen_ms: 1,
                last_seen_ms: 600,
            },
            SeenApp {
                exe_name: "tone.exe".to_owned(),
                display_name: String::new(),
                exe_path: None,
                first_seen_ms: 1,
                last_seen_ms: 700,
            },
        ];
        let mut live = LiveMap::new();
        live.insert("tone.exe".to_owned(), (true, 0.4));
        let apps = build_audio_apps(&seen, &live);
        assert_eq!(apps.len(), 2);
        let discord = apps
            .iter()
            .find(|a| a.exe == "Discord.exe")
            .expect("discord");
        assert!(!discord.active);
        assert_eq!(discord.peak, 0.0);
        assert_eq!(discord.last_active_unix_ms, 500);
        let ff = apps.iter().find(|a| a.exe == "tone.exe").expect("tone");
        assert!(ff.active);
        assert_eq!(ff.name, "tone");
        assert!((ff.peak - 0.4).abs() < 1e-6);
    }

    #[test]
    fn audio_peak_is_clamped() {
        let seen = vec![SeenApp {
            exe_name: "x.exe".to_owned(),
            display_name: "X".to_owned(),
            exe_path: None,
            first_seen_ms: 0,
            last_seen_ms: 0,
        }];
        let mut live = LiveMap::new();
        live.insert("x.exe".to_owned(), (true, f32::NAN));
        assert_eq!(build_audio_apps(&seen, &live)[0].peak, 0.0);
        live.insert("x.exe".to_owned(), (true, 7.0));
        assert_eq!(build_audio_apps(&seen, &live)[0].peak, 1.0);
    }

    #[test]
    fn vendor_ids_map_to_labels() {
        assert_eq!(vendor_name(0x10DE), "nvidia");
        assert_eq!(vendor_name(0x1002), "amd");
        assert_eq!(vendor_name(0x8086), "intel");
        assert_eq!(vendor_name(0x1414), "other");
    }

    #[test]
    fn encoders_without_report_are_not_ready() {
        match encoders_reply(None, Vec::new()) {
            Reply::Encoders {
                encoder_ready,
                encoders,
                ..
            } => {
                assert!(!encoder_ready);
                assert!(encoders.is_empty());
            }
            other => panic!("unexpected reply {other:?}"),
        }
    }
}
