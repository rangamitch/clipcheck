# Feature wave F1: contracts (decisions D26-D29)

Handover backlog items. Each section is owned by exactly one agent; nothing here touches the files of the
game-icons work (gameicons.rs, icons.rs, Gallery.svelte, api.ts, src-tauri lib.rs/commands.rs, clipcheckd lib.rs/appinfo.rs/service.rs).

## D26: trim execution (crates/clipedit, new file src/run.rs; mod line in lib.rs allowed)
`clipcheck_clipedit::plan_trim` already produces a `TrimPlan` (lossless keyframe-snapped or precise). New:
```rust
pub struct TrimRequest<'a> { pub ffmpeg: &'a Path, pub src: &'a Path, pub dst: &'a Path, pub plan: &'a TrimPlan,
    pub video_crf: u8 /* precise mode, default 20 */, pub encoder: &'a str /* precise mode, e.g. "libx264" */ }
pub struct TrimProgress { pub out_time_ms: i64, pub percent: f32 /* 0..=100 */ }
pub enum TrimRunError { Io(String), Ffmpeg { code: Option<i32>, tail: String }, Cancelled, BadPlan(String) }
pub fn build_args(req: &TrimRequest<'_>, tmp: &Path) -> Vec<String>;      // pure
pub fn run_trim(req: &TrimRequest<'_>, cancel: &AtomicBool, on_progress: &mut dyn FnMut(TrimProgress)) -> Result<(), TrimRunError>;
pub fn parse_progress_line(line: &str) -> Option<i64>;                      // `out_time_us=` / `out_time_ms=` from `-progress pipe:1`
```
Lossless: `-ss <actual_start> -i src -t <duration> -map 0 -c copy -avoid_negative_ts make_zero -movflags +faststart` (input-side -ss with copy lands on the keyframe at/before start; ALL streams and audio titles kept: `-map 0 -map_metadata 0`). Precise: re-encode video only (`-c:v <encoder> -crf <crf>` for libx*, `-cq` for nvenc; audio `-c:a copy`), `-ss` before `-i` at the reencode keyframe + `-t`, then accurate cut by `-ss`/`-t` as the plan's discard_head says (use output-side `-ss <discard_head>` for the video re-encode path). Write to `<dst>.part`, rename on success, delete `.part` on failure/cancel; never touch src; refuse if dst exists. Progress via `-progress pipe:1 -nostats`, ~4 Hz; cancel kills ffmpeg within 500 ms.

## D27: trimmer UI (new files only: apps/ui/src/lib/player/trim.ts, apps/ui/src/lib/trimapi.ts, apps/ui/src/lib/Trimmer.svelte)
Backend command contract (implemented later in src-tauri, not now): `invoke('trim_clip', { req: { id, startMs, endMs, mode: 'lossless'|'precise', overwrite: false } }) -> { jobId }`,
`invoke('cancel_trim', { jobId })`, events `trim-progress {jobId, percent, outTimeMs}` and `trim-done {jobId, ok, newId?, error?}`, `invoke('get_trim_preview', {id,startMs,endMs,mode}) -> {actualStartMs, actualEndMs, snapDeltaMs}` (planner result for the snap readout).
`trim.ts` (pure): `TrimSelection {startMs,endMs}`, `clampSelection(sel, durationMs, minMs=100)`, `nudge(sel, which, deltaMs, durationMs)`, `setIn/ setOut(sel, positionMs, durationMs)`, `snapReadout(requested, actual)` text like "Requested 00:12:04.200 – 00:12:31.900, output 00:12:02.000 – 00:12:31.900 (start snapped back 2.2 s)", `formatHmsMs(ms)`, `parseTimecode(str)` (hh:mm:ss, mm:ss, ss, with optional .mmm; invalid => null), `estimateOutputBytes(sizeBytes, durationMs, sel)`, `trimKeyToAction(e)` (I, O, [ ], Shift+[ ], Ctrl+Enter save new).
`Trimmer.svelte` props `{ id: string; durationMs: number; positionMs: number; onclose: () => void; seek: (ms:number)=>void }`: timeline with draggable in/out handles (pointer events), shaded range, mode toggle, snap readout, Save as new clip button, progress panel with Cancel, error banner. Uses `trimapi.ts` wrappers only.

## D28: update UX (new files: apps/ui/src/lib/updates.ts, UpdateModal.svelte, UpdatePill.svelte, settings/tabs/UpdatesTab.svelte; one-line edits allowed in TitleBar.svelte (mount pill), App.svelte (mount modal), settings/tabs.ts (register 'updates' tab))
Backend already exists in apps/ui/src-tauri/src/updater.rs: commands `check_for_update`, `install_update`, `skip_update_version`, `get_update_state`; events `update-available` (UpdateInfo), `update-progress`. The agent reads updater.rs lines ~240-290 and ~440-470 and crates/update/src/manifest.rs `UpdateInfo` (~55-75) for exact shapes. `updates.ts` = typed wrappers + a Svelte store (available, progress, state, error) + pure helpers (formatBytes reuse from format.ts, `isRecordingBlock(err)`). Pill: "↑ Update x.y.z" in the title bar when available; modal: notes, size, Update now / Later / Skip this version, recording warning (Wait / Stop & update is NOT built: show Wait only), progress bar, error states `recording`, `signature`, network. Settings → Updates tab: current version, last checked, Check now, auto-check toggle bound to config path `updates.check` ('notify' | 'never'; read the config schema for the real enum) through `setPath`.

## D29: audio starvation guard (crates/encode/src/audio_in.rs)
A track receiving no PCM stalls the ffmpeg muxer (and therefore video). The audio writer must inject paced silence: pure `silence_deficit(elapsed: Duration, written_samples: u64, sample_rate: u32, channels: u16) -> usize` (samples needed to catch up to wall clock; 0 when ahead; capped at 1 s worth), used by the writer loop every 20 ms only while the sink is connected and not finished and the queue is empty for > 100 ms. Never inject after `finish`/close. Real data resets the clock reference so injected silence never overlaps real audio (written_samples counts both).
