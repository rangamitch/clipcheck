//! The recording service (task D1c).
//!
//! [`Service`] wires together the pieces that make the daemon record: the
//! encoder probe, the [`Recorder`], game detection, clip and session hotkeys,
//! and registration of finished files in the library. One service thread owns
//! all of them, so nothing here has to be `Send`. IPC handlers reach the thread
//! through [`Service::status`], [`Service::clip`] and the session methods.
//! Events the service emits go out on the [`EventBus`].
//!
//! Degraded mode: when the encoder probe fails, the service keeps
//! running. Detection and hotkeys still work, status reports the reason, and
//! recording commands fail with `WorkerUnavailable`. Nothing here panics on a
//! missing encoder.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use clipcheck_core::config::schema::{Codec, Config, DetectionMode};
use clipcheck_core::config::validate::{apply_class, validate, ApplyClass, Diagnostic};
use clipcheck_detect::known_games;
use clipcheck_detect::lists::name_matches;
use clipcheck_detect::runner::{DetectorEvent, DetectorLoop, WinSource};
use clipcheck_detect::{AllowRule, DetectConfig};
#[cfg(windows)]
use clipcheck_encode::probe::probe_native;
use clipcheck_encode::probe::ProbeReport;
use clipcheck_engine::catalog::{
    commit_prepared, find_registered, prepare_recording, RegisterMeta,
};
use clipcheck_engine::recorder::{
    LevelSnapshot, Recorder, RecorderConfig, RecorderError, RecorderEvent,
};
use clipcheck_hotkeys::{
    ApplyError, FailureReason, HotkeyAction, HotkeyEvent, HotkeyRegistry, HotkeyService,
    RegisterFailure, Registry,
};
use clipcheck_ipc::proto::{
    AutostartState, ErrorCode, Event, GameInfo, GameRef, RecState, SessionInfo, StatusDto,
    TrackLevel,
};
use clipcheck_library::db::RecordingSource;
use clipcheck_library::schema::now_ms;
use clipcheck_library::{Library, RecordingKind};
use serde_json::Value;
use ulid::Ulid;

use crate::app::now_unix_ms;
use crate::configstore::{self, ConfigStore};
use crate::events::EventBus;
use crate::library_service::{LibraryOptions, LibraryService};
use crate::logging;
use crate::salvage::{self, SalvageOutcome};
use crate::settings_map::{self, EncoderChoiceInfo};

/// Encoder probe cache file name inside the data directory.
pub const PROBE_CACHE_FILE: &str = "encoder-cache.json";
/// Recorder on/off choice, in the data directory next to the config. Absent means on.
/// Content: `{"enabled": bool}`.
pub const RECORDER_STATE_FILE: &str = "recorder-state.json";
/// Error text for commands that need the recorder while it is turned off.
pub const RECORDER_OFF_MESSAGE: &str = "recorder is turned off";
/// Thumbnail folder inside the data directory.
const THUMBS_DIR: &str = "thumbs";
/// Longest clip the IPC accepts, in seconds.
pub const MAX_CLIP_SECONDS: u32 = 3600;
/// How long an IPC request waits for the service thread before answering `busy`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Lower and upper bounds for the detection poll interval, in ms (schema range).
const POLL_MS_RANGE: (u32, u32) = (250, 5000);
/// Degraded reason shown while the encoder probe is still running.
const PROBE_PENDING: &str = "encoder probe in progress";
/// How long a call that already started keeps waiting for its answer after the
/// first timeout. Past this the caller gets a non-retryable "outcome unknown".
const RUNNING_GRACE: Duration = Duration::from_secs(30);

/// Claim state of one IPC call, shared by the caller and the service thread.
/// The caller abandons a call that has not started yet; the service thread
/// starts a call only if it is still pending. Exactly one side wins the swap.
const CALL_PENDING: u8 = 0;
const CALL_RUNNING: u8 = 1;
const CALL_ABANDONED: u8 = 2;

/// Error returned by a service call. The code maps directly to an IPC error reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    /// Machine-readable code.
    pub code: ErrorCode,
    /// Human-readable detail.
    pub message: String,
}

impl ServiceError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Everything the service needs from the daemon at startup.
pub struct ServiceOptions {
    /// Configuration in effect.
    pub config: Config,
    /// Daemon data directory (config, library, logs, buffer, thumbs).
    pub data_dir: PathBuf,
    /// The daemon's library. The service is its only writer.
    pub library: Arc<Mutex<Library>>,
    /// Bus that carries pushed IPC events.
    pub bus: Arc<EventBus>,
    /// Behave as if no encoders were found (degraded mode). Test hook.
    pub no_encoders: bool,
}

/// Messages handled by the service thread.
enum Msg {
    /// The encoder probe finished: the report, or the reason it failed.
    Probe(Result<ProbeReport, String>),
    /// A detection event.
    Detector(DetectorEvent),
    /// A registered hotkey was pressed.
    Hotkey(HotkeyEvent),
    /// An event from the recorder.
    Recorder(RecorderEvent),
    /// Time to sample the recorder's audio levels and publish them (every 100 ms).
    LevelsTick,
    /// A call from an IPC handler, with the channel for its answer and the claim
    /// state shared with the caller (see [`CALL_PENDING`]).
    Call(Op, Sender<Result<Answer, ServiceError>>, Arc<AtomicU8>),
    /// Crash salvage finished: one outcome per leftover staged session part.
    Salvaged(Vec<SalvageOutcome>),
    /// Stop the service: detach, unregister hotkeys, stop detection.
    Stop,
}

/// Operations an IPC handler can ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Status,
    StartSession,
    StopSession,
    SplitSession,
    Clip(u32),
    GetConfig,
    PatchConfig {
        patch: Value,
        dry_run: bool,
    },
    ReloadConfig,
    Encoders,
    /// Start shutting down (D47). Refused with `busy` unless `force` while a session is active.
    BeginShutdown {
        force: bool,
    },
    /// Turn the recorder on (true) or off (false).
    SetRecorderEnabled(bool),
    /// Suspend (true) or resume (false) the hotkeys. A suspension lasts `timeout`
    /// unless renewed by another call.
    SetHotkeysSuspended {
        suspended: bool,
        timeout: Duration,
    },
}

/// Successful answers to [`Op`].
enum Answer {
    Status(Box<StatusDto>),
    Done,
    Clip(String),
    Config(ConfigView),
    Outcome(ConfigOutcome),
    Encoders(Option<ProbeReport>),
}

/// The configuration in effect, as returned by `get_config`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigView {
    /// Full configuration as a JSON object.
    pub config: Value,
    /// `config_version` of that configuration.
    pub version: u32,
    /// Absolute path of the config file.
    pub path: String,
}

/// Result of a patch or reload.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigOutcome {
    /// The change was saved (or the reload took effect) and applied.
    Applied {
        /// Strongest apply class of the change.
        class: ApplyClass,
        /// True when part of the change waits until no game is attached.
        restart_required: bool,
        /// Warnings. Warnings never block a change.
        warnings: Vec<Diagnostic>,
    },
    /// A dry run passed. Nothing was written or applied.
    Checked {
        /// Apply class the change would have.
        class: ApplyClass,
        /// Whether the change would wait for the next detach.
        restart_required: bool,
        /// Warnings the change would produce.
        warnings: Vec<Diagnostic>,
    },
    /// The change has errors. Nothing was written or applied.
    Rejected(Vec<Diagnostic>),
}

struct Inner {
    tx: Sender<Msg>,
    join: Mutex<Option<JoinHandle<()>>>,
    /// Library scans, thumbnails and imports. Its own thread does the work.
    library: LibraryService,
}

/// Handle to the running recording service. Cheap to clone.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

impl Service {
    /// Starts the service thread. Returns once the thread exists; the encoder
    /// probe runs in the background and recording becomes ready later.
    pub fn start(opts: ServiceOptions) -> Result<Service, String> {
        let recording = Arc::new(AtomicBool::new(false));
        let library = LibraryService::start(LibraryOptions {
            data_dir: opts.data_dir.clone(),
            storage: opts.config.storage.clone(),
            bus: Arc::clone(&opts.bus),
            recording: Arc::clone(&recording),
        })?;
        let (tx, rx) = mpsc::channel::<Msg>();
        let thread_tx = tx.clone();
        let core_library = library.clone();
        let spawned = thread::Builder::new()
            .name("service".to_owned())
            .spawn(move || {
                let mut core = Core::new(opts, thread_tx, core_library, recording);
                core.run(rx);
            });
        let handle = match spawned {
            Ok(handle) => handle,
            Err(e) => {
                library.stop();
                return Err(format!("cannot start service thread: {e}"));
            }
        };
        Ok(Service {
            inner: Arc::new(Inner {
                tx,
                join: Mutex::new(Some(handle)),
                library,
            }),
        })
    }

    /// The library service: scans, thumbnails, imports and the library commands.
    pub fn library(&self) -> &LibraryService {
        &self.inner.library
    }

    /// Stops the service and waits for its thread. Safe to call more than once.
    pub fn stop(&self) {
        let _ = self.inner.tx.send(Msg::Stop);
        let handle = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Status snapshot. The caller fills in the daemon-level fields.
    pub fn status(&self) -> Result<StatusDto, ServiceError> {
        match self.call(Op::Status)? {
            Answer::Status(status) => Ok(*status),
            _ => Err(unexpected_answer()),
        }
    }

    /// Starts a session on the attached game.
    pub fn start_session(&self) -> Result<(), ServiceError> {
        self.call(Op::StartSession).map(|_| ())
    }

    /// Stops the active session and writes its final part.
    pub fn stop_session(&self) -> Result<(), ServiceError> {
        self.call(Op::StopSession).map(|_| ())
    }

    /// Ends the current session part and starts the next one.
    pub fn split_session(&self) -> Result<(), ServiceError> {
        self.call(Op::SplitSession).map(|_| ())
    }

    /// Queues a clip of the last `seconds`. Returns the clip id that the
    /// `clip_saved` event will carry.
    pub fn clip(&self, seconds: u32) -> Result<String, ServiceError> {
        match self.call(Op::Clip(seconds))? {
            Answer::Clip(id) => Ok(id),
            _ => Err(unexpected_answer()),
        }
    }

    /// The configuration in effect and where it is stored.
    pub fn config(&self) -> Result<ConfigView, ServiceError> {
        match self.call(Op::GetConfig)? {
            Answer::Config(view) => Ok(view),
            _ => Err(unexpected_answer()),
        }
    }

    /// Merges `patch` into the configuration, validates it, and saves and applies it.
    /// With `dry_run` only the validation runs.
    pub fn patch_config(&self, patch: Value, dry_run: bool) -> Result<ConfigOutcome, ServiceError> {
        match self.call(Op::PatchConfig { patch, dry_run })? {
            Answer::Outcome(outcome) => Ok(outcome),
            _ => Err(unexpected_answer()),
        }
    }

    /// Re-reads `config.toml` from disk, validates it, and applies it.
    pub fn reload_config(&self) -> Result<ConfigOutcome, ServiceError> {
        match self.call(Op::ReloadConfig)? {
            Answer::Outcome(outcome) => Ok(outcome),
            _ => Err(unexpected_answer()),
        }
    }

    /// The cached encoder probe, or `None` while it runs or when no encoder is usable.
    pub fn encoder_report(&self) -> Result<Option<ProbeReport>, ServiceError> {
        match self.call(Op::Encoders)? {
            Answer::Encoders(report) => Ok(report),
            _ => Err(unexpected_answer()),
        }
    }

    /// Turns the recorder on or off and persists the choice. Turning it off stops and saves
    /// a recording session first. Pending settings apply as soon as the recorder is idle.
    pub fn set_recorder_enabled(&self, enabled: bool) -> Result<(), ServiceError> {
        self.call(Op::SetRecorderEnabled(enabled)).map(|_| ())
    }

    /// Suspends (true) or resumes (false) the global hotkeys. While suspended every chord
    /// is ignored, so a settings UI can capture new chords. A suspension ends by itself
    /// [`HOTKEY_SUSPEND_TIMEOUT`] after the last call that set it, and also ends on a config
    /// change or a hotkey restart. See `Command::SetHotkeysSuspended`.
    pub fn set_hotkeys_suspended(&self, suspended: bool) -> Result<(), ServiceError> {
        self.set_hotkeys_suspended_for(suspended, HOTKEY_SUSPEND_TIMEOUT)
    }

    /// [`Service::set_hotkeys_suspended`] with the auto-resume window given by the caller.
    /// Integration tests use it with a short window, since `cfg(test)` is not set for them.
    #[doc(hidden)]
    pub fn set_hotkeys_suspended_for(
        &self,
        suspended: bool,
        timeout: Duration,
    ) -> Result<(), ServiceError> {
        self.call(Op::SetHotkeysSuspended { suspended, timeout })
            .map(|_| ())
    }

    /// Marks the daemon as shutting down (D47). Without `force` it is refused with
    /// [`ErrorCode::Busy`] while a recording session is active. The check and the mark are
    /// one step on the service thread, so no session can start after an accepted call.
    pub fn begin_shutdown(&self, force: bool) -> Result<(), ServiceError> {
        self.call(Op::BeginShutdown { force }).map(|_| ())
    }

    fn call(&self, op: Op) -> Result<Answer, ServiceError> {
        call_via(&self.inner.tx, op, REQUEST_TIMEOUT, RUNNING_GRACE)
    }
}

/// Sends one call to the service thread and waits for its answer, with the
/// timeout and cancel rules described in [`CALL_PENDING`].
fn call_via(
    tx: &Sender<Msg>,
    op: Op,
    timeout: Duration,
    grace: Duration,
) -> Result<Answer, ServiceError> {
    let (reply_tx, reply_rx) = mpsc::channel();
    let state = Arc::new(AtomicU8::new(CALL_PENDING));
    tx.send(Msg::Call(op, reply_tx, Arc::clone(&state)))
        .map_err(|_| stopped())?;
    match reply_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            // Busy is the only retryable code, so it may be reported only when
            // the call is guaranteed not to run. Winning the swap here makes
            // the service skip it. If the service already started it, the call
            // is applied and its real answer is awaited instead.
            if state
                .compare_exchange(
                    CALL_PENDING,
                    CALL_ABANDONED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return Err(ServiceError::new(
                    ErrorCode::Busy,
                    "the recording service did not answer in time; the request was not applied",
                ));
            }
            match reply_rx.recv_timeout(grace) {
                Ok(result) => result,
                Err(RecvTimeoutError::Timeout) => Err(ServiceError::new(
                    ErrorCode::Internal,
                    "the recording service is still working on the request; its outcome is unknown",
                )),
                Err(RecvTimeoutError::Disconnected) => Err(stopped()),
            }
        }
        Err(RecvTimeoutError::Disconnected) => Err(stopped()),
    }
}

fn stopped() -> ServiceError {
    ServiceError::new(ErrorCode::ShuttingDown, "the recording service has stopped")
}

fn unexpected_answer() -> ServiceError {
    ServiceError::new(
        ErrorCode::Internal,
        "unexpected reply from the recording service",
    )
}

/// A game window the service records or would record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    hwnd: isize,
    /// Normalized exe name, the game's identity.
    exe: String,
    pid: u32,
    /// Name used in output file names.
    label: String,
}

/// The session the service last started. Kept after it ends so late part
/// events can still name it.
struct SessionMeta {
    id: String,
    /// Start of the whole session. Set once, when the session begins, and kept across splits.
    started_ms: i64,
    /// Parts finished so far.
    parts_saved: u32,
}

/// State owned by the service thread.
struct Core {
    config: Config,
    data_dir: PathBuf,
    library: Arc<Mutex<Library>>,
    bus: Arc<EventBus>,
    tx: Sender<Msg>,
    no_encoders: bool,
    /// Why recording is unavailable. `None` only when the recorder is ready.
    error: Option<String>,
    /// Recorder settings in effect, once the probe succeeded.
    recorder_cfg: Option<RecorderConfig>,
    encoder: Option<EncoderChoiceInfo>,
    recorder: Option<Recorder>,
    /// Probe report, kept so the recorder can be rebuilt.
    probe: Option<ProbeReport>,
    /// Recorder settings changed and are waiting until the recorder is idle (no session
    /// recording and no part still being written). Holds the strongest apply class of
    /// the waiting changes. Applied by [`Core::apply_pending_if_idle`].
    settings_pending: Option<ApplyClass>,
    /// The recorder is turned on. When off, no game is attached and no session starts;
    /// detection keeps running. Persisted in [`RECORDER_STATE_FILE`].
    recorder_enabled: bool,
    /// Game currently attached to the recorder.
    attached: Option<Target>,
    /// When `attached` was set, in Unix milliseconds.
    attached_since_ms: Option<i64>,
    /// Game that wanted recording before the recorder was ready.
    pending: Option<Target>,
    /// Game detected now that passes the detection rules, whether or not the recorder is
    /// on. Lets turning the recorder on start recording it without a new detection event.
    detected: Option<Target>,
    /// Exe of the most recently attached game, for library rows.
    last_game_exe: Option<String>,
    session: Option<SessionMeta>,
    /// A split announced part N+1 but its predecessor is still being written. The
    /// `SessionPartStarted` for it is published after `SessionPartFinished` for part N.
    /// Holds (session id, part, path).
    pending_part_start: Option<(String, u32, String)>,
    detector: Option<DetectorLoop>,
    hotkeys: Option<HotkeyService>,
    /// True while the hotkey service is down and the user has been told. Keeps a
    /// failed restart from warning again on every later config change.
    hotkeys_down: bool,
    /// Hotkeys are suspended until this instant (see [`Service::set_hotkeys_suspended`]).
    /// `None` when not suspended. An instant in the past counts as not suspended.
    hotkeys_suspended_until: Option<Instant>,
    /// The config file, read on reload and written on patch.
    store: ConfigStore,
    /// The library service. Told about storage changes and when the encoder probe is done.
    libraries: LibraryService,
    /// True while a recording session is being written. Shared with the library service.
    recording: Arc<AtomicBool>,
    /// Set by an accepted shutdown (D47). Blocks new sessions, game attaches and game
    /// detection until the service stops.
    shutting_down: bool,
    /// Crash salvage runs once per service start, before any recorder run exists.
    salvage_started: bool,
}

impl Core {
    fn new(
        opts: ServiceOptions,
        tx: Sender<Msg>,
        libraries: LibraryService,
        recording: Arc<AtomicBool>,
    ) -> Self {
        let store = ConfigStore::new(opts.data_dir.join(configstore::CONFIG_FILE_NAME));
        let recorder_enabled = load_recorder_enabled(&opts.data_dir);
        Self {
            config: opts.config,
            data_dir: opts.data_dir,
            library: opts.library,
            bus: opts.bus,
            tx,
            no_encoders: opts.no_encoders,
            error: Some(PROBE_PENDING.to_owned()),
            recorder_cfg: None,
            encoder: None,
            recorder: None,
            probe: None,
            settings_pending: None,
            recorder_enabled,
            attached: None,
            attached_since_ms: None,
            pending: None,
            detected: None,
            last_game_exe: None,
            session: None,
            pending_part_start: None,
            detector: None,
            hotkeys: None,
            hotkeys_down: false,
            hotkeys_suspended_until: None,
            store,
            libraries,
            recording,
            shutting_down: false,
            salvage_started: false,
        }
    }

    /// Publishes whether a recording session is being written, or a finished part is still
    /// being written by the recorder (D48), for the library service.
    fn sync_recording_flag(&self) {
        let active = self
            .recorder_status()
            .is_some_and(|s| s.session_active || s.finalizing > 0);
        self.recording.store(active, Ordering::SeqCst);
    }

    /// Thread body: starts the background work, then handles messages until stop.
    fn run(&mut self, rx: Receiver<Msg>) {
        self.start_detection();
        self.start_hotkeys();
        self.start_probe();
        spawn_levels_ticker(self.tx.clone());
        logging::info("recording service started");
        while let Ok(msg) = rx.recv() {
            if !self.handle(msg) {
                break;
            }
            // The levels tick wakes this loop every 100 ms, so an expired suspension ends
            // within that window even when nothing else happens.
            self.expire_hotkey_suspension();
            self.sync_recording_flag();
            // Pending settings wait for the recorder to go idle. Recorder events and the
            // levels tick (every 100 ms) both pass through here, so this catches the moment.
            self.apply_pending_if_idle();
        }
        self.shutdown(&rx);
    }

    /// Handles one message. Returns `false` when the service must stop.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Probe(result) => self.on_probe(result),
            Msg::Detector(event) => self.on_detector(event),
            Msg::Hotkey(event) => self.on_hotkey(event.action),
            Msg::Recorder(event) => self.on_recorder(event),
            Msg::Salvaged(outcomes) => self.on_salvaged(outcomes),
            Msg::LevelsTick => {
                if let Some(rec) = self.recorder.as_ref() {
                    if let Some(ev) = levels_event(&rec.audio_levels()) {
                        self.bus.publish(ev);
                    }
                }
            }
            Msg::Call(op, reply, state) => {
                // The caller gave up before this call started, so it must not run.
                if state
                    .compare_exchange(
                        CALL_PENDING,
                        CALL_RUNNING,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_err()
                {
                    logging::warn("an IPC call timed out before it started and was not applied");
                    return true;
                }
                let answer = self.on_call(op);
                let _ = reply.send(answer);
            }
            Msg::Stop => return false,
        }
        true
    }

    // ----- startup ---------------------------------------------------------

    fn start_detection(&mut self) {
        let (tx, rx) = mpsc::channel();
        let poll_ms = self
            .config
            .advanced
            .detect_poll_ms
            .clamp(POLL_MS_RANGE.0, POLL_MS_RANGE.1);
        let poll = Duration::from_millis(u64::from(poll_ms));
        let cfg = detect_config(&self.config);
        self.detector = Some(DetectorLoop::spawn(
            cfg,
            poll,
            Box::new(WinSource::default()),
            tx,
        ));
        forward("detector-events", rx, self.tx.clone(), Msg::Detector);
    }

    fn start_hotkeys(&mut self) {
        let Some(registry) = self.build_hotkey_registry("registered") else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        forward("hotkey-events", rx, self.tx.clone(), Msg::Hotkey);
        match HotkeyService::start_with_threshold(
            &registry,
            tx,
            clipcheck_core::config::schema::LONG_PRESS_MS,
        ) {
            Ok((service, failures)) => {
                self.hotkeys = Some(service);
                // A freshly started service is never suspended, so end any suspension that
                // was still recorded for the one it replaces.
                self.resume_hotkeys("hotkeys restarted; suspension ended");
                if self.hotkeys_down {
                    self.hotkeys_down = false;
                    logging::info("hotkeys running again");
                }
                self.warn_hotkey_failures(failures);
            }
            Err(e) => self.warn_hotkeys_down(format!("hotkeys not started: {e}")),
        }
    }

    /// Reports that the hotkey service is down. The warning is published once per
    /// outage; later failed restarts, which run on each hotkey change, stay quiet.
    fn warn_hotkeys_down(&mut self, message: String) {
        if !self.hotkeys_down {
            self.hotkeys_down = true;
            self.warn("hotkey", message);
        }
    }

    /// Builds the hotkey registry from the current config. Warnings about the
    /// bindings and every conflict are published, using `verb` in the conflict
    /// text ("registered" or "changed"). Returns `None` when conflicts block the set.
    fn build_hotkey_registry(&self, verb: &str) -> Option<Registry> {
        let (bindings, warnings) = settings_map::clip_hotkey_bindings(&self.config);
        for warning in warnings {
            self.warn("hotkey", warning);
        }
        match HotkeyRegistry::build(&bindings, self.config.replay.max_seconds) {
            Ok(registry) => Some(registry),
            Err(conflicts) => {
                for conflict in conflicts {
                    self.warn("hotkey", format!("hotkeys not {verb}: {conflict}"));
                }
                None
            }
        }
    }

    /// Logs and publishes one warning per chord the OS refused. The chords that
    /// did register stay active.
    fn warn_hotkey_failures(&self, failures: Vec<RegisterFailure>) {
        for failure in failures {
            self.warn(
                "hotkey",
                format!(
                    "hotkey {} not registered: {}",
                    failure.chord,
                    describe_failure(&failure.reason)
                ),
            );
        }
    }

    fn start_probe(&mut self) {
        let tx = self.tx.clone();
        let no_encoders = self.no_encoders;
        let spawned = thread::Builder::new()
            .name("encoder-probe".to_owned())
            .spawn(move || {
                let result = probe_encoders(no_encoders);
                let _ = tx.send(Msg::Probe(result));
            });
        if let Err(e) = spawned {
            self.degrade(format!("cannot start encoder probe: {e}"));
        }
    }

    fn on_probe(&mut self, result: Result<ProbeReport, String>) {
        let report = match result {
            Ok(found) => found,
            Err(msg) => {
                self.degrade(msg);
                return;
            }
        };
        let (rc, info) =
            match settings_map::build_recorder_config(&self.config, &report, &self.data_dir) {
                Ok(built) => built,
                Err(e) => {
                    self.degrade(format!("recording unavailable: {e}"));
                    return;
                }
            };

        let (tx, rx) = mpsc::channel();
        forward("recorder-events", rx, self.tx.clone(), Msg::Recorder);
        let codec = codec_name(self.config.video.codec);
        self.bus.publish(Event::EncoderSelected {
            codec: codec.to_owned(),
            encoder: info.encoder_name.clone(),
        });
        if let Some(from) = &info.fell_back_from {
            self.warn(
                "encoder_fallback",
                format!(
                    "the {from} encoder is unavailable; using {}",
                    info.encoder_name
                ),
            );
        }
        logging::info(&format!(
            "recording ready with encoder {}",
            info.encoder_name
        ));

        self.recorder = Some(Recorder::new(rc.clone(), tx));
        self.recorder_cfg = Some(rc);
        self.encoder = Some(info);
        self.libraries.set_ready();
        self.probe = Some(report);
        self.error = None;

        // No recorder run exists yet, so every `run-*` stage left on disk is from a dead process.
        if let Some(rc) = self.recorder_cfg.clone() {
            self.start_salvage(&rc);
        }

        if let Some(target) = self.pending.take() {
            self.attach(target);
        }
    }

    /// Starts crash salvage of staged session parts left by an earlier run. Runs once.
    /// Only the scan is done here; the joining work runs on a worker thread that sends
    /// `Msg::Salvaged` back. Nothing is deleted.
    fn start_salvage(&mut self, rc: &RecorderConfig) {
        if self.salvage_started {
            return;
        }
        self.salvage_started = true;
        let stages = salvage::discover(&rc.work_dir);
        if stages.is_empty() {
            return;
        }
        self.warn(
            "session_recovery",
            format!(
                "recovering {} unsaved session part(s) left by an earlier run",
                stages.len()
            ),
        );
        let sessions_dir = rc.sessions_dir.clone();
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("salvage".to_owned())
            .spawn(move || {
                let outcomes = salvage::salvage_stages(&sessions_dir, stages);
                // The service may have stopped already; a failed send is fine.
                let _ = tx.send(Msg::Salvaged(outcomes));
            });
        if let Err(e) = spawned {
            logging::error(&format!("cannot start the salvage thread: {e}"));
        }
    }

    /// Registers each recovered part as a session part of a new session, marked
    /// recovered, and reports each failure. Session parts from one run share a
    /// session id. Nothing is deleted.
    fn on_salvaged(&mut self, outcomes: Vec<SalvageOutcome>) {
        let mut sessions: HashMap<(PathBuf, u64), (String, u32)> = HashMap::new();
        for outcome in outcomes {
            match outcome {
                SalvageOutcome::Recovered { stage, part } => {
                    let entry = sessions
                        .entry((stage.run_dir.clone(), stage.session_seq))
                        .or_insert_with(|| (Ulid::new().to_string(), 0));
                    entry.1 += 1;
                    let session_id = entry.0.clone();
                    let parts_saved = entry.1;
                    let path = part.path.display().to_string();
                    let duration = match self.register(
                        &part.path,
                        RecordingKind::Session,
                        "",
                        Some(session_id.clone()),
                        Some(stage.part),
                    ) {
                        Ok(id) => {
                            let marked = self
                                .library
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .mark_recovered(&id);
                            if let Err(e) = marked {
                                self.warn(
                                    "library",
                                    format!("recovered part {id} not marked as recovered: {e}"),
                                );
                            }
                            self.library_duration_ms(&id)
                        }
                        Err(msg) => {
                            self.warn(
                                "library",
                                format!("recovered part not added to the library: {msg}"),
                            );
                            None
                        }
                    };
                    self.bus.publish(Event::SessionPartFinished {
                        session_id,
                        part: stage.part,
                        path: path.clone(),
                        duration_ms: duration.unwrap_or(0),
                        duration_unknown: duration.is_none(),
                        bytes: file_len(&part.path),
                        recovered: true,
                        started_unix_ms: started_unix_ms_for(&part.path),
                        parts_saved,
                    });
                    self.warn(
                        "session_recovery",
                        format!(
                            "recovered session part {} from an earlier run: {path}",
                            stage.part
                        ),
                    );
                }
                SalvageOutcome::Failed { stage, error } => self.warn(
                    "session_recovery",
                    format!(
                        "could not recover the staged session parts in {}: {error}; they were kept",
                        stage.stage_dir.display()
                    ),
                ),
            }
        }
    }

    // ----- shutdown ---------------------------------------------------------

    fn shutdown(&mut self, rx: &Receiver<Msg>) {
        self.bus.publish(Event::ShuttingDown {
            reason: "daemon is stopping".to_owned(),
        });
        // A deferred settings change is not worth rebuilding the recorder for on the way out.
        self.settings_pending = None;
        self.detach();
        if let Some(hotkeys) = self.hotkeys.take() {
            hotkeys.stop();
        }
        if let Some(detector) = self.detector.take() {
            detector.stop();
        }
        // Dropping the recorder closes its event channel, so the forwarder drains what
        // detach() queued (for example "session not saved") and then exits.
        self.recorder = None;
        self.drain_recorder_events(rx);
        self.libraries.stop();
        logging::info("recording service stopped");
    }

    /// Handles recorder events that are still queued after the loop has stopped, so
    /// their errors and warnings reach the log instead of being dropped with the channel.
    /// Stops after a quiet period or a hard deadline.
    fn drain_recorder_events(&mut self, rx: &Receiver<Msg>) {
        const QUIET: Duration = Duration::from_millis(100);
        const DEADLINE: Duration = Duration::from_secs(5);
        let deadline = Instant::now() + DEADLINE;
        // Only recorder events extend the quiet period. The 100 ms levels tick (and any
        // other message) must not, or a tick landing in every window keeps the drain
        // running to the deadline and the daemon misses its exit budget.
        let mut quiet_until = Instant::now() + QUIET;
        loop {
            let now = Instant::now();
            let until = quiet_until.min(deadline);
            if now >= until {
                break;
            }
            match rx.recv_timeout(until - now) {
                Ok(Msg::Recorder(event)) => {
                    self.on_recorder(event);
                    quiet_until = Instant::now() + QUIET;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    // ----- detection --------------------------------------------------------

    fn on_detector(&mut self, event: DetectorEvent) {
        match event {
            DetectorEvent::GameStarted { hwnd, exe, pid, .. } => {
                self.game_seen(hwnd, &exe, pid);
            }
            DetectorEvent::GameRelaunched { hwnd, exe, pid, .. } => {
                self.detach_if(&exe);
                self.game_seen(hwnd, &exe, pid);
            }
            DetectorEvent::GameStopped { exe } => {
                if self.pending.as_ref().is_some_and(|p| p.exe == exe) {
                    self.pending = None;
                }
                if self.detected.as_ref().is_some_and(|d| d.exe == exe) {
                    self.detected = None;
                }
                self.detach_if(&exe);
                self.bus.publish(Event::GameLost {
                    game_id: game_id(&exe),
                    reason: "game window closed".to_owned(),
                });
            }
            DetectorEvent::TargetChanged {
                exe,
                old_hwnd: _,
                new_hwnd,
                pid,
            } => {
                if let Some(d) = self.detected.as_mut().filter(|d| d.exe == exe) {
                    d.hwnd = new_hwnd;
                    d.pid = pid.unwrap_or(0);
                }
                let label = match self.attached.as_ref() {
                    Some(target) if target.exe == exe => target.label.clone(),
                    _ => return,
                };
                self.detach();
                self.attach(Target {
                    hwnd: new_hwnd,
                    exe,
                    pid: pid.unwrap_or(0),
                    label,
                });
            }
        }
    }

    /// Reports a detected game and starts recording it when the settings allow.
    fn game_seen(&mut self, hwnd: isize, exe: &str, pid: Option<u32>) {
        let pid = pid.unwrap_or(0);
        crate::gameicons::ensure_async(&self.data_dir, &game_id(exe), pid);
        self.bus.publish(Event::GameDetected {
            game: GameRef {
                game_id: game_id(exe),
                exe: exe.to_owned(),
                pid,
                title: None,
            },
        });
        // During shutdown (D47) nothing new is attached or queued.
        if self.shutting_down || !self.should_record(exe) {
            return;
        }
        let target = Target {
            hwnd,
            exe: exe.to_owned(),
            pid,
            label: self.label_for(exe),
        };
        // Remembered even while the recorder is off, so turning it on can record this game.
        self.detected = Some(target.clone());
        if !self.recorder_enabled {
            logging::info(&format!("{exe} detected; the recorder is turned off"));
            return;
        }
        if self.recorder.is_some() {
            self.attach(target);
        } else {
            logging::info(&format!("{exe} will be recorded once recording is ready"));
            self.pending = Some(target);
        }
    }

    /// True when auto-record is on and the game passes the allowlist mode.
    fn should_record(&self, exe: &str) -> bool {
        let detection = &self.config.detection;
        let allow: Vec<&str> = detection.allow.iter().map(|e| e.as_str()).collect();
        let block: Vec<&str> = detection.block.iter().map(|e| e.as_str()).collect();
        record_permitted(
            exe,
            detection.auto_record,
            matches!(detection.mode, DetectionMode::Allowlist),
            &allow,
            &block,
        )
    }

    fn label_for(&self, exe: &str) -> String {
        display_override(&self.config, exe)
            .or_else(|| known_games::stable_name(exe).map(str::to_owned))
            .unwrap_or_else(|| exe.strip_suffix(".exe").unwrap_or(exe).to_owned())
    }

    fn attach(&mut self, target: Target) {
        // During shutdown (D47) no new game is attached. Nothing is attached while the recorder is off.
        if self.shutting_down || !self.recorder_enabled {
            return;
        }
        // A detector restart reports the running game again; it is already attached.
        if self
            .attached
            .as_ref()
            .is_some_and(|t| t.hwnd == target.hwnd && t.exe == target.exe)
        {
            return;
        }
        let result = match self.recorder.as_mut() {
            Some(recorder) => recorder.attach(target.hwnd, &target.label),
            None => return,
        };
        match result {
            Ok(()) => {
                logging::info(&format!("recording {}", target.exe));
                self.last_game_exe = Some(target.exe.clone());
                self.attached_since_ms = Some(now_unix_ms());
                let auto_session = self
                    .recorder
                    .as_ref()
                    .is_some_and(|r| r.status().session_active);
                self.attached = Some(target);
                // The recorder starts a session on attach when detection.auto_record is set.
                if auto_session {
                    self.begin_session();
                }
            }
            Err(e) => {
                let msg = format!("cannot record {}: {e}", target.exe);
                self.warn("attach_failed", msg);
            }
        }
    }

    /// Stops capture for the attached game. A session that is still recording is not
    /// stopped here; callers that end a session stop it first. Deferred settings are not
    /// applied here: the service loop applies them once the recorder is idle.
    fn detach(&mut self) {
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.detach();
        }
        if let Some(target) = self.attached.take() {
            logging::info(&format!("stopped recording {}", target.exe));
        }
        self.attached_since_ms = None;
    }

    /// Turns the recorder on or off and persists the choice. Off: a session that is
    /// recording is stopped and its part is saved up to this point, then the game is
    /// detached. On: a game that is still detected starts recording again.
    fn set_recorder_enabled(&mut self, enabled: bool) -> Result<(), ServiceError> {
        if self.shutting_down {
            return Err(ServiceError::new(
                ErrorCode::ShuttingDown,
                "the daemon is shutting down",
            ));
        }
        save_recorder_enabled(&self.data_dir, enabled)
            .map_err(|msg| ServiceError::new(ErrorCode::Internal, msg))?;
        self.recorder_enabled = enabled;
        if enabled {
            logging::info("recorder turned on");
            if self.attached.is_none() {
                if let Some(target) = self.detected.clone() {
                    if self.recorder.is_some() {
                        self.attach(target);
                    } else {
                        self.pending = Some(target);
                    }
                }
            }
        } else {
            logging::info("recorder turned off");
            self.pending = None;
            // The same stop a user's stop-session makes. The part is queued and written
            // in the background (D48), and detach() below finds no session left.
            let stop = match self.recorder.as_mut() {
                Some(recorder) if recorder.status().session_active => Some(recorder.session_stop()),
                _ => None,
            };
            if let Some(Err(e)) = stop {
                self.warn(
                    "recorder",
                    format!(
                        "the session could not be stopped when the recorder was turned off: {e}"
                    ),
                );
            }
            self.detach();
        }
        self.apply_pending_if_idle();
        let status = self.status_dto();
        self.bus.publish(Event::StateChanged {
            state: status.state,
            game: status.game,
        });
        Ok(())
    }

    /// Applies the settings that wait for the recorder to go idle. Idle means no session
    /// is recording and no finished part is still being written. A game that is attached
    /// but not in a session counts as idle, so the change applies then too. The recorder is
    /// rebuilt and the attached game is attached again to the new recorder.
    fn apply_pending_if_idle(&mut self) {
        let Some(class) = self.settings_pending else {
            return;
        };
        if self.defers_recorder_change() {
            return;
        }
        self.settings_pending = None;
        logging::info("applying deferred recorder settings");
        self.rebuild_recorder();
        self.bus.publish(Event::ConfigChanged {
            apply_class: configstore::class_name(class).to_owned(),
            restart_required: false,
        });
    }

    /// Fails with `invalid_state` while the recorder is turned off.
    fn require_recorder_enabled(&self) -> Result<(), ServiceError> {
        if self.recorder_enabled {
            Ok(())
        } else {
            Err(ServiceError::new(
                ErrorCode::InvalidState,
                RECORDER_OFF_MESSAGE,
            ))
        }
    }

    fn detach_if(&mut self, exe: &str) {
        if self.attached.as_ref().is_some_and(|t| t.exe == exe) {
            self.detach();
        }
    }

    // ----- hotkeys ----------------------------------------------------------

    fn on_hotkey(&mut self, action: HotkeyAction) {
        // Belt and braces: the hotkey service already ignores chords while suspended, so an
        // event that was queued before the suspension started must not act.
        if self.hotkeys_suspended() {
            logging::debug("hotkey ignored while hotkeys are suspended");
            return;
        }
        match action {
            HotkeyAction::ClipLast { name, seconds } => {
                if let Err(e) = self.clip(seconds) {
                    self.warn(
                        "clip_hotkey",
                        format!("clip '{name}' failed: {}", e.message),
                    );
                }
            }
            HotkeyAction::SessionSplit => {
                if let Err(e) = self.split_session() {
                    self.warn("hotkey", format!("split failed: {}", e.message));
                }
            }
            HotkeyAction::SessionStopStart => {
                let active = self.recorder_status().is_some_and(|s| s.session_active);
                let result = if active {
                    self.stop_session()
                } else {
                    self.start_session()
                };
                if let Err(e) = result {
                    self.warn(
                        "hotkey",
                        format!("session start/stop failed: {}", e.message),
                    );
                }
            }
            HotkeyAction::ToggleBuffer | HotkeyAction::MuteMic => {
                logging::info("hotkey action not implemented in this build; ignored");
            }
        }
    }

    // ----- settings ---------------------------------------------------------

    /// True while a hotkey suspension is in effect. A deadline in the past does not count.
    fn hotkeys_suspended(&self) -> bool {
        self.hotkeys_suspended_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// Suspends the hotkeys for `timeout` from now, or resumes them when `suspended` is false.
    fn set_hotkeys_suspended(&mut self, suspended: bool, timeout: Duration) {
        if !suspended {
            self.resume_hotkeys("hotkeys resumed");
            return;
        }
        if !self.hotkeys_suspended() {
            logging::info("hotkeys suspended while a hotkey is being captured");
        }
        self.hotkeys_suspended_until = Some(Instant::now() + timeout);
        if let Some(service) = self.hotkeys.as_ref() {
            set_hotkey_suspension(service, true);
        }
    }

    /// Ends any suspension and tells the hotkey service to listen again. `reason` is
    /// logged only when a suspension was actually in effect.
    fn resume_hotkeys(&mut self, reason: &str) {
        if self.hotkeys_suspended_until.take().is_some() {
            logging::info(reason);
        }
        if let Some(service) = self.hotkeys.as_ref() {
            set_hotkey_suspension(service, false);
        }
    }

    /// Ends a suspension whose window has run out without a renewal.
    fn expire_hotkey_suspension(&mut self) {
        if self.hotkeys_suspended_until.is_some() && !self.hotkeys_suspended() {
            self.resume_hotkeys("hotkey suspension timed out; hotkeys are live again");
        }
    }

    /// Applies a new configuration in memory. The detector, hotkeys and recorder
    /// each take the change as their settings require. Returns the strongest apply
    /// class and whether part of the change waits for the next detach.
    fn apply_config(&mut self, new: Config) -> (ApplyClass, bool) {
        self.resume_hotkeys("config changed; hotkey suspension ended");
        let old = std::mem::replace(&mut self.config, new);
        let class = apply_class(&old, &self.config);
        if old.storage != self.config.storage {
            self.libraries.storage_changed(self.config.storage.clone());
        }

        // Compare what the detector receives, not the raw config. Replay length and
        // output folder are not detector inputs, so editing them keeps detection running.
        let detector_changed = detect_config(&old) != detect_config(&self.config)
            || old.advanced.detect_poll_ms != self.config.advanced.detect_poll_ms;
        let hotkeys_changed = old.hotkeys != self.config.hotkeys
            || old.replay.max_seconds != self.config.replay.max_seconds;

        if detector_changed {
            self.restart_detection();
        }
        if hotkeys_changed {
            self.apply_hotkeys();
        }
        if recorder_settings_changed(&old, &self.config) && self.probe.is_some() {
            self.settings_pending = Some(self.settings_pending.map_or(class, |p| p.max(class)));
        }
        // Applies at once when the recorder is already idle. Otherwise it waits.
        self.apply_pending_if_idle();
        (class, self.settings_pending.is_some())
    }

    /// True while a session records or a finished part is still being written. The
    /// recorder is not rebuilt then, so the change waits until it is idle. A game that is
    /// attached without a session does not defer the change.
    fn defers_recorder_change(&self) -> bool {
        self.recorder_status()
            .is_some_and(|s| s.session_active || s.finalizing > 0)
    }

    /// Rebuilds the recorder from the current config and the stored probe. Only
    /// called when no session is recording and no part is being written. A game that
    /// is attached stays attached: it is attached again to the new recorder.
    fn rebuild_recorder(&mut self) {
        let Some(report) = self.probe.as_ref() else {
            return;
        };
        let (rc, info) =
            match settings_map::build_recorder_config(&self.config, report, &self.data_dir) {
                Ok(built) => built,
                Err(e) => {
                    self.warn("settings", format!("recorder settings not applied: {e}"));
                    return;
                }
            };
        let kept = self.attached.take();
        if kept.is_some() {
            if let Some(recorder) = self.recorder.as_mut() {
                recorder.detach();
            }
        }
        let (tx, rx) = mpsc::channel();
        forward("recorder-events", rx, self.tx.clone(), Msg::Recorder);
        // Drop the old recorder first so its threads and buffer are released.
        self.recorder = None;
        self.recorder = Some(Recorder::new(rc.clone(), tx));
        self.recorder_cfg = Some(rc);
        self.bus.publish(Event::EncoderSelected {
            codec: codec_name(self.config.video.codec).to_owned(),
            encoder: info.encoder_name.clone(),
        });
        self.encoder = Some(info);
        logging::info("recorder rebuilt with the new settings");
        if let Some(target) = kept {
            self.attach(target);
        }
    }

    /// Applies the hotkey set live. If hotkeys never registered, this starts them.
    fn apply_hotkeys(&mut self) {
        if self.hotkeys.is_none() {
            self.start_hotkeys();
            return;
        }
        let Some(registry) = self.build_hotkey_registry("changed") else {
            return;
        };
        let Some(service) = self.hotkeys.as_ref() else {
            return;
        };
        match service.apply(registry) {
            Ok(failures) if failures.is_empty() => logging::info("hotkeys updated"),
            Ok(failures) => {
                logging::info("hotkeys updated with some chords refused");
                self.warn_hotkey_failures(failures);
            }
            Err(ApplyError::Stopped) => {
                // The hotkey thread is gone, so this handle can never apply again.
                // Drop it and start a fresh service from the current config now. If
                // that fails too, `hotkeys` stays `None` and the next change retries.
                self.warn(
                    "hotkey",
                    "hotkey service stopped; restarting it from the current config".to_owned(),
                );
                if let Some(dead) = self.hotkeys.take() {
                    dead.stop();
                }
                self.start_hotkeys();
            }
        }
    }

    /// Stops the detector and starts one with the current detection settings.
    fn restart_detection(&mut self) {
        if let Some(detector) = self.detector.take() {
            detector.stop();
        }
        self.start_detection();
    }

    /// Current configuration for `get_config`.
    fn config_view(&self) -> Result<ConfigView, ServiceError> {
        let config = configstore::to_value(&self.config)
            .map_err(|e| ServiceError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(ConfigView {
            config,
            version: self.config.config_version,
            path: self.store.path().display().to_string(),
        })
    }

    /// Merges, validates, saves and applies a patch. With `dry_run` nothing is saved.
    fn patch_config(
        &mut self,
        patch: &Value,
        dry_run: bool,
    ) -> Result<ConfigOutcome, ServiceError> {
        let next = configstore::apply_patch(&self.config, patch)
            .map_err(|e| ServiceError::new(ErrorCode::BadRequest, e.to_string()))?;
        let diagnostics = validate(&next);
        if configstore::has_errors(&diagnostics) {
            return Ok(ConfigOutcome::Rejected(diagnostics));
        }
        let class = apply_class(&self.config, &next);
        if dry_run {
            let restart_required = self.probe.is_some()
                && recorder_settings_changed(&self.config, &next)
                && self.defers_recorder_change();
            return Ok(ConfigOutcome::Checked {
                class,
                restart_required,
                warnings: diagnostics,
            });
        }
        self.store
            .save(&next)
            .map_err(|msg| ServiceError::new(ErrorCode::Internal, msg))?;
        let (class, restart_required) = self.apply_config(next);
        self.publish_config_changed(class, restart_required);
        Ok(ConfigOutcome::Applied {
            class,
            restart_required,
            warnings: diagnostics,
        })
    }

    /// Re-reads the file. A file with errors is rejected and the current config stays.
    fn reload_config(&mut self) -> Result<ConfigOutcome, ServiceError> {
        self.resume_hotkeys("config reloaded; hotkey suspension ended");
        let (next, mut diagnostics) = match self.store.read_checked() {
            Ok(checked) => checked,
            Err(diagnostics) => return Ok(ConfigOutcome::Rejected(diagnostics)),
        };
        diagnostics.extend(validate(&next));
        if configstore::has_errors(&diagnostics) {
            return Ok(ConfigOutcome::Rejected(diagnostics));
        }
        if next == self.config {
            return Ok(ConfigOutcome::Applied {
                class: ApplyClass::Live,
                restart_required: self.settings_pending.is_some(),
                warnings: diagnostics,
            });
        }
        let (class, restart_required) = self.apply_config(next);
        self.publish_config_changed(class, restart_required);
        Ok(ConfigOutcome::Applied {
            class,
            restart_required,
            warnings: diagnostics,
        })
    }

    fn publish_config_changed(&self, class: ApplyClass, restart_required: bool) {
        self.bus.publish(Event::ConfigChanged {
            apply_class: configstore::class_name(class).to_owned(),
            restart_required,
        });
    }

    // ----- recorder events --------------------------------------------------

    fn on_recorder(&mut self, event: RecorderEvent) {
        match event {
            RecorderEvent::ClipSaved {
                job,
                path,
                seconds_requested: _,
                seconds_actual,
                short,
            } => {
                if short {
                    logging::info(&format!("clip {} is shorter than requested", job.0));
                }
                let game = self.last_game_id();
                if let Err(msg) = self.register(&path, RecordingKind::Clip, &game, None, None) {
                    self.warn("library", format!("clip not added to the library: {msg}"));
                }
                let duration_ms = seconds_to_ms(seconds_actual);
                self.bus.publish(Event::ClipSaved {
                    clip_id: job.0.to_string(),
                    path: path.display().to_string(),
                    duration_ms,
                    bytes: file_len(&path),
                });
                // Both feedbacks are non-blocking: the sound plays asynchronously and
                // the toast is handed off to its own window.
                crate::sound::play_clip_saved();
                let toast_seconds = (seconds_actual.round() as u32).max(1);
                crate::toast::clip_saved(toast_seconds);
            }
            RecorderEvent::ClipFailed { job, error } => {
                self.warn("clip_failed", format!("clip {} failed: {error}", job.0));
                // Only failures reported by the recorder reach this arm. Requests
                // refused before the recorder (such as "no window is attached") are
                // returned to the caller or logged elsewhere and are not toasted.
                crate::toast::message("Clip failed");
            }
            RecorderEvent::SessionPartSaved {
                path,
                part_index,
                final_part: _,
            } => {
                // A finished part always belongs to the session that recorded it. With
                // no session there is nothing to attach it to, so log it and stop.
                let Some(session) = self.session.as_mut() else {
                    self.warn(
                        "session_part",
                        format!(
                            "session part {part_index} was saved with no active session; not published: {}",
                            path.display()
                        ),
                    );
                    return;
                };
                session.parts_saved += 1;
                let session_id = session.id.clone();
                let started_unix_ms = session.started_ms;
                let parts_saved = session.parts_saved;
                let game = self.last_game_id();
                // One probe per part: registration probes the file and stores the
                // duration, so the row is read back instead of probing again.
                let duration = match self.register(
                    &path,
                    RecordingKind::Session,
                    &game,
                    Some(session_id.clone()),
                    Some(part_index),
                ) {
                    Ok(id) => self.library_duration_ms(&id),
                    Err(msg) => {
                        // The probe ran once inside `register`; it is not repeated here.
                        self.warn(
                            "library",
                            format!("session part not added to the library: {msg}"),
                        );
                        None
                    }
                };
                let duration_unknown = duration.is_none();
                self.bus.publish(Event::SessionPartFinished {
                    session_id,
                    part: part_index,
                    path: path.display().to_string(),
                    duration_ms: duration.unwrap_or(0),
                    duration_unknown,
                    bytes: file_len(&path),
                    recovered: false,
                    started_unix_ms,
                    parts_saved,
                });
                // The split that asked for part N+1 waits for this Finished event.
                self.announce_pending_part_start(part_index);
            }
            RecorderEvent::SessionPartFailed {
                part_index,
                final_part: _,
                error,
                kept_stage,
            } => {
                // No SessionPartFinished for a failed part. Its footage stays in the kept
                // stage directory. The following `Error` event carries the session message.
                self.warn(
                    "session_part",
                    format!(
                        "session part {part_index} could not be saved; its footage is kept on disk: {error} ({})",
                        kept_stage.display()
                    ),
                );
                // The split that asked for part N+1 must still be announced, even though
                // part N produced no file.
                self.announce_pending_part_start(part_index);
            }
            RecorderEvent::Error(msg) => self.warn("recorder", msg),
            RecorderEvent::Warning(msg) => self.warn("recorder_warning", msg),
        }
    }

    /// Announces the pending part N+1 once part N has been reported (saved or failed).
    /// Keeps the pending split when it waits for a different part.
    fn announce_pending_part_start(&mut self, part_index: u32) {
        if let Some((next_session, next_part, next_path)) = self.pending_part_start.take() {
            if next_part == part_index + 1 {
                self.bus.publish(Event::SessionPartStarted {
                    session_id: next_session,
                    part: next_part,
                    path: next_path,
                });
            } else {
                self.pending_part_start = Some((next_session, next_part, next_path));
            }
        }
    }

    /// Registers a finished file in the library. Returns the recording id.
    ///
    /// Same steps as `register_recording`, but the library lock is held only for the
    /// lookup and the insert. The media probe and the poster are done between them,
    /// without the lock, so library readers are not blocked by the probe.
    fn register(
        &self,
        path: &Path,
        kind: RecordingKind,
        game: &str,
        session_id: Option<String>,
        part_index: Option<u32>,
    ) -> Result<String, String> {
        let rc = self
            .recorder_cfg
            .as_ref()
            .ok_or_else(|| "recording is not ready".to_owned())?;
        // Clips and sessions sit under <root>/clips and <root>/sessions.
        let root = rc
            .clips_dir
            .parent()
            .ok_or_else(|| "storage root is unknown".to_owned())?;
        let thumbs = self.data_dir.join(THUMBS_DIR);
        let existing = {
            let library = self.library.lock().unwrap_or_else(|e| e.into_inner());
            find_registered(&library, root, path).map_err(|e| e.to_string())?
        };
        if let Some(id) = existing {
            return Ok(id);
        }
        let prepared = prepare_recording(&thumbs, root, path).map_err(|e| e.to_string())?;
        let library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        commit_prepared(
            &library,
            prepared,
            RegisterMeta {
                kind,
                game,
                session_id,
                part_index,
                created_ms: now_ms(),
            },
            RecordingSource::Recorded,
        )
        .map_err(|e| e.to_string())
    }

    /// Duration stored on a registered recording row. `None` when the row is
    /// missing, cannot be read, or holds a negative duration; each case is logged.
    fn library_duration_ms(&self, recording_id: &str) -> Option<u64> {
        let library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        match library.get_recording(recording_id) {
            Ok(Some(row)) => {
                let duration = u64::try_from(row.duration_ms).ok();
                if duration.is_none() {
                    logging::warn(&format!(
                        "recording {recording_id} has a negative duration in the library; duration unknown"
                    ));
                }
                duration
            }
            Ok(None) => {
                logging::warn(&format!(
                    "recording {recording_id} is missing from the library; duration unknown"
                ));
                None
            }
            Err(e) => {
                logging::warn(&format!(
                    "cannot read recording {recording_id} from the library: {e}; duration unknown"
                ));
                None
            }
        }
    }

    fn last_game_id(&self) -> String {
        self.last_game_exe
            .as_deref()
            .map(game_id)
            .unwrap_or_default()
    }

    // ----- IPC calls --------------------------------------------------------

    fn on_call(&mut self, op: Op) -> Result<Answer, ServiceError> {
        match op {
            Op::Status => Ok(Answer::Status(Box::new(self.status_dto()))),
            Op::StartSession => self.start_session().map(|()| Answer::Done),
            Op::StopSession => self.stop_session().map(|()| Answer::Done),
            Op::SplitSession => self.split_session().map(|()| Answer::Done),
            Op::Clip(seconds) => self.clip(seconds).map(Answer::Clip),
            Op::GetConfig => self.config_view().map(Answer::Config),
            Op::PatchConfig { patch, dry_run } => {
                self.patch_config(&patch, dry_run).map(Answer::Outcome)
            }
            Op::ReloadConfig => self.reload_config().map(Answer::Outcome),
            Op::Encoders => Ok(Answer::Encoders(self.probe.clone())),
            Op::BeginShutdown { force } => self.begin_shutdown(force).map(|()| Answer::Done),
            Op::SetRecorderEnabled(enabled) => {
                self.set_recorder_enabled(enabled).map(|()| Answer::Done)
            }
            Op::SetHotkeysSuspended { suspended, timeout } => {
                self.set_hotkeys_suspended(suspended, timeout);
                Ok(Answer::Done)
            }
        }
    }

    /// Accepts a shutdown (D47). Graceful and update requests are refused with `busy`
    /// while a recording session is active, and change nothing then. A forced request
    /// always marks the service as shutting down; the normal shutdown path detaches and
    /// finalizes any active session.
    fn begin_shutdown(&mut self, force: bool) -> Result<(), ServiceError> {
        if !force && self.recorder_status().is_some_and(|s| s.session_active) {
            return Err(ServiceError::new(
                ErrorCode::Busy,
                "A recording session is in progress",
            ));
        }
        self.shutting_down = true;
        Ok(())
    }

    fn recorder_mut(&mut self) -> Result<&mut Recorder, ServiceError> {
        match self.recorder.as_mut() {
            Some(recorder) => Ok(recorder),
            None => Err(ServiceError::new(
                ErrorCode::WorkerUnavailable,
                self.error
                    .clone()
                    .unwrap_or_else(|| "recording is not ready".to_owned()),
            )),
        }
    }

    fn recorder_status(&self) -> Option<clipcheck_engine::recorder::RecorderStatus> {
        self.recorder.as_ref().map(Recorder::status)
    }

    fn start_session(&mut self) -> Result<(), ServiceError> {
        if self.shutting_down {
            return Err(ServiceError::new(
                ErrorCode::ShuttingDown,
                "the daemon is shutting down",
            ));
        }
        self.require_recorder_enabled()?;
        self.recorder_mut()?
            .session_start()
            .map_err(to_service_error)?;
        self.begin_session();
        Ok(())
    }

    /// Gives a newly started session its id and announces part 1. Used for
    /// manual starts and for sessions the recorder starts on attach.
    fn begin_session(&mut self) {
        let id = Ulid::new().to_string();
        let path = self.sessions_dir();
        let started_ms = now_unix_ms();
        self.session = Some(SessionMeta {
            id: id.clone(),
            started_ms,
            parts_saved: 0,
        });
        self.pending_part_start = None;
        self.bus.publish(Event::SessionStarted {
            session: SessionInfo {
                session_id: id.clone(),
                part: 1,
                started_unix_ms: started_ms,
                path: path.clone(),
                parts_saved: 0,
            },
        });
        self.bus.publish(Event::SessionPartStarted {
            session_id: id,
            part: 1,
            path,
        });
    }

    fn stop_session(&mut self) -> Result<(), ServiceError> {
        self.require_recorder_enabled()?;
        self.require_session()?;
        self.recorder_mut()?
            .session_stop()
            .map_err(to_service_error)
    }

    fn split_session(&mut self) -> Result<(), ServiceError> {
        self.require_recorder_enabled()?;
        self.require_session()?;
        // Checked before the split starts, so a missing session id never reaches an event.
        let session_id =
            self.session.as_ref().map(|s| s.id.clone()).ok_or_else(|| {
                ServiceError::new(ErrorCode::InvalidState, "no session is active")
            })?;
        let path = self.sessions_dir();
        let recorder = self.recorder_mut()?;
        let next_part = recorder.status().part_index + 1;
        let split = recorder.session_split().map_err(to_service_error);
        match split {
            Ok(()) => {
                // Part N is queued on the recorder's finalizer thread and reported later
                // through `SessionPartSaved`, or `SessionPartFailed` if the write fails.
                // Announce N+1 only after N has been reported.
                self.pending_part_start = Some((session_id, next_part, path));
                Ok(())
            }
            Err(e) => {
                // `session_split` swaps in part N+1 before it finishes part N, so an
                // error after the swap means part N was not written while N+1 records.
                // Keep the state consistent: announce N+1, report the failure as an
                // error event, and treat the split as done. Part N's staged segments
                // are kept on disk by the recorder.
                let swapped = self
                    .recorder_status()
                    .is_some_and(|s| s.session_active && s.part_index == next_part);
                if !swapped {
                    return Err(e);
                }
                self.bus.publish(Event::SessionPartStarted {
                    session_id,
                    part: next_part,
                    path,
                });
                self.warn(
                    "session_split",
                    format!(
                        "session part {} could not be saved; its footage is kept on disk: {}",
                        next_part - 1,
                        e.message
                    ),
                );
                Ok(())
            }
        }
    }

    fn require_session(&self) -> Result<(), ServiceError> {
        match self.recorder_status() {
            Some(status) if status.session_active => Ok(()),
            Some(_) => Err(ServiceError::new(
                ErrorCode::InvalidState,
                "no session is active",
            )),
            None => Err(ServiceError::new(
                ErrorCode::WorkerUnavailable,
                self.error
                    .clone()
                    .unwrap_or_else(|| "recording is not ready".to_owned()),
            )),
        }
    }

    fn clip(&mut self, seconds: u32) -> Result<String, ServiceError> {
        // Single source of the clip-length rule. IPC and hotkeys both reach this check.
        if !(1..=MAX_CLIP_SECONDS).contains(&seconds) {
            return Err(ServiceError::new(
                ErrorCode::BadRequest,
                format!("clip length must be 1..={MAX_CLIP_SECONDS} seconds"),
            ));
        }
        self.require_recorder_enabled()?;
        let job = self
            .recorder_mut()?
            .clip(seconds)
            .map_err(to_service_error)?;
        Ok(job.0.to_string())
    }

    fn status_dto(&self) -> StatusDto {
        let rs = self.recorder_status();
        let session_active = rs.as_ref().is_some_and(|s| s.session_active);
        // A probe that is merely still running is a normal startup phase, not a failure.
        let probing = self.error.as_deref() == Some(PROBE_PENDING);
        let state = if probing {
            RecState::Idle
        } else if self.error.is_some() {
            RecState::Degraded
        } else if session_active {
            RecState::Recording
        } else if !self.recorder_enabled {
            RecState::Idle
        } else if self.attached.is_some() {
            RecState::BufferOnly
        } else {
            RecState::Armed
        };
        let session = if session_active {
            self.session.as_ref().map(|s| SessionInfo {
                session_id: s.id.clone(),
                part: rs.as_ref().map_or(0, |r| r.part_index),
                started_unix_ms: s.started_ms,
                path: self.sessions_dir(),
                parts_saved: s.parts_saved,
            })
        } else {
            None
        };
        StatusDto {
            state,
            game: self.attached.as_ref().map(|t| GameRef {
                game_id: game_id(&t.exe),
                exe: t.exe.clone(),
                pid: t.pid,
                title: None,
            }),
            game_info: self.attached.as_ref().map(|t| GameInfo {
                name: t.label.clone(),
                exe: t.exe.clone(),
                hwnd: t.hwnd as i64,
                attached_since_unix_ms: self.attached_since_ms.unwrap_or(0),
            }),
            session,
            buffer_seconds: rs.as_ref().map_or(0, |r| secs_to_u32(r.buffer_secs)),
            buffer_secs: rs.as_ref().map_or(0.0, |r| r.buffer_secs as f32),
            settings_pending_restart: self.settings_pending.is_some(),
            recorder_enabled: self.recorder_enabled,
            server_unix_ms: now_unix_ms(),
            // Set by the daemon layer (`app::AppState::status`).
            exe_path: None,
            // No byte counter exists in the buffer, so this is an estimate: buffered
            // seconds times the configured bitrate (kbps / 8 * 1000 bytes per second).
            buffer_bytes: rs.as_ref().map_or(0, |r| {
                let kbps = self.recorder_cfg.as_ref().map_or(0, |c| c.bitrate_kbps);
                (r.buffer_secs * f64::from(kbps) * 125.0) as u64
            }),
            encoder: self.encoder.as_ref().map(|e| e.encoder_name.clone()),
            codec: self
                .encoder
                .as_ref()
                .map(|_| codec_name(self.config.video.codec).to_owned()),
            fps_target: self.recorder_cfg.as_ref().map_or(0, |c| c.fps),
            bitrate_kbps: self.recorder_cfg.as_ref().map(|c| c.bitrate_kbps),
            // The DTO has no unknown state for these fields, so each value must be the
            // real state. There is no mute feature (MuteMic is ignored), and capture runs
            // in-process on threads, so there is no worker process to report or restart.
            // `autostart` and `uptime_s` are overwritten by the app layer (`AppState::status`).
            mic_muted: false,
            // Detection runs from startup until the service stops it, so this is true only
            // when no detector is running.
            detection_paused: self.detector.is_none(),
            autostart: AutostartState::Disabled,
            worker_pid: None,
            worker_restarts_10min: 0,
            hotkeys_down: self.hotkeys_down,
            hotkeys_suspended: self.hotkeys_suspended(),
            finalizing_parts: self.recorder_status().map_or(0, |s| s.finalizing),
            uptime_s: 0,
            degraded_reason: if probing { None } else { self.error.clone() },
        }
    }

    /// Sessions folder for clients. Only called while a session can exist, so a
    /// missing recorder config is logged and the path is sent empty.
    fn sessions_dir(&self) -> String {
        match self.recorder_cfg.as_ref() {
            Some(c) => c.sessions_dir.display().to_string(),
            None => {
                logging::warn("sessions folder unknown: the recorder config is missing");
                String::new()
            }
        }
    }

    // ----- reporting --------------------------------------------------------

    /// Records why recording is unavailable and tells subscribers.
    fn degrade(&mut self, reason: String) {
        logging::error(&reason);
        self.bus.publish(Event::Degraded {
            reason: reason.clone(),
        });
        self.error = Some(reason);
    }

    /// Logs a non-fatal problem and sends it as a `warning` event.
    fn warn(&self, code: &str, message: String) {
        logging::warn(&message);
        self.bus.publish(Event::Warning {
            code: code.to_owned(),
            message,
        });
    }
}

/// Reads the recorder on/off choice. Absent or unreadable means on, so a damaged
/// state file never leaves the recorder off by accident.
fn load_recorder_enabled(data_dir: &Path) -> bool {
    let path = data_dir.join(RECORDER_STATE_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
        Err(e) => {
            logging::warn(&format!(
                "cannot read {}; the recorder stays on: {e}",
                path.display()
            ));
            return true;
        }
    };
    match serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("enabled").and_then(Value::as_bool))
    {
        Some(enabled) => enabled,
        None => {
            logging::warn(&format!(
                "{} is not valid; the recorder stays on",
                path.display()
            ));
            true
        }
    }
}

/// Writes the recorder on/off choice through a temporary file, so a crash never leaves
/// a half-written state file.
fn save_recorder_enabled(data_dir: &Path, enabled: bool) -> Result<(), String> {
    let path = data_dir.join(RECORDER_STATE_FILE);
    let tmp = data_dir.join(format!("{RECORDER_STATE_FILE}.tmp"));
    let text = serde_json::json!({ "enabled": enabled }).to_string();
    fs::write(&tmp, text)
        .and_then(|()| fs::rename(&tmp, &path))
        .map_err(|e| format!("cannot save {}: {e}", path.display()))
}

/// Runs the encoder probe in process (Media Foundation, D58). Returns the report, or a
/// message for the degraded-mode status.
///
/// The probe runs at each start and is not cached, so a driver change is seen
/// without a fingerprint.
fn probe_encoders(no_encoders: bool) -> Result<ProbeReport, String> {
    if no_encoders {
        return Err("encoders disabled for this run; recording is off".to_owned());
    }
    native_probe()
}

/// The Media Foundation probe. Recording needs Windows.
#[cfg(windows)]
fn native_probe() -> Result<ProbeReport, String> {
    let report = probe_native();
    if !report.encoders.iter().any(|e| e.works) {
        return Err(
            "no working video encoder found (Media Foundation); recording is off".to_owned(),
        );
    }
    Ok(report)
}

/// The Media Foundation probe. Recording needs Windows.
#[cfg(not(windows))]
fn native_probe() -> Result<ProbeReport, String> {
    Err("recording needs Windows".to_owned())
}

/// Interval between audio level samples on the `levels` topic.
const LEVELS_INTERVAL: Duration = Duration::from_millis(100);

/// Level reported for a non-finite dB value (NaN or -inf).
const LEVEL_FLOOR_DB: f32 = -120.0;

/// Builds the `AudioLevels` event for the given per-track snapshots. Returns `None`
/// when there are no tracks. At most 255 tracks are reported, numbered from 0.
fn levels_event(snapshots: &[LevelSnapshot]) -> Option<Event> {
    if snapshots.is_empty() {
        return None;
    }
    let finite = |db: f32| if db.is_finite() { db } else { LEVEL_FLOOR_DB };
    let tracks = snapshots
        .iter()
        .take(usize::from(u8::MAX))
        .enumerate()
        .map(|(i, s)| TrackLevel {
            track: i as u8,
            peak_db: finite(s.peak_db),
            rms_db: finite(s.rms_db),
        })
        .collect();
    Some(Event::AudioLevels { tracks })
}

/// Starts the detached "levels-tick" thread that sends `Msg::LevelsTick` every
/// [`LEVELS_INTERVAL`]. It ends when a send fails, which happens once the service
/// has stopped and dropped its receiver. It is never joined.
fn spawn_levels_ticker(tx: Sender<Msg>) {
    let spawned = thread::Builder::new()
        .name("levels-tick".to_owned())
        .spawn(move || loop {
            thread::sleep(LEVELS_INTERVAL);
            if tx.send(Msg::LevelsTick).is_err() {
                break;
            }
        });
    if spawned.is_err() {
        logging::warn("audio level ticker could not start; levels will not be published");
    }
}

/// Forwards every item from `rx` into the service channel as `wrap(item)`.
/// The forwarder ends when the producer drops its sender.
fn forward<T, F>(name: &str, rx: Receiver<T>, tx: Sender<Msg>, wrap: F)
where
    T: Send + 'static,
    F: Fn(T) -> Msg + Send + 'static,
{
    let spawned = thread::Builder::new().name(name.to_owned()).spawn(move || {
        for item in rx {
            if tx.send(wrap(item)).is_err() {
                break;
            }
        }
    });
    if let Err(e) = spawned {
        logging::error(&format!("cannot start {name} forwarder: {e}"));
    }
}

/// True when a change touches what the recorder is built from.
fn recorder_settings_changed(old: &Config, new: &Config) -> bool {
    old.video != new.video
        || old.audio != new.audio
        || old.replay != new.replay
        || old.storage != new.storage
}

/// Builds the detector configuration from `detection.*` and `advanced.*`.
fn detect_config(cfg: &Config) -> DetectConfig {
    let defaults = DetectConfig::default();
    DetectConfig {
        stop_grace_ms: cfg.detection.stop_grace_s.saturating_mul(1000),
        allow: cfg
            .detection
            .allow
            .iter()
            .map(|exe| AllowRule {
                exe: exe.as_str().to_owned(),
                display_name: display_override(cfg, exe.as_str()),
            })
            .collect(),
        block_extra: cfg
            .detection
            .block
            .iter()
            .map(|exe| exe.as_str().to_owned())
            .collect(),
        allowlist_only: matches!(cfg.detection.mode, DetectionMode::Allowlist),
        ..defaults
    }
}

/// Display name from a per-game override. Uses the same key matching as validation
/// (`exe` or `exe@store:id`), so every documented key form applies.
fn display_override(cfg: &Config, exe: &str) -> Option<String> {
    cfg.game_override(exe)
        .and_then(|game| game.display_name.clone())
}

/// Canonical game id used in events and library rows.
///
/// A game that matched a known wildcard entry (every build of FiveM) gets one stable id.
/// Every other exe keeps `exe:<file name>`, so existing library rows and folders stay valid.
fn game_id(exe: &str) -> String {
    known_games::canonical_for(exe).unwrap_or_else(|| format!("exe:{exe}"))
}

/// Pure form of the daemon's start gate: auto-record must be on, the exe must not be on
/// the block list (`detection.block`, wildcards allowed), and in allowlist mode an allow
/// entry (`detection.allow`, wildcards allowed) must match it.
fn record_permitted(
    exe: &str,
    auto_record: bool,
    allowlist: bool,
    allow: &[&str],
    block: &[&str],
) -> bool {
    if !auto_record || block.iter().any(|b| name_matches(b, exe)) {
        return false;
    }
    !allowlist || allow.iter().any(|a| name_matches(a, exe))
}

/// Config codec as the lowercase name used in status.
fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    }
}

/// Maps a recorder error to an IPC error code and message.
fn to_service_error(e: RecorderError) -> ServiceError {
    let code = match &e {
        RecorderError::NotAttached => ErrorCode::NotRecording,
        RecorderError::AlreadyAttached
        | RecorderError::SessionActive
        | RecorderError::NoSession
        | RecorderError::NoSegments
        | RecorderError::BufferDisabled
        | RecorderError::BufferEmpty => ErrorCode::InvalidState,
        RecorderError::InvalidRequest(_) => ErrorCode::BadRequest,
        RecorderError::Unsupported => ErrorCode::WorkerUnavailable,
        RecorderError::Io(_) => ErrorCode::Internal,
    };
    ServiceError::new(code, e.to_string())
}

/// How long a hotkey suspension lasts without a renewing call (the UI renews about every 20 s).
const HOTKEY_SUSPEND_TIMEOUT: Duration = Duration::from_secs(60);

/// Suspends or resumes the chords of a running hotkey service. The hook ignores chords while
/// suspended; the hotkey service also expires a suspension on its own after 60 s.
fn set_hotkey_suspension(service: &HotkeyService, suspended: bool) {
    service.set_suspended(suspended);
}

/// Text for a hotkey the OS refused to register.
fn describe_failure(reason: &FailureReason) -> String {
    match reason {
        FailureReason::TakenByAnotherApp => "already used by another application".to_owned(),
        FailureReason::Os(code) => format!("windows error {code}"),
    }
}

/// Seconds to whole milliseconds, never negative.
fn seconds_to_ms(seconds: f64) -> u64 {
    let ms = (seconds * 1000.0).round();
    if ms.is_finite() && ms > 0.0 {
        // Saturating float-to-int conversion is the intent here.
        ms as u64
    } else {
        0
    }
}

/// Buffer seconds as a whole number, clamped to the `u32` range.
fn secs_to_u32(seconds: f64) -> u32 {
    if seconds.is_finite() && seconds > 0.0 {
        // Saturating float-to-int conversion is the intent here.
        seconds.min(f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

/// Modification time of a file in Unix milliseconds, used as the start of a recovered
/// part. Falls back to now when the time cannot be read.
fn started_unix_ms_for(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or_else(now_unix_ms)
}

/// Size of a file in bytes. A file that cannot be read is logged and reported as 0.
fn file_len(path: &Path) -> u64 {
    match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) => {
            logging::warn(&format!(
                "cannot read the size of {}: {e}; reporting 0 bytes",
                path.display()
            ));
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_config_maps_grace_and_lists() {
        let mut cfg = Config::default();
        cfg.detection.stop_grace_s = 7;
        let detect = detect_config(&cfg);
        assert_eq!(detect.stop_grace_ms, 7000);
        assert!(detect.allow.is_empty());
    }

    #[test]
    fn seconds_conversions_are_clamped() {
        assert_eq!(seconds_to_ms(4.25), 4250);
        assert_eq!(seconds_to_ms(-1.0), 0);
        assert_eq!(seconds_to_ms(f64::NAN), 0);
        assert_eq!(secs_to_u32(-5.0), 0);
        assert_eq!(secs_to_u32(12.9), 12);
    }

    #[test]
    fn levels_event_is_none_without_tracks() {
        assert!(levels_event(&[]).is_none());
    }

    #[test]
    fn levels_event_numbers_tracks_from_zero() {
        let snap = LevelSnapshot {
            peak_db: -6.0,
            rms_db: -20.0,
        };
        let Some(Event::AudioLevels { tracks }) = levels_event(&[snap, snap]) else {
            panic!("expected AudioLevels");
        };
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].track, 0);
        assert_eq!(tracks[1].track, 1);
        for t in &tracks {
            assert_eq!(t.peak_db, -6.0);
            assert_eq!(t.rms_db, -20.0);
        }
    }

    #[test]
    fn levels_event_maps_non_finite_db_to_floor() {
        let snap = LevelSnapshot {
            peak_db: f32::NEG_INFINITY,
            rms_db: f32::NAN,
        };
        let Some(Event::AudioLevels { tracks }) = levels_event(&[snap]) else {
            panic!("expected AudioLevels");
        };
        assert_eq!(tracks[0].peak_db, -120.0);
        assert_eq!(tracks[0].rms_db, -120.0);
    }

    #[test]
    fn levels_event_caps_at_255_tracks() {
        let snap = LevelSnapshot {
            peak_db: -1.0,
            rms_db: -2.0,
        };
        let snaps = vec![snap; 300];
        let Some(Event::AudioLevels { tracks }) = levels_event(&snaps) else {
            panic!("expected AudioLevels");
        };
        assert_eq!(tracks.len(), 255);
        assert_eq!(tracks.last().map(|t| t.track), Some(254));
    }

    #[test]
    fn game_id_uses_exe_prefix() {
        assert_eq!(game_id("game.exe"), "exe:game.exe");
    }

    #[test]
    fn game_id_is_stable_for_pattern_matched_builds() {
        assert_eq!(game_id("FiveM_b2802_GTAProcess.exe"), "exe:fivem");
        assert_eq!(game_id("FiveM_b3258_GTAProcess.exe"), "exe:fivem");
        assert_eq!(game_id("FiveM_b3258_GameProcess.exe"), "exe:fivem");
        // Exact known games and unknown games keep their file-name id.
        assert_eq!(game_id("Terraria.exe"), "exe:Terraria.exe");
        assert_eq!(game_id("unknown_game.exe"), "exe:unknown_game.exe");
    }

    #[test]
    fn record_permitted_applies_wildcards_to_allow_and_block() {
        // Auto mode records anything not blocked.
        assert!(record_permitted("game.exe", true, false, &[], &[]));
        assert!(!record_permitted("game.exe", false, false, &[], &[]));
        assert!(!record_permitted(
            "game.exe",
            true,
            false,
            &[],
            &["GAME.exe"]
        ));
        // Allowlist mode: a wildcard allow entry covers every build.
        let allow = ["FiveM*GTAProcess.exe"];
        assert!(record_permitted(
            "FiveM_b3258_GTAProcess.exe",
            true,
            true,
            &allow,
            &[]
        ));
        assert!(!record_permitted("Other.exe", true, true, &allow, &[]));
        // A wildcard block entry wins over an allow entry.
        assert!(!record_permitted(
            "FiveM_b3258_GTAProcess.exe",
            true,
            true,
            &allow,
            &["fivem*"]
        ));
        // Plain allow entries still match exactly, ignoring case.
        assert!(record_permitted("game.exe", true, true, &["Game.EXE"], &[]));
    }

    /// Config with `game.exe` allowed and a per-game override for it.
    fn issue190_config() -> Config {
        use clipcheck_core::config::schema::{ExeName, GameOverride};
        let mut cfg = Config::default();
        cfg.detection
            .allow
            .push("game.exe".parse::<ExeName>().unwrap());
        cfg.games.insert(
            "game.exe".to_owned(),
            GameOverride {
                display_name: Some("Game".to_owned()),
                folder: Some(PathBuf::from("C:\\Clips\\Game")),
                replay_max_seconds: Some(60),
                ..Default::default()
            },
        );
        cfg
    }

    #[test]
    fn issue190_replay_max_seconds_does_not_restart_detector() {
        let mut cfg = issue190_config();
        let before = detect_config(&cfg);
        cfg.games.get_mut("game.exe").unwrap().replay_max_seconds = Some(120);
        assert_eq!(before, detect_config(&cfg));
    }

    #[test]
    fn issue190_folder_does_not_restart_detector() {
        let mut cfg = issue190_config();
        let before = detect_config(&cfg);
        cfg.games.get_mut("game.exe").unwrap().folder = Some(PathBuf::from("D:\\Other"));
        assert_eq!(before, detect_config(&cfg));
    }

    #[test]
    fn issue190_display_name_restarts_detector() {
        let mut cfg = issue190_config();
        let before = detect_config(&cfg);
        cfg.games.get_mut("game.exe").unwrap().display_name = Some("Renamed".to_owned());
        let after = detect_config(&cfg);
        assert_ne!(before, after);
        assert_eq!(after.allow[0].display_name.as_deref(), Some("Renamed"));
    }

    #[test]
    fn issue190_stop_grace_restarts_detector() {
        let mut cfg = issue190_config();
        let before = detect_config(&cfg);
        cfg.detection.stop_grace_s += 1;
        assert_ne!(before, detect_config(&cfg));
    }

    #[test]
    fn issue190_override_for_unlisted_exe_does_not_restart_detector() {
        use clipcheck_core::config::schema::GameOverride;
        let mut cfg = issue190_config();
        let before = detect_config(&cfg);
        cfg.games.insert(
            "other.exe".to_owned(),
            GameOverride {
                replay_max_seconds: Some(45),
                ..Default::default()
            },
        );
        assert_eq!(before, detect_config(&cfg));
    }

    const CALL_TEST_TIMEOUT: Duration = Duration::from_millis(50);

    #[test]
    fn call_nobody_services_is_busy_and_abandoned() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let result = call_via(&tx, Op::Status, CALL_TEST_TIMEOUT, CALL_TEST_TIMEOUT);
        match result {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Busy);
                assert!(e.message.contains("not applied"), "message: {}", e.message);
            }
            Ok(_) => panic!("an unserviced call must not succeed"),
        }
        // The queued call was abandoned, so a service that picks it up must skip it.
        match rx.recv() {
            Ok(Msg::Call(_, _, state)) => {
                assert_eq!(state.load(Ordering::SeqCst), CALL_ABANDONED);
                assert!(state
                    .compare_exchange(
                        CALL_PENDING,
                        CALL_RUNNING,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_err());
            }
            _ => panic!("expected the queued call"),
        }
    }

    #[test]
    fn call_started_before_timeout_is_waited_for_not_busy() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let service = thread::spawn(move || match rx.recv() {
            Ok(Msg::Call(_, reply, state)) => {
                assert!(state
                    .compare_exchange(
                        CALL_PENDING,
                        CALL_RUNNING,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok());
                thread::sleep(Duration::from_millis(120));
                let _ = reply.send(Ok(Answer::Done));
            }
            _ => panic!("expected a call"),
        });
        let result = call_via(&tx, Op::Status, CALL_TEST_TIMEOUT, Duration::from_secs(2));
        assert!(
            matches!(result, Ok(Answer::Done)),
            "call must return the real answer"
        );
        service.join().expect("service thread");
    }

    #[test]
    fn call_running_past_grace_is_unknown_not_busy() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let service = thread::spawn(move || match rx.recv() {
            Ok(Msg::Call(_, reply, state)) => {
                assert!(state
                    .compare_exchange(
                        CALL_PENDING,
                        CALL_RUNNING,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok());
                thread::sleep(Duration::from_millis(500));
                let _ = reply.send(Ok(Answer::Done));
            }
            _ => panic!("expected a call"),
        });
        let result = call_via(
            &tx,
            Op::Status,
            CALL_TEST_TIMEOUT,
            Duration::from_millis(50),
        );
        match result {
            Err(e) => {
                assert_ne!(
                    e.code,
                    ErrorCode::Busy,
                    "a started call must not be retryable"
                );
                assert!(e.message.contains("unknown"), "message: {}", e.message);
            }
            Ok(_) => panic!("a call that outlives the grace must not report success"),
        }
        service.join().expect("service thread");
    }

    #[test]
    fn call_with_dropped_receiver_reports_stopped() {
        let (tx, rx) = mpsc::channel::<Msg>();
        drop(rx);
        let result = call_via(&tx, Op::Status, CALL_TEST_TIMEOUT, CALL_TEST_TIMEOUT);
        match result {
            Err(e) => assert_eq!(e, stopped()),
            Ok(_) => panic!("a stopped service must not answer"),
        }
    }
}

/// Unit tests for the recorder switch. They build the core directly, because a session
/// cannot be made without an encoder, and so pending settings never queue through the public API.
#[cfg(test)]
mod recorder_switch_tests {
    use super::*;

    fn core_in(dir: &Path) -> Core {
        let library = Library::open(dir.join("library.sqlite")).expect("open library");
        let bus = Arc::new(EventBus::new());
        let recording = Arc::new(AtomicBool::new(false));
        let libraries = LibraryService::start(LibraryOptions {
            data_dir: dir.to_path_buf(),
            storage: Config::default().storage,
            bus: Arc::clone(&bus),
            recording: Arc::clone(&recording),
        })
        .expect("start library service");
        let (tx, _rx) = mpsc::channel();
        Core::new(
            ServiceOptions {
                config: Config::default(),
                data_dir: dir.to_path_buf(),
                library: Arc::new(Mutex::new(library)),
                bus,
                no_encoders: true,
            },
            tx,
            libraries,
            recording,
        )
    }

    fn game() -> Target {
        Target {
            hwnd: 7,
            exe: "game.exe".to_owned(),
            pid: 42,
            label: "Game".to_owned(),
        }
    }

    #[test]
    fn turning_off_applies_pending_settings_at_once_when_idle() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core_in(dir.path());
        core.settings_pending = Some(ApplyClass::PipelineReinit);
        core.set_recorder_enabled(false).expect("turn off");
        assert!(
            core.settings_pending.is_none(),
            "an idle recorder applies pending settings when it is turned off"
        );
        assert!(!core.recorder_enabled);
        core.libraries.stop();
    }

    #[test]
    fn pending_settings_apply_on_the_idle_check_without_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core_in(dir.path());
        core.settings_pending = Some(ApplyClass::NextPart);
        core.apply_pending_if_idle();
        assert!(core.settings_pending.is_none());
        core.libraries.stop();
    }

    #[test]
    fn turning_on_records_a_game_that_is_still_detected() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core_in(dir.path());
        core.set_recorder_enabled(false).expect("turn off");
        core.detected = Some(game());
        core.set_recorder_enabled(true).expect("turn on");
        // No recorder exists in degraded mode, so the game waits for the probe.
        assert_eq!(core.pending, Some(game()));
        core.libraries.stop();
    }

    #[test]
    fn turning_off_drops_the_waiting_game_and_detaches() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core_in(dir.path());
        core.pending = Some(game());
        core.set_recorder_enabled(false).expect("turn off");
        assert!(core.pending.is_none());
        assert!(core.attached.is_none());
        core.libraries.stop();
    }

    #[test]
    fn state_file_roundtrips_and_bad_values_default_on() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_recorder_enabled(dir.path()), "absent is on");
        save_recorder_enabled(dir.path(), false).unwrap();
        assert!(!load_recorder_enabled(dir.path()));
        save_recorder_enabled(dir.path(), true).unwrap();
        assert!(load_recorder_enabled(dir.path()));
        fs::write(dir.path().join(RECORDER_STATE_FILE), r#"{"enabled":"no"}"#).unwrap();
        assert!(
            load_recorder_enabled(dir.path()),
            "a non-bool value is ignored"
        );
    }
}
