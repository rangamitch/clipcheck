# 13 — In-app Video Editor (timeline editor + exporter)

Status: Draft v1, first run implemented but not yet executed or verified (see DESIGN.md D76 for the known gaps)
Owners: `apps/ui` (editor UI), new crate `timeline` (model), new crate `render` (export engine), `mfdec` (audio decode), `library`/`core` (export location, registration)
Depends on: 08 (player, `clipedit` trimmer), 07 (library), 09 (config), 12 / D58 (no FFmpeg; everything native), section 14 below (methods adopted from the MIT/Apache-2.0 FilmCraft editor)
Reference editors surveyed: Medal.tv editor, Adobe Premiere Pro, DaVinci Resolve (Cut page), CapCut, Clipchamp, Shotcut, Filmora.

---

## 1. Goal and scope

### 1.1 Goal

Let the user build a finished video from one or many ClipCheck recordings (and imported files) without leaving the app: cut, arrange, group, adjust audio and volume, choose canvas size, and export with explicit control over quality, dimensions and location.

### 1.2 Relationship to the existing trimmer

The quick trimmer (doc 08, D26/D27/D60) stays as the one-clip, two-handle tool. The editor is a **separate, project-based, multi-clip timeline** reached from the gallery ("Edit", multi-select "Edit together") and from the player ("Open in editor", which carries the current in/out selection over). Neither replaces the other.

### 1.3 Explicit non-goals for the first run

No effects, filters, LUTs/colour grading, chroma key, masks, keyframed motion, motion tracking, stabilisation, AI features (auto-highlights, auto-captions, background removal), nested sequences, multicam, plugin API, cloud/upload/sharing, screen-record-into-editor, GPU-compute user effects, HDR tone mapping, proxy-based editing of non-MP4 containers. Basic **transitions (cut, crossfade, fade-through-black), text titles and image overlays** are in as P1/P2 because they are structural, not effects.

### 1.4 Priority and size legend

- **P0**: the editor is not shippable without it (first run).
- **P1**: first run if the wave has room; expected in the same release.
- **P2**: designed for, built after P0/P1 are green.
- Size: **S** < 1 day, **M** 1–3 days, **L** 3–7 days, **XL** > 1 week of agent time.

---

## 2. Hard constraints from the existing codebase

| Constraint | Consequence |
|---|---|
| D58/D62: FFmpeg is neither shipped nor used | Export is an in-process native pipeline: `mfdec` decode → `gpuconv`/compositor → `hwenc` → `mp4` mux, audio through a new AAC decode → mixer → `aacenc`. |
| `mfdec` decodes video and probes audio but **cannot decode audio** | New: Media Foundation audio source reader (AAC/MP3/WAV/M4A → f32 PCM). Needed for waveforms, mixing, music import, loudness. |
| `hwenc`/`aacenc`/`mp4` produce H.264/HEVC + AAC in MP4 only (D1) | Export container is **MP4 only**; codecs H.264, HEVC, (AV1 hardware only, D11, if `hwenc` gains it). No WebM/MKV/MOV export in the first run. |
| D4: `clipcheckd` is the single writer of `library.db` | Export runs in the UI process and registers the file through IPC (`RegisterEditedRecording`, extended with kind `export`). The editor never writes the DB. |
| D6/D18: sources have closed-GOP keyframes at ~segment boundaries | Enables the **smart/lossless export fast path** (§8.6) when a timeline is cuts-only. |
| Recorder must not be disturbed by editing | Export threads run at below-normal priority, and by default **pause while a game recording is active** (§8.9). |
| WebView2 plays H.264 (and HEVC only if the OS codec exists) | Preview uses the existing direct-play/proxy policy (08 §2.3); export always reads originals. |

---

## 3. Architecture

```
 UI (Svelte)                                   Tauri commands (UI process)                 Native crates
 ┌───────────────────────────┐   invoke/events  ┌──────────────────────────────┐
 │ Editor view               │ ───────────────► │ editor_* commands            │
 │  Media bin  Timeline      │                  │  project load/save/autosave  │──► timeline (pure model,
 │  Preview    Inspector     │ ◄─────────────── │  waveform / filmstrip jobs   │     ops, undo, validation)
 │  Export dialog            │  progress events │  export job manager          │──► render (export engine)
 └───────────────────────────┘                  └──────────────┬───────────────┘      ├ mfdec  video + NEW audio decode
        │ preview: <video> pool + Web Audio                    │ IPC                  ├ gpuconv scale/letterbox/transform
        │ via clip:// range protocol                           ▼                      ├ hwenc  H.264/HEVC encode
        ▼                                         clipcheckd (single DB writer)       ├ aacenc AAC encode
  originals / proxies                             RegisterEditedRecording(kind=export)└ mp4    mux / lossless remux
```

### 3.1 New and changed crates

| Crate | Status | Contents |
|---|---|---|
| `timeline` | NEW, pure Rust, no I/O, `#![forbid(unsafe_code)]` | Project/timeline data model, edit operations (split, trim, move, ripple, group…), undo/redo command stack, snapping, validation, JSON (de)serialisation with `schema_version` + migrations, "render plan" derivation (flattened per-time layer/clip/audio stack used by both preview and export). |
| `render` | NEW | Export engine: job config, decode scheduling per clip, compositor, audio mixer, encoder feed, progress/cancel, smart-render eligibility, still-frame and audio-only export. |
| `mfdec` | EXTEND | `AudioDecoder` (PCM f32 interleaved, resample to 48 kHz stereo, seek), `waveform_peaks()`. |
| `gpuconv` | EXTEND | Canvas compositing: scale modes fit/fill/stretch, crop, position, rotate-90/flip, opacity, solid background, image/text-texture overlay blit. |
| `hwenc` / `encode` | EXTEND | CQ/target-bitrate/CBR/VBR control for offline (non-realtime) encode; configurable GOP; optional AV1 HW. |
| `mp4` | EXTEND | Mux with arbitrary audio-track count/titles, `+faststart`, multiple audio tracks preserved on request. |
| `clipedit` | KEEP | Quick trimmer unchanged. `plan::suggest_output_name` reused for naming. |
| `library` / `core` | EXTEND | `storage.export_dir`, kind `export`, gallery filter, `[editor]`/`[export]` config sections and validation codes. |
| `ipc` / `clipcheckd` | EXTEND | `RegisterEditedRecording` gains `kind: export` + `project_id`; `RecordingAdded` fires as usual. |

### 3.2 Source-of-truth rule

The **Rust `timeline` render plan is the single definition of "what the output is"**. Preview (JS) implements a subset for speed; every behaviour preview shows must match export. A golden-vector test set (JSON timelines → expected per-time layer/gain tables) is run in both Rust (`cargo test`) and the UI (`vitest`) so the two cannot drift (§13).

---

## 4. Data model (`timeline` crate)

All times are an exact integer `Tick` (newtype over `i64`), **254,016,000,000 ticks per second** (section 14.2, M1): every common frame rate and audio rate divides it with no remainder, so edits never drift or round. The model has no floats for time. Field names below ending in `_us` are `Tick`s.

```
Project
  schema_version: u32
  id: Uuid, name, created_ms, modified_ms
  canvas { width, height, fps_num, fps_den, background: Rgba, aspect_preset }
  media: [MediaRef]                  // the media bin
  tracks: [Track]                    // ordered bottom→top for video, top→bottom for audio
  groups: [Group]
  markers: [Marker]
  work_area: Option<{in_us, out_us}> // export range
  export_defaults: ExportSettings    // last used for this project
  ui_state { zoom, scroll, playhead_us }   // cosmetic, restored on open

MediaRef { id, kind: video|audio|image, path, library_recording_id: Option, 
           fingerprint {size, mtime_ms, duration_us}, probe: {w,h,fps,codec,audio_tracks:[{title,codec,ch,rate}]},
           missing: bool }

Track { id, kind: video|audio, name, index, muted, solo, locked, hidden(video only),
        volume_db (audio), height_px (cosmetic) }

Clip { id, track_id, media_id, 
       timeline_start_us, source_in_us, source_out_us,        // duration = out-in (before speed)
       speed: f64 (1.0), reverse: bool (P2),
       // video
       transform { scale_mode: fit|fill|stretch|custom, scale, pos_x, pos_y, rotate_90, flip_h, flip_v, crop {l,t,r,b}, opacity },
       // audio (clips on audio tracks, or the embedded audio of a video clip)
       audio { source_track_index, gain_db, muted, pan, fade_in_us, fade_out_us, envelope: [(t_us, gain_db)] },
       link_id: Option<Uuid>,       // A/V link (see groups)
       group_id: Option<Uuid>,
       enabled: bool, label_color, 
       transition_in: Option<Transition> }

Transition { kind: crossfade|dip_to_black|dip_to_white, duration_us }   // P1

TitleClip / ImageClip: Clip variants (text: string, font, size, colour, outline, alignment, position, box background; image: media_id + transform)

Group { id, name, member_clip_ids }       // user group; Ctrl+G
Marker { id, time_us, label, color }
ExportSettings { see §8 }
```

Design notes:
- A **video clip from a ClipCheck recording** is inserted as a *linked* pair: one video clip plus one audio clip **per source audio track** (Mix, Game, Mic… from the recording's track plan, D30). Only the Mix lane is enabled by default; the other lanes are inserted muted/collapsed so the user can un-mute a Mic or Game lane without re-importing. This is the ClipCheck-specific advantage over generic editors.
- Linked pairs share `link_id` (soft link, toggled with Alt-click / Ctrl+Shift+L). User groups (`group_id`) are a separate, stronger concept (see F-GRP).
- Clip overlap on one track is not allowed (§F-TL); overlaps for crossfades are modelled as `transition_in` consuming handle frames, not as overlapping clips.
- Undo/redo: command pattern, each command has `apply`/`revert`, coalescing for drags, depth 200, persisted across autosave only within a session.

---

## 5. Feature specification

IDs are stable; each becomes a test target. "Where" says the layer that owns it.

### 5.1 Project and media management

| ID | Feature | Pri | Size | Where |
|---|---|---|---|---|
| F-PRJ-1 | New project (empty, or from selected gallery clips); name; canvas chosen on creation (default = first clip's size/fps, snapped to presets) | P0 | M | timeline, UI |
| F-PRJ-2 | Save / Save As / Open / Recent projects list (Editor home screen); projects stored in `%LOCALAPPDATA%\ClipCheck\projects\<id>.ccproj` (JSON, human-readable, atomic write via `.part` + rename) | P0 | M | UI cmd |
| F-PRJ-3 | Autosave every `editor.autosave_s` (default 30 s) to a sidecar `<id>.autosave`; crash recovery prompt on next open | P0 | M | UI cmd |
| F-PRJ-4 | Rename, duplicate, delete project (delete → Recycle Bin; never touches source clips) | P0 | S | UI cmd |
| F-PRJ-5 | Project references media by library id + path + fingerprint; **missing/changed media** shown with red badge, "Relink…" dialog (also relinks siblings in the same folder) | P0 | M | timeline, UI |
| F-PRJ-6 | Project settings dialog: canvas size/fps/background; changing canvas never moves clips, only recomputes fit | P0 | S | UI |
| F-PRJ-7 | Project templates / "Reset layout" | P2 | S | UI |
| F-PRJ-8 | "Collect project" (copy used media + project to a folder) | P2 | M | UI cmd |
| F-MED-1 | **Media bin**: grid/list of project media with thumbnail, name, duration, resolution, fps, codec, audio-track count, "used" tick; sort/search | P0 | M | UI |
| F-MED-2 | Add from ClipCheck library (picker reusing gallery filters/search/multi-select, cross-game) | P0 | M | UI |
| F-MED-3 | Import external files (drag-drop or picker): MP4/M4V/MOV (H.264/HEVC) video; MP3/WAV/M4A/AAC audio; PNG/JPG/BMP/WebP images. Import **references**, does not copy (copy-into-library is the existing D24 import) | P0 | M | UI, mfdec |
| F-MED-4 | Drag media to timeline / double-click to preview in source monitor; set in/out in the **source monitor** then Insert (`,`) or Overwrite (`.`) or Append | P0 | L | UI |
| F-MED-5 | Reveal in folder, replace media (relink to a different file keeping edits), remove unused | P1 | S | UI |
| F-MED-6 | Unsupported files get a clear reason (codec/container) and an offer to import via the existing proxy path where it exists | P1 | M | UI |

### 5.2 Timeline editing

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-TL-1 | Multi-track timeline: default V1,V2,V3 + A1–A4 (add/remove tracks up to **6 video + 8 audio**); video tracks stack with higher on top; track header with name, mute, solo, lock, hide, height toggle | P0 | L |
| F-TL-2 | Time ruler with timecode `HH:MM:SS:FF`, adaptive tick density; playhead scrub; click ruler to seek | P0 | M |
| F-TL-3 | Zoom (Ctrl+wheel, `+`/`-`, slider), fit-to-window (`\`), zoom-to-selection; horizontal scroll (wheel/shift-wheel/drag-pan); vertical scroll for tracks | P0 | M |
| F-TL-4 | Selection tool (V): click, Shift/Ctrl multi-select, marquee select, select-all (Ctrl+A), select track forward (A), select clips at playhead | P0 | M |
| F-TL-5 | Move clips by drag within and across tracks (type-compatible only); collision = reject or push depending on mode; multi-clip move keeps relative offsets | P0 | L |
| F-TL-6 | **Trim edges** by dragging clip start/end (respects source bounds); **ripple trim** (modifier) closes/opens the gap and shifts everything after; **roll trim** (edit-point between two adjacent clips) | P0 | L |
| F-TL-7 | **Split at playhead** (`Ctrl+K` / `S`): splits selected clips, or all unlocked tracks if none selected; Cut tool (`C`) splits where clicked | P0 | M |
| F-TL-8 | Delete (gap remains) and **Ripple delete** (`Shift+Delete`, closes gap); delete gap (right-click) | P0 | M |
| F-TL-9 | Duplicate (Alt-drag or Ctrl+D), copy/cut/paste at playhead (Ctrl+C/X/V), paste onto selected track | P0 | M |
| F-TL-10 | **Snapping** (toggle `N`): to playhead, clip edges, markers, in/out, 0; visible snap guide; snap threshold 8 px; hold Alt to disable temporarily | P0 | M |
| F-TL-11 | **Magnetic / ripple mode** toggle: on = clips on a track auto-close gaps (Medal/CapCut feel); off = free placement (Premiere feel). Default per `editor.magnetic_default` (true) | P0 | L |
| F-TL-12 | Undo / redo (Ctrl+Z / Ctrl+Y or Ctrl+Shift+Z), history panel optional (P2) | P0 | L |
| F-TL-13 | Insert vs overwrite edit modes when dropping clips onto occupied time | P1 | M |
| F-TL-14 | Slip (change source in/out inside the same slot) and Slide tools | P2 | M |
| F-TL-15 | Markers (`M`): add/rename/colour/delete, jump prev/next (Shift+↑/↓ or `[`/`]`-style), listed in a Markers panel | P1 | M |
| F-TL-16 | **Work area / in-out range** (`I` / `O` on timeline, clear with `Alt+X`): drives export range and loop playback | P0 | S |
| F-TL-17 | Lift / Extract the in-out range (remove range, with/without closing gap) | P1 | M |
| F-TL-18 | Clip labels colour; clip name overlay; show/hide thumbnails and waveforms per track | P1 | S |
| F-TL-19 | Gap indicators; "Close all gaps" on a track | P1 | S |
| F-TL-20 | Freeze frame at playhead (inserts a still of N seconds), "Extend frame" | P1 | M |
| F-TL-21 | Join through-edit (merge two adjacent clips that continue the same source) | P2 | S |
| F-TL-22 | Auto-ripple on insert for magnetic mode, right-to-left nudge keys (`,` `.` = nudge 1 frame, Shift = 10 frames) | P1 | S |

### 5.3 Groups and linking (explicit user requirement)

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-GRP-1 | **Group** selected clips (`Ctrl+G`): a group moves, trims-as-block (only the outer edges), splits, deletes, copies and duplicates together; selecting any member selects all members; group shown with a shared coloured bracket and a name | P0 | L |
| F-GRP-2 | **Ungroup** (`Ctrl+Shift+G`): members become independent; selection retained | P0 | S |
| F-GRP-3 | Enter group (double-click) to select/edit a single member without ungrouping; Esc leaves | P0 | M |
| F-GRP-4 | Groups may span tracks and kinds (video + audio + title); a clip is in at most one group (no nesting in the first run) | P0 | S |
| F-GRP-5 | **A/V link** (separate from groups): video clip and its audio lane(s) are linked by default; move/split/delete act on both; **Unlink** (`Ctrl+Shift+L`) lets you slide audio against video, **Link** re-links selected | P0 | M |
| F-GRP-6 | "Detach audio" convenience: unlink + mute the embedded audio lane(s) in one step | P0 | S |
| F-GRP-7 | Group-level volume (applies a gain offset to all audio members) and group-level speed (P2) | P1 | M |
| F-GRP-8 | Group rename + colour; groups listed in the Inspector | P1 | S |
| F-GRP-9 | Nested groups, groups-as-compound-clips | P2 | L |

### 5.4 Audio editing (explicit user requirement)

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-AUD-1 | Every recording's audio tracks appear as separate lanes (see §4); Mix lane on, others muted but present and toggleable per clip | P0 | M |
| F-AUD-2 | **Per-clip volume** slider/field in dB (−60…+12), mute toggle, reset | P0 | S |
| F-AUD-3 | **Per-track volume** (fader in the track header), mute, solo, with a live **level meter** during preview | P0 | M |
| F-AUD-4 | **Master volume** + master meter + clip indicator; true-peak limiter on export (default on, ceiling −1 dBFS) so +dB never clips hard | P0 | M |
| F-AUD-5 | **Waveforms** drawn on audio clips (peaks computed once by the background job, cached in `%LOCALAPPDATA%\ClipCheck\cache\peaks\<fingerprint>.pk`); zoom-aware | P0 | L |
| F-AUD-6 | **Fade in / out** handles on clip corners (linear / equal-power), numeric in Inspector | P0 | M |
| F-AUD-7 | **Volume envelope** (rubber-band): add/move/delete points on the clip gain line, ducking music under voice; linear interpolation in dB | P1 | L |
| F-AUD-8 | Audio-only clips: import music/SFX (MP3/WAV/M4A) onto audio tracks; loop to length (P1) | P0 | M |
| F-AUD-9 | **Pan** (−100…+100) and mono→stereo / stereo→mono downmix per clip | P1 | S |
| F-AUD-10 | **Normalise** clip or selection to a target (peak −1 dBFS, or loudness −14/−16 LUFS) one-click; "Auto-balance" suggests gains for Game vs Mic | P1 | L |
| F-AUD-11 | **Extract / detach audio** as its own clip on a new audio track (video keeps playing silent) | P0 | S |
| F-AUD-12 | **Mic/game balance quick control**: one slider on a recording clip that trades Mic vs Game lanes (ClipCheck-specific) | P1 | S |
| F-AUD-13 | Speed change with audio: P0 = resample (pitch follows speed, labelled), P1 = pitch-preserving time-stretch (WSOLA) | P0/P1 | M/L |
| F-AUD-14 | Reverse audio with reverse video | P2 | S |
| F-AUD-15 | Simple EQ presets (low-cut, voice boost), noise gate re-use of the recorder's gate, de-noise | P2 | L |
| F-AUD-16 | Voice-over recording directly onto a track | P2 | L |
| F-AUD-17 | **Export audio layout**: single stereo Mix (default) or **keep N separate tracks** (each audio track on the timeline exported as its own AAC track, titled) | P0 | M |
| F-AUD-18 | Beat/marker auto-sync, ducking automation, audio crossfades between adjacent audio clips | P2 | M |

### 5.5 Video editing, canvas and framing

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-VID-1 | **Canvas presets**: 16:9 (1920×1080, 2560×1440, 3840×2160, 1280×720), 9:16 vertical (1080×1920), 1:1 (1080×1080), 4:5 (1080×1350), 21:9, 4:3, **custom W×H** (even numbers, 2…8192 per side, validated), lock-aspect toggle | P0 | M |
| F-VID-2 | Project frame rate: 24 / 25 / 30 / 50 / 60 / 120 (and "match first clip"); source clips with other fps are resampled by repeat/drop (no optical flow) | P0 | M |
| F-VID-3 | **Fit modes** per clip when aspect differs from canvas: Fit (letterbox), Fill (crop-to-fill), Stretch, Custom; background = solid colour (P0) or **blurred copy of the clip** (P1; counts as a compositing mode, not a user effect) | P0/P1 | M |
| F-VID-4 | Transform tools in preview: drag to reposition, handles to scale, rotate in 90° steps, flip H/V, numeric fields, reset; safe-area / centre guides | P0 | L |
| F-VID-5 | **Crop** (L/T/R/B numeric + on-canvas handles) | P0 | M |
| F-VID-6 | **Speed**: constant 0.1×–8× per clip (Inspector + Ctrl+R-style dialog), ripple the clip length accordingly; slow-motion uses frame repeat (no interpolation) | P0 | M |
| F-VID-7 | Reverse clip | P2 | M |
| F-VID-8 | **Opacity** per clip (0–100 %), blend = normal only | P1 | S |
| F-VID-9 | Picture-in-picture: V2 clip with transform over V1 (falls out of F-VID-4 + F-VID-8 + multi-track) | P1 | M |
| F-VID-10 | Multi-track compositing order and `hidden` track toggle | P0 | M |
| F-VID-11 | Still frame export to PNG/JPG from playhead (`Ctrl+Shift+S`), quality option | P1 | S |
| F-VID-12 | Gaps render as the canvas background colour (black by default) | P0 | S |
| F-VID-13 | Per-clip enable/disable (mute video + audio, keep on timeline) | P0 | S |
| F-VID-14 | Cursor/aspect-aware auto-reframe for vertical (centre-crop with manual offset keyframes) | P2 | L |
| F-VID-15 | Colour: basic brightness/contrast/saturation | P2 (effects line) | M |
| F-VID-16 | Black / colour / checkerboard generator clips ("solid") | P1 | S |

### 5.6 Transitions, titles and overlays (structural, not effects)

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-TRN-1 | **Cut** (default) | P0 | — |
| F-TRN-2 | **Crossfade** (video blend + audio equal-power crossfade) at the junction of two adjacent clips; duration 0.1–5 s; drag to resize; needs handle frames, otherwise clamps | P1 | L |
| F-TRN-3 | **Fade from/to black** at project start/end and per clip (video opacity + audio fade tied together) | P1 | M |
| F-TRN-4 | Dip to white | P2 | S |
| F-TXT-1 | **Text clip** on a video track: text, font (system fonts via DirectWrite), size, bold/italic, colour, outline, shadow, alignment, box background, position; rendered to a texture on the GPU | P1 | L |
| F-TXT-2 | Text presets (title, lower third, caption bar), duration handles, fade in/out | P2 | M |
| F-IMG-1 | **Image clip** (PNG with alpha, JPG): transform, opacity, duration; usable as watermark/logo | P1 | M |
| F-IMG-2 | Reusable "watermark" preset (position corner, opacity, margin) saved to config | P2 | S |

### 5.7 Playback and preview

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-PRV-1 | Preview monitor with play/pause (`Space`/`K`), `J`/`K`/`L` shuttle, frame step (`←`/`→`), 1 s step (`Shift+←/→`), jump to prev/next edit (`↑`/`↓`), Home/End, loop in-out | P0 | L |
| F-PRV-2 | Timeline playback across cuts and across multiple clips with audio in sync; target ≥ 30 fps preview at 1080p on a mid GPU; drop frames, never audio | P0 | XL |
| F-PRV-3 | Preview quality selector Full / ½ / ¼ (decode proxy size) to keep up on weak machines; auto-downgrade on dropped frames | P1 | M |
| F-PRV-4 | Audio preview identical to export mix semantics (gain, fades, envelope, pan, solo/mute) through Web Audio, shared gain-table from the render plan | P0 | L |
| F-PRV-5 | Safe-area and grid overlays, canvas boundary shown for out-of-canvas content, fit/100 %/200 % zoom of the monitor | P1 | S |
| F-PRV-6 | Timeline thumbnails/filmstrip per video clip (reuses the sprite sheet/`filmstrip.ts` pipeline where present, else decode job), generated lazily | P0 | M |
| F-PRV-7 | Fullscreen preview | P1 | S |
| F-PRV-8 | Source monitor (media-bin clip with its own in/out) separate from the program monitor | P0 | M |
| F-PRV-9 | Proxy playback for HEVC when WebView2 cannot decode (reuse 08 §5.6) | P1 | M |

### 5.8 Editor UX

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-UX-1 | Layout: left media bin / middle preview / right inspector / bottom timeline; resizable splitters, remembered; panels collapsible | P0 | L |
| F-UX-2 | Inspector shows properties of the selection (clip, track, group, project) with numeric inputs and reset buttons | P0 | L |
| F-UX-3 | Tools: Select (V), Cut (C, internal tool id `razor`), Hand (H), Zoom (Z); on-screen toolbar mirrors keys | P0 | S |
| F-UX-4 | Context menus on clips, tracks, gaps, ruler, media | P0 | M |
| F-UX-5 | Full keyboard map (§6), shown in a Shortcuts overlay (`?`) | P0 | M |
| F-UX-6 | Unsaved-changes guard on close/navigate; autosave status indicator | P0 | S |
| F-UX-7 | Light/dark follows app theme; accessibility labels; all timeline ops also reachable from menus | P1 | M |
| F-UX-8 | Drag a clip from the Gallery straight into an open editor | P1 | S |
| F-UX-9 | "Open in editor" from the player carries the trimmer's in/out as the clip's source range | P0 | S |
| F-UX-10 | Quick start: "Edit these 3 clips" from a gallery multi-select creates a project with clips appended in selection order on V1 | P0 | S |
| F-UX-11 | Errors never lose work: failed decode shows a placeholder clip ("media error") and continues | P0 | M |

### 5.9 Export (explicit user requirement)

See §8 for the full dialog, presets and location rules. Summary of capabilities:

| ID | Feature | Pri | Size |
|---|---|---|---|
| F-EXP-1 | Export dialog: preset, resolution, aspect/canvas, fps, codec, quality mode, audio, range, name, location | P0 | L |
| F-EXP-2 | **Resolution**: Match canvas, or scale to 4K / 1440p / 1080p / 720p / 480p / 360p / custom W×H (aspect locked by default, letterbox/crop per fit mode when changed) | P0 | M |
| F-EXP-3 | **Quality modes**: Quality slider (constant-quality), Target bitrate (VBR, kbps/Mbps), CBR, **Target file size** (MB; solves bitrate from duration and audio bitrate) | P0 | M |
| F-EXP-4 | **Codec**: H.264 (default), HEVC; AV1 shown only when the machine's hardware encoder supports it (D11) | P0 | M |
| F-EXP-5 | **Encoder**: Auto (hardware first) / chosen GPU encoder / software; fallback message if HW fails | P0 | M |
| F-EXP-6 | **FPS**: project fps, or 24/30/60 override | P0 | S |
| F-EXP-7 | **Audio**: AAC bitrate 96/128/192/256/320 kbps, sample rate 44.1/48 kHz, stereo mixdown or keep separate tracks, loudness normalise on export (off/−14/−16 LUFS), peak limiter | P0 | M |
| F-EXP-8 | **Range**: entire timeline / work area / selected clips | P0 | S |
| F-EXP-9 | **Presets** (§8.3): Original quality, High 1080p60, Balanced 1080p30, Small, Discord (≤10 MB / ≤50 MB / ≤500 MB), YouTube, Shorts/TikTok/Reels vertical, Twitter/X, GIF-like short loop (P2); user can **save custom presets** | P0 | M |
| F-EXP-10 | **Export location setting** (§8.1): new `storage.export_dir`, default `<storage.root>\Exports\` — **flat, never grouped by game**; per-export override with a Browse button; last location remembered | P0 | M |
| F-EXP-11 | Filename template `{project} {yyyy-MM-dd HH-mm-ss}` (editable, `{project}`, `{date}`, `{time}`, `{res}`, `{preset}`, `{n}` tokens), illegal-char sanitising, collision-safe numbering, never overwrites silently | P0 | S |
| F-EXP-12 | **Progress**: percent, elapsed, ETA, current speed (× realtime), output size estimate, Cancel (≤ 500 ms), partial `.part` file removed on cancel/failure | P0 | M |
| F-EXP-13 | **Background export**: user can keep editing or leave the editor; a status pill in the title bar shows progress; export uses a *snapshot* of the project so later edits don't affect it | P0 | L |
| F-EXP-14 | **Export queue**: multiple jobs run one at a time, reorderable, cancellable, survive navigation (not app restart in v1) | P1 | M |
| F-EXP-15 | On finish: toast "Open / Show in folder / Play", auto-registers in library (`kind=export`, `parent`=project, shows in gallery under the Exports filter), thumbnail generated | P0 | M |
| F-EXP-16 | **Smart lossless export** when the timeline is cuts-only from same-codec/res/fps sources (§8.6): "Export without re-encoding" button; fast and exact quality | P1 | L |
| F-EXP-17 | Export a **still frame** (PNG/JPG) and **audio only** (M4A AAC) | P1 | M |
| F-EXP-18 | **Estimated file size** and a warning chip when a preset's size limit (Discord) will likely be exceeded; for target-size mode, a verify pass reports the achieved size | P0 | M |
| F-EXP-19 | Hardware-encoder session limits handled (NVENC concurrent sessions): queue serialises, never errors with a cryptic MF code | P0 | S |
| F-EXP-20 | Post-export actions: open folder, copy file to clipboard (for pasting into Discord), delete project export, re-export with same settings | P1 | S |
| F-EXP-21 | Two-pass / slow-quality encode option ("Higher quality, slower") where the encoder supports it | P2 | M |
| F-EXP-22 | GIF / WebM / MOV / MKV export | P2 | L (not first run; container work) |
| F-EXP-23 | Export-time pause while recording & throttle toggle (§8.9) | P0 | M |

### 5.10 Settings (Settings → Editor & Export)

| ID | Setting | Default |
|---|---|---|
| F-SET-1 | `storage.export_dir` (Browse, Reset, "Open folder") | `<storage.root>\Exports` |
| F-SET-2 | `editor.autosave_s` | 30 |
| F-SET-3 | `editor.magnetic_default`, `editor.snap_default` | true / true |
| F-SET-4 | `editor.default_canvas` (`match_first_clip`, or preset) | `match_first_clip` |
| F-SET-5 | `editor.preview_quality` (`auto`, `full`, `half`, `quarter`) | `auto` |
| F-SET-6 | `editor.cache_dir`, `editor.cache_max_gb` (peaks, thumbnails, proxies) with "Clear cache" | `%LOCALAPPDATA%\ClipCheck\cache`, 5 |
| F-SET-7 | `export.default_preset`, `export.encoder` (`auto`/…), `export.pause_while_recording`, `export.open_folder_when_done`, `export.filename_template`, `export.low_priority` | `balanced_1080p`, `auto`, true, false, `{project} {yyyy-MM-dd HH-mm-ss}`, true |
| F-SET-8 | `export.custom_presets` (array of tables) | `[]` |
| F-SET-9 | Shortcut customisation for editor keys | P2 |

---

## 6. Keyboard map (editor focus only; never global hotkeys)

| Key | Action | Key | Action |
|---|---|---|---|
| Space / K | Play / pause | Ctrl+Z / Ctrl+Y | Undo / redo |
| J / L | Shuttle back / forward (tap to step speed) | Ctrl+S | Save project |
| ← / → | Frame back / forward | Ctrl+E | Open Export dialog |
| Shift+← / → | ±1 s | Ctrl+Shift+S | Export still frame |
| ↑ / ↓ | Previous / next edit point | Ctrl+K or S | Split at playhead |
| Home / End | Start / end of timeline | C | Cut tool |
| V | Selection tool | H / Z | Hand / Zoom tool |
| I / O | Set in / out point | Alt+X | Clear in/out |
| M | Add marker | Delete | Delete (leaves gap) |
| N | Toggle snapping | Shift+Delete | Ripple delete |
| Ctrl+G / Ctrl+Shift+G | Group / ungroup | Ctrl+Shift+L | Link / unlink A/V |
| Ctrl+C / X / V | Copy / cut / paste | Ctrl+D | Duplicate |
| + / − / \ | Zoom in / out / fit | Ctrl+A | Select all |
| `,` / `.` | Insert / overwrite from source | Esc | Leave group / cancel drag |

---

## 7. UI surfaces (Svelte, `apps/ui/src/lib/editor/`)

- `EditorHome.svelte` — recent projects, New, "Edit from library".
- `Editor.svelte` — shell and splitters; owns the open project store and undo stack mirror.
- `MediaBin.svelte`, `SourceMonitor.svelte`, `ProgramMonitor.svelte`, `Inspector.svelte`
- `Timeline.svelte` (canvas-rendered tracks for performance; DOM overlay for handles/context menu), `TrackHeader.svelte`, `Ruler.svelte`, `ClipView` (canvas drawing: thumbnails strip, waveform, name, fade handles, envelope)
- `ExportDialog.svelte`, `ExportQueue.svelte`, `ExportPill.svelte` (title bar)
- Stores/models under `editor/` mirror the Rust model (generated TS types from `timeline` via a checked-in JSON schema test to prevent drift).
- Reuse: `player/mixer.ts` (Web Audio graph) is generalised into an N-source timeline mixer; `player/filmstrip.ts` for thumbnails; `player/Timeline.svelte` is **not** reused (single-clip handles) but its time/format helpers are.

---

## 8. Export design

### 8.1 Export location (explicit requirement)

- New config key **`storage.export_dir`**, default `%USERPROFILE%\Videos\ClipCheck\Exports` (i.e. `<storage.root>\Exports`). Edited in Settings → Storage alongside the clips path.
- Exports are written **directly in that folder: no `<Game>` subfolder, no per-game grouping**, regardless of `storage.per_game_folders`. (`storage.per_game_folders` keeps applying to Clips/Sessions only.)
- If the user sets `export_dir` outside `storage.root`, the folder is automatically added as an extra library (D24 style, indexed in place, never moved) so exports still appear in the gallery.
- Export dialog: shows the resolved folder with **Change…**; choice is remembered per user (`export.last_dir`), "Always ask where to save" is a Settings checkbox (default off). Free-space check before start with a clear message if estimated size > free space.
- Library: new recording `kind = export`; the gallery gets an **Exports** filter/tab and the row shows an "Edited" badge and links to its project. `game` label = `Exports` (no real game folder involved).
- Scanner: `Exports\` is a first-class top-level folder like `Clips\` and `Sessions\`; the D24 junk skip list is unchanged. Retention/quota: exports count towards `storage.quota_gb` but are **excluded from auto-delete** by default (user-made work), with a Settings toggle.

### 8.2 Export dialog (exact fields)

```
 Export                                                         [x]
 Preset         [ Balanced 1080p30 ▾ ]        [Save as preset…]
 ───────────────────────────────────────────────────────────────
 Name           [ My montage 2026-10-13 21-04-55 ]
 Location       D:\Videos\ClipCheck\Exports   [Change…]
 Range          (•) Entire timeline  ( ) Work area  ( ) Selection
 ── Video ───────────────────────────────────────────────────────
 Resolution     [ 1920 x 1080 ▾ ]  W[1920] H[1080] 🔒   Canvas: 16:9
 Frame rate     [ 30 ▾ ]
 Codec          [ H.264 ▾ ]     Encoder [ Auto (NVIDIA NVENC) ▾ ]
 Quality        (•) Quality  [=====|---] 70   ( ) Bitrate [ 8 ] Mbps
                ( ) Target size [ 50 ] MB
 ── Audio ───────────────────────────────────────────────────────
 Tracks         (•) Mix to stereo  ( ) Keep 3 separate tracks
 Bitrate        [ 192 kbps ▾ ]   Sample rate [ 48 kHz ▾ ]
 Loudness       [ Off ▾ ]  [x] Peak limiter
 ───────────────────────────────────────────────────────────────
 Estimated size ~ 142 MB     Duration 00:03:42     Free space 212 GB
                                       [Cancel]  [Export without re-encoding]  [Export]
```

### 8.3 Built-in presets

| Preset | Res / fps | Video | Audio |
|---|---|---|---|
| Original quality | Canvas, source fps | H.264 CQ high (~90) | AAC 256 |
| High 1080p60 | 1920×1080 / 60 | H.264 CQ 80, ≤ 20 Mbps | AAC 192 |
| Balanced 1080p30 (default) | 1920×1080 / 30 | H.264 CQ 70, ~8 Mbps | AAC 192 |
| 1440p / 4K | 2560×1440, 3840×2160 / source | H.264 or HEVC CQ 75 | AAC 256 |
| Small file | 1280×720 / 30 | H.264 ~2.5 Mbps | AAC 128 |
| Discord ≤ 10 MB | 1280×720 or lower | target size 9.5 MB, 2-step res fallback | AAC 96 |
| Discord ≤ 50 MB | 1920×1080 / 30 | target size 48 MB | AAC 128 |
| Discord ≤ 500 MB | 1920×1080 / source | target size 480 MB | AAC 192 |
| YouTube | canvas / source | HEVC or H.264 per YouTube bitrate table | AAC 256 |
| Vertical (Shorts / TikTok / Reels) | 1080×1920 / 30–60 | H.264 ~10 Mbps | AAC 192 |
| X / Twitter | 1280×720 or 1920×1080 / 30–60, ≤ 140 s warning | H.264 ~6 Mbps | AAC 128 |
| Audio only | — | — | M4A AAC 192 |
| Still frame | canvas | PNG/JPG | — |

Presets are data (TOML in `core`), versioned, and shown with live size estimates.

### 8.4 Render pipeline (`render` crate)

1. **Snapshot** the project, derive the **render plan**: sorted list of time segments, each with the visible video layers (clip, source time, transform, opacity, crop) and audio contributors (clip, source track, gain curve, pan).
2. **Video thread**: for each output frame time `t`, ask per-clip `VideoDecoder`s (one live decoder per active clip, keyed by media; seek only at clip boundaries; keep ≤ 4 decoders open; share a decoder across back-to-back clips of the same media when contiguous) for the source frame at `src_time(t)` (speed applied); upload BGRA/NV12 to a D3D11 texture; `gpuconv` composites layers bottom-to-top on the canvas texture (fit/fill/crop/transform/opacity/background); overlays (text/image) are pre-rendered textures; then scale to the export resolution and feed the encoder. Frame repeat/drop for fps mismatch.
3. **Audio thread**: per-clip `AudioDecoder` → resample to export rate → apply speed (resample or WSOLA), gain/pan/fade/envelope → sum into per-track buses → master (limiter, optional LUFS normalise) → `aacenc` (one encoder per output track).
4. **Mux** through `mp4` into `<name>.mp4.part`; atomic rename on success (same discipline as D26).
5. **Progress**: output time / total, ~4 Hz, plus speed × realtime and ETA; cancel flag checked per frame and per audio block.
6. **Target-size mode**: bitrate = (target_bytes × 8 / duration) − audio bitrate − 2 % container overhead; CBR-ish rate control; after encode, if > target by > 3 %, re-run at reduced bitrate (max 2 retries) or report.
7. **Failure policy**: any decode error in one clip substitutes black/silence for the affected range and records a warning list shown at completion (never aborts the whole export for a single corrupt GOP unless nothing can be rendered).

### 8.5 Performance targets (reference machine, D17: RTX 5080)

- 1080p60 H.264 NVENC export of a single-layer timeline ≥ 4× realtime; with two overlapping layers ≥ 2× realtime.
- Software H.264 fallback ≥ 0.5× realtime at 1080p30.
- Memory ≤ 600 MB during export at 1080p, ≤ 1.2 GB at 4K.
- Export must not drop frames in a concurrently running recorder (see §8.9).

### 8.6 Smart lossless export (no re-encode)

Eligible when: every video segment is a straight cut (no transform, speed 1.0, opacity 100 %, no overlays/transition), all clips share codec/profile/resolution/fps/SPS-compatible parameters, canvas equals source, and audio is unmodified (gain 0 dB, no fades) or is only a track selection. Then `mp4::remux::concat_remux` stitches ranges, snapping each cut start back to the preceding keyframe. UI shows "Snaps start of clip 2 back by 1.3 s" like the trimmer's snap readout. Anything not eligible disables the button with the first blocking reason ("Clip 2 has a different resolution").

### 8.7 Output properties

MP4, faststart, H.264 High/HEVC Main, AAC-LC; track titles retained when exporting separate tracks; metadata title = project name; creation time = export time. 8-bit 4:2:0 only.

### 8.8 Limits (validated, CFG codes in `core`)

Width/height even, 128…8192; fps 1…120; project duration ≤ 4 h; ≤ 6 video + 8 audio tracks (D9-consistent); export bitrate 0.1…200 Mbps; target size 1 MB…64 GB.

### 8.9 Not disturbing the game

Export threads use below-normal priority and the encoder session is created on a lower-priority queue where the vendor allows. If `export.pause_while_recording` (default true) and the daemon reports `recording`/`buffering-active-game`, a running export **pauses at the next frame boundary** with a visible "Paused while a game is running" state and resumes automatically; the user can override per export ("Export anyway"). Editing and preview remain available.

---

## 9. Persistence, IPC and config changes

- **Project file** `*.ccproj` JSON, `schema_version: 1`, UTF-8, pretty-printed, atomic write; unknown fields preserved on load/save for forward compatibility; migrations in `timeline::migrate`.
- **IPC** (`ipc` crate): `RegisterEditedRecording { path, kind: "export", project_id, title, parent_ids[] }` (extends D4/D5). Existing `RecordingAdded` event unchanged. No other new daemon traffic.
- **Tauri commands** (UI process): `editor_list_projects`, `editor_open_project`, `editor_save_project`, `editor_autosave`, `editor_delete_project`, `editor_probe_media`, `editor_import_media`, `editor_waveform(media_id, from, to, buckets)`, `editor_thumbnails(media_id, ...)`, `editor_estimate_export`, `editor_check_smart_export`, `export_start`, `export_cancel`, `export_pause`, `export_resume`, `export_queue`, `export_still`; events `export-progress`, `export-done`, `export-failed`, `export-paused`, `peaks-ready`, `thumbs-ready`.
- **Config** (`core::config`): `storage.export_dir`; `[editor]`; `[export]` (+ `[[export.custom_presets]]`). New validation codes `CFG-X001…` (resolution, bitrate, target size, dir not writable). Old configs load unchanged; saving adds the new keys.
- **Library**: schema addition `recordings.kind` value `export`, nullable `project_id`; gallery filter; thumbnail generation reuses the existing job.

---

## 10. Phased delivery (waves, following `haiku-swarm`)

Each wave ends with the full verification suite (CLAUDE.md rule 5) and a commit. Parallel agents only on disjoint files.

| Wave | Scope | New/changed files (owner per file) | Done when |
|---|---|---|---|
| **E0 Decisions + scaffolding** | D66–D70 recorded; crates `timeline`, `render` created as stubs; config keys + validation + `Exports` dir + library `kind=export`; UI route and empty Editor shell | `DESIGN.md`, `core`, `library`, stubs | builds, tests green, Settings shows Export folder |
| **E1 Model** | `timeline` crate: model, JSON, ops (split/trim/move/ripple/delete/group/link/speed), snapping, undo/redo, render-plan derivation, golden vectors | `crates/timeline/*` | ≥ 150 unit tests incl. property tests (no overlaps, undo∘do = id) |
| **E2 Audio engine** | Spike first: confirm Media Foundation decodes MP3, WAV, M4A and FLAC on the dev machine and record the result in D-notes (Vorbis expected to need an OS extension, so out of scope). Then `mfdec::AudioDecoder`, peaks, resample, mixer (gain/pan/fade/envelope/limiter/LUFS), speed | `mfdec`, `render::audio` | PCM golden tests; waveform job ready |
| **E3 Export MVP** | `render` video path: decode → compose (fit/fill/crop/transform) → encode → mux; export job manager, progress, cancel, pause-while-recording, register via IPC, atomic output, location rules | `render`, `gpuconv`, `hwenc`, `apps/ui/src-tauri/src/export.rs` | Cuts-only 3-clip project exports; A/V sync ≤ 40 ms; plays in WebView2 |
| **E4 Editor UI core (parallel with E3)** | Shell, media bin, timeline canvas, ruler, select/cut, trim, split, delete, ripple, snap, undo/redo, save/load/autosave | `apps/ui/src/lib/editor/*` | Edit and save/reopen a project |
| **E5 Preview** | Program/source monitor, `<video>` pool scheduler, Web Audio timeline mixer, meters, shuttle, waveforms, filmstrips | `editor/preview*`, `mixer` generalisation | 3-clip timeline plays in sync; golden gain-table parity with Rust |
| **E6 Groups, audio UI, inspector** | Group/ungroup, A/V link, detach audio, per-clip/track/master volume, fades, envelope, normalise, inspector | UI + `timeline` | All F-GRP and F-AUD P0 pass |
| **E7 Export dialog + presets** | Dialog, presets, target-size solver, estimate, queue, pill, post-export actions, audio layout | UI + `render` | Discord ≤ 10 MB preset lands under 10 MB on the corpus |
| **E8 Canvas, transform, speed, vertical** | Canvas presets, transform handles, crop, speed, opacity, PiP | UI + `render` + `gpuconv` | Vertical 9:16 export of a 16:9 clip verified visually and by pixel tests |
| **E9 Transitions, text, image, smart export** | Crossfade, fades, text/image clips, smart lossless export, still/audio-only | `render`, UI | F-TRN/F-TXT/F-IMG/F-EXP-16/17 |
| **E10 Hardening** | Perf, memory, long-project (1 h, 200 clips) soak, relink, error placeholders, installer/README/DESIGN updates | all | Targets in §8.5 met; NOT-verified list empty or accepted |

Critical path: E1 → E3 and E1 → E4/E5; E2 → E3 (audio) and E5 (waveforms). E3 and E4 run in parallel after E1.

---

## 10b. Known limitations (as built)

- **Preview playback is laggy** (about 10 fps reported on a 1280x720 60 fps recording with several audio lanes). Not fixed; see DESIGN.md D77 for what was tried, the `previewStats` readout, the `MULTI_STREAM_AUDIO` bisect switch and the suggested next steps (proxy files or a native preview path). Export speed is a separate path and is not affected.

## 11. Risks

| Risk | Mitigation |
|---|---|
| Preview in WebView2 cannot match export frame-exactly or stay in sync across clip boundaries | Export is the source of truth (§3.2); preview uses a clock-driven `<video>` pool with pre-seek of the next clip 1 s ahead; golden-vector parity tests; fall back to native decoded-frame preview over a custom protocol (E5b) if sync fails the acceptance test. |
| Native audio decode/time-stretch/loudness is new code with no FFmpeg to lean on | Isolated in `render::audio`, golden PCM tests, WSOLA deferred to P1, LUFS (BS.1770) implemented from the spec with test vectors. |
| Compositor GPU work contends with a running game | §8.9 pause-while-recording, below-normal priority, preview defaults to reduced resolution while a game is running. |
| Hardware encoder session limits / vendor quirks in offline mode | Serial queue; automatic fall back to Microsoft software H.264 MFT with a visible notice; per-vendor tests on the D17 dev machine. |
| Target-size mode misses the cap (Discord) | Safety margin 2–5 %, post-encode verify, auto retry at lower bitrate (max 2). |
| Large projects (hours of footage, hundreds of clips) | Lazy thumbnails/peaks, virtualised timeline rendering, decoder pool cap, project size limits (§8.8). |
| Missing/moved source media after the Clips quota deletes originals | Relink flow; "Protect media used by projects" quota exemption (setting, default on); export gets a pre-flight "N clips missing" error. |
| Scope creep into effects | §1.3 is a hard line; additions require a new D-number. |
| HEVC sources not decodable by WebView2 | Existing proxy policy (08 §2.3/§5.6); export decodes via Media Foundation (HEVC decode needs the OS extension — pre-flight check with a clear Store link, same as the player). |

---

## 12. Open questions (defaults chosen; owner can overturn)

1. **Gallery presence of exports**: a new top-level "Exports" tab vs a filter chip. Default: filter chip + "Edited" badge.
2. **Quota**: should exports and media used by projects be exempt from auto-delete? Default: yes, both, toggleable.
3. **Export process isolation**: in the UI process (simple, D4-consistent) vs a child `ClipCheck.exe --export` (crash isolation, more IPC). Default: UI process for the first run; revisit if encoder crashes are seen.
4. **Maximum tracks / project length**: defaults in §8.8; fine?
5. **Pitch-preserving speed**: ship P0 resample-only first (default) or hold the speed feature until WSOLA lands?
6. **Naming**: "Editor" (default) vs "Studio".
7. **Windows 10 support**: same minimum as the app (2004+); software H.264 only on machines with no HW encoder.

---

## 13. Test strategy (separate chain per CLAUDE.md)

- `timeline`: unit + property tests (no overlap invariant, undo/redo identity, group/link semantics, snapping, render-plan golden vectors, JSON round-trip + migration fixtures).
- `render`/`mfdec`: synthetic media generated in-test (solid colours + sine tones with known frame numbers burnt in as pixel patterns) so output frames and audio can be asserted exactly; A/V sync, speed, crop/fit pixel checks, target-size accuracy, cancel latency, atomic `.part` handling, pause/resume.
- Contract tests: Rust golden vectors == UI vitest results for gain tables and visible-layer lists.
- UI: vitest for stores/ops, snapshot tests of timeline layout maths; manual smoke script in `scripts/` for the real-app export (never launches games or touches the user's recordings; uses repo fixtures only).
- Corpus: fixtures under `crates/testkit` (short H.264 clips, multi-audio-track clip, vertical clip, odd-size clip, VFR-ish, corrupt-tail file).
- Permanent tests never write outside temp dirs and never touch the real `Exports` folder.

---

## 14. Methods adopted from FilmCraft (and licensing / clean-room rules)

Reference: https://github.com/storytold/filmcraft, an open-source, pure-Rust non-linear editor, dual **MIT OR Apache-2.0**, whose README says "No FFmpeg inside" and that it contains no GPL/LGPL code. **How it was reviewed:** only its public README, AGENTS.md, ROADMAP.md, ATTRIBUTION.md and crate listing, read through a page summariser. Its source was **not** read, and CONTRIBUTING.md and docs/architecture.md could not be fetched, so claims about its internals are unverified. We adopt its *methods and rules*, not its code.

### 14.1 Licensing and clean-room policy for the editor (extends docs/LICENSING.md, D61)

| Rule | Detail |
|---|---|
| Specs, not source | Containers, codecs and DSP (BS.1770 loudness, resampling, limiter, WSOLA) are implemented from public specs (ISO/IEC, ITU-T, EBU, RFCs). The spec edition is recorded in each module README. We do **not** read or port GPL/LGPL/AGPL editors or codecs (FFmpeg, x264/x265, MLT, Kdenlive, Shotcut, Olive, LAME). |
| No look-alike assets | We reproduce *workflows* (what Medal, Premiere, Resolve and CapCut let you do), **never their icons, logos, fonts, presets, sounds, templates, or screenshots/layouts traced from them**. UI icons are drawn by us in SVG/code. Reference screenshots used while designing stay out of the repo (gitignored). |
| FFmpeg only as an oracle | FFmpeg/ffprobe may be used by a developer on their own machine to cross-check output in manual or `#[ignore]` tests. Never linked, bundled, downloaded by the app, or required by CI (consistent with D58/D62). |
| Dependency license gate | New crates must pass `cargo deny` against the existing allow-list in `deny.toml` (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, CC0, BSL-1.0; MPL-2.0 unmodified only). A pure-Rust audio decoder crate (e.g. `symphonia`, MPL-2.0) is **not planned**: audio decodes through the Media Foundation readers we already use (D58). If the E2 spike (below) shows an important format does not decode, the question returns to the owner with that evidence. Any new dependency needs a D-number and owner approval (CLAUDE.md rule 7). |
| Third-party code in the tree | Only if license-compatible, with the original copyright and permission notice kept in the file header and listed in `NOTICE`. None is planned. Reusing FilmCraft code (legal under its Apache-2.0 option) is **not planned**; any such reuse needs the owner's explicit approval, a pinned commit and a NOTICE entry. |
| Asset attribution | Every bundled asset (fonts, preset art, test media) has an `<asset>.attribution` sidecar (title, author, source, license, date) and a row in `ATTRIBUTION.md`; `scripts/check-assets.mjs` fails if one is missing. Fonts: OFL-1.1 class only; user text uses system fonts via DirectWrite, which are never redistributed. Presets live in code/TOML. Downloads (fonts, models) need approval with file, source and size, and are pinned by SHA-256. |
| Test media | Generated procedurally in tests (section 13). Real-file corpora, if ever needed, are fetched by a script, pinned by commit and sha256, never committed, and only from clearly licensed sources. |

### 14.2 Engineering methods adopted

| # | FilmCraft method | How we adopt it | Spec impact |
|---|---|---|---|
| M1 | **Exact integer time** (254,016,000,000 ticks/s) | `timeline::Tick` newtype with checked arithmetic. Checked: every common frame rate (24, 25, 30, 50, 60, 120, 23.976, 29.97, 59.94) and audio rate (44.1, 48, 90 kHz) divides it exactly. Conversions to frame and sample indices are defined once and property-tested | section 4 updated |
| M2 | **Everything is a command** (menu, button, drag and CLI dispatch the same typed commands) | `timeline::cmd`: every edit is a serialisable `Command` (id + typed params) applied by one `Session::apply`. The UI is a thin dispatcher. Gives undo/redo, headless tests, and a future `clipcheck-cli edit` / automation door | F-TL-12 rests on it; E1 |
| M3 | **Layering enforced by a checker** | `timeline` and the pure parts of `render` have no Windows or Tauri dependency. `scripts/check-layers.mjs` (reads `cargo metadata`) fails if `timeline` depends on `windows`, `tauri`, `mfdec`, etc. OS code stays in `mfdec`/`gpuconv`/`hwenc` | section 3.1; wave verification |
| M4 | **CPU reference renderer + GPU compositor, parity-tested** | `render::reference` renders a frame on the CPU from the render plan; GPU output must match within a tolerance on procedural footage. Also the fallback with no usable D3D11 device and the oracle for preview/export parity (D69) | sections 8.4, 13; E3 |
| M5 | **Golden tests on procedural footage with a "bless" switch** | Golden frames/PCM come from synthetic inputs; `CLIPCHECK_BLESS=1` rewrites them, normal runs only compare. No external media in the repo | section 13 |
| M6 | **Audio device clock is master; picture follows audio** | Preview clock = Web Audio `currentTime` when running, `performance.now` otherwise. Video drops frames to keep sync; audio never glitches | F-PRV-2, D69 |
| M7 | **Content-keyed render cache + render bar** | P2: cache pre-rendered preview segments by hash(plan segment + source fingerprints) under `editor.cache_dir`; the ruler marks play-ready spans | new F-PRV-10 (P2) |
| M8 | **Proxies for editing, full-res for export** | Background H.264 proxy (1/2 or 1/4 res) for heavy, 4K or HEVC sources; export always reads originals | F-PRV-3/9; new F-MED-7 (P1) |
| M9 | **GOP-cache budget, frame pool, background decoders, draft decode during playback** | `render` owns a bounded decoder/GOP LRU (`editor.decode_cache_mb`, default 512) and a pooled frame allocator. Playback uses reduced-res draft decode; paused frames and exports use full res | section 8.4; settings |
| M10 | **Never-crash rules** | New crates use `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]` (tests exempt). Export, peaks and thumbnail workers run under `catch_unwind` and surface a failed job, never a crash. Project JSON, probes and dropped files are hostile input. Every crash fix gets a regression test | section 13; all waves |
| M11 | **Mutation-fuzz parsers, hostile-parameter tests per command** | Byte-mutation tests for the `.ccproj` loader and the audio-decode wrapper; every `Command` has invalid-parameter tests (negative, overflow, NaN, over-limit return `Err`, never panic) | section 13 |
| M12 | **Export as background jobs; queue; Quick Export; CLI export by preset** | Already in section 8. Add Quick Export (one click, default preset) and `clipcheck-cli export <project> --preset <name>` reusing the same `render` job | F-EXP-24 (P1), F-EXP-25 (P2) |
| M13 | **Deterministic export where possible** | Software H.264 path and the lossless remux are golden-hashed; hardware encoders are documented as not byte-deterministic | sections 8.4, 13 |
| M14 | **Interchange formats** | Defer. P2: OpenTimelineIO JSON and CMX 3600 EDL import/export (small open specs) to move cuts to and from Premiere/Resolve | new F-PRJ-9 (P2) |
| M15 | **Premiere-style editing model** (track targeting, three-point edits, source/program monitors, ripple/roll/slip/slide) | Three-point edits and monitors are already P0; add **track targeting** (which tracks receive Insert/Overwrite/Split-All) as P1. Nested sequences stay out (section 1.3) | new F-TL-23 (P1) |
| M16 | **Audio**: loudness match to BS.1770, ducking that writes volume keyframes, voice-over punch-in | Ours: ducking writes envelope points (F-AUD-18 becomes P1), BS.1770 match (F-AUD-10), voice-over punch-in (F-AUD-16, P2). Submixes, sends, inserts and 5.1 stay out | section 5.4 |
| M17 | **Spec editions recorded per crate** | Each new module README lists the specs and editions it was written from | section 14.1 |
| M18 | **Honest breadth vs depth reporting** | Each wave summary says what works end-to-end on real files (depth) versus merely implemented (breadth) | CLAUDE.md rule 8 |

### 14.3 Deliberately not adopted

Its own pure-Rust codec stack (we use Media Foundation and the existing `hwenc`/`mp4`), wgpu (we use D3D11 like the rest of ClipCheck), egui (we keep Svelte/Tauri), linear-light float colour pipeline and grading (section 1.3), multicam, plugin hosting, Premiere project import, TTS/speech, and its agent/MCP server (the command layer M2 keeps that door open).

### 14.4 Added wave tasks

- **E0**: `scripts/check-layers.mjs`, `scripts/check-assets.mjs`, an `ATTRIBUTION.md` skeleton, clippy lint config for the new crates, a "specs used" list per module.
- **E1**: `Tick`, the `Command`/`Session` layer, hostile-parameter tests.
- **E3**: `render::reference` CPU renderer, GPU parity harness, bounded decoder/GOP cache.
- **E7**: Quick Export and headless CLI export.
- **E10**: soak and fuzz suite, determinism check, full license and attribution audit (`cargo deny`, assets script).
