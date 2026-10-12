# Video editor: implementation contract (for workers)

Spec: `docs/design/13-video-editor.md` (read only the sections named in your task). Rules for all
workers: `docs/impl/AGENT_RULES.md`. This file is the contract between workers; if your task prompt
and this file disagree, the prompt wins.

## 0. Global rules for this run (owner instructions)

- **CODE ONLY. Nobody runs tests.** Do NOT run `cargo test`, `npx vitest`, `scripts/test-all.sh`,
  or launch the app. Allowed checks: `cargo check -p <crate>`, `cargo check -p <crate> --tests`
  (compiles test code without running it), `cargo clippy -p <crate> --all-targets -- -D warnings`
  (compile + lint, no test execution), `npm run build` / `npx tsc --noEmit` / `npx svelte-check` in
  `apps/ui`. If a check fails in a file you do not own, a sibling worker is probably mid-edit:
  wait a minute and re-run; only fix your own files.
- Implementers write source and tiny inline `#[cfg(test)]` helpers only if needed. A separate
  TESTER worker writes the real tests (also never run).
- Windows. No Python. Use the Write/Edit tools, not shell heredocs, for file content.
- Own only the files in your task. Never edit another worker's file, `Cargo.toml` of a crate you do
  not own, `lib.rs` module lines, or `DESIGN.md`. If you need a change elsewhere, say so in your
  report.
- No `unwrap()`, `expect()`, `panic!`, slice indexing `[i]` or `todo!()` in new Rust code (use
  `get()`, `?`, checked math). The `timeline` and `render` crates deny these lints; `src-tauri`
  editor modules follow the same rule. Tests may use them.
- Never panic on bad input; hostile project files and media are expected.
- Do not use em dashes in code comments or UI text. Keep comment density like the surrounding code.
- Final report to the orchestrator: at most 25 lines: files, public API, deviations, unfinished.

## 1. Decisions (also recorded in DESIGN.md)

- D72: Editor/export user settings live in a UI-owned `settings.json` (`EditorSettings` in
  `apps/ui/src-tauri/src/editor/settings.rs`), not in `config.toml`, except `storage.export_dir`.
- D73: v1 compositing is the CPU reference compositor (`render::reference`), not the GPU. A GPU
  path can replace it later; the reference stays as the oracle.
- D74: Exports are registered as library kind `clip` with game `Exports` (new optional `game` field
  on `RegisterEditedRecording`). No schema migration.
- D75: Edit state lives in Rust (`timeline::Session` inside Tauri `EditorState`); the UI sends
  `Command`s and receives snapshots. Preview asks Rust for the render plan; no TS port of the ops.

## 2. Crates and files (owners)

`crates/timeline` (pure): `time.rs`, `model.rs`, `error.rs` are FINAL contracts (do not edit).
`export.rs` types final, functions owned by `tl-export`. Other files owned as in their headers.
`crates/render`: see file headers. `crates/mfdec/src/audio.rs`: `mfdec-audio`.
`apps/ui/src-tauri/src/editor/*`: see headers. `apps/ui/src/lib/editor/*`: section 4.

Time: `Tick` = i64, 254,016,000,000 per second, exact. JSON numbers (< 2^53). Project length <= 4 h.

### Semantics worth repeating

- A clip's timeline length is `dur`; its source length is `dur * speed` (`Clip::src_len`). Speed 0.0 =
  freeze (video clips only).
- Video clips from a recording come with linked audio clips (one per audio stream, `link_id` shared,
  only stream 0 audible). Audio clips live on audio tracks; video, image, title, solid clips on video
  tracks. Video tracks stack in list order (first = bottom).
- Group (`group_id`) and link (`link_id`) partners move, split, delete, duplicate together
  (`ops_group::expand_selection`). Only `place_media`, `group_clips` and `link_clips` create ids.
- No overlaps on a track. Operations are atomic: validate first, then mutate (or clone-and-swap).
- Undo = full project snapshots in `Session` (depth 200).
- Plan: `plan::derive_plan` is what preview and export read.

## 3. Tauri command contract

Every command is in `apps/ui/src-tauri/src/editor/*.rs` with a doc comment describing behaviour and
returns `Result<T, String>`. camelCase DTO fields (`serde(rename_all = "camelCase")`). `timeline`
types serialize in snake_case enum/field form exactly as declared in `model.rs` (they have no
rename_all on structs, so field names are snake_case: `track_id`, `src_in`, ...). The UI therefore
sees snake_case project data and camelCase DTO wrappers. `Command` JSON is
`{"type":"split_at","time":123,"ids":[]}`.

Events (Tauri `emit`): `export-progress`, `export-done`, `export-failed`, `export-queue` (payload
`JobDto`/`JobDto[]`), `peaks-ready` (`{mediaId, stream}`).

## 4. UI contract (`apps/ui/src/lib/editor/`)

Svelte 5 (runes allowed; match `Player.svelte` style), TypeScript strict. Theme with the app's CSS
variables from `apps/ui/src/app.css` (read its `:root` block; do not invent colours). Keyboard
operable, `aria-label` on icon buttons, no em dashes. Icons: reuse `player/Icon.svelte` and add new
paths there only if your task owns it; otherwise inline small SVG.

| Module | Exports (names are the contract) |
|---|---|
| `types.ts` | `Tick`, `Rate`, `Rgba`, `Project`, `Canvas`, `MediaRef`, `Track`, `Clip`, `Transform`, `ClipAudio`, `Group`, `Marker`, `WorkArea`, `EnvPoint`, `Transition`, `TextSpec`, `ExportSettings` (+ `QualityMode`, `ExportRange`, `AudioExport`, `ExportKind`), `Preset`, `SizeEstimate`, `Command` (discriminated union on `type`, exactly mirroring `timeline::cmd::Command`), `ClipPatch`, `TrackPatch`, `PlaceMode`, `Edge`, `Applied`, `EditorSnapshot`, `ApplyReply`, `RenderPlan`, `PlanSegment`, `VideoLayer`, `AudioSource`, `ProjectSummary`, `JobDto`, `ExportDefaults`, `EstimateReply`, `EditorSettings`, `SnapPointDto`, `LoudnessReport`, `MediaStatus` |
| `ticks.ts` | `TICKS_PER_SECOND`, `secToTick(s)`, `tickToSec(t)`, `msToTick`, `tickToMs`, `frameDurationTicks(rate)`, `snapToFrame(t, rate)`, `formatTimecode(t, rate)` (`HH:MM:SS:FF`), `formatDuration(t)` (`M:SS` or `H:MM:SS`), `parseTimecode(text, rate)` (returns Tick or null), `clipEnd(clip)`, `srcLen(clip)` (dur*speed rounded) |
| `api.ts` | `editorApi` object with one typed async method per Tauri command (names: `listProjects, newProject, openProject, recoverAutosave, saveProject, autosave, closeProject, renameProject, duplicateProject, deleteProject, apply, undo, redo, setUiState, plan, snapPoints, clipboardLen, probeMedia, mediaFromRecording, mediaStatus, waveform, thumbnails, mediaUrl, measureClipLoudness, exportDefaults, estimateExport, checkSmartExport, exportStart, exportCancel, exportPause, exportPauseOverride, exportQueue, exportDismiss, exportReorder, exportStill, savePreset, deletePreset, getSettings, setSettings`) and event helpers `onExportProgress, onExportDone, onExportFailed, onExportQueue, onPeaksReady` returning an unlisten function |
| `store.ts` | Svelte stores `snapshot`, `selection` (string[]), `playhead` (Tick), `playing`, `tool` (`'select'|'razor'|'hand'|'zoom'`), `pxPerSec`, `scrollX`, `scrollY`, `toast` (message or null), `busy`, `exportJobs` (JobDto[]); derived `project`, `plan`, `selectedClips`; functions `openSnapshot(s)`, `closeEditor()`, `dispatch(cmd)` (returns ApplyReply or null; on error sets `toast`), `undo()`, `redo()`, `selectClips(ids, mode)` (`'replace'|'add'|'toggle'`), `selectAllClips()`, `copySelection()`, `pasteAtPlayhead()`, `startAutosave(seconds)`, `stopAutosave()`, `seek(t)`, `zoomBy(factor, anchorTick?)`, `zoomToFit(viewWidthPx)` |
| `layout.ts` | pure geometry: `trackLayout(project, scrollY)`, `clipRect(clip, layout, view)`, `hitTest(project, layout, view, x, y)` returning `{kind:'clip'|'edge-start'|'edge-end'|'gap'|'ruler'|'track'|'empty', clipId?, trackId?, tick?}`, `xToTick`, `tickToX`, `dragMoveTarget(...)`, `snapTick(t, points, thresholdPx, pxPerSec)`, `niceRulerStep(pxPerSec)`, `marqueeSelect(project, layout, view, rect)` |
| `shortcuts.ts` | `ACTIONS` ids, `resolveShortcut(e: {key, ctrlKey, shiftKey, altKey, metaKey, target?}) -> ActionId | null` per design doc section 6, `SHORTCUT_HELP` table for the overlay |
| `preview/clock.ts`, `preview/engine.ts`, `preview/audioGraph.ts`, `preview/scheduler.ts` | see task |
| Components | `EditorHome.svelte`, `Editor.svelte`, `Toolbar.svelte`, `MediaBin.svelte`, `MediaPicker.svelte`, `ProgramMonitor.svelte`, `SourceMonitor.svelte`, `Timeline.svelte`, `TrackHeader.svelte`, `Ruler.svelte`, `Inspector.svelte`, `ClipInspector.svelte`, `TrackInspector.svelte`, `ProjectInspector.svelte`, `ExportDialog.svelte`, `ExportQueue.svelte`, `ExportPill.svelte`, `ContextMenu.svelte`, `ShortcutsOverlay.svelte`, `EditorSettingsPanel.svelte`, `Toasts.svelte` |

App wiring (done by the orchestrator): `nav.ts` gets view `'editor'` and `editorTarget`
(`{ projectId: string | null, recordingIds: string[] } | null`); `App.svelte` mounts
`Editor.svelte`/`EditorHome.svelte`.

## 5. Tests (TESTER workers only; never run)

Rust tests go in `crates/<crate>/tests/<topic>_tests.rs` (public API only). TS tests next to the
module as `*.test.ts` (vitest, no DOM unless the module needs it; mock `@tauri-apps/api/core`
with `vi.mock`). Testers verify compilation with `cargo check -p <crate> --tests` or
`npx tsc --noEmit`, never execution. Synthetic data only; never touch real recordings, games, or
the user's folders; temp dirs via `tempfile`.

## 6. Checking UI code without node_modules downloads

`apps/ui/node_modules` is a junction to the main checkout's (git-ignored). Use `npm run build` in
`apps/ui` (vite, catches Svelte and import errors) and `npx tsc --noEmit 2>&1 | grep "lib/editor"`
(the repo has unrelated pre-existing tsc errors; only yours matter). Never run `npm install`.
