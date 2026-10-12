//! Protocol v1 message types for the daemon <-> UI IPC (design doc 10, sections 4.1 to 4.2 and 7).
//!
//! All enums use serde's internally tagged form `{"type": ..., "data": ...}` with
//! snake_case names. No wire type denies unknown fields.

use serde::{Deserialize, Serialize};

/// Protocol version implemented by this build.
pub const PROTOCOL_VERSION: u16 = 1;

/// Oldest protocol version this build can still speak.
pub const PROTOCOL_MIN_SUPPORTED: u16 = 1;

/// Wire frame: protocol version, optional request id, optional event sequence, and body.
///
/// Requests and replies carry `id` (echoed in the reply). Events carry `seq` and no `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<B> {
    /// Protocol version. Hello uses 1; later frames carry the negotiated version.
    pub v: u16,
    /// Client-chosen request id, echoed by the reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// Per-daemon monotonic event sequence number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Command, reply or event payload.
    pub body: B,
}

impl<B> Envelope<B> {
    /// Builds a request frame at [`PROTOCOL_VERSION`] with the given id.
    pub fn request(id: u64, body: B) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: Some(id),
            seq: None,
            body,
        }
    }

    /// Builds a reply frame at [`PROTOCOL_VERSION`] echoing the request id.
    pub fn reply(id: u64, body: B) -> Self {
        Self::request(id, body)
    }

    /// Builds an event frame at [`PROTOCOL_VERSION`] with the given sequence number.
    pub fn event(seq: u64, body: B) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: None,
            seq: Some(seq),
            body,
        }
    }
}

/// A client-to-daemon frame.
pub type Request = Envelope<Command>;
/// A daemon-to-client reply frame.
pub type Response = Envelope<Reply>;
/// A daemon-to-client pushed event frame.
pub type Push = Envelope<Event>;

/// Commands a client may send.
///
/// Wire form: `{"type": "<snake_case name>", "data": {...}}`, with no `data` key for
/// commands without fields. The serde impls are hand-written only for
/// [`Command::Shutdown`] (see the impls after this enum); all other variants go through
/// the private derived mirror `CommandWire`, which must list the same variants.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Handshake, sent first. The client's supported version range.
    Hello {
        /// Lowest protocol version the client supports.
        proto_min: u16,
        /// Highest protocol version the client supports.
        proto_max: u16,
        /// Client name, for example `clipcheck-ui`.
        client: String,
        /// Client build version.
        client_version: String,
    },
    /// Liveness check. Answered with [`Reply::Pong`].
    Ping,
    /// Full status snapshot. Answered with [`Reply::Status`].
    GetStatus,
    /// Start the full recording session.
    StartSession,
    /// Stop the full recording session.
    StopSession,
    /// Split the current session at the next keyframe.
    SplitSession,
    /// Save the last `seconds` of the replay buffer as a clip. Valid range 1..=3600.
    ClipNow {
        /// Clip length in seconds.
        seconds: u32,
    },
    /// Read the current configuration. Answered with [`Reply::Config`].
    GetConfig,
    /// Apply a partial configuration update.
    ///
    /// Objects merge recursively, arrays and scalars replace, and `null` clears an
    /// optional field (or removes a `games` entry). Unknown keys are rejected with
    /// `bad_request`. Answered with [`Reply::ConfigApplied`], [`Reply::ConfigRejected`]
    /// or, for `dry_run`, [`Reply::ConfigChecked`].
    PatchConfig {
        /// Partial configuration object to merge into the current config.
        patch: serde_json::Value,
        /// Validate only. Nothing is written or applied.
        dry_run: bool,
    },
    /// Re-read `config.toml` from disk (after a hand edit). Same replies as
    /// [`Command::PatchConfig`].
    ReloadConfig,
    /// Rename a recording.
    RenameRecording {
        /// Recording to rename.
        recording_id: String,
        /// New display name.
        name: String,
    },
    /// Set or clear the library favourite flag of a recording. Replies [`Reply::Ok`].
    SetFavorite {
        /// Recording to update.
        recording_id: String,
        /// New favourite state.
        favorite: bool,
    },
    /// Delete a recording.
    DeleteRecording {
        /// Recording to delete.
        recording_id: String,
    },
    /// Register a file produced by an external edit as a recording.
    RegisterEditedRecording {
        /// Recording the edit was derived from, if any.
        source_recording_id: Option<String>,
        /// Absolute Windows path of the edited file.
        path: String,
        /// When true and the source recording exists, the new recording takes the source's
        /// creation time and favourite flag.
        keep_source_time: bool,
        /// Game name for the new recording. Overrides the source's game when set. An empty or
        /// missing value keeps the source's game. `Exports` (case-insensitive) may register a
        /// file outside every configured library folder; its own folder becomes the root.
        game: Option<String>,
    },
    /// Enable pushed events for the given topics.
    Subscribe {
        /// Topics to subscribe to.
        topics: Vec<Topic>,
    },
    /// Disable pushed events for the given topics.
    Unsubscribe {
        /// Topics to unsubscribe from.
        topics: Vec<Topic>,
    },
    /// Ask the daemon to shut down. Answered with [`Reply::ShutdownAck`] before the
    /// daemon exits.
    ///
    /// Per D47: `graceful` and `update` are refused with [`ErrorCode::Busy`] (retryable)
    /// while a recording session is active; the daemon then keeps running and does
    /// nothing else. `force` always shuts down. The check and the start of shutdown are
    /// atomic, so no new session can start after an accepted request. The replay buffer
    /// alone never refuses. When `mode` is absent, [`ShutdownMode::effective`] decides.
    Shutdown {
        /// Why the client asks for the shutdown, for example `update` or `installer`.
        reason: Option<String>,
        /// How the daemon should treat the request. `None` means [`ShutdownMode::effective`].
        mode: Option<ShutdownMode>,
    },
    /// Applications that play (or recently played) audio. Answered with [`Reply::AudioApps`].
    ListAudioApps,
    /// Active capture and render endpoints. Answered with [`Reply::AudioDevices`].
    ListAudioDevices,
    /// GPUs and encoder probe results. Answered with [`Reply::Encoders`].
    ListEncoders,
    /// Visible windows that can be picked as games. Answered with [`Reply::RunningApps`].
    ListRunningApps,
    /// Start or refresh (`enable: true`) or stop (`enable: false`) the microphone
    /// meter. Answered with [`Reply::Ok`]. While enabled, subscribers of
    /// [`Topic::Meter`] receive [`Event::MicLevel`]. The meter stops by itself after
    /// 30 s without a refresh and when the client disconnects.
    MicMeter {
        /// True to start or keep the meter running, false to stop it.
        enable: bool,
        /// Capture endpoint id, or `None` for the default capture device.
        device: Option<String>,
    },
    /// Records `MIC_TEST_SECONDS` (5 s) from a capture endpoint and answers with
    /// [`Reply::MicTest`]. Takes about 5 seconds.
    MicTest {
        /// Capture endpoint id, or `None` for the default capture device.
        device: Option<String>,
    },
    /// Storage roots (primary and extra library folders) with their counts. Answered with
    /// [`Reply::Libraries`].
    ListLibraries,
    /// Rescans one library root, or every root when `root_id` is absent or null. Answered
    /// with [`Reply::Ok`]. Progress arrives as [`Event::LibraryScan`].
    ScanLibraries {
        /// Root to scan, or `None` for all roots.
        root_id: Option<i64>,
    },
    /// Plans an import of files or folders into the primary root. Copies nothing. Answered
    /// with [`Reply::ImportPlan`].
    ImportPlan {
        /// Files or folders to import. Folders are walked recursively.
        paths: Vec<String>,
        /// Game folder for every file. `None` derives it from each file's parent folder.
        game: Option<String>,
    },
    /// Starts copying the planned files into the primary root. Answered with
    /// [`Reply::ImportStarted`]. Progress arrives as [`Event::ImportProgress`] and the end as
    /// [`Event::ImportDone`]. Only one import runs at a time (`busy`).
    ImportStart {
        /// Files or folders to import.
        paths: Vec<String>,
        /// Game folder for every file. `None` derives it from each file's parent folder.
        game: Option<String>,
    },
    /// Cancels a running import. Completed files are kept. Answered with [`Reply::Ok`].
    ImportCancel {
        /// Job id from [`Reply::ImportStarted`].
        job_id: String,
    },
    /// Game folders in the library with their clip counts. Answered with [`Reply::GameNames`].
    ListGameNames,
    /// Turns the recorder on or off. Answered with [`Reply::Ok`].
    ///
    /// Off: a recording session is stopped and saved up to this point, the game is
    /// detached, and no session starts on detection. Clip, split and session commands
    /// then fail with `invalid_state` ("recorder is turned off"). Detection keeps running,
    /// so status still shows the detected game. Pending settings are applied as soon as the
    /// recorder is idle. On: recording resumes, and a game that is still detected starts
    /// buffering again. The choice persists across daemon restarts.
    SetRecorderEnabled {
        /// True to turn the recorder on, false to turn it off.
        enabled: bool,
    },
    /// Suspends (true) or resumes (false) the global hotkeys. Answered with [`Reply::Ok`].
    ///
    /// While suspended, every registered chord is ignored, so the chords can be captured in
    /// a settings UI without saving clips. The suspension ends by itself 60 s after the last
    /// request that set it, so the client must send `suspended: true` again to keep it (about
    /// every 20 s). It also ends when the client that suspended disconnects, when the config
    /// is reloaded or patched, and when the hotkey service restarts. Status shows it as
    /// `hotkeys_suspended`.
    SetHotkeysSuspended {
        /// True to suspend the hotkeys, false to resume them.
        suspended: bool,
    },
}

/// How the daemon should treat a shutdown request (D47).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode {
    /// Refused with `busy` while a recording session is active.
    Graceful,
    /// Refused with `busy` while a recording session is active (sent by the updater).
    Update,
    /// Always shuts down. The normal shutdown path finalizes any active session.
    Force,
}

impl ShutdownMode {
    /// `mode` if given; else `Update` when `reason == Some("update")`; else `Graceful`.
    pub fn effective(mode: Option<ShutdownMode>, reason: Option<&str>) -> ShutdownMode {
        match (mode, reason) {
            (Some(mode), _) => mode,
            (None, Some("update")) => ShutdownMode::Update,
            (None, _) => ShutdownMode::Graceful,
        }
    }
}

/// Derived wire mirror of [`Command`] for every variant except `Shutdown`.
/// `Shutdown` is unit here only because a derived struct variant always writes a
/// `data` key; the real `Shutdown` is handled by hand in the impls below.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum CommandWire {
    Hello {
        proto_min: u16,
        proto_max: u16,
        client: String,
        client_version: String,
    },
    Ping,
    GetStatus,
    StartSession,
    StopSession,
    SplitSession,
    ClipNow {
        seconds: u32,
    },
    GetConfig,
    PatchConfig {
        patch: serde_json::Value,
        #[serde(default)]
        dry_run: bool,
    },
    ReloadConfig,
    RenameRecording {
        recording_id: String,
        name: String,
    },
    SetFavorite {
        recording_id: String,
        favorite: bool,
    },
    DeleteRecording {
        recording_id: String,
    },
    RegisterEditedRecording {
        source_recording_id: Option<String>,
        path: String,
        #[serde(default)]
        keep_source_time: bool,
        #[serde(default)]
        game: Option<String>,
    },
    Subscribe {
        topics: Vec<Topic>,
    },
    Unsubscribe {
        topics: Vec<Topic>,
    },
    Shutdown,
    ListAudioApps,
    ListAudioDevices,
    ListEncoders,
    ListRunningApps,
    MicMeter {
        enable: bool,
        #[serde(default)]
        device: Option<String>,
    },
    MicTest {
        #[serde(default)]
        device: Option<String>,
    },
    ListLibraries,
    ScanLibraries {
        #[serde(default)]
        root_id: Option<i64>,
    },
    ImportPlan {
        paths: Vec<String>,
        #[serde(default)]
        game: Option<String>,
    },
    ImportStart {
        paths: Vec<String>,
        #[serde(default)]
        game: Option<String>,
    },
    ImportCancel {
        job_id: String,
    },
    ListGameNames,
    SetRecorderEnabled {
        enabled: bool,
    },
    SetHotkeysSuspended {
        suspended: bool,
    },
}

/// Payload of `{"type":"shutdown","data":{"reason":...,"mode":...}}`. Absent keys are
/// omitted on write; unknown keys are ignored on read.
#[derive(Serialize, Deserialize)]
struct ShutdownData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<ShutdownMode>,
}

impl From<Command> for CommandWire {
    fn from(command: Command) -> Self {
        match command {
            Command::Hello {
                proto_min,
                proto_max,
                client,
                client_version,
            } => CommandWire::Hello {
                proto_min,
                proto_max,
                client,
                client_version,
            },
            Command::Ping => CommandWire::Ping,
            Command::GetStatus => CommandWire::GetStatus,
            Command::StartSession => CommandWire::StartSession,
            Command::StopSession => CommandWire::StopSession,
            Command::SplitSession => CommandWire::SplitSession,
            Command::ClipNow { seconds } => CommandWire::ClipNow { seconds },
            Command::GetConfig => CommandWire::GetConfig,
            Command::PatchConfig { patch, dry_run } => CommandWire::PatchConfig { patch, dry_run },
            Command::ReloadConfig => CommandWire::ReloadConfig,
            Command::RenameRecording { recording_id, name } => {
                CommandWire::RenameRecording { recording_id, name }
            }
            Command::SetFavorite {
                recording_id,
                favorite,
            } => CommandWire::SetFavorite {
                recording_id,
                favorite,
            },
            Command::DeleteRecording { recording_id } => {
                CommandWire::DeleteRecording { recording_id }
            }
            Command::RegisterEditedRecording {
                source_recording_id,
                path,
                keep_source_time,
                game,
            } => CommandWire::RegisterEditedRecording {
                source_recording_id,
                path,
                keep_source_time,
                game,
            },
            Command::Subscribe { topics } => CommandWire::Subscribe { topics },
            Command::Unsubscribe { topics } => CommandWire::Unsubscribe { topics },
            Command::Shutdown { .. } => CommandWire::Shutdown,
            Command::ListAudioApps => CommandWire::ListAudioApps,
            Command::ListAudioDevices => CommandWire::ListAudioDevices,
            Command::ListEncoders => CommandWire::ListEncoders,
            Command::ListRunningApps => CommandWire::ListRunningApps,
            Command::MicMeter { enable, device } => CommandWire::MicMeter { enable, device },
            Command::MicTest { device } => CommandWire::MicTest { device },
            Command::ListLibraries => CommandWire::ListLibraries,
            Command::ScanLibraries { root_id } => CommandWire::ScanLibraries { root_id },
            Command::ImportPlan { paths, game } => CommandWire::ImportPlan { paths, game },
            Command::ImportStart { paths, game } => CommandWire::ImportStart { paths, game },
            Command::ImportCancel { job_id } => CommandWire::ImportCancel { job_id },
            Command::ListGameNames => CommandWire::ListGameNames,
            Command::SetRecorderEnabled { enabled } => CommandWire::SetRecorderEnabled { enabled },
            Command::SetHotkeysSuspended { suspended } => {
                CommandWire::SetHotkeysSuspended { suspended }
            }
        }
    }
}

impl From<CommandWire> for Command {
    fn from(wire: CommandWire) -> Self {
        match wire {
            CommandWire::Hello {
                proto_min,
                proto_max,
                client,
                client_version,
            } => Command::Hello {
                proto_min,
                proto_max,
                client,
                client_version,
            },
            CommandWire::Ping => Command::Ping,
            CommandWire::GetStatus => Command::GetStatus,
            CommandWire::StartSession => Command::StartSession,
            CommandWire::StopSession => Command::StopSession,
            CommandWire::SplitSession => Command::SplitSession,
            CommandWire::ClipNow { seconds } => Command::ClipNow { seconds },
            CommandWire::GetConfig => Command::GetConfig,
            CommandWire::PatchConfig { patch, dry_run } => Command::PatchConfig { patch, dry_run },
            CommandWire::ReloadConfig => Command::ReloadConfig,
            CommandWire::RenameRecording { recording_id, name } => {
                Command::RenameRecording { recording_id, name }
            }
            CommandWire::SetFavorite {
                recording_id,
                favorite,
            } => Command::SetFavorite {
                recording_id,
                favorite,
            },
            CommandWire::DeleteRecording { recording_id } => {
                Command::DeleteRecording { recording_id }
            }
            CommandWire::RegisterEditedRecording {
                source_recording_id,
                path,
                keep_source_time,
                game,
            } => Command::RegisterEditedRecording {
                source_recording_id,
                path,
                keep_source_time,
                game,
            },
            CommandWire::Subscribe { topics } => Command::Subscribe { topics },
            CommandWire::Unsubscribe { topics } => Command::Unsubscribe { topics },
            CommandWire::Shutdown => Command::Shutdown {
                reason: None,
                mode: None,
            },
            CommandWire::ListAudioApps => Command::ListAudioApps,
            CommandWire::ListAudioDevices => Command::ListAudioDevices,
            CommandWire::ListEncoders => Command::ListEncoders,
            CommandWire::ListRunningApps => Command::ListRunningApps,
            CommandWire::MicMeter { enable, device } => Command::MicMeter { enable, device },
            CommandWire::MicTest { device } => Command::MicTest { device },
            CommandWire::ListLibraries => Command::ListLibraries,
            CommandWire::ScanLibraries { root_id } => Command::ScanLibraries { root_id },
            CommandWire::ImportPlan { paths, game } => Command::ImportPlan { paths, game },
            CommandWire::ImportStart { paths, game } => Command::ImportStart { paths, game },
            CommandWire::ImportCancel { job_id } => Command::ImportCancel { job_id },
            CommandWire::ListGameNames => Command::ListGameNames,
            CommandWire::SetRecorderEnabled { enabled } => Command::SetRecorderEnabled { enabled },
            CommandWire::SetHotkeysSuspended { suspended } => {
                Command::SetHotkeysSuspended { suspended }
            }
        }
    }
}

impl Serialize for Command {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Command::Shutdown { reason, mode } = self {
            if reason.is_some() || mode.is_some() {
                // Hand-built so the `data` key is present only when a field is set.
                return serde_json::json!({
                    "type": "shutdown",
                    "data": ShutdownData {
                        reason: reason.clone(),
                        mode: *mode,
                    },
                })
                .serialize(serializer);
            }
            return serde_json::json!({ "type": "shutdown" }).serialize(serializer);
        }
        CommandWire::from(self.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("type").and_then(serde_json::Value::as_str) == Some("shutdown") {
            // `{"type":"shutdown"}` (no data), `{"type":"shutdown","data":null}` and
            // `{"type":"shutdown","data":{}}` all mean "no reason, no mode".
            let data = match value.get("data") {
                None | Some(serde_json::Value::Null) => ShutdownData {
                    reason: None,
                    mode: None,
                },
                Some(data) => ShutdownData::deserialize(data).map_err(D::Error::custom)?,
            };
            return Ok(Command::Shutdown {
                reason: data.reason,
                mode: data.mode,
            });
        }
        CommandWire::deserialize(value)
            .map(Command::from)
            .map_err(D::Error::custom)
    }
}

/// Replies the daemon sends to a request. Each carries the request's `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Reply {
    /// Handshake accepted.
    HelloAck {
        /// Negotiated protocol version.
        proto: u16,
        /// Daemon build version.
        daemon_version: String,
        /// Daemon process id.
        daemon_pid: u32,
        /// Optional features the daemon supports.
        features: Vec<String>,
    },
    /// Reply to [`Command::Ping`].
    Pong {
        /// Daemon time in Unix milliseconds.
        unix_ms: i64,
    },
    /// Reply to [`Command::GetStatus`].
    Status(Box<StatusDto>),
    /// Generic success.
    Ok,
    /// Reply to [`Command::Shutdown`]. The daemon exits after sending it.
    ShutdownAck,
    /// Clip accepted; the `clip_saved` event follows when work completes.
    ClipQueued {
        /// Id of the queued clip.
        clip_id: String,
    },
    /// Reply to [`Command::GetConfig`].
    Config {
        /// Full configuration object in effect.
        config: serde_json::Value,
        /// `config_version` of the configuration in effect.
        config_version: u32,
        /// Absolute path of the config file.
        path: String,
    },
    /// A configuration change was saved and applied.
    ConfigApplied {
        /// Strongest apply class of the change (`live`, `next_part`, `pipeline_reinit`,
        /// `audio_reinit`, `replay_flush` or `restart`).
        apply_class: String,
        /// True when part of the change waits until no game is attached.
        restart_required: bool,
        /// Warnings. Warnings never block a change.
        #[serde(default)]
        diagnostics: Vec<DiagnosticDto>,
    },
    /// A dry run passed. Nothing was written or applied.
    ConfigChecked {
        /// Apply class the change would have.
        apply_class: String,
        /// Whether the change would wait for the next detach.
        restart_required: bool,
        /// Warnings the change would produce.
        diagnostics: Vec<DiagnosticDto>,
    },
    /// The change has errors. Nothing was written or applied.
    ConfigRejected {
        /// Every finding, errors and warnings.
        diagnostics: Vec<DiagnosticDto>,
    },
    /// Reply to [`Command::RegisterEditedRecording`].
    RecordingRegistered {
        /// Id assigned to the new recording.
        recording_id: String,
    },
    /// Reply to [`Command::Subscribe`].
    Subscribed {
        /// Topics now subscribed.
        topics: Vec<Topic>,
    },
    /// Reply to [`Command::Unsubscribe`].
    Unsubscribed {
        /// Topics that were unsubscribed.
        topics: Vec<Topic>,
    },
    /// Reply to [`Command::ListAudioApps`].
    AudioApps {
        /// Apps that are playing now or played recently, most recent first.
        apps: Vec<AudioAppDto>,
    },
    /// Reply to [`Command::ListAudioDevices`].
    AudioDevices {
        /// Active capture endpoints, default first.
        capture: Vec<DeviceDto>,
        /// Active render endpoints, default first.
        render: Vec<DeviceDto>,
    },
    /// Reply to [`Command::ListEncoders`]. `encoders` is empty and `encoder_ready`
    /// false while the encoder probe is still running or found no encoder.
    Encoders {
        /// True once the encoder probe has finished successfully. Older daemons
        /// sent this as `ffmpeg_ready` (still accepted on read).
        #[serde(alias = "ffmpeg_ready")]
        encoder_ready: bool,
        /// Display adapters, hardware first.
        gpus: Vec<GpuDto>,
        /// Probed encoders, one per backend and codec.
        encoders: Vec<EncoderDto>,
    },
    /// Reply to [`Command::ListRunningApps`].
    RunningApps {
        /// Candidate windows, suggested games first, then by name.
        apps: Vec<RunningAppDto>,
    },
    /// Reply to [`Command::MicTest`]: a 5-second mono 16-bit WAV, base64 encoded.
    MicTest {
        /// Standard base64 (with padding) of a complete WAV file.
        wav_base64: String,
        /// Clip length in milliseconds.
        duration_ms: u32,
        /// Peak level over the clip, in dBFS (-100 = silence, 0 = full scale).
        peak_db: f32,
    },
    /// Reply to [`Command::ListLibraries`].
    Libraries {
        /// Primary root first, then extra roots in configuration order.
        libraries: Vec<LibraryDto>,
    },
    /// Reply to [`Command::ImportPlan`]. Nothing was copied.
    ImportPlan {
        /// Every candidate file in walk order. Junk and zero-byte files are not listed.
        items: Vec<ImportItemDto>,
        /// Sum of the sizes of the items that will be copied.
        total_bytes: u64,
        /// Free bytes on the primary root's volume. 0 when the query failed.
        free_bytes: u64,
        /// True when the free space covers the copy plus a 1 GiB margin.
        enough_space: bool,
        /// True when the primary root is read-only (imports refuse to start).
        primary_read_only: bool,
    },
    /// Reply to [`Command::ImportStart`].
    ImportStarted {
        /// Id of the import job; carried by its progress, done and cancel messages.
        job_id: String,
    },
    /// Reply to [`Command::ListGameNames`].
    GameNames {
        /// Game folders, most clips first.
        games: Vec<GameNameDto>,
    },
    /// Request failed.
    Error {
        /// Machine-readable error code.
        code: ErrorCode,
        /// Human-readable detail.
        message: String,
        /// Whether retrying the same request may succeed.
        retryable: bool,
    },
}

/// Unsolicited daemon-to-client events. Delivered after a matching subscribe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    /// Recording state changed.
    StateChanged {
        /// New recording state.
        state: RecState,
        /// Game being recorded, if any.
        game: Option<GameRef>,
    },
    /// A supported game was detected.
    GameDetected {
        /// The detected game.
        game: GameRef,
    },
    /// A previously detected game exited.
    GameLost {
        /// Id of the game that was lost.
        game_id: String,
        /// Why the game is considered lost.
        reason: String,
    },
    /// A new recording session started. `started_unix_ms` is the start of the whole
    /// session and does not change when the session splits into parts.
    SessionStarted {
        /// The new session.
        session: SessionInfo,
    },
    /// A new part of a session started.
    SessionPartStarted {
        /// Session id.
        session_id: String,
        /// Part number within the session.
        part: u32,
        /// Output path of the part.
        path: String,
    },
    /// A part of a session finished.
    SessionPartFinished {
        /// Session id.
        session_id: String,
        /// Part number within the session.
        part: u32,
        /// Output path of the part.
        path: String,
        /// Duration of the part in milliseconds.
        duration_ms: u64,
        /// True when `duration_ms` is not a measured value (the media probe failed), so clients can tell "unknown" from "zero". Missing in older daemons: defaults to false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        duration_unknown: bool,
        /// Size of the part in bytes.
        bytes: u64,
        /// Whether the part was repaired after an unclean end.
        recovered: bool,
        /// Start of the whole session in Unix milliseconds (the first part's start).
        started_unix_ms: i64,
        /// Parts of this session finished so far, including this one.
        parts_saved: u32,
    },
    /// A clip finished saving.
    ClipSaved {
        /// Id of the clip.
        clip_id: String,
        /// Output path of the clip.
        path: String,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Size in bytes.
        bytes: u64,
    },
    /// Periodic capture health sample.
    CaptureHealth {
        /// Current frames per second.
        fps: f32,
        /// Frames dropped since the last sample.
        dropped_frames: u64,
        /// Whether black-frame handling is active.
        black_frames_active: bool,
    },
    /// The video encoder was selected.
    EncoderSelected {
        /// Codec name, for example `h264`.
        codec: String,
        /// Encoder name, for example `nvenc_h264`.
        encoder: String,
    },
    /// Audio level sample (topic `levels`, about 10 Hz while subscribed).
    AudioLevels {
        /// Per-track levels.
        tracks: Vec<TrackLevel>,
    },
    /// Microphone mute state changed.
    MicMuteChanged {
        /// New mute state.
        muted: bool,
    },
    /// Detection was paused or resumed.
    DetectionPauseChanged {
        /// New pause state.
        paused: bool,
    },
    /// A configuration change was applied. Sent after every successful save or reload.
    ConfigChanged {
        /// Strongest apply class of the change.
        apply_class: String,
        /// True when part of the change waits until no game is attached.
        restart_required: bool,
    },
    /// Configuration was reloaded.
    SettingsReloaded {
        /// Whether the reload succeeded.
        ok: bool,
        /// Any configuration problems found.
        errors: Vec<ConfigIssue>,
    },
    /// The capture worker process was restarted.
    WorkerRestarted {
        /// Restart attempt number.
        attempt: u32,
        /// Why the worker was restarted.
        reason: String,
        /// Exit code of the previous worker, if known.
        exit_code: Option<i32>,
    },
    /// The daemon entered degraded mode.
    Degraded {
        /// Why the daemon is degraded.
        reason: String,
    },
    /// Non-fatal warning.
    Warning {
        /// Stable warning code.
        code: String,
        /// Human-readable message.
        message: String,
    },
    /// The daemon is shutting down. Always delivered, regardless of subscriptions.
    ShuttingDown {
        /// Why the daemon is shutting down.
        reason: String,
    },
    /// Microphone meter sample (topic `meter`, about 10 Hz while the meter runs).
    MicLevel {
        /// RMS level in dBFS, -100..0.
        rms_db: f32,
        /// Peak level in dBFS, -100..0.
        peak_db: f32,
    },
    /// Progress of a library scan of one root.
    LibraryScan {
        /// `storage_roots.id` of the root.
        root_id: i64,
        /// Absolute folder path of the root.
        path: String,
        /// `walking`, `probing`, `thumbnails`, `done` or `offline`.
        phase: String,
        /// Items done in the phase (files found while walking, files probed while probing).
        done: u64,
        /// Items to do in the phase, 0 when unknown.
        total: u64,
        /// Rows added so far in this scan.
        added: u64,
        /// Rows marked missing so far in this scan.
        missing: u64,
    },
    /// Library rows changed; the gallery should refresh.
    LibraryChanged {},
    /// Progress of an import job.
    ImportProgress {
        /// Job id from [`Reply::ImportStarted`].
        job_id: String,
        /// One-based index of the file being copied.
        file_index: u64,
        /// Number of files that will be copied.
        file_count: u64,
        /// Bytes finished or in progress across all files.
        bytes_done: u64,
        /// Total bytes to copy.
        bytes_total: u64,
        /// Destination file name of the current file.
        current_name: String,
    },
    /// An import job finished, failed or was cancelled.
    ImportDone {
        /// Job id from [`Reply::ImportStarted`].
        job_id: String,
        /// Files copied and added to the library.
        imported: u64,
        /// Files skipped (duplicate, already in the library, unsupported).
        skipped: u64,
        /// Files that failed, with the reason.
        failed: Vec<ImportFailDto>,
        /// True when the job stopped because of a cancel.
        cancelled: bool,
    },
    /// The daemon process is exiting for good (tray Exit, shutdown command, or
    /// Ctrl+C). Open UIs should quit too. Always delivered, regardless of subscriptions.
    AppExit,
}

impl Event {
    /// Topic this event belongs to, or `None` if it is always delivered.
    pub fn topic(&self) -> Option<Topic> {
        match self {
            Event::StateChanged { .. }
            | Event::GameDetected { .. }
            | Event::GameLost { .. }
            | Event::Degraded { .. }
            | Event::DetectionPauseChanged { .. }
            | Event::MicMuteChanged { .. } => Some(Topic::State),
            Event::ClipSaved { .. } => Some(Topic::Clips),
            Event::SessionStarted { .. }
            | Event::SessionPartStarted { .. }
            | Event::SessionPartFinished { .. } => Some(Topic::Sessions),
            Event::CaptureHealth { .. } | Event::EncoderSelected { .. } => Some(Topic::Health),
            Event::AudioLevels { .. } => Some(Topic::Levels),
            Event::MicLevel { .. } => Some(Topic::Meter),
            Event::LibraryScan { .. }
            | Event::LibraryChanged { .. }
            | Event::ImportProgress { .. }
            | Event::ImportDone { .. } => Some(Topic::Library),
            Event::WorkerRestarted { .. } | Event::Warning { .. } => Some(Topic::Warnings),
            Event::SettingsReloaded { .. } | Event::ConfigChanged { .. } => Some(Topic::Settings),
            Event::ShuttingDown { .. } | Event::AppExit => None,
        }
    }
}

/// Subscription topics for pushed events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Recording state, game detection, detection pause, mic mute.
    State,
    /// Clip saved events.
    Clips,
    /// Session part events.
    Sessions,
    /// Capture health and encoder selection.
    Health,
    /// Audio levels (10 Hz).
    Levels,
    /// Warnings and worker restarts.
    Warnings,
    /// Settings reloads.
    Settings,
    /// Microphone meter samples (see [`Command::MicMeter`]).
    Meter,
    /// Library scans, library changes and import progress.
    Library,
}

/// Machine-readable error codes carried by [`Reply::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Client and server protocol ranges do not overlap.
    ProtoMismatch,
    /// Peer is not allowed to talk to the daemon.
    Unauthorized,
    /// Malformed or out-of-range request.
    BadRequest,
    /// Unknown command type.
    UnknownType,
    /// Command is known but its handler is not implemented in this build.
    NotImplemented,
    /// Frame exceeds the size limit.
    FrameTooLarge,
    /// Operation needs an active recording.
    NotRecording,
    /// Operation is not valid in the current state.
    InvalidState,
    /// Daemon is busy with another operation.
    Busy,
    /// Daemon is shutting down.
    ShuttingDown,
    /// Capture worker is not available.
    WorkerUnavailable,
    /// Unexpected internal failure.
    Internal,
}

/// Recording state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecState {
    /// No game and no active recording.
    Idle,
    /// Waiting for a game to start recording.
    Armed,
    /// Full session is recording.
    Recording,
    /// Replay buffer only.
    BufferOnly,
    /// Session is splitting into a new part.
    Splitting,
    /// Session is stopping.
    Stopping,
    /// Recording is paused.
    Paused,
    /// Daemon is degraded.
    Degraded,
}

/// A detected game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameRef {
    /// Normalized exe name (plus store id), used as a stable key.
    pub game_id: String,
    /// Executable file name.
    pub exe: String,
    /// Process id.
    pub pid: u32,
    /// Window title, only when `diagnostics.include_window_titles` is set.
    pub title: Option<String>,
}

/// Full status snapshot returned by [`Command::GetStatus`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDto {
    /// Current recording state.
    pub state: RecState,
    /// Game being recorded, if any.
    pub game: Option<GameRef>,
    /// Active session, if any.
    pub session: Option<SessionInfo>,
    /// Seconds of footage in the replay buffer.
    pub buffer_seconds: u32,
    /// Bytes held by the replay buffer.
    pub buffer_bytes: u64,
    /// Selected encoder name.
    pub encoder: Option<String>,
    /// Selected codec name.
    pub codec: Option<String>,
    /// Target frames per second.
    pub fps_target: u32,
    /// Current bitrate in kbps.
    pub bitrate_kbps: Option<u32>,
    /// Whether the microphone is muted.
    pub mic_muted: bool,
    /// Whether detection is paused.
    pub detection_paused: bool,
    /// Autostart state.
    pub autostart: AutostartState,
    /// Capture worker process id, if running.
    pub worker_pid: Option<u32>,
    /// Worker restarts in the last 10 minutes.
    pub worker_restarts_10min: u32,
    /// True while the global hotkey service is down (it failed to start or stopped and has not been
    /// rebuilt). Hotkeys do not work while this is set. Missing in older daemons: defaults to false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hotkeys_down: bool,
    /// True while the hotkeys are suspended for capture (see [`Command::SetHotkeysSuspended`]).
    /// Omitted when false. Missing in older daemons: defaults to false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hotkeys_suspended: bool,
    /// Session parts queued or being written by the recorder (D48). Omitted when 0.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub finalizing_parts: u32,
    /// Whether the recorder is turned on (see [`Command::SetRecorderEnabled`]). Always sent.
    /// Missing in older daemons: defaults to true.
    #[serde(default = "default_true")]
    pub recorder_enabled: bool,
    /// Daemon uptime in seconds.
    pub uptime_s: u64,
    /// Reason the daemon is degraded, if it is.
    pub degraded_reason: Option<String>,
    /// The attached game with its name and attach time, if any.
    pub game_info: Option<GameInfo>,
    /// Seconds of footage in the replay buffer, with fractions.
    pub buffer_secs: f32,
    /// True when a settings change waits until no game is attached.
    pub settings_pending_restart: bool,
    /// Daemon clock in Unix milliseconds at the time of this snapshot. Clients use it
    /// to correct clock skew against their own clock.
    pub server_unix_ms: i64,
    /// Absolute path of the running executable (`std::env::current_exe`), set by the
    /// daemon. Clients use it to tell which installation is running. Omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe_path: Option<String>,
}

/// Serde default for fields that are absent in older daemons and mean "on".
fn default_true() -> bool {
    true
}

/// Serde helper: omit a zero count from the wire.
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// The attached game, as shown in status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameInfo {
    /// Display name (per-game override, or the exe name without `.exe`).
    pub name: String,
    /// Executable file name.
    pub exe: String,
    /// Window handle the recorder is attached to.
    pub hwnd: i64,
    /// When the game was attached, in Unix milliseconds.
    pub attached_since_unix_ms: i64,
}

/// One validation finding, as sent to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticDto {
    /// Doc code, for example `CFG-V001`.
    pub code: String,
    /// Error or warning.
    pub severity: DiagSeverity,
    /// Dotted key path, for example `replay.max_seconds`.
    pub path: String,
    /// Human-readable message.
    pub message: String,
}

/// Severity of a [`DiagnosticDto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagSeverity {
    /// Blocks the change.
    Error,
    /// Shown only.
    Warning,
}

/// Active session details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id.
    pub session_id: String,
    /// Current part number.
    pub part: u32,
    /// Session start in Unix milliseconds. Set from the first part and kept across splits.
    pub started_unix_ms: i64,
    /// Output path of the current part.
    pub path: String,
    /// Parts of this session finished so far.
    pub parts_saved: u32,
}

/// Autostart (HKCU Run) state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutostartState {
    /// Autostart is enabled.
    Enabled,
    /// Autostart is disabled.
    Disabled,
    /// Windows has disabled the entry.
    DisabledByWindows,
    /// The registered path no longer matches the installed binary.
    PathStale,
}

/// Audio level for one track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackLevel {
    /// Track index.
    pub track: u8,
    /// Peak level in dBFS.
    pub peak_db: f32,
    /// RMS level in dBFS.
    pub rms_db: f32,
}

/// A configuration problem reported by a settings reload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigIssue {
    /// Dotted configuration key, for example `logging.level`.
    pub key: String,
    /// Description of the problem.
    pub message: String,
}

/// An application that plays or recently played audio (see [`Reply::AudioApps`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioAppDto {
    /// Executable file name, for example `Discord.exe`.
    pub exe: String,
    /// Friendly name (file description, else the exe stem).
    pub name: String,
    /// 32x32 PNG icon, base64 without a `data:` prefix.
    #[serde(default)]
    pub icon_png_b64: Option<String>,
    /// True while the app is making sound.
    pub active: bool,
    /// Current peak meter value, 0..1.
    pub peak: f32,
    /// Unix milliseconds when the app was last seen playing audio.
    pub last_active_unix_ms: i64,
}

/// An audio endpoint (see [`Reply::AudioDevices`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDto {
    /// Endpoint id, usable as the `device` of [`Command::MicMeter`].
    pub id: String,
    /// Friendly endpoint name.
    pub name: String,
    /// True for the console default endpoint of its direction.
    pub is_default: bool,
}

/// A display adapter (see [`Reply::Encoders`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDto {
    /// Adapter description.
    pub name: String,
    /// `nvidia`, `amd`, `intel` or `other`.
    pub vendor: String,
    /// Enumeration index of the adapter.
    pub index: u32,
    /// Dedicated video memory in MiB.
    pub dedicated_mb: u64,
}

/// One probed encoder (see [`Reply::Encoders`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderDto {
    /// `nvenc`, `amf`, `qsv` or `software`.
    pub backend: String,
    /// `h264`, `h265` or `av1`.
    pub codec: String,
    /// Encoder id, for example `h264_nvenc`. Older daemons sent `ffmpeg_name`
    /// (still accepted on read).
    #[serde(alias = "ffmpeg_name")]
    pub encoder_name: String,
    /// True when the test encode succeeded.
    pub works: bool,
}

/// A visible window that can be picked as a game (see [`Reply::RunningApps`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAppDto {
    /// Executable file name.
    pub exe: String,
    /// Window title, else the exe stem.
    pub name: String,
    /// 32x32 PNG icon, base64 without a `data:` prefix.
    #[serde(default)]
    pub icon_png_b64: Option<String>,
    /// Owning process id.
    pub pid: u32,
    /// The detector would accept it, or it is already known (see `suggested`).
    pub is_game: bool,
    /// Listed in `detection.allow`. Entries already in the allow list are not returned, so this is false.
    pub in_allow_list: bool,
    /// Score of 30 or more, or a known or library game. Suggested entries come first.
    pub suggested: bool,
}

/// Picks the highest protocol version supported by both sides.
///
/// Both ranges are inclusive. Returns `None` if the ranges do not overlap
/// (for example a client that only supports v2 talking to a v1 server).
pub fn negotiate(
    client_min: u16,
    client_max: u16,
    server_min: u16,
    server_max: u16,
) -> Option<u16> {
    let lo = client_min.max(server_min);
    let hi = client_max.min(server_max);
    if lo <= hi {
        Some(hi)
    } else {
        None
    }
}

/// A storage root (see [`Reply::Libraries`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryDto {
    /// Root id, used by [`Command::ScanLibraries`].
    pub id: i64,
    /// Absolute folder path.
    pub path: String,
    /// User-visible label; empty means show the folder name.
    pub label: String,
    /// True for the primary root, where recordings and imports go.
    pub primary: bool,
    /// True if the library never writes into this folder.
    pub read_only: bool,
    /// False while the folder cannot be reached (for example a missing drive).
    pub online: bool,
    /// Clip rows, excluding missing ones.
    pub clip_count: i64,
    /// Session-part rows, excluding missing ones.
    pub session_count: i64,
    /// Sum of file sizes, excluding missing rows.
    pub total_bytes: i64,
    /// Unix milliseconds of the last completed scan, or `None` if never scanned.
    pub last_scan_unix_ms: Option<i64>,
    /// True while this root is being walked, probed or thumbnailed, or waits for a scan.
    pub scanning: bool,
}

/// One file in an import plan (see [`Reply::ImportPlan`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportItemDto {
    /// Source path. The source is only read.
    pub src: String,
    /// Source file name.
    pub name: String,
    /// Source size in bytes.
    pub size: u64,
    /// Game folder used for the destination.
    pub game: String,
    /// Destination path. For a skipped item, where it would have gone.
    pub dest: String,
    /// Why the file is not copied: `unsupported_extension`, `already_in_library`,
    /// `duplicate_in_import` or `unreadable: <reason>`. `None` means it will be copied.
    #[serde(default)]
    pub skip_reason: Option<String>,
}

/// A game folder and its clip count (see [`Reply::GameNames`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameNameDto {
    /// Display name of the game.
    pub name: String,
    /// Clip and session rows of the game, excluding missing ones.
    pub clip_count: u64,
}

/// A file that failed during an import (see [`Event::ImportDone`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFailDto {
    /// Source file name.
    pub name: String,
    /// Human-readable reason.
    pub error: String,
}

#[cfg(test)]
mod shutdown_wire_tests {
    use super::{Command, ShutdownMode};

    fn parse(text: &str) -> Result<Command, serde_json::Error> {
        serde_json::from_str(text)
    }

    #[test]
    fn bare_shutdown_has_no_data_key() {
        let cmd = Command::Shutdown {
            reason: None,
            mode: None,
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"type":"shutdown"}"#
        );
    }

    #[test]
    fn shutdown_with_mode_writes_only_present_keys() {
        let cmd = Command::Shutdown {
            reason: None,
            mode: Some(ShutdownMode::Force),
        };
        let value: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type":"shutdown","data":{"mode":"force"}})
        );
    }

    #[test]
    fn shutdown_with_reason_writes_reason_only() {
        let cmd = Command::Shutdown {
            reason: Some("update".to_owned()),
            mode: None,
        };
        let value: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type":"shutdown","data":{"reason":"update"}})
        );
    }

    #[test]
    fn empty_or_null_data_means_no_reason_no_mode() {
        for text in [
            r#"{"type":"shutdown"}"#,
            r#"{"type":"shutdown","data":null}"#,
            r#"{"type":"shutdown","data":{}}"#,
        ] {
            assert_eq!(
                parse(text).unwrap(),
                Command::Shutdown {
                    reason: None,
                    mode: None
                },
                "{text}"
            );
        }
    }

    #[test]
    fn reason_only_and_mode_only_parse() {
        assert_eq!(
            parse(r#"{"type":"shutdown","data":{"reason":"update"}}"#).unwrap(),
            Command::Shutdown {
                reason: Some("update".to_owned()),
                mode: None
            }
        );
        assert_eq!(
            parse(r#"{"type":"shutdown","data":{"mode":"force"}}"#).unwrap(),
            Command::Shutdown {
                reason: None,
                mode: Some(ShutdownMode::Force)
            }
        );
    }

    #[test]
    fn unknown_mode_is_rejected_and_unknown_keys_are_ignored() {
        assert!(parse(r#"{"type":"shutdown","data":{"mode":"later"}}"#).is_err());
        assert_eq!(
            parse(r#"{"type":"shutdown","data":{"extra":1}}"#).unwrap(),
            Command::Shutdown {
                reason: None,
                mode: None
            }
        );
    }

    #[test]
    fn effective_mode_rules() {
        use ShutdownMode::*;
        assert_eq!(ShutdownMode::effective(None, None), Graceful);
        assert_eq!(ShutdownMode::effective(None, Some("installer")), Graceful);
        assert_eq!(ShutdownMode::effective(None, Some("update")), Update);
        assert_eq!(ShutdownMode::effective(Some(Force), Some("update")), Force);
        assert_eq!(ShutdownMode::effective(Some(Graceful), None), Graceful);
    }

    #[test]
    fn encoders_reply_uses_neutral_names_and_accepts_legacy_keys() {
        use super::{EncoderDto, Reply};
        let r = Reply::Encoders {
            encoder_ready: true,
            gpus: Vec::new(),
            encoders: vec![EncoderDto {
                backend: "nvenc".into(),
                codec: "h264".into(),
                encoder_name: "h264_nvenc".into(),
                works: true,
            }],
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"encoder_ready\""), "{json}");
        assert!(json.contains("\"encoder_name\""), "{json}");
        assert!(!json.contains("ffmpeg_"), "{json}");

        let legacy = json
            .replace("encoder_ready", "ffmpeg_ready")
            .replace("encoder_name", "ffmpeg_name");
        let back: Reply = serde_json::from_str(&legacy).unwrap();
        assert_eq!(back, r);
    }
}
