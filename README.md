<p align="center"><img src="logo/logo.svg" alt="ClipCheck logo" width="128"></p>

# ClipCheck

A lightweight, local-only game recorder for Windows. Think Medal, minus the social features.

Join the [ClipCheck Discord](https://discord.gg/jy2NG2EVNw) for support, suggestions, bug reports, or just general chatter.

<!-- Screenshots: add images of the gallery, player and settings here once they exist. -->

## What it is

ClipCheck runs quietly in the system tray, notices when you start a game, and records only that
game's window. Press a hotkey to save the last 30 seconds (or any length you set, up to 60 minutes),
or let it record the whole session. Everything stays on your PC: there are no accounts, no uploads
and no telemetry.

It is a Rust recorder with a small Tauri v2 + Svelte UI on the system WebView2, so there is no
bundled browser. Recording, clipping, trimming, thumbnails and playback all run in-process using
Windows' own Media Foundation and ClipCheck's own MP4 muxer.

## Features

- **Automatic game detection.** Process and window heuristics, a known-games list and store library
  matching start recording when a game appears. Allow and block lists, plus an allowlist-only mode.
- **Records only the game window.** Uses Windows Graphics Capture bound to the game's window, never
  the desktop. If the window is minimized or hidden, black frames are recorded instead.
- **Replay buffer and clip hotkeys.** Up to 8 clip hotkeys, each with its own length (5 s to 60 min).
  One key can give a quick clip on tap and a longer one on a long press.
- **Full sessions and clips at the same time.** Video is encoded once and shared by the replay buffer
  and the session recording. Split a session or start/stop it with hotkeys.
- **Hardware encoding.** NVIDIA, AMD and Intel hardware encoders through Media Foundation, with a
  software H.264 fallback. H.264 or H.265, quality presets (Low to Ultra) or custom bitrate settings.
  Recordings follow the game's aspect ratio, so ultrawide games are not letterboxed.
- **Multi-track audio.** Track 1 is a mix of everything; optional separate tracks for the game, your
  microphone, chosen apps (Discord by default) and other apps. Per-source gain and a mic noise gate.
- **Gallery.** Search, filter, sort, rename, favourite and delete recordings. Extra folders (for
  example an old clips folder) can be indexed in place without moving anything.
- **Player and trimmer.** Built-in player with volume, speed and per-track audio controls, plus a
  timeline trimmer that saves a new clip or replaces the original.
- **In-app updates.** An update button appears when a new version is out. Updates are signature
  checked before they install, and checking can be turned off.

## Install

Requirements: Windows 10 (version 2004 or later) or Windows 11, 64-bit.

1. Download `ClipCheck_<version>_x64-setup.exe` from
   [GitHub Releases](https://github.com/rangamitch/clipcheck/releases).
2. Run it. It installs for your user only (no administrator rights) to
   `%LOCALAPPDATA%\Programs\ClipCheck`.
3. The installer is not code-signed yet, so Windows SmartScreen may show "Windows protected your PC".
   Click **More info**, then **Run anyway**.

After that you do not need to download installers again: ClipCheck tells you when an update is
available and installs it in one click. Each update is verified against a signature key built into
the app, and a bad signature cancels the update and keeps your current version.

Uninstalling never deletes your recordings. Settings and the library are kept unless you tick the
box to remove them.

## Quick start

1. Start ClipCheck. It lives in the tray; click the tray icon to open the window.
2. Launch a game. ClipCheck detects it and starts recording automatically.
3. Press **Alt+F9** to save the last 30 seconds, or **Alt+F10** for the last 60 seconds.
4. Open the gallery to watch, trim or rename your clips.

Default hotkeys (all changeable in Settings):

| Action | Hotkey |
|---|---|
| Save last 30 s / 60 s | Alt+F9 / Alt+F10 |
| Toggle the replay buffer | Alt+F8 |
| Start or stop the session | Alt+F12 |
| Split the session | Alt+F11 |
| Mute the microphone | Alt+F7 |

Where things go by default:

- Recordings: `%USERPROFILE%\Videos\ClipCheck`, in `Clips\<Game>` and `Sessions\<Game>`.
- Replay buffer (5 minutes by default): `%LOCALAPPDATA%\ClipCheck\buffer`.
- Settings: `%APPDATA%\ClipCheck\config.toml`.

By default ClipCheck records a full session for each detected game as well as keeping the replay
buffer. If you only want clips, turn off full-session recording in **Settings > Recordings**.

## Known limitations

- Games in exclusive fullscreen may record as black. Use borderless or windowed fullscreen.
- Hotkeys use a passive low-level keyboard hook (keys keep working in other apps). Some anti-cheat
  systems may flag global keyboard hooks; this has not been tested against anti-cheat.
- On Windows 10 the yellow capture border around the game cannot be turned off.
- Playing H.265 recordings in the app depends on Windows' HEVC codec being installed.

## Privacy

ClipCheck has no accounts, no uploads and no telemetry. The recorder never uses the network.
The only network request ClipCheck makes on its own is the update check: the app downloads a small
`latest.json` file from GitHub Releases at launch and every 12 hours. The request carries no
identifiers. Set **Settings > Updates** to never check and ClipCheck makes no background network
requests at all.

## Build from source

Prerequisites: Windows 10/11 x64, the stable Rust toolchain (with clippy and rustfmt), Node.js 22
and WebView2 (built into Windows 11).

```sh
# Recorder crates
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings

# UI (the Tauri shell in apps/ui/src-tauri is its own Cargo workspace)
cd apps/ui
npm ci
npm run build
npx vitest run
npx tauri dev                    # run the app in development
npx tauri build --bundles nsis   # build the installer
```

The installer lands in `apps/ui/src-tauri/target/release/bundle/nsis/`. The full local test suite,
including the GPU, audio and capture tests that CI cannot run, is `bash scripts/test-all.sh`.

## Project layout

| Path | What it holds |
|---|---|
| `apps/ui` | Tauri v2 + Svelte app: gallery, player, trimmer, settings. `ClipCheck.exe --daemon` is the recorder |
| `apps/clipcheckd` | Recorder service: tray, IPC server, detection, hotkeys, recording |
| `crates/detect` | Game detection |
| `crates/capture` | Windows Graphics Capture window capture |
| `crates/audio` | WASAPI per-app loopback and microphone capture |
| `crates/gpuconv`, `crates/hwenc`, `crates/aacenc`, `crates/encode` | GPU colour conversion, Media Foundation video and AAC encoding |
| `crates/mp4`, `crates/mfdec` | MP4 muxer, reader and remux; Media Foundation decode and thumbnails |
| `crates/buffer`, `crates/session`, `crates/engine` | Replay buffer and clip extraction, session recording, the pipeline |
| `crates/library`, `crates/clipedit` | SQLite library, trimming |
| `crates/core`, `crates/ipc`, `crates/hotkeys`, `crates/util` | Config schema, named-pipe protocol, hotkeys, shared helpers |
| `crates/testkit` | Dev-only test media generator |
| `installer/` | NSIS installer hooks |

Design documents: [DESIGN.md](DESIGN.md) and [docs/design/](docs/design/).

## Contributing

Bug reports and ideas are welcome on GitHub issues or the Discord. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the license rules and the checks to run before a pull request.

## Security

Please report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), not in public
issues or on Discord.

## License

Licensed under the Apache License 2.0 - see [LICENSE](LICENSE) and [NOTICE](NOTICE). Third-party
notices are generated into `licenses/THIRD-PARTY-NOTICES.txt` for each release; the full licensing
position is in [docs/LICENSING.md](docs/LICENSING.md).
