# 08 — In-app Player and Clip Editor (`clipedit`)

Status: Draft v1
Owners: UI (`apps/ui`), `clipedit` crate, `library` crate (sprite sheets)
Depends on: `00-brief.md` (process model, naming, config locations), session writer and encode docs (keyframe interval, container choice), library doc (thumbnails, DB writes)

---

## 1. Goal & scope

### 1.1 Goal

Let the user watch any ClipCheck recording (clip or full session) inside the Tauri UI, and turn any recording into a shorter clip without leaving the app. Two surfaces, one shared media stack:

- **Player**: HTML5 `<video>` fed by a `clip://` custom protocol with HTTP range support, plus Web Audio mixing for multi-track audio, hover-thumbnail scrubbing, and keyboard-first controls.
- **Clip shortener** ("Medal-style trimmer"): a filmstrip timeline with start/end handles, live preview of the selection, lossless keyframe-snapped stream-copy trim (default), optional precise re-encode, progress and cancel, and save-as-new or overwrite.

The `clipedit` crate is the file-level engine behind the editor: probe, keyframe index, stream-copy trim, precise re-encode, faststart remux, repair, audio-track extraction, and playback proxy generation. It never touches live capture.

### 1.2 In scope

| Area | In | Out (this doc) |
|---|---|---|
| Playback transport | play/pause, scrub, seek step, speed 0.25x–2x, frame step, fullscreen | Picture-in-picture, casting, chapters |
| Video delivery | `clip://` protocol, range requests, allowlisted IDs only | Streaming from network or other machines |
| Codec support | H.264 and HEVC direct play when WebView2 can decode; H.264 proxy fallback | AV1 proxy (AV1 direct play allowed if WebView2 reports support, no extra work) |
| Audio | Per-track audible toggle, solo, per-track gain, mixing via Web Audio, sidecar audio extraction | Audio editing, loudness normalization baked into files |
| Hover thumbnails | Sprite-sheet lookup, canvas fallback | Per-frame preview at 60 fps |
| Clip shortener | Filmstrip handles, live preview, lossless trim, precise trim, progress, cancel, save-as-new / overwrite | Multi-segment timeline, cut-and-join, transitions, captions, effects |
| Session to clip | Trim a full session part into a new clip row linked to the session | Auto-highlight detection (v2) |
| Audio in output | All audio tracks preserved bit-for-bit (stream copy) in lossless mode | Track removal in output (v2) |

### 1.3 Explicit non-goals

- No capture, encode of live recordings, buffer, or detection code. The editor's precise mode reuses the `encode` crate's encoder selection only.
- No cloud upload, sharing links, or social features.
- No editing of metadata inside the media file (title tags, chapters). Titles live in `recordings` only.
- No frame-accurate lossless cuts. Lossless means keyframe-snapped; precise mode exists for exact cuts.

---

## 2. User-facing behavior (precise)

### 2.1 Opening a recording

- Double-click a row in the gallery or press Enter on a selected row → **Player view** for that recording.
- Player opens at the last saved position for that recording, unless that position is within 5 s of the end of the file, in which case it opens at 0. Position is stored in the library DB table `playback_state` (`recording_id`, `position_ms`, `updated_ms`); the UI reads it through the read-only handle and writes through the service (`player.position` command, debounced 5 s, flushed on close). This is the only player state that lives in the DB; preferences live in config (Section 6).
- Playback starts paused. Autoplay is off by default. First user gesture unlocks Web Audio.
- Opening a file that is still being written (live session part) shows a **LIVE** badge; the duration grows as `RecordingUpdated` events arrive (Section 7). Seeking past the currently written end is clamped to the last known keyframe.

### 2.2 Transport and controls

| Control | Behavior |
|---|---|
| Play/Pause | Toggles. At end of file, Play restarts from 0. |
| Scrub bar | Click jumps; drag scrubs. Scrub while playing pauses playback during the drag and resumes afterwards if it was playing. Seek is applied on pointer-up; during drag the preview frame is the hover thumbnail, not the video element (avoids seek storms on multi-hour files). |
| Hover thumbnail | While the pointer is over the scrub bar, a 160x90 thumbnail and timecode float above the pointer. Source: sprite sheet (Section 3.4). Latency target under 16 ms. |
| Time display | `HH:MM:SS / HH:MM:SS` in local timezone-free media time. Elapsed time is media time, not wall time. |
| Seek step | Left/Right = -5 s / +5 s (configurable `player.seek_step_s`). J/L = -10 s / +10 s. Holding a key repeats at 4 Hz. |
| Frame step | `,` and `.` step one frame back/forward while paused. Frame duration = 1 / average frame rate from probe. Stepping while playing pauses first. Rapid presses coalesce: a new step is queued while a seek is in flight and issued on `seeked`. |
| Speed | `<` / `>` cycle 0.25, 0.5, 0.75, 1, 1.25, 1.5, 2. Pitch is preserved (`preservesPitch = true`). Speed is per-session; default from `player.default_speed`. |
| Volume | Slider 0–100 %, Up/Down ±5 %. Applies to master gain (Web Audio path) or `video.volume` (single-track direct path). Volume is persisted to `player.volume` on change (debounced). |
| Mute | `M` toggles. Mute state persisted to `player.muted`. Mute is independent of volume: unmuting restores the prior volume. |
| Fullscreen | `F` or double-click on video. `Esc` exits fullscreen first, then nothing else. Controls auto-hide after 2.5 s of pointer idle in fullscreen. |
| Audio tracks | Button opens the track popover (2.4). |
| Clip | Opens the Clip shortener (2.5) with the current position as the initial in-point; the out-point defaults to end of file. |

Keyboard shortcuts are active only when the player or editor has focus. They are never global hotkeys (the brief's global hotkey registry is for recording only).

| Key | Action |
|---|---|
| Space or K | Play/pause |
| J / L | Seek -10 s / +10 s |
| Left / Right | Seek -5 s / +5 s (`player.seek_step_s`) |
| Shift+Left / Shift+Right | Seek -1 s / +1 s |
| `,` / `.` | Frame step back / forward |
| `<` / `>` | Speed down / up |
| Up / Down | Volume +5 / -5 % |
| M | Mute toggle |
| F | Fullscreen toggle |
| Esc | Exit fullscreen |
| 0–9 | Jump to 0 %–90 % of duration |
| Alt+1 … Alt+9 | Toggle audible for audio track 1–9 |
| Shift+Alt+1 … Shift+Alt+9 | Solo track 1–9 (solo overrides toggles until released) |
| C | Open Clip shortener at current position |
| I / O (editor) | Set in-point / out-point at playhead |
| Ctrl+Enter (editor) | Save as new clip |
| Ctrl+Shift+Enter (editor) | Overwrite original (confirm dialog) |

### 2.3 Codec fallback behavior

- On open, the UI runs a **direct-play check** (Section 5.1) against the recording's probed codec, profile, bit depth, and resolution.
- If direct play is supported: the `clip://rec/<id>` source is used.
- If not supported and `player.proxy_policy = auto`: a **Preparing playback** banner appears with a progress bar and an ETA; the H.264 proxy is generated in the background (Section 5.6). Playback starts automatically when the proxy has at least 10 s buffered ahead of the playhead, or when it finishes.
- `proxy_policy = never`: the player shows a "Codec not supported by WebView2 (HEVC Video Extensions missing)" message with a link to the Microsoft Store listing for the HEVC Video Extensions, and offers "Open in default app" (launches Windows' default player via shell with the original file).
- `proxy_policy = always`: proxy is used even when direct play would work (for weak GPUs).
- Trimming always reads the original file, never the proxy. The editor shows a small "Original" / "Proxy" tag in the status bar so the user knows which source is on screen.

### 2.4 Audio track popover

```
 Audio tracks                                      [x]
 ------------------------------------------------------
  [x] 1  Mix (game + all apps)    AAC  stereo  48k   S
  [ ] 2  Game only                AAC  stereo  48k   S
  [ ] 3  Microphone               Opus stereo  48k   S
  [ ] 4  Chat / Discord           AAC  mono    48k   S
  [ ] 5  Music                    AAC  stereo  48k   S
 ------------------------------------------------------
  Gain   1: [=====|----] 0 dB      (per track, -20..+6 dB)
  Note: this changes preview playback only. The file is not changed.
```

- `[x]` = audible in preview. `S` = solo button (Shift+Alt+digit). Track 1 is audible by default; all others are off.
- Tracks are labelled from `StreamInfo.title` (stored in MKV/MP4 metadata written by the session writer) falling back to the default layout names in the brief (Section "Audio policy"). Unknown tracks are named `Track N`.
- Audibility, solo, and gain are preview-only. They are not written to the file. Lossless and precise trims always keep all tracks at their original gain.
- Preview state is kept per recording for the session (not persisted), so reopening the same recording keeps the last mix.

### 2.5 Clip shortener (editor)

Opens as a full-width view replacing the player's transport; the video frame stays at the top.

Behavior:

1. **Filmstrip**: a row of N cells (`editor.filmstrip_cells`, default 40) across the full duration. Each cell is a still from the sprite sheet nearest its midpoint. Cells are generated from sprites; no extra decoding is needed when sprites exist.
2. **Handles**: a green in-handle and a red out-handle drag along the filmstrip. Dragging updates the time label above the handle (`HH:MM:SS.mmm`). The selected range is shaded. Keyboard: I and O set handles at the playhead; `[` and `]` nudge the focused handle by one frame; Shift+`[`/`]` by 1 s.
3. **Keyframe ticks** (lossless mode only): thin ticks on the timeline mark keyframes from the keyframe index (Section 5.2). Handles snap to the nearest tick when within 8 px, with a Shift-drag override to disable snap.
4. **Snap readout**: below the timeline, a line states the actual output range in lossless mode:
   `Requested 00:12:04.200 – 00:12:31.900   Output 00:12:02.000 – 00:12:31.900 (start snapped back 2.2 s)`
   Start always snaps backward to the preceding keyframe so the first requested frame is never cut. End is packet-accurate (last packet with PTS below the out-point).
5. **Live preview**: playhead loops over the selection. `Space` plays the selection; playback loops at out-point back to in-point. Preview uses the same source as the player (original or proxy). In lossless mode the preview range is the snapped output range so what you see is what gets saved. A "Preview requested range" toggle disables snapping for preview only.
6. **Mode selector**:
   - **Lossless (stream copy)** (default): fast, no quality loss, keyframe-snapped. Applies `editor.default_mode`.
   - **Precise (re-encode)**: decodes from the in-point, re-encodes video (exact cut). Encoder and CRF from config. Audio is stream-copied for all tracks, so audio cut is packet-accurate.
   Switching modes re-renders the snap readout and keyframe ticks.
7. **Output**:
   - **Save as new clip** (default, Ctrl+Enter): writes a new recording next to the original, title `{game} {time} clip-trim`, linked to the source via `parent_id`. Original is untouched.
   - **Overwrite original** (Ctrl+Shift+Enter): replaces the source file in place. The user sees a confirm dialog every time unless `editor.confirm_overwrite = false`. The original goes to the Recycle Bin (recoverable), not deleted. Refused if the source is a live session part or is referenced by an active job.
   - Output file name follows the template `{game} {yyyy-MM-dd HH-mm-ss} {kind}` with `kind = clip-trim` (new kind, see Open questions).
8. **Progress panel** (replaces the Save button area while running):

```
 Exporting clip-trim (lossless)                          [Cancel]
 [##########################----------------]  58 %   ~6 s left
 Written 412 MB   Speed 1.8 GB/min   Keyframe snap: yes
```

- Progress is written output time over requested duration, updated at ~4 Hz.
- Cancel kills the job within 500 ms, deletes the temp file, and returns to the editor with the selection intact.
- On completion the new row appears in the gallery (via `RecordingAdded`) and a toast offers "Open" and "Show in folder".

### 2.6 Session to clip

- The editor opens from a session part (`kind = session-part1` etc.) with the same UI. The output row gets `kind = clip-trim` and `parent_id = session_part_id`.
- A session part of 3 hours defaults the filmstrip to 40 cells (each about 4.5 min). The timeline supports zoom: mouse wheel over the filmstrip zooms around the pointer, down to 10 s per screen width. When zoomed, the filmstrip cells are re-sampled from the sprite sheet at a finer interval if available, otherwise from the nearest sprite.
- The editor may also jump to a "Go to time" dialog that accepts `hh:mm:ss`, `mm:ss`, or `ss`.

### 2.7 Error and empty states

| Situation | Shown |
|---|---|
| File missing on disk | Player shows "This recording's file is missing" with Locate… (file picker) and Remove from library (requires confirm; service marks row missing, does not delete file). |
| Probe failed | "Could not read this file" with the error code and "Try repair" (faststart/moov repair, Section 5.7). |
| No video stream | Opens audio-only view (waveform placeholder, same controls). |
| Single audio track | Audio button hidden; mixer not created. |
| Insufficient disk space for job | Refused before starting: estimated output size (lossless ≈ source bitrate × duration × range fraction) compared against free space + 10 % margin. |

---

## 3. Architecture & data flow

### 3.1 Process and crate placement

```
 +-------------------- clipcheck (UI process, Tauri v2) --------------------+
 |                                                                          |
 |  web/ (Svelte + TS)                    src-tauri/ (Rust)                 |
 |  +----------------------+              +---------------------------------+ |
 |  | Player.svelte        |  invoke()    | commands::edit::*               | |
 |  | Editor.svelte        | ----------->| commands::player::*             | |
 |  | WebAudioMixer.ts     |              | protocol::clip  (clip:// URI)   | |
 |  | <video> <audio>x N   |<-- clip:// --| jobs::JobRunner (threads)       | |
 |  +----------------------+  range GET   +----------------+----------------+ |
 |                                                         |                 |
 |                                   links clipedit, library (read-only)     |
 +---------------------------------------------------------|-----------------+
                                                           v
   +------------------- clipedit (lib) ---------------------------------------+
   |  probe.rs  keyframes.rs  trim.rs  precise.rs  remux.rs  repair.rs        |
   |  audio_extract.rs  proxy.rs  cancel.rs  progress.rs   uses encode::ffmpeg |
   +--------------------------------------------------------------------------+

 +------------------- clipcheckd (service) -----------------------------------+
 |  owns library.db write handle, sprite generation scheduler, file watcher   |
 |  receives RegisterEditedRecording / SettingsPatch / PlaybackPosition       |
 +---------------------------------------------------------------------------+
```

Rules:

- **Library writes** (new rows, overwrite metadata, position, settings) always go through the service over the named pipe. The UI keeps its read-only WAL handle.
- **File-level edit work** (trim, extract, proxy, repair) runs in the UI process inside `JobRunner`. It is file-to-file processing, not live capture, so it does not break the brief's rule that the UI never touches capture/encode. This is an explicit decision; see Open questions for the alternative of running jobs in the service.
- **Sprite sheets** (hover thumbnails, filmstrip) are generated by the `library` crate in the service after a recording is finalized, and read by the UI from `%LOCALAPPDATA%\ClipCheck\thumbs\`.

### 3.2 Data flow: playback

```
 Gallery row --open--> Player.svelte
                          |
                          | invoke player_open(recording_id)
                          v
                 Rust: lookup row (read-only DB), probe (cached), choose source
                          |                         |
           direct play ok?|                         | no, proxy_policy=auto
                          v                         v
          source = clip://rec/<id>        ensure_proxy() job -> clip://proxy/<id>
                          |                         |
                          +------------+------------+
                                       v
              <video src=...> (muted if mixer active)   <audio src=clip://audio/<id>/<n>> x (N-1)
                                       |
                    WebAudioMixer: MediaElementSource -> Gain_n -> Master -> Limiter -> destination
                                       |
                    Sync loop (250 ms): drift(video, audio_n) > threshold -> audio_n.currentTime = video.currentTime
```

### 3.3 Data flow: trim

```
 Editor (ranges, mode, target)
     |  invoke edit_start(TrimRequest)
     v
 JobRunner: spawn thread "clipedit-job-<ulid>"
     |  probe(src) -> keyframes(src) (cached) -> snap range
     |  trim(lossless|precise) -> writes  <dest_dir>\.<name>.cliptmp
     |  progress events  ---> UI (job://progress/<job_id>, ~4 Hz)
     v
 Done: temp file complete, faststart applied
     |  invoke edit_commit(job_id, final_name, parent_id, replace_id?)
     v
 UI --IPC RegisterEditedRecording--> service
     service: validate path under recordings root, rename .cliptmp -> final,
              (overwrite: move original to Recycle Bin first), insert/update row,
              enqueue sprite generation, emit RecordingAdded/Updated
```

Temp files are dot-prefixed `.cliptmp` names in the destination folder so the library's file watcher can ignore them and the gallery never lists them. Startup sweep: the UI deletes `.*.cliptmp` files older than 24 h inside recording roots that have no live job.

### 3.4 Sprite sheets (owned by `library`, consumed here)

- Generated by the service after finalize or after first open of a recording without sprites.
- Sample interval: `max(2 s, duration / 300)` (about 300 thumbnails maximum per file), overridable with `player.thumb_interval_s`.
- Each thumbnail 160x90, JPEG q75; 10 x 10 grid per sheet (1600 x 900 px, about 150 KB per sheet).
- Index file `index.json`: `{ "interval_ms": 36000, "cols": 10, "rows": 10, "thumb_w": 160, "thumb_h": 90, "count": 300, "sheets": ["sheet-000.jpg", ...] }`.
- Path: `%LOCALAPPDATA%\ClipCheck\thumbs\<RecordingId>\`.
- Generation uses keyframe-only decoding (`skip_frame = nokey`), so a 3 h file takes seconds, not minutes. Decoding is at below-normal thread priority.
- The UI reads sprites through `clip://thumb/<id>/<sheet>.jpg` (same allowlist rules, Section 5.5).

### 3.5 Module layout

```
crates/clipedit/
  src/
    lib.rs            public API re-exports
    error.rs          EditError
    probe.rs          ProbeInfo, StreamInfo, probe()
    keyframes.rs      KeyframeIndex, build/cache, floor_key/ceil_key
    range.rs          TimeRange, snap logic
    trim.rs           lossless stream-copy trim
    precise.rs        decode-from-in + re-encode video, copy audio
    remux.rs          faststart, container change
    repair.rs         moov rebuild for truncated fMP4
    audio_extract.rs  per-track sidecar extraction
    proxy.rs          H.264 playback proxy + LRU cache
    job.rs            JobControl, Progress, CancelToken
    progress_parse.rs ffmpeg-style key=value progress parsing (for CLI fallback tests)

apps/ui/src-tauri/src/
  protocol/clip.rs    clip:// handler, range parsing, allowlist resolver
  jobs.rs             JobRunner, job table, event emission
  commands/edit.rs    edit_start, edit_cancel, edit_commit
  commands/player.rs  player_open, player_position, proxy_status

apps/ui/web/src/player/
  Player.svelte, Transport.svelte, ScrubBar.svelte, HoverThumb.svelte,
  TrackPopover.svelte, WebAudioMixer.ts, keymap.ts, directPlay.ts
apps/ui/web/src/editor/
  Editor.svelte, Filmstrip.svelte, Handles.svelte, SnapReadout.svelte, ExportPanel.svelte
```

---

## 4. Key types, traits and function signatures

### 4.1 `clipedit` — public API (Rust)

```rust
// error.rs
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("job cancelled")]
    Cancelled,
    #[error("probe failed: {0}")]
    Probe(String),
    #[error("no video stream in source")]
    NoVideoStream,
    #[error("invalid range {start_ms}..{end_ms} for duration {duration_ms}")]
    InvalidRange { start_ms: i64, end_ms: i64, duration_ms: i64 },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("ffmpeg error {code}: {msg}")]
    Ffmpeg { code: i32, msg: String },
    #[error("not enough disk space: need {need_bytes}, have {have_bytes}")]
    DiskFull { need_bytes: u64, have_bytes: u64 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// job.rs
#[derive(Clone, Default)]
pub struct CancelToken(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl CancelToken {
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Progress {
    pub out_time_ms: i64,      // output media time written so far
    pub total_ms: i64,         // requested output duration
    pub bytes_written: u64,
    pub speed_x: f32,          // media seconds processed per wall second
}

pub struct JobControl {
    pub cancel: CancelToken,
    pub progress: Box<dyn Fn(Progress) + Send + Sync>,
}

// range.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimeRange { pub start_ms: i64, pub end_ms: i64 }
impl TimeRange {
    pub fn duration_ms(&self) -> i64 { self.end_ms - self.start_ms }
    pub fn validate(&self, source_ms: i64) -> Result<(), EditError>;   // 0 <= start < end <= source
}

// probe.rs
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeInfo {
    pub duration_ms: i64,
    pub start_time_ms: i64,            // first PTS, normalized to zero (may be 0.5 s from B-frames)
    pub container: Container,          // Mp4 | Fmp4 | Mkv | Other
    pub has_moov: bool,                // false = truncated fMP4 / needs repair
    pub faststart: bool,
    pub video: Option<VideoInfo>,
    pub audio: Vec<AudioInfo>,         // index = audio track number - 1
    pub other_streams: u32,            // data/attachment, dropped in output
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct VideoInfo {
    pub stream_index: u32,
    pub codec: VideoCodec,             // H264 | Hevc | Av1 | Other(String)
    pub profile: String,               // e.g. "Main 10"
    pub bit_depth: u8,
    pub chroma: String,                // "4:2:0"
    pub width: u32,
    pub height: u32,
    pub avg_fps: f64,                  // 0.0 if unknown
    pub vfr: bool,                     // avg vs r_frame_rate differ by > 1 %
    pub color_tag: Option<String>,     // "bt709", "bt2020nc", ...
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioInfo {
    pub stream_index: u32,
    pub codec: AudioCodec,             // Aac | Opus | Other(String)
    pub channels: u8,
    pub sample_rate: u32,
    pub title: Option<String>,         // metadata "title" of the stream
    pub language: Option<String>,
    pub default_flag: bool,
}

pub fn probe(path: &std::path::Path) -> Result<ProbeInfo, EditError>;

// keyframes.rs
#[derive(Debug, Clone)]
pub struct KeyframeIndex {
    pts_ms: Vec<i64>,                  // sorted, de-duplicated, video stream only
}
impl KeyframeIndex {
    /// Largest keyframe <= t. Returns first keyframe if t < first.
    pub fn floor_key(&self, t_ms: i64) -> i64;
    /// Smallest keyframe >= t. Returns duration if none.
    pub fn ceil_key(&self, t_ms: i64) -> i64;
    pub fn is_empty(&self) -> bool;
}
pub struct KeyframeCache { root: std::path::PathBuf }   // %LOCALAPPDATA%\ClipCheck\index\
pub fn keyframes(src: &std::path::Path, probe: &ProbeInfo, cache: &KeyframeCache,
                 ctl: &JobControl) -> Result<KeyframeIndex, EditError>;
// Cache key: recording id + file size + mtime. Stored as little-endian i64 array with 16-byte header.

// trim.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutputContainer { SameAsSource, Mp4Faststart, Mkv }

#[derive(Debug, Clone)]
pub struct LosslessOpts {
    pub container: OutputContainer,
    pub drop_other_streams: bool,      // default true; data/attachment streams cannot go to MP4
}

pub fn trim_lossless(src: &std::path::Path, probe: &ProbeInfo, kf: &KeyframeIndex,
                     req: TimeRange, opts: &LosslessOpts, dst_tmp: &std::path::Path,
                     ctl: &JobControl) -> Result<TrimOutcome, EditError>;

#[derive(Debug, Clone, serde::Serialize)]
pub struct TrimOutcome {
    pub requested: TimeRange,
    pub actual: TimeRange,             // after snapping
    pub bytes: u64,
    pub audio_tracks_written: u32,
    pub snapped_start: bool,
}

pub fn snap_lossless(kf: &KeyframeIndex, req: TimeRange, duration_ms: i64) -> TimeRange {
    // start = kf.floor_key(req.start_ms); end = req.end_ms (packet accurate)
}

// precise.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VideoEncoderChoice { Auto, Nvenc, Amf, Qsv, X264, X265 }

#[derive(Debug, Clone)]
pub struct PreciseOpts {
    pub encoder: VideoEncoderChoice,   // Auto uses encode::select_encoder(codec)
    pub quality: u8,                   // CRF / CQ / QP-equivalent, 0..=51
    pub preset: String,                // "p5" (NVENC), "balanced" (AMF), "medium" (x264)
    pub container: OutputContainer,
}

pub fn trim_precise(src: &std::path::Path, probe: &ProbeInfo, req: TimeRange,
                    opts: &PreciseOpts, dst_tmp: &std::path::Path,
                    ctl: &JobControl) -> Result<TrimOutcome, EditError>;

// remux.rs / repair.rs
pub fn remux_faststart(src: &std::path::Path, dst_tmp: &std::path::Path,
                       ctl: &JobControl) -> Result<u64, EditError>;
pub fn repair_truncated(src: &std::path::Path, dst_tmp: &std::path::Path,
                        ctl: &JobControl) -> Result<ProbeInfo, EditError>;

// audio_extract.rs
/// Extracts audio track `track` (1-based) to a standalone .m4a (AAC) file.
/// Stream-copies AAC; transcodes Opus to AAC-LC 192 kbps for Chromium-safe playback.
pub fn extract_audio_track(src: &std::path::Path, probe: &ProbeInfo, track: u32,
                           dst_tmp: &std::path::Path, ctl: &JobControl) -> Result<u64, EditError>;

// proxy.rs
#[derive(Debug, Clone)]
pub struct ProxyOpts {
    pub max_height: u32,               // 1080 default
    pub encoder: VideoEncoderChoice,   // Auto
    pub quality: u8,                   // 23
}
pub struct ProxyCache { pub root: std::path::PathBuf, pub max_bytes: u64, /* ... */ }
impl ProxyCache {
    /// Returns the proxy path if present and valid; otherwise None.
    pub fn lookup(&self, recording: &RecordingKey) -> Option<std::path::PathBuf>;
    /// Evicts least-recently-used proxies until under max_bytes. Never evicts `pinned`.
    pub fn evict_to_budget(&self, pinned: &[RecordingKey]) -> std::io::Result<u64>;
}
pub struct RecordingKey { pub id: String, pub size: u64, pub mtime_ms: i64 }
pub fn ensure_proxy(src: &std::path::Path, key: &RecordingKey, cache: &ProxyCache,
                    opts: &ProxyOpts, ctl: &JobControl) -> Result<std::path::PathBuf, EditError>;
// Proxy is video-only H.264 (plus no audio; audio always comes from track sidecars or the
// original's track 1 via the proxy's own audio when present). See 5.6.
```

Note on proxy audio: the proxy keeps **track 1 audio** (stream copy when AAC, transcode to AAC when Opus), so single-track playback works from the proxy alone. Tracks 2..N are always sidecars.

### 4.2 Playback protocol (Rust, `apps/ui/src-tauri/src/protocol/clip.rs`)

```rust
pub enum ClipSource {
    Recording(std::path::PathBuf),   // original recording, resolved from library DB
    Proxy(std::path::PathBuf),       // %LOCALAPPDATA%\ClipCheck\proxy\<id>.mp4
    Audio(std::path::PathBuf),       // sidecar .m4a for track N
    Thumb(std::path::PathBuf),       // sprite sheet jpg
}

/// Allowlist. The URL never contains a filesystem path; only typed keys are resolved.
pub trait ClipResolver: Send + Sync {
    fn resolve(&self, key: &ClipKey) -> Option<ClipSource>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipKey {
    Rec { id: String },                    // clip://localhost/rec/<ULID>
    Proxy { id: String },                  // clip://localhost/proxy/<ULID>
    Audio { id: String, track: u32 },      // clip://localhost/audio/<ULID>/<n>
    Thumb { id: String, sheet: String },   // clip://localhost/thumb/<ULID>/<sheet>
}
impl ClipKey {
    pub fn parse(path: &str) -> Result<ClipKey, ClipError>;   // strict: exact segments, ULID charset only
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange { pub start: u64, pub end_inclusive: u64 }

pub enum RangeOutcome {
    Full,                              // no Range header: 200, whole body (only for files <= 1 MiB)
    Partial(ByteRange),                // 206
    Unsatisfiable,                     // 416, Content-Range: bytes */len
}

/// Parses a single `bytes=start-end`, `bytes=start-`, or `bytes=-suffix` range.
/// Multi-range requests are answered with the first range only (Chromium never sends them for media).
/// Responses are capped at `max_chunk` bytes so memory stays bounded on multi-GB files.
pub fn parse_range(header: Option<&str>, len: u64, max_chunk: u64) -> Result<RangeOutcome, ClipError>;

pub struct FileHandleCache { /* LRU, 8 open handles, keyed by path */ }
pub fn serve(req: &http::Request<Vec<u8>>, resolver: &dyn ClipResolver,
             handles: &FileHandleCache) -> http::Response<Vec<u8>>;
```

Response headers for every media response:

| Header | Value |
|---|---|
| `Content-Type` | `video/mp4`, `audio/mp4`, `image/jpeg` by extension; `video/x-matroska` for MKV (direct play will normally fail, which triggers proxy) |
| `Accept-Ranges` | `bytes` |
| `Content-Length` | bytes in this response |
| `Content-Range` | `bytes s-e/len` for 206 |
| `Access-Control-Allow-Origin` | `*` (required so `MediaElementAudioSourceNode` can read the samples; the server is local, read-only, and keyed by ULID) |
| `Access-Control-Expose-Headers` | `Content-Range, Content-Length, Accept-Ranges` |
| `Cache-Control` | `no-store` for recordings still being written; `private, max-age=3600` for finalized files and sprites |

`OPTIONS` requests return `204` with `Access-Control-Allow-Methods: GET, HEAD, OPTIONS` and `Access-Control-Allow-Headers: Range`.

`HEAD` returns the headers without body.

### 4.3 Editor state and IPC types (TypeScript, `apps/ui/web/src/editor/types.ts`)

```ts
export type TrimMode =
  | { kind: "lossless"; container: "same" | "mp4" | "mkv" }
  | { kind: "precise"; encoder: "auto" | "nvenc" | "amf" | "qsv" | "x264" | "x265";
      quality: number; /* 0..51 */ };

export type SaveTarget =
  | { kind: "new_clip" }
  | { kind: "overwrite"; recordingId: string };

export interface TrimRequest {
  recordingId: string;               // source (always original file)
  rangeMs: { start: number; end: number };
  mode: TrimMode;
  target: SaveTarget;
}

export interface SnapResult {
  requestedMs: { start: number; end: number };
  actualMs: { start: number; end: number };
  snappedStart: boolean;
  estimatedBytes: number;
}

export type JobState =
  | { state: "queued" }
  | { state: "running"; progress: { outTimeMs: number; totalMs: number; speedX: number; bytes: number } }
  | { state: "finished"; tempPath: string; actualMs: { start: number; end: number }; bytes: number }
  | { state: "cancelled" }
  | { state: "failed"; code: string; message: string };

export interface PlayerSourceInfo {
  recordingId: string;
  durationMs: number;
  source: "original" | "proxy";
  videoUrl: string;                  // clip://... (http://clip.localhost/... on Windows)
  audioTracks: { track: number; label: string; channels: number; defaultAudible: boolean; url: string }[];
  live: boolean;
  thumbs: { intervalMs: number; cols: number; rows: number; thumbW: number; thumbH: number;
            count: number; sheetUrls: string[] } | null;
}
```

### 4.4 Web Audio mixer (TypeScript)

```ts
export interface TrackRoute {
  track: number;                     // 1-based
  element: HTMLMediaElement;         // <video> for track 1, <audio> for others
  source: MediaElementAudioSourceNode;
  gain: GainNode;                    // per-track dB gain (preview only)
  audible: boolean;
  solo: boolean;
}

export class WebAudioMixer {
  constructor(ctx: AudioContext, master: GainNode, limiter: DynamicsCompressorNode);
  attach(track: number, el: HTMLMediaElement): void;   // creates MediaElementAudioSourceNode once
  setAudible(track: number, on: boolean): void;        // solo overrides
  setSolo(track: number, on: boolean): void;
  setGainDb(track: number, db: number): void;          // clamped -20..+6
  setMasterVolume(v: number, muted: boolean): void;    // 0..1
  syncTo(master: HTMLVideoElement, thresholdMs: number): void; // starts 250 ms drift loop
  dispose(): void;
}
```

Limiter: `DynamicsCompressorNode` with threshold -1 dB, knee 0, ratio 20, attack 3 ms, release 100 ms, to prevent clipping when several tracks sum to a hot master.

### 4.5 Keyframe snapping (pure function, property-tested)

```rust
pub fn snap_lossless(kf: &KeyframeIndex, req: TimeRange, duration_ms: i64) -> TimeRange {
    let start = kf.floor_key(req.start_ms).max(0);
    let end = req.end_ms.min(duration_ms);
    TimeRange { start_ms: start, end_ms: end }
}
```

Invariants (property tests in Section 10):
- `actual.start_ms <= requested.start_ms`
- `actual.start_ms` is a keyframe PTS (or 0)
- `actual.end_ms == requested.end_ms` (packet accurate, to within one frame)
- `actual.duration_ms >= requested.duration_ms`

---

## 5. Algorithms / state machines

### 5.1 Direct-play decision

```
Input: ProbeInfo (video), browser capabilities probed at app start (cached)

1. If video is None -> audio-only view, no decision needed.
2. Build MIME+codecs string:
     H.264 High  -> video/mp4; codecs="avc1.640028"     (use profile/level from probe, else 640028)
     HEVC Main   -> video/mp4; codecs="hvc1.1.6.L120.90"
     HEVC Main10 -> video/mp4; codecs="hvc1.2.4.L120.90"
     AV1         -> video/mp4; codecs="av01.0.08M.08"
3. canPlayType(mime) == "" -> UNSUPPORTED (go to 6)
4. navigator.mediaCapabilities.decodingInfo({
       type: "file", video: { contentType, width, height, bitrate, framerate },
       audio: { contentType: 'audio/mp4; codecs="mp4a.40.2"', channels, bitrate, samplerate } })
   .supported == false  -> UNSUPPORTED (go to 6)
   .smooth == false and .powerEfficient == false  -> SUPPORTED_SLOW (play; show a dropped-frame hint)
5. SUPPORTED -> direct play with clip://rec/<id>.
6. UNSUPPORTED:
     proxy_policy == never  -> show missing-codec message, offer default app
     proxy_policy == auto   -> ensure_proxy() then play proxy
     proxy_policy == always -> ensure_proxy() then play proxy (skip step 4)
```

Capability probe at startup: a cached table in memory, keyed by codec string; refreshed when the app regains focus after more than 24 h (user may have installed the HEVC extension).

Microsoft's HEVC support on Windows requires the "HEVC Video Extensions" package (Store, paid or OEM-included). WebView2 relies on the same Media Foundation decoders, so probe results are authoritative.

### 5.2 Keyframe index build

```
1. cache_path = index\<id>.kf ; key = (id, size, mtime)
2. If cache_path exists and header matches key -> load and return.
3. If probe.container is finalized MP4 with faststart (moov at front):
     use libav index entries of the video stream (AVSTREAM index with AVINDEX_KEYFRAME flag). Fast path, O(stss).
4. Else demux-only scan:
     open input; for each packet of video stream:
        if packet.flags & AV_PKT_FLAG_KEY: push pts_ms (pts * time_base, normalized by start_time)
     (no decode; ~ disk-read bound)
5. Sort, dedupe, drop PTS < 0.
6. If zero keyframes found -> store empty index; snap_lossless then returns requested start unchanged
   and the UI shows "No keyframe index: lossless start is exact but may not be decodable"; the editor
   forces precise mode with a banner.
7. Write cache: header { magic "CCKF", version 1, size, mtime_ms, count } + i64[] little-endian.
```

Budget: 3 h H.264 MP4 faststart fast path under 50 ms; demux-only scan of a 3 h, 6 GB file under 20 s on NVMe.

### 5.3 Lossless trim (stream copy)

```
Input: src, probe, kf, requested TimeRange R, LosslessOpts, dst_tmp

1. R.validate(probe.duration_ms)                                  -> InvalidRange on failure
2. A = snap_lossless(kf, R, duration)                              (A.start <= R.start, A.end == R.end)
3. Check disk: need = estimate(A) = source_bytes * (A.duration / duration) * 1.02; require free >= need + 512 MiB
4. Open input (with start_time normalization).
5. Open output muxer at dst_tmp:
     container = same as source (mp4 -> mp4 with movflags +faststart applied at the end, mkv -> mkv)
     streams: video (stream copy) + ALL audio streams (stream copy) ; drop data/attachment if drop_other_streams
6. Seek input to A.start (av_seek_frame with AVSEEK_FLAG_BACKWARD on the video stream) — lands on keyframe.
7. Loop over packets:
     if ctl.cancel -> Err(Cancelled)
     skip packets whose pts (in ms, normalized) < A.start for audio streams (audio may start before keyframe; keep audio >= A.start - 0 to avoid pre-roll click; see edge case 6)
     if pts >= A.end -> break (for all streams)
     rebase pts so first output PTS == 0 (per stream; video anchor drives)
     write packet (interleaved)
     emit progress every 250 ms wall time
8. Trailer write. If mp4: rewrite with faststart (second pass copying moov to front; this is an I/O copy but
   with no re-encode). Target <= 2x source read time.
9. fsync dst_tmp. Return TrimOutcome { requested: R, actual: A, bytes, audio_tracks_written, snapped_start }.
```

Notes:

- Video packets are never dropped between A.start and A.end, so the first output frame is the keyframe at A.start.
- Audio packets before the keyframe are skipped, so audio starts at the first audio packet at or after A.start. This creates up to one AAC frame (21 ms) of silence-free drift at most; acceptable.
- Timestamps: `avoid_negative_ts = make_zero`-equivalent logic is built in (rebase step).

### 5.4 Precise trim (re-encode video, copy audio)

```
1. R.validate; A = R (no snapping).
2. Choose encoder:
     Auto -> encode::select_encoder(codec_of_source_video, prefer = [Nvenc, Amf, Qsv, X264|X265])
     If the codec is HEVC and the chosen encoder is x265, quality/preset map to x265 defaults.
3. Open input; seek to floor_key(A.start) (keyframe before start) to keep decoding cheap.
4. Decode video from that keyframe; discard decoded frames with pts < A.start (accurate trim).
5. Encode frames with (encoder, quality, preset); keep the GOP at 2 s (matching session writer).
6. Audio: stream copy all audio packets with pts in [A.start, A.end); keep leading audio before keyframe
   out, so audio and video both begin at A.start.
7. Output container: same rule as lossless. Set color metadata copied from source (color_tag).
8. Progress: out_time from encoder output PTS vs total_ms.
9. Failure: if the chosen HW encoder fails to open (driver/session limit), fall back once to x264 (or x265)
   and record "fallback_encoder" in the TrimOutcome for the UI to show.
```

Speed expectation: NVENC 1080p60 H.264 at 3x to 6x realtime; x264 veryfast on a 6 core CPU at 1.5x to 3x.

### 5.5 Playback serving (per request)

```
serve(req):
 1. method = OPTIONS -> 204 with CORS headers.
 2. key = ClipKey::parse(req.uri().path()); on error -> 400
 3. source = resolver.resolve(key); None -> 404 (no path leaks)
 4. (path, len) = metadata; for Recording, if the row is "live", len = last known good size from
    the session writer's index (fMP4 fragment boundary) to avoid returning a partially written box
 5. range = parse_range(req.headers["Range"], len, max_chunk = 8 MiB)
      Full (len <= 1 MiB) -> 200 with whole body
      Full (len > 1 MiB)  -> treat as bytes=0-(max_chunk-1), 206  (never send GB bodies)
      Partial(r)          -> 206 with Content-Range
      Unsatisfiable       -> 416
 6. handle = handles.get_or_open(path)   (LRU of 8 open files, shared read, FILE_SHARE_READ|FILE_SHARE_WRITE)
 7. seek + read exact length; if the file shrank since open, return 416 and drop the handle
 8. HEAD -> headers only
```

Why 8 MiB chunks: WebView2 issues many range requests during scrubbing. Chunking keeps buffered memory bounded while letting Chromium fetch as needed. Chromium reads ahead roughly 10–20 MB on local files, so the cost is a few extra round trips, not a stall.

Latency target: range response under 2 ms for a 1 MiB read from page cache, under 15 ms cold on NVMe.

### 5.6 Proxy generation

```
ensure_proxy(src, key, cache, opts, ctl):
 1. If cache.lookup(key) -> return path (touch mtime for LRU)
 2. Reserve: out = proxy\<id>.mp4.part ; estimate size = min(src_size, 0.25 * src_size) ; require disk space
 3. Build pipeline:
      video: decode HW if available (D3D11VA/NVDEC) else SW; scale to max_height keeping aspect, even dims;
             encode H.264 (Auto encoder; quality opts.quality; GOP 2 s; B-frames 2)
      audio: track 1 only: copy if AAC; else transcode to AAC-LC 192 kbps. Other tracks dropped.
      container: MP4 with +faststart
 4. Progress = out_time / duration; ETA from speed_x (smoothed over 10 s)
 5. On success: rename .part -> <id>.mp4 ; cache.evict_to_budget(pinned = [key])
 6. On cancel/failure: delete .part
Priority: below-normal thread priority; at most one proxy job at a time (editor and player share it).
```

Quality target: proxy visually close enough for scrubbing and preview. Source bitrate ignored; fixed CRF 23 at 1080p.

LRU budget: `player.proxy_max_gb` (default 10 GB). Proxies in use by an open player are pinned.

### 5.7 Repair truncated fMP4

Trigger: `probe.has_moov == false` (session writer crashed or is still writing) or probe error with a valid `ftyp`.

```
1. Scan top-level boxes: keep complete moof+mdat pairs; discard trailing partial box.
2. Rebuild moov from the init segment (moov written at session start, stored at the head of the file).
   If the init moov is missing (truncated before first fragment): Err(Unsupported("no init segment")).
3. Write dst_tmp = [ftyp][moov][moof/mdat...][mfra?] with +faststart semantics.
4. Re-probe; if duration within 2 s of expected (from session index if any), accept.
```

Implementation uses ffmpeg's movenc in remux mode (`-c copy -movflags +faststart`) on the truncated input, which handles most cases; the box scanner is a pre-step that trims the partial tail so ffmpeg does not reject the file.

### 5.8 Audio sidecar extraction (lazy)

```
Triggered when the user first enables track N >= 2 in the popover.
1. Path = audio\<id>\track-<N>.m4a ; if present and key matches -> return URL
2. probe.audio[N-1].codec:
     AAC  -> stream copy of that stream only, to .m4a (mp4 muxer, +faststart)
     Opus -> decode to PCM, encode AAC-LC 192 kbps (audio-only, ~50x realtime)
3. Duration check: file duration within 50 ms of source audio duration, else Err(Probe)
4. Return URL clip://audio/<id>/<N>
```

The sidecar is needed for playback only. The output of lossless and precise trims uses the original streams and never these files.

### 5.9 Player state machine

```
          open()
 Idle ----------------> Probing ----> DirectPlayCheck
                                         |            |
                              supported  |            | unsupported
                                         v            v
                                    LoadingOriginal  ProxyWaiting(progress)
                                         |            |   (proxy ready or 10 s buffered)
                                         v            v
                                       Ready <--------+
                                         |
          play() / pause()  <-----------> Playing / Paused
                                         |
                         scrub/seek -> Seeking -> (seeked) -> Playing|Paused (restore)
                                         |
                    buffer empty(waiting) -> Buffering -> (canplay on all elements) -> Playing
                                         |
                    track toggle (N>=2) -> ExtractingSidecar(N) -> Mixing (video muted, Web Audio on)
                                         |
                    error -> Error(code) with Retry, Locate, OpenDefault
```

Mixing is entered lazily. When all sidecars are audible-off and only track 1 is on, the mixer disconnects tracks 2..N and the video element returns to `muted = false` with `volume` set (no Web Audio needed for the common case).

### 5.10 Multi-track sync loop

```
every 250 ms while Playing (also on seeked, ratechange):
   t_v = video.currentTime
   for each sidecar audio element a_n:
       drift = a_n.currentTime - t_v
       if |drift| > threshold_ms (default 30 ms):
           a_n.currentTime = t_v        // hard resync, causes a short click at worst; rare in practice
   if any element.readyState < HAVE_FUTURE_DATA:
       pause all, enter Buffering, resume when all are >= HAVE_FUTURE_DATA
   a_n.playbackRate = video.playbackRate  (set on every ratechange)
```

Hard resync on `seeked` for every element; resync is skipped while `Seeking` to avoid fighting the browser.

Chromium's audio clock for MediaElementAudioSource runs off the AudioContext clock; the 30 ms threshold is chosen so an unnoticed correction happens at most a few times per minute on a 3 h file. Measured in Section 10.

### 5.11 Filmstrip and hover thumbnail lookup

```
hover_thumb(t_ms):
  idx = clamp(round(t_ms / interval_ms), 0, count-1)
  sheet = floor(idx / (cols*rows)); cell = idx % (cols*rows)
  if sheet loaded: draw sprite region (cell_x, cell_y, 160, 90) to a canvas
  else: request sheet (prefetch neighbours) and draw from the nearest loaded cell in the meantime
  if sprites absent (count == 0): fallback canvas seek via hidden <video> with throttle 100 ms; show spinner
```

Filmstrip cells use the same lookup at evenly spaced `t_ms` values. Zoom re-samples at the new spacing, using sprites as-is (no extra decode).

### 5.12 Trim job state machine

```
 queued --start--> running --(ok)----------> finished --commit--> committed (row registered)
   |                 |  \--(cancel)--> cancelled (temp deleted)
   |                 \--(error)------> failed  (temp deleted)
   \--(cancel)------> cancelled

 committed:
   new_clip -> service renames temp -> final name, inserts row, sprite generation queued
   overwrite -> service moves original to Recycle Bin, renames temp -> original name,
                updates row (size, duration, file_hash), invalidates thumbs, keyframe index,
                proxy for that id
```

Commit is a single IPC request. If the service is unreachable at commit, the UI keeps the temp file and shows "Could not save yet, retry" (the temp survives until retry or the 24 h sweep).

### 5.13 Overwrite safety

```
1. Refuse if the recording is live (session writer holds it).
2. Refuse if any job other than this one references the id.
3. Write output to .<name>.cliptmp in the same directory (same volume, so rename is atomic).
4. Verify temp: probe succeeds, duration within 2 frames of actual range, all audio streams present.
5. Service: SHFileOperationW(FOF_ALLOWUNDO | FOF_NOCONFIRMATION) moves original to the Recycle Bin.
6. Rename temp -> original name (MoveFileExW with MOVEFILE_REPLACE_EXISTING as belt-and-braces).
7. Update row; emit RecordingUpdated.
If step 5 fails: abort, keep original, delete temp, report error.
```

---

## 6. Config keys owned by this feature

All keys live in `%APPDATA%\ClipCheck\config.toml` under the listed tables. Hot-reloadable. UI-changed values (volume, mute, speed) are written by the service through the `SettingsPatch` IPC message so the service remains the only writer of `config.toml`.

### 6.1 `[player]`

| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `player.volume` | f32 | 0.8 | 0.0–1.0 | Persisted on change (debounced 500 ms) |
| `player.muted` | bool | false | — | Persisted on change |
| `player.default_speed` | f32 | 1.0 | {0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0} | |
| `player.seek_step_s` | f32 | 5.0 | 0.5–60.0 | Left/Right step |
| `player.proxy_policy` | enum | "auto" | "auto", "never", "always" | See 2.3 |
| `player.proxy_max_gb` | u32 | 10 | 1–200 | LRU budget for proxy cache |
| `player.proxy_max_height` | u32 | 1080 | 480, 720, 1080 | |
| `player.hover_thumbnails` | bool | true | — | Off = no sprite lookup, no hover image |
| `player.thumb_interval_s` | u32 | 0 | 0 (auto), 2–300 | 0 = `max(2, duration/300)` |
| `player.resync_threshold_ms` | u32 | 30 | 5–200 | Advanced; 2.2 sync loop |
| `player.resume_position` | bool | true | — | Off = always start at 0 |
| `player.audio_track_defaults` | string | "1" | comma list of track numbers, e.g. "1,3" | Audible on open |

Note: `player.volume` and `player.muted` are global. Per-recording position is in the `playback_state` DB table, not in config.

### 6.2 `[editor]`

| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `editor.default_mode` | enum | "lossless" | "lossless", "precise" | |
| `editor.default_target` | enum | "new_clip" | "new_clip", "overwrite" | Overwrite still asks each time |
| `editor.confirm_overwrite` | bool | true | — | Cannot be disabled while `editor.default_target = "overwrite"` |
| `editor.filmstrip_cells` | u32 | 40 | 10–200 | |
| `editor.snap_pixels` | u32 | 8 | 0–24 | 0 disables handle snapping |
| `editor.precise_encoder` | enum | "auto" | "auto", "nvenc", "amf", "qsv", "x264", "x265" | Forced choice falls back to x264/x265 if unavailable |
| `editor.precise_quality` | u8 | 18 | 0–51 | CRF/CQ-equivalent |
| `editor.precise_preset` | string | "balanced" | "fast", "balanced", "quality" | Mapped per encoder |
| `editor.output_container` | enum | "same" | "same", "mp4", "mkv" | |
| `editor.job_concurrency` | u32 | 1 | 1–2 | Jobs beyond limit queue |
| `editor.temp_free_margin_mb` | u32 | 512 | 64–10240 | Extra free space required before a job |

---

## 7. IPC messages / events produced or consumed

Transport: named pipe `\\.\pipe\clipcheck-ipc`, length-prefixed JSON (4-byte little-endian length, UTF-8 JSON body). Envelope `{ "type": "<Name>", "id": "<ULID>", "payload": { ... } }`. Responses echo `id`.

### 7.1 UI to service (requests)

| Type | Payload | Response | Purpose |
|---|---|---|---|
| `RegisterEditedRecording` | `{ temp_path, final_name, kind: "clip-trim", parent_id?: string, replace_id?: string, probe_summary: {duration_ms, bytes, audio_tracks, video_codec} }` | `{ recording_id }` or error `path_not_allowed`, `exists`, `missing_source`, `recording_live`, `io` | Commit an edit. Service validates temp path lives under recordings root and starts with `.`; renames; inserts or updates row; enqueues sprites |
| `PlaybackPosition` | `{ recording_id, position_ms }` | `{}` | Debounced 5 s and on close |
| `SettingsPatch` | `{ key: "player.volume" \| "player.muted" \| ..., value }` | `{ applied: true }` | UI-originated preference writes; service validates range and writes TOML |
| `RequestSprites` | `{ recording_id, interval_ms? }` | `{ queued: bool }` | Ask for sprite generation if missing (e.g. file opened before sprites existed) |
| `MarkMissing` | `{ recording_id }` | `{}` | When the UI found the file gone and the user chose "Remove from library" |
| `CheckRecordingState` | `{ recording_id }` | `{ live: bool, size: u64, duration_ms: i64 }` | Used when opening a file that may still be written |

### 7.2 Service to UI (events)

| Event | Payload | UI reaction |
|---|---|---|
| `RecordingAdded` | `{ recording: RecordingRow }` | Gallery insert; if `parent_id` matches an open editor source, show toast |
| `RecordingUpdated` | `{ recording_id, fields: {duration_ms?, bytes?, live?, has_thumbs?} }` | Player: update duration and LIVE badge; editor: refresh filmstrip if thumbs changed |
| `RecordingRemoved` | `{ recording_id, reason: "deleted" \| "missing" \| "moved_to_recycle_bin" }` | Close player with notice if open |
| `ThumbsReady` | `{ recording_id, index_url }` | Player reloads sprite index |
| `ConfigReloaded` | `{ keys: string[] }` | Re-read player/editor prefs |
| `SessionPartFinalized` | `{ recording_id, duration_ms, faststart: bool }` | If open, switch LIVE to final; re-probe keyframes |

### 7.3 UI-internal events (Tauri event bus, not IPC)

| Event | Payload | Source |
|---|---|---|
| `job://progress/<job_id>` | `Progress` (Section 4.1) | JobRunner, ~4 Hz |
| `job://state/<job_id>` | `JobState` (Section 4.3) | JobRunner on each transition |
| `proxy://progress/<recording_id>` | `{ out_time_ms, total_ms, eta_s }` | ensure_proxy |
| `player://track-state` | `{ track, audible, solo, gain_db }` | Mixer changes (for popover sync across windows, if any) |

### 7.4 Tauri commands (UI front-end to Rust)

| Command | Args | Returns |
|---|---|---|
| `player_open` | `{ recording_id }` | `PlayerSourceInfo` |
| `player_codec_check` | `{ recording_id }` | `{ direct: bool, reason?: string }` |
| `proxy_request` | `{ recording_id }` | `{ job_id }` |
| `sidecar_request` | `{ recording_id, track }` | `{ url }` |
| `editor_snap` | `{ recording_id, range_ms, mode }` | `SnapResult` |
| `edit_start` | `TrimRequest` | `{ job_id }` |
| `edit_cancel` | `{ job_id }` | `{}` |
| `edit_commit` | `{ job_id, final_name, parent_id?, replace_id? }` | `{ recording_id }` (calls service) |
| `probe_repair` | `{ recording_id }` | `{ job_id }` |

---

## 8. Edge cases & failure modes

| # | Case | Behavior |
|---|---|---|
| 1 | HEVC not decodable (extension missing) | Direct-play check fails; proxy (auto) or missing-codec message (never). Proxy job appears in status bar. |
| 2 | HEVC 10-bit or 4:2:2 not supported by HW decoder but "supported" by canPlayType | `decodingInfo` reports `smooth=false`; play anyway with dropped-frame hint; if frames drop above 20 % for 10 s, offer "Switch to proxy". |
| 3 | Truncated fMP4 from a crash (no moov) | `has_moov=false`; player shows "Recording was interrupted". Repair (5.7) offered; repair is also run automatically on finalize by the service, so this only happens if the service itself died. |
| 4 | Live session file still being written | LIVE badge; `clip://rec` serves up to the last complete fragment (size taken from session index); duration grows via RecordingUpdated; editor disables Overwrite; lossless snapping clamps end to last keyframe written. |
| 5 | Session file with B-frames, start_time 0.5 s | `ProbeInfo.start_time_ms` normalizes all times; UI shows timecode from 0, not from start_time. Trim rebases PTS. |
| 6 | Audio pre-roll: AAC frames before the keyframe at trim start | Audio packets before the keyframe start are dropped; first audio starts at or after requested start (up to one AAC frame later). Documented in SnapReadout. |
| 7 | Opus audio tracks not decodable in the WebView2 path for a given container | Sidecar extraction transcodes Opus to AAC (5.8), so playback never depends on Opus support. Lossless and precise trims keep Opus as-is in the output (stream copy); the transcode only affects preview files. |
| 8 | Multiple audio tracks with different channel layouts (mono, stereo, 5.1) | Each sidecar is decoded to stereo by Web Audio channel mixdown (`channelCount = 2`, `channelCountMode = "explicit"`). 5.1 downmix uses the Web Audio default matrix. |
| 9 | Audio and video drift beyond threshold over 3 h | Resync loop (5.10) on 250 ms ticks. If resync fails three times in 10 s (e.g., decoder stall), mixer falls back to track 1 only and shows a warning. |
| 10 | Autoplay blocked / AudioContext suspended | Context created on first user gesture (play button, click on video, or key press). If still suspended on play, call `resume()`. Mixer is not created until a gesture. |
| 11 | Keyframe index missing (no keyframes or scan failed) | Editor forces precise mode with banner "Lossless needs a keyframe index". Precise mode works without keyframes. |
| 12 | Start or end out of range, or start >= end | `InvalidRange` returned by `validate`; UI disables Save and highlights the handle. Clamped to [0, duration] when dragging. |
| 13 | Disk full during trim | Free-space check before start (needs estimate + margin). If the write fails mid-job, `Io` error with ENOSPC mapped to `DiskFull`; temp deleted; original untouched. |
| 14 | Cancel during precise re-encode | Encoder session closed, process threads joined within 500 ms, temp deleted. HW encoder session released (NVENC session slots are limited on consumer cards). |
| 15 | Hardware encoder unavailable or session limit reached | One automatic fallback to x264/x265 with `fallback_encoder` reported in the outcome and a toast. |
| 16 | Variable frame rate source (game capture with dropped frames) | `vfr=true`; frame step uses the mean frame duration, and the UI notes "approximate frame step". Lossless trim unaffected. Precise re-encode writes CFR at `avg_fps` to avoid A/V drift in players. |
| 17 | Missing file (moved or deleted externally) | `clip://` returns 404; player shows missing-file state with Locate… (updates path via service) or Remove. Never crashes. |
| 18 | Overwrite target held open by another process (Explorer preview, antivirus scan) | Rename fails with sharing violation; retry 5 times over 2 s; then error "File is in use; try again", original kept, temp kept for retry. |
| 19 | Proxy cache exhausted with all entries pinned | Eviction skips pinned entries; new proxy job still runs if disk allows; if disk too low, job refuses with a message naming the budget setting. |
| 20 | Memory pressure on long playback (WebView2 media buffers) | Chromium manages media buffers; the protocol keeps range responses at 8 MiB max. Player releases sidecar buffers when tracks are muted for more than 60 s (sets `src = ""` and reloads on unmute). |
| 21 | Non-ASCII file paths and names | Protocol uses IDs only, so URLs are ASCII. Paths handled as `OsString` in Rust; FFmpeg receives UTF-16 paths via libav wide-char APIs. |
| 22 | Recording deleted while an editor job is running | Job continues on the source handle (Windows keeps the file alive until closed); commit then fails with `missing_source`; temp is kept with a toast "Source was removed; save as file instead". |
| 23 | Service unreachable at commit | Temp kept; UI shows "Could not save yet, retry". |
| 24 | Keyboard focus in the video (fullscreen) steals shortcuts from the editor | Shortcut map is scoped by view: editor shortcuts are only bound in the editor component. Esc in fullscreen is consumed by fullscreen first. |
| 25 | Zero-length or corrupted duration metadata | Probe falls back to scanning the last packet PTS; if still unknown, `duration_ms = 0`, the player shows "Duration unknown" and the editor refuses to trim until a repair run. |
| 26 | Window closed during a running job | JobRunner cancels all jobs on UI exit with a confirm dialog ("Export in progress: cancel and exit?"). Temp files are cleaned by the startup sweep. |

---

## 9. Performance & resource budget

### 9.1 Player

| Metric | Target | Notes |
|---|---|---|
| Open to first frame, local H.264 1080p | p95 < 400 ms | Excludes proxy |
| Open to first frame, HEVC 1080p direct play | p95 < 600 ms | |
| Seek to new position (keyframe at 2 s GOP) | p95 < 150 ms | |
| Hover thumbnail update | < 16 ms per pointer move | Sprite cached; no decode |
| Scrub drag decode load | 0 decodes during drag | Video seeks only on release |
| Playback CPU, 1080p HEVC HW decode | < 8 % of one core | Measured with WebView2 GPU decode |
| Playback CPU, 1080p H.264 HW decode | < 6 % of one core | |
| Sidecar audio tracks (4 extra) | < 2 % of one core total | AAC decode in WebView2 |
| Player memory (WebView2 renderer, 3 h file, sprites loaded) | < 350 MB | Chromium-owned; includes buffers |
| UI app idle with player open and paused | < 120 MB (brief quality bar, Rust side) | Rust side share, excluding WebView2 renderer |
| Protocol response latency | < 2 ms (cached), < 15 ms (cold NVMe) for 1 MiB range | |
| Protocol memory per in-flight request | <= 8 MiB | Enforced by max_chunk |
| Open file handles per recording | 1 (LRU cap 8 across all) | |

### 9.2 Editor and jobs

| Metric | Target | Notes |
|---|---|---|
| Keyframe index, MP4 faststart | < 50 ms | Index from container |
| Keyframe index, demux scan, 3 h / 6 GB file, NVMe | < 20 s | Cached after first run |
| Lossless trim throughput | >= 80 % of sequential disk read/write bandwidth | I/O bound; e.g. 1.2 GB/min on a 200 MB/s drive |
| Lossless trim memory | < 64 MB | Packet queue bounded |
| Lossless faststart rewrite | <= 2x source read time | Second pass only for MP4 output |
| Precise re-encode, 1080p60 H.264, NVENC | >= 3x realtime | RTX 30-series class |
| Precise re-encode, 1080p60 H.264, x264 veryfast, 6 cores | >= 1.5x realtime | Software fallback |
| Proxy generation, 1080p H.264, NVENC, from HEVC | >= 3x realtime | |
| Proxy generation, 1080p H.264, x264 superfast fallback | >= 1x realtime | Notify if below 1x (slow drive or CPU) |
| Audio sidecar extraction, AAC copy, 3 h | < 10 s | Demux + mux |
| Sprite sheet generation, 3 h, keyframes only | < 60 s | Below-normal priority |
| Job progress event rate | 4 Hz | Coalesced |
| Job CPU overhead (progress and channel) | < 0.5 % of one core | |

Budget notes:

- Trim and proxy jobs run at below-normal thread priority so recording is never starved. Recording is in the service and this UI job would only compete if the user edits while a game is recording; in that case the editor shows a banner and jobs are capped to one thread (`editor.job_concurrency = 1` still applies).
- Disk: proxies capped by `player.proxy_max_gb`; sprites ~150 KB per 300 thumbnails (about 0.5 MB per 3 h recording); keyframe index 8 bytes per keyframe (a 3 h file at 2 s GOP is about 5400 keyframes, 43 KB).

---

## 10. Test plan

### 10.1 Unit tests (`clipedit`, Rust)

| Area | Tests |
|---|---|
| `parse_range` | `bytes=0-`, `bytes=100-199`, `bytes=-500` (suffix), end past EOF (clamp), start past EOF (416), malformed (400), multi-range (first only), zero-length file, max_chunk clamp |
| `ClipKey::parse` | Each variant; reject `..`, `%2e%2e`, extra segments, non-ULID chars, empty id, unknown kind |
| `KeyframeIndex` | floor/ceil with exact hit, between, before first, after last, empty index, duplicate PTS |
| `snap_lossless` | Property tests (proptest): invariants in 4.5 for random keyframes and ranges |
| `TimeRange::validate` | start<0, start>=end, end>duration, equal endpoints |
| Progress parse | key=value blocks, `out_time_us=N/A` handling, `progress=end` |
| `ProxyCache::evict_to_budget` | LRU order, pinned never evicted, budget zero, race with lookup |
| Commit path validation | Temp path outside root rejected; name without leading dot rejected; overwrite target with live state rejected (service side) |
| Mixer math | dB to linear, solo override table (pure function extracted to TS and tested with vitest) |

### 10.2 Integration tests with fixtures

Fixtures generated by a script (`tools/gen-fixtures.ps1`, run in CI, uses lavfi) and stored under `crates/clipedit/tests/fixtures/` (generated, git-ignored, 10 s each unless noted):

| Fixture | Content |
|---|---|
| `h264-1080p60-4aud.mp4` | H.264 High, 60 fps, 4 AAC stereo tracks, 2 s GOP, faststart |
| `hevc-1080p60-main.mp4` | HEVC Main, 4:2:0 8-bit, 1 AAC track |
| `hevc-10bit-main10.mp4` | HEVC Main 10 |
| `h264-session-fmp4.mp4` | fragmented MP4, no mfra, 2 s GOP |
| `h264-truncated.mp4` | fMP4 cut at 60 % of bytes (no moov tail) |
| `opus-mkv.mkv` | H.264 + Opus 2 tracks in MKV |
| `vfr.mp4` | variable frame rate with dropped frames |
| `bframes-start0.5.mp4` | start_time offset 0.5 s with B-frames |
| `novideo.m4a` | audio only |
| `long-3h-lowres.mp4` | 3 h, 320x180, 1 fps GOP 2 s; for scale tests, generated on demand not in CI |

Tests:

- `probe` returns expected streams for each fixture.
- `keyframes` demux scan and faststart fast path agree for faststart files.
- `trim_lossless` on each fixture: output probes with duration within 1 frame of `actual`; video first PTS == 0; all source audio tracks present; byte-identical video packets compared to source (stream copy check: compare packet payload hashes in the range).
- `trim_lossless` with start exactly on a keyframe: `snapped_start == false`.
- `trim_precise`: decoded output frames match reference frames within PSNR > 40 dB at the same timestamps (decoded from the source using the exact range).
- `remux_faststart`: moov before mdat after rewrite.
- `repair_truncated`: repaired file probes, duration within 2 s of expected for the 60 % cut.
- `extract_audio_track` on Opus and AAC fixtures: output duration within 50 ms of source track.
- `ensure_proxy`: H.264 output, max height respected, track 1 audio present, cancel mid-run leaves no `.part`.
- Cancel tests: cancel at 10 %, 50 %, 90 % for lossless and precise; temp deleted, error `Cancelled`, return within 500 ms.
- Disk-full simulation via a small RAM disk or a quota-limited test directory: `DiskFull` returned, original untouched.

### 10.3 Protocol tests (`apps/ui/src-tauri`)

- Serve each `ClipKey` against a temp library; check headers listed in 4.2.
- Range matrix: request the entire file through Chromium-like chunk requests (sequential 8 MiB ranges plus random seeks) and verify byte-exact content via SHA-256.
- Concurrency: 16 parallel range requests across 4 files; no handle leak (LRU cap enforced).
- Live file: append while serving; response never exceeds last complete fragment.
- CORS: preflight 204 with allowed headers; `crossOrigin="anonymous"` audio element loads and `MediaElementAudioSourceNode` produces non-zero samples (automated in WebView2 test harness, see 10.4).

### 10.4 UI tests

- Svelte component tests (vitest + @testing-library/svelte): transport state machine, keymap (every shortcut in 2.2), handle drag math and snapping, snap readout strings, track popover solo logic.
- WebView2 end-to-end (Tauri test harness with WebView2 driver, Windows CI runner):
  - Open H.264 fixture, play 5 s, seek, frame step count, speed change, volume persistence.
  - Open HEVC fixture on a machine with and without HEVC extension (CI runner matrix: both); verify direct-play vs proxy path.
  - Toggle track 2 on a 4-track fixture; verify `AudioContext` graph has 2 active sources (via debug hook).
- Manual test checklist (run before each release, recorded in the release notes):
  - Hover thumbnails on a 3 h session: no stutter while moving pointer across the bar.
  - Scrub 30 times across 3 h: no frozen frames > 500 ms after release.
  - Trim 10 min out of a 2 h session in lossless mode: output plays, audio on all tracks, duration matches snap readout.
  - Precise trim of a 4-second segment: first frame matches requested in-point (check with frame-by-frame compare against reference).
  - Overwrite flow: confirm dialog, original appears in Recycle Bin, gallery row updated.
  - Cancel during export, then immediately start another: no leftover `.cliptmp` after restart sweep.
  - Keyboard-only run: every 2.2 shortcut works without mouse.

### 10.5 A/V sync verification

- Synthetic clap fixture: a 2 s beep at 0, 60, 120 ... s on track 1 (video element) and the same beep on track 2 (sidecar) with a 1-frame offset tolerated.
- Automated run: play for 3 h at 1x in headless WebView2 (or sped up 8x for CI, with the threshold scaled), record `drift` values every 250 ms via debug hook; acceptance: 99th percentile |drift| <= 25 ms after resync, and no resync needed more than once per 60 s in steady state.
- Seek test: 200 random seeks across the file; after each seek, drift within 30 ms within 300 ms.

### 10.6 Soak and stress

- 4 h continuous playback with three sidecar tracks toggled every 10 min: memory growth < 50 MB over the run, no dropped audio.
- 50 consecutive lossless trims of random ranges on a 2 h file: all outputs probe and match snap invariants.
- Concurrent: editor job running while recording a game on a test machine: recording dropped frames = 0 (checked via service stats), trim completes.

---

## 11. Implementation task breakdown

Ordered. Each task is about 0.5–2 days. Tasks 1–6 are the playback path; 7–12 the editor; 13–14 polish and tests.

1. **clipedit skeleton and probe** (1 d). Crate, `EditError`, `ProbeInfo` via `encode::ffmpeg` wrapper, fixtures generator script, unit tests on probe output.
2. **Keyframe index** (1 d). Demux-scan and faststart fast path, cache file format, `floor_key`/`ceil_key`, proptest invariants.
3. **clip:// protocol: ClipKey, range parsing, resolver** (1.5 d). `parse_range`, headers, OPTIONS/HEAD, handle cache, resolver backed by read-only library handle. Protocol tests.
4. **Codec capability check and proxy-free direct play** (1 d). Frontend `directPlay.ts`, MediaCapabilities probe, cache, missing-codec message, Store link.
5. **Player UI basics** (2 d). `Player.svelte`, transport, scrub bar, time display, keymap for transport, speed, frame step with coalescing, volume and mute persistence via `SettingsPatch`, playback position via `PlaybackPosition`.
6. **Hover thumbnails and sprite generation** (1.5 d). Library sprite generator (keyframe-only decode), index.json, `clip://thumb`, HoverThumb component, canvas fallback.
7. **Web Audio mixer and track popover** (2 d). `WebAudioMixer.ts`, lazy routing, solo/audible/gain, limiter, sync loop with resync, sidecar request command, `sidecar_request` extraction via `audio_extract.rs`.
8. **Proxy generation and fallback** (1.5 d). `proxy.rs` with ProxyCache LRU, job wiring, `proxy://progress`, banner UI, pinning rules, policy handling.
9. **Lossless trim engine** (2 d). `trim_lossless` with snapping, rebase, faststart second pass, progress, cancel; invariant tests and fixture tests.
10. **Precise trim engine** (2 d). `trim_precise` decode-from-keyframe, encoder selection via `encode::select_encoder`, fallback, progress, cancel; PSNR tests.
11. **JobRunner and commit path** (1.5 d). Job table, progress events, temp naming and sweep, `edit_commit`, service `RegisterEditedRecording` validation and rename, row insert/update.
12. **Editor UI** (2.5 d). Filmstrip, handles with snap, keyframe ticks, snap readout, live preview loop, mode selector, export panel with progress and cancel, session-to-clip linking.
13. **Overwrite safety and recovery** (1 d). Recycle Bin move, verification, retry on sharing violation, repair entry point (`repair_truncated`), missing-file handling.
14. **Test suite completion and manual checklist** (1.5 d). WebView2 e2e harness, sync verification run, soak test scripts, release checklist.

Estimated total: 22–24 days for one developer.

Dependencies: 1 blocks everything in clipedit; 3 blocks 4–7; 6 is needed for 12 (filmstrip reuses sprites); 9–10 can run in parallel with 5–8 after 2.

---

## 12. Open questions

1. **Where do file-level jobs run?** This doc puts trim, extract, proxy, and repair in the UI process (`JobRunner`). Running them in `clipcheckd` would let jobs survive closing the UI and keep the "UI never touches encode" rule literal, at the cost of a job queue and progress over IPC. Recommendation: UI for v1, revisit if users complain about closing the window mid-export. Affects the brief's process model section.
2. **New `kind` value.** This doc introduces `clip-trim` for editor outputs (naming template in brief lists `clip-30s` and `session-part1`). The brief should add `clip-trim` to the enumerated kinds, or editor output should reuse `clip-30s` with a `parent_id` marker. Recommendation: add `clip-trim`.
3. **Session container.** The playback path assumes session files are fragmented MP4 with a seekable index after finalize (faststart rewrite). If the session doc chooses MKV, every session plays via proxy or a remux, which would be slow. Recommendation: session writer uses fMP4 for session parts (the brief allows this). Confirm with the session doc. Also confirm keyframe interval <= 2 s, since scrubbing and lossless snapping depend on it.
4. **Tauri custom scheme origin on Windows.** This doc assumes Tauri v2 maps `clip://` to `http://clip.localhost/...` on WebView2 and that CORS with `Access-Control-Allow-Origin: *` is needed for `MediaElementAudioSourceNode`. Verify against the pinned Tauri version in the first spike (task 3) and update the URL forms in Section 4.2 if different.
5. **HEVC default.** Many Windows installs lack the paid HEVC Video Extensions. Should the proxy policy default to `auto` (ships proxies silently) or should the app recommend installing the extension first? Recommendation: `auto`, with a one-time notice.
6. **Proxy audio.** The proxy keeps track 1 audio and tracks 2..N are sidecars. An alternative is to keep all tracks in the proxy and drop sidecars entirely, at the cost of a larger proxy and a re-encode of every track. Recommendation: keep current design; revisit if sidecar extraction time on 3 h files exceeds 20 s.
7. **Recycle Bin for overwrite.** Using the Recycle Bin is reversible but can fill the bin on large sessions. Should overwrite instead use a versioned backup in `%LOCALAPPDATA%\ClipCheck\trash\` with a size cap? Recommendation: Recycle Bin for v1, consistent with user expectations.
8. **Preview-only audio state.** Track audible/solo/gain are not written to the file. Users may expect "mute track 3 in the export" to be possible. That is a v2 feature (track removal in output) and needs a UI decision.
9. **Keyframe index storage.** Index files live in `%LOCALAPPDATA%\ClipCheck\index\`. Alternative is a `keyframes` blob in the library DB. File cache is simpler to invalidate and keeps the DB small. Recommendation: file cache.
10. **Playback position storage.** `playback_state` is a DB table written by the service. If the team prefers no extra table, position could go in `recordings.last_position_ms`. Recommendation: separate table to avoid churn on the main recordings row.
11. **Overwrite of files referenced by sprites and proxies.** The current plan invalidates caches on overwrite. A content hash in `RecordingKey` would make invalidation automatic; cost is hashing large files. Recommendation: size+mtime key, as in Section 4.1.
12. **Precise re-encode quality defaults.** CRF 18 with a balanced preset is a guess; confirm against the encode doc's quality presets (Low/Medium/High/Ultra), and consider mapping the editor to those presets for consistency.
