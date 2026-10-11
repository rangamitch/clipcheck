# Player MVP: contract shared by all agents (decision D25)

Scope: watch any recording inside the app with a built-in player: play/pause, scrub, time, **volume slider**, mute,
speed, fullscreen, and an **audio track selector** (choose which track(s) are audible, per-track gain). Trimmer, rename,
delete, proxy for HEVC, sprite thumbnails are LATER (docs/design/08). Full spec for reference only: docs/design/08.

## Why sidecars
WebView2/Chromium exposes only the first audio track of an MP4 (`video.audioTracks` is disabled). So for a recording with
2+ audio tracks the backend extracts each track to its own `.m4a` ("sidecar") in a cache; the UI plays the video element
MUTED plus one hidden `<audio>` per track, kept in sync with the video, mixed through Web Audio gain nodes.
With 0-1 audio tracks (or if extraction fails) the video element plays its own audio, the volume slider drives `video.volume`.

## URLs (Windows WebView2: custom scheme `clip` is served as `http://clip.localhost/...`; CSP already allows it)
- Video: `http://clip.localhost/rec/<id>`   (supports HTTP Range; 206 responses)
- Sidecar: `http://clip.localhost/aud/<id>/<n>`  (n = 0-based audio stream index, `audio/mp4`)
`<id>` must match `^[0-9A-Za-z]{1,40}$`. The path is ALWAYS looked up from the library DB (never taken from the URL).
Allowed video extensions: mp4, m4v, mov, mkv (mkv is served but flagged `playable:false`).

## Rust, apps/ui/src-tauri/src
### media.rs (owner: agent R1)
```rust
pub const MAX_CHUNK: u64 = 8 * 1024 * 1024;           // an open-ended or oversized range is capped to this many bytes
pub enum RangeSpec { Full, Partial { start: u64, end_inclusive: u64 }, Unsatisfiable }
pub fn parse_range(header: Option<&str>, len: u64) -> RangeSpec;   // "bytes=a-b", "bytes=a-", "bytes=-n"; invalid/multi-range/non-bytes => Full; start>=len or n==0 => Unsatisfiable; end clamped to len-1; result capped to MAX_CHUNK
pub struct MediaResponse { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub fn content_type_for_ext(ext: &str) -> &'static str;           // mp4|m4v => video/mp4, mov => video/quicktime, mkv => video/x-matroska, m4a => audio/mp4, else application/octet-stream
pub fn is_valid_id(id: &str) -> bool;
pub fn resolve_recording_path(db: &Path, id: &str) -> Result<PathBuf, MediaError>;  // Library::open_readonly + get_recording; root_path.join(rel_path); NotFound if no row or file missing
pub fn sidecar_path(cache_dir: &Path, id: &str, size: u64, mtime_ms: i64, n: u32) -> PathBuf;  // PRE-WRITTEN by orchestrator: cache_dir/audio/<id>/<size>-<mtime_ms>/track<n>.m4a
pub fn serve(db: &Path, cache_dir: &Path, url_path: &str, range: Option<&str>) -> MediaResponse;
```
`serve`: url_path like `/rec/<id>` or `/aud/<id>/<n>` (leading slash; ignore query string). Responses:
200 full (only when body <= MAX_CHUNK, else it behaves as Partial 0..MAX_CHUNK-1 with status 206); 206 with `Content-Range: bytes s-e/len`,
`Content-Length`, `Accept-Ranges: bytes`, `Content-Type`, `Cache-Control: no-store`; 416 with `Content-Range: bytes */len`;
400 bad path/id; 404 unknown id / missing file / sidecar not yet extracted. HEAD is not needed. Reads only the requested bytes (Seek + take), never the whole file.
`MediaError` is a thiserror enum (BadId, NotFound, Db(String), Io(String)).

### player.rs (owner: agent R2)
```rust
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct AudioTrackDto { pub index: u32, pub title: String, pub codec: String, pub channels: u32, pub sample_rate: u32, pub url: Option<String> }
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct PlayerOpenDto { pub id: String, pub video_url: String, pub duration_ms: i64, pub has_video: bool, pub playable: bool,
    pub width: u32, pub height: u32, pub video_codec: String, pub tracks: Vec<AudioTrackDto>, pub mixed: bool, pub audio_error: Option<String> }
pub struct ProbeInfo { pub duration_ms: i64, pub has_video: bool, pub width: u32, pub height: u32, pub video_codec: String, pub audio: Vec<ProbeAudio> }
pub struct ProbeAudio { pub index: u32, pub codec: String, pub channels: u32, pub sample_rate: u32, pub title: String }
pub fn parse_probe_json(json: &str) -> Result<ProbeInfo, PlayerError>;       // `ffprobe -v error -print_format json -show_format -show_streams`; audio index = position among audio streams; title = tags.title or ""; missing numbers => 0; duration from format.duration (seconds, string) else the video stream
pub fn extract_args(src: &Path, n: u32, codec: &str, out_tmp: &Path) -> Vec<String>;   // pure: -nostdin -hide_banner -loglevel error -y -i src -map 0:a:n -vn, then "-c:a copy" if codec=="aac" else "-c:a aac -b:a 192k", then "-movflags +faststart -f mp4" out_tmp
pub fn open_at(db: &Path, cache_dir: &Path, ffmpeg: Option<&clipcheck_encode::locate::FfmpegPaths>, id: &str) -> Result<PlayerOpenDto, PlayerError>;
#[tauri::command] pub async fn open_player(id: String) -> Result<PlayerOpenDto, String>;   // spawn_blocking; db = commands::default_db_path(); cache_dir = cache_dir_from(env CLIPCHECK_CACHE_DIR, LOCALAPPDATA) ; ffmpeg = clipcheck_encode::locate::locate_ffmpeg()
pub fn cache_dir_from(env_value: Option<&str>, local_app_data: Option<&str>) -> PathBuf;  // env wins; else <LOCALAPPDATA>\ClipCheck\cache; else temp_dir()\ClipCheck\cache
```
`open_at`: resolve path (media::resolve_recording_path), fs metadata (size, mtime ms), run ffprobe (`ffmpeg.ffprobe`) when ffmpeg is Some; if None => audio_error="ffmpeg not found", tracks empty, mixed=false, duration from the DB row.
`playable` = ext != "mkv". If audio.len() >= 2: for each track extract to `media::sidecar_path` unless the file exists (write to `<name>.part` then rename; delete `.part` on failure); on full success set url=Some(`http://clip.localhost/aud/<id>/<n>`) for all and `mixed=true`; on any failure set mixed=false, all urls None, audio_error=Some(message) (video then plays its own audio). With 0-1 audio tracks: tracks listed, urls None, mixed=false, no error.
Child processes use `CREATE_NO_WINDOW` (0x08000000) via `std::os::windows::process::CommandExt` under `#[cfg(windows)]`. 60 s timeout per extraction (kill the child).

### Registration (orchestrator pre-wires, workers do NOT edit lib.rs)
`lib.rs`: `pub mod media; pub mod player;`, `.register_uri_scheme_protocol("clip", ...)` calling `media::serve` (db = default_db_path, cache = player::cache_dir_from(...)),
and `player::open_player` in `generate_handler!`. Cargo: `clipcheck-encode` path dependency.

## Frontend, apps/ui/src
### api.ts (orchestrator pre-added): `AudioTrackDto`, `PlayerOpenDto` (camelCase mirror) and `openPlayer(id)`.
### src/lib/player/model.ts (owner: agent F1; PURE, no DOM, no Svelte)
```ts
export interface TrackState { index: number; audible: boolean; solo: boolean; gainDb: number }
export const GAIN_MIN_DB = -20, GAIN_MAX_DB = 6, SPEEDS = [0.25,0.5,0.75,1,1.25,1.5,2], SEEK_STEP_S = 5;
export function initTracks(count: number): TrackState[];                 // track 0 audible, rest not; no solo; gain 0
export function toggleTrack(s: TrackState[], i: number): TrackState[];   // immutable; unknown i => unchanged copy
export function soloTrack(s: TrackState[], i: number): TrackState[];     // toggles solo of i (several solos allowed)
export function setGainDb(s: TrackState[], i: number, db: number): TrackState[];   // clamped to [GAIN_MIN_DB, GAIN_MAX_DB]; NaN => 0
export function dbToLinear(db: number): number;                          // 10^(db/20); -Infinity/NaN => 0
export function isTrackAudible(s: TrackState[], i: number): boolean;     // if ANY track has solo: only soloed tracks; else the audible flags
export function trackGain(s: TrackState[], i: number, volume: number, muted: boolean): number;  // 0 if muted or not audible; else dbToLinear(gain)*clamp01(volume)
export function clampVolume(v: number): number;                          // [0,1], NaN => 1
export function stepVolume(v: number, deltaPct: number): number;        // v + deltaPct/100, clamped, rounded to 2 decimals
export function stepSpeed(cur: number, dir: 1 | -1): number;             // next/prev entry of SPEEDS; unknown cur => nearest; clamps at ends
export function clampTime(t: number, durationS: number): number;        // [0,duration]; NaN => 0; unknown duration (<=0 or NaN) => max(0,t)
export function formatTime(sec: number): string;                         // "m:ss" under one hour, "h:mm:ss" otherwise; NaN/negative => "0:00"; floors
export function percentJump(digit: number, durationS: number): number;  // digit 0-9 => digit*10% of duration
export function needsResync(videoT: number, audioT: number, thresholdS?: number): boolean;  // |diff| > threshold (default 0.15); NaN => false
export function trackLabel(t: { index: number; title: string }): string; // title.trim() or `Track ${index+1}`
export type PlayerAction = { type: 'toggle-play' } | { type: 'seek'; deltaS: number } | { type: 'volume'; deltaPct: number } | { type: 'mute' } | { type: 'fullscreen' } | { type: 'speed'; dir: 1 | -1 } | { type: 'jump'; digit: number } | { type: 'track-toggle'; index: number } | { type: 'track-solo'; index: number } | { type: 'frame'; dir: 1 | -1 };
export function keyToAction(e: { key: string; code?: string; shiftKey: boolean; altKey: boolean; ctrlKey: boolean; metaKey?: boolean }): PlayerAction | null;
```
`keyToAction` (docs/design/08 §2.2): Space/k = toggle-play; j/l = seek -10/+10; ArrowLeft/Right = seek -5/+5 (Shift: -1/+1); ArrowUp/Down = volume +5/-5; m = mute; f = fullscreen; `<`/`>` (and `,`/`.` with shift) = speed; `,`/`.` = frame -1/+1; digits 0-9 = jump; Alt+digit 1-9 = track-toggle index digit-1 (use `code` "Digit1".. when present, since Alt changes `key`); Shift+Alt+digit = track-solo. Ctrl/Meta combos => null (do not hijack browser shortcuts). Unknown => null.

### src/lib/player/mixer.ts (owner: agent F2)
```ts
export interface MixerDeps { createContext?: () => AudioContext }
export class TrackMixer {
  constructor(deps?: MixerDeps);
  attach(elements: HTMLMediaElement[]): void;     // one MediaElementSource + GainNode per element -> master destination; call once per set of tracks
  apply(states: TrackState[], volume: number, muted: boolean): void;   // gain.value = trackGain(...) per element (setTargetAtTime 0.01 s)
  resume(): Promise<void>;                        // ctx.resume() after a user gesture
  detach(): void;                                 // disconnect nodes, close the context; safe to call twice
  readonly attached: boolean;
}
export function syncAudioToVideo(video: HTMLMediaElement, audios: HTMLMediaElement[], thresholdS?: number): void; // sets audio.currentTime=video.currentTime when needsResync; mirrors playbackRate; pauses audio when video paused/ended, plays when playing
```
### src/lib/Player.svelte (owner: agent F3, after F1+F2 land) props `{ id: string; onclose: () => void }`
Calls `openPlayer(id)`; `<video src={videoUrl}>` (muted when `mixed`), hidden `<audio>` per track when mixed; controls bar: play/pause, seek slider with elapsed/total, volume slider + mute button, speed button, fullscreen button, Audio tracks popover (checkbox audible, S solo, gain slider -20..+6 dB per track, note "Preview only, the file is not changed"); keyboard via `keyToAction` (only while the player root has focus); states: loading, error (message + Back), `playable:false` banner ("MKV can't be played in the app yet"), `audioError` notice; Back button calls `onclose`. Use existing CSS variables from app.css; follow Gallery.svelte style. Svelte 5 runes (check how Gallery.svelte does it).
### Wiring (agent F4 after F3): `nav.ts` adds `playerTarget` store + `openRecording(id)`/`closePlayer()`; `App.svelte` shows `<Player>` when `$playerTarget` is set; `Gallery.svelte` double-click / Enter on a ready row calls `openRecording(id)`.

## Test chain (separate fresh agents; public API + this contract only)
Rust: `apps/ui/src-tauri/tests/media_tests.rs`, `player_tests.rs` (temp data dirs; build a library DB with `clipcheck_library::Library` in a tempdir; synthetic 2-audio-track mp4 made with the vendored ffmpeg `lavfi` sources, skip with a loud `eprintln` ONLY when ffmpeg is missing). Vitest: `src/lib/player/model.test.ts`, `mixer.test.ts` (fake AudioContext).
