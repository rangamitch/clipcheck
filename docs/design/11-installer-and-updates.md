# 11 — Installer, updates and release pipeline

Status: plan v1 (implements GitHub issue #1, Windows only, delivery via GitHub Releases).
Supersedes the packaging parts of doc 10 §3.6 / §5.12 where they differ (see "Decisions").

> History note: the FFmpeg pack (section 5) was removed (DESIGN.md D58, D62), and the
> `clipcheck-update` crate was deleted (D63). In-app updates use `tauri-plugin-updater`
> (`apps/ui/src-tauri/src/updater.rs`); `latest.json` comes from `scripts/release/make-latest-json.mjs`.

## 1. Goals
1. One small installer, no admin rights, per-user install.
2. **Updates from inside the app**: when a new version exists a button appears in the app; one click downloads, verifies, installs and relaunches. Nobody downloads an installer by hand after the first install.
3. Updates never re-download the big encoder libraries (FFmpeg is a separate, rarely-changing component).
4. Releases come from CI on a tag; no keys in the repo.
5. No telemetry. The only network traffic ClipCheck ever makes by itself is "is there a newer version?" (and the one-time FFmpeg download), and the first can be switched off.

Non-goals (v1): MSI/MSIX/Store, Linux/macOS, delta updates, beta channel (format reserved), auto-installing without a click.

## 2. Decisions (resolving issue #1's open questions)
| # | Question | Decision |
|---|---|---|
| 1 | Repo visibility | **Needs the owner.** The repo is currently PRIVATE, and unauthenticated clients cannot download private release assets. Two options: (a) make `clipcheck` public, or (b) keep source private and publish artifacts to a PUBLIC releases-only repo (e.g. `clipcheck-releases`). The code is written against a configurable `RELEASE_REPO` / `updates.endpoint`; nothing is created or published until the owner picks. |
| 2 | Signing certificate | v1 ships **without Authenticode** (Windows SmartScreen will warn "unknown publisher" until reputation builds). **Update integrity is NOT dependent on it**: updates are Ed25519-signed (Tauri updater), the public key is compiled into the app. The workflow signs with Authenticode automatically when `WINDOWS_CERT_*` / Azure Trusted Signing secrets exist. |
| 3 | Update host | GitHub Releases. Endpoint `https://github.com/<RELEASE_REPO>/releases/latest/download/latest.json`. |
| 4 | FFmpeg in the base installer | **No.** FFmpeg's GPL build is ~200 MB unpacked (avcodec alone 114 MB), far above the 40 MB budget. It ships as a separate **FFmpeg pack** release asset, downloaded on first run (and offered in-app), verified by SHA-256, stored in `%LOCALAPPDATA%\ClipCheck\ffmpeg\<pack-version>\`. App updates therefore stay ~15–20 MB. |
| 5 | Default update mode | **`notify`** (check on launch and every 12 h; show the update button; install only on click). Users can set `never` in Settings → Updates. A manual "Check for updates" always works. The check is a single HTTPS GET with no identifiers. |

## 3. Artifacts of one release (`vX.Y.Z`)
| Asset | Purpose |
|---|---|
| `ClipCheck_X.Y.Z_x64-setup.exe` | NSIS per-user installer (also used silently by the updater) |
| `ClipCheck_X.Y.Z_x64-setup.exe.sig` | Ed25519 signature (Tauri updater) |
| `latest.json` | updater manifest: version, notes, pub_date, platform url + signature, minimum FFmpeg pack |
| `ffmpeg-pack-<P>-win64.zip` + `ffmpeg-pack-<P>.json` | FFmpeg component (only attached on releases where the pack version changes; otherwise the manifest points at the existing asset) |
| `THIRD-PARTY-NOTICES.txt`, `SHA256SUMS` | licenses and hashes |
| `ClipCheck-symbols.zip` | PDBs |

## 4. Installer (NSIS, per-user)
- Built by `tauri build --bundles nsis` with `installMode: currentUser`; target `%LOCALAPPDATA%\Programs\ClipCheck\`. No UAC.
- Payload is just `ClipCheck.exe` (UI **and** daemon: `--daemon` mode), resources, licenses. (No separate daemon exe, no FFmpeg.)
- WebView2: evergreen bootstrapper (`downloadBootstrapper`), skipped if present (Windows 11 has it).
- Installer hooks (`installer/hooks.nsh`): PRE-INSTALL: ask a running daemon to shut down gracefully (`ClipCheck.exe --shutdown-daemon` → IPC `shutdown{reason}`); if it is recording, refuse with a clear message in interactive mode (in silent update mode the app already refused earlier); force-terminate after 10 s. POST-INSTALL: write `HKCU\Software\ClipCheck\InstalledVersion`, create the Start Menu shortcut, "Launch ClipCheck" on the finish page.
- Uninstall: stops processes, removes shortcut, Run value (autostart) and our registry keys; **never deletes recordings**; settings/library (`%APPDATA%\ClipCheck`) kept unless the user ticks "Also remove my settings and library". The FFmpeg pack under `%LOCALAPPDATA%\ClipCheck\ffmpeg` is removed (it is re-downloadable).
- `/S` silent mode supported (used by the updater with `passive` UI).

## 5. FFmpeg pack (component manager)
- A tiny shared crate `clipcheck-update` (no UI deps) owns: version comparison, manifest parsing, `FfmpegPack` download + SHA-256 verify + zip extract + atomic activate, and the `latest.json` parser.
- Pack manifest: `{ "pack_version": "8.1.3-1", "ffmpeg": "n8.1", "url": "...zip", "sha256": "...", "size": 90330568, "files": { "ffmpeg.exe": "sha256…", … }, "license": "GPL-3.0-or-later" }`.
- Locate order (extends `locate_ffmpeg`): `CLIPCHECK_FFMPEG`, `%LOCALAPPDATA%\ClipCheck\ffmpeg\current\bin`, `<exe dir>\ffmpeg\bin`, repo `vendor/…`, `PATH`.
- Daemon: missing/incompatible pack ⇒ degraded state `ffmpeg_missing` (IPC works, no recording). The UI shows a banner "Set up video components (≈ 90 MB)" with a Download button (also auto-prompts on first run) and a progress bar; verifies SHA-256 and every file hash; downloads to `ffmpeg\.tmp-<ver>` then renames; keeps the previous pack until the new one verifies. Resumable (HTTP Range) is a v1.1 nicety.
- An app update may require a newer pack (`latest.json.min_ffmpeg_pack`): the update modal says "this update also needs a one-time video component update (90 MB)" and runs it after the app update succeeds.

## 6. In-app updates (what the user sees)
1. **Check**: on launch (after the window is up) and every 12 h while the app/daemon runs, unless `updates.check = never`. The UI process makes the request (Tauri updater plugin); the daemon never touches the network.
2. **Button appears**: the title bar shows a pill `↑ Update 0.3.0` (red accent). Settings → Updates shows details.
3. **Click → modal**: version, release notes (from `latest.json`), download size, buttons **Update now** / **Later** / **Skip this version**. If a session is being recorded: "A recording session is in progress" with **Wait until recording stops** (default) and **Stop recording & update**.
4. **Update now**: progress bar (download) → "Verifying…" (Ed25519 signature; abort + message on mismatch) → "Installing — ClipCheck will restart". Sequence in the UI process: `daemon: shutdown{reason:"update"}` (finalizes any session part, replies busy if recording) → run the signed installer in passive mode → installer replaces files, relaunches ClipCheck → UI starts and respawns the daemon → "ClipCheck updated to 0.3.0" toast.
5. **Failure handling**: download/verify/install errors leave the old version intact (installer is only started after a verified download); the pill turns into "Update failed — retry" with the reason; the previous installer is kept in `%LOCALAPPDATA%\ClipCheck\updates\previous\` for manual rollback.
6. **Settings → Updates**: current version, last checked, "Check for updates", "Automatically check" toggle (= `notify`/`never`), channel (stable; beta reserved), link to release notes.

## 7. Release pipeline (GitHub Actions)
- `ci.yml` (pull requests and `main`): `windows-latest` → fmt, clippy `-D warnings`, per-crate tests (`scripts/test-all.sh` without the desktop/GPU-only suites that cannot run on hosted runners — tagged `#[ignore]`/skipped by env `CI=1`), frontend `npm ci && npm run build && npm test`, cargo-deny, version-consistency check, release size budget.
- `release.yml` (tag `v*`): version check (tag == Cargo workspace == `tauri.conf.json`) → build frontend → `tauri build --bundles nsis` (profile `dist`) → sign updater artifact (`TAURI_SIGNING_PRIVATE_KEY`) → optional Authenticode (`signtool`, only if secrets exist) → build/verify FFmpeg pack when `ffmpeg-pack.version` changed → generate `latest.json`, `SHA256SUMS`, THIRD-PARTY-NOTICES → **draft** GitHub release (human publishes) in `RELEASE_REPO`.
- Secrets live in a protected GitHub **environment** `release` (only tagged refs on `main`): `TAURI_SIGNING_PRIVATE_KEY(+_PASSWORD)`, optional signing-cert secrets, and `RELEASES_TOKEN` (fine-grained PAT with Contents:write on the releases repo, only needed in the two-repo layout).
- `scripts/release/` (all tested locally): `check-versions.mjs`, `make-latest-json.mjs`, `package-ffmpeg-pack.ps1`, `gen-update-keys.ps1`, `size-budget.mjs`.

## 8. Config & state
- `config.toml`: `[updates] check = "notify" | "never"`, `channel = "stable"`, `interval_hours = 12`, `endpoint = ""` (empty = built-in default; override for forks/testing).
- `%APPDATA%\ClipCheck\update-state.json`: `last_checked_unix_ms`, `skipped_version`, `last_installed_from`. (Not config: written by the app, not edited by users.)

## 9. Work breakdown (each item: implementer → separate tester → fixer loop; ≤100k context each)
| ID | Work | Owner area |
|---|---|---|
| I1 | `updates` config section + validation + docs | crates/core |
| I2 | `clipcheck-update` crate: semver compare, `latest.json` + pack-manifest parsing, SHA-256, zip extract, FFmpeg pack download/verify/activate (blocking `ureq`), progress callbacks, fake-server tests | crates/update |
| I3 | `locate_ffmpeg` pack dir support; daemon `ffmpeg_missing` state + IPC (`ffmpeg_status`, `ffmpeg_install` with progress events, `shutdown{reason}` incl. busy reply) | crates/encode, apps/clipcheckd, crates/ipc |
| I4 | Tauri updater integration (Rust): plugin, keys in `tauri.conf.json`, commands `check_for_update`, `install_update` (+ progress events, recording-busy handling, daemon shutdown, update-state.json), `--shutdown-daemon` CLI | apps/ui/src-tauri |
| I5 | UI: title-bar update pill, update modal, Settings → Updates tab, FFmpeg setup banner/first-run | apps/ui/src |
| I6 | NSIS config + `installer/hooks.nsh` + license bundle + `LICENSE` | apps/ui/src-tauri, installer/ |
| I7 | Release scripts (`scripts/release/*`) + `ci.yml` + `release.yml` + `deny.toml` + `docs/release.md` (key generation, secrets, how to cut a release) | scripts/, .github/ |
| I8 | Install-flow tests on this machine (temp dir, silent): fresh install, upgrade N→N+1 keeps settings/library, uninstall keeps recordings, daemon shutdown hook, no-network check in `never` mode | tests/installer |
| I9 | Claude skill documenting the swarm workflow | .claude/skills |

Dependencies: I1 → I3/I4; I2 → I3/I4; I3/I4 → I5; I6/I7 independent; I8 after I4+I6.

## 10. Risks / open items
- **Private repo** blocks unauthenticated updates (decision 1).
- **SmartScreen** warnings until Authenticode/reputation (decision 2).
- Tauri passive NSIS update while the app (and daemon, same exe) is running: the installer must close both; covered by hooks + the pre-update daemon shutdown, tested in I8.
- First run needs internet for the FFmpeg pack; an offline "full" installer variant is a later option.
- GitHub rate limits on `latest/download` are fine at this scale; `latest.json` is a static asset.
