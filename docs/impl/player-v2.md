# Player v2 (D60): instant open, trim handles, details panel

Goal: match Medal's clip viewer. Contract shared by all workers. Replaces the layout of
`apps/ui/src/lib/Player.svelte`; keeps the pure model (`player/model.ts`), mixer, `trim.ts`.

## Requirements (owner request)

1. Shorten a clip from either side (drag in/out handles on the timeline; "Save Edits").
2. Instant, or much faster, open. The video starts playing on open.
3. Icon buttons (play/pause etc.), not text buttons.
4. Playback speed control (menu: 0.25x .. 2x).
5. Going to Settings (or Gallery) in the left rail leaves the clip.
6. Video is slightly smaller; a details panel sits on the right:
   editable name (textbox), game, size, length, date/time of clip, favourite toggle,
   file location (clickable: opens Explorer with the exact file selected).

## Why opening is slow today (measured by reading the code, not profiled)

- `Player.svelte` mounts `<video>` only after `await open_player`, which probes the file AND,
  for 2+ audio tracks, extracts every audio sidecar (`ensure_sidecar`, a remux per track on first
  open) before returning. The user stares at "Loading recording..." for all of it.
- `preload="metadata"`, no `autoplay`: playback needs another click after metadata.
- The data the player needs for the first frame (id, name, size, duration, ext, game, date) is
  already in the Gallery's `RecordingDto`, but is not passed on.

## Design

### A. Instant start (frontend)

- `openRecording(row: RecordingDto)` stores the whole row (`nav.playerRow`). Player renders
  immediately from the row: `<video src="http://clip.localhost/rec/{id}" poster={thumbUrl(id)}
  autoplay preload="auto">`. No await before the video element exists. Autoplay rejection (webview
  policy) is caught; a big play overlay is shown instead.
- `open_player` becomes fast (probe only, in-memory cache keyed id+size+mtime). It is called in
  parallel, only for codec/dimensions/track list.
- Audio sidecars move to a new command `prepare_audio(id)`, run in the background after open.
  Until it resolves the video plays with its own embedded default track (track 0). When it resolves
  `mixed:true`, the Audio menu appears and the mixer takes over at the current time (video muted,
  audios seeked to `video.currentTime`, then synced). Failure keeps default-track playback and
  shows a dismissible note.
- Warm-up: Gallery card `pointerenter` (80 ms debounce, once per id) calls `warmRecording(id)`:
  `fetch(videoUrl, {headers:{Range:'bytes=0-262143'}})` plus `prepare_audio(id)` fire-and-forget.
  By the time of the click the OS cache and sidecars are usually ready.

### B. Layout

```
.player (row)
  .main (flex 1, column)
    .stage          <video>, click = play/pause, dblclick = fullscreen
    Controls        play/pause icon, time, volume (icon+slider), speed menu, fullscreen icon
    Timeline        ruler + filmstrip + playhead + in/out handles (Medal style)
    .actions        Audio dropdown (left)  ...  [Reset] Save Edits (right, enabled when trimmed)
  DetailsPanel (fixed ~300px, scrolls)
```
Below ~760px width the details panel stacks under the main column.
Back button stays in a slim top bar with the RecordingMenu (delete etc.).

### C. Trim UX

- Selection `{startMs,endMs}` (existing `TrimSelection`, `setIn/setOut/clampSelection`,
  `DEFAULT_MIN_MS`). Handles drag in the Timeline; the dimmed region outside is shown.
- Playback is confined to the selection while it is narrower than the clip: at `endMs` the player
  pauses and seeks back to `startMs`; pressing play at the end restarts from `startMs`.
- Keys: `I`/`O` set in/out at the playhead (via `trimKeyToAction` if it already maps them).
- "Save Edits" opens a small confirm popover: "Save as new clip" (default) and
  "Replace original" (original goes to the Recycle Bin via `deleteRecording` only AFTER the new
  clip registered). Mode lossless by default (keyframe-snapped; readout shows snap), "Exact cut"
  checkbox = `precise`.
- Progress/done through existing `trimapi.ts` events; on success the player switches to the new id.

### D. Details panel

Rows: Name (textbox, stem only, extension shown as fixed suffix; Enter/blur commits with
`renameRecording`, Esc reverts, validation per `validateStem`, inline error), Game (+icon),
Size (`formatBytes`), Length (`formatDuration`), Recorded (`formatDateTime`), Favourite (star
toggle -> `setFavorite`), Location (full path text, click -> `revealRecording(id)`, tooltip full path).
Fields come from the row immediately; `path` arrives from `get_recording_details(id)`.

### E. Navigation

`setView(view)` always calls `closePlayer()` first, so Settings/Gallery rail clicks leave the clip.

## Contracts

### Rust (apps/ui/src-tauri/src)

`player.rs` (owner: B1)
```rust
pub struct PlayerOpenDto { /* existing fields */ pub audio_pending: bool }  // true when >=2 audio tracks and sidecars not yet checked
pub fn open_fast_at(db:&Path, id:&str) -> Result<PlayerOpenDto, PlayerError>; // probe only, mixed=false, track urls None; cached
pub struct AudioPrepDto { pub tracks: Vec<AudioTrackDto>, pub mixed: bool, pub audio_error: Option<String> }  // camelCase
pub fn prepare_audio_at(db:&Path, cache_dir:&Path, id:&str) -> Result<AudioPrepDto, PlayerError>;
#[tauri::command] pub async fn open_player(id:String) -> Result<PlayerOpenDto,String>   // now = open_fast_at (spawn_blocking)
#[tauri::command] pub async fn prepare_audio(id:String) -> Result<AudioPrepDto,String> // spawn_blocking
```
`open_at` keeps its signature (existing tests): it calls both and merges. Concurrent
`prepare_audio` calls for the same id must not run two remuxes (per-id mutex/in-flight map).

`details.rs` (owner: B2, new file)
```rust
#[serde(rename_all="camelCase")] pub struct RecordingDetailsDto { pub id, pub path:String, pub dir:String, pub file_name:String, pub size_bytes:u64 /*from fs, 0 if missing*/, pub exists:bool }
#[tauri::command] pub fn get_recording_details(id:String) -> Result<RecordingDetailsDto,String>
```
Path comes from `media::resolve_recording_path`.

`trim.rs` (owner: B4, new file) - the D27 backend. Commands `trim_clip(req)`, `cancel_trim(job_id)`,
`get_trim_preview(id,start_ms,end_ms,mode)`; events `trim-progress {jobId,percent,outTimeMs}`,
`trim-done {jobId,ok,newId?,error?}` exactly as `trimapi.ts`. Keyframes from
`clipcheck_mp4::reader::read_movie` (video track sample `keyframe` flags -> ms). Non-MP4 source ->
error "Trimming needs an MP4 recording". Output = `clipedit::suggest_output_name` in the source's
directory; `clipedit::plan_trim` + `run_trim` on `spawn_blocking`, cancel via `AtomicBool` map;
then daemon `RegisterEditedRecording` (`ipc::daemon_request`) to get `newId`. One job at a time.
Request has `overwrite:false` always (replace is done by the UI after registration).

Daemon favourite (owner: B3): `Command::SetFavorite { recording_id, favorite }` in
`crates/ipc/src/proto.rs` (wire + conversions), handled in `apps/clipcheckd/src/server.rs` via
`Library::set_favorite`, reply `ok`; unknown id -> `not_found`-style error used by rename.

### TypeScript (apps/ui/src/lib)

- `api.ts` (orchestrator, pre-done): `audioPending` on `PlayerOpenDto`; `AudioPrepDto`,
  `prepareAudio(id)`, `RecordingDetailsDto`, `getRecordingDetails(id)`.
- `recordingActions.ts` (orchestrator, pre-done): `setFavorite(id, favorite)`.
- `nav.ts` (orchestrator, pre-done): `playerRow` store, `openRecording(row)`, `setView` closes player,
  `updatePlayerRow(patch)`.
- `player/warm.ts` (owner W1): `warmRecording(id)` as above, dedupes, never throws.
- `player/details.ts` (owner W2, pure): `validateStem(s): string|null` (blank, > 200 chars,
  `<>:"/\|?*`, control chars, trailing dot/space, reserved names CON/PRN/AUX/NUL/COM1-9/LPT1-9),
  `splitName`, `formatDateTime(ms)` (locale, "11 Oct 2026, 14:03"), `detailRows(row, path?)`.
- `player/timeline.ts` (owner W3, pure): `msToX(ms,w,dur)`, `xToMs(x,w,dur)` (clamped, NaN-safe),
  `tickStep(durMs, widthPx)` (nice steps 1/2/5/10/15/30/60.. s, min 60px apart), `filmstripCount(w, aspect, h)`,
  `confinePlayback(nowMs, sel, durMs): {action:'none'|'pause-and-seek', toMs:number}`.
- `player/Icon.svelte` (owner W4): `name` in play pause volume volume-low mute speed fullscreen
  fullscreen-exit folder star star-filled back scissors audio check close reset; inline SVG,
  `currentColor`, `size` prop default 20, `aria-hidden`.
- `player/Controls.svelte` (owner W5): props `paused,now,duration,volume,muted,rate,fullscreen`;
  callbacks `ontoggle,onvolume,onmute,onrate,onfullscreen`. Icon buttons with aria-labels; speed
  button shows "1x" and opens a menu of `SPEEDS`; keyboard accessible; Esc closes the menu.
- `player/Timeline.svelte` (owner W6): props `durationMs, nowMs, selection, frames:string[]`,
  callbacks `onseek(ms)`, `onselection(sel, kind)`; pointer events with capture; keyboard on
  handles (arrows = FRAME_MS, shift = COARSE_MS); uses `timeline.ts` and `trim.ts`.
- `player/filmstrip.ts` (owner W7): `makeFilmstrip(videoUrl, count, height, signal): AsyncIterable<{index,dataUrl}>`
  using a hidden `<video muted preload="auto">` + canvas, sequential seeks, aborts on signal, skips
  frames on error, never blocks playback (starts after the main video fires `playing`).
- `player/DetailsPanel.svelte` (owner W8): props `row: RecordingDto, path: string|null, gameIcon?:string,
  onrename(stem):Promise<void>, onfavorite(v):Promise<void>, onreveal()`; commit/revert behaviour as D above.
- `player/AudioMenu.svelte` (owner W9): button "Audio" (icon+label) opening a dropdown listing
  tracks (checkbox-style speaker toggles like Medal: Game, Discord, Microphone...), solo and gain
  kept in an "Advanced" disclosure, the existing `OutputPicker` at the bottom. Props: `tracks info,
  states, onchange`. Hidden when fewer than 2 tracks or not mixed.
- `Player.svelte` (owner I1, wave 2): composition of all of the above, replacing the old controls.

## Rules for workers

- Edit only OWNED files. Read only named files/ranges. No tests, no test runs (owner request: tests
  come in a separate chain after the owner confirms features). Verify with `npm run build` in
  `apps/ui` (UI) or `cargo clippy -p <crate> -- -D warnings` / `cargo check` in `apps/ui/src-tauri`.
- Style: match the surrounding code: Svelte 5 runes, TS strict, comment density like `model.ts`.
- Use CSS variables from `apps/ui/src/app.css` (read it for names); no new dependencies.
- Report <= 25 lines.
