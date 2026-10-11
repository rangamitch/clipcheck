# ClipCheck Windows installer (NSIS)

Per-user NSIS installer built by Tauri (`tauri build --bundles nsis`) from
`apps/ui`. No administrator rights are needed.

## Files

| File | Purpose |
|---|---|
| `hooks.nsh` | Tauri NSIS hooks: stop the app/daemon, registry keys, autostart, data cleanup |
| `English.nsh` | English strings for the Tauri template; only change is the uninstaller checkbox text |
| `../LICENSE` | Apache-2.0 text, shown on the licence page and copied to `licenses\LICENSE.txt` |
| `../NOTICE` | Apache-2.0 NOTICE, copied to `licenses\NOTICE.txt` |
| `../licenses/` | Shipped next to `ClipCheck.exe` in `licenses\` (THIRD-PARTY-NOTICES) |

Configuration lives in `apps/ui/src-tauri/tauri.conf.json` under `bundle`.

## Install layout

- Install dir: `%LOCALAPPDATA%\Programs\ClipCheck\` (`installMode: currentUser`)
- Payload: `ClipCheck.exe` (UI and daemon; the daemon runs with `--daemon`), `licenses\`
- WebView2: evergreen bootstrapper, silent, only if missing (Windows 11 has it)
- FFmpeg is not bundled, downloaded or used, not even by tests (D58, D62).

## Hooks (`hooks.nsh`)

| Hook | What it does |
|---|---|
| `NSIS_HOOK_PREINSTALL` | Stop sequence (below). |
| `NSIS_HOOK_POSTINSTALL` | Writes `HKCU\Software\ClipCheck` `InstalledVersion` = `${VERSION}` and `InstallDir` = `$INSTDIR`. |
| `NSIS_HOOK_PREUNINSTALL` | Same stop sequence as PREINSTALL. Deletes `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\ClipCheck` (autostart), unless `/UPDATE`. |
| `NSIS_HOOK_POSTUNINSTALL` | Deletes `HKCU\Software\ClipCheck`. Data removal (below). |

### Stop sequence (PREINSTALL and PREUNINSTALL)

Only the copy at `$INSTDIR\ClipCheck.exe` is affected. Other copies (a portable or
developer build, or another install) and their processes are never stopped.
Matching is by full executable path, case-insensitive.

1. If `$INSTDIR\ClipCheck.exe` exists, run `ClipCheck.exe --shutdown-daemon` (graceful).
   - Exit code `3` (recording in progress): abort. A message box in interactive mode, a
     silent abort otherwise.
   - Any other exit code (including `0`, or an unknown flag in an older build): continue.
2. One PowerShell call polls for up to 10 s (20 x 500 ms). It returns as soon as no
   process running from `$INSTDIR\ClipCheck.exe` is left.
3. If such a process is still running after 10 s, it is force-stopped
   (`Stop-Process -Force`), again only for that path.

The old `taskkill /F /IM ClipCheck.exe` step is gone. It killed every process with that
image name, including other copies.

### Data removal (POSTUNINSTALL)

Recordings are never deleted. The default recording folder is
`%USERPROFILE%\Videos\ClipCheck`, and user-chosen folders are not known to the
installer. Nothing in the uninstaller reads or deletes them.

No recursive delete is applied to a parent folder. Only the known app-owned files and
folders below are removed, and each parent folder is removed with a non-recursive
`RMDir`, which succeeds only when it is empty.

If the data checkbox was ticked (and not `/UPDATE`):

- `%APPDATA%\ClipCheck`: files `config.toml`, `config.toml.bak`, `library.db*`,
  `encoder-cache.json`, `update-state.json`. Folders `logs`, `thumbs` (recursive). Then
  `RMDir` on the folder.
- `%LOCALAPPDATA%\ClipCheck`: folders `buffer`, `ffmpeg` (legacy, pre-D58), `updates`, `logs`, `sessions`
  (recursive). Then `RMDir` on the folder.

Always (also on `/UPDATE`):

- `%LOCALAPPDATA%\ClipCheck\ffmpeg` (recursive; legacy FFmpeg pack folder left by versions before D58)
- `%LOCALAPPDATA%\ClipCheck\updates\previous` (recursive)

A user-chosen buffer or storage folder that lies outside these known folders is never
touched. One that lies inside a removed folder (for example under `...\ClipCheck\buffer`)
is removed with it, so the app must not keep recordings in those folders.

### Exit-code contract (for the app)

`ClipCheck.exe --shutdown-daemon` should exit:
- `0`: daemon shut down (or was not running)
- `3`: refused because a recording is in progress (`CC_SHUTDOWN_BUSY` in `hooks.nsh`)
- anything else: treated as a failed graceful shutdown; the installer waits up to 10 s, then force-stops only the processes running from `$INSTDIR\ClipCheck.exe`

## Uninstall and the data checkbox

The uninstaller asks whether to delete data, with the checkbox text from
`English.nsh`:

> Also remove my ClipCheck settings and library (recordings are never deleted)

Tauri's default label was "Delete the application data". The template has no
`!define` for the text. It is a `LangString` in the language file, so the text is
overridden through `bundle.windows.nsis.customLanguageFiles`. Tauri's template
also runs its own `RmDir /r` on `%APPDATA%\dev.clipcheck.app` for the checkbox;
that folder is not used by ClipCheck and is harmless.

Silent uninstall (`/S`) leaves the checkbox unticked, so settings and library are kept.

## Silent modes

- `/S` silent install and uninstall are supported.
- `/P` passive mode is used by the in-app updater.
- `/UPDATE` is set by the Tauri updater. Autostart is kept and settings are not deleted.

## Build

From `apps/ui`:

```
npm run build
npx tauri build --bundles nsis
```

Output: `apps/ui/src-tauri/target/release/bundle/nsis/ClipCheck_<version>_x64-setup.exe`.
The signed updater artifact (`.exe.sig`) requires `TAURI_SIGNING_PRIVATE_KEY`
(set in the `release` environment in CI). See `docs/release.md` once written.

## Not done here

- The app side of `--shutdown-daemon` (exit codes above).
- The updater plugin, its public key and `latest.json` (`plugins.updater`).
- Authenticode signing (optional; added by the release workflow when secrets exist).
