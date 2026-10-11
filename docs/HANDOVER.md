# ClipCheck — handover (state of the project and what is left)

Written when the previous orchestrator session stopped dispatching new work. Read this first, then `CLAUDE.md`
and `.claude/skills/haiku-swarm/SKILL.md` (the mandatory swarm workflow), then `DESIGN.md` (decision log D1–D24).

## 1. What ClipCheck is
A local-only, Medal-style game recorder for Windows (no social features). Rust workspace + a Tauri v2 / Svelte app.
**One exe**: `ClipCheck.exe` is the UI; `ClipCheck.exe --daemon` is the background recorder (tray icon, detection,
capture, encode, buffer, clips, sessions, library). Closing the window (default "minimize to tray") ends only the UI process.
FFmpeg is an **external child process** (`ffmpeg.exe`), located at runtime (not bundled in the installer; see §6B).

Repo: https://github.com/rangamitch/clipcheck (PRIVATE). Owner: Mitch. Dev machine: RTX 5080 + AMD iGPU, 2560x1440.
The owner runs their own copy from `dist\ClipCheck\ClipCheck.exe` (default data dir `%APPDATA%\ClipCheck`) —
**never kill it, never let tests touch it** (tests use unique pipe names + temp data dirs).

## 2. Layout
```
crates/  core (config schema/validation/ids/media types)   ipc (framing, proto, overlapped named pipe)
         detect (scoring, signals, runner)   capture (WGC)   audio (WASAPI, devices, sessions, meter)
         encode (ffmpeg locate/probe/pipe encoder, tuning, audio inputs)   buffer (ring index, segments, extract)
         session (pure reducer)   library (SQLite, roots, fsops)   clipedit (trim planner)   hotkeys
         engine (recorder, pipeline, audio mixer, catalog, scan, import)   update (versions, manifests, ffmpeg pack)
apps/    clipcheckd (daemon lib+bin: service, server, tray, discovery, library_service, settings_map)
         ui (Tauri app: src/ Svelte frontend, src-tauri/ Rust shell; own [workspace], excluded from the root workspace)
installer/ (hooks.nsh, README, ffmpeg-pack.json)   scripts/ (test-all.sh, kill-test-strays.ps1, release/*)
.github/workflows/ (ci.yml, release.yml)   docs/design/ (00–11)   docs/release.md   logo/ assets/icons/
```
Design docs: `docs/design/00-brief.md` … `11-installer-and-updates.md` (11 = the installer/update plan, authoritative).

## 3. How to work: the Haiku swarm (NON-NEGOTIABLE)
This is the owner's standing process. It is also in `.claude/skills/haiku-swarm/SKILL.md` (with copy-paste prompt templates in
`references/prompt-templates.md`, pitfalls in `references/lessons.md`, the checklist in `references/verification.md`) and
`CLAUDE.md`. If anything here and there ever disagree, this section wins, and fix the other file. NOTE: the OLD
`docs/impl/AGENT_RULES.md` rule 6 (implementer spawns its own tester) was replaced; do not revive it.

### 3.1 Roles
- **Orchestrator (you, the main Claude).** Plans, writes decisions into docs (DESIGN.md D-numbers), pre-creates shared edits,
  dispatches agents, advances each chain when its completion notification arrives, **verifies everything yourself**, commits,
  reports to the owner. You do not hand-implement large features. Tiny edits (<~20 lines, one file) you may do directly, then
  still run the checks. You never trust an agent's report: re-run the build/tests/clippy yourself.
- **Workers are Haiku agents** (`Agent` tool, `model: "haiku"`, `subagent_type: "general-purpose"`, `run_in_background: true`).
  Sonnet only for genuinely hard/creative/meta work (e.g. writing the skill) or after Haiku fails the same task twice.
- Agents close when they finish. Spawn new ones; never keep one alive to accumulate context.

### 3.2 Keeping contexts small (every agent <= 100k tokens total)
One agent = one narrow task (one module / crate / screen / test file). Prompts are HEAVILY GUIDED so the agent never explores:
1. **Goal** in two sentences and why (the bug or requirement).
2. **OWNED files** (exactly which it may create/edit) and **do-not-touch files/crates** (who else is editing what right now).
3. **Read-only references with scope**: file paths, function names, or section/line ranges ("grep for X, read <=200 lines"). Never
   "read the design doc"; never whole large files. Say which public APIs to READ (signatures only).
4. **Contract**: exact function signatures, JSON/IPC shapes (field names!), error behaviour, defaults, ranges, edge cases.
5. **How to verify** with cheap commands only (`cargo build -p X`, `cargo clippy -p X -- -D warnings`, `npm run build`).
6. **Report format**: <= 20-30 lines: files changed, public API summary, deviations from the spec, what was NOT done, how a tester
   should run/drive it. Then STOP.
Split anything big into several agents (we did: schema agent, engine agent, daemon agent, UI-store agent, UI-screen agent...).

### 3.3 Swarming (parallelism)
- Run agents in parallel ONLY on **disjoint files**. Declare one owner per file in each prompt.
- Before launching a parallel wave the orchestrator pre-creates everything shared: `mod` lines in `lib.rs`, compiling module
  stubs, Cargo.toml dependencies and windows-crate feature lists, `store.ts` type fields, tab/registry entries, test lock files.
  Workers then touch only their own files. If a worker truly must touch a shared file: one minimal `Edit` after a fresh re-read,
  stated in the prompt (this worked for `lib.rs` mod lines, `store.ts`, `tabs.ts`).
- Features run as independent CHAINS (3.4) concurrently; you advance each chain whenever its notification arrives. Do not poll;
  do not `sleep` (blocked in git-bash); use background commands + notifications.
- **Serialize anything that spawns a real daemon / uses the global desktop or GPU/audio** (daemon integration tests, WGC capture,
  hotkey SendInput, audio loopback): they collide on pipes, data dirs, ffmpeg, foreground focus. One such chain at a time, or
  make each use unique pipe names + temp data dirs (all current tests do) and still avoid running two at once.
- Check state yourself when an agent ends oddly ("waiting for the tester", "launched X in background", stale timer
  notifications): look at `git status`, running processes, and re-run the crate tests. Use `TaskStop` for agents whose timers keep
  firing after they finished.

### 3.4 Testing is a SEPARATE chain (never the implementer)
The agent that writes code must NOT be the agent that tests it, and no single agent carries implementation + verification context.
1. **IMPLEMENTER** (Haiku): edits only owned files; verifies only that it compiles, passes clippy `-D warnings` (or `npm run build`)
   and tiny inline `#[cfg(test)]` unit tests for tricky pure functions. It writes NO integration tests and does NOT spawn agents.
   Its report ends with the public API summary and how to run/drive the code, so a tester can start cold.
2. **TESTER** (a DIFFERENT, fresh Haiku): receives ONLY the spec/contract + the public API signatures + behaviours and edge cases
   + how to run (paste them from the implementer's report). It must not read the implementation to decide what is "correct".
   It may create/edit ONLY test files (`crates/<x>/tests/**`, `*.test.ts`, `scripts/**/*.test.mjs`), never `src`. It writes and runs
   the tests (table-driven, boundaries, malformed input, concurrency/cancel/failure paths, real tools like the vendored ffmpeg when
   relevant) and reports: counts, pass/fail list, and every deviation from the spec with evidence (test name, expected vs actual,
   output). If code deviates from the spec it REPORTS it; it does not change the code. "Report-only" cases (unknown behaviour) are
   observed and documented, not asserted.
3. **FIXER** (a fresh Haiku) if anything is red: receives the failing test names + assertion output + the owned source files + the
   spec. It fixes SOURCE (and may fix a test only if the test is wrong, with written justification), adds focused regression tests in
   its own new test file, and does not widen scope.
4. **RE-TESTER** (a fresh Haiku, or the same tester resumed with `SendMessage` if its context is small) re-runs the whole suite plus
   the fixer's tests and the previous report-only cases. Loop FIXER -> RE-TESTER until green.
5. **Escalate** after two failed Haiku rounds on the same problem: the orchestrator reproduces and adds diagnostics itself
   (examples this project: a VEH crash handler found the WGC/MTA crash; dumping the detector's enriched snapshot found the missing
   signals), or hands the diagnosis to a Sonnet agent.
6. **Orchestrator gate** after each chain and after every wave: `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`;
   `scripts/test-all.sh` (per crate, hard time caps, kills only repo-launched strays, never the owner's installed ClipCheck);
   in `apps/ui`: `npm run build` + `npx vitest run`; in `apps/ui/src-tauri`: `cargo clippy --all-targets -- -D warnings` +
   `cargo test -- --test-threads=1`. Investigate every failure to root cause (reproduce; add diagnostics; do not just retry).
   Then update README/DESIGN.md and commit (stage explicit paths; trailer per the harness).
7. Pass work back and forth like a relay: implementer -> tester -> fixer -> re-tester -> (orchestrator gate). Different agents at each
   leg; the orchestrator is the only memory between legs.

### 3.5 Safety and environment rules
- Permanent tests NEVER launch games or touch the owner's real apps/recordings. Ad-hoc manual launches (to diagnose) only with the
  owner's OK, by the orchestrator, closing what you launched afterwards; the owner's own ClipCheck (`dist\ClipCheck`) keeps running.
- Nothing outside the repo is created/published/deleted without asking (GitHub repos/releases/comments, keys). No secrets in the repo.
  Downloads need approval (file, source, size). Hooks/installer tests must never act on a copy outside their own temp install dir.
- Python is not installed: use node, PowerShell (as `.ps1` files, not inline in bash) or perl. Bash heredocs with apostrophes break:
  use the Write/Edit tools. git-bash `sleep` loops are blocked.
- Tests that spawn daemons use unique pipe names + temp data dirs and serialize on the shared test lock; late IPC subscribers miss past
  events, so assert on `get_status`, not only events.
- Report to the owner concisely: done / verified (with numbers) / NOT verified / needs your decision.

## 4. State of the repo (IMPORTANT)
- Last commits: `9646294` (tray/settings/live status/close behavior; 949 workspace tests green at that point),
  `65e487d` (detect diagnostic example). **Everything after that is UNCOMMITTED (~128 files) and not yet verified as a whole.**
  Contents of the uncommitted work: new audio/video config model + Medal-style Audio/Video/Games settings screens + discovery
  pickers; encoder tuning (rate control/speed/B-frames); audio mixer with per-app sources; detection signals (library scan,
  known games, GPU PDH, D3D modules) + enrichment + pinned-first ranking; partial hotkey registration fix; per-game folder
  layout; library scanner/import (engine) + daemon library service + IPC; storage `libraries` config; `updates` config;
  `clipcheck-update` crate; NSIS installer config/hooks/licenses; release scripts + workflows; Tauri updater backend + CLI;
  the haiku-swarm skill + CLAUDE.md. First job: get it green and commit it in sensible chunks (stage explicit paths).
- **Known compile break (fix first, trivial):** `Command::Shutdown` became `Shutdown { reason: Option<String> }` (hand-written
  serde keeps `{"type":"shutdown"}` valid). These still use the unit form and do not compile:
  `apps/clipcheckd/tests/{daemon,tray_tests,service_tests}.rs`, `apps/ui/src-tauri/tests/{single_exe_tests,ipc_client_tests,closing_tests}.rs`
  (and check `apps/ui/src-tauri/src/*` for the same). Change to `Shutdown { reason: None }`.
- **Two agents were STOPPED mid-task at handover. Their work is in the tree, unverified. No agents are running.**
  1. **Fixer for `crates/update` (stopped).** Measured after stopping: unit 12 pass; `http_tests` 27/27, `manifest_tests` 13/13,
     `pack_tests` 21 pass + 1 ignored (zip bomb, report-only), `version_tests` 9/9. So the 3 spec failures the independent tester
     found (an oversize download must be an error, lowercase-hex only, a zip whose only top-level entry is `bin/`) are FIXED, and an
     install lock + unique temp dirs + an extraction-size cap were added. Its own `tests/fixer_tests.rs` has 15 pass and
     **1 FAIL**: `download_declared_length_over_limit_is_refused_before_writing`. That is a TEST bug: the tiny_http server sends no
     Content-Length, so the early-refusal path is never reached; use the raw-socket helper in `tests/common`, which does send it.
     To finish: fix that test, run `cargo clippy -p clipcheck-update --all-targets -- -D warnings`, then dispatch a FRESH re-tester that
     also re-runs the two report-only cases (concurrent same-version installs must never leave a store without a current pack;
     the 200 MB zip bomb must now return `ArchiveTooLarge`).
  2. **Tester for the release scripts and workflows (stopped before it reported).** Its harness had hung: `spawnSync` blocks Node's
     event loop, so the in-process HTTP server could not answer PowerShell; use async `spawn`. State: the implementer's 15 Node tests
     pass (`node --test scripts/release`). STILL TO DO with a fresh tester: edge tests (CRLF Cargo.toml, tags, notes with
     quotes/newlines/emoji, installer names with spaces or `+`, a CRLF .sig, size-budget boundaries, write-notices determinism);
     structural checks of `.github/workflows/{ci,release}.yml` (needs, minimal permissions, pinned `uses:`, draft release, guarded
     Authenticode step, no secret echo); parse-check the embedded PowerShell; verify every `uses: owner/repo@ref` exists via read-only
     `gh api`; `cargo deny check` (cargo-deny is not installed: ask before installing); cross-check `docs/release.md` against the
     real script, flag and secret names. No GitHub write call has ever been made.
  Three `node` processes (PIDs 8372, 42040, 49256) were running at handover. They are not from the swarm: do not kill them.

## 5. Feature status
| Area | Status |
|---|---|
| Detection (scorer, signals, enrichment, pinned-first, allowlist mode) | built; **verified on real Terraria (borderless) and My Singing Monsters (windowed): both score 100 via `cargo run -p clipcheck-detect --example dump_candidates -- <name>`**. End-to-end daemon recording of a real game NOT yet done (see 6H). |
| Capture (WGC), encode (ffmpeg pipe, NVENC/AMF/x264/x265, tuning), buffer, clips, sessions (split/stop), audio (system/game/mic/per-app mixer, gate) | built + tested (synthetic windows/tones) |
| Daemon (tray, single instance per data dir, IPC, events, settings patch, discovery lists) | built + tested |
| Settings UI: General, Storage(old), Games, Audio (Medal-style), Video (preset cards), Replay & Clips, Hotkeys | built; **never looked at in a running window by the orchestrator** (owner has used an earlier build) |
| Gallery list/sort/filter + live status panel + custom title bar | built; thumbnails NOT shown yet (placeholder blocks) |
| Library: schema v2, roots, scanner (Medal-shaped trees), import engine, per-game folders `Clips\<Game>`, `Sessions\<Game>` | built + tested in engine/library; **no UI yet** |
| Daemon library service (scan scheduler, `list_libraries`, `scan_libraries`, `import_plan/start/cancel`, `list_game_names`, events) | built; needs a separate tester |
| Installer (NSIS per-user, 3.7 MB, hooks kill only `$INSTDIR\ClipCheck.exe`, never deletes recordings) | built; **never run**; hooks wait on `--shutdown-daemon` without a timeout |
| Updater backend (`check_for_update`, `install_update`, `--shutdown-daemon` CLI, update-state.json) | built; needs tester; **no UI** |
| `clipcheck-update` (versions, manifests, verified ffmpeg pack install) | built; tester found 5 defects, fixer was running |
| Release scripts/workflows | built; tester was running; nothing run on GitHub |
| Player, clip trimmer, rename/delete/favorites in UI | **not started** (docs 08 + `clipedit` planner + `library::fsops` exist) |

## 6. Remaining work (prioritized)
**A. Library UI (contract is done in the daemon).** Tauri `tauri-plugin-dialog` (folder/file pickers; capability `dialog:default`)
and the asset protocol for thumbnails (`app.security.assetProtocol` scope `$APPDATA/ClipCheck/thumbs/**`; posters at
`<data>/thumbs/<id>/poster.jpg`, generated lazily by the daemon). Storage tab: primary location (path + free space),
"Other locations" list (label, clips count, size, online/offline pill, Rescan, Remove = stop indexing, NEVER delete files),
Add location (folder picker), folder-structure preview `<primary>\Clips\<Game>\`. When the user changes the PRIMARY location
the UI must offer/auto-add the OLD primary to `storage.libraries` (the daemon removes DB rows of roots no longer in config).
Import modal (file/folder picker + drag-and-drop onto the window): game dropdown (`list_game_names`, "auto from folder name",
"other"), plan preview, clear warning "Importing makes a COPY and uses extra disk space (X needed, Y free on <drive>)",
progress + cancel, result summary. Gallery: show posters, source badge (recorded/import/external), library/location filter,
game filter from `list_game_names`, offline-library notice, scan progress, "mkv not playable" note.
IPC (flat `{type,data}` like the rest; see `crates/ipc/src/proto.rs`): commands `list_libraries`, `scan_libraries{root_id?}`,
`import_plan{paths,game?}`, `import_start{paths,game?}`, `import_cancel{job_id}`, `list_game_names`; replies `libraries`,
`import_plan{items[{src,name,size,game,dest,skip_reason?}],total_bytes,free_bytes,enough_space,primary_read_only}`,
`import_started{job_id}`, `game_names`; events (topic `library`) `library_scan`, `library_changed`, `import_progress`,
`import_done`; errors `busy`, `invalid_state`, `worker_unavailable`, `bad_request`. Formats: mp4, m4v, mov, mkv (webm unsupported).
Small follow-ups: `apps/clipcheckd/src/library_service/sync.rs` should delegate to `clipcheck_library::roots::normalize_root_path`;
merge pre-existing duplicate root rows; separate tester for the daemon library service.

**B. Update UX + FFmpeg component (plan: docs/design/11).**
1. Daemon (`I3b`): IPC `ffmpeg_status` / `ffmpeg_install` (+ progress events) using `clipcheck_update::pack::PackStore`
   (store `%LOCALAPPDATA%\ClipCheck\ffmpeg`, `locate_ffmpeg` already looks there first); degraded state while missing; hot-start
   the recorder after install (re-locate, re-probe) without restart. Pack manifest URL = `updates.endpoint` with
   `latest.json` replaced by `ffmpeg-pack.json`, so **every release must attach `ffmpeg-pack.json`** (fix `release.yml`/scripts).
2. UI: title-bar pill `↑ Update x.y.z` (event `update-available`), modal (notes, size, Update now / Later / Skip this version;
   if a session is recording: Wait / Stop & update; progress via `update-progress`; error states `recording`, `signature`),
   Settings -> Updates tab (current version, last checked, Check now, auto-check toggle = `updates.check`), first-run
   "Set up video components (~90 MB)" banner with progress.
3. Tester chain for the Tauri updater backend (`apps/ui/src-tauri/src/{updater,cli,update_state}.rs`): CLI exit codes
   (0 no daemon/not ours/ok, 3 recording, 4 error) against a real daemon (`clipcheck-ui.exe --daemon` + `--shutdown-daemon`
   with CLIPCHECK_PIPE_NAME/DATA_DIR), fake local update endpoint (debug builds allow http://127.0.0.1; sign with the dev key:
   `npx tauri signer sign -f <scratch>\dev-update.key <file>`; serve with node, not python).
4. Updater key: `tauri.conf.json` holds the production public key (CABC178AAFF59A6A); the private key lives in the owner's
   `%USERPROFILE%\.clipcheck\` and the `release` environment secrets. Local fake-endpoint tests need a throwaway key whose
   pubkey is swapped into that test build only; never commit it.

**C. Installer testing (never executed).** Separate tester chain: build `npx tauri build --bundles nsis` (3.7 MB), silent install into a
TEMP dir (`/S /D=<tmp>`) with `CLIPCHECK_PIPE_NAME`/`CLIPCHECK_DATA_DIR` pointing at test values, verify files + `licenses\`,
upgrade N -> N+1 (settings/library survive; the hook calls `ClipCheck.exe --shutdown-daemon` — now exists, give the hook a
TIMEOUT: wrap in PowerShell `Start-Process -PassThru; WaitForExit(8000)`), uninstall (recordings untouched; data kept unless the
checkbox; only the named app subfolders are deleted), no-network check in `updates.check = never`, residue check
(HKCU Uninstall entry, Start Menu shortcut) and clean up. NEVER kill/modify the owner's running copy (the hooks only touch
`$INSTDIR\ClipCheck.exe`). Also: NSIS `licenseFile` adds a click-through license page (Apache-2.0) (review), Authenticode optional (unsigned
=> SmartScreen warning), run `cargo deny check` locally (not installed), validate workflows on a real runner (tag a test repo).

**D. Player + trimmer + library actions (not started).** Spec: `docs/design/08-player-and-clip-editor.md`. Needs the asset/clip
protocol with HTTP range, HTML5 player (volume, scrub, hover thumbs), multi-track audio via Web Audio (sidecar track extraction),
lossless keyframe-snapped trim using `crates/clipedit` + ffmpeg stream copy (save as new `clip-trim`, optional overwrite),
rename (library::fsops::rename_on_disk_and_db exists) / delete (Recycle Bin) / favorites via daemon IPC (proto has
`rename_recording`/`delete_recording`; check they are implemented in `server.rs`), HEVC playback fallback (proxy), mkv not
playable in WebView2 (offer remux).

**E. Recorder quality items.** Audio starvation guard: a track receiving no PCM stalls the ffmpeg muxer (and thus video) — make the
audio writer inject paced silence (`crates/encode/src/audio_in.rs`); NVENC `gpu_index` for multi-GPU (settings_map passes None);
canvas should follow the source aspect ratio (ultrawide) instead of fixed 16:9; HDR tone map; exclusive-fullscreen game hook
(v2); UWP windows resolve to `ApplicationFrameHost` (detect `win.rs`); pinned app does not displace an already-recording
unpinned game (decide); the first `WinSource::snapshots()` does a synchronous library scan (~1 s; consider pre-warm).

**F. Settings/product polish.** "Why wasn't my game detected?" panel (expose `dump_candidates` logic over IPC); push-to-talk needs
the opt-in low-level keyboard hook (not built; `hotkeys.mute_mic_mode` unused); real noise suppression beyond the gate;
config save rewrites the file (comments lost; consider `toml_edit`); doc 10 still mentions `updates.check_interval_hours`
(authoritative key is `updates.interval_hours`, doc 11 §8); docs 06/09/10 need the decisions below recorded.

**G. Hardening backlog.** IPC peer-SID check on the pipe; `PipeConn::flush` before close; library duplicate-root merge;
service.rs is ~1500 lines (split); `HotkeyService` partial registration done — consider surfacing failures in status;
zip/pack concurrency lock (fixer in flight); replay buffer location default; multi-user (fast user switching).

**H. Real-game end-to-end check (do manually, ad-hoc, with the owner's OK, and close what you launch).** Launch Terraria
(`steam://rungameid/105600`) and My Singing Monsters (`steam://rungameid/1419170`) from the ADMIN-free shell; run the new build
(`apps/ui` `npx tauri build --no-bundle` then `dist\ClipCheck`, with a TEMP data dir via `CLIPCHECK_DATA_DIR`/`CLIPCHECK_PIPE_NAME`
so the owner's copy is untouched); confirm auto-detection (status `game_info`), clip hotkey -> `Clips\<Game>\...mp4` with
the expected audio tracks (Mix/Game/Mic/...), session split/stop, files registered in the library, then close the games.

**I. Test debt: implemented but never independently tested (each needs a fresh tester agent; tests only, public API only).**
- Daemon library service (`apps/clipcheckd/src/library_service*`): the IPC contract in 6A; drive it with
  `clipcheckd --data-dir <tmp> --pipe-name <unique>` plus a `config.toml` containing `[storage] root=...` and `[[storage.libraries]]`
  (hidden flag `--no-ffmpeg` disables scan/import but keeps `list_libraries`). Cover: root sync from config, offline root, scan events,
  pause while a session records, import plan/start/cancel/busy/space errors, `source='import'`, mov/m4v/mkv handling.
- Daemon shutdown contract (`Shutdown{reason}` + busy reply when a session is recording, `StatusDto.exe_path`) and the
  `locate_ffmpeg` pack-store order (`CLIPCHECK_FFMPEG_STORE`).
- Tauri updater backend and `--shutdown-daemon` CLI (6B.3), installer (6C), the updates config UI (not built).
- Everything the old implementer-spawns-its-own-tester rule produced (engine scan/import/layout, library roots, the Audio/Video/Games
  screens, discovery commands) had a tester, but the tester saw the implementation; a second independent pass is cheap insurance.
- Never exercised in a real window: custom title bar, tray menu, close-to-tray + exit confirm, all settings screens, the update
  button (does not exist yet). Do a manual pass with the owner.

## 7. Decisions not yet written into DESIGN.md (add as D25+)
New Medal-style audio model (Mix track + optional separate tracks, per-source gain, mic gate; `Audio::track_plan()`),
video presets table (Low 480p30/4M, Standard 720p60/8M, High 1080p60/14M, Ultra 1440p60/30M, Custom), encoder tuning
(`EncoderTuning`), detection signals + enrichment, **pinned apps rank first and allowlist mode filters candidates before selection**,
partial hotkey registration, storage layout + libraries (D24 exists), updates default `notify`, FFmpeg as a downloadable pack,
`Shutdown{reason}` + `exe_path` safety contract (CLI only acts on its own copy's daemon), installer hook safety rules,
process-lifetime MTA (D20 exists), single instance per data folder, harness lessons (see skill `references/lessons.md`).

## 8. Owner decisions still open
1. Repo visibility: public, or a separate PUBLIC releases-only repo (the updater cannot read a private repo's assets). Nothing was created.
2. Authenticode certificate / Azure Trusted Signing (v1 can ship unsigned).
3. Replace the dev updater key (see 6B.4) before any public release; set GitHub `release` environment secrets.
4. Update default `notify` (chosen) vs `never`.
5. Confirm the releases repo name used in `DEFAULT_UPDATE_ENDPOINT`.
6. Importing: keep original extension for mov/m4v (currently yes, DB ext = mp4).

## 9. Useful facts
- IPC default pipe `\\.\pipe\clipcheck-ipc-<user SID>`; override with `CLIPCHECK_PIPE_NAME`; data dir override `CLIPCHECK_DATA_DIR`;
  daemon flags `--data-dir --pipe-name --no-ffmpeg(hidden)`. Daemon is the ONLY writer of `library.db` and `config.toml`.
- Late IPC subscribers miss past events: clients (UI and tests) must read `get_status` for state, events for changes.
- Medal library at `F:\Medal\Clips\<Game>\MedalTV<Game><yyyyMMddHHmmssSSS>.mp4` (~700 clips/540 GB) is the target for the
  external-library scan; it contains junk (`$R*` recycle files, Thumbnails, Temp, Video-Editor, Databases) that is skipped. Never modify it.
- FFmpeg dev build: `vendor/ffmpeg/ffmpeg-n8.1-latest-win64-gpl-shared-8.1/bin` (git-ignored). `dist/` is git-ignored.
- Last known per-crate counts at the last green sweep (`9646294`): core 139, ipc 55, session 73, buffer 73, library 88, detect 156,
  hotkeys 41, clipedit 35, encode 63, capture 11, audio 30, engine 73, clipcheckd 112; UI vitest ~1036, src-tauri 75.
  These have all grown since; recompute.
