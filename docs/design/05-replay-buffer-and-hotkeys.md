# 05 — Replay buffer and clip hotkeys (`buffer`, `hotkeys` crates)

> **Current hotkey backend (D53):** the shipped Windows backend is a non-swallowing low-level keyboard hook (Section 5.8). Parts of this document written before D53 (the RegisterHotKey, `Auto`/`Register`/`Hook` method, `consume_keys`, conflict-table and task-plan material) describe the earlier design. Where they differ from Sections 2.2, 4.9 and 5.8, those sections win.

Status: draft for review
Crates: `crates/buffer/`, `crates/hotkeys/`, plus a small `apps/clipcheckd/src/feedback/` module (toast + sound)
Depends on: 00-brief (process model, packet fan-out, capture policy, config locations), encode doc (`EncodedPacket` producer, closed-GOP rule, keyframe interval), session doc (session sink, split), library doc (ingests `ClipSaved`), settings doc (consumes Section 6), ipc doc (Section 7 message encoding)

Conventions used below: `TimeUs = i64` is the pipeline clock (QPC-derived, monotonic, microseconds since service start). Wall-clock time is used only for file names and UI display, never for clip math.

---

## 1. Goal & scope

### 1.1 Goal
Keep the last N minutes of game footage available at all times while a game is being captured, so the user can press a hotkey and get a clip of the last 15 s, 30 s, 2 min, 5 min (or any configured duration) in well under two seconds, with no re-encode and no stall in the recording pipeline.

### 1.2 In scope
| Area | In | Out |
|---|---|---|
| Ring buffer | GOP-aligned fMP4 segments on disk; optional RAM ring; segment naming, rotation, eviction, leases | Full-session file writing (session doc) |
| Crash recovery | Detecting and salvaging leftover segments from a previous service run | Automatic clip creation from recovered footage (user must choose "Save as clip") |
| Clip extraction | Lossless remux of a snapped keyframe-aligned window, concurrent with session recording | Re-encode trim (clipedit crate) |
| Clip timing | Keyframe snapping, pre-pad, post-roll | Per-clip manual trim UI |
| Hotkeys | Multiple clip hotkeys with individual durations, validation vs buffer max, non-swallowing low-level keyboard hook (no RegisterHotKey), tap/long gestures, conflict detection | Full remap UI (settings doc) |
| Feedback | Sound + small on-screen toast on clip saved/failed | Notification center integration |
| Async write | Clip write never blocks the packet pipeline | Thread scheduling policy for encoders (encode doc) |
| Disk guards | Ring cap, free-space floor, clip-output pre-flight check | Session disk policy beyond a shared free-space query |
| Toggle | Buffer enabled/disabled independent of session recording | Session start/stop policy |

### 1.3 Out of scope (explicit)
- Clips created from the session file. Clips always come from the ring.
- Game-hook (DLL) input capture. Hotkeys are system-level only.
- Editing clips after creation (clipedit).
- Mic push-to-talk hotkey (audio doc). This doc only defines the hotkey machinery it can reuse.

---

## 2. User-facing behavior

### 2.1 Buffer on/off
- Settings toggle "Instant replay" (`buffer.enabled`). Default **on**.
- Toggling it never starts or stops a full session recording.
- When on and a game is being captured, the ring fills continuously even if no session is recording.
- When off, no segments are written, no ring disk I/O happens, and hotkeys show the toast "Replay buffer is off" (see 2.5).

### 2.2 Clip hotkeys
- The user defines any number of clip hotkeys. Each has a chord, a duration in seconds, and an enable flag.
- Defaults (all `Ctrl+Shift+F*`, chosen to avoid Intel GPU hotkeys on Ctrl+Alt+F*, Alt+F4, and Alt+Tab conflicts):

| id | Keys | Seconds | Default enabled |
|---|---|---|---|
| `clip-15s` | Ctrl+Shift+F8 | 15 | no |
| `clip-30s` | Ctrl+Shift+F9 | 30 | yes |
| `clip-2m` | Ctrl+Shift+F10 | 120 | yes |
| `clip-5m` | Ctrl+Shift+F11 | 300 | yes |

- A press saves a clip immediately (no confirmation dialog). The hotkey handler returns in under 1 ms; the save runs asynchronously.
- Repeated presses of the same hotkey within 1.5 s are ignored (debounce). Presses of different hotkeys are independent.
- A bare key (no modifier) is rejected in validation, except F13 to F24.
- Each clip has a `press`: `"tap"` (default) or `"long"`. A long clip fires once its key has been held for `LONG_PRESS_MS` (400 ms, `clipcheck_core::config::schema`). A press shorter than that does nothing.
- Two clips may share one chord only when one is `tap` and the other is `long`. Every other shared chord is a conflict (CFG-H002), including a session hotkey on a clip's chord.
- Timing when a chord has both a tap and a long clip: a tap fires on release, so the tap clip saves when the key is let go, not on press. A hold fires the long clip once, at 400 ms, and the later release fires nothing. A chord with only a tap clip still fires on press with no added delay. A chord with only a long clip fires at 400 ms of hold.
- Hotkeys never swallow a key. A ClipCheck chord is observed by a low-level keyboard hook that always passes the key on, so Pause, F9 or Alt+F7 keep their normal function in other apps and in the game (D53).
- The hook ticks every 10 ms (`WM_TIMER` on the hotkey thread) only while a hold is pending. Auto-repeat is not a new press, because the hook tracks which keys are down.

### 2.3 Duration validation
- Allowed range per hotkey: `5 <= seconds <= buffer.max_minutes * 60`.
- The settings UI rejects out-of-range values on save with `DurationExceedsBuffer { max_seconds }`.
- If the user later lowers `buffer.max_minutes` below an existing hotkey duration, that hotkey is set to status `Invalid(DurationExceedsBuffer)`. It is not clamped silently. The service shows one toast on apply: "Hotkey Ctrl+Shift+F11 disabled: 5 min is longer than the 3 min buffer."

### 2.4 Clip contents
- A "Past N" clip covers `[head - N - pre_pad, head + postroll]`, snapped backward to the nearest keyframe (Section 5.3).
- The clip is therefore never shorter than requested unless the buffer does not contain enough footage. In that case the clip is saved with whatever exists and is flagged `truncated`.
- Clips include all audio tracks that the session would include, using the same track layout.
- Output is MP4 when audio is AAC (default), MKV when audio is Opus. The container is chosen by audio codec, not by user choice.

### 2.5 Feedback
| Event | Sound | Toast (bottom-right of game monitor, configurable corner) |
|---|---|---|
| Clip saved | Short chime (bundled `clip-saved.wav`) | "Saved 30 s clip" + sub-line with output name, fades after 2 s |
| Clip saved, truncated | Same chime | "Saved 1 m 12 s (buffer had less than 2 m)" |
| Clip failed | Error tone (bundled `clip-error.wav`) | "Clip not saved: not enough disk space (needs 1.2 GB, 0.4 GB free)" |
| Buffer off hotkey press | none | "Replay buffer is off" |
| No game captured | none | "No game is being recorded" (only if buffer is Idle or Off; if Frozen, clip still works) |
| Busy (queue full) | none | "Clip queue busy, try again in a moment" |
| Keyboard hook cannot be installed at start | none | "Hotkeys not started: keyboard hook could not be installed (Win32 error N)" |

- The toast is a click-through, no-activate, topmost layered window owned by `clipcheckd` (no webview). It never takes focus from the game.
- WGC captures only the game HWND, so the toast can never appear inside a recorded clip. This is a property to keep, and it is in the test plan.
- In exclusive fullscreen the toast may be invisible; the sound still plays. This is documented.

### 2.6 Recovery
- After a crash, the next service start offers "Recovered N s of replay footage from the previous session" in the UI (ipc event `BufferRecoveredAvailable`). The user can save it as a clip or discard it. Recovered footage is kept for `buffer.recovered_ttl_hours` (default 24) and is never auto-saved.

---

## 3. Architecture & data flow

### 3.1 Position in the pipeline

```
Encoder (encode crate)
   |  Arc<EncodedPacket>  (video + N audio tracks, pipeline clock)
   v
PacketFanout (buffer crate exports the trait; fanout owned by clipcheckd recorder)
   |                                      |
   | bounded channel, try_send, never blocks
   v                                      v
BufferSink (buffer crate)            SessionSink (session crate)
   |  segment writer thread               |  session writer thread
   v                                      v
%LOCALAPPDATA%\ClipCheck\buffer\run-*\   %USERPROFILE%\Videos\ClipCheck\<Game>\*.mkv|mp4

                 ^ reads sealed segments + committed prefix of open segment
                 |  (never touches the session file, never touches the packet channel)
ClipWorker (buffer crate, one background thread, bounded queue of 4)
   |  Ring::snapshot_for_clip -> ClipPlan (leases)
   |  ClipWriter::write -> <name>.partial -> rename -> ClipOutcome
   v
Feedback (clipcheckd::feedback: sound + toast)  and  IPC event ClipSaved -> library ingests row
```

### 3.2 Crate responsibilities
| Crate | Owns |
|---|---|
| `core` | `EncodedPacket`, `TrackKind`, `TimeUs`, `PacketSink` trait, error base types, ULID newtypes |
| `buffer` | `BufferConfig`, `Ring`, `SegmentWriter`, `SegmentLease`, `BufferSink`, `ClipPlan`, `ClipWriter`, `ClipWorker`, `Recovery`, disk guard, buffer state machine |
| `hotkeys` | `Chord`, `Binding`, `HotkeyRegistry`, non-swallowing keyboard hook backend (`win.rs`, `hook_state.rs`), tap/long gestures, conflict analysis, `validate_clip_seconds` |
| `clipcheckd` | Wiring: builds the fanout, starts/stops encoder based on `buffer` + `session` demand, dispatches `HotkeyEvent` to `ClipWorker`, `feedback` module, emits IPC events |
| `library` | Inserts `recordings` row with `kind = clip-<dur>` on `ClipSaved` |

Note: `buffer` never talks to SQLite and never talks to IPC directly. It returns outcomes; `clipcheckd` turns them into events. This keeps `buffer` testable without a service.

### 3.3 Demand-driven encoder
The encoder runs when at least one sink wants packets:

```
encoder_wanted = (buffer.enabled && capture_active) || session.wants_packets
```

| buffer | session | Encoder | Sinks attached | Extra cost of buffer |
|---|---|---|---|---|
| off | off | stopped | none | 0 |
| off | on | running | session | 0 |
| on | off | running | buffer | disk writes only |
| on | on | running | buffer + session | disk writes only (encode is shared) |

Sinks attach and detach independently. A sink attaching mid-stream waits for the next keyframe (gate, Section 5.1) so it never receives a stream that starts mid-GOP.

---

## 4. Key types, traits and function signatures

### 4.1 Shared types (`core`)

```rust
pub type TimeUs = i64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind { Video, Audio }

pub struct EncodedPacket {
    pub track: u8,              // 0 = video, 1.. = audio tracks in layout order
    pub kind: TrackKind,
    pub pts_us: TimeUs,         // presentation time, pipeline clock
    pub dts_us: TimeUs,         // decode time, pipeline clock (closed GOP: dts <= pts)
    pub duration_us: u32,
    pub keyframe: bool,         // video IDR only; audio always false
    pub data: bytes::Bytes,     // cheap clone
}

pub trait PacketSink: Send {
    fn name(&self) -> &'static str;
    /// Called on the sink's own thread. Must not block indefinitely.
    fn on_packet(&mut self, p: &EncodedPacket) -> Result<(), SinkError>;
    /// Seal/finalize. Called once on detach or stop.
    fn on_stop(&mut self) -> Result<(), SinkError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("disk write failed: {0}")] Io(#[from] std::io::Error),
    #[error("mux failed: {0}")] Mux(String),
    #[error("disk low: {free_bytes} bytes free")] DiskLow { free_bytes: u64 },
}
```

### 4.2 Fan-out contract (implemented in `clipcheckd`, specified here)

```rust
pub struct SinkHandle {
    tx: crossbeam_channel::Sender<Arc<EncodedPacket>>,  // bounded
    overruns: AtomicU64,
}

impl PacketFanout {
    /// Called by encoder thread for every packet. Never blocks.
    pub fn publish(&self, p: Arc<EncodedPacket>) {
        for h in self.sinks.read().iter() {
            if h.tx.try_send(p.clone()).is_err() {
                h.overruns.fetch_add(1, Relaxed);
                // Sink marks itself discontinuous; see Section 8 edge case E4.
            }
        }
    }
}
```

Bounded capacity: 512 packets per sink (about 1 s of video plus audio at 60 fps and 3 tracks).

### 4.3 Buffer config (`buffer` crate)

```rust
pub struct BufferConfig {
    pub enabled: bool,
    pub max_minutes: u8,                 // 1..=60
    pub segment_ms: u32,                 // 1000..=4000, default 2000
    pub fragment_ms: u32,                // 100..=1000, default 500
    pub storage: BufferStorage,
    pub min_free_disk_bytes: u64,        // ring floor, default 2 GiB
    pub max_disk_bytes: Option<u64>,     // None = auto (Section 6)
    pub pre_pad: Duration,               // 0..=5 s
    pub postroll: Duration,              // 0..=10 s
    pub keep_on_disable: bool,
    pub frozen_ttl: Duration,            // 0..=3600 s
    pub keep_recovered: bool,
    pub recovered_ttl: Duration,         // 1 h..=7 d
    pub clip_min_free_bytes: u64,        // output pre-flight margin, default 512 MiB
    pub clip_dir: Option<PathBuf>,       // None = <recordings root>\<Game>\
    pub name_template: String,           // default "{game} {yyyy-MM-dd HH-mm-ss} clip-{dur}"
}

pub enum BufferStorage {
    Disk { dir: PathBuf },               // default %LOCALAPPDATA%\ClipCheck\buffer
    Ram { max_bytes: u64 },              // effective length may be shorter, see 5.6
}

impl BufferConfig {
    /// Maximum retained duration after RAM cap adjustment.
    pub fn effective_max(&self, est_bitrate_bps: u64) -> Duration;
}
```

### 4.4 Identifiers and segment metadata

```rust
pub struct RunId(pub ulid::Ulid);        // one per buffer enable / capture start
pub struct SegmentSeq(pub u64);          // monotonic within a run

pub struct SegmentEntry {
    pub run: RunId,
    pub seq: SegmentSeq,
    pub epoch: u32,                      // bumps on codec/track signature change or gap
    pub start_us: TimeUs,                // first packet pts (min across tracks)
    pub keyframes: Vec<TimeUs>,         // video IDR pts, ascending
    pub track_sig: u64,                  // hash of codec params + track layout
    pub source: SegmentSource,
    pub shared: Arc<SegmentShared>,
    pub leases: AtomicU32,
    pub evicted: AtomicBool,             // removed from index; delete when leases reach 0
}

pub struct SegmentShared {
    pub committed_bytes: AtomicU64,      // readable prefix length (fragment granularity)
    pub committed_end_us: AtomicI64,     // pts of last committed fragment end
    pub sealed: AtomicBool,
}

pub enum SegmentSource {
    File(PathBuf),                       // "...mp4" or "...mp4.part" while open
    Ram(Arc<RamBlob>),
}
```

### 4.5 Ring and leases

```rust
pub struct Ring {
    inner: parking_lot::RwLock<RingInner>,
    cfg: BufferConfig,
}

struct RingInner {
    segments: VecDeque<Arc<SegmentEntry>>,   // ascending seq
    total_bytes: u64,
    gap_epoch_end: Option<TimeUs>,           // latest discontinuity; clips never cross it
    current_epoch: u32,
}

pub struct SegmentLease {
    seg: Arc<SegmentEntry>,
    pub bound_bytes: u64,                    // readable length at lease time
    pub end_us: TimeUs,                      // committed_end_us at lease time (open seg) or end (sealed)
}

impl Drop for SegmentLease {
    fn drop(&mut self) {
        if self.seg.leases.fetch_sub(1, AcqRel) == 1
            && self.seg.evicted.load(Acquire) {
            let _ = remove_backing(&self.seg.source);  // deferred delete
        }
    }
}

pub struct ClipPlan {
    pub leases: Vec<SegmentLease>,
    pub origin_us: TimeUs,                   // snapped keyframe; output t=0
    pub end_us: TimeUs,                      // exclusive
    pub truncated: bool,
    pub est_bytes: u64,
    pub streams: Vec<StreamLayout>,          // from first lease's track_sig
}

impl Ring {
    pub fn head_us(&self) -> Option<TimeUs>;
    pub fn snapshot_for_clip(
        &self,
        want: Duration,
        pre_pad: Duration,
        head_at_request: TimeUs,
    ) -> Result<ClipPlan, ClipError>;
    pub fn evict_if_needed(&self, free_bytes: u64) -> EvictReport;
    pub fn clear_frozen(&self);
}
```

### 4.6 Segment writer and buffer sink

```rust
pub struct BufferSink {
    cfg: BufferConfig,
    ring: Arc<Ring>,
    writer: Option<SegmentWriter>,       // None until first IDR (gate)
    gate_open: bool,
    audio_hold: VecDeque<EncodedPacket>, // at most 200 ms, see 5.2
    last_pts: Option<TimeUs>,
    epoch: u32,
}

impl PacketSink for BufferSink { /* 5.2 */ }

pub struct SegmentWriter {
    run: RunId,
    seq: SegmentSeq,
    file: Option<std::fs::File>,         // or RamBlob for RAM ring
    mux: MuxState,                       // libav fMP4 muxer, custom AVIO
    shared: Arc<SegmentShared>,
    keyframes: Vec<TimeUs>,
    start_us: TimeUs,
    last_fragment_end: TimeUs,
}

impl SegmentWriter {
    pub fn open(run: RunId, seq: SegmentSeq, start_us: TimeUs,
                tracks: &[StreamLayout], dir: &Path) -> Result<Self, SinkError>;
    pub fn write_packet(&mut self, p: &EncodedPacket) -> Result<(), SinkError>;
    /// Flushes a fragment and publishes committed_bytes. Called every fragment_ms.
    pub fn commit_fragment(&mut self) -> Result<(), SinkError>;
    /// Writes the trailer, renames .part -> final, returns the sealed entry.
    pub fn seal(self) -> Result<Arc<SegmentEntry>, SinkError>;
}
```

Muxer settings (fixed in code, not user-configurable):

| Option | Value | Why |
|---|---|---|
| format | `mp4` | fMP4 is crash-tolerant and playable prefix |
| movflags | `frag_custom+empty_moov+default_base_moof` | moov first, fragments self-describing |
| `use_editlist` | `0` | packets keep absolute pipeline timestamps (tfdt64 = absolute pts in track timescale) |
| tfdt | absolute, 64-bit | removes per-segment rebasing at clip time |
| fragment | manual `commit_fragment` every `fragment_ms` | bounds the clip-latency window |

### 4.7 Clip pipeline

```rust
pub struct ClipRequest {
    pub request_id: ulid::Ulid,
    pub source: ClipSource,
    pub seconds: u32,
    pub requested_at_us: TimeUs,         // head at press time
    pub game: String,                    // for name template
    pub out_dir: PathBuf,
    pub name_template: String,
    pub pre_pad: Duration,
    pub postroll: Duration,
}

pub enum ClipSource {
    Hotkey { binding_id: String },
    Ui,
    Recovered { recovery_id: ulid::Ulid },
}

pub struct ClipOutcome {
    pub request_id: ulid::Ulid,
    pub path: PathBuf,
    pub duration: Duration,
    pub truncated: bool,
    pub bytes: u64,
    pub took: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("replay buffer is off")] Disabled,
    #[error("no game is being captured")] NoCapture,
    #[error("no keyframe in buffer yet")] NoKeyframe,
    #[error("buffer holds no usable footage")] Empty,
    #[error("not enough disk space: need {needed} bytes, {free} free")]
    NoSpace { needed: u64, free: u64 },
    #[error("clip queue full")] QueueFull,
    #[error("mux error: {0}")] Mux(String),
    #[error("io error: {0}")] Io(#[from] std::io::Error),
}

pub struct ClipWorker { tx: crossbeam_channel::Sender<ClipJob>, /* thread handle */ }

impl ClipWorker {
    pub fn start(ring: Arc<Ring>, cfg: BufferConfig, out: Sender<ClipEvent>) -> Self;
    /// Returns immediately. Err(QueueFull) when 4 jobs are pending.
    pub fn submit(&self, req: ClipRequest) -> Result<(), ClipError>;
    pub fn cancel_all(&self);            // used on shutdown; in-flight job checks a flag per packet
}

pub enum ClipEvent {
    Started { request_id: ulid::Ulid },
    Saved(ClipOutcome),
    Failed { request_id: ulid::Ulid, error: String },
}

pub struct ClipWriter;
impl ClipWriter {
    /// Pure function of (plan, output path). Writes "<out>.partial", renames to out on success.
    pub fn write(plan: &ClipPlan, out: &Path, cancel: &AtomicBool)
        -> Result<ClipStats, ClipError>;
}
```

### 4.8 Recovery

```rust
pub struct RecoveredSet {
    pub id: ulid::Ulid,
    pub dir: PathBuf,                    // %LOCALAPPDATA%\ClipCheck\buffer\recovered\<run>
    pub segments: Vec<RecoveredSegment>,
    pub span: Duration,
    pub bytes: u64,
    pub created_unix_ms: i64,
}

pub struct RecoveredSegment {
    pub path: PathBuf,
    pub start_us: TimeUs,                // parsed from file name
    pub end_us: TimeUs,                  // from last fragment's tfdt + duration
    pub keyframes: Vec<TimeUs>,          // from trun sample flags
}

pub fn recover_leftovers(buffer_root: &Path, current: RunId) -> RecoveryReport;
pub fn salvage_part(path: &Path) -> Result<Option<RecoveredSegment>, RecoveryError>;
```

### 4.9 Hotkeys crate

Crate `crates/hotkeys` (`clipcheck-hotkeys`). Pure parts (no Win32): `chord.rs` (`Chord`, `Key`, parsing, display), `registry.rs` (`Binding`, `HotkeyRegistry::build`, conflict and reserved-chord analysis), `gesture.rs` (`GestureTracker`, tap versus long decisions from timestamps), `hook_state.rs` (`HookState`, chord matching over key events, Windows only). Windows backend: `win.rs`, which is the only file that makes Win32 calls.

Public API (Windows):

```rust
pub struct HotkeyService;                       // owns the hotkey-msg thread; Drop/stop() removes the hook and joins
impl HotkeyService {
    pub fn start(registry: &Registry, tx: Sender<HotkeyEvent>) -> Result<(HotkeyService, Vec<RegisterFailure>), StartError>;
    pub fn start_with_threshold(registry: &Registry, tx: Sender<HotkeyEvent>, long_press_ms: u32) -> Result<(HotkeyService, Vec<RegisterFailure>), StartError>;
    pub fn apply(&self, registry: Registry) -> Result<Vec<RegisterFailure>, ApplyError>;   // replaces the chord table
    pub fn set_suspended(&self, suspended: bool);                                           // UI key capture; auto-clears after 60 s
    pub fn stop(self);
}
pub struct HotkeyEvent { pub action: HotkeyAction }
pub enum StartError { Spawn(String), Window(u32), Hook(u32), NoReply }   // Hook: WH_KEYBOARD_LL could not be installed
pub enum FailureReason { TakenByAnotherApp, Os(u32) }                    // TakenByAnotherApp is never produced by the hook backend
```

The `RegisterFailure` list is always empty. It stays in the signatures so callers do not change. See D53 and Section 5.8.

### 4.10 Feedback (`clipcheckd::feedback`)

```rust
pub struct Feedback {
    sound: Option<SoundPlayer>,          // None if disabled or device unavailable
    toast: Option<ToastWindow>,          // None if disabled
}

impl Feedback {
    pub fn clip_saved(&self, o: &ClipOutcome);
    pub fn clip_failed(&self, label: &str, reason: &str);
    pub fn info(&self, text: &str);      // toast only, no sound
}

struct ToastWindow { hwnd: HWND, /* render thread */ }
struct SoundPlayer { saved: PathBuf, error: PathBuf }   // PlaySoundW(SND_FILENAME | SND_ASYNC | SND_NODEFAULT)
```

---

## 5. Algorithms and state machines

### 5.1 Buffer state machine

```
          enable (no capture)            capture starts
   +-------+ -----------------> +------+ ------------------> +---------+
   |  Off  |                    | Idle |                     | Running |<----+
   +-------+ <----------------- +------+ <------------------ +---------+     |
      ^  disable                    ^   frozen ttl expires        |  |         | capture
      |                             |                             |  |         | starts again
      |                             |  capture stops              |  | free < floor
      |                             |   (ring kept, sealed)       |  v         | (hysteresis
      |                             |                        +----------+      |  +256 MiB)
      |                             +------------------------| Frozen   |   +----------+
      |                                                       +----------+   | Degraded |
      |                                                                      +----------+
      +------------- disable from any state (keep_on_disable=false deletes run dir)
```

| State | Packets accepted | Hotkey clips | Notes |
|---|---|---|---|
| Off | none | "buffer is off" toast | no sinks, encoder may still run for session |
| Idle | none | "no game" toast | enabled, no capture running |
| Running | yes (gated to IDR) | yes | normal |
| Frozen | none | yes, until `frozen_ttl` or next capture start | ring sealed, kept for last-minute clips after game exit |
| Degraded(LowDisk) | none (sink detached) | yes, from existing ring | auto-resumes when free > floor + 256 MiB |
| Degraded(WriteError) | none | yes, from existing ring | needs user action or restart; toast shows path |

Transitions on capture start: a new `RunId` is created; any Frozen ring from a previous game is cleared (`Ring::clear_frozen`).

### 5.2 Segmenter (BufferSink::on_packet)

```
on_packet(p):
  if !gate_open:
      if p.kind == Video && p.keyframe:
          gate_open = true
          open_segment(seq = next, start = p.pts)
      else:
          return Ok   // drop until first IDR, no error
  
  if p.kind == Audio:
      audio_hold.push(p); flush_audio_hold_if_stale()   // hold <= 200 ms
      return Ok

  // video packet
  if p.keyframe:
      age = p.pts - current.start
      if age >= segment_us * 0.9:
          seal_current_and_open_new(start = p.pts)      // cut on IDR
          split audio_hold by pts: pts < p.pts -> old segment, others -> new
  elif (p.pts - last_pts) > 1.5 * segment_us:
      discontinuity(): seal, epoch += 1, open_segment(gap = true)   // see E6

  writer.write_packet(p); flush audio_hold to writer where pts <= p.pts
  last_pts = p.pts
  if p.pts - last_commit >= fragment_us: writer.commit_fragment()
```

IDR cadence is the encoder's job: keyint = `segment_ms` at the target fps. If no IDR arrives within `1.25 * segment_ms`, the sink sets the shared `ForceIdr` flag and the encoder emits an IDR on its next frame (encode doc).

### 5.3 Clip snapping

```
snapshot_for_clip(want, pre_pad, head_at_request):
  target = head_at_request - want - pre_pad
  epoch_segs = segments with epoch == current_epoch (after last gap)
  if epoch_segs empty: return NoCapture-or-Empty

  // find snap keyframe: greatest IDR pts <= target inside epoch
  K = max { k in seg.keyframes | seg in epoch_segs, k <= target }
  truncated = false
  if K is None:
      K = min { k in seg.keyframes | seg in epoch_segs }     // earliest available IDR
      if K is None: return NoKeyframe
      truncated = (K > target)                              // buffer shorter than want
  // the clip must not end before it starts
  end = head_at_request                                      // snapshot head
  leases = [seg | seg.end > K && seg is in epoch_segs]      // acquire leases under read lock
  return ClipPlan { origin: K, end, truncated, leases, ... }
```

Snap rule, stated plainly:
- Start is always a keyframe at or before the target. Clip length is `want + [0, gop_length)` on success, so the output is always at least as long as requested, and at most one GOP (2 s by default) longer.
- Clip end is the head snapshot, not a keyframe. Tail frames of the last open GOP are allowed because the file ends there.
- Pre-pad moves the target earlier before snapping. Post-roll delays the snapshot (Section 5.5), so end = head at request + postroll.

### 5.4 Clip writing (ClipWriter::write)

```
write(plan, out):
  check cancel flag at every packet
  streams = plan.streams (taken from first lease; all leases share track_sig by construction)
  container = if audio is Opus { mkv } else { mp4 with +faststart }
  octx = open_output(container, out + ".partial")
  for each stream s: add_stream(copy codecpar, timebase = s.timebase)

  for lease in plan.leases:
      ictx = open_input(BoundedReader(lease.source, lease.bound_bytes))   // bounded AVIO
      for pkt in ictx.packets():
          if pkt.pts_us < plan.origin_us: continue            // audio before origin
          if pkt.pts_us >= plan.end_us: break_out_of_segment_loop_and_plan
          if !started_on_key[pkt.stream]: 
              if pkt.stream is video and !pkt.keyframe: continue
              mark started
          pkt.pts = rescale(pkt.pts_us - origin, timebase)
          pkt.dts = rescale(pkt.dts_us - origin, timebase)
          if pkt.dts <= last_dts[stream]: continue            // monotonic guard
          last_dts[stream] = pkt.dts
          octx.interleaved_write(pkt)
  octx.write_trailer(); flush; FlushFileBuffers(out.partial)
  rename(out.partial, out)                                    // atomic within the same volume
```

Notes:
- Remux is lossless: packets are copied byte-for-byte; no decoder is loaded.
- `BoundedReader` returns EOF at `bound_bytes`, so an open `.part` segment is read only up to the committed prefix.
- Timestamps are on the pipeline clock, so no per-segment offset arithmetic is needed beyond `origin`.
- Output `+faststart` costs one extra copy of the moov (small) but the mdat is moved; for a 5-minute clip this adds about 100 ms on NVMe. Accepted.

### 5.5 Hotkey press to clip (async path)

```
on HotkeyEvent(e) in clipcheckd dispatcher:
  if e.action == ToggleBuffer: toggle_buffer(); return
  if e.action == SaveRecovered: submit recovered job; return

  // action == ClipLast { seconds }
  if !buffer.enabled: feedback.info("Replay buffer is off"); return
  if state == Idle: feedback.info("No game is being recorded"); return
  if debounce[e.binding_id] within 1500 ms: return
  debounce[e.binding_id] = now
  req = ClipRequest { request_id: new_ulid(), seconds, requested_at_us: ring.head_us(), ... }
  match clip_worker.submit(req):
      Ok  -> return                         // < 1 ms on the dispatcher thread
      Err(QueueFull) -> feedback.info("Clip queue busy...")

ClipWorker thread:
  if postroll > 0: wait until ring.head_us() >= requested_at_us + postroll  (poll 50 ms, timeout postroll + 2 s)
  plan = ring.snapshot_for_clip(...)                 // read lock held for microseconds
  est = plan.est_bytes; free = GetDiskFreeSpaceEx(out_dir)
  if free < est * 1.02 + clip_min_free: return Failed(NoSpace)
  name = render(name_template, game, now_local, duration_label)  // unique: append " (2)" on collision
  ClipWriter::write(plan, out)                       // leases released when plan drops
  emit Saved(outcome)
```

Flow on the clip thread is single-threaded so two clips never contend for the same reads. Clip thread priority is `THREAD_MODE_BACKGROUND_BEGIN`, which lowers CPU and I/O priority, so a 5-minute clip never slows the encoder's disk writes.

### 5.6 Eviction and leases

```
evict_if_needed(free_bytes):
  loop:
      span   = newest.end_us - oldest.start_us         // across current epoch chain
      over_t = span > max_duration + segment_duration   // slack for snapping
      over_b = total_bytes > max_disk_bytes
      over_d = free_bytes < min_free_disk_bytes
      if !(over_t || over_b || over_d): break
      victim = oldest sealed segment                     // never the open segment
      if none: break (report DiskLow)
      remove victim from deque; total_bytes -= victim.bytes
      victim.evicted = true
      if victim.leases == 0: delete backing now
      else: deferred to SegmentLease::drop
```

Lease rule: a lease holds an `Arc<SegmentEntry>`, so the entry stays alive even after eviction; the file deletion is deferred until the last lease drops. The ring may therefore briefly exceed its cap by the size of in-flight clip data. This is bounded by `clip queue depth x max clip size`, and that is accepted.

Ring length slack: `max_duration + segment_duration`. The extra segment guarantees a full `max_minutes` clip can still snap back to a keyframe.

RAM mode: eviction drops the `Arc<RamBlob>`; no file delete. Leases keep blobs alive.

Effective max under RAM cap:
```
effective_max_s = min(max_minutes * 60, ram_max_bytes * 8 / est_bitrate_bps)
```
The UI shows the effective value when it is shorter than the configured one.

### 5.7 Disk guard

Checked every 5 s on the buffer volume and on the clip output volume (`GetDiskFreeSpaceExW`, cached).

| Condition | Action |
|---|---|
| free < `min_free_disk_bytes` | Evict faster: lower the effective ring cap to 50% of current, notify once (`DiskLow`) |
| free < `min_free_disk_bytes / 4` | Detach buffer sink, state Degraded(LowDisk), notify |
| free > `min_free_disk_bytes + 256 MiB` for 30 s | Re-attach sink on next IDR, state Running |
| clip output: `free < est + clip_min_free` | Fail that clip with `NoSpace`; never evicts ring to make room |
| write error (ENOSPC, EIO, path gone) | Detach sink, Degraded(WriteError), notify with path; no retry loop |

The session writer has its own disk guard (session doc). Both share a `DiskProbe` cache so they do not each call `GetDiskFreeSpaceEx` on every packet.

### 5.8 Hotkey registration (keyboard hook, no swallowing)

Decision D53. The hotkey backend does not call `RegisterHotKey`. `RegisterHotKey` swallows the chord system-wide: while a clip chord such as Pause, F9 or Alt+F7 was bound, its normal function was lost in every other app and in the game, and the UI's key-capture box never received that key. The backend now observes keys with a low-level keyboard hook and never consumes them.

Thread `hotkey-msg`, one per `HotkeyService`:

```
start():
   hwnd = CreateWindowExW(..., HWND_MESSAGE)                 // message-only window
   CTX  = { slots from registry, HookState, tx, suspended atomic }
   hook = SetWindowsHookExW(WH_KEYBOARD_LL, keyboard_proc, module, 0)
          failure -> StartError::Hook(code), thread exits, start() returns Err
   ready
   loop GetMessageW():
       WM_TIMER(GESTURE_TIMER)  -> HookState::tick(now); dispatch fired chords
       WM_APP_APPLY             -> replace slots and HookState (pending holds dropped); reply with empty failures
       WM_QUIT                  -> UnhookWindowsHookEx, DestroyWindow, exit

keyboard_proc(code, wParam, lParam):                         // every key event on the desktop
   if code >= 0 and message is WM_KEYDOWN/WM_SYSKEYDOWN/WM_KEYUP/WM_SYSKEYUP:
       HookState::on_key(vkCode, down, flags, now)           // bit operations, a set lookup, a short scan
       send each fired chord's action on the mpsc channel
       SetTimer(GESTURE_TIMER, 10 ms) while a long-press hold is pending
   return CallNextHookEx(...)                                // ALWAYS: the key is never swallowed
```

The callback budget is well under the 200 ms that Windows allows before it drops a slow hook. It does no I/O, no allocation beyond the channel send, and no `GetAsyncKeyState` polling.

Rules in `HookState` (pure, unit-tested in `hook_state.rs`):

- **Modifiers** come from the events themselves: Ctrl, Shift, Alt and Win are tracked per left and right key. Generic codes (`VK_CONTROL` and friends) count for both sides. On every non-modifier event, Alt is resynchronised from `LLKHF_ALTDOWN`, which covers a modifier that was already held when the hook started.
- **Exact match.** A chord matches only when its key is pressed and its modifier set equals the held set. A bare key does not fire while any modifier is held. `Alt+F9` does not fire for `F9`.
- **Auto-repeat** is not a new press. A key already in the held set produces nothing. This replaces `MOD_NOREPEAT`.
- **Key-up without a prior key-down** (the key was held before the hook started, or the press was ignored) does nothing.
- **Tap versus long** on a shared chord (one tap clip, one long clip): tap-only chords fire on key-down. A chord with a long binding fires the tap on release when the hold is under 400 ms, and fires the long action once at 400 ms while still held. The release after a long press fires nothing. The decision logic is the existing pure `GestureTracker`, fed with real key-down and key-up events.
- **Modifier released before the key** does not cancel a hold; the key's own release decides it.
- **Injected events** (`LLKHF_INJECTED`, for example from `SendInput`) are accepted. Tests and remote tools trigger chords the same way a keyboard does.
- **Suspension.** `HotkeyService::set_suspended(true)` (the UI is capturing a new key) stops all matching and drops pending holds. Modifier tracking continues, so the state is correct when capture ends. The suspension expires 60 s after the last request, so a lost resume cannot disable hotkeys.

Registration and conflicts. Chords are not registered with the OS, so the apply reply is always an empty failure list and `TakenByAnotherApp` is never produced. An overlap with another application's own hotkey is therefore not reported, and the chord fires for ClipCheck while the other app also sees the key. Two ClipCheck bindings on one chord are still rejected at validation (CFG-H002). A hook install failure reports `StartError::Hook(code)`, and the daemon shows "hotkeys not started" (see 2.2).

Anti-cheat. A global low-level keyboard hook is a documented Windows mechanism and it is passive here, but some anti-cheat systems flag any installed global input hook, even one that only observes. The previous default path installed no hook. This risk is not verified against a specific anti-cheat, and it is an open question (Section 12).

Verification status. The pure state machine has 26 unit tests (`cargo test -p clipcheck-hotkeys --lib hook_state`). The real key behaviour (no swallowing in a game, `SendInput` delivery, the hook surviving a long session) has not been tested on a live desktop. `crates/hotkeys/tests/win_tests.rs` still asserts the old `RegisterHotKey` conflict behaviour, and it injects real keystrokes, so it must not be run on the owner's desktop.

### 5.9 Conflict detection summary

| Check | When | Result |
|---|---|---|
| Chord parse | Settings save and apply | `BindingError::NoKeyName` |
| Bare key | Settings save and apply | `BindingError::BareKey` (F13 to F24 exempt) |
| Duration range | Settings save and apply | `DurationExceedsBuffer` / `DurationTooShort` |
| Duplicate in ClipCheck | Apply | `DuplicateInConfig`, second binding inactive |
| Windows-reserved | Apply | `ReservedByWindows` (table below) |
| Another app owns RegisterHotKey | Apply | `TakenByAnotherApp`, no hook fallback unless `allow_hook_on_conflict` |
| Another app uses the same key via hook only | not detectable | Both fire; documented |

Reserved table (not registerable or user-hostile, checked before RegisterHotKey):

| Chord | Reason |
|---|---|
| Ctrl+Alt+Delete | Secure attention sequence, cannot be registered |
| Win+L | Lock workstation |
| Alt+F4, Alt+Tab, Ctrl+Shift+Esc | Core shell |
| Win+G, Win+Alt+R, Win+Alt+G | Xbox Game Bar / Game DVR |
| Win+Shift+S, PrintScreen, Win+PrintScreen | Screenshot |
| Ctrl+Alt+F12 | Intel graphics control hotkey (warn, not block) |

`consume_keys = true` swallows the chord from the foreground app. It is off by default because a swallowed key that is also a game binding breaks the game. It is only available for `method = "hook"` bindings.

### 5.10 Recovery procedure (service start)

```
recover_leftovers(buffer_root, current_run):
  lock = LockFileEx(buffer_root/lock, EXCLUSIVE, NB)     // another live service owns it -> skip
  for dir in buffer_root/run-*  where dir != current_run:
      ring_jsonl = read(dir/ring.jsonl) (ignore malformed trailing line)
      for each sealed record: verify file exists && size >= committed_bytes  -> keep
      for each *.mp4.part and each *.mp4 not in journal:
          salvage_part(path):
              parse top-level boxes: ftyp, moov (must be complete)
              walk moof+mdat pairs; stop at first truncated box
              if zero complete fragments -> delete
              keyframes from trun sample flags (non-sync bit)
              end_us from last tfdt + sample durations
              copy complete prefix to recovered/<run>/<same name without .part>
      write recovered/<run>/manifest.json
  delete dir after move (keep recovered)
  purge recovered/* older than recovered_ttl
  emit BufferRecoveredAvailable { count, bytes, span } if any recovered
```

Salvage is a copy, not an in-place truncate, so the original is preserved until the copy is fsynced. Salvage never re-encodes.

### 5.11 Run directory lifecycle

```
enable / capture start:  run = new RunId; mkdir run-<RunId>; write nothing yet
segment sealed:          append JSON line to ring.jsonl; fsync the file (not every packet)
capture stop:            seal open segment, state Frozen, start frozen timer
frozen timeout / next capture start:  clear_frozen -> delete run dir async
disable (keep_on_disable = false):    stop sink, delete run dir async
disable (keep_on_disable = true):     stop sink, keep run dir until ttl expires (state Off, ring hidden)
```

Journal line (one per sealed segment):
```json
{"seq":12,"epoch":1,"file":"00000012-0000000092160000.mp4","start_us":92160000,"end_us":94160000,"bytes":5148213,"keys":[92160000]}
```

---

## 6. Config keys owned by this feature

Settings doc consumes this table. All keys live under `%APPDATA%\ClipCheck\config.toml`. Hot-reload: `buffer.*` and `hotkeys.*` apply on reload without restarting capture, except `buffer.storage` and `buffer.dir`, which apply at the next run.

### 6.1 `[buffer]`

| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `buffer.enabled` | bool | `true` | | Independent of session recording |
| `buffer.max_minutes` | u8 | `5` | 1..=60 | Retained window; ring keeps +1 segment slack |
| `buffer.segment_ms` | u32 | `2000` | 1000..=4000 | Target GOP / segment length |
| `buffer.fragment_ms` | u32 | `500` | 100..=1000 | Clip-latency granularity for open segment |
| `buffer.storage` | enum | `"disk"` | `"disk"`, `"ram"` | RAM is for short buffers |
| `buffer.dir` | path | `%LOCALAPPDATA%\ClipCheck\buffer` | any writable local path | Fast drive recommended |
| `buffer.ram_max_mb` | u32 | `512` | 64..=4096 | Used only when `storage = "ram"` |
| `buffer.max_disk_mb` | u32 | `0` (auto) | 0 or 256..=262144 | 0 = auto (Section 6.4) |
| `buffer.min_free_disk_mb` | u32 | `2048` | 512..=102400 | Ring floor |
| `buffer.keep_on_disable` | bool | `false` | | |
| `buffer.frozen_ttl_s` | u32 | `300` | 0..=3600 | Clippable after capture stops |
| `buffer.pre_pad_ms` | u32 | `0` | 0..=5000 | Extra footage before the requested window |
| `buffer.postroll_ms` | u32 | `0` | 0..=10000 | Waits before snapshot |
| `buffer.keep_recovered` | bool | `true` | | Keep salvaged footage for user to save |
| `buffer.recovered_ttl_h` | u32 | `24` | 1..=168 | |
| `buffer.clip_dir` | path | empty (= recordings root / Game) | | |
| `buffer.clip_min_free_mb` | u32 | `512` | 64..=102400 | Clip output pre-flight margin |
| `buffer.clip_name` | string | `"{game} {yyyy-MM-dd HH-mm-ss} clip-{dur}"` | tokens: `{game}`, `{yyyy}`, `{MM}`, `{dd}`, `{HH}`, `{mm}`, `{ss}`, `{dur}` | `{dur}` = `15s`, `2m`, `1m30s` |

### 6.2 `[[buffer.clip_hotkeys]]` (array of tables)

| Key | Type | Default (first entry shown) | Range / values |
|---|---|---|---|
| `id` | string | `"clip-30s"` | unique, `[a-z0-9-]{1,32}` |
| `keys` | string | `"Ctrl+Shift+F9"` | chord syntax, Section 4.9 |
| `seconds` | u32 | `30` | 5..=`buffer.max_minutes*60` |
| `method` | enum | `"auto"` | `"auto"`, `"register"`, `"hook"` |
| `enabled` | bool | `true` | |
| `label` | string | empty | optional, shown in toast; max 40 chars |

Shipped defaults: `clip-15s` Ctrl+Shift+F8 (disabled), `clip-30s` Ctrl+Shift+F9, `clip-2m` Ctrl+Shift+F10 (120 s), `clip-5m` Ctrl+Shift+F11 (300 s).

### 6.3 `[hotkeys]`

| Key | Type | Default | Range | Notes |
|---|---|---|---|---|
| `hotkeys.hook_fallback` | bool | `true` | | Installs LL hook for `auto` bindings |
| `hotkeys.allow_hook_on_conflict` | bool | `false` | | Use hook even when another app owns the chord |
| `hotkeys.dedupe_ms` | u32 | `300` | 50..=1000 | Window for merging RegisterHotKey and hook events |
| `hotkeys.consume_keys` | bool | `false` | | Only for `method = "hook"` bindings |
| `hotkeys.toggle_buffer` | chord string or empty | empty | chord syntax | Optional toggle hotkey; action `ToggleBuffer` |
| `hotkeys.save_recovered` | chord string or empty | empty | chord syntax | Saves the most recent recovered set |

### 6.4 `[feedback]`

| Key | Type | Default | Range | Notes |
|---|---|---|---|---|
| `feedback.sound` | bool | `true` | | Chime on saved, tone on failed |
| `feedback.sound_file` | path | empty (bundled) | existing `.wav` | |
| `feedback.toast` | bool | `true` | | |
| `feedback.toast_seconds` | u32 | `2` | 1..=10 | |
| `feedback.toast_corner` | enum | `"br"` | `tl`, `tr`, `bl`, `br` | Monitor of the captured game window |

### 6.5 Auto disk cap formula

```
est_bps        = encode.max_bitrate_bps + audio_tracks * 320_000
auto_max_bytes = max_minutes * 60 * est_bps / 8 * 1.15 + 2 * segment_bytes_estimate
```

Example: 20 Mbps video, 3 audio tracks at 160 kbps: est 20.48 Mbps, so a 5-minute buffer caps near 0.9 GB and a 60-minute buffer near 9.1 GB.

---

## 7. IPC messages and events

Transport is the named pipe `\\.\pipe\clipcheck-ipc` with length-prefixed JSON (brief). Field names are `snake_case`. Ids are ULID strings. Times are unix ms unless named `_us`.

### 7.1 Commands (UI to service)

| Command | Payload | Reply |
|---|---|---|
| `buffer.set_enabled` | `{ "enabled": bool }` | `Ack` |
| `buffer.clip` | `{ "seconds": u32 }` | `ClipAccepted { request_id }` or `Error { code }` |
| `buffer.list_recovered` | `{}` | `RecoveredList { sets: [...] }` |
| `buffer.save_recovered` | `{ "recovery_id": ulid, "name": string? }` | `ClipAccepted` |
| `buffer.discard_recovered` | `{ "recovery_id": ulid }` | `Ack` |
| `hotkeys.validate` | `{ "keys": "Ctrl+Shift+F9", "seconds": 30 }` | `HotkeyValidation { ok, error?, conflict? }` |
| `hotkeys.status` | `{}` | `HotkeysStatus { bindings: [{ id, keys, status, via }] }` |

`hotkeys.validate` is used by the settings UI while the user types, so it must be pure (no RegisterHotKey call); conflict detection against other apps is reported as `unknown` until apply.

### 7.2 Events (service to UI)

| Event | Payload | When |
|---|---|---|
| `buffer.state` | `{ "state": "off"\|"idle"\|"running"\|"frozen"\|"degraded", "reason": string?, "buffered_s": u32, "ring_bytes": u64, "effective_max_s": u32 }` | Every state change and every 5 s while running |
| `clip.started` | `{ "request_id", "seconds", "source": "hotkey"\|"ui"\|"recovered" }` | Worker starts a job |
| `clip.saved` | `{ "request_id", "clip_id": ulid, "path", "duration_ms", "truncated": bool, "bytes", "took_ms" }` | Output renamed into place |
| `clip.failed` | `{ "request_id", "code": "no_space"\|"no_keyframe"\|"empty"\|"queue_full"\|"io"\|"mux", "message" }` | Any failure |
| `hotkeys.applied` | `{ "statuses": [{ "id", "status", "via", "detail" }] }` | After each apply |
| `hotkeys.conflict` | `{ "id", "reason", "detail" }` | Per conflicting binding at apply |
| `buffer.recovered_available` | `{ "sets": n, "bytes", "span_ms" }` | Startup, after recovery |
| `disk.low` | `{ "volume", "free_bytes", "action" }` | Guard fires |

`clip_id` is allocated by `clipcheckd` before the event; the library inserts the row when `clip.saved` is emitted (kind `clip-<dur>`), so the gallery never shows a clip that the library does not know about.

### 7.3 Example exchange

```json
-> {"type":"buffer.clip","seconds":30}
<- {"type":"clip.accepted","request_id":"01JFXK3Q7T8W2N5V9R4B6C0D1E"}
<- {"type":"clip.started","request_id":"01JFXK3Q7T8W2N5V9R4B6C0D1E","seconds":30,"source":"ui"}
<- {"type":"clip.saved","request_id":"01JFXK3Q7T8W2N5V9R4B6C0D1E","clip_id":"01JFXK3R1ZA0M4P2Q8S6T9U3V5","path":"C:\\Users\\mitch\\Videos\\ClipCheck\\Game\\Game 2026-10-09 21-14-07 clip-30s.mp4","duration_ms":31840,"truncated":false,"bytes":6109233,"took_ms":412}
```

---

## 8. Edge cases & failure modes

| # | Case | Behavior |
|---|---|---|
| E1 | Clip requested longer than buffered footage (just started, or RAM cap) | Clip saved from earliest IDR; `truncated = true`; toast states actual length. Never an error. |
| E2 | No IDR in the ring yet (sink just attached, encoder slow to IDR) | `clip.failed(no_keyframe)`; toast "Buffer warming up". Gate guarantees first segment starts on IDR. |
| E3 | Eviction races a clip read | Lease holds the `Arc`; file delete deferred to last lease drop. Clip completes from the original data. |
| E4 | Buffer sink falls behind (channel full) | `try_send` fails, packets for the buffer are dropped and a discontinuity is recorded (epoch + gap marker). Clips never cross the gap. Session sink is unaffected. Logged once per minute. |
| E5 | Encoder reconfigured mid-buffer (resolution, fps, audio layout, device change) | `track_sig` changes; new epoch. Clip snapping only uses the current epoch. Old-epoch segments stay until evicted, unused by clips. |
| E6 | Timeline gap (sleep/resume, encoder stall >1.5 segments) | Discontinuity marker; epoch+1; `gap_epoch_end` set; clips crossing the gap start after it and are `truncated`. |
| E7 | Service crash mid-segment | `.part` files salvaged to `recovered\` on next start (Section 5.10). Sealed segments in `ring.jsonl` are kept. RAM ring is lost (documented). |
| E8 | Disk full during a segment write (ENOSPC) | Sink detaches, state `degraded(write_error)`, toast with path. Session writer's own failure handling applies independently. |
| E9 | Buffer volume removed or unplugged | Same as E8; sink detaches; run dir considered gone; recovery later finds nothing. |
| E10 | Output folder not writable, path too long, or network share disconnect | `clip.failed(io)`; `.partial` file removed; toast "Could not write clip: <reason>". |
| E11 | Output name collision | Append ` (2)`, ` (3)`, ... before the extension. Never overwrite. |
| E12 | Hotkey pressed during an active clip of the same binding | Debounced (1.5 s). Pressing a different binding queues a second clip (queue depth 4). Queue full: toast "busy". |
| E13 | Game exits while a clip job is running | Ring is sealed and goes to Frozen; in-flight clip finishes from leases. |
| E14 | Keyboard layout / OEM keys | Chords stored by virtual-key code. Display uses `MapVirtualKeyW`. A binding saved on US layout may show a different symbol on other layouts; documented. |
| E15 | Hook removed by the OS (timeout) or after sleep | Hook callback is O(1). On `PBT_APMRESUMEAUTOMATIC` the hook is reinstalled. A silent removal is caught by the RegisterHotKey path for `auto` bindings. |
| E16 | Feedback sound device unavailable, or sound mixer busy | Sound call fails silently; logged once. Toast still shown. |
| E17 | Our own sound effect enters the "Mix" audio track | Audio doc must exclude the `clipcheckd` process from the system mix (EXCLUDE loopback on own PID). Test in Section 10. |
| E18 | Game uses exclusive fullscreen; toast cannot be seen | Sound plays; toast is best-effort. Documented limitation. |
| E19 | Recovered set requested while a new run is writing | Recovered sets live in a separate directory tree and are read-only to the ring. No contention. |
| E20 | `duration` in a hotkey lowered below the buffer after a config edit | Binding set to `Invalid(DurationExceedsBuffer)`, one toast, no silent clamp. |
| E21 | Two binding ids with the same chord | Second one inactive with `DuplicateInConfig`. |
| E22 | Hotkey fires while buffer is Off or Idle | Toast explains; no state change. |
| E23 | Very large clip (60-minute buffer, 5-minute request is fine; 60-minute request) | Remux is streaming; memory bounded by one packet plus demuxer buffers (~4 MB). Disk pre-flight rejects if output would not fit. |

---

## 9. Performance & resource budget

### 9.1 Targets

| Resource | Budget | Basis |
|---|---|---|
| Buffer writes (steady) | ~2.6 MB/s at 20 Mbps video + 3 audio tracks | encode bitrate, no re-encode |
| Ring memory, disk mode | < 4 MB (open fragment + mux state + index) | fragment_ms = 500 |
| Ring memory, RAM mode | `ram_max_mb` (default 512 MB) | hard cap, effective length shrinks instead |
| Ring index | <= 3600 entries at 60 min / 1 s segments | trivial |
| CPU, buffer sink | < 0.5 % of one core while recording | mux only, no encode |
| CPU, clip remux | < 2 % of one core for a 5-min clip, background priority | packet copy |
| Clip latency: 30 s clip, NVMe | <= 300 ms to file rename | |
| Clip latency: 5 min clip, NVMe | <= 1.5 s | ~75 MB copy |
| Clip latency: 5 min clip, SATA SSD | <= 3 s | |
| Hotkey press to dispatcher | < 5 ms (RegisterHotKey), < 1 ms (hook callback) | |
| Hotkey press to toast visible | <= 150 ms (first clip toast can be +frame) | Feedback is independent of save duration: an "accepted" toast shows immediately, then "Saved" |
| LL hook callback | < 50 microseconds, no allocation, no logging | LowLevelHooksTimeout default 300 ms |
| Idle (no game, buffer on) | 0 % CPU, no extra wakeups beyond the 5 s disk probe | |
| Service RSS contribution | < 2 MB (disk mode) | budget within the 15 MB service idle target |

Feedback flow detail: on press the toast shows "Saving 30 s clip..." immediately, then "Saved" on completion. The toast is therefore never a source of latency.

### 9.2 I/O and isolation rules
- The clip thread uses `THREAD_MODE_BACKGROUND_BEGIN`.
- The buffer sink thread is separate from the session writer thread, so a slow clip read never delays the session.
- Clip reads use `FILE_FLAG_SEQUENTIAL_SCAN`.
- Segment writes use `FILE_FLAG_WRITE_THROUGH` off; durability is provided by `commit_fragment` plus journal fsync on seal, because a crash loses at most one fragment (500 ms default), which is inside the brief's 2 s bound.

### 9.3 Disk footprint
| Config | Approx. ring size |
|---|---|
| 5 min, 20 Mbps + 3 audio tracks | ~0.8 GB |
| 2 min, 20 Mbps | ~0.3 GB |
| 60 min, 20 Mbps | ~9.2 GB |

---

## 10. Test plan

### 10.1 Unit tests (`buffer`, no Windows I/O unless stated)

| ID | Test | Assertion |
|---|---|---|
| U-B1 | Snap: target exactly on IDR | K == target |
| U-B2 | Snap: target between IDRs | K == previous IDR; `truncated == false` |
| U-B3 | Snap: target before all IDRs, buffer longer than request | K = earliest IDR; `truncated == true` |
| U-B4 | Snap: no IDR at all | `NoKeyframe` |
| U-B5 | Snap crosses gap | start >= gap end; `truncated == true` |
| U-B6 | Pre-pad moves target earlier before snap | expected K |
| U-B7 | Eviction keeps `max + segment` slack | span within bounds after evict |
| U-B8 | Lease blocks file delete until drop | file exists until lease dropped (temp dir) |
| U-B9 | Eviction with leases: index shrinks immediately | segment count decreases; file count does not |
| U-B10 | Effective max under RAM cap | `min(max, cap*8/bps)` |
| U-B11 | Segment name parse/format round trip | equal |
| U-B12 | Journal parse with truncated last line | previous lines kept |
| U-B13 | Salvage: truncate `.part` at every byte offset 0..N of a 3-fragment fixture | never panics; output decodes ffprobe with complete fragments only |
| U-B14 | Dedupe window across two sources | exactly one event |
| U-B15 | Name template tokens and collision suffix | `(2)` appended, original kept |
| U-B16 | `format_duration_label` | 15 -> "15s", 120 -> "2m", 90 -> "1m30s", 300 -> "5m" |
| U-B17 | Debounce 1500 ms | second press ignored; press at 1600 ms accepted |
| U-B18 | Disk guard hysteresis | Degraded at floor/4, resumes only above floor+256 MiB |

### 10.2 Hotkey unit tests (`hotkeys`)

| ID | Test | Assertion |
|---|---|---|
| U-H1 | Chord parse: `Ctrl+Shift+F9`, `ctrl + shift + f9`, `Win+Alt+K` | Expected mods and vk |
| U-H2 | Bare key rejected, F13 to F24 accepted bare | per table |
| U-H3 | Reserved chords flagged | Section 5.9 table |
| U-H4 | `validate_clip_seconds(300, 5)` ok; `(301, 5)` error; `(4, 5)` too short | |
| U-H5 | Duplicate in config | second binding `DuplicateInConfig` |
| U-H6 | Hook state machine: auto-repeat ignored, modifiers tracked, injected events accepted | counts |

### 10.3 Integration tests (`buffer`, real libav, synthetic stream)

Input: a deterministic synthetic encoder (test pattern video at 60 fps, IDR every 2 s, closed GOP, 3 audio sine tracks at 48 kHz) feeding `BufferSink` directly.

| ID | Test | Assertion |
|---|---|---|
| I-B1 | Run 3 minutes, request 30 s, 2 m, 5 m | Each clip duration within `[want, want + 2.1 s]`, `truncated` only where expected |
| I-B2 | Clip first packet is a keyframe | ffprobe `key_frame=1` on first video packet |
| I-B3 | Timestamps monotonic per stream; no duplicate dts | ffprobe scan |
| I-B4 | A/V sync: audio start within one audio frame of video start | |
| I-B5 | Clip contents match source frame numbers | encode frame index as overlay, decode, compare |
| I-B6 | Clip while open segment is writing | clip end within `fragment_ms` of head; decodes |
| I-B7 | Eviction while clip reading (inject slow reader) | clip succeeds; disk file removed after clip |
| I-B8 | Epoch change mid-run (track layout change) | clips never mix epochs |
| I-B9 | Gap injection (pause sink for 4 s) | clip crossing gap is truncated and starts after it |
| I-B10 | Stream copy is lossless | output packet payload bytes equal source packet bytes (hash compare) |
| I-B11 | Opus audio produces MKV, AAC produces MP4 | container check |

### 10.4 Concurrency and session coexistence

| ID | Test | Assertion |
|---|---|---|
| C-1 | Buffer + session both on, 10 min, clip every 20 s | Session file has zero dropped packets (session counter); session output duration equals wall time within 100 ms |
| C-2 | Clip of 5 min while session writes at full rate | Encoder packet publish latency p99 < 2 ms; no sink overruns on session |
| C-3 | Buffer off, session on: no buffer files, no buffer thread CPU | process file handles and disk writes verified |
| C-4 | Buffer on, session off: segments grow; no session file | |
| C-5 | Toggle buffer off mid-game, then on | Session unaffected; ring recreated with new run |
| C-6 | Fanout backpressure: stall buffer sink for 3 s | encoder never blocks; buffer records gap; session has no gap |
| C-7 | Clip queue: 6 rapid hotkey presses across two bindings | at most 4 queued, rest toasted "busy"; debounced duplicates counted |

### 10.5 Crash and recovery

| ID | Test | Assertion |
|---|---|---|
| R-1 | `kill -9` (TerminateProcess) during recording at 20 random times | Next start: salvaged segments decode; sealed segments kept; no files left in run dir |
| R-2 | Two service instances | second instance skips recovery (lock held) |
| R-3 | Recovered set saved as clip | clip duration matches salvaged span within 1 GOP |
| R-4 | recovered_ttl expiry | sets older than TTL deleted on start |

### 10.6 Hotkey manual and automated matrix

Automated: inject keys with `SendInput` (sets LLKHF_INJECTED) on a headless test host; verify `HotkeyEvent` count and `via`.

Manual matrix (must pass before release):

| Target | RegisterHotKey | LL hook | Notes |
|---|---|---|---|
| Notepad (elevated and not) | yes | yes | baseline |
| Chrome, Discord | yes | yes | |
| Borderless fullscreen game (3 titles) | yes | yes | toast visible, clip saved |
| Exclusive fullscreen DirectInput test app | record result | yes | expected: hook fires |
| Game with Raw Input exclusive mode | record result | yes | |
| Intel Ctrl+Alt+F12 conflict | conflict reported | n/a | warning shown |
| Sleep/resume | hook reinstalled | yes | |
| Non-US keyboard layout (DE, FR) | yes | yes | label renders |

### 10.7 Feedback and audio

| ID | Test | Assertion |
|---|---|---|
| F-1 | Sound plays on saved, error tone on failed | audible; no sound on Off/Idle hotkey |
| F-2 | Toast appears on borderless game, not inside clip | visual; decode clip and check no toast pixels (pixel diff on corner) |
| F-3 | Own sound not in Mix track | record a clip with chime, analyze track 1 for chime signature absent (requires audio doc EXCLUDE on own PID) |
| F-4 | Toast over exclusive fullscreen | best-effort; sound-only fallback verified |

### 10.8 Soak and budget

| ID | Test | Assertion |
|---|---|---|
| S-1 | 4 h soak, buffer on, 5 min ring, 30 clips/hour | RSS flat (+/- 2 MB), handle count flat, disk usage stable at cap |
| S-2 | Disk full: ImDisk 1 GB volume as buffer dir | Guards fire; no crash; recovery after space freed |
| S-3 | RAM ring 512 MB, 5 min at 20 Mbps | ring capped; effective length reported; no OOM |
| S-4 | Idle, no game, buffer on | 0 % CPU over 60 s; wakeups < 1/s |

---

## 11. Implementation task breakdown

Ordered. Each task is roughly 0.5 to 2 days. Tasks T1 to T6 are the buffer crate (testable without a game). T7 to T10 are hotkeys and feedback. T11 to T12 integrate with the service.

| # | Task | Depends on | Deliverable | Est. |
|---|---|---|---|---|
| T1 | `core` types: `EncodedPacket`, `TimeUs`, `PacketSink`, ULID newtypes | none | compiled types and unit tests | 0.5 d |
| T2 | fMP4 `SegmentWriter` over libav (custom AVIO, `use_editlist=0`, absolute tfdt), `commit_fragment`, `seal` | T1, ffi wrapper | writes playable segments; I-B10 scaffolding | 2 d |
| T3 | `BufferSink`: gate, IDR cuts, audio hold, gap detection, journal | T2 | segment rotation tests | 1.5 d |
| T4 | `Ring`: index, epochs, leases, eviction, disk guard, RAM blob storage | T3 | U-B1..U-B10, U-B18 | 2 d |
| T5 | `ClipWriter` (bounded reader, stream copy, monotonic guard, mp4/mkv select) + `snapshot_for_clip` | T4 | I-B1..I-B11 | 2 d |
| T6 | `ClipWorker` thread, queue, postroll, name template, `.partial` rename, background priority | T5 | I-B6, C-7 | 1 d |
| T7 | `hotkeys` parsing, validation, reserved table, duration validation, dedupe, debounce | T1 | U-H1..U-H5 | 1 d |
| T8 | RegisterHotKey backend on message-only thread, apply/report, conflict mapping | T7 | apply tests, `SendInput` smoke | 1.5 d |
| T9 | LL hook backend: state tracking, auto-repeat, injected flag, reinstall on resume, `consume_keys` | T8 | U-H6, E15 | 1.5 d |
| T10 | `clipcheckd::feedback`: sound (async PlaySound), layered toast window, corner/monitor placement | none | F-1, F-2, F-4 | 2 d |
| T11 | Service wiring: fanout, demand-driven encoder, buffer state machine, enable toggle, `ClipSaved` to library, IPC commands/events | T6, T9, T10, IPC doc | C-1 to C-5, IPC examples | 2 d |
| T12 | Crash recovery (`recover_leftovers`, salvage copy, manifest, TTL, UI event) + soak and disk-full tests | T4, T11 | U-B13, R-1 to R-4, S-1 to S-4 | 2 d |

Critical path: T1, T2, T3, T4, T5, T6, T11. Hotkeys (T7 to T9) and feedback (T10) can run in parallel after T1.

---

## 12. Open questions

1. **Units of `max_minutes`.** The brief says "default 5 min, up to 60". This doc reads it as minutes (1..=60). If it meant seconds, the range becomes 30..=3600 and the disk footprint table changes. Confirm.
2. **Closed GOP requirement.** Clip start correctness and segment independence depend on closed GOP with no B-frame references across IDRs. This must be a hard requirement in the encode doc (encoder option `open_gop=0`, IDR cadence = `segment_ms`). Confirm with the encode owner.
3. **Repeated `moov` per segment.** Each segment is standalone (about 1 to 3 KB of moov each). The alternative is one shared init file per run, which saves negligible space but makes every segment depend on a second file. Proposed: keep standalone segments.
4. **Anti-cheat and global keyboard hooks.** Some anti-cheat systems treat global low-level keyboard hooks as suspicious. Proposed default is `hook_fallback = true` for `auto` bindings, with a per-game exclusion list in settings and a README note. The alternative is to default the hook off and enable it only for bindings that set `method = "hook"`. Decide before beta; this has user-facing risk.
5. **Keeping recovered footage by default.** `keep_recovered = true` keeps up to 24 h of salvaged footage on disk after a crash. Alternative is delete on start with no UI. Proposed: keep, with a visible notification, because the crash footage is usually the most important.
6. **Clip container for Opus.** MKV output for Opus audio means Opus clips will not open in some editors. Alternative is forcing AAC for the buffer when clips are the main use. Proposed: keep Opus as the session choice and accept MKV for clips.
7. **Post-roll semantics.** `postroll_ms` delays the snapshot, which is simple but delays the clip save by up to 10 s. A user may expect the clip to include footage after the press without waiting. Confirm this is acceptable as an explicit option.
8. **Clip output location when `buffer.clip_dir` is unset.** Proposed: recordings root / `<Game>`, so clips sit beside sessions. Alternative: a separate `Clips` folder. Confirm with the library doc owner.
9. **Own-process audio exclusion.** E17 requires the audio doc to exclude `clipcheckd` from the Mix track. If the audio doc cannot do this, the chime appears in clips. Confirm.
10. **Frozen ring after game exit.** Proposed `frozen_ttl_s = 300`, so a user can still clip the last minutes of a game after it exits. This keeps footage on disk after the game ends; confirm the privacy stance is acceptable (local only, so it is).
11. **`EncodedPacket` location.** Section 4.1 places it in `core`. The encode doc may prefer owning it. Either works; `core` avoids a dependency from `buffer` on `encode`.
12. **Hotkey consume mode.** `consume_keys` is only for `method = "hook"`. Some users will want consumption for the default Auto mode. Proposed: not supported in v1, because it breaks game bindings on the same key.
