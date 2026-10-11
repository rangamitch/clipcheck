# 04 — Audio: configurable multi-track capture (`crates/audio`)

Status: Draft v1 · Owner: audio · Depends on: `core`, `session`, `encode`, `buffer`, `library`, `hotkeys`, `clipedit`, `ipc`
Conforms to: `00-brief.md` (Audio policy, Core data pipeline, Naming/ID, Config locations).

---

## 1. Goal & scope

### 1.1 Goal

Record all PC audio as a set of numbered, independently level-controlled tracks alongside the game video, so that a clip or session can be re-balanced later (game music down, Discord up, mic muted) without re-recording. Every track is encoded once and fanned out to the replay buffer and the full-session writer, same as video.

### 1.2 In scope

| Area | In v1 |
|---|---|
| Per-process capture | WASAPI process loopback, INCLUDE and EXCLUDE target-process-tree modes (Win10 build 19041 / 2004+) |
| System mix | Device loopback on the default render endpoint (what the user hears, post-endpoint volume and APOs) |
| Microphone | One capture endpoint on its own track (default device or selected device); stereo output via dual-mono upmix |
| Routing | Ordered rules `exe -> track N`; at most one "everything else" catch-all track |
| Dynamic sessions | Apps that start (or first emit audio) mid-recording and match a rule are attached without restart |
| Device handling | Device selection, device removal, default-device change, hot-plug re-attach with silence fill |
| Conversion | Resample every source to 48 kHz, 32-bit float, stereo internally |
| Mixing | Per-track gain (dB) and mute, smoothed, soft-clip guard, peak/RMS meters |
| Mic hotkey | Push-to-talk and toggle-mute via global hotkey |
| Timing | QPC-based timeline shared with video, drift estimation and correction, A/V anchor |
| Encoding | Per-track AAC-LC (default) or Opus, 128–320 kbps, through `encode` FFmpeg wrapper |
| Silence | Silence fill for any absent source, so every track is always present and time-aligned |
| Fallback | Windows builds before 2004 (no process loopback): documented degraded behavior |
| Container | Track layout written into the container (names, flags, codec, routing snapshot); player track switching and mixing |

### 1.3 Out of scope (v1)

- Per-app volume reflection. Process-loopback tracks are recorded **pre-endpoint-volume**; Windows volume-mixer changes do not affect them (see edge case E7). This is intentional.
- Capture of **exclusive-mode** WASAPI streams. No loopback API can see them; those apps produce silence in every track (E6).
- Audio from non-default render endpoints for the per-app catch-all (E8).
- Multiple microphones at once (one mic track in v1; schema allows more later).
- Real-time monitoring / sidetone output to speakers.
- Noise suppression, echo cancellation, AGC, loudness normalization on export (possible later as per-track filters; the track model leaves room).
- Mouse-button push-to-talk (keyboard only in v1).
- Linux/macOS (brief).

### 1.4 Refinements to the brief (must not contradict; these narrow it)

1. **Multi-track sessions and clips default to Matroska (`.mkv`)**. MKV is the only container in the brief that holds Opus and several named audio tracks in one file; MP4 is allowed only when codec = AAC and the user explicitly picks MP4 (track names then go into `hdlr`, which is weaker). See section 3.6.
2. **Catch-all ("everything else") is implemented by enumerating audio sessions and capturing each unclaimed process tree**, not by subtracting from the system mix. Rationale in section 5.6.
3. **Track 2 "Game only" is the detected game's process tree (INCLUDE)**. If the game has an exclusive-mode stream, Track 2 is silent. Documented, not hidden.
4. **Push-to-talk needs key-up events**, which `RegisterHotKey` does not deliver. Toggle-mute uses `RegisterHotKey`; push-to-talk uses a low-level keyboard hook in the `hotkeys` crate (section 5.7).

---

## 2. User-facing behavior (precise)

### 2.1 Default layout (fresh install)

| Track | Kind | Name (in file) | Source | Default gain | Default |
|---|---|---|---|---|---|
| 1 | `mix` | `1 Mix (all audio)` | Device loopback, default render endpoint | 0 dB | enabled |
| 2 | `game` | `2 Game` | Process loopback INCLUDE, detected game PID tree | 0 dB | enabled |
| 3 | `mic` | `3 Microphone` | Default capture endpoint | 0 dB | enabled |
| 4 | `app_group` | `4 Chat (Discord)` | Routes: `Discord.exe -> 4` | 0 dB | enabled |
| 5–12 | `app_group` | user-defined (`5 Custom`, …) | User rules | 0 dB | not present |

Catch-all: none by default. The user may dedicate one track (any `app_group` slot in 5–12) as catch-all.

### 2.2 Track kinds

| Kind | Meaning | Source(s) | Allowed count |
|---|---|---|---|
| `mix` | What the user hears, every process | 1 device loopback | 0–1 |
| `game` | Detected game process tree only | 1 process loopback INCLUDE (retargeted if the game PID changes, section 5.5) | 0–1 |
| `game_excluded` | Everything except the game tree | 1 process loopback EXCLUDE (game PID) | 0–1 |
| `mic` | Microphone | 1 capture endpoint | 0–1 |
| `app_group` | Sessions matched by rules | N process loopbacks INCLUDE, one per matched session tree | 0–8 (ids 4–12) |
| `catch_all` | Sessions matched by no rule (and not game, not ClipCheck) | N process loopbacks INCLUDE, dynamic | 0–1 |

`game_excluded` is cheap (one client, no enumeration) and is the sensible "non-game audio" track for users who want their Discord and music separated from game sound without setting up rules.

### 2.3 Routing behavior

- A rule is `exe` (basename, case-insensitive, e.g. `discord.exe`) plus `track`.
- Rules are evaluated in list order; the **first enabled match wins**. Each session tree is routed to exactly one `app_group` track or to the catch-all.
- A session matches no rule → goes to the catch-all track if one exists; otherwise it is heard only on Track 1.
- Game-tree sessions are never routed to catch-all (`catch_all_excludes_game = true`), but they **can** be routed by an explicit rule to an app track (the same audio then appears on Track 2 and on that app track; this is intended).
- A rule may not target a `mix`, `game`, `game_excluded`, or `mic` track. Validation error on save.
- Rules are live: editing them during a recording takes effect within one `session_poll_ms` tick (default 1 s). Sessions no longer matching move to their new track; there is a 20 ms crossfade on the session's contribution to avoid clicks.

### 2.4 Mic behavior

- Mic device: `default` (follows default communications/console capture device changes, see 2.5) or a specific endpoint ID chosen in Settings.
- Mono device input is duplicated to L/R. Track is always stereo 48 kHz.
- Mute and push-to-talk never stop the capture stream. They change the mixer gain (10 ms ramp), so the track stays present with silence.
- Push-to-talk (`mic.ptt_mode = "push_to_talk"`): mic is open only while `mic.ptt_hotkey` is held, plus `mic.ptt_release_tail_ms` after release.
- Toggle-mute (`mic.ptt_mode = "toggle_mute"`): each press of the hotkey flips mute.
- UI mute (`audio.set_track_mute`) and hotkeys combine as: `effective_mute = ui_mute OR (ptt_mode == push_to_talk AND !ptt_open)`.
- A hotkey press during a recording is applied immediately, and an `audio.mic_state` event is sent to the UI and the tray.

### 2.5 Device behavior

| Event | Behavior |
|---|---|
| Mic endpoint set to `default`; Windows default capture device changes | Re-open on the new default after a 150 ms debounce; 20 ms crossfade; `audio.device_changed` event |
| Selected mic endpoint is unplugged or disabled | Track goes silent (silence fill) within one buffer period; device monitor retries every 0.25/0.5/1/2/5 s (cap 5 s); reattach automatically on return |
| Default render device changes | Mix track (device loopback) re-attaches to the new default; session enumerator is re-registered on the new endpoint |
| Default render device changes while process loopbacks run | No effect on process tracks (process loopback is endpoint-independent) |
| Device sample rate or channel count changes | Source is re-opened with new format; resampler rebuilt; no timeline discontinuity |
| No audio device at all | All audio tracks silent; `audio.source_state` = `absent`; recording continues (video unaffected) |

### 2.6 Dynamic sessions

- An app that starts after recording began and matches a rule gets its own process loopback within `session_poll_ms` of its first audio session (or of its process appearing, see 5.5).
- The track starts with silence until audio flows; no gap in the file timeline.
- Apps that exit have their capture stopped; the track returns to silence for that source.

### 2.7 Settings UI (summary)

Settings > Audio shows: track table (name, kind, gain slider -60…+12 dB, mute, enabled, source status badge), route table (drag to reorder, exe picker from currently running audio sessions), device pickers (render for mix, capture for mic), codec (AAC/Opus), bitrate, and the mic PTT binding with a "Press a key" capture control. The Record bar shows a 5-track peak meter strip while recording (meters at 10 Hz).

### 2.8 Player behavior (summary; detail in 3.7)

- The player lists tracks by their file names with a kind icon, a per-track gain/mute, and a "Solo" control.
- Default playback: all tracks enabled at their recorded gain; the user's changes are local to the viewer and are not saved to the file.
- Track layout is read from the library cache; if the container is re-indexed, layout is re-read from the file.

---

## 3. Architecture & data flow

### 3.1 Position in the pipeline

```
          WASAPI                              audio crate                               encode crate                 buffer / session
 +-----------------------+   PCM f32 48k    +-------------------+   PcmBlock   +--------------------+  EncodedAudioPacket  +-----------+
 | Device loopback (mix) |----------------->|                   |------------->|                    |--------------------->| Replay    |
 | Process loopback x N  |   (per source    | Source ring       |  per track   | FfmpegAudioEncoder |   (per track, PTS    | ring (fMP4)|
 | Mic capture           |    resampled)    | -> TrackMixer     |   (10 ms)    | AAC | Opus         |    48 kHz timeline)  +-----------+
 +-----------------------+                  | -> meters, gains  |              +--------------------+                      +-----------+
          ^                                 +-------------------+                                                      | Session   |
          |                                          ^                                                                  | writer    |
   IMMNotificationClient / IAudioSessionManager2     | QPC timeline (shared with video)                                 | (MKV)     |
   WH_KEYBOARD_LL (PTT) via hotkeys crate            |                                                                  +-----------+
                                                                                                    
```

### 3.2 Crate layout (`crates/audio/src`)

```
audio/
  lib.rs              public API: AudioPipeline, AudioLayout re-exports
  clock.rs            QpcTimeline, sample-index <-> QPC mapping, drift estimator, PI controller
  format.rs           WAVEFORMATEX builders, sample conversion (i16/i24/i32/f32 -> f32), channel up/down-mix
  resample.rs         Resampler (rubato-based sinc, dynamic ratio)
  source/
    mod.rs            SourceSpec, SourceState, SourceSink
    device_loop.rs    default render endpoint loopback (IAudioClient + LOOPBACK flag)
    process_loop.rs   ActivateAudioInterfaceAsync process loopback (INCLUDE/EXCLUDE)
    capture.rs        mic capture endpoint
    silence.rs        synthetic source for absent cases
    ring.rs           lock-free SPSC PCM ring with QPC-tagged writes
  devices.rs          IMMDeviceEnumerator wrapper, IMMNotificationClient, device selection
  sessions.rs         IAudioSessionManager2 enumeration + IAudioSessionNotification, process snapshot, ancestry
  router.rs           Router: rules -> track id, catch-all, shadow detection
  mixer.rs            TrackMixer, gain/mute smoothing, meters, soft clip
  ptt.rs              PushToTalk state machine (consumes hotkey events)
  encode.rs           TrackEncoder trait (implemented in `encode` crate), packet fan-out
  layout.rs           AudioLayout, validation, container metadata (names/tags) serialization
  fallback.rs         OS capability probe (build number), degraded-mode selection
  thread.rs           MTA init, MMCSS "Pro Audio" registration, thread naming
  metrics.rs          per-source/track counters (underruns, silence samples, resyncs, ppm)
```

Dependency rules: `audio` depends on `core`, `windows`, `rubato`, `serde`. It does **not** depend on `encode`. The `TrackEncoder` trait lives in `audio::encode`; `encode` implements it (`encode::audio::FfmpegAudioEncoder`), so the dependency points `encode -> audio`. `hotkeys` depends on nothing in `audio`; `audio::ptt` consumes an abstract `PttEvent` stream that `hotkeys` produces.

### 3.3 Threads

| Thread | Count | Role | Priority |
|---|---|---|---|
| WASAPI capture thread per source | 1 per source (≤ 16 process + 1 device + 1 mic) | Event-driven `WaitForSingleObject` on the WASAPI event, `GetBuffer`, convert, resample, write ring | MMCSS "Pro Audio", THREAD_PRIORITY_TIME_CRITICAL |
| Mixer tick thread | 1 | 10 ms timeline tick: pull from rings, mix, meters, emit `PcmBlock` per track | MMCSS "Pro Audio" |
| Encoder thread | 1 per track (≤ 12) | `FfmpegAudioEncoder::encode`, emit packets to fan-out | Normal+1 |
| Session poll thread | 1 | 1 Hz (`session_poll_ms`) process snapshot and audio-session diff | Below normal |
| Device monitor | COM callback threads (system-owned) + 1 worker | Callbacks only post `DeviceEvent`s to a channel; worker mutates state | Normal |
| PTT hook thread | 1 (in `hotkeys`) | `WH_KEYBOARD_LL` + message pump; callback only posts to channel (must return < 300 ms, see E12) | Normal |

COM: every audio thread calls `CoInitializeEx(None, COINIT_MULTITHREADED)`. Notification callbacks never block.

### 3.4 Timeline and anchor

- Master clock is **QPC** (`QueryPerformanceCounter`, `QueryPerformanceFrequency`), expressed internally in 100 ns units (`Qpc100ns`, i64).
- The audio timeline is a **48 kHz sample index** `S` where `S = round((qpc_100ns - anchor_100ns) * 48000 / 10_000_000)`.
- `anchor_100ns` is the QPC of the **first video frame that will be written to the session part**, set by the `session` crate and passed to `AudioPipeline::start(anchor)`. Samples earlier than the anchor are discarded; if audio starts after the anchor, the timeline is padded with silence (no shift).
- Audio packet PTS written to the container = `S_packet_start` (sample index), converted to the container timebase. Video PTS uses the same anchor. This makes A/V alignment a property of one shared origin rather than of two independent wall-clock readings.

### 3.5 Clock sources per capture type

| Source | Timestamp available | Used as |
|---|---|---|
| Device loopback / mic | `IAudioCaptureClient::GetBuffer(..., qpc_position)` (100 ns, QPC base) | Packet arrival QPC; drift estimate |
| Process loopback | Same `GetBuffer` QPC position; if reported as 0, fall back to QPC at wake-up minus `buffer_latency` | Same |
| Video (WGC) | QPC sampled in the WGC `FrameArrived` callback (owned by `capture`) | Anchor (first frame) and split keyframes |

### 3.6 Container and track layout storage

**Matroska (default for sessions and clips with ≥ 1 audio track):**

| Element | Used for | Written by |
|---|---|---|
| `Tracks/TrackEntry/Name` | Track name, e.g. `2 Game` (UTF-8) | `audio::layout::to_container_tracks` |
| `Tracks/TrackEntry/Language` | `und` (no language info) | same |
| `Tracks/TrackEntry/FlagDefault` | `1` for track 1 only, so default players pick the mix | same |
| `Tracks/TrackEntry/FlagEnabled` | `1` for all written tracks | same |
| `Tracks/TrackEntry/CodecID` | `A_AAC` or `A_OPUS` | encode |
| `Tracks/TrackEntry/Audio/Channels`, `SamplingFrequency` | 2, 48000 | encode |
| `Tags` (TargetTypeValue 50 "Album/whole file") | SimpleTag `CLIPCHECK_AUDIO_LAYOUT` = compact JSON of `AudioLayout` (see 3.6.1) | `audio::layout::to_tags_json` |
| `Tags` SimpleTag `CLIPCHECK_LAYOUT_VERSION` | `1` | same |
| Per-track TrackNumber / TrackUID | 1..N in layout order | session writer |

**MP4 (only when codec = AAC and user picks MP4):** track names in `hdlr` handler name; layout JSON in a `udta/meta/ilst` atom under `©cmt` (ClipCheck-specific). Names are best-effort in MP4 and are shown as `Track N` if missing.

**Replay ring segments (fMP4, 2 s):** each segment has one `moof`/`traf` per audio track with the track name in `hdlr`. Clip export (`clipedit`) re-muxes to MKV and writes the full Matroska metadata above, so clips always carry the proper layout.

#### 3.6.1 Embedded layout JSON (schema v1)

```json
{
  "v": 1,
  "sample_rate": 48000,
  "codec": "aac",
  "bitrate_kbps": 192,
  "tracks": [
    {"id": 1, "kind": "mix",       "name": "1 Mix (all audio)", "gain_db": 0.0, "muted_at_record": false, "source": "device_loopback:default"},
    {"id": 2, "kind": "game",      "name": "2 Game",            "gain_db": 0.0, "muted_at_record": false, "source": "process_include:pid=18432,exe=game.exe", "degraded": false},
    {"id": 3, "kind": "mic",       "name": "3 Microphone",      "gain_db": 0.0, "muted_at_record": false, "source": "capture:default"},
    {"id": 4, "kind": "app_group", "name": "4 Chat (Discord)",  "gain_db": 0.0, "muted_at_record": false, "source": "routes:discord.exe"}
  ],
  "routes": [{"exe": "discord.exe", "track": 4}],
  "catch_all_track": 0,
  "fallback": null
}
```

`muted_at_record` records the state at the time the track was written (not the viewer's choice). `gain_db` is the **recording-time** gain; the player applies its own gain on top.

### 3.7 Player (UI, `apps/ui`) — how tracks are presented and played

Problem: Chromium's `<video>` (WebView2) plays only one audio track from a file and exposes no `audioTracks` switching. Decoding a multi-track file with Web Audio also picks the first track only.

Solution (decided):

1. The `<video>` element is **muted**. It supplies the picture.
2. For each audio track, the player creates an `<audio>` element whose `src` is `clip://<recording_id>/track/<n>`.
3. The `clip://` handler serves an **audio-only, stream-copied** file for track *n*:
   - AAC → fragmented MP4 (`ftyp iso5`, `moov`, `moof` segments), `Content-Type: audio/mp4`
   - Opus → WebM (`audio/webm`)
   - Range requests supported (same handler as video).
4. Each `<audio>` feeds a `MediaElementAudioSourceNode` → `GainNode` (per-track gain/mute) → `destination`. This gives true per-track mixing, solo, and mute in the viewer.
5. Sync: on `play`, `pause`, `seeked`, and `ratechange` of the video, mirror the state to every audio element. Every 250 ms, if an audio element's `currentTime` differs from the video by > 30 ms, correct it (`currentTime = video.currentTime`). Above 300 ms, hard-seek.
6. The per-track audio files are generated **lazily** on first request by `clipedit` (stream copy; no decode) and cached in `%LOCALAPPDATA%\ClipCheck\trackcache\<RecordingId>\track-<n>.{m4a|webm}`. Cache is LRU-capped at 512 MB and cleared for a recording on delete. If generation fails, the track falls back to "unavailable" in the UI and the video still plays with the mix.
7. Track names, kinds, recorded gains, and degraded flags come from the library cache table `audio_tracks` (section 4.4), populated from the embedded layout at index time. The UI never parses the container itself.

Tracks are shown in the player control as a list: icon by kind, name, a level meter (from the Web Audio analyser), gain slider, mute, solo. Viewer changes are kept in memory only (and optionally in `localStorage` as a convenience); they never rewrite the file.

---

## 4. Key types, traits and function signatures

All names are in `clipcheck_audio` unless stated. Types shared with other crates (`TrackId` shape, `RecordingId`) are re-exported from `core`.

### 4.1 Constants and primitive types

```rust
pub const SAMPLE_RATE: u32 = 48_000;
pub const INTERNAL_CHANNELS: usize = 2;
pub const TICK_FRAMES: usize = 480;            // 10 ms at 48 kHz
pub const MAX_TRACKS: u8 = 12;
pub const MAX_PROCESS_CAPTURES_HARD: usize = 32;

/// 100-nanosecond QPC-based timestamp.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Qpc100ns(pub i64);

/// Track id, 1..=MAX_TRACKS. Stable for the life of a layout.
pub type TrackId = u8;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(pub u64);
```

### 4.2 Layout and config types (serde; mirror the TOML in section 6)

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind { Mix, Game, GameExcluded, Mic, AppGroup, CatchAll }

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceSelector { Default, Id(String) }

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackSpec {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,            // 1..=48 chars, no control chars
    pub enabled: bool,
    pub gain_db: f32,            // -60.0..=12.0
    pub muted: bool,
    pub device: DeviceSelector,  // used by Mix and Mic; ignored otherwise
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RouteRule {
    pub exe: String,             // lowercase basename, e.g. "discord.exe"
    pub track: TrackId,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec { Aac, Opus }

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioLayout {
    pub tracks: Vec<TrackSpec>,
    pub routes: Vec<RouteRule>,
    pub codec: AudioCodec,
    pub bitrate_kbps: u32,               // 128..=320
    pub catch_all_excludes_game: bool,   // default true
    pub allow_degraded_game_track: bool, // default true
}

impl AudioLayout {
    /// Validates ids, kinds, counts, name lengths, route targets, gain ranges.
    pub fn validate(&self) -> Result<(), LayoutError>;
    /// Default layout from section 2.1.
    pub fn default_layout() -> Self;
    /// Returns the single catch-all track id, if any.
    pub fn catch_all(&self) -> Option<TrackId>;
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("track id {0} out of range 1..=12")] IdOutOfRange(TrackId),
    #[error("duplicate track id {0}")] DuplicateId(TrackId),
    #[error("more than one {0:?} track")] TooMany(TrackKind),
    #[error("route for {exe} targets {track}, which is {kind:?}")] RouteToNonAppTrack { exe: String, track: TrackId, kind: TrackKind },
    #[error("route for {exe} targets missing track {track}")] RouteToMissingTrack { exe: String, track: TrackId },
    #[error("gain {0} dB outside -60..=12")] GainOutOfRange(f32),
    #[error("bitrate {0} kbps outside 128..=320")] BitrateOutOfRange(u32),
    #[error("opus requires Matroska container")] OpusNeedsMkv,
}
```

### 4.3 Capture sources

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSpec {
    /// Whole-system mix of the given render endpoint (loopback).
    DeviceLoopback { device: DeviceSelector },
    /// Per-process INCLUDE (process tree).
    ProcessInclude { pid: u32, exe: String },
    /// Per-process EXCLUDE (process tree).
    ProcessExclude { pid: u32, exe: String },
    /// Capture endpoint (microphone).
    Capture { device: DeviceSelector },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    Idle,
    Opening,
    Active { device_fmt: DeviceFormat },
    Absent { reason: AbsentReason, since: Qpc100ns },
    Reconnecting { attempt: u32, next_try: Qpc100ns },
    Unsupported { reason: &'static str },
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbsentReason { DeviceRemoved, DeviceDisabled, ProcessExited, NoSession, PermissionDenied, NoDevice }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceFormat { pub sample_rate: u32, pub channels: u16, pub bits: u16, pub is_float: bool }

/// A capture source owns one WASAPI client (or activation) and one capture thread.
pub trait CaptureSource: Send {
    fn id(&self) -> SourceId;
    fn spec(&self) -> &SourceSpec;
    fn state(&self) -> SourceState;
    /// Starts the capture thread; writes resampled 48 kHz stereo into `ring`.
    fn start(&mut self, ring: SourceRingWriter, timeline: TimelineHandle) -> Result<(), AudioError>;
    fn stop(&mut self);
}

pub struct SourceRingWriter { /* SPSC producer; writes PcmChunk with QPC tag */ }

pub struct PcmChunk {
    pub qpc: Qpc100ns,       // QPC of first frame in `samples`
    pub frames: usize,       // stereo frames
    pub samples: Vec<f32>,   // interleaved L,R,L,R (len = frames * 2)
}
```

### 4.4 Process loopback, sessions, routing

```rust
/// Activation wrapper around ActivateAudioInterfaceAsync(VAD\Process_Loopback, ...).
pub struct ProcessLoopbackClient { /* IAudioClient, IAudioCaptureClient, event handle */ }

impl ProcessLoopbackClient {
    /// Fails with AudioError::ProcessLoopbackUnsupported on builds < 19041.
    pub fn activate(pid: u32, mode: LoopbackMode, format: WaveFormatSpec) -> Result<Self, AudioError>;
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LoopbackMode { IncludeTree, ExcludeTree }

/// Requested format: IEEE float32, 48000 Hz, 2 channels. Driver/engine converts (AUTOCONVERTPCM | SRC_DEFAULT_QUALITY).
pub struct WaveFormatSpec { pub sample_rate: u32, pub channels: u16 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSessionInfo {
    pub pid: u32,
    pub root_pid: u32,         // topmost ancestor that is also an audio session holder (dedupe key)
    pub exe_lower: String,     // basename, lowercase
    pub active: bool,          // session state == AudioSessionStateActive
}

pub trait SessionEnumerator: Send {
    /// Re-enumerates sessions on the default render endpoint and registers for new sessions.
    fn refresh(&mut self) -> Result<Vec<AudioSessionInfo>, AudioError>;
    fn on_default_render_changed(&mut self) -> Result<(), AudioError>;
}

pub struct ProcessSnapshot { /* Toolhelp32 list: pid, ppid, exe */ }
impl ProcessSnapshot {
    pub fn take() -> Result<Self, AudioError>;
    pub fn exe_of(&self, pid: u32) -> Option<&str>;
    pub fn parent_of(&self, pid: u32) -> Option<u32>;
    pub fn ancestors(&self, pid: u32) -> impl Iterator<Item = u32> + '_;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteDecision {
    /// Session tree routed to an app_group track by rule `index`.
    Rule { track: TrackId, rule_index: usize },
    /// No rule matched; goes to catch-all if configured.
    CatchAll { track: TrackId },
    /// Nothing claims this session (no catch-all, or excluded as game/ClipCheck/system).
    Unrouted { why: &'static str },
}

pub struct Router { /* compiled rules, catch_all, game_pid */ }
impl Router {
    pub fn new(layout: &AudioLayout, game_pid: Option<u32>) -> Self;
    pub fn decide(&self, exe_lower: &str, pid: u32) -> RouteDecision;
}
```

### 4.5 Devices and hot-plug

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow { Render, Capture }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role { Console, Multimedia, Communications }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo { pub id: String, pub name: String, pub flow: Flow, pub is_default: bool, pub active: bool }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    Added(DeviceInfo),
    Removed { id: String },
    StateChanged { id: String, active: bool },
    DefaultChanged { flow: Flow, role: Role, id: String },
}

/// Implements IMMNotificationClient. Callbacks only send on the channel.
pub struct DeviceMonitor { /* IMMNotificationClient registration, Sender<DeviceEvent> */ }

pub fn list_devices() -> Result<Vec<DeviceInfo>, AudioError>;
```

### 4.6 Mixer, resampler, meters

```rust
pub struct Resampler { /* rubato sinc resampler; in_rate -> 48000; dynamic ratio */ }
impl Resampler {
    pub fn new(in_rate: u32, channels: usize) -> Result<Self, AudioError>;
    /// Multiplies the nominal ratio by (1 + ppm / 1e6). Called by the drift controller.
    pub fn set_trim_ppm(&mut self, ppm: f64);
    pub fn process(&mut self, input_interleaved: &[f32], out: &mut Vec<f32>);
}

pub struct TrackMixer {
    pub id: TrackId,
    gain_lin: SmoothedF32,     // 5 ms ramp
    mute_ramp: SmoothedF32,    // 10 ms ramp, 0.0 or 1.0
    inputs: Vec<SourceId>,
}
impl TrackMixer {
    /// Mixes `frames` from every input ring at timeline index `start_sample`
    /// into `out`; missing input samples are silence and are counted.
    pub fn mix_tick(&mut self, rings: &RingSet, start_sample: u64, frames: usize, out: &mut [f32]) -> MixStats;
}

#[derive(Default, Clone, Copy, Debug)]
pub struct MixStats { pub silence_frames: u32, pub clipped_samples: u32, pub peak: f32, pub rms: f32 }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Meter { pub track: TrackId, pub peak_dbfs: f32, pub rms_dbfs: f32 }

/// Soft clip guard applied after the sum, before encode: tanh-like knee above -1 dBFS.
pub fn soft_clip(x: f32) -> f32;
```

### 4.7 Encoding fan-out (trait lives in `audio`, implemented in `encode`)

```rust
pub trait TrackEncoder: Send {
    fn codec(&self) -> AudioCodec;
    /// AAC-LC: 1024 frames per packet. Opus: 960 frames (20 ms).
    fn frame_size(&self) -> usize;
    /// `pcm` is interleaved stereo f32, multiple of frame_size is NOT required; encoder buffers remainder.
    fn encode(&mut self, pcm: &[f32], pts_48k: u64) -> Result<Vec<EncodedAudioPacket>, AudioError>;
    fn flush(&mut self) -> Result<Vec<EncodedAudioPacket>, AudioError>;
    /// Priming/delay samples the encoder adds at the start (AAC ~1024, Opus ~312).
    fn priming_samples(&self) -> u32;
}

#[derive(Clone, Debug)]
pub struct EncodedAudioPacket {
    pub track: TrackId,
    pub pts_48k: u64,        // timeline sample index of first sample (after priming compensation)
    pub duration_48k: u32,
    pub data: Vec<u8>,
    pub keyframe: bool,      // always true for audio; kept for uniform muxer API
}

/// Encoder factory injected by the `encode` crate.
pub trait AudioEncoderFactory: Send + Sync {
    fn create(&self, codec: AudioCodec, bitrate_kbps: u32, track: TrackId) -> Result<Box<dyn TrackEncoder>, AudioError>;
}
```

### 4.8 Push-to-talk

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttMode { Off, PushToTalk, ToggleMute }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttInput { KeyDown, KeyUp, Press }   // Press = RegisterHotKey toggle event

pub struct PushToTalk { mode: PttMode, release_tail_ms: u32, /* state */ }
impl PushToTalk {
    pub fn new(mode: PttMode, release_tail_ms: u32) -> Self;
    /// Returns Some(new_ptt_open) when the open state changes.
    pub fn on_input(&mut self, input: PttInput, now: Qpc100ns) -> Option<bool>;
    /// Called every tick; applies release tail expiry.
    pub fn tick(&mut self, now: Qpc100ns) -> Option<bool>;
}
```

### 4.9 Pipeline facade (used by `session` and `buffer`)

```rust
pub struct AudioPipelineConfig {
    pub layout: AudioLayout,
    pub game_pid: Option<u32>,
    pub target_buffer_ms: u32,        // 40..=400, default 100
    pub drift_max_ppm: f64,           // 50..=1000, default 300
    pub hard_resync_ms: u32,          // 10..=500, default 40
    pub max_process_captures: usize,  // 4..=32, default 16
    pub session_poll_ms: u32,         // 250..=5000, default 1000
}

pub struct AudioPipeline { /* owns sources, mixer, encoders, router, ptt, device monitor */ }

impl AudioPipeline {
    pub fn new(cfg: AudioPipelineConfig, factory: Arc<dyn AudioEncoderFactory>) -> Result<Self, AudioError>;
    /// Starts capture. `anchor` is the QPC of the first video frame of this session part.
    pub fn start(&mut self, anchor: Qpc100ns, sink: Arc<dyn PacketSink>) -> Result<(), AudioError>;
    /// Splits at a video keyframe QPC: returns the sample index of the cut.
    pub fn split_at(&mut self, keyframe_qpc: Qpc100ns) -> u64;
    pub fn stop(&mut self) -> Result<(), AudioError>;
    pub fn apply_layout(&mut self, layout: AudioLayout) -> Result<(), LayoutError>; // live edits
    pub fn set_track_gain(&self, id: TrackId, gain_db: f32) -> Result<(), AudioError>;
    pub fn set_track_mute(&self, id: TrackId, muted: bool) -> Result<(), AudioError>;
    pub fn ptt_input(&self, input: PttInput);
    pub fn meters(&self) -> Vec<Meter>;
    pub fn status(&self) -> PipelineStatus;
}

pub trait PacketSink: Send + Sync {
    /// Called from encoder threads; packets are in per-track PTS order.
    fn on_packets(&self, packets: Vec<EncodedAudioPacket>);
    fn on_event(&self, event: AudioEvent);
}

#[derive(Clone, Debug)]
pub enum AudioEvent {
    SourceState { source: SourceId, state: SourceState },
    DeviceChanged { flow: Flow, role: Role, device: DeviceInfo, reason: &'static str },
    RouteMatched { exe: String, pid: u32, track: TrackId },
    RouteShadowed { exe: String, pid: u32, by_pid: u32 },
    DriftCorrected { source: SourceId, ppm: f64, offset_ms: f64 },
    HardResync { source: SourceId, offset_ms: f64 },
    FallbackActive { mode: DegradedMode },
    MicState { open: bool, reason: &'static str },
    Error { code: AudioErrorCode, message: String },
}

#[derive(Clone, Debug)]
pub struct PipelineStatus {
    pub tracks: Vec<TrackStatus>,
    pub sources: Vec<(SourceId, SourceState)>,
    pub process_captures: usize,
    pub fallback: Option<DegradedMode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DegradedMode {
    /// Win10 < 2004: Game track = device loopback (all PC audio), flagged.
    GameTrackSystemMix,
    /// Win10 < 2004: app_group/catch_all/game_excluded tracks are silent and flagged.
    AppTracksUnavailable,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("device not found: {0}")] DeviceNotFound(String),
    #[error("process loopback unsupported on this Windows build")] ProcessLoopbackUnsupported,
    #[error("audio activation failed: {0:?}")] ActivationFailed(windows::core::HRESULT),
    #[error("microphone access denied by Windows privacy settings")] MicAccessDenied,
    #[error("unsupported device format: {0:?}")] FormatUnsupported(DeviceFormat),
    #[error("resampler: {0}")] Resampler(String),
    #[error("encoder: {0}")] Encoder(String),
    #[error("windows: {0}")] Win(#[from] windows::core::Error),
    #[error("layout: {0}")] Layout(#[from] LayoutError),
}
```

### 4.10 Fallback probe

```rust
pub struct OsCaps { pub build: u32, pub process_loopback: bool }
pub fn probe_os() -> OsCaps;   // RtlGetVersion build; process_loopback = build >= 19041
```

### 4.11 IPC payloads (serde, see section 7)

Defined in `ipc` but typed from `audio::layout` and `audio::AudioEvent` (`TrackSpec`, `RouteRule`, `Meter`, `PipelineStatus` subset).

---

## 5. Algorithms / state machines

### 5.1 Pipeline start

```
start(anchor, sink):
  1. caps = probe_os()
  2. layout.validate()                                    -> on error: abort start, emit Error
  3. router = Router::new(layout, game_pid)
  4. create one SourceSpec per needed source:
       mix       -> DeviceLoopback(default render)       (always, if mix enabled)
       game      -> ProcessInclude(game_pid)              (if game_pid known and game track enabled)
       game_excl -> ProcessExclude(game_pid)              (if enabled and game_pid known)
       mic       -> Capture(device)                        (if enabled)
       app/catch -> dynamic, from section 5.5
  5. for each source:
       if caps.process_loopback == false and spec is Process*:
            state = Unsupported; apply fallback (5.8); continue
       source.start(ring, timeline)
  6. mixer and encoders created per enabled track (encoder via factory)
  7. spawn mixer tick thread (5.2) and session poll thread (5.5)
  8. device monitor registered (5.6)
  9. emit SourceState for all sources
```

### 5.2 Mixer tick (timeline-driven, 10 ms)

The mixer never waits for sources. It pulls a fixed window from the timeline and fills silence where data is missing. This keeps every track continuous when a source disappears.

```
loop every 10 ms (waitable timer, MMCSS):
  now      = QPC_100ns()
  target   = sample_index(now) - target_buffer_samples     # latency = target_buffer_ms
  # target is the end of the window to emit; emit window [next_emit, next_emit + 480)
  while next_emit + 480 <= target:
     for each enabled track T:
        stats = T.mix_tick(rings, next_emit, 480, out)
        tracker.update(T, stats)                        # silence, peak, rms
        pts = next_emit
        encoder[T].encode(out, pts)                      # may return packets
        fanout(packets)
     next_emit += 480
  tick_ptt(now)
```

`mix_tick` for each input `s`:

```
read_index = next_emit
ring_s.read(read_index, 480)
  - if ring_s has samples for [read_index, read_index+480): copy
  - for ranges with no data: write 0.0, silence_frames += n
  - if ring_s.write_index < read_index (source behind): silence; underrun count++
  - if ring_s.start_index > read_index (source ahead, data older than window): drop, overflow count++
out += gain_lin.next() * mute_ramp.next() * sample
```

The ring stores samples keyed by timeline index (not arrival order), so late or early bursts are placed correctly.

### 5.3 Source capture loop (per WASAPI source)

```
thread start: CoInitializeEx(MTA); MMCSS("Pro Audio")
loop:
  WaitForSingleObject(event, 200 ms)                    # event-driven, AUDCLNT_STREAMFLAGS_EVENTCALLBACK
  if timeout: check device status; continue
  loop while GetPacketLength > 0:
    GetBuffer(&data, &frames, &flags, &dev_pos, &qpc_pos)
    qpc = qpc_pos if qpc_pos != 0 else (now_qpc - frames_latency)
    if flags & AUDCLNT_BUFFERFLAGS_SILENT: write zeros
    convert to f32 (format.rs), to stereo (up/down-mix)
    resampler.process(in) -> out48k                        # rate conversion
    ring.write(qpc_to_sample(qpc) mapped with drift controller output, out48k)
    ReleaseBuffer(frames)
    drift.observe(qpc, cumulative_input_frames)            # 5.4
  on error AUDCLNT_E_DEVICE_INVALIDATED: state = Absent(DeviceRemoved); exit loop -> 5.6 reconnect
```

Mapping QPC to the timeline uses the **estimated** rate, not the nominal one. The sample index for a chunk is `S = round((qpc - anchor) * 48000 / 1e7)` with the same mapping as the mixer, so any residual rate error appears as a growing offset that 5.4 corrects.

### 5.4 Drift estimation and correction

Each source compares the rate at which its samples arrive (device clock) with the QPC clock that the timeline uses.

```
observe(qpc, cum_frames_out):           # cum_frames_out = frames delivered at 48 kHz
  push (qpc, cum_frames_out) into ring buffer of last 10 s (≈ 1 point / 10–20 ms)
  every 1 s:
     fit slope m over window by least squares: frames_out ~ m * qpc + b
     ideal slope = 48000 / 1e7 frames per 100ns  (i.e., 4.8e-3 frames per 100ns)
     ppm = (m / ideal - 1) * 1e6
  PI controller on buffer fill error (fill_samples - target_fill_samples):
     e = (fill - target) / 48000                          # seconds
     integral += e * dt
     trim_ppm = clamp(Kp * e + Ki * integral, -drift_max_ppm, +drift_max_ppm)
     Kp = 200 ppm/s, Ki = 20 ppm/(s·s)                    # tuned in tests (section 10)
  resampler.set_trim_ppm(trim_ppm)                        # resampler adapts output rate
  emit DriftCorrected every 10 s if |ppm| > 20

offset check (every 1 s):
  expected = (qpc_now - anchor) * 48000 / 1e7
  offset_ms = (written_samples - expected) / 48
  if |offset_ms| > hard_resync_ms:
      if written_samples < expected:  insert (expected - written) silence samples at write point
      else:                           drop (written - expected) samples at write point
      emit HardResync
```

Rationale: resampler trim is inaudible (≤ 300 ppm ≈ 0.026 cents of pitch shift, below perception thresholds), while hard resync is a rare, logged discontinuity. Long sessions (hours) rely on trim alone; the hard resync is a backstop for device stalls.

### 5.5 Session discovery and dynamic routing

Runs every `session_poll_ms` while the pipeline is recording.

```
refresh():
  snap   = ProcessSnapshot::take()                          # Toolhelp, ~1 ms
  sess   = enumerator.refresh()                             # IAudioSessionManager2 on default render
           (also registered for IAudioSessionNotification: OnSessionCreated sets dirty flag)
  pids   = { s.pid : s in sess, s.pid != 0, exe not in {audiodg.exe} }
  # Exe-only matches for processes that exist but have not created a session yet:
  pids  += { p.pid : p in snap, router.has_rule(p.exe) }
  pids  -= { ClipCheck tree, pids of disabled source }

  decisions = { pid : router.decide(snap.exe_of(pid), pid) for pid in pids }

  # Dedupe by ancestry (process loopback INCLUDE covers the whole tree)
  for pid in decisions sorted by depth ascending (root first):
     for anc in snap.ancestors(pid):
        if anc in active_claims:
           if active_claims[anc].track != decisions[pid].track:
               emit RouteShadowed(pid, by_pid=anc); drop pid
           else:
               drop pid                                     # already covered by ancestor capture
           break

  desired = { pid -> track } after dedupe
  # Diff
  for (pid, track) in desired not in active:
     if active_process_captures >= max_process_captures:  emit Error(cap); skip
     start ProcessLoopbackClient(pid, IncludeTree) -> ring of track
     emit RouteMatched
  for (pid, track) in active not in desired:
     stop capture; ring gets silence from now on; crossfade 20 ms out
```

Notes:

- Exe-only matching means an `app_group` track starts capturing as soon as the matched process exists, even before it renders. Process loopback on a silent process yields silence, so no harm; this avoids missing the first seconds of a Discord call.
- `catch_all` membership is "every session pid not matched by rule and not game tree and not ClipCheck". Because a tree include captures children, a child session of an unmatched parent is already covered; dedupe handles it.
- A pid that exits is removed on the next refresh; its ring receives silence.

### 5.6 Why catch-all is enumeration, not subtraction

| Option | Mechanism | Result |
|---|---|---|
| A. Subtract claimed captures from system mix | `catch_all = system_mix − Σ claimed` | **Rejected.** System mix is post-endpoint-volume and post-APO; process captures are pre-mix. Subtraction leaves residue whenever the user changes endpoint volume, uses Equalizer APO, or has spatial audio. Not sample-exact, so audible artifacts. |
| B. Single EXCLUDE client | `ProcessExclude(pid)` | **Rejected** for multi-rule setups: EXCLUDE takes one PID. Works only for `game_excluded`. |
| C. Enumerate sessions, INCLUDE each unclaimed tree (chosen) | One process loopback per unclaimed session tree | Exact per-app audio; cost scales with number of audio apps (typically 3–12); capped by `max_process_captures`. |

If C exceeds the cap, the oldest unclaimed sessions are skipped with an `Error(cap)` event and the catch-all track carries a `degraded: "partial"` marker in the layout written to the file.

### 5.7 Push-to-talk and toggle-mute state machine

```
PushToTalk (mode = push_to_talk):
   state: Closed (default)
   KeyDown  : Closed -> Open   (return Some(true))
   KeyUp    : Open   -> Tail   (record t_release; release_tail_ms)
   tick(now): Tail && now - t_release >= tail -> Closed (return Some(false))
   KeyDown during Tail: Tail -> Open (cancel tail)
   auto-repeat KeyDown (already Open): ignored

ToggleMute (mode = toggle_mute):
   state: Open (unmuted) by default
   Press: flip state (return Some(new_open))

Off: all inputs ignored.

effective_mic_mute = ui_mute || (mode == PushToTalk && state == Closed)
Gain change: 10 ms ramp via TrackMixer.mute_ramp (never a hard cut).
```

Hotkey wiring: `hotkeys` crate registers the toggle binding with `RegisterHotKey` (`Press`). For push-to-talk it installs a `WH_KEYBOARD_LL` hook, matches the binding on key down and key up (modifiers tracked from the same hook), posts `KeyDown`/`KeyUp`, and **does not swallow** the key (the key still reaches the game). Auto-repeat is filtered by the PushToTalk state.

### 5.8 Device state machine (per source)

```
           start()
  Idle ───────────► Opening ──ok──► Active
                       │              │
                  fail │              │ DEVICE_INVALIDATED / removed / disabled
                       ▼              ▼
                    Absent ◄──────── (device gone)
                       │
             retry timer (0.25, 0.5, 1, 2, 5, 5, ... s)
                       ▼
                  Reconnecting ──ok──► Active
                       │
                       └─ PermissionDenied → Absent(permission) (no retry until
                                              the next Settings/hotkey change)
  Stopped ◄── stop() from any state
  Unsupported ◄── process loopback requested on build < 19041 (terminal for this run)
```

On any transition into `Absent`, the source's ring simply stops receiving data, and the mixer's silence fill takes over at the next tick. On return to `Active`, the source resumes at the **current timeline index**; the gap is silence, never a timeline jump. A 20 ms crossfade is applied on the way in and out to avoid clicks.

### 5.9 Default-device change handling

```
on DefaultChanged(flow=Capture, role=Console or Communications, id):
   if mic.device == Default:
       debounce 150 ms (coalesce bursts)
       stop mic source; crossfade 20 ms
       start Capture(Default) with new endpoint
       emit DeviceChanged(reason="default_changed")

on DefaultChanged(flow=Render, role=Console, id):
   if mix track present:
       stop DeviceLoopback; start DeviceLoopback(Default)
   session_enumerator.on_default_render_changed()     # re-register notifications
   process captures unaffected

on Removed(id) for the currently used endpoint:
   source -> Absent(DeviceRemoved); retry logic 5.8
```

Note that `eCommunications` default changes are only used by the mic when the user selected "Communications default"; v1 uses `eConsole` for both mic and mix, which is the same device for most users. Documented in section 12.

### 5.10 Encoder fan-out and A/V anchor

```
per track T (encoder thread):
   pending: Vec<f32>
   on PcmBlock(T, pts, samples):
       if pts + frames <= anchor_index:      drop block       # before video anchor
       if pts < anchor_index:                trim head
       if first block and pts > anchor_index: prepend (pts - anchor_index) zeros
       packets = encoder.encode(samples, pts)
       for p in packets:
           p.pts_48k -= encoder.priming_samples()           # align decoded start to PCM start
           sink.on_packets(p)
flush at stop: encoder.flush() -> packets, sink
```

A/V offset is verified at the end of the first 10 s (`audio_first_pts − video_first_pts`); if |offset| > 20 ms it is logged as a warning in the session's metadata and surfaced in the library's session info.

### 5.11 Split (session part rollover)

```
split_at(keyframe_qpc):
   cut = sample_index(keyframe_qpc)
   Session writer finishes part N at packets with pts < cut.
   Packets with pts >= cut go to part N+1, starting with the packet containing `cut`.
   Accuracy: ±1 AAC packet (21.3 ms) or ±1 Opus packet (20 ms); the splitter prefers the packet boundary <= cut.
   The cut is recorded in each part's layout JSON as "split_offset_samples" for later concatenation.
```

### 5.12 Track-to-file mapping at session open

```
open_part(layout):
   tracks = layout.tracks.filter(enabled && source ok or silent-by-design) sorted by id
   for each track: add TrackEntry(name, codec, default=(id==1))
   add Tags(CLIPCHECK_AUDIO_LAYOUT = layout JSON, degraded flags included)
   every track must be present even if silent for the whole part (silence fill guarantees packets)
```

### 5.13 Windows version fallback

```
probe_os():
  build >= 19041 → process_loopback = true → normal path
  build <  19041 → process_loopback = false:
     Mix track               : normal (device loopback works on all supported builds)
     Mic track               : normal
     Game track              :
          if allow_degraded_game_track: DeviceLoopback(Default) labeled "2 Game (system mix fallback)",
                                         layout.fallback = GameTrackSystemMix
          else: silent, labeled "2 Game (unavailable)"
     game_excluded/app/catch :
          silent, labeled "<name> (unavailable: Windows < 2004)", fallback = AppTracksUnavailable
     UI: persistent banner "Per-app audio needs Windows 10 2004 or newer."
```

The fallback is decided once at pipeline start, recorded in the layout, and shown in the library and player. It is not retried mid-session.

---

## 6. Config keys owned by this feature

Stored in `%APPDATA%\ClipCheck\config.toml` (brief). Hot-reloadable: `apply_layout` for everything in sections `[audio]`, `[[audio.tracks]]`, `[[audio.routes]]`, `[mic]`; changes to `codec` or `bitrate_kbps` take effect on the **next session part** (encoders are not reconfigured mid-stream).

### 6.1 `[audio]`

| Key | Type | Default | Range / values | Hot-reload |
|---|---|---|---|---|
| `audio.enabled` | bool | `true` | — | next part |
| `audio.codec` | enum | `"aac"` | `"aac"` \| `"opus"` (opus forces MKV) | next part |
| `audio.bitrate_kbps` | u32 | `192` | 128–320, step 16 | next part |
| `audio.catch_all_track` | u8 | `0` | `0` (none) or 5–12 | live |
| `audio.catch_all_excludes_game` | bool | `true` | — | live |
| `audio.allow_degraded_game_track` | bool | `true` | Win10 < 2004 only | next session |
| `audio.max_process_captures` | u8 | `16` | 4–32 | live |
| `audio.session_poll_ms` | u32 | `1000` | 250–5000 | live |
| `audio.target_buffer_ms` | u32 | `100` | 40–400 | next part |
| `audio.drift_max_ppm` | u32 | `300` | 50–1000 | live |
| `audio.hard_resync_ms` | u32 | `40` | 10–500 | live |
| `audio.meter_hz` | u8 | `10` | 2–30 | live |

### 6.2 `[[audio.tracks]]` (array, max 12 entries)

| Key | Type | Default (for default entries) | Range / values |
|---|---|---|---|
| `id` | u8 | — (required) | 1–12, unique |
| `kind` | enum | — (required) | `mix` \| `game` \| `game_excluded` \| `mic` \| `app_group` \| `catch_all` (`catch_all` is written by the system from `audio.catch_all_track`; users set `app_group` + `audio.catch_all_track`) |
| `name` | string | see 2.1 | 1–48 chars, no control chars |
| `enabled` | bool | `true` | — |
| `gain_db` | f32 | `0.0` | −60.0 to +12.0, 0.5 dB step |
| `muted` | bool | `false` | — |
| `device` | string | `"default"` | `"default"` or endpoint id; used by `mix` (render) and `mic` (capture) only |

Constraints: at most one each of `mix`, `game`, `game_excluded`, `mic`, `catch_all`; `app_group` ids must not collide with the catch-all id.

### 6.3 `[[audio.routes]]` (array, max 64 entries)

| Key | Type | Default | Range / values |
|---|---|---|---|
| `exe` | string | — (required) | basename, 1–128 chars, lowercased on load, must end with `.exe` (case-insensitive) |
| `track` | u8 | — (required) | id of an `app_group` track |
| `enabled` | bool | `true` | — |

Default routes: `{ exe = "discord.exe", track = 4, enabled = true }`.

### 6.4 `[mic]`

| Key | Type | Default | Range / values | Hot-reload |
|---|---|---|---|---|
| `mic.ptt_mode` | enum | `"off"` | `"off"` \| `"push_to_talk"` \| `"toggle_mute"` | live |
| `mic.ptt_hotkey` | string | `""` | empty (none) or `Ctrl+Alt+M`-style (modifiers `Ctrl`, `Alt`, `Shift`, `Win`; one key from `A–Z`, `0–9`, `F1–F24`, `Pause`, `ScrollLock`) | live |
| `mic.ptt_release_tail_ms` | u16 | `150` | 0–500 | live |
| `mic.ptt_start_open` | bool | `false` | toggle mode starting state | next part |

### 6.5 Settings doc hand-off

The Settings doc should expose: codec, bitrate, catch-all track, track names/gains/mutes, routes editor, mic device, PTT mode and hotkey. Advanced (hidden under "Advanced audio"): `target_buffer_ms`, `drift_max_ppm`, `hard_resync_ms`, `max_process_captures`, `session_poll_ms`, `meter_hz`.

---

## 7. IPC messages / events

Transport and framing per brief: named pipe `\\.\pipe\clipcheck-ipc`, length-prefixed JSON. Shapes below are serde `{ "type": ..., "data": ... }` externally tagged.

### 7.1 Commands (UI → service)

| Message | Payload | Reply | Notes |
|---|---|---|---|
| `audio.get_layout` | `{}` | `AudioLayout` | Current effective layout |
| `audio.apply_layout` | `AudioLayout` | `Ok` or `LayoutError` | Validated; live-applied where allowed (6) |
| `audio.set_track_gain` | `{ id: u8, gain_db: f32 }` | `Ok` | Live, 10 ms ramp |
| `audio.set_track_mute` | `{ id: u8, muted: bool }` | `Ok` | Live, 10 ms ramp |
| `audio.set_mic_mute` | `{ muted: bool }` | `Ok` | UI mute on the mic track |
| `audio.list_devices` | `{ flow: "render" \| "capture" }` | `Vec<DeviceInfo>` | Used by pickers |
| `audio.list_audio_apps` | `{}` | `Vec<{exe, pid, active}>` | Currently running audio sessions, for the route picker |
| `audio.set_mic_device` | `{ device: "default" \| id }` | `Ok` | Live re-attach |
| `audio.get_status` | `{}` | `PipelineStatus` | Source/track states |

### 7.2 Events (service → UI)

| Event | Payload | Rate / trigger |
|---|---|---|
| `audio.meters` | `{ tracks: [{ id, peak_dbfs, rms_dbfs }] }` | `meter_hz` (default 10 Hz) while recording |
| `audio.source_state` | `{ source, kind, state, reason? }` | On transition |
| `audio.device_changed` | `{ flow, role, device, reason }` | On default/removal/add |
| `audio.route_matched` | `{ exe, pid, track }` | On new routed session |
| `audio.route_shadowed` | `{ exe, pid, by_pid }` | On nested conflict |
| `audio.drift_corrected` | `{ source, ppm, offset_ms }` | Every 10 s if \|ppm\| > 20 |
| `audio.hard_resync` | `{ source, offset_ms }` | Rare; logged |
| `audio.fallback_active` | `{ mode }` | Once at start when degraded |
| `audio.mic_state` | `{ open, reason }` | On PTT or mute change (`ptt`, `ui`, `hotkey_toggle`) |
| `audio.error` | `{ code, message }` | Non-fatal pipeline errors |

The UI subscribes on connection; the service sends `audio.meters` only to connected subscribers and drops them when the UI disconnects (no meter cost when the UI is closed).

### 7.3 Cross-crate events

- To `session`: `AudioEvent::Error` escalates to a session warning; the session continues (video is independent).
- To `library`: at finalize, layout and per-track summary (`audio_tracks` rows, section 4.4 of library — see 8.x) are written.
- From `hotkeys`: `PttInput` events (`KeyDown`, `KeyUp`, `Press`).

---

## 8. Edge cases & failure modes

| # | Case | Behavior | Detection / test |
|---|---|---|---|
| E1 | Game exits and relaunches (new PID) | Game track retargets to the new PID; the old capture is stopped; a 500 ms gap is silence. Game PID comes from `detect`. | Test with a relaunch script |
| E2 | Game spawns a child process that makes the audio (launcher → game) | INCLUDE-tree of the detected PID covers children. If the audio-producing process is a sibling, Game track is silent: documented; `detect` should report the audio-owning tree when known. | Known limitation; "Game track silent" diagnostic in the UI |
| E3 | Browser or Electron app (many renderer/utility children) | Process tree INCLUDE covers children; the ancestry dedupe prevents double capture. | Test with Chrome + YouTube |
| E4 | Exclusive-mode WASAPI stream (some games, pro audio) | No loopback can see it. Track silent. UI shows "exclusive-mode audio not capturable" hint when a session reports exclusive state (if detectable) or when the Game track is silent for > 5 s while the game is audible on the mix. | Manual test with an exclusive-mode test app |
| E5 | Protected content (DRM video in browser) | Loopback returns silence for protected audio. Not fixable; documented. | Manual |
| E6 | Microphone privacy off (Settings > Privacy > Microphone, or "Let desktop apps access your microphone" on Win11) | `Initialize` returns `E_ACCESSDENIED`; source → `Absent(PermissionDenied)`; UI banner with a link to the Windows setting (opened by the UI, not changed by us). Retried only on next start or settings change. | Toggle the privacy setting in test |
| E7 | User changes Windows per-app volume during recording | Process tracks are unaffected (pre-endpoint); the Mix track follows it. Documented in Settings tooltip. | Test: mute Discord in mixer, Chat track still records |
| E8 | App plays on a non-default endpoint (e.g. headset vs speakers) | Process loopback is endpoint-independent, so per-app tracks still capture it. Session enumeration uses the default endpoint, so the app may not be discovered by session notification. Mitigation: exe-only matching (5.5) for rule apps. Catch-all may miss unrouted sessions on non-default endpoints. | Test with two endpoints |
| E9 | Default render device changes mid-recording | Mix re-attaches with 20 ms crossfade; session notifications re-registered; no gap in file timeline beyond the crossfade. | Test: switch headset ↔ speakers |
| E10 | USB mic unplugged mid-session | Mic track silent within one tick; device retry; auto re-attach; `audio.source_state` events. | Physical unplug test |
| E11 | Sample-rate change (44.1 ↔ 48 kHz device) or bit-depth change on re-open | Source re-opens, resampler rebuilt; timeline continuous. | Test by changing the device's format in Sound settings |
| E12 | Low-level keyboard hook timed out (system drops hook if callback > ~300 ms) | The callback only posts to a channel; if the hook is removed by the system, `hotkeys` re-installs it and emits `audio.error`; PTT falls back to Closed (safe default: no leaked speech). | Stress test: busy UI thread |
| E13 | Mic PTT key stuck down (missed key-up on focus change) | Release tail still applies; a 30 s watchdog closes the PTT if no KeyUp after a KeyDown with the key not physically down (`GetAsyncKeyState` check). | Test: hold key, alt-tab |
| E14 | Process loopback returns QPC position 0 | Fallback timestamp = QPC at wake − buffer latency; drift controller uses the fallback; logged once. | Unit test with a fake client |
| E15 | Process loopback on Windows build < 19041 | Fallback (5.13). Never attempted. | Test on a VM with an old build image (manual) |
| E16 | System sounds / notification sounds | Appear only on Mix (session pid 0 / audiodg.exe are excluded from per-app routing). | Test: notification sound in Mix only |
| E17 | ClipCheck's own audio (e.g. UI notification beep) | Excluded from `catch_all` and app routing via own-tree check; present in Mix (device loopback cannot exclude). Documented. | Test |
| E18 | Rule targets a track that is later disabled | Rule is kept but inactive; validation warns in Settings; apps unrouted go to catch-all (if any) or Mix only. | Unit test |
| E19 | Too many audio sessions (> `max_process_captures`) | Oldest unclaimed sessions are skipped; `audio.error(cap)`; catch-all marked `partial` in the file's layout JSON. | Stress test with 20 apps |
| E20 | Clock drift huge (USB device with bad clock, ~1000+ ppm) | Trim clamps at `drift_max_ppm`; residual drift triggers a hard resync every few minutes; logged. The clamp limits pitch change to ~0.1 cent. | Simulated 1000 ppm source |
| E21 | Device stall (no data for > 500 ms, device still present) | Silence fill; source flagged `stalled`; after 2 s, device is re-opened. | Simulated by pausing a driver stream |
| E22 | Session split during active rule switch | Routing changes apply at the next tick after the cut; the split does not wait for them. | Unit test with interleaved events |
| E23 | Layout JSON exceeds container tag size | Tag stores compact JSON capped at 8 KB; if larger (should not happen, ≤ 12 tracks and 64 routes), route list is truncated with `"routes_truncated": true`. | Unit test |
| E24 | Opus chosen with MP4 container (user config mismatch) | Validation error `OpusNeedsMkv` at apply time; session start uses MKV automatically and emits a warning. | Unit test |
| E25 | Track with no source ever (e.g., Mix disabled) | Track is not written; the layout JSON lists it as `enabled: false`. Track numbering in the file stays in layout-id order with gaps allowed. | Unit test |
| E26 | Mic yields 8-bit / 24-bit / 32-bit int formats | Converted to f32 in `format.rs`; unknown formats → `FormatUnsupported`; the source tries the engine's mix format and then 48k float as a last resort. | Format matrix test |

---

## 9. Performance & resource budget

Budgets are per recording session on the reference machine (6-core, 2.5 GHz class, 16 GB). Numbers are design targets to be validated in task A11.

### 9.1 Targets

| Metric | Target | Notes |
|---|---|---|
| CPU, audio pipeline total (6 tracks, 4 sources, AAC 192k) | ≤ 0.8 % of one core | Part of the brief's global < 3 % budget |
| CPU, idle (no game running; pipeline not started) | 0 % (no threads except device monitor callbacks) | Pipeline starts only when recording is active |
| CPU, one process loopback client | ≤ 0.05 % of one core | Mostly WASAPI event wake-ups |
| CPU, one resample (44.1k → 48k stereo sinc) | ≤ 0.1 % of one core | rubato, sinc length 128 |
| CPU, one AAC-LC 192 kbps encoder | ≤ 0.1 % of one core | libavcodec native `aac` |
| CPU, Opus 192 kbps encoder | ≤ 0.1 % of one core | libopus |
| CPU, session poll (1 Hz) | ≤ 0.01 % | Toolhelp snapshot + session enumeration |
| Memory, per source ring | 500 ms × 48 kHz × 2 ch × 4 B = 192 KB | Fixed |
| Memory, total audio (16 sources, 12 tracks) | ≤ 12 MB | Rings + encoders + resamplers |
| Latency, capture → encoder input | ≤ target_buffer_ms (100 ms default) + 20 ms | Configurable down to 40 ms |
| Latency, capture → file (on disk) | ≤ 2 s (fMP4 segments / MKV cluster flush) | Matches the session's crash-loss bound |
| Disk, audio bitrate (6 tracks, 192k) | 1.44 MB/min per track; ≈ 8.6 MB/min total | Track cache for player is extra, LRU 512 MB |
| Meters | ≤ 0.02 % CPU at 10 Hz | Peak/RMS computed in mixer; IPC only when UI subscribed |

### 9.2 Constraints that drive the design

- No allocation on the audio thread after start: rings and scratch buffers are preallocated; resamplers reuse buffers.
- Mixer tick never blocks on a source lock (SPSC rings, atomics for indices).
- Encoder threads are separate so an encoder stall cannot underrun capture; a full encoder queue drops **video-side** nothing and instead increases `encoder_backlog` and logs; if backlog > 1 s, the encoder skips to the current timeline with a logged discontinuity (should not happen with HW video encode).

---

## 10. Test plan

### 10.1 Unit tests (`cargo test -p clipcheck-audio`, run on Windows and Linux where possible)

| Area | Test | Pass criterion |
|---|---|---|
| Layout validation | Every LayoutError variant triggered | Exact error variant |
| Layout defaults | `default_layout().validate()` ok; ids 1–4 default names | Snapshot |
| Router | First-match-wins; catch-all fallback; game excluded from catch-all; rule to mix/game/mic rejected | Table tests |
| Router shadowing | Child routed to different track than ancestor | RouteShadowed produced |
| Mixer | Gain ramp 5 ms; mute ramp 10 ms; no zipper (max sample-to-sample delta within bound) | Delta ≤ threshold |
| Mixer silence fill | Source missing for 3 ticks | Exact zeros, `silence_frames` = 3×480 |
| Ring | Out-of-order writes placed by timeline index | Output equals reference |
| Resampler | 44.1k sine 1 kHz → 48k: THD+N and frequency error | Error < 0.01 %, THD+N < −90 dB |
| Resampler trim | `set_trim_ppm(+200)` changes output rate by 200 ppm ± 2 | Measured within bound |
| Drift estimator | Synthetic source at +150 ppm, +1000 ppm, −300 ppm | Estimated ppm within 5 ppm after 10 s |
| PI controller | Step fill error 50 ms | Settles < 20 ms within 30 s; no overshoot > 10 ms; trim clamped |
| Hard resync | Injected 60 ms offset with threshold 40 ms | Single resync, offset ≤ 2 ms after |
| Timeline mapping | QPC ↔ sample index round trip over 10 h | Error ≤ 1 sample |
| PTT | All transitions in 5.7, including Tail cancel and auto-repeat | Sequence matches table |
| Soft clip | Input 2.0 → output < 1.0; input 0.5 → unchanged within 0.5 % | Bounds |
| Format conversion | i16, i24-in-32, i32, f32 at 1-, 2-, 6-channel | Max abs error ≤ 1 LSB |
| Layout JSON | Serialize → parse → equal; size ≤ 8 KB for max config | Round trip |
| Container tags | MKV tags written and re-read by the library indexer | Layout equal |
| Fallback | build 18363 → process_loopback false; all routing degraded per 5.13 | Labels and flags |

### 10.2 Windows integration tests (self-hosted runner, real audio engine)

| Test | Method | Pass criterion |
|---|---|---|
| Process INCLUDE isolation | Test harness plays a 440 Hz tone from process A and 880 Hz from process B (both `ffplay`-like tiny exes built for test) | Track for A contains 440 only (FFT peak ratio > 40 dB) |
| EXCLUDE isolation | Same two processes; `game_excluded` track | Contains 880 only |
| Tree INCLUDE | Parent launches child that plays tone; INCLUDE parent | Tone captured |
| Mix track completeness | Both tones playing | Mix contains both |
| Mic round trip | Loopback cable (virtual) from test tone into mic input | Mic track contains tone; latency reported ≤ 100 ms + 20 ms |
| Rule hot-add | Start with rule for app X; start app X mid-recording | Track gets tone within 1.5 s of audio start |
| Rule hot-remove | Stop app X | Track silent within 1.5 s |
| Default device change | Switch default render device during recording | Mix continues; no gap > 40 ms; crossfade present |
| Mic unplug/replug | Disable device in Sound settings for 10 s | Silence during, resumes after; `source_state` events in order |
| Default capture change | Change default mic | Track re-attaches within 300 ms |
| Privacy denied | Turn off mic access | `PermissionDenied`, banner; no crash |
| Sample rate change | Change device rate 48k → 44.1k | Continuous timeline; resampler rebuilt |
| Drift long run | 2 h recording with a test source at +200 ppm (simulated via a virtual device) | Final A/V offset ≤ 20 ms; hard resyncs = 0 |
| A/V sync | Clapper: flash + click at 1 Hz for 60 s with the WGC capture of a test window | Measured offset ≤ 20 ms (p95) |
| PTT hold | Hold hotkey 2 s with tone playing on mic loopback | Mic on track only during hold + tail (± 20 ms) |
| PTT toggle | Press twice | Mute state flips twice; recorded silence matches |
| Build fallback | Run on Windows 10 1909 VM | Degraded labels, Mix works, no panic |
| Many sessions | 16 test apps playing | 16 process captures, cap behavior at 17 |

### 10.3 Container and playback tests

| Test | Method | Pass criterion |
|---|---|---|
| Track names | `ffprobe -show_streams` on produced MKV | Names equal layout |
| Default flag | ffprobe disposition | Track 1 default only |
| Opus in MKV | Opus codec; WebView2 play via `clip://` | Plays track audio |
| Per-track extraction | `clipedit` extracts track 3 | Only that stream; duration equal to session |
| Player sync | Web Audio graph with 4 tracks, 30 min video, seek 20× | Max desync ≤ 30 ms after seek; ≤ 300 ms hard-seek threshold |
| Cache eviction | Fill to 512 MB | LRU eviction; re-generation works |
| Missing track cache | Delete cache file mid-play | Regenerated on next request; no crash |

### 10.4 Soak and performance

| Test | Pass |
|---|---|
| 4 h session, 6 tracks, 4 sources | No underruns > 10 per hour; RSS growth ≤ 5 MB; no handle leaks (Process Explorer count stable) |
| CPU measurement with PresentMon-style sampling | Within budget 9.1 |
| Repeated start/stop 100× | No leaked threads or COM objects |

---

## 11. Implementation task breakdown

Ordered; each task is roughly 0.5–2 days. Tasks A1–A3 are the spike that de-risks Windows APIs before anything else depends on them.

| # | Task | Output | Days | Depends |
|---|---|---|---|---|
| A1 | **Spike: process loopback.** Standalone exe: `ActivateAudioInterfaceAsync` with `VAD\Process_Loopback`, INCLUDE and EXCLUDE, float32 48k stereo init with `AUDCLNT_STREAMFLAGS_LOOPBACK \| EVENTCALLBACK \| AUTOCONVERTPCM \| SRC_DEFAULT_QUALITY` (verify flags); confirm QPC position behavior; confirm Chromium tree INCLUDE. Record findings in the doc's open questions. | Spike notes, minimal sample | 1 | — |
| A2 | Spike: device loopback + mic capture + `IMMNotificationClient` + `IAudioSessionManager2` notifications on Win11 and Win10 2004 VM. | Spike notes | 1 | — |
| A3 | Spike: rubato dynamic-ratio resampler API and quality/latency at 44.1 k and 96 k inputs. | Benchmark numbers in section 9 | 0.5 | — |
| A4 | `format.rs` + `ring.rs` + `clock.rs` (QPC conversion, timeline index, unit tests). | Core primitives with tests | 1.5 | A1 |
| A5 | `source/device_loop.rs` + `source/capture.rs` (event-driven capture thread, conversion, resample, state machine 5.8). | Two working sources | 2 | A2, A3, A4 |
| A6 | `source/process_loop.rs` (activation completion handler, INCLUDE/EXCLUDE, fallback probe `fallback.rs`). | Process sources | 2 | A1, A4 |
| A7 | `mixer.rs` + `TrackMixer` silence fill, gains, meters, soft clip; mixer tick thread (5.2). | Mixed tracks to a test sink | 2 | A4, A5 |
| A8 | `layout.rs` + `router.rs` + validation + default layout + TOML mapping (section 6). | Layout API, router, tests | 1.5 | — (parallel) |
| A9 | `sessions.rs` + `ProcessSnapshot` + session poll + dedupe + dynamic attach/detach (5.5), catch-all. | Dynamic routing | 2 | A6, A8 |
| A10 | `devices.rs` (DeviceMonitor, default change, reconnect backoff 5.8/5.9). | Hot-plug working | 1.5 | A5 |
| A11 | Drift estimator, PI controller, hard resync (5.4); soak harness. | Drift test pass | 2 | A7 |
| A12 | `encode.rs` trait + `encode::audio::FfmpegAudioEncoder` (AAC, Opus) + priming compensation + fan-out (5.10). | Encoded packets | 2 | A7 (encode crate owner) |
| A13 | `ptt.rs` + `hotkeys` integration (RegisterHotKey toggle; WH_KEYBOARD_LL push-to-talk; watchdog E13). | PTT working | 1.5 | A8, hotkeys owner |
| A14 | Session writer integration: MKV tracks, names, default flag, layout tags, split at keyframe (5.11, 5.12); fMP4 ring tracks in `buffer`. | Playable multi-track MKV | 2 | A12, session/buffer owners |
| A15 | `ipc` messages (7.1–7.2) and meter throttling; `AudioEvent` mapping. | IPC complete | 1 | A7, ipc owner |
| A16 | Library: `audio_tracks` table + migration + indexer reads layout tags (section 4.4 of library). | Gallery shows track info | 1 | A14 |
| A17 | `clipedit` per-track audio-only remux + `clip://track/<n>` handler + LRU track cache. | Per-track playback source | 1.5 | A16, clipedit owner |
| A18 | Player: `<video muted>` + per-track `<audio>` + Web Audio mixer, sync loop (3.7), track panel UI. | Multi-track player | 2 | A17, ui owner |
| A19 | Settings UI: track table, routes editor with running-app picker, device pickers, PTT capture control, degraded banners. | Settings done | 2 | A15, settings owner |
| A20 | Test suite: unit (10.1), integration harness (10.2), container (10.3), soak (10.4). | CI green on Windows runner | 2 | all |

Critical path: A1 → A4 → A6 → A9 → A14 → A18. Parallel: A8 can start on day 1.

---

## 12. Open questions

These are decisions in this doc that differ from, or extend, the brief, or depend on spike results. Each needs a yes/no from the owner before implementation of the related task.

1. **MKV as default multi-track container (1.4 #1).** Brief lists MKV/MP4 without a default. Proposal: MKV for all sessions and clips with audio. Confirm, and confirm that MP4 is offered only for AAC-only, single-user-facing exports.
2. **Catch-all via session enumeration (5.6).** Costs one process loopback per unclaimed audio app and needs `max_process_captures` (default 16). Alternative: drop catch-all in v1. Confirm.
3. **Exact process-loopback flags and QPC behavior (A1).** The init flags and whether `GetBuffer`'s QPC position is valid for process loopback must be verified; section 5.3 has a fallback but accuracy claims depend on it.
4. **Push-to-talk via `WH_KEYBOARD_LL` (1.4 #4, 5.7).** Needs key-up. Risk: hook timeouts and anti-cheat software that flags low-level hooks. Confirm we accept this in the `hotkeys` crate, and whether to offer a `GetAsyncKeyState` polling mode (10 ms) as the alternative (no hook, but polling cost ~0.05 % CPU).
5. **Player multi-track approach (3.7).** Muted `<video>` + per-track `<audio>` via `clip://track/<n>` with a lazily generated cache (≈1.4 MB/min per track). Alternative: store per-track sidecar files at record time (more disk, simpler player). Confirm.
6. **Game track on Win10 < 2004 (5.13).** Default is system-mix fallback labeled as such. Alternative: silent. Confirm; the choice affects whether the Game track is ever a lie on old builds.
7. **Mic device role (5.9).** v1 uses `eConsole` for both mic and mix. Should the mic follow `eCommunications` (Discord/Teams default) for users who set a separate communications device? Confirm.
8. **Limiter (4.6).** v1 uses a soft-clip guard only. A look-ahead peak limiter on the Mix track would prevent clipping in loud games. Add in v1 or later?
9. **Mic channel handling (2.4).** Dual-mono stereo for mono mics. Alternative: store mono tracks (cheaper, but breaks "48 kHz stereo" in brief). Brief says stereo; this doc keeps stereo. Confirm.
10. **`game_excluded` track (2.2).** Not in the brief. It is cheap and useful (non-game audio separated without rules). Confirm adding it as a track kind.
11. **Audio from non-default endpoints (E8).** Process tracks are endpoint-independent, but session discovery is per default endpoint. Accept the limitation for catch-all, or enumerate all render endpoints (more COM work, up to ~5 enumerators)?
12. **Session audio-owner detection for launcher-style games (E2).** Should `detect` report the audio-owning process tree (for example, by inspecting which process holds an active session), or is the detected PID enough? Needs coordination with the `detect` owner.
13. **Brief numbers to verify.** Brief says 128–320 kbps per track and "AAC-LC default or Opus (MKV)". Table 6.1 uses 192 kbps default, which is a deviation from nothing in the brief but a choice that should be confirmed for the Settings doc.
14. **Track-cache location and size (3.7 #6).** `%LOCALAPPDATA%\ClipCheck\trackcache`, 512 MB LRU. Confirm this is acceptable relative to the brief's `buffer` directory conventions.
