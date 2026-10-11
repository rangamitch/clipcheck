# ClipCheck — Shared Architecture Brief

This brief fixes the cross-cutting decisions so every feature design doc stays consistent.
Feature docs elaborate; they must not contradict this file. If you believe a decision here is wrong, say so under "Open questions" in your doc instead of silently diverging.

## Product
ClipCheck is a local-only, Medal-style game recorder with no social/cloud features. Windows 10 2004+ / Windows 11 first (x64). Linux/macOS are out of scope for v1.

## Language and stack (decided)
- **Rust** (stable, edition 2021) for everything backend. Cargo workspace.
- **Windows APIs via the `windows` crate**: Windows Graphics Capture (WGC), D3D11, WASAPI (incl. process loopback), Media Foundation as fallback, `RegisterHotKey`, `Shell_NotifyIcon`.
- **FFmpeg (libav\*, LGPL/GPL shared build, dynamically loaded DLLs)** for encoders (NVENC, AMF, QSV, x264/x265, SVT-AV1 optional) and muxing. Accessed through a thin in-house `ffi` wrapper, not a huge crate graph.
- **UI: Tauri v2** (system WebView2, no bundled Chromium) with a small TypeScript + Svelte frontend. Video playback uses the HTML5 `<video>` element fed by a local custom-protocol (`clip://`) with HTTP range support.
- **SQLite** (`rusqlite`, bundled) for library metadata. **TOML** (`serde`, `toml_edit`) for settings.
- Project license: Apache-2.0 (D61; was GPL-3.0-or-later until then).

## Process model
- `clipcheck-core` (Rust crate, library) holds all recording logic.
- **`clipcheckd` — the recorder service**: tiny background process, tray icon, no webview. Owns detection, capture, encode, buffers, hotkeys. Target idle < 15 MB RSS, < 0.3 % CPU when no game is running.
- **`clipcheck` — the UI app**: Tauri window for gallery, player, editor, settings. Launched on demand from the tray; talks to `clipcheckd` over a **named pipe (`\\.\pipe\clipcheck-ipc`) with length-prefixed JSON messages**. Closing the UI leaves the recorder running.
- The UI never touches capture/encode; it reads the SQLite library (read-only handle, WAL mode) and sends commands (start/stop/split/clip/reload-settings) to the service.

## Workspace layout (crates)
```
crates/
  core/        shared types, config schema, ids, errors, events
  detect/      game detection
  capture/     WGC window capture, D3D11 frame pool, black-frame fallback
  audio/       WASAPI sources, per-process loopback, mixer, track routing
  encode/      encoder probing/selection, rate control, FFmpeg wrapper
  buffer/      segment ring buffer, clip extraction
  session/     full-session recorder, split/stop logic, state machine
  library/     SQLite, thumbnails, rename, filter/sort queries, file watcher
  clipedit/    lossless trim/remux and optional re-encode trim
  hotkeys/     global hotkey registry
  ipc/         pipe protocol
apps/
  clipcheckd/  service binary (tray)
  ui/          Tauri app (src-tauri + web/)
docs/design/
```

## Core data pipeline (decided)
```
Detector -> picks target window (HWND, pid, exe)
  Capture (WGC, GPU texture, target fps) -> D3D11 scale/convert (NV12) -> Encoder (HW preferred) --+
  Audio sources (per-app process loopback x N, mic, optional system mix) -> per-track AAC/Opus enc --+
                                                                                                     v
                                                         Muxer-agnostic "EncodedPacket" stream (video + N audio tracks)
                                                              |                               |
                                                  Segment ring buffer (replay)      Session writer (full session file)
```
**Encode once, fan out to sinks.** Both the replay buffer and the full-session writer consume the same encoded packet stream, so a full session and Past-N-minute clips run simultaneously at no extra encode cost.

- Replay buffer: GOP-aligned fragmented-MP4 segments (~2 s, forced keyframe at segment boundary) written to a temp ring directory on disk (RAM-ring option for short buffers). Max duration configurable (default 5 min, up to 60). Clipping Past N = concatenating the last ceil(N/segment) segments by remux (no re-encode, near-instant).
- Full session: one MKV/MP4 file per session part, written incrementally (fragmented MP4 or MKV so a crash loses < 2 s).
- Split: close current file at the next keyframe, finalize, open the next file on that same keyframe, zero dropped packets; session continues as "Part N+1".
- Stop/disable: finalize and index the current file, then idle (until re-enabled or next game launch depending on settings).

## Capture policy (decided)
- Capture **the detected game window only** (WGC `GraphicsCaptureItem` from HWND). Never the desktop. Other apps' pixels must never appear.
- If the window is visible but not focused: keep capturing.
- If the window is minimized/hidden/not producing frames (e.g. exclusive fullscreen alt-tabbed): emit **black frames at target fps** (no other fallback content). Resume capturing when the window returns.
- Exclusive-fullscreen games that WGC cannot capture: v1 documents the limitation and prefers borderless/windowed-fullscreen; a game-hook (DLL injection, OBS-style) path is a **v2 stretch**, designed but not built in v1.

## Audio policy (decided)
- Tracks are configurable and numbered. Default layout: Track 1 = **Mix (game + all apps)**, Track 2 = **Game only**, Track 3 = **Microphone**, Track 4 = **Chat/Discord**, Track 5+ = user-defined app groups.
- Per-app capture via WASAPI **process loopback** (`AUDCLNT_PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`); "all other apps" via EXCLUDE mode.
- All PC audio is recorded even though only the game's video is. Mic is always a separate track, with optional push-to-talk/mute hotkey.
- Audio codec AAC-LC (default) or Opus (MKV); 48 kHz stereo; 128–320 kbps per track.

## Encoding policy (decided)
- Codecs: H.264 (default), H.265/HEVC, AV1 (hardware only, optional).
- Encoder selection order (Auto): NVENC > AMF > QSV > software (x264/x265). User can force any. Probe at startup and cache.
- Rate control: CQP/CRF-style "quality" or CBR/VBR bitrate. Presets: Low / Medium / High / Ultra / Custom, with resolution (native/1440p/1080p/720p) and fps (30/60/120) selection.

## Config and data locations
- Config: `%APPDATA%\ClipCheck\config.toml` (hot-reloadable; service watches file + receives IPC reload).
- Library DB: `%APPDATA%\ClipCheck\library.db`.
- Recordings root: user-configurable, default `%USERPROFILE%\Videos\ClipCheck\<GameName>\`.
- Temp buffer ring: `%LOCALAPPDATA%\ClipCheck\buffer\` (or user-chosen fast drive).
- Logs: `%LOCALAPPDATA%\ClipCheck\logs\`.

## Naming/ID conventions
- `GameId` = normalized exe name + optional store id. `SessionId`, `ClipId` = ULIDs. Timestamps UTC, stored as unix ms.
- File name template default: `{game} {yyyy-MM-dd HH-mm-ss} {kind}` where kind = `clip-30s` | `session-part1`.
- Clips and sessions are rows in one `recordings` table (`kind` column).

## Quality bar
Lightweight: UI app idle < 120 MB with window open, service < 15 MB idle, < 3 % one-core CPU while recording on HW encode, installer < 40 MB (excluding optional FFmpeg download if split).

## Required structure of every feature doc
1. Goal & scope (what is in/out)
2. User-facing behavior (precise)
3. Architecture & data flow (reference crates/modules above)
4. Key types, traits and function signatures (Rust, concrete)
5. Algorithms / state machines (step-by-step; ASCII diagrams welcome)
6. Config keys owned by this feature (name, type, default, range) — **feeds the Settings doc**
7. IPC messages / events produced or consumed
8. Edge cases & failure modes (list at least 8)
9. Performance & resource budget
10. Test plan
11. Implementation task breakdown (ordered, each task ~0.5–2 days) 
12. Open questions
Be concrete and decisive. Prefer tables and code blocks over prose. 600–1200 lines is fine; do not pad.
