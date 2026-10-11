# 06 — Session recording (`session` crate)

Status: draft for implementation. Conforms to `00-brief.md`. Where this doc refines the brief, the refinement is called out in §12 (Open questions) rather than silently diverging.

Crate: `crates/session/` (depends on `core`, `encode`, `library` (types only), `ipc` (types only)). Consumes the shared encoded packet stream; never touches capture, audio capture or the replay buffer.

---

## 1. Goal & scope

### Goal
Record the **entire** duration of a game session (from game start to game exit, or to a user-chosen stop) into one or more playable files ("parts"), driven by a state machine, without re-encoding and without dropping frames at part boundaries. Clips from the replay buffer must keep working independently of session recording.

### In scope
- Session state machine: `Idle`, `Armed`, `Recording`, `Splitting`, `Stopping`, `StoppedByUser`.
- Auto start on game detected, auto finalize on game exit (normal, crash, game switch, suspend, service shutdown).
- "Split now": finalize the current part at the next keyframe and open part N+1 on that same keyframe with zero gap; same `SessionId`, `part_index` incremented.
- "Stop session recording" at any time: finalize and keep everything recorded so far.
- Re-enable mid-game: starts a new session while the game is still running.
- Max part duration / max part size auto-split (keyframe aligned), off by default.
- Incremental, crash-safe file writer; crash recovery of unfinalized parts on service start.
- Disk-full / disk-low / volume-removed handling.
- Game crash handling (process crash, relaunch joining, hung window).
- Metadata handed to the library on finalize.
- Tray, UI and IPC commands/events for the feature.

### Out of scope
- Encoder selection and rate control (`encode`), capture (`capture`), audio routing (`audio`).
- Replay buffer segments and clip extraction (`buffer`), clip trimming (`clipedit`).
- Library schema and gallery UI (`library`, `apps/ui`) beyond the metadata contract in §3.4.
- Game detection heuristics (`detect`); this doc consumes its events only.
- Game hook / exclusive-fullscreen capture (v2).

---

## 2. User-facing behavior

### 2.1 Session modes (setting `session.mode`)

| Mode | Behavior |
|---|---|
| `auto` (default when session recording is enabled) | Game detected → session starts. Game exits → session finalizes. |
| `manual` | Session only starts via "Start session recording" (tray, UI, hotkey, IPC). Game exit still finalizes an active session. |

`session.enabled = false` disables the feature entirely (state `Idle`). Enabling it with a game already running starts a session immediately in `auto` mode.

### 2.2 Controls

| Control | Available when | Effect |
|---|---|---|
| Start session recording | `Armed` or `StoppedByUser`, a target game is currently detected | New session (new `SessionId`, part 1). Re-enable mid-game lands here. |
| Split now | `Recording` | Finalize current part at next keyframe, open next part. Button disabled while `Splitting`. |
| Stop session recording | `Recording`, `Splitting` | Finalize current part immediately (no keyframe wait). Session goes to `StoppedByUser`. Game-exit auto-start is suppressed until the game exits or the user restarts. |
| Session on/off toggle | always | Off: finalize active part (reason `UserStop`) and go `Idle`. On: go `Armed` (or start if a game is running in `auto` mode). |

Clicking "Stop" while a split is pending cancels the split and stops immediately. "Split now" while already splitting is a no-op returning `Accepted` with `already_pending: true`.

### 2.3 Automatic behavior

| Trigger | Result |
|---|---|
| Target game detected, mode `auto`, state `Armed` | New session, part 1, state `Recording`. |
| Target game exits normally | Finalize part (`GameExited`). State → `Armed` (auto) or `Idle` (feature off). Session ends. |
| Target game crashes (`ExitReason::Crashed`) | Finalize part (`GameCrashed`). If `join_relaunch_after_crash` and the same exe relaunches within `join_relaunch_window_s`, the next part is joined to the same session (part index + 1). Otherwise new session. |
| Game switch (exit of A and detect of B in same tick) | Finalize (`GameSwitched`), then new session for B if mode `auto`. |
| Max part length / size reached | Auto split at next keyframe (`MaxDuration` / `MaxSize`). Same session. |
| Encoder stream parameters change (resolution, codec, track layout) | Forced split at next keyframe (`StreamParamsChanged`). Same session. |
| Session settings change affecting layout (audio track set, codec) | Split at next keyframe (`ConfigChanged`). Same session. |
| System suspend | Finalize (`Suspend`). On resume, if the game still runs and mode is `auto`, a joined part starts (same session). |
| Service shutdown | Finalize (`ServiceShutdown`) with a 5 s budget, then the remux job is left to recovery on next start. |

### 2.4 Notifications (toasts)

| Event | Toast |
|---|---|
| Session started | None (tray icon change only). |
| Part finalized by auto split | None. |
| Disk low (warning threshold) | "Session recording: 4 GB left on D:." (once per 10 min). |
| Disk low (stop threshold) | "Session recording stopped: disk almost full on D:. Saved 1h 12m." |
| Disk full | "Session recording stopped: disk full on D:. Your recording was saved." |
| Volume removed | "Session recording stopped: D: is no longer available. Recording recovered on next start." |
| Recovery completed | "Recovered 2 interrupted session recordings." |
| Unrecoverable partial | "Could not recover 'Elden Ring 2026-10-09 14-02-11 session-part2'. The file was kept." |

---

## 3. Architecture & data flow

### 3.1 Where the session sits

```
encode::Fanout (Arc<EncodedPacket>)
   ├── buffer::Ring       (replay, always independent)
   └── session::SessionSink  (attached only while a session part exists)
            │ bounded by bytes (queue_mib), non-blocking offer()
            ▼
       session::PartWorker thread  (owns PartWriter, split/finalize ordering)
            │  fragmented MP4 (.partial)
            ▼
       disk  <recordings root>\<Game>\<name>.mp4.partial
            │ on finalize: close -> faststart remux job -> rename to .mp4
            ▼
       FinalizedPart event -> service -> library::insert_recording (kind = session-part)

session::SessionMachine (pure reducer, main service thread)
   ├── Input: detector events, commands, worker events, 1 Hz Tick, power, config
   └── Effects: StartPart, SplitAtNextKeyframe, FinalizePart, RequestKeyframe,
                Emit(IPC event), Notify(toast), JournalSet
```

Key properties:
- The machine is a **pure reducer** (`step(Input) -> Vec<Effect>`) with no I/O, so it is table-tested.
- The packet hot path never calls into the machine. Ordering between packets and split/finalize decisions is enforced by putting control messages in the **same channel** as packets (`WorkItem`), so the worker sees them in order.
- Session attaches to the fanout only while a part exists. With no session, the session costs zero CPU and zero memory.

### 3.2 Threads

| Thread | Owner | Work |
|---|---|---|
| Service main (tokio runtime) | `clipcheckd` | `SessionMachine::step`, IPC handling, tray, timers. |
| Session worker (1 per active session) | `session` | Drains `WorkItem` channel, writes fragments, handles split/finalize, sync. |
| Remux job (short-lived, pooled 1) | `session` | Faststart remux of closed parts, then rename, then `FinalizedPart`. |
| Fanout (encoder side) | `encode` | Calls `SessionSink::offer` — must never block. |

### 3.3 Container decision

Session parts are **fragmented MP4 while being written, faststart MP4 once finalized**.

Why:
- `WebView2`'s `<video>` (Chromium) plays MP4 and WebM, not MKV. The UI must play session parts directly, so MKV is out for the final file.
- Fragmented MP4 needs no index to be readable after a crash. Each fragment is cut at a keyframe, so a crash loses at most the last fragment (about one GOP, ~2 s at the default keyframe interval) plus the unsynced tail.
- A faststart remux (`moov` before `mdat`, stream copy) gives the best seeking for progressive playback. It is done once, on finalize, off the main path.

Audio in session parts: AAC-LC by default. If the global audio codec is Opus, Opus is written into MP4 (ISOBMFF Opus mapping). See §12 Q2.

Track layout inside a part: video track 1, audio tracks 1..N in the order of the audio config. Chromium plays the first audio track only; per-track selection is the editor's job (`clipedit`).

### 3.4 Metadata contract with `library`

`session` emits a `FinalizedPart` (defined in `core::events`). The service converts it to `library::NewRecording` and inserts it. The library needs these columns on `recordings` (owned by the library doc, listed here as a requirement):

| Column | Type | Notes |
|---|---|---|
| `kind` | TEXT | `session-part` for session parts (brief: `session-partN`). |
| `session_id` | TEXT NULL | ULID. Used to group parts in the gallery. |
| `part_index` | INTEGER NULL | 1-based. |
| `end_reason` | TEXT | Serialized `EndReason`. |
| `recovered` | INTEGER | 0/1. |
| `degraded` | INTEGER | 0/1 (queue overflow happened). |
| `dropped_packets` | INTEGER | Video + audio packets dropped by overflow or pre-keyframe discard. |
| `gap_ms` | INTEGER | Non-recorded time inside the part caused by overflow. |
| `needs_faststart` | INTEGER | 1 if the remux was skipped/failed and the file is still fragmented. |

Insert is an upsert keyed by `path`, so a recovered part registered twice is harmless.

---

## 4. Key types, traits and function signatures

All types live in `crates/session/src/` unless noted. Identifiers `SessionId`, `GameId` come from `core::ids` (ULID / normalized exe).

### 4.1 Configuration

```rust
// config.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode { Auto, Manual }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    pub enabled: bool,                       // session.enabled
    pub mode: SessionMode,                   // session.mode
    pub max_part_minutes: u32,               // session.max_part_minutes (0 = off)
    pub max_part_gb: u32,                    // session.max_part_gb (0 = off)
    pub min_free_space_mb: u32,              // session.min_free_space_mb
    pub queue_mib: u32,                      // session.queue_mib
    pub sync_interval_ms: u32,               // session.sync_interval_ms
    pub join_relaunch_after_crash: bool,     // session.join_relaunch_after_crash
    pub join_relaunch_window_s: u32,         // session.join_relaunch_window_s
    pub split_keyframe_timeout_ms: u32,      // session.split_keyframe_timeout_ms
    pub recover_on_start: bool,              // session.recover_on_start
    pub faststart_on_finalize: bool,         // session.faststart_on_finalize
    pub disk_check_interval_s: u32,          // session.disk_check_interval_s
    // Injected, not owned by this feature:
    pub recordings_root: std::path::PathBuf, // settings: recordings root
    pub journal_dir: std::path::PathBuf,     // %LOCALAPPDATA%\ClipCheck\sessions
}
```

### 4.2 Shared types used (in `core`)

```rust
// core::events
pub enum ExitReason { Normal, Crashed { code: u32 }, Unknown }

pub enum DetectorEvent {
    GameStarted { game: GameId, pid: u32 },
    GameEnded   { game: GameId, pid: u32, reason: ExitReason },
}

pub enum PowerEvent { Suspend, Resume }

pub enum SplitReason { UserRequested, MaxDuration, MaxSize, StreamParamsChanged, ConfigChanged }

pub enum EndReason {
    UserStop,
    GameExited,
    GameCrashed,
    GameSwitched,
    Suspend,
    Split(SplitReason),
    DiskLow,
    DiskFull,
    VolumeGone,
    WriterError,
    ServiceShutdown,
    Recovered,
}

pub struct FinalizedPart {
    pub session_id: SessionId,
    pub part_index: u32,
    pub game: GameId,
    pub path: std::path::PathBuf,        // .mp4 when faststart succeeded, else .mp4.partial
    pub started_at_ms: i64,              // unix ms UTC
    pub ended_at_ms: i64,
    pub duration_ms: u64,                // from pts, not wall clock
    pub size_bytes: u64,
    pub video: VideoParams,              // codec, width, height, fps
    pub audio: Vec<AudioParams>,         // codec, channels, sample_rate, bitrate, role
    pub end_reason: EndReason,
    pub recovered: bool,
    pub degraded: bool,
    pub needs_faststart: bool,
    pub dropped_packets: u64,
    pub gap_ms: u64,
    pub late_audio_packets: u64,
}

// core::media
pub struct EncodedPacket {
    pub track: TrackId,       // 0 = video, 1..N = audio
    pub is_video: bool,
    pub keyframe: bool,       // always false for audio
    pub pts_us: i64,          // monotonic, service clock
    pub dts_us: i64,
    pub data: bytes::Bytes,
}
```

### 4.3 Packet ingress (hot path)

```rust
// sink.rs
pub trait PacketSink: Send + Sync {
    /// Must return immediately. Never blocks the encoder thread.
    fn offer(&self, pkt: std::sync::Arc<EncodedPacket>);
}

pub struct SessionSink {
    tx: crossbeam_channel::Sender<WorkItem>,
    queued_bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    cap_bytes: usize,                               // queue_mib * 1 MiB
    overflowed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PacketSink for SessionSink {
    fn offer(&self, pkt: std::sync::Arc<EncodedPacket>) {
        // 1. If overflowed and this is not a video keyframe: count dropped, return.
        // 2. If queued_bytes + pkt.data.len() > cap_bytes: set overflowed, count dropped, return.
        //    (overflow clears on the next video keyframe; see §5.5)
        // 3. fetch_add(queued_bytes), try_send(WorkItem::Packet(pkt)). Full channel = overflow.
    }
}
```

### 4.4 Worker control messages and writer trait

```rust
// worker.rs
pub enum WorkItem {
    Open(PartSpec),               // open a new writer (first part or after a split)
    Packet(std::sync::Arc<EncodedPacket>),
    Split(SplitReason),           // mark pending; executed on next video keyframe
    CancelSplit,
    Finalize(EndReason, crossbeam_channel::Sender<Result<(), WriteError>>), // drain, close
    RequestTick(crossbeam_channel::Sender<PartStats>),
}

pub struct PartSpec {
    pub session_id: SessionId,
    pub part_index: u32,
    pub game: GameId,
    pub pid: u32,
    pub dir: std::path::PathBuf,         // <recordings root>\<Game>\
    pub file_stem: String,               // "{game} {yyyy-MM-dd HH-mm-ss} session-part{N}"
    pub video: VideoParams,
    pub audio: Vec<AudioParams>,
}

pub struct PartStats {
    pub bytes: u64,
    pub elapsed_ms: u64,                 // video pts span of this part
    pub dropped_packets: u64,
    pub degraded: bool,
    pub late_audio_packets: u64,
}

pub enum WriteError { DiskFull, VolumeGone, Io(std::io::Error) }

pub trait PartWriter: Send {
    /// Writes one packet. First packet of a part must be a video keyframe (enforced by worker).
    fn write(&mut self, pkt: &EncodedPacket) -> Result<(), WriteError>;
    /// Flushes the current fragment and calls FlushFileBuffers.
    fn sync(&mut self) -> Result<(), WriteError>;
    /// Writes the last fragment and mfra box, closes the handle. Does not remux.
    fn close(self: Box<Self>) -> Result<std::path::PathBuf, WriteError>; // returns .mp4.partial path
    fn stats(&self) -> PartStats;
}

pub trait PartWriterFactory: Send + Sync {
    fn open(&self, spec: &PartSpec, start_pts_us: i64) -> Result<Box<dyn PartWriter>, WriteError>;
}
```

`MkvWriter` (from the brief) is **not** used. The concrete factory is `Fmp4WriterFactory`, implemented over `encode::ffi::Muxer` with `movflags = frag_keyframe+empty_moov+default_base_moof`. Fragment boundary = keyframe, so every fragment starts on a keyframe.

### 4.5 Finalize / remux

```rust
// finalize.rs
pub struct FinalizeJob {
    pub part: PartSpec,
    pub partial: std::path::PathBuf,
    pub reason: EndReason,
    pub stats: PartStats,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
}

/// Runs on the remux job thread. Returns the FinalizedPart to be sent to the service.
pub fn finish_part(job: FinalizeJob, cfg: &SessionConfig, journal: &JournalWriter)
    -> FinalizedPart;

/// Faststart remux via encode::ffi (stream copy). Writes <dst>.tmp then renames.
pub fn faststart(src: &std::path::Path, dst: &std::path::Path) -> Result<(), WriteError>;

/// Atomic move: MoveFileExW(MOVEFILE_WRITE_THROUGH); retried 3x at 500 ms for AV locks.
pub fn commit_rename(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()>;
```

### 4.6 State machine (pure)

```rust
// machine.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState { Idle, Armed, Recording, Splitting, Stopping, StoppedByUser }

pub enum Command {
    SetEnabled { enabled: bool, mode: SessionMode },
    StartSession,
    SplitNow,
    StopSession,
    Shutdown,
}

pub enum WorkerEvent {
    PartOpened { part_index: u32, path: std::path::PathBuf },
    PartClosed { part_index: u32, result: Result<(), WriteErrorKind> },
    Failed(WriteErrorKind),           // DiskFull, VolumeGone, Io
    StreamParamsChanged,              // from encoder via fanout control message
    KeyframeTimeout { attempts: u32 },
    Warning(Warning),
}

pub enum Input {
    Detector(DetectorEvent),
    Command(Command),
    Worker(WorkerEvent),
    Power(PowerEvent),
    Config(SessionConfig),
    Tick { now_ms: i64, part: Option<PartStats>, free_bytes: Option<u64> },
}

pub enum Effect {
    Work(WorkItem),                              // send to worker
    StartNewPart { session_id: SessionId, part_index: u32, game: GameId, pid: u32 },
    Journal(JournalUpdate),
    Emit(IpcEvent),
    Notify(Toast),
    RequestEncoderKeyframe,
    Reply(CommandReply),                         // for IPC requests
}

pub struct SessionMachine { /* state, cfg, current_game, session, timers, pending_start */ }

impl SessionMachine {
    pub fn new(cfg: SessionConfig) -> Self;
    pub fn state(&self) -> SessionState;
    pub fn step(&mut self, input: Input) -> Vec<Effect>;
}
```

`current_game` is tracked independently of state: it is `Some` whenever a target game is detected, regardless of whether a session is active. That is how "start session while game running" and "auto start on enable" are decided.

### 4.7 Recovery

```rust
// recovery.rs
pub struct JournalEntry {
    pub session_id: SessionId,
    pub game: GameId,
    pub status: JournalStatus,         // Active, Ended
    pub parts: Vec<JournalPart>,
}
pub struct JournalPart {
    pub part_index: u32,
    pub partial: std::path::PathBuf,
    pub final_path: Option<std::path::PathBuf>,
    pub stage: PartStage,              // Writing, Closing, Remuxing, Done
    pub started_at_ms: i64,
}
pub enum PartStage { Writing, Closing, Remuxing, Done }

pub fn recover_all(journal_dir: &std::path::Path, cfg: &SessionConfig)
    -> RecoveryReport;                 // returns FinalizedPart list + failures

pub struct RecoveryReport {
    pub recovered: Vec<FinalizedPart>,
    pub failed: Vec<(std::path::PathBuf, String)>,
    pub skipped_locked: Vec<std::path::PathBuf>,
}
```

Journal files: `%LOCALAPPDATA%\ClipCheck\sessions\<session_id>.journal.json`, written with write-to-temp + `MoveFileExW(REPLACE_EXISTING)`. Written on: session start, part open (stage `Writing`), close (`Closing`), remux start (`Remuxing`), done (`Done` plus `final_path`), session end (`status = Ended`).

---

## 5. Algorithms and state machines

### 5.1 State diagram

```
                       enable (feature on)                 target game detected (mode auto)
   +------+ ---------------------------------> +-------+ ----------------------------------> +-----------+
   | Idle | <------- disable / shutdown ------ | Armed | <--- stop-finalize (game exit,       | Recording |
   +------+                                     +-------+      crash, switch, suspend,        +-----------+
      ^                                            ^  |        disk, volume) ---------------+     |      ^
      |                                            |  | StartSession (game running)         |     | split |  kf reached
      |                                            |  +-------------------------------------+-----+ req   |  (part N+1 opened)
      |                                            |                                        |     v       |
      |                                            |                                        | +-----------+
      |                                            |                                        | | Splitting |----+
      |                                            |                                        | +-----------+
      |                                            |                                        |     |
      |          disable / shutdown                |                                        |     | StopSession
      |        (Stopping -> Idle)                  |                                        v     v
      |                                            |                                    +-----------+
      +------------------------------------------- +----------------------------------- | Stopping  |
                                                                                        +-----------+
                                                                                             | close done
                                                                                             v
                                                                                     +---------------+
                                                                  StopSession ---->  | StoppedByUser |
                                                                                     +---------------+
                                                                                      game exit -> Armed
                                                                                      StartSession (game running) -> Recording
```

Simplified adjacency (authoritative version is the transition table in §5.2):

```
Idle --enable--> Armed
Armed --game start (auto) | StartSession--> Recording
Recording --SplitNow | MaxDuration | MaxSize | StreamParams | ConfigChanged--> Splitting
Splitting --keyframe--> Recording
Recording | Splitting --StopSession--> Stopping --> StoppedByUser
Recording | Splitting --game exit | crash | switch | suspend | disk | volume | writer error--> Stopping --> Armed
Recording | Splitting --disable | shutdown--> Stopping --> Idle
StoppedByUser --game exit--> Armed
StoppedByUser --StartSession (game running)--> Recording
Armed | StoppedByUser --disable--> Idle
```

### 5.2 Transition table

`Game` = a target game is currently detected (`current_game.is_some()`).

| # | From | Input | Guard | Effects | To |
|---|---|---|---|---|---|
| T1 | Idle | Command SetEnabled{true, m} | — | Journal none; if m=auto and Game: StartNewPart(new session). | Recording if Game && auto, else Armed |
| T2 | Idle | Detector GameStarted | — | Track `current_game` only. | Idle |
| T3 | Armed | Detector GameStarted | mode=auto, disk ok | StartNewPart(new session). | Recording |
| T4 | Armed | Detector GameStarted | mode=auto, disk low | Notify(DiskLow blocked). | Armed |
| T5 | Armed | Command StartSession | Game | StartNewPart(new session). | Recording |
| T6 | Armed | Command StartSession | !Game | Reply(Err NoTargetGame). | Armed |
| T7 | Armed | Command SetEnabled{false} | — | — | Idle |
| T8 | Armed | Detector GameEnded | — | `current_game` cleared. | Armed |
| T9 | Recording | Command SplitNow | — | Work(Split(UserRequested)); RequestEncoderKeyframe if no keyframe in 1 s. | Splitting |
| T10 | Recording | Tick | max_part_minutes>0 and elapsed ≥ it | Work(Split(MaxDuration)). | Splitting |
| T11 | Recording | Tick | max_part_gb>0 and bytes ≥ it | Work(Split(MaxSize)). | Splitting |
| T12 | Recording | Worker StreamParamsChanged | — | Work(Split(StreamParamsChanged)). | Splitting |
| T13 | Recording | Config changed (layout) | — | Work(Split(ConfigChanged)) (applies to next part). | Splitting |
| T14 | Recording | Command StopSession | — | Work(Finalize(UserStop)). | Stopping |
| T15 | Recording | Detector GameEnded{Normal} | same game | Work(Finalize(GameExited)). | Stopping |
| T16 | Recording | Detector GameEnded{Crashed} | same game | Work(Finalize(GameCrashed)); remember `resume_session` for join window. | Stopping |
| T17 | Recording | Detector GameEnded + GameStarted (same tick, different game) | — | Work(Finalize(GameSwitched)); `pending_start = B`. | Stopping |
| T18 | Recording | Power Suspend | — | Work(Finalize(Suspend)); remember `resume_session`. | Stopping |
| T19 | Recording | Worker Failed(DiskFull) | — | Work(Finalize(DiskFull)); Notify. | Stopping |
| T20 | Recording | Tick free < min_free | — | Work(Finalize(DiskLow)); Notify. | Stopping |
| T21 | Recording | Worker Failed(VolumeGone) | — | Work(Finalize(VolumeGone)); Notify. | Stopping |
| T22 | Recording | Worker Failed(Io) | — | Work(Finalize(WriterError)); Notify. | Stopping |
| T23 | Recording | Command SetEnabled{false} | — | Work(Finalize(UserStop)). `session.enabled=false`. | Stopping (→ Idle) |
| T24 | Recording | Command Shutdown | — | Work(Finalize(ServiceShutdown)). | Stopping (→ Idle) |
| T25 | Splitting | Worker PartOpened (keyframe reached) | — | Journal; Emit(PartOpened). | Recording |
| T26 | Splitting | Worker KeyframeTimeout{n<3} | — | RequestEncoderKeyframe. | Splitting |
| T27 | Splitting | Worker KeyframeTimeout{n≥3} | — | Work(CancelSplit); Notify(warning). | Recording |
| T28 | Splitting | Command SplitNow | — | Reply(already_pending). | Splitting |
| T29 | Splitting | Command StopSession | — | Work(Finalize(UserStop)) (abandons pending split). | Stopping |
| T30 | Splitting | Detector GameEnded | — | Work(Finalize(GameExited/GameCrashed)). | Stopping |
| T31 | Splitting | Worker Failed(*) | — | Work(Finalize(reason)). | Stopping |
| T32 | Splitting | Command SetEnabled{false} / Shutdown | — | Work(Finalize(...)). | Stopping |
| T33 | Stopping | Worker PartClosed (ok) | pending_start=None and reason=UserStop | Emit(state); remux job spawned by worker. | StoppedByUser |
| T34 | Stopping | Worker PartClosed (ok) | reason ∈ {GameExited, GameCrashed, Suspend, DiskLow, VolumeGone, WriterError, DiskFull} | — | Armed (auto & enabled) else Idle |
| T35 | Stopping | Worker PartClosed (ok) | pending_start=Some(B), mode=auto, disk ok | StartNewPart(new session for B; a GameSwitched never joins). | Recording |
| T36 | Stopping | Worker PartClosed (ok) | reason=GameCrashed and join window open and same exe relaunches | — | Armed with join marker; StartNewPart(joined) on relaunch. |
| T37 | Stopping | Worker PartClosed (ok) | reason=Split/ServiceShutdown/disable | — | per reason: ServiceShutdown/disable → Idle |
| T38 | Stopping | Worker PartClosed (err) | — | Notify(partial kept); Journal; Emit(warning) | same mapping as T33/T34/T37 |
| T39 | Stopping | any Command (Start/Split/Stop) | — | Reply(Err Busy). | Stopping |
| T40 | Stopping | Detector GameStarted | — | `pending_start` if mode=auto. | Stopping |
| T41 | StoppedByUser | Detector GameEnded | — | `current_game` cleared. | Armed (auto & enabled) else Idle |
| T42 | StoppedByUser | Command StartSession | Game | StartNewPart(new session). | Recording |
| T43 | StoppedByUser | Command SetEnabled{false} | — | — | Idle |
| T44 | StoppedByUser | Detector GameStarted | — | Suppressed (sticky until exit). | StoppedByUser |
| T45 | Recording/Splitting/Stopping | Config (session disabled via settings reload) | — | as T23 | — |
| T46 | any | Command SetEnabled{true, m} when already enabled | m changed | Mode update only; if m=auto and state Armed and Game → as T3. | — |

Notes:
- T34 with `mode=manual` goes to `Armed` (waiting for a manual start).
- T36: a crash with `join_relaunch_after_crash=true` keeps the session id and `resume_session` for `join_relaunch_window_s`. On relaunch of the same exe, the new part gets `part_index + 1` and the same session id. Window expiry clears `resume_session` (Tick).
- A game switch (T17) never joins: it is always a new session.

### 5.3 Session start (`StartNewPart`) procedure

```
1. session_id = new ULID (or resume_session.id if joining)
2. part_index = 1 (new) or last_index + 1 (joined)
3. Check disk: free ≥ min_free_space_mb, else Notify(DiskLow blocked) and abort (T4)
4. Resolve dir = recordings_root / sanitize(game.display_name); create_dir_all
5. file_stem = "{game} {local yyyy-MM-dd HH-mm-ss} session-part{N}" (sanitized, ≤120 chars)
   if "<stem>.mp4" or ".mp4.partial" exists: append " (2)", " (3)", ...
6. Journal::write(session_id, game, parts += {Writing, partial path})
7. Attach SessionSink to Fanout (fanout.subscribe(sink)); worker thread spawned if none
8. Worker::Open(spec) -> factory.open(); on error -> Failed(Io/VolumeGone) (T31/T22 path)
9. RequestEncoderKeyframe   (so the part starts quickly)
10. Emit(state_changed Recording), Emit(part_opened)
```

### 5.4 Zero-gap split procedure (executed in worker)

Trigger: `WorkItem::Split(reason)` sets `split_pending = Some(reason)` and `split_started = now`.

```
on WorkItem::Packet(pkt):
    if split_pending.is_some() && pkt.is_video && pkt.keyframe:
        // (a) close the old part with everything strictly before this keyframe
        writer_old.close()                       // fragment boundary == this keyframe
        Journal(stage = Closing); spawn FinalizeJob(old, reason = Split(reason))
        // (b) open the new part and write the keyframe as its first packet
        spec_new = spec_old with part_index + 1 (same session_id, same game)
        writer_new = factory.open(spec_new, start_pts_us = pkt.pts_us)
        writer_new.write(pkt)                    // no packet is written twice, none dropped
        split_pending = None
        Effect: Emit(part_opened), state Recording (PartOpened event -> T25)
        return

    write pkt to current writer (drop rules in §5.5 apply)

Audio packets:
    Audio with pts_us < start_pts_us of the new part but arriving after the split goes to the
    NEW part, clamped to start_pts_us and counted in late_audio_packets. Expected count is
    0 because the merger in `encode` holds audio up to 50 ms (brief-consistent jitter bound).
```

Zero-gap guarantee: the keyframe is written to exactly one part (the new one); every packet with `pts < keyframe.pts` belongs to the old part because the old writer receives them before the keyframe in arrival order. Gap = 0 in the timeline (duration of old + duration of new = total).

Timeline rebasing: each part's timestamps are rebased so its first packet has pts 0 (`pts_us - start_pts_us`), so each file plays standalone.

### 5.5 Packet overflow and degraded mode

```
queue cap = queue_mib MiB (bytes, not packets)
offer(pkt):
  if overflowed:
      if pkt.is_video && pkt.keyframe: overflowed = false (resume) -> mark degraded, gap starts/ends
      else: dropped += 1; return
  if queued + len > cap: overflowed = true; dropped += 1; warning once; return
  send
```

Worker records `degraded = true` and `gap_ms` (pts delta from first dropped to the resuming keyframe). Resume only at a video keyframe, so the file never contains an inter-frame without its reference. Overflow is never silent: it sets `degraded` in the part metadata and emits a `Warning(QueueOverflow)`.

### 5.6 Finalize procedure (worker + remux job)

```
on WorkItem::Finalize(reason, reply):
    (queue already drained up to this item, because the channel is FIFO)
    1. writer.sync()                      // flush last fragment, FlushFileBuffers
    2. partial = writer.close()           // writes mfra, closes handle; Journal(Closing)
    3. reply.send(Ok(()))                 // machine receives PartClosed -> Stopping ends
    4. remux job (background):
         if faststart_on_finalize and free ≥ size + 256 MiB:
             Journal(Remuxing)
             faststart(partial, <stem>.mp4.tmp)       // stream copy, moov first
             commit_rename(<stem>.mp4.tmp, <stem>.mp4)
             remove partial (after rename)            // remove only after successful rename
             final_path = <stem>.mp4; needs_faststart = false
         else:
             final_path = partial; needs_faststart = (faststart_on_finalize)
         Journal(Done, final_path)
         send FinalizedPart to service -> library insert
```

Why the reply is sent after `close()` and before remux: the state machine can leave `Stopping` quickly, so a new session can start while a previous part is still remuxing. Remux jobs queue behind each other (pool of 1).

Crash safety of finalize: stages are journaled; a crash after rename but before journal `Done` leaves both `.mp4` and `.mp4.partial`. Recovery (§5.8) detects that `final_path` exists and removes the stale partial only if the faststart output validates.

### 5.7 Disk handling

| Check | When | Action |
|---|---|---|
| Free space warning | Tick every `disk_check_interval_s` | If free < 2 × min_free: Notify(DiskLow warning, once per 10 min). |
| Free space stop | Tick | If free < min_free: T20 → finalize (`DiskLow`). Parts already written are kept. |
| Start guard | StartNewPart | If free < min_free: refuse, Notify. |
| Write ENOSPC | writer.write / sync | `WriteError::DiskFull` → T19. Finalize is still attempted; `close()` failing is logged and the partial kept. |
| Volume removed | write or open error `ERROR_NOT_READY (21)`, `ERROR_DEV_NOT_EXIST (55)`, `ERROR_BAD_NETPATH`, etc. | `WriteError::VolumeGone` → T21. |
| Free-space check for remux | finalize | Remux needs `size + 256 MiB` free. If not, keep the fragmented file with `needs_faststart = true`. A background retry runs every 10 min while the service is up. |

`GetDiskFreeSpaceExW` is called on the recordings volume. The replay buffer lives on another (or the same) drive; see §12 Q5.

### 5.8 Crash recovery procedure (service start)

```
if !recover_on_start: return
for each journal in journal_dir/*.journal.json:
    if status == Ended and all parts Done: delete journal (keep after 7 days if user wants) ; continue
    for part in parts where stage != Done:
        partial = part.partial (or final_path if set)
        if !exists(partial) and final_path exists and validates: mark Done; continue
        if !exists(partial): report failed(partial, "missing"); continue
        try open(partial, share = READ | DELETE, deny WRITE)  // if another ClipCheck writer holds it -> skipped_locked
        validate: probe via encode::ffi (container opens, has video stream, duration from last fragment)
           - if unreadable (0 bytes, no moov/moof): report failed; keep file; Notify
           - if readable: duration_ms = probe duration; bytes = file len
        faststart_if_possible(partial) -> <stem>.mp4 (same as §5.6 step 4 with recovered=true)
        build FinalizedPart{ end_reason: Recovered, recovered: true, duration_ms, ... }
        service inserts into library (upsert by path)
    journal.status = Ended (after recovery); remove after success
Emit(recovery report) and Notify
```

Important properties:
- Recovery never deletes a media file. A partial is removed only after its faststart replacement has been renamed into place and validated.
- Sharing: the writer opens files with `FILE_SHARE_READ | FILE_SHARE_DELETE` but not `FILE_SHARE_WRITE`; recovery opens with `FILE_SHARE_READ | FILE_SHARE_WRITE` only if it can take an exclusive lock, so it never races a live writer.
- Power loss: data written after the last `FlushFileBuffers` (≤ `sync_interval_ms`) may be lost. Fragments are at keyframes, so the file ends on a fragment boundary.

### 5.9 Game crash handling

| Situation | Detection | Behavior |
|---|---|---|
| Process exits with NTSTATUS exception code, or WER crash event | detect → `ExitReason::Crashed{code}` | T16: queued packets are still written (FIFO drain), then finalize `GameCrashed`. Join window opens (§5.2 T36). |
| Process exits normally | `ExitReason::Normal` | T15: finalize `GameExited`, new session on next start. |
| Process alive, window hung (no WGC frames) | capture emits black frames (capture policy) | Session keeps recording. No state change. A hang is not an end. |
| Window destroyed but process alive (launcher/menu) | detect policy: GameEnded after 10 s without window | Treated as normal exit (T15). |
| Process restarts within join window after crash | detect GameStarted same exe | Joined part (§5.2 T36). |

### 5.10 Split timers and ticks

Tick: every 1 s from the service loop, passed to the machine with `PartStats` requested from the worker (`RequestTick` reply channel, non-blocking via `try_recv` on previous answer).

- Max duration: `elapsed_ms ≥ max_part_minutes * 60000` → T10. The split still happens at the next keyframe, so actual part length is ≥ the limit by up to one GOP.
- Max size: `bytes ≥ max_part_gb * 2^30` → T11.
- FAT32 guard: if the recordings volume is FAT32 (4 GiB file limit), the effective size limit is `min(max_part_gb, 3.5 GiB)` and Notify once. exFAT and NTFS have no such limit here.
- Keyframe timeout: T9/T26/T27 as in §5.2.

---

## 6. Config keys owned by this feature

All keys are under `[session]` in `%APPDATA%\ClipCheck\config.toml`. Hot-reloadable: changes apply immediately to the machine; layout-changing changes apply at the next keyframe via split.

| Key | Type | Default | Range / values | Applies | Notes |
|---|---|---|---|---|---|
| `session.enabled` | bool | `false` | — | immediate | Master switch. |
| `session.mode` | enum | `"auto"` | `auto`, `manual` | immediate | See §2.1. |
| `session.max_part_minutes` | u32 | `0` | 0 (off), 1–1440 | next keyframe | Auto split by duration. |
| `session.max_part_gb` | u32 | `0` | 0 (off), 1–200 | next keyframe | Auto split by size. Capped at 3 GiB on FAT32. |
| `session.min_free_space_mb` | u32 | `2048` | 512–102400 | immediate | Start refused below; stop below; warn at 2×. |
| `session.queue_mib` | u32 | `16` | 4–128 | next session | Byte cap of the worker queue. Memory cost. |
| `session.sync_interval_ms` | u32 | `1000` | 250–5000 | next session | `FlushFileBuffers` cadence. Bounds data loss on power cut. |
| `session.join_relaunch_after_crash` | bool | `true` | — | immediate | §5.2 T36. |
| `session.join_relaunch_window_s` | u32 | `120` | 0–900 | immediate | 0 disables joining. |
| `session.split_keyframe_timeout_ms` | u32 | `4000` | 1000–15000 | immediate | Before a keyframe is requested. |
| `session.recover_on_start` | bool | `true` | — | next start | §5.8. |
| `session.faststart_on_finalize` | bool | `true` | — | immediate | If false, parts stay fragmented (`needs_faststart = true`); UI playback may be limited. |
| `session.disk_check_interval_s` | u32 | `10` | 5–300 | immediate | Free-space poll. |
| `session.split_hotkey` | string | `""` | key chord or empty | immediate | Owned here, registered via `hotkeys`. Empty = unbound. |
| `session.stop_hotkey` | string | `""` | key chord or empty | immediate | Stop session recording. No confirm dialog. |

Settings doc consumes this table. Recordings root and journal dir are settings/paths keys owned elsewhere.

---

## 7. IPC messages / events

Transport and framing belong to `ipc` (named pipe, length-prefixed JSON). Messages use `"type"` as the serde tag in snake_case.

### 7.1 Requests (UI/tray → service)

| Request | Payload | Reply |
|---|---|---|
| `session.get_status` | — | `SessionStatus` |
| `session.set_enabled` | `{ enabled: bool, mode?: "auto" \| "manual" }` | `SessionStatus` |
| `session.start` | — | `SessionStatus` or error `NoTargetGame`, `Busy`, `DiskLow` |
| `session.split_now` | — | `{ accepted: bool, already_pending: bool, error?: string }` |
| `session.stop` | — | `{ accepted: bool }`. Completion is announced via `session.part_finalized`. |

`SessionStatus`:

```json
{
  "state": "recording",
  "session_id": "01J...",
  "game": "eldenring.exe",
  "part_index": 3,
  "elapsed_ms": 4123456,
  "bytes_written": 2147483648,
  "degraded": false,
  "split_pending": false,
  "free_bytes": 53687091200,
  "mode": "auto",
  "enabled": true
}
```

### 7.2 Events (service → subscribed UI/tray)

| Event | Payload |
|---|---|
| `session.state_changed` | `{ from, to, session_id?, reason? }` |
| `session.part_opened` | `{ session_id, part_index, path }` |
| `session.part_finalized` | `{ session_id, part_index, path, duration_ms, size_bytes, end_reason, recovered, degraded, needs_faststart, recording_id? }` |
| `session.warning` | `{ kind: "disk_low" \| "disk_full" \| "volume_gone" \| "queue_overflow" \| "keyframe_timeout" \| "late_audio" \| "faststart_failed" \| "unrecoverable", message, path? }` |
| `session.recovery` | `{ recovered: u32, failed: u32, skipped_locked: u32 }` |
| `session.tick` | `{ elapsed_ms, bytes_written, part_index, free_bytes }` every 1 s while not Idle (throttled to 5 s when UI is hidden) |

### 7.3 Tray

| Item | Visible/enabled when |
|---|---|
| Start session recording | Armed / StoppedByUser, game detected |
| Stop session recording | Recording / Splitting |
| Split now | Recording |
| Mode: Auto / Manual (radio) | enabled |
| Session recording on/off (checkbox) | always |

Tray icon: grey = Armed/Idle, red dot = Recording, amber = Splitting or Stopping, dim red = StoppedByUser. Tooltip: `ClipCheck: session recording, part 3, 01:08:56`.

### 7.4 UI

Session panel (gallery top bar): state badge, elapsed, part number, buttons Split now / Stop session. Session parts are grouped by `session_id` in the gallery, with part numbers shown as "Part 1 of 3" (count from the library).

---

## 8. Edge cases & failure modes

| # | Case | Expected behavior |
|---|---|---|
| E1 | Split requested, no keyframe arrives | `RequestEncoderKeyframe` after 1 s and again every `split_keyframe_timeout_ms`. After 3 timeouts, cancel split, warn, stay Recording. |
| E2 | Split requested twice | Second is a no-op (`already_pending`). |
| E3 | Stop during Splitting | Abandon the pending split, finalize the current part now. No keyframe needed. |
| E4 | Game exits during Splitting | Finalize with the game-exit reason, pending split abandoned. |
| E5 | Game switch | New session for the new game (T17). Never joined. |
| E6 | Encoder stream params change (resolution, codec) | Forced split (`StreamParamsChanged`). If the change happens during a split, the split is reused. |
| E7 | Audio track config changes | Split at next keyframe (`ConfigChanged`). Applies to the new part only. |
| E8 | Packet queue overflow (disk too slow) | Drop until next video keyframe, `degraded = true`, `gap_ms` recorded, warning. Buffer/clips unaffected. |
| E9 | Writer `write` blocks for seconds (network share, HDD spin-up) | Queue absorbs; if it overflows, E8. Warn if a single write > 2 s. |
| E10 | Disk full mid-part | T19. Parts already synced are complete. The partial fragment is cut. Toast. Auto-start blocked until free space ≥ min_free. |
| E11 | Disk low (below min) during a session | T20: graceful finalize (`DiskLow`) before the disk is truly full. |
| E12 | Recording volume removed (USB, network drive) | T21. Finalize attempt fails; partial kept; recovery on next start if still missing → report "volume unavailable" and retry on remount. |
| E13 | Service crash / kill mid-part | Journal says `Writing`. Recovery on next start. Loss ≤ last fragment + sync interval. |
| E14 | Power loss | As E13. Fragment-aligned end. |
| E15 | Game crash with relaunch | Joined part within window (T36). Outside window → new session. |
| E16 | Game hang (no frames) | Keep recording (black frames per capture policy). Not finalized. |
| E17 | System suspend / hibernate | Finalize (`Suspend`) before sleep; resume joins if game still running. |
| E18 | Clock jump (NTP, manual change) | Durations come from pts (monotonic), wall time only in metadata. File name uses time at part start. |
| E19 | Recordings root changed mid-session | Applies to the next part (T34/StartNewPart). Current part finishes in old location. |
| E20 | Game name with illegal chars / very long | Sanitize: replace `<>:"/\|?*` and control chars with `_`; trim to 120 chars. |
| E21 | Antivirus or indexer locks the file at rename | `commit_rename` retries 3 × 500 ms. If still locked: keep `.mp4.partial`, `needs_faststart`, warn, retry in background. |
| E22 | FAT32 recordings volume | Size cap 3.5 GiB per part (§5.10). |
| E23 | Two ClipCheck service instances | Single-instance mutex on `clipcheckd` (service doc). Session additionally holds the journal lock. |
| E24 | Session settings disabled while Stopping | Finalize continues, then Idle (T37). |
| E25 | Pre-keyframe packets at part start | Dropped and counted in `dropped_packets` (normally zero because keyframe is requested in step 9 of §5.3). |
| E26 | Audio packet arrives after split with pts before new part start | Written to new part clamped to start pts; `late_audio_packets` incremented. |
| E27 | Faststart remux fails (I/O error, disk) | Keep fragmented file, `needs_faststart = true`, warn `faststart_failed`, retry in background. |
| E28 | Unfinalized partial older than 30 days and unrecoverable | Never auto-deleted. Shown in gallery as "Damaged session part" for the user to remove. |

---

## 9. Performance & resource budget

Targets: service < 15 MB RSS idle (session adds 0 when no session), < 3 % one core while recording on hardware encode (session share ≤ 0.5 %).

| Resource | Budget | How |
|---|---|---|
| RSS while recording | +queue_mib (default 16 MiB) + 2 MiB | Byte-bounded queue; packets are `Arc`-shared with the buffer ring, not copied. |
| Idle (no session) | 0 | Sink detached from fanout. |
| CPU, mux (copy) | ≤ 0.5 % one core at 60 fps 50 Mbps | Fragment mux only, no transcode. Packet copy into MP4 mdat. |
| CPU, finalize remux | Burst, background, low priority (`THREAD_MODE_BACKGROUND_BEGIN`) | Stream copy. |
| Disk write | encode bitrate + ≈ 1 % | Fragments at keyframes. |
| Disk I/O on finalize | +1 × part size read and write | Only when faststart is on. Needs `size + 256 MiB` free. |
| Syncs | 1 per `sync_interval_ms` (default 1 s) | `FlushFileBuffers`. |
| Latency split | 0 ms gap, ≤ 1 GOP to trigger | Keyframe-aligned. |
| Latency stop | < 300 ms to `StoppedByUser` (excl. remux) | Drain + close only. |
| Recovery | < 2 s per GB of partial file | Validation probe reads the tail only; faststart runs in background. |

Capacity reference (1080p60 at 20 Mbps video + 3 AAC tracks at 160 kbps): about 9 GB per hour. At 50 Mbps: about 22 GB per hour.

---

## 10. Test plan

### 10.1 Unit (pure, no I/O)
- State machine: one test per row of the §5.2 table (T1–T46), plus the sequences: split during stop, stop during split, switch during split, crash then join, suspend then resume, disable during stopping. Use a fake clock and scripted inputs; assert effects and final state.
- Property test on the machine: from any random input sequence, the state is always one of six variants, `Splitting` is never entered without `Recording` before, and `Stopping` always eventually emits `PartClosed`-triggered exit (use a fake worker that always replies).
- Config validation: range checks, FAT32 cap.
- Sanitizer and filename collision (E20, §5.2 StartNewPart step 5).

### 10.2 Worker and writer (with a fake encoder and an in-memory writer)
- Packet conservation across split: for N random packets with keyframes every 2 s, the union of all parts' packets equals the input, with no duplicates, no drops, and each part's first video packet is a keyframe.
- Zero-gap timeline: duration(part1) + duration(part2) == total span within one frame duration.
- Audio ordering: per track, pts is monotonic within each part.
- Queue overflow: slow writer → `degraded` set, drops only between a non-keyframe and the next keyframe, no P-frames without a reference in the output.
- Timeout path: no keyframe for 3 × timeout → split canceled, state back to Recording.

### 10.3 Writer on real files (Windows, temp dir)
- Fragmented MP4 is readable by `ffprobe` at any point (sample every 500 ms while writing).
- Finalize produces a faststart MP4 (`moov` before `mdat`) with correct duration; `ffprobe` duration within 50 ms of pts span.
- Faststart remux preserves all audio tracks (count and codec).
- Remux failure path keeps the partial and sets `needs_faststart`.
- Chromium playback check: the UI loads a session part via `clip://` in WebView2, plays from start, and seeks to 90 % (manual + automated with the UI test harness).

### 10.4 Failure injection
- Disk full at Nth write (inject `WriteError::DiskFull`): state → Stopping → Armed/blocked; part valid up to last sync.
- Volume removed (inject `VolumeGone`): same, plus recovery after remount.
- Antivirus lock on rename (hold handle for 2 s): retry succeeds; hold 10 s: `needs_faststart` path.
- Kill service with `TerminateProcess` at random points (200 runs): on restart, recovery yields a valid file whose duration ≥ (written duration − sync_interval − fragment length), and no media file is deleted.
- Power-loss simulation: write with `FILE_FLAG_NO_BUFFERING` disabled, then truncate the file at a random byte offset after the last sync; recovery produces a playable file.

### 10.5 Integration with other crates
- Replay buffer independence: start/stop session 100 times during a 30-minute synthetic stream; clip extraction from buffer succeeds at each step, and clip durations are unchanged.
- Clip after session off: buffer continues; clip request succeeds.
- Detector events: simulated game start/exit/crash/switch sequences produce the expected parts in the library (`insert_recording` mock).

### 10.6 Soak and manual
- 8-hour soak with synthetic 1080p60 encode: RSS growth < 2 MB after warm-up; no handle leaks (count handles before and after 100 splits).
- Real games (manual checklist): one game per GPU vendor path (NVENC, AMF, QSV, x264). For each: start/split/stop 3×, alt-tab, Alt+F4 crash, Task Manager kill of game, relaunch within window.
- Suspend/resume on a laptop: part finalized before sleep, joined part after resume.
- Full disk on a 2 GB test volume with min_free 512 MB: stops gracefully with a playable file.

### 10.7 Acceptance (must pass before merge)
- All §10.1–10.5 automated tests green.
- E8, E10, E12, E13, E15 covered by automated tests.
- Packet conservation test passes 1000 random seeds.

---

## 11. Implementation task breakdown

Each task is roughly 0.5–2 days, in dependency order.

| # | Task | Depends on | Output | Est. |
|---|---|---|---|---|
| S1 | Config struct, validation, serde, hot-reload diff (layout vs non-layout) | core | `config.rs`, unit tests | 1 d |
| S2 | Core event/type additions (`EndReason`, `FinalizedPart`, `SplitReason`, `DetectorEvent`, `PowerEvent`) and `EncodedPacket` adjustments | core | types + serde tests | 0.5 d |
| S3 | Pure `SessionMachine` with transitions T1–T46, effects enum | S1, S2 | `machine.rs` + table tests | 2 d |
| S4 | `SessionSink` with byte-bounded queue and overflow policy (§5.5) | S2 | `sink.rs` + tests with fake fanout | 1 d |
| S5 | `PartWorker` loop: Open/Packet/Split/Finalize ordering, zero-gap split (§5.4), timeline rebasing | S4 | `worker.rs` + conservation property test | 2 d |
| S6 | `Fmp4WriterFactory` over `encode::ffi::Muxer` (fragmented MP4, keyframe fragments, sync, close/mfra) | S5, encode ffi | `writer_fmp4.rs` + ffprobe tests | 2 d |
| S7 | Faststart remux, atomic rename with retries, disk space checks (§5.6, §5.7) | S6 | `finalize.rs` + failure tests | 1.5 d |
| S8 | Journal read/write (atomic), part stages, journal lock | S5 | `journal.rs` + tests | 1 d |
| S9 | Recovery on start (§5.8): validation probe, rename, faststart, report | S7, S8 | `recovery.rs` + kill-injection tests | 2 d |
| S10 | Service integration: attach sink to fanout, timers (Tick), disk poll, power events, detector wiring, library insert, IPC requests/events | S3–S9, ipc, library | `session` public API, integration tests | 2 d |
| S11 | Tray items, toasts, session panel in UI (IPC bindings) | S10, ipc | apps/clipcheckd tray, apps/ui store | 1.5 d |
| S12 | Hotkeys (`session.split_hotkey`, `session.stop_hotkey`) | S10, hotkeys | bindings + conflict notice | 0.5 d |
| S13 | Soak, handle-leak and failure-injection harness; manual real-game checklist | S10 | scripts, CI job (soak nightly) | 1.5 d |

Critical path: S1 → S3 → S5 → S6 → S7 → S9 → S10 → S11. S2, S4, S8 can proceed in parallel after S1.

---

## 12. Open questions

| # | Question | Proposed answer / impact |
|---|---|---|
| Q1 | The brief says session parts are "MKV/MP4". This doc picks **fragmented MP4 → faststart MP4**, because WebView2 cannot play MKV in `<video>`. Confirm the brief may be updated to say "MP4". | Proposed: yes, update brief §Core data pipeline wording. Affects library and clipedit (they must accept MP4 session parts). |
| Q2 | Opus audio: the brief allows Opus only in MKV. With MP4 as the session container, Opus must be written into MP4. Chromium supports Opus in MP4, but this needs confirmation for every target WebView2 version, and FFmpeg's mov muxer needs Opus support in the shipped LGPL build. | Option A: write Opus into MP4 (proposed). Option B: session forces AAC or disables faststart for Opus setups. Owned jointly with the `audio` doc. |
| Q3 | Chromium progressive playback of a faststart MP4 with 3–5 audio tracks: only the first audio track plays. Is that acceptable for the UI, with the editor exposing other tracks? | Proposed: yes for v1. The gallery player offers a track selector later (v1.1) using clipedit remux. |
| Q4 | Auto-split by size on FAT32 is capped at 3.5 GiB. Should the UI warn when the recordings root is FAT32 or refuse to start? | Proposed: warn once, cap automatically. |
| Q5 | Replay buffer and session share a drive by default (`%LOCALAPPDATA%` vs `%USERPROFILE%\Videos`) in many setups. If the disk fills, the buffer write fails too. Who has priority? | Proposed: session stops first (T20 at 2 GiB), so the buffer keeps working until the hard floor. Needs a buffer-side floor (buffer doc). |
| Q6 | Join across crash (T36) changes the meaning of `session_id` ("one game run" vs "one user session"). Is a crash-relaunch one session? | Proposed: yes, joined only within 120 s of the same exe. Documented in gallery grouping. |
| Q7 | Should session start be allowed while the encoder is not running for the buffer (session-only encode)? | Proposed: yes. The encoder runs whenever either sink is attached. Settings doc must not disable encode when only session is on. |
| Q8 | Brief says "Stop/disable: ... then idle (until re-enabled or next game launch depending on settings)". This doc makes `StoppedByUser` sticky until the game exits, and never auto-starts mid-game. Is that the intended meaning? | Proposed: yes. An alternative "auto-resume on next game launch only" is what this already does. |
| Q9 | Should a finalized session part be automatically moved into a per-session folder for multi-part sessions? | Proposed: no. Files stay in `<Game>\`, grouped by `session_id` in the DB. Avoids surprises for users who browse the folder. |
