# 10 — App shell, IPC, service lifecycle and packaging

Status: Draft v1 · Date: 2026-10-09 · Depends on: `00-brief.md`
Crates/apps owned: `crates/ipc/`, `apps/clipcheckd/` (tray, supervisor, single-instance, autostart, notifications, diagnostics), `apps/ui/src-tauri/` (shell only), `installer/`, `.github/workflows/`, root `Cargo.toml`.
Not owned here: detection, capture, encode, audio, buffer, library schema, clip editing (see their docs). Screen contents for Gallery, Player/Editor and Settings are summarized here only for navigation and shell behavior.

> **Superseded in part (read first).** The shipped process model is DESIGN.md **D46**: one executable, `ClipCheck.exe` (UI) and `ClipCheck.exe --daemon` (recorder, tray, IPC). There is **no** `clipcheckd --worker` child, worker supervisor, heartbeat, `RestartWorker` command or `WorkerRestarted` event; every passage below that describes them (§1.2 Processes and Reliability rows, §2 process diagram, §5.1, §5.4 worker stop, §5.5) is historical design, not the implementation. Recording runs on the daemon's service thread and engine threads; FFmpeg is an external `ffmpeg.exe` child (D14). `StatusDto.worker_pid` / `worker_restarts_10min` are kept for wire compatibility and are always `None` / 0. Shutdown modes are DESIGN.md **D47** (`graceful` | `update` | `force`); session-part finalizing is **D48**.

---

## 1. Goal & scope

### 1.1 Goal
Provide the process shell around ClipCheck: a tiny always-available recorder process (`clipcheckd`) with a tray icon, an on-demand Tauri UI (`clipcheck`), a versioned local IPC protocol between them, correct single-instance and autostart behavior on Windows, crash containment for the capture/encode path, and a signed, reproducible installer with opt-in updates and no telemetry.

### 1.2 In scope / out of scope
| Area | In | Out |
|---|---|---|
| Processes | `clipcheckd.exe` (tray, IPC server, detection host, supervisor), `clipcheckd.exe --worker` (capture/encode/buffer/session, spawned on demand), `clipcheck.exe` (Tauri UI) | Game hook / DLL injection (v2 stretch) |
| IPC | Named pipe `\\.\pipe\clipcheck-ipc`, length-prefixed JSON, protocol v1, commands/replies/events, ACL and peer verification | gRPC, HTTP, any network listener |
| Shell | Tray icon and menu, Windows toast notifications, UI navigation map, UI-to-daemon lifecycle | In-game overlay or HUD (explicitly excluded) |
| Startup | Single instance, HKCU Run key autostart, `clipcheck://` protocol registration | Windows Service (SCM), Task Scheduler, admin-required setup |
| Reliability | Worker supervisor, heartbeat, restart policy, crash records, minidumps | Kernel or driver recovery |
| Ops | Logging, diagnostics bundle, NSIS installer, FFmpeg DLL layout, Authenticode signing, opt-in updater, CI, release profile | Linux/macOS packaging, Microsoft Store, MSIX |
| Verification | Budget measurement harness, egress verification, ACL tests | Detailed capture/encode benchmarks (owned by those docs) |

### 1.3 Non-goals (v1)
- No administrator rights required for install, run, autostart or update.
- No SCM service. "Service" in this doc means the per-user background process `clipcheckd`, consistent with the brief's "tiny background process".
- No cloud sync, accounts, analytics or crash upload.
- No concurrent recording for multiple Windows users. One interactive user owns the recorder at a time (Open questions Q2).

---

## 2. User-facing behavior (precise)

### 2.1 First launch
1. User runs the installer (per-user, no UAC prompt). The installer ends with "Launch ClipCheck" checked.
2. `clipcheck.exe` opens the **Welcome** route (`/welcome`), three steps: Recordings folder (default `%USERPROFILE%\Videos\ClipCheck\`), Autostart toggle (default **off**), Updates mode (default **never**, see §3.6). Choices are written to `config.toml` and applied through IPC (`reload_settings`, `set_autostart`).
3. If `clipcheckd` is not running, the UI starts it (`clipcheckd.exe --background`) before showing Welcome. The tray icon appears within 1.5 s on a warm disk.
4. Closing the Welcome window does not stop the recorder.

### 2.2 Tray icon states
| State | Icon | Tooltip (first line) | Trigger |
|---|---|---|---|
| Idle | Monochrome C | `ClipCheck: idle` | No game detected |
| Armed | C with green dot | `ClipCheck: armed for <Game>` | Game detected, recording not started |
| Recording | C with red dot | `ClipCheck: recording <Game> (part N)` | Session writer active |
| Buffer only | C with blue dot | `ClipCheck: replay buffer on` | Buffer active, session disabled |
| Paused | C with grey bars | `ClipCheck: detection paused` | User paused detection, or session lock policy |
| Degraded | C with amber `!` | `ClipCheck: degraded (<reason>)` | Restart budget exhausted or fatal worker error |

Icon changes are debounced: a state is applied only after it has been stable for 500 ms.

### 2.3 Tray menu (contents, in order)
```
ClipCheck: recording Valorant (part 2)          [disabled status line]
------------------------------------------------
Save clip: last 30 s                            [enabled when buffer or session is active]
Save clip: last 60 s
Save clip: last 5 min
Split session now                               [enabled only when recording]
Stop recording                                  [toggle: "Start recording" when stopped]
Microphone: Mute / Unmute                       [toggle, label reflects state]
Pause detection                                 [toggle: "Resume detection" when paused]
------------------------------------------------
Open Gallery
Open Live status
Settings...
Open recordings folder
------------------------------------------------
Diagnostics
    Export diagnostics bundle...
    Open logs folder
Check for updates...                            [manual, always available, one network call]
Restart recorder                                [shown only in Degraded state]
Quit ClipCheck recorder                         [stops daemon; confirms first if recording]
```
Rules:
- The menu is built on each open from the current `StatusDto`, so labels are never stale.
- Left click opens Gallery (`tray.left_click_action = "gallery"`). Double-click has no extra action.
- Quit while recording shows a native confirmation (`MessageBoxW`, default button Cancel). Quitting finalizes the open part first (§5.4).

### 2.4 In-game notifications (overlay-free)
ClipCheck never draws over the game. All feedback is out-of-band.

| Event | Channel | Default |
|---|---|---|
| Clip saved | Windows toast (buttons: "Open", "Show in folder"); optional system sound | Toast on, sound off |
| Session started / stopped / split | Toast | On |
| Non-fatal warning (e.g. "Window not producing frames, black fallback") | Toast, rate-limited to one per 60 s per code | On |
| Fatal error or Degraded | Toast (never suppressed) and amber tray icon | Always on |
| Non-fatal event while a fullscreen app is foreground | Toast suppressed, event logged, tray icon still updates | `notifications.suppress_when_fullscreen` |

Suppression check: `SHQueryUserNotificationState` returns `QUNS_BUSY`, `QUNS_RUNNING_D3D_FULL_SCREEN` or `QUNS_PRESENTATION_MODE`. Suppressed non-fatal toasts are dropped, not queued, to avoid a burst after alt-tab. A fatal toast that arrives during fullscreen is held until the state returns to `QUNS_ACCEPTS_NOTIFICATIONS` or 30 s pass, whichever comes first.

### 2.5 One UI window, many entry points
- At most one UI window exists. Every entry point (tray, toast button, second launch of `clipcheck.exe`, `clipcheck://` link) forwards its route to the running UI or starts one.
- Closing the UI window follows `general.close_behavior` (default `minimize_to_tray`). With `minimize_to_tray` the UI process exits and the daemon with its tray icon keeps running. With `exit`, the UI asks for confirmation when a session is recording (`confirm_exit_while_recording`) or when the daemon state cannot be read, then stops the daemon and exits. See `docs/design/09` and `crates/core/src/config/schema.rs` (`CloseBehavior`).

### 2.6 Autostart
- Setting in Settings > General and in Welcome. When enabled, the daemon writes the HKCU Run value described in §3.4.5.
- Logon starts `clipcheckd.exe --background` (tray only, no UI). Work not needed for the tray, such as the encoder probe, is deferred 5 s after logon.
- If Windows has disabled the startup entry (`StartupApproved\Run`), the Settings toggle shows "Disabled in Windows Startup settings" with a link to the Startup apps page. The daemon never re-enables it.

### 2.7 UI navigation map and screens
#### Routes
```
/welcome                          first run (three steps)
/gallery                          default; tabs: All | Clips | Sessions
  /gallery?game=<id>&sort=...     filters are query params, never paths
/clip/<ULID>                      Player/Editor (mode = view | edit)
/live                             Live status
/settings/general
/settings/recording               owned by encode doc (codec, rate control, fps, resolution)
/settings/audio                   owned by audio doc (tracks, mic, push-to-talk)
/settings/hotkeys                 owned by hotkeys doc
/settings/storage                 recordings root, buffer location, retention
/settings/notifications
/settings/updates
/settings/diagnostics
/settings/about
/error/recorder-unavailable       Unavailable state (Start button)
/error/incompatible               ProtoMismatch
```
Rules:
- Tray entries map to `/gallery`, `/live`, `/settings/general`.
- Toast actions map to `/clip/<ULID>` or `/gallery`.
- Unknown route opens `/gallery` with a non-modal banner "Unknown link".
- Back and forward work within one window. Deep links do not stack history across launches.

```
          +-----------+
          |  welcome  |  (first run only)
          +-----+-----+
                |
  +-------------v--------------+        +-----------+
  |          gallery           |<------>|   live    |
  +--+------------+------------+        +-----------+
     | open clip  | settings
     v            v
 +-----------+  +--------------------------------------------+
 | clip/:id  |  | settings: general|recording|audio|hotkeys|   |
 | view|edit|  | storage|notifications|updates|diag|about   |
 +-----------+  +--------------------------------------------+
   (any screen if recorder unavailable -> error/recorder-unavailable)
```

#### Screen shells (behavior owned here)
| Screen | Shell behavior owned by this doc | Detailed content owned by |
|---|---|---|
| Welcome | Three-step wizard; writes config; calls `set_autostart`; shows result | settings doc |
| Gallery | Connection banner; refresh on `clips` and `sessions` events; "Delete" moves to Recycle Bin (`SHFileOperationW` with `FOF_ALLOWUNDO`), never permanent | library doc |
| Clip (Player/Editor) | Loads `<video src="clip://clip/<ULID>">`; route guard checks the ULID exists in the library | clipedit doc |
| Live | Subscribes to `state`, `health`, `levels` only while mounted; unsubscribes on unmount so the 10 Hz level stream costs nothing otherwise | this doc (topics), UI doc (meters) |
| Settings | Forms bound to config; Apply sends `reload_settings`; invalid fields show inline `ConfigIssue`s | settings doc |
| Recorder unavailable | [Start recorder] runs `clipcheckd.exe --background` and retries connect; [Diagnostics] | this doc |

#### Tauri shell specifics
- Tauri v2 with `withGlobalTauri = false`. The webview gets only the permissions it needs (`core:default` plus the app's own commands). No filesystem or shell plugin is exposed to the webview.
- All backend calls go through Tauri commands in `src-tauri` (module `ipc_client`). The webview never opens the pipe.
- CSP: `default-src 'self'; media-src 'self' http://clip.localhost; img-src 'self' data: http://clip.localhost; connect-src 'self' ipc: http://ipc.localhost; script-src 'self'; style-src 'self' 'unsafe-inline'`.
- `clip://` is the logical scheme used in this doc set. WebView2 on Windows serves a custom scheme as `http://clip.localhost/...` (Q8). The handler is registered with `register_asynchronous_uri_scheme_protocol("clip", ...)` and supports `Range` requests (206 Partial Content, `Accept-Ranges: bytes`).
- The clip protocol resolves only `clip/<ULID>`, via a read-only library query, to a file path. Any other request returns 404. The webview never supplies a file path.
- DevTools are disabled in release builds. Navigation outside the app origin is blocked.

---

## 3. Architecture & data flow

### 3.1 Process topology
```
                      per-user desktop session
 +--------------------------------------------------------------------+
 |  clipcheck.exe (Tauri UI, short-lived)                             |
 |    webview (WebView2) --Tauri commands--> src-tauri ipc client     |
 |    clip:// protocol --> library.db (read-only, WAL) + file serving |
 +--------------|-----------------------------------------------------+
                |  \\.\pipe\clipcheck-ipc  (JSON frames, ACL = current user)
 +--------------v-----------------------------------------------------+
 |  clipcheckd.exe (tray, long-lived, target 8-10 MB RSS)             |
 |    IPC server  | Tray (Shell_NotifyIcon) | Hotkeys | Detection     |
 |    Config watcher | Autostart | Notifications | Diagnostics        |
 |    Worker supervisor (Job object, heartbeat, restart policy)       |
 +--------------|-----------------------------------------------------+
                |  spawned on demand: clipcheckd.exe --worker --hb <h>
 +--------------v-----------------------------------------------------+
 |  clipcheckd.exe --worker (capture/encode/buffer/session/library)   |
 |    detect -> capture -> encode -> fan-out (brief pipeline)         |
 |    exits after 30 s grace when nothing is armed and buffer is off  |
 +--------------------------------------------------------------------+
```

Rationale for the worker split (Q1): FFmpeg, D3D11 driver and WGC faults can crash or hang a process. The parent stays small, keeps the tray and IPC alive through a worker crash, and restarts the worker. When nothing is armed the worker does not exist, so the idle budget (< 15 MB, < 0.3 % CPU) is met by the parent alone.

### 3.2 Crate placement
| Concern | Location |
|---|---|
| Frame codec, message enums, versioning, pipe server and client, ACL | `crates/ipc` |
| Config schema, paths (`%APPDATA%`, `%LOCALAPPDATA%`), error types, ids, logging init | `crates/core` |
| Tray, toasts, autostart registry, single-instance mutex, supervisor, diagnostics | `apps/clipcheckd/src/{tray,notify,autostart,singleton,supervisor,diag,main}.rs` |
| Worker entry point and command dispatch | `apps/clipcheckd/src/worker.rs` (calls `session`, `buffer`, `library`) |
| Tauri commands (thin wrappers over the `ipc` client) | `apps/ui/src-tauri/src/{ipc_client,commands,routes}.rs` |
| Installer script, FFmpeg manifest, licenses | `installer/` |

### 3.3 Data flow for a typical action ("Save clip, 30 s" from the UI)
```
UI button -> Tauri command save_clip(30) -> ipc client (frame id=01J..)
  -> pipe -> daemon IPC server: verify peer SID, decode frame, check proto
  -> daemon forwards SaveClip to worker over the worker pipe
  -> worker: buffer::extract(30 s) -> writes clip file -> library insert
  -> worker emits ClipSaved -> daemon fans out to subscribers
  -> daemon has already replied ClipQueued (reply is immediate, work is async)
  -> UI receives clips topic event -> refreshes Gallery
  -> daemon: toast "Clip saved" (if enabled and not suppressed)
```

### 3.4 Windows platform integration

#### 3.4.1 Pipe ACL and creation
```
SDDL (protected DACL, no inheritance):
  D:P(A;;GA;;;<current user SID>)(A;;GA;;;SY)
  - <current user SID> read from the daemon's own token (TokenUser) at startup
  - SYSTEM kept so Windows and endpoint protection can inspect the pipe
  - no Everyone, no Authenticated Users, no Administrators ACE
Built with ConvertStringSecurityDescriptorToSecurityDescriptorW(SDDL_REVISION_1).
CreateNamedPipeW flags:
  PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED
  PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS
  nMaxInstances = MAX_CLIENTS + 1 (one spare instance waiting for connect)
  nInBufferSize = nOutBufferSize = 65536
```
`FILE_FLAG_FIRST_PIPE_INSTANCE` stops a squatter from becoming the first owner of the name. Together with the singleton mutex, a second daemon cannot silently replace the first.

#### 3.4.2 IPC threat model
| Threat | Mitigation |
|---|---|
| Other Windows users connect | Protected user-only DACL; peer SID check by impersonation (§5.6) |
| Remote SMB clients | `PIPE_REJECT_REMOTE_CLIENTS` |
| Pipe squatting before the daemon starts | `FILE_FLAG_FIRST_PIPE_INSTANCE`; daemon exits with code 2 if it fails |
| Fake daemon in front of the UI | UI verifies the server image path (§5.6) |
| Malicious frames (huge lengths, bad JSON) | 1 MiB cap, strict types, 3 s handshake timeout, max 8 clients |
| Arbitrary file writes | Daemon never accepts write paths; diagnostics are staged internally; clip ids are ULIDs resolved by the library |
| Same-user malware | Out of scope: it can already read the user's files. Documented, not mitigated in v1 |

#### 3.4.3 Single-instance mutex
`Local\ClipCheck.Recorder` (session-local). The mutex is per session, so Fast User Switching sessions are independent at this level. The pipe is machine-wide (Q2).

#### 3.4.4 Tray implementation
- Hidden message-only window (`HWND_MESSAGE`), class `ClipCheckTray`.
- `Shell_NotifyIconW(NIM_ADD)` with `NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_GUID`. The GUID is generated once per install and stored in `state.json`.
- Icons: 16, 20, 24 and 32 px variants per state, embedded as `.ico`, chosen with `GetSystemMetricsForDpi`.
- Registered `TaskbarCreated` message: re-adds the icon when Explorer restarts.
- Menu: `TrackPopupMenuEx` with `TPM_RIGHTBUTTON | TPM_RETURNCMD`, after `SetForegroundWindow` (needed so the menu dismisses correctly).
- Callback version `NOTIFYICON_VERSION_4`.

#### 3.4.5 Autostart
| Item | Value |
|---|---|
| Key | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Value name | `ClipCheck` |
| Data | `"C:\Users\<u>\AppData\Local\Programs\ClipCheck\clipcheckd.exe" --background` (quoted, absolute) |
| Windows toggle | `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run\ClipCheck` (first byte `0x02` = enabled, `0x03` = disabled) |
| Admin needed | No |

Task Scheduler was considered and rejected (Q3).

#### 3.4.6 Protocol registration (`clipcheck://`)
Registered per user at install, and at daemon start if missing:
```
HKCU\Software\Classes\clipcheck
  (Default) = "URL:ClipCheck Protocol"
  URL Protocol = ""
  shell\open\command = "C:\...\clipcheck.exe" "%1"
```
The UI parses `%1` strictly: scheme `clipcheck`, route allowlist (§2.7), ULID validation. Unknown routes open Gallery.

#### 3.4.7 Toast notifications
- AppUserModelID `ClipCheck.Recorder` for the daemon. The installer sets `System.AppUserModel.ID` on the Start Menu shortcut. Dev builds register `HKCU\Software\Classes\AppUserModelId\ClipCheck.Recorder` (`DisplayName = "ClipCheck"`, `IconUri` = installed icon).
- Toast XML is built with `Windows.Data.Xml.Dom`. Text is XML-escaped. No remote images.

#### 3.4.8 Power and session events
- `WTSRegisterSessionNotification` for lock and unlock. Pauses or resumes detection when `service.pause_on_session_lock` is set.
- `RegisterPowerSettingNotification` and `PBT_APMRESUMEAUTOMATIC`: the worker is told to re-create the capture device. Capture doc owns the handling.
- `WM_QUERYENDSESSION` starts the Graceful shutdown path (§5.4) and returns `TRUE`. If a recording is active, the daemon calls `ShutdownBlockReasonCreate` and returns `FALSE` until the user confirms.

### 3.5 Logging and diagnostics architecture
| Item | Decision |
|---|---|
| Crates | `tracing`, `tracing-subscriber` (env-filter), `tracing-appender` (non-blocking, daily rolling) |
| Location | `%LOCALAPPDATA%\ClipCheck\logs\clipcheckd.YYYY-MM-DD.log`, `clipcheck-ui.YYYY-MM-DD.log`. The worker writes to `clipcheckd.YYYY-MM-DD.log` with `proc=worker` |
| Format | One line per event: RFC 3339 UTC timestamp, level, target, `pid`, `tid`, message, then structured fields as `key=value` |
| Retention | Pruned at start: files older than `logging.retention_days` are removed, then oldest-first until the total is under `logging.max_total_mb` |
| Privacy | Logs contain exe names and ids. They never contain window titles (unless `diagnostics.include_window_titles`), never full paths outside the recordings root, and the username is replaced by `%USER%`. IPC payload bodies are not logged at info level |
| Levels | Default `info`. `CLIPCHECK_LOG=debug` or `clipcheckd=trace` overrides for support sessions |
| Panics | Panic hook logs the message and location, then writes a crash record (§5.5) |

Diagnostics bundle contents and steps are in §5.10. A test asserts that the bundle contains no Windows username unless it appears inside a `%USER%` token.

### 3.6 Packaging, signing and updates

#### 3.6.1 Installer choice
| Option | Decision |
|---|---|
| NSIS (per-user) | **Chosen.** Per-user install with no UAC, small, supported by the Tauri bundler, scriptable silent install for updates |
| WiX MSI | Deferred. Better for enterprise or GPO deployment; more work for per-user install and the FFmpeg tree. Revisit if demand appears (Q4) |
| MSIX | Rejected for v1: the signing and store model is awkward for a tray app that uses a Run key |

| Install parameter | Value |
|---|---|
| Install dir | `%LOCALAPPDATA%\Programs\ClipCheck\` |
| Mode | `installMode: currentUser` (no admin) |
| Shortcuts | Start Menu `ClipCheck.lnk` with AUMID set. Desktop shortcut off by default |
| Uninstall entry | `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\ClipCheck` |
| Compression | LZMA solid |
| WebView2 | Evergreen bootstrapper in `downloadBootstrapper` mode (WebView2 is preinstalled on Windows 11 and most Windows 10 2004+ systems) |
| Pre-install | Ask the running daemon to shut down (`shutdown{graceful}`). If recording, the installer stops with a message (the silent update path uses `update` mode, §5.12). After 10 s, terminate the processes |
| Post-install | Write `HKCU\Software\ClipCheck\InstalledVersion`; register `clipcheck://`; reconcile autostart (§5.8) |
| Uninstall | Stops processes, removes the Run value, protocol keys and Start Menu shortcut. Settings and library (`%APPDATA%\ClipCheck`, `%LOCALAPPDATA%\ClipCheck`) are kept unless the "Also delete settings and library" box is checked (default unchecked). Recordings are never deleted by the uninstaller |

#### 3.6.2 Installed layout
```
%LOCALAPPDATA%\Programs\ClipCheck\
  clipcheck.exe                       UI (Tauri)
  clipcheckd.exe                      recorder (tray, and worker mode)
  ffmpeg\
    manifest.json                     version, configure line, SHA-256 per DLL
    bin\avutil-*.dll, avcodec-*.dll, avformat-*.dll, avfilter-*.dll,
        swresample-*.dll, swscale-*.dll, libx264-*.dll, libx265-*.dll
  resources\                          icons, tray .ico variants
  licenses\
    LICENSE-GPL-3.0.txt               project and FFmpeg GPL build terms
    LICENSE-LGPL-2.1.txt              FFmpeg LGPL terms for the LGPL-licensed parts
    FFMPEG-SOURCE.txt                 exact source revision and build script URL (source offer)
    THIRD-PARTY-NOTICES.txt           generated by cargo-about
```

#### 3.6.3 FFmpeg DLL distribution and loading
- Build: an in-house reproducible build of FFmpeg as a **shared** GPL build (includes x264 and x265, which the GPL-3.0-or-later project allows). NVENC, AMF and QSV are built against their headers and load their runtime from the driver at run time, so no SDK DLLs are redistributed. The build script and configure line are pinned in `installer/ffmpeg/build.ps1` and recorded in `manifest.json`.
- Loading uses absolute paths only, never a PATH search:
  ```rust
  let dir = install_dir().join("ffmpeg").join("bin");
  let wide_dir = to_wide(&dir);
  AddDllDirectory(PCWSTR(wide_dir.as_ptr()));              // registers dir for dependency lookup
  LoadLibraryExW(PCWSTR(to_wide(&dir.join("avcodec-61.dll")).as_ptr()),
                 None, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);  // dependencies resolved from added dir
  ```
  The `avcodec-61.dll` name is an example; the encode doc owns the exact version.
- Integrity: at worker start, each DLL's SHA-256 is compared with `manifest.json`. A mismatch removes all encoders from the probe list and sets `Degraded{reason="ffmpeg integrity check failed"}`. The check takes about 30 ms and is cached by size and mtime in `state.json`.
- Override: the `ffmpeg.path` config key (encode doc) can point to a user-supplied shared build. That build is not hash-checked, and the UI shows a "custom FFmpeg" badge.
- Size: estimated 22–28 MB uncompressed, roughly 12–15 MB in the NSIS LZMA payload. Total installer target is under 40 MB (brief quality bar).
- LGPL/GPL obligations: the source offer file, the pinned source URL and revision, and license texts are shipped with the installer. Because x264 and x265 are GPL, the distributed binary set is GPL-3.0-or-later as a whole, which matches the project license.

#### 3.6.4 Code signing
| Artifact | Signed | Method |
|---|---|---|
| `clipcheck.exe`, `clipcheckd.exe` | Yes: Authenticode, SHA-256, RFC 3161 timestamp | `signtool sign /fd SHA256 /tr <tsa> /td SHA256` in CI |
| NSIS installer and uninstaller | Yes | Same |
| FFmpeg DLLs (our build) | Yes | Same. The hash manifest also covers them |
| Tauri update manifest | Ed25519 (`tauri signer`), separate from Authenticode | Private key in CI secret, public key compiled into the UI |

- Certificate: prefer Azure Trusted Signing (cloud HSM, no USB token in CI) or an OV code-signing certificate on an HSM. Decision pending eligibility (Q6). SmartScreen reputation builds per publisher and per file, so early releases may show warnings.
- CI runs `signtool verify /pa /v` on every shipped PE and the installer, and fails the release if any file is unsigned.
- Keys are never in the repository. CI uses environment secrets in a protected release environment.

#### 3.6.5 Auto-update (opt-in, no telemetry)
| Aspect | Decision |
|---|---|
| Default | `updates.check = "never"`: no network calls except a user-initiated "Check for updates..." |
| Opt-in | `notify`: periodic check, toast only. Install requires a click |
| Endpoint | `https://updates.clipcheck.example/<channel>/latest.json` (placeholder domain, Q7) |
| Request | GET only, no cookies, no identifiers. User-Agent `ClipCheck/<version> (Windows x64)` |
| Manifest | Tauri updater format: version, notes URL, per-platform URL, Ed25519 signature |
| Install | Verified download (signature and Authenticode), then the silent per-user NSIS run after a clean shutdown (§5.12) |
| Rollback | Not automatic. The previous installer is kept in `updates\previous\` for one version |
| No telemetry | No analytics SDKs, no crash upload, no usage pings, no remote feature flags. WebView2 background networking is reduced with `AdditionalBrowserArguments="--disable-background-networking"`, and egress is verified (§9.3) |

---

## 4. Key types, traits and function signatures

### 4.1 `crates/ipc` constants and frame
```rust
pub const PIPE_NAME: &str = r"\\.\pipe\clipcheck-ipc";
pub const PROTO_VERSION: u16 = 1;            // major protocol version implemented by this build
pub const PROTO_MIN_SUPPORTED: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1 << 20;  // 1 MiB body limit, enforced on read AND write
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
pub const MAX_CLIENTS: u8 = 8;

/// Wire frame. Requests and replies carry `id`; pushed events carry `seq` and no `id`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Frame<B> {
    pub v: u16,                        // must equal the negotiated proto (hello itself uses v = 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,            // ULID, client-generated, echoed in the reply
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,              // per-daemon monotonic event sequence
    pub body: B,
}

pub type Request = Frame<Command>;
pub type Response = Frame<Reply>;
pub type Push = Frame<Event>;
```

### 4.2 Message enums (full set, protocol v1)
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Command {
    Hello { proto_min: u16, proto_max: u16, client: String, client_version: String },
    Ping,
    GetStatus,
    Subscribe { topics: Vec<Topic> },
    Unsubscribe { topics: Vec<Topic> },
    SetRecording { enabled: bool },                        // start/stop the full session
    Split,                                                 // split current session at next keyframe
    SaveClip { seconds: u32, game_id: Option<String> },    // seconds 1..=3600
    SetMicMuted { muted: bool },
    SetDetectionPaused { paused: bool, until_unix_ms: Option<i64> },
    ReloadSettings,
    SetAutostart { enabled: bool },
    ExportDiagnostics,                                     // writes staging bundle, returns its path
    Activate { route: Option<String> },                    // show or start the UI at a route (allowlist, §2.7)
    RestartWorker,                                         // valid in Degraded, or on explicit user request
    Shutdown { mode: ShutdownMode },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Reply {
    HelloAck { proto: u16, daemon_version: String, daemon_pid: u32, features: Vec<String> },
    Pong { unix_ms: i64 },
    Status(StatusDto),
    Subscribed { topics: Vec<Topic> },
    Unsubscribed { topics: Vec<Topic> },
    ClipQueued { clip_id: String },
    DiagnosticsReady { path: String, bytes: u64 },
    Ok,
    Error { code: ErrorCode, message: String, retryable: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    StateChanged { state: RecState, game: Option<GameRef> },
    GameDetected { game: GameRef },
    GameLost { game_id: String, reason: String },
    SessionPartStarted { session_id: String, part: u32, path: String },
    SessionPartFinished { session_id: String, part: u32, path: String, duration_ms: u64, bytes: u64, recovered: bool },
    ClipSaved { clip_id: String, path: String, duration_ms: u64, bytes: u64 },
    CaptureHealth { fps: f32, dropped_frames: u64, black_frames_active: bool },
    EncoderSelected { codec: String, encoder: String },
    AudioLevels { tracks: Vec<TrackLevel> },               // topic "levels", 10 Hz, only while subscribed
    MicMuteChanged { muted: bool },
    DetectionPauseChanged { paused: bool },
    SettingsReloaded { ok: bool, errors: Vec<ConfigIssue> },
    WorkerRestarted { attempt: u32, reason: String, exit_code: Option<i32> },
    Degraded { reason: String },
    Warning { code: String, message: String },
    ShuttingDown { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic { State, Clips, Sessions, Health, Levels, Warnings, Settings }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode { Graceful, Update, Force }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ProtoMismatch, Unauthorized, BadRequest, UnknownType, FrameTooLarge,
    NotRecording, InvalidState, Busy, ShuttingDown, WorkerUnavailable, Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecState { Idle, Armed, Recording, BufferOnly, Splitting, Stopping, Paused, Degraded }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameRef {
    pub game_id: String,                 // GameId from brief: normalized exe name (+ store id)
    pub exe: String,
    pub pid: u32,
    pub title: Option<String>,           // only when diagnostics.include_window_titles
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDto {
    pub state: RecState,
    pub game: Option<GameRef>,
    pub session: Option<SessionInfo>,
    pub buffer_seconds: u32,
    pub buffer_bytes: u64,
    pub encoder: Option<String>,
    pub codec: Option<String>,
    pub fps_target: u32,
    pub bitrate_kbps: Option<u32>,
    pub mic_muted: bool,
    pub detection_paused: bool,
    pub autostart: AutostartState,
    pub worker_pid: Option<u32>,
    pub worker_restarts_10min: u32,
    pub uptime_s: u64,
    pub degraded_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo { pub session_id: String, pub part: u32, pub started_unix_ms: i64, pub path: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutostartState { Enabled, Disabled, DisabledByWindows, PathStale }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackLevel { pub track: u8, pub peak_db: f32, pub rms_db: f32 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigIssue { pub key: String, pub message: String }
```
Serialization rules (normative): all enum tags are `snake_case`; no `deny_unknown_fields` on any wire type; timestamps are `unix_ms` integers (brief convention); paths are JSON strings in Windows form.

### 4.3 Traits (server and client)
```rust
/// Length-prefixed frame IO over any byte stream (pipe handle in production, in-memory in tests).
pub trait FrameIo {
    fn read_frame(&mut self) -> Result<Vec<u8>, IpcError>;   // FrameTooLarge, Eof, Io
    fn write_frame(&mut self, body: &[u8]) -> Result<(), IpcError>;
}

/// Verifies that the connected peer runs as the same Windows user as the daemon.
pub trait PeerVerifier: Send + Sync {
    fn verify(&self, pipe: HANDLE) -> Result<PeerInfo, AuthError>;
}

pub struct PeerInfo { pub pid: u32, pub user_sid: String, pub elevated: bool }

/// Implemented by the daemon. Called on the connection thread; must not block for long.
pub trait CommandHandler: Send + Sync {
    fn handle(&self, peer: &PeerInfo, req: Request) -> Response;   // reply always carries the request id
    fn on_subscribe(&self, peer: &PeerInfo, topics: &[Topic]) -> Result<(), ErrorCode>;
}

pub struct PipeServer { /* accept thread, instance pool, subscriber registry */ }
impl PipeServer {
    /// Fails with IpcError::AlreadyRunning if FILE_FLAG_FIRST_PIPE_INSTANCE cannot be obtained.
    pub fn start(handler: Arc<dyn CommandHandler>,
                 verifier: Arc<dyn PeerVerifier>,
                 shutdown: ShutdownToken) -> Result<Self, IpcError>;
    /// Assigns the next seq and fans out to subscribers of the event's topic.
    pub fn publish(&self, event: Event);
    pub fn stop(self, grace: Duration);
}

pub struct PipeClient { /* HANDLE, next_id, pending map, event receiver */ }
impl PipeClient {
    pub fn connect(timeout: Duration, check: ServerCheck) -> Result<Self, IpcError>;
    pub fn hello(&mut self) -> Result<Reply, IpcError>;               // negotiation, see §5.3
    pub fn call(&mut self, cmd: Command, timeout: Duration) -> Result<Reply, IpcError>;
    pub fn events(&self) -> &std::sync::mpsc::Receiver<Push>;
}

/// Client-side check that the server is the installed clipcheckd.exe.
pub struct ServerCheck { pub expected_image: PathBuf }   // %LOCALAPPDATA%\Programs\ClipCheck\clipcheckd.exe

pub struct ShutdownToken(Arc<(Mutex<bool>, Condvar)>);
impl ShutdownToken {
    pub fn new() -> Self;
    pub fn trigger(&self);
    pub fn wait(&self, timeout: Duration) -> bool;
    pub fn is_set(&self) -> bool;
}
```

### 4.4 Errors
```rust
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("frame of {0} bytes exceeds MAX_FRAME_BYTES")] FrameTooLarge(usize),
    #[error("peer closed the pipe")] Eof,
    #[error("protocol mismatch: server {server}, client supports {min}..={max}")]
    ProtoMismatch { server: u16, min: u16, max: u16 },
    #[error("server is not the expected clipcheckd.exe ({0})")] UntrustedServer(String),
    #[error("another clipcheckd instance owns the pipe")] AlreadyRunning,
    #[error("request timed out after {0:?}")] Timeout(Duration),
    #[error("json: {0}")] Json(#[from] serde_json::Error),
    #[error("win32 error {0:#x}")] Win32(u32),
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("peer user SID differs from daemon user SID")] DifferentUser,
    #[error("peer is remote (rejected by PIPE_REJECT_REMOTE_CLIENTS)")] Remote,
    #[error("cannot read peer token: win32 {0:#x}")] TokenUnavailable(u32),
}
```

### 4.5 Daemon-side types
```rust
pub struct SingleInstance { mutex: OwnedHandle }
impl SingleInstance {
    pub fn acquire(name: &str) -> Result<Self, SingleInstanceError>;   // "Local\ClipCheck.Recorder"
}
pub enum SingleInstanceError { AlreadyRunning, Win32(u32) }

pub struct RestartPolicy {
    pub window: Duration,         // 10 min
    pub max_in_window: u32,       // service.restart_max_per_10min
    pub base_backoff: Duration,   // 500 ms
    pub cap_backoff: Duration,    // service.restart_backoff_cap_ms
    pub stable_after: Duration,   // 10 min of healthy run resets the counter
}

pub struct WorkerSupervisor {
    job: OwnedHandle,             // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    state: WorkerState,
    policy: RestartPolicy,
    heartbeat_timeout: Duration,  // service.worker_heartbeat_timeout_ms
    restarts: VecDeque<Instant>,
}
pub enum WorkerState {
    NotRunning,
    Starting { since: Instant },
    Ready { pid: u32, since: Instant },
    Stopping { deadline: Instant },
    Degraded { reason: String },
}

/// Written as JSON to %LOCALAPPDATA%\ClipCheck\crashes\<ulid>.json
pub struct CrashRecord {
    pub id: String, pub at_unix_ms: i64, pub exit_code: Option<i32>, pub reason: String,
    pub worker_pid: u32, pub uptime_ms: u64, pub minidump: Option<String>, pub app_version: String,
}

pub struct Notifier { /* AUMID, toast builder, fullscreen check, rate limiter */ }
impl Notifier {
    pub fn notify(&self, kind: NoticeKind, title: &str, body: &str, actions: &[NoticeAction]) -> NoticeOutcome;
}
pub enum NoticeKind { Info, Warning, Fatal }
pub enum NoticeAction { Open { route: String }, ShowInFolder { clip_id: String } }   // resolved by daemon, not by path
pub enum NoticeOutcome { Shown, SuppressedFullscreen, RateLimited, Disabled }

pub struct TrayHandle { /* hidden HWND, NOTIFYICONDATA, menu builder */ }
impl TrayHandle {
    pub fn create(actions: Sender<TrayAction>) -> Result<Self, TrayError>;
    pub fn set_status(&self, s: &StatusDto);            // icon + tooltip, debounced 500 ms
}
pub enum TrayAction {
    OpenGallery, OpenLive, OpenSettings, SaveClip(u32), Split, ToggleRecording,
    ToggleMic, TogglePause, ExportDiagnostics, OpenLogs, CheckUpdates, RestartWorker, Quit,
}

pub struct AutostartManager;
impl AutostartManager {
    pub fn state(exe: &Path) -> AutostartState;                             // Run value + StartupApproved
    pub fn set(enabled: bool, exe: &Path) -> Result<(), AutostartError>;    // HKCU Run value
    pub fn reconcile(want: bool, exe: &Path) -> Result<(), AutostartError>; // §5.8
}
```

### 4.6 Config structs owned by this feature (in `crates/core`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub autostart: bool,
    pub worker_heartbeat_timeout_ms: u32,
    pub restart_max_per_10min: u32,
    pub restart_backoff_cap_ms: u32,
    pub pause_on_session_lock: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsConfig {
    pub clip_saved: bool,
    pub recording_state: bool,
    pub warnings: bool,
    pub suppress_when_fullscreen: bool,
    pub sound_on_clip: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig { pub level: LogLevel, pub retention_days: u32, pub max_total_mb: u32 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsConfig { pub include_window_titles: bool, pub include_minidumps: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatesConfig { pub check: UpdateCheck, pub channel: UpdateChannel, pub interval_hours: u32 }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum UpdateCheck { Never, Notify }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum UpdateChannel { Stable, Beta }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum LogLevel { Error, Warn, Info, Debug, Trace }
```

---

## 5. Algorithms and state machines

### 5.1 Daemon startup (`clipcheckd.exe [--background] [--worker ...]`)
```
main(args):
  if args contains --worker -> worker::run(args); return          // separate path, see 5.5
  logging::init(%LOCALAPPDATA%\ClipCheck\logs, level from config or "info")
  h = SingleInstance::acquire("Local\ClipCheck.Recorder")
  if h == AlreadyRunning:
      client = PipeClient::connect(timeout 2 s, ServerCheck{installed exe})
      if ok:  client.hello(); client.call(Activate{route: args.route}); exit 0
      else:   log "instance present but pipe unreachable"; exit 1    // Edge E3
  cfg = config::load_or_default(%APPDATA%\ClipCheck\config.toml)     // on error: defaults + ConfigIssues
  AutostartManager::reconcile(cfg.service.autostart, exe)            // self-heal stale path, 5.8
  server = PipeServer::start(...)?          // FIRST_PIPE_INSTANCE failure -> exit 2 "owned by another user"
  tray = TrayHandle::create(...)?
  supervisor.start()                        // worker is not spawned until something is armed
  if args.route and not --background: send Activate to self (opens UI)
  state = Idle; run event loop until ShutdownToken is set
  shutdown_sequence()                       // 5.4
```
The encoder probe (owned by encode doc) runs 5 s after logon when started with `--background`.

### 5.2 Daemon top-level state machine
```
            +----------+  config + IPC up  +------+
 start ---->| Starting |----------------->| Idle |<--------------------------+
            +----------+                   +--+---+                           |
                                              | game detected                 |
                                              v                               |
                                          +--------+  start session /         |
                                          | Armed  |  buffer on               |
                                          +---+----+----------+               |
                                              | worker Ready |                |
                                              v              v                |
                                       +-----------+   +------------+          |
                                       | Recording |<->| BufferOnly |          |
                                       +-----+-----+   +------+-----+          |
                                             | stop/quit      | game lost      |
                                             v                v                |
                                       +-----------+  done  +-----------------+ |
                                       | Stopping  |------->| Idle or Armed   |-+
                                       +-----------+        +-----------------+

  Any state --(restart budget exhausted)--> Degraded --(user RestartWorker)--> Starting
  Any state --(detection paused)--> Paused (grey bars; worker stops after finalize)
  Any state --(Shutdown)--> ShuttingDown (5.4)
```
The daemon owns the top-level state. The worker owns session and buffer sub-states (documented in the session doc). The daemon derives `RecState` from worker events plus its own detection and pause state.

### 5.3 Pipe server accept and connection loop
```
accept_loop (one thread):
  create instance I (PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED, byte mode, reject remote,
                     nMaxInstances = MAX_CLIENTS + 1, SD = user-only DACL, 64 KiB buffers)
  ConnectNamedPipe(I) overlapped; wait on {connect event, shutdown event}
  if shutdown: close I; return
  if active_clients >= MAX_CLIENTS: disconnect I (log once per minute)
  spawn connection thread(I) and loop (a fresh instance is created before waiting again)

connection(I):
  peer = verifier.verify(I)                // 5.6; on Err: disconnect, log at warn (rate-limited)
  deadline = now + HANDSHAKE_TIMEOUT
  first = read_frame(I) until deadline
  if first.body is not Hello  -> Error{Unauthorized}, disconnect
  if first.v != 1             -> Error{ProtoMismatch}, disconnect
  proto = min(PROTO_VERSION, client.proto_max)
  if proto < max(PROTO_MIN_SUPPORTED, client.proto_min) -> Error{ProtoMismatch}, disconnect
  write HelloAck{proto, daemon_version, daemon_pid, features}
  loop:
     frame = read_frame(I)                 // shutdown cancels blocking IO (CancelSynchronousIo)
     if frame.v != proto                   -> Error{ProtoMismatch} (connection stays open)
     decode body:
        unknown "type" tag                  -> Error{UnknownType} with request id, connection stays open
        Subscribe   -> handler.on_subscribe; record topics; reply Subscribed
        other       -> reply = handler.handle(peer, frame); write reply
  on EOF or IO error: drop subscriptions, close instance
```
Replies and event pushes share one write lock per connection, so frames never interleave.

### 5.4 Shutdown sequence
```
Quit requested (tray, Shutdown{Graceful}, WM_QUERYENDSESSION):
  if recording and mode != Force:
      tray: ask user (default Cancel); IPC: reply Error{Busy}; WM_QUERYENDSESSION: block
  publish ShuttingDown{reason}
  ShutdownToken.trigger(); stop accepting new pipe connections
  if worker alive:
      send worker Stop{finalize: true} over the worker pipe
      wait up to 8 s for worker exit
      if still alive: TerminateJobObject(job, 0)   // parts recover on next start (crash-safe format)
  close client connections (pending requests get Error{ShuttingDown}); wait up to 2 s
  remove tray icon (NIM_DELETE); flush logs; exit 0
```
`Shutdown{Update}` behaves like Graceful, but returns `Busy` while a recording is active (the update then waits; see E9).

### 5.5 Worker supervisor and crash watchdog
```
supervisor loop (parent thread), triggered by demand (armed or buffer on) or RestartWorker:
  if want_worker and state == NotRunning:
     hb = anonymous pipe pair (child inherits write end; handle passed as --hb)
     child = CreateProcessW("clipcheckd.exe --worker --hb <h> --gen <n>",
                            CREATE_SUSPENDED | CREATE_NO_WINDOW)
     AssignProcessToJobObject(job, child); ResumeThread(child)
     state = Starting{since = now}
  Starting -> Ready when the worker sends WorkerHello on the worker pipe (5 s timeout, else kill and count as crash)

  wait on: worker process handle | heartbeat read (expected every 1000 ms) | shutdown | command queue
  no heartbeat for > heartbeat_timeout -> TerminateProcess(child, 1), reason = "hang"
  on child exit:
     code = GetExitCodeProcess
     write CrashRecord{exit_code, reason, uptime, minidump path if the worker produced one}
     if uptime >= stable_after: restarts.clear()
     restarts.retain(|t| now - t < window)
     if restarts.len() >= max_in_window:
         state = Degraded{reason}; publish Degraded; Fatal toast; stop auto-restart
     restarts.push(now)
     publish WorkerRestarted{attempt, reason, exit_code}
     sleep backoff = min(base * 2^(restarts.len()-1), cap)   (interruptible by shutdown)
     state = NotRunning; loop while still wanted
```

Worker-side containment:
- `SetUnhandledExceptionFilter` in the worker writes a minidump (`MiniDumpWriteDump`, normal plus thread info, no full memory) to `%LOCALAPPDATA%\ClipCheck\crashes\<ulid>.dmp`, then exits with code `0xC0DE0001`.
- Panics: release builds use `panic = "unwind"`. The worker wraps each command in `catch_unwind`. A panic in a non-capture subsystem becomes a `Warning`. A panic in capture or encode is fatal and exits the worker.

Recovery after any worker death:
1. Scan the session directory for `*.partial` files (fragmented MP4 or MKV, crash-safe per brief).
2. For each one, the session or library owner rebuilds the index (remux with stream copy, or header repair), marks the row `recovered = 1`, and publishes `SessionPartFinished{recovered: true}`.
3. Replay buffer ring: segment files are self-describing. The index is rebuilt by scanning on worker start (target under 50 ms for 300 segments).

### 5.6 Peer verification and daemon verification
Server (daemon), per connection:
```
ImpersonateNamedPipeClient(pipe)             // binds to the real client token, avoids PID-reuse races
OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &tok)
GetTokenInformation(tok, TokenUser) -> client_sid
EqualSid(client_sid, daemon_user_sid)? else AuthError::DifferentUser
GetTokenInformation(tok, TokenElevation) -> elevated flag (logged, not a rejection in v1, Q9)
RevertToSelf()
```
Client (UI), before trusting the pipe:
```
GetNamedPipeServerProcessId(pipe) -> pid
QueryFullProcessImageNameW(pid) -> path
path must equal ServerCheck.expected_image (install dir clipcheckd.exe), case-insensitive
otherwise IpcError::UntrustedServer
```

### 5.7 Single instance and activation
```
Daemon start:
  CreateMutexW("Local\ClipCheck.Recorder") -> ERROR_ALREADY_EXISTS => second instance
  second instance: connect pipe (2 s); send Activate{route}; exit 0
UI start (clipcheck.exe):
  tauri-plugin-single-instance keyed on identifier "dev.clipcheck.ui"
  second launch forwards its argv (route) to the first window, then exits 0
  if the recorder pipe is absent: start clipcheckd --background, retry connect with
  250 ms, 500 ms, 1 s, 2 s backoff (4 s total)
```

### 5.8 Autostart reconcile
```
exe = canonical path of current_exe()
run_value = HKCU\...\Run["ClipCheck"]
reconcile(want):
  if want:
      desired = "\"<exe>\" --background"
      if run_value != desired: write desired          // covers a moved or updated install dir
  else:
      delete run_value if present
state():
  if run_value missing                                  -> Disabled
  else if StartupApproved\Run["ClipCheck"] first byte = 0x03 -> DisabledByWindows
  else if run_value path != exe                         -> PathStale
  else                                                  -> Enabled
```
The Run value is authoritative for what Windows launches. `service.autostart` is the stored preference, reconciled on every daemon start.

### 5.9 Notification pipeline
```
notify(kind, title, body, actions):
  if the category toggle is off and kind != Fatal    -> Disabled
  if kind != Fatal and suppress_when_fullscreen and SHQueryUserNotificationState() in {BUSY, D3D_FULL_SCREEN, PRESENTATION}
                                                      -> SuppressedFullscreen
  if rate_limiter.allow(code, 60 s) == false          -> RateLimited
  build toast XML (AUMID "ClipCheck.Recorder"); action URIs are clipcheck://<route> only
  ToastNotificationManager.CreateToastNotifierWithId(AUMID).Show(toast)
  if sound_on_clip and kind is clip-saved: PlaySoundW with the system alias SystemAsterisk
  -> Shown
```
Action URIs carry only a route (`clipcheck://clip/<ULID>`, `clipcheck://gallery`). The UI validates the ULID before any lookup.

### 5.10 Diagnostics bundle
```
ExportDiagnostics (daemon):
  stage = %LOCALAPPDATA%\ClipCheck\diagnostics\<ulid>\
  collect:
    app.txt         app versions, proto version, uptime, last 200 state events
    system.txt      Windows build, CPU, RAM, GPU adapters and driver versions (DXGI), HAGS flag
    encoders.json   encoder probe cache (names, pass or fail, errors)
    config.toml     redacted copy (absolute paths replaced by %USERPROFILE% / %LOCALAPPDATA% tokens)
    logs/           last 7 days of clipcheckd and worker logs (window titles removed unless opted in)
    crashes/        CrashRecords from the last 30 days; .dmp only if diagnostics.include_minidumps
    library.txt     schema version, row counts, free space on the recordings root (no file names)
  zip stage -> <stage>.zip (deflate); delete stage directory
  reply DiagnosticsReady{path, bytes}
UI: "Save diagnostics bundle" dialog; copies the zip to the chosen path; reveals it in Explorer
```
The bundle never leaves the machine automatically. Nothing is uploaded.

### 5.11 UI startup and connection state machine
```
Launching -> TryConnect --ok--> Handshake --> Connected
                |                  |
                | no pipe          | ProtoMismatch    -> Incompatible  (/error/incompatible)
                v                  | UntrustedServer  -> Blocked       (security warning, no commands)
            StartDaemon (clipcheckd --background) -> TryConnect (backoff 250..2000 ms, 4 s total)
                | still failing -> Unavailable (/error/recorder-unavailable, [Start] button)

Connected --pipe EOF--> Reconnecting (backoff 0.5 s .. 8 s) --> Handshake --> GetStatus --> Subscribe(state, clips, health, warnings)
```
After any reconnect the UI calls `get_status` before trusting events, because events may have been missed. Gaps in `seq` also trigger a status refresh and a clip-list refresh.

### 5.12 Update flow (opt-in, §3.6.5)
```
Triggers:
  updates.check == notify AND (now - last_check) >= interval (checked at UI start, daemon start, then every interval +/- 10 % jitter)
  OR the user clicks "Check for updates..." (always allowed, still a network call)

fetch latest.json (HTTPS, TLS verify, 10 s timeout, no cookies)
if newer than installed:
  Info toast "Update <ver> available" -> click opens /settings/updates
user clicks "Download and install" in UI:
  download installer to %LOCALAPPDATA%\ClipCheck\updates\ (progress shown)
  verify Ed25519 signature from the updater manifest      -> fail: delete file, show error
  verify Authenticode (WinVerifyTrust) and publisher name -> fail: delete file, show error
  if recording: stop; show "Stop recording to update"     (Shutdown{Update} returns busy)
  Shutdown{Update} -> wait up to 10 s for clean exit
  run installer silently (/S, per-user)
  installer relaunches clipcheck.exe
```

---

## 6. Config keys owned by this feature

All keys live in `%APPDATA%\ClipCheck\config.toml`. They are hot-reloadable through the file watch and `reload_settings`.

| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `service.autostart` | bool | `false` | — | Preference; the Run value is reconciled on start (§5.8) |
| `service.worker_heartbeat_timeout_ms` | u32 | `10000` | 3000–60000 | Hang detection |
| `service.restart_max_per_10min` | u32 | `5` | 1–20 | Crash budget before Degraded |
| `service.restart_backoff_cap_ms` | u32 | `30000` | 1000–300000 | Backoff upper bound |
| `service.pause_on_session_lock` | bool | `true` | — | Pause detection while Windows is locked (Q10) |
| `tray.left_click_action` | enum | `"gallery"` | `gallery`, `live`, `none` | — |
| `notifications.clip_saved` | bool | `true` | — | Toast on clip save |
| `notifications.recording_state` | bool | `true` | — | Session start, stop and split toasts |
| `notifications.warnings` | bool | `true` | — | Non-fatal warnings (rate-limited) |
| `notifications.suppress_when_fullscreen` | bool | `true` | — | Fatal toasts are never suppressed |
| `notifications.sound_on_clip` | bool | `false` | — | System sound on clip save |
| `logging.level` | enum | `"info"` | `error`, `warn`, `info`, `debug`, `trace` | `CLIPCHECK_LOG` env var overrides |
| `logging.retention_days` | u32 | `14` | 1–90 | Old logs deleted at start |
| `logging.max_total_mb` | u32 | `200` | 20–2000 | Oldest-first pruning |
| `diagnostics.include_window_titles` | bool | `false` | — | Window titles in logs and bundle |
| `diagnostics.include_minidumps` | bool | `false` | — | Minidumps in bundle |
| `updates.check` | enum | `"never"` | `never`, `notify` | Opt-in. `never` means zero background network |
| `updates.channel` | enum | `"stable"` | `stable`, `beta` | Selects manifest URL |
| `updates.interval_hours` | u32 | `12` | 1–168 | Used only when `check = "notify"` |

Non-config state (not user-editable): `%LOCALAPPDATA%\ClipCheck\state.json` holds `last_update_check_unix_ms`, `first_run_done`, `last_clean_shutdown`, tray GUID, and FFmpeg hash cache. Written atomically (temp file, then rename).

Dev-only override (debug builds and `--features test-pipe`): `CLIPCHECK_PIPE_NAME` replaces the pipe name so tests never collide with a running daemon.

---

## 7. IPC messages produced or consumed

### 7.1 Framing
- Transport: named pipe `\\.\pipe\clipcheck-ipc`, `PIPE_TYPE_BYTE`, overlapped I/O.
- Frame: a 4-byte unsigned little-endian length `N`, then `N` bytes of UTF-8 JSON (no BOM). `N` must be `<= 1 MiB`. For larger frames, the reader replies `Error{FrameTooLarge}` if it can still write, then disconnects.
- Each request is one frame and each reply is one frame. Events are unsolicited frames.
- Requests and replies are correlated by `id` (ULID). Replies echo the request `id`. Events have `seq` and no `id`.

### 7.2 Handshake
The client sends `hello` first, with `v: 1`. The server answers `hello_ack` or `error`. After that, every frame must carry `v` equal to the negotiated `proto`.

```json
{"v":1,"id":"01JDK8Z4M2W7Q0R5TXV3Y9A1BC","body":{"type":"hello","data":{"proto_min":1,"proto_max":1,"client":"clipcheck-ui","client_version":"0.1.0"}}}
```
```json
{"v":1,"id":"01JDK8Z4M2W7Q0R5TXV3Y9A1BC","body":{"type":"hello_ack","data":{"proto":1,"daemon_version":"0.1.0","daemon_pid":4120,"features":["clip_save","split","levels","diagnostics"]}}}
```
Protocol mismatch (a client that only supports v2 talking to a v1 daemon):
```json
{"v":1,"id":"01JDK8Z9Q1T5...","body":{"type":"error","data":{"code":"proto_mismatch","message":"server supports 1..=1","retryable":false}}}
```

### 7.3 Commands and replies

| Command (`type`) | Data | Reply | Notes |
|---|---|---|---|
| `ping` | — | `pong{unix_ms}` | Liveness |
| `get_status` | — | `status{...}` | Full snapshot, used after reconnect |
| `subscribe` | `{topics}` | `subscribed{topics}` | Enables pushes for those topics |
| `unsubscribe` | `{topics}` | `unsubscribed{topics}` | — |
| `set_recording` | `{enabled}` | `ok`, or `error{not_recording / invalid_state}` | Start or stop the full session |
| `split` | — | `ok`, or `error{not_recording}` | Split at the next keyframe |
| `set_recorder_enabled` | `{enabled}` | `ok`, or `error{invalid_state}` | Off stops and saves an active session, detaches the game, and stops auto-start. Clip and session commands then fail with `recorder is turned off`. On resumes recording a game that is still detected. Persisted in `recorder-state.json` next to the config. Status field `recorder_enabled` (always sent; missing means true) |
| `set_hotkeys_suspended` | `{suspended: bool}` | `ok` | `true` makes the daemon ignore every global hotkey chord, so the Settings UI can capture a new chord without firing the old ones. The suspension lasts 60 s and the UI renews it about every 20 s (`true` again). It ends early on `false`, when the client that suspended disconnects, on a config reload or patch, and on a hotkey service restart. Status field `hotkeys_suspended` (omitted when false; missing means false) |
| `save_clip` | `{seconds: 1..=3600, game_id?}` | `clip_queued{clip_id}`, then event `clip_saved` | Reply is immediate, work is async |
| `set_mic_muted` | `{muted}` | `ok` | Emits `mic_mute_changed` |
| `set_detection_paused` | `{paused, until_unix_ms?}` | `ok` | `until` must be in the future and at most 24 h away |
| `reload_settings` | — | `ok` | Emits `settings_reloaded` with any issues |
| `set_autostart` | `{enabled}` | `ok`, or `error{internal}` | Writes the HKCU Run value |
| `export_diagnostics` | — | `diagnostics_ready{path, bytes}` | Path is inside the internal staging directory |
| `activate` | `{route?}` | `ok` | Route must match the allowlist in §2.7 |
| `restart_worker` | — | `ok`, or `error{invalid_state}` | Valid in Degraded, or on explicit user request |
| `shutdown` | `{mode}` | `ok` (then close), or `error{busy}` | `graceful` returns `busy` while recording, unless `force` |

Examples:
```json
{"v":1,"id":"01JDK9A1X0Q3F7M2N8P4R6S5T1","body":{"type":"get_status"}}
```
```json
{"v":1,"id":"01JDK9A1X0Q3F7M2N8P4R6S5T1","body":{"type":"status","data":{"state":"recording","game":{"game_id":"valorant.exe","exe":"VALORANT-Win64-Shipping.exe","pid":18844,"title":null},"session":{"session_id":"01JDK7W2Q9Z6H4T1V8B3N5M7KC","part":2,"started_unix_ms":1791510843000,"path":"C:\\Users\\mitch\\Videos\\ClipCheck\\Valorant\\Valorant 2026-10-09 21-14-03 session-part2.mkv"},"buffer_seconds":300,"buffer_bytes":412876800,"encoder":"nvenc_h264","codec":"h264","fps_target":60,"bitrate_kbps":12000,"mic_muted":false,"detection_paused":false,"autostart":"enabled","worker_pid":20312,"worker_restarts_10min":0,"uptime_s":5421,"degraded_reason":null}}}
```

Save a clip:
```json
{"v":1,"id":"01JDK9B7H3K8Z2Q5W1R9T4X6YM","body":{"type":"save_clip","data":{"seconds":30,"game_id":"valorant.exe"}}}
```
```json
{"v":1,"id":"01JDK9B7H3K8Z2Q5W1R9T4X6YM","body":{"type":"clip_queued","data":{"clip_id":"01JDK9B7KCV4Y1N8T6P2Q9R3WE"}}}
```

Error with retry hint:
```json
{"v":1,"id":"01JDK9C0F6N2...","body":{"type":"error","data":{"code":"not_recording","message":"no session or replay buffer is active","retryable":true}}}
```

Subscribe:
```json
{"v":1,"id":"01JDK9D2G7Q1...","body":{"type":"subscribe","data":{"topics":["state","clips","health","warnings"]}}}
```

### 7.4 Events (daemon to client, after subscribe)
Topics: `state` (StateChanged, GameDetected, GameLost, Degraded, DetectionPauseChanged, MicMuteChanged), `clips` (ClipSaved), `sessions` (SessionPartStarted, SessionPartFinished), `health` (CaptureHealth, EncoderSelected), `levels` (AudioLevels, 10 Hz), `warnings` (Warning, WorkerRestarted), `settings` (SettingsReloaded). `ShuttingDown` is always delivered, regardless of subscriptions.

```json
{"v":1,"seq":1042,"body":{"type":"clip_saved","data":{"clip_id":"01JDK9B7KCV4Y1N8T6P2Q9R3WE","path":"C:\\Users\\mitch\\Videos\\ClipCheck\\Valorant\\Valorant 2026-10-09 21-14-03 clip-30s.mp4","duration_ms":30012,"bytes":48211234}}}
```
```json
{"v":1,"seq":1043,"body":{"type":"state_changed","data":{"state":"recording","game":{"game_id":"valorant.exe","exe":"VALORANT-Win64-Shipping.exe","pid":18844,"title":null}}}}
```
```json
{"v":1,"seq":1047,"body":{"type":"worker_restarted","data":{"attempt":1,"reason":"access violation in encode","exit_code":-1073741819}}}
```
```json
{"v":1,"seq":1050,"body":{"type":"settings_reloaded","data":{"ok":false,"errors":[{"key":"logging.level","message":"expected one of error|warn|info|debug|trace, got \"verbose\""}]}}}
```

### 7.5 Producers and consumers
| Message | Produced by | Consumed by |
|---|---|---|
| Commands | UI (Tauri commands), second instance (`activate`) | Daemon IPC server; capture-related commands forwarded to the worker |
| Replies | Daemon | UI, second instance |
| State and health events | Daemon (from worker events, detection and pause state) | UI Live screen, tray refresh |
| Clip and session events | Worker, relayed by daemon | UI Gallery, Notifier |
| Diagnostics results | Daemon | UI (save dialog) |

Internal worker channel (not user-facing IPC): pipe `\\.\pipe\clipcheck-worker-<daemon-pid>-<gen>` with the same user-only ACL. It uses the same frame codec with worker-only `WorkerCommand` and `WorkerEvent` enums, documented in the session doc. Heartbeats travel on the inherited anonymous pipe, not on this pipe, so a blocked worker-pipe handler cannot mask a hang.

---

## 8. Edge cases & failure modes

| # | Case | Expected behavior |
|---|---|---|
| E1 | Daemon crashes while recording | The worker dies with the job object. Open session parts are crash-safe (loss under 2 s). On next start, parts are recovered and marked; the UI shows a warning; the tray starts Idle |
| E2 | Worker crashes repeatedly (bad driver) | Restart with backoff 0.5, 1, 2, 4, 8 s. After 5 restarts within 10 min the daemon enters Degraded, shows a Fatal toast and stops restarting until the user clicks "Restart recorder" |
| E3 | A second `clipcheckd` starts while the first is hung (mutex held, pipe unresponsive) | The second instance waits up to 2 s for the pipe, then exits 1 with a log line. The daemon is never killed automatically. The UI offers "Restart recorder" only when the daemon process is confirmed gone |
| E4 | Handshake never completes (malicious or stuck client) | Connection closed after 3 s. Other clients are still accepted, up to MAX_CLIENTS |
| E5 | Client sends a frame over 1 MiB | `frame_too_large` error if possible, then disconnect. No allocation beyond the cap |
| E6 | Client sends an unknown command type from a newer minor version | `unknown_type` error with the request id; connection stays open |
| E7 | Protocol major mismatch after an update that did not replace the UI | UI shows "Recorder update required". The daemon keeps running. Restart does not install anything; the installer must be run |
| E8 | Install directory moved or updated, autostart still enabled | `PathStale` detected at start. Run value rewritten to the current exe. No duplicate entries |
| E9 | Update requested while recording | `shutdown{update}` returns `busy`. The UI tells the user to stop recording. The installer does not start |
| E10 | Explorer crashes or restarts | `TaskbarCreated` re-adds the tray icon. Icon state is restored from `StatusDto` |
| E11 | Fullscreen game is foreground while a fatal toast is pending | Fatal toast shows when the state returns to accepting notifications, or after 30 s, whichever comes first |
| E12 | Toast AUMID not registered (dev build run from `target\`) | Registration at daemon start under HKCU. If registration fails, notifications fall back to a tray balloon (`NIF_INFO`) |
| E13 | Config file corrupt or invalid TOML | Defaults are used for the broken section. The file is renamed to `config.toml.bad-<ts>`. `settings_reloaded{ok:false}` carries the parse position. The daemon never refuses to start because of config |
| E14 | FFmpeg DLL missing or hash mismatch | Encoder list empty, Degraded with reason, diagnostics points to the bad file. No partial loading |
| E15 | Two Windows users logged in (fast user switching) | The second user's daemon cannot create the first-instance pipe, logs, and exits 2. The mutex is per session, so both can start. The UI shows the recorder as "running for another user" (Q2) |
| E16 | Sleep or resume during a recording | Worker receives the resume notification and re-creates the capture device. If that takes over 5 s it reports `black_frames_active` and keeps retrying |
| E17 | Logon autostart races with a manual start | The mutex decides. The loser activates the existing instance and exits 0 |
| E18 | Recordings drive full | Worker emits a Warning and finalizes the current part cleanly. The replay buffer keeps working if it is on another drive. Fatal toast if no part can be written |
| E19 | Wall clock jumps (NTP correction) | Timestamps are stored as unix ms, but durations are measured with a monotonic clock. A jump over 2 s is logged as a warning |
| E20 | Diagnostics requested while the worker is hung | The daemon collects what it can. Worker logs are read from disk. The bundle notes that the worker did not respond |
| E21 | Update signature valid but Authenticode publisher differs | Installer refused and deleted. Error explains the publisher mismatch |
| E22 | User deletes `state.json` | Recreated with defaults. Tray GUID regenerated (harmless). FFmpeg hash cache rebuilt |
| E23 | `--background` start with no config directory yet | Directories created with a user-only ACL. Defaults written on first save |
| E24 | Explorer or AUMID unavailable at logon (toasts fail) | Toast errors are logged and treated as `Disabled` for that event; tray icon still reflects state |
| E25 | Worker exits cleanly with code 0 while a session is still armed | Treated as a crash for restart policy (`reason = "unexpected clean exit"`), so no silent loss of armed state |

---

## 9. Performance & resource budget, verification plan

### 9.1 Budgets
| Scenario | Metric | Budget | Stretch target |
|---|---|---|---|
| Daemon idle, no game (no worker running) | Working set (RSS) | < 15 MB | 8–10 MB |
| Daemon idle | Private bytes | < 12 MB | 6–8 MB |
| Daemon idle | CPU, one core | < 0.3 % | < 0.1 % |
| Daemon idle | Threads | <= 12 | — |
| Daemon idle | Handles | <= 300 | — |
| Daemon idle | Disk writes | Only log flushes | — |
| UI, window open, idle, recorder running | Process-tree working set (UI plus WebView2 children) | < 120 MB | < 100 MB |
| UI closed | ClipCheck-owned processes | 0 `clipcheck.exe`, no WebView2 children | — |
| IPC | Round trip of `get_status` (local, p99) | < 5 ms | < 1 ms |
| IPC | Event fan-out, 1 subscriber, 100 events/s | < 1 % CPU of daemon | — |
| Worker cold start (game detected to Ready) | Time | < 1.5 s | < 0.8 s |
| Worker crash to restarted Ready | Time, including first backoff | < 2.5 s | — |
| Recording (owned by capture and encode docs) | Worker CPU, one core, HW encode | < 3 % | — |
| Installer | Size | < 40 MB | < 30 MB |
| `clipcheckd.exe` release | Size (stripped) | < 4 MB | < 3 MB |
| `clipcheck.exe` release | Size (excludes WebView2 runtime) | < 12 MB | < 9 MB |

Worker memory while recording is set by the capture, encode and buffer docs. This doc requires only that the parent stays within the idle budget while a worker runs.

### 9.2 Measurement harness
- `tools/perf/measure.ps1 -ProcessId <pid> -Seconds 300 -Out <csv>`: samples every second with `Get-Process` (WorkingSet64, PrivateMemorySize64, TotalProcessorTime delta). For the UI, the tree sum uses `Win32_Process.ParentProcessId` to include WebView2 children.
- Scenarios in `tools/perf/scenarios/`:
  1. `idle-daemon`: start the daemon with `--background`, no game, 10 min soak.
  2. `ui-open`: UI on Gallery with a 500-clip library, 5 min idle.
  3. `armed-recording`: test fixture `tools/fixtures/fake-game.exe` (a windowed D3D11 app drawing a moving pattern), 10 min recording.
  4. `ipc-stress`: 10 000 `get_status` calls plus `subscribe(levels)`, 60 s.
  5. `crash-loop`: fault injection kills the worker every 2 s. Expect Degraded after 5 restarts and stable daemon RSS.
- Results are compared against thresholds in `tools/perf/budgets.toml`. CI fails on any violation.
- PR gate: 2-minute variants on hosted `windows-2025` runners, with a 1.5x tolerance because hosted runners are noisy. Nightly: full 30-minute soak on a self-hosted Windows machine with strict thresholds.
- Investigation tooling: Windows Performance Recorder profiles (CPU sampling, heap) with WPA. The daemon uses the system heap and no custom allocator.

### 9.3 Egress verification (no telemetry)
- Scenario `egress-idle`: UI and daemon run with `updates.check = "never"` for 30 min. Sample `Get-NetTCPConnection` every 5 s, plus an ETW trace of `Microsoft-Windows-TCPIP`. Expected: zero remote connections owned by `clipcheckd.exe` or `clipcheck.exe`. WebView2 connections only to localhost IPC.
- Scenario `egress-notify`: `updates.check = "notify"`. Expected: exactly one GET per interval to the configured manifest host, and nothing else.
- The test fails on any unexpected destination host.

---

## 10. Test plan

| Level | Test | Tooling |
|---|---|---|
| Unit | Frame codec: length prefix, max size, partial reads, zero-length frame rejected | `cargo test -p ipc` |
| Unit | Serde golden fixtures for every Command, Reply and Event in `crates/ipc/tests/fixtures/v1/*.json`: round-trip equality, unknown fields ignored, unknown `type` yields `UnknownType` | `cargo test -p ipc` |
| Unit | Property tests: random JSON never panics the decoder; random bytes produce errors, not UB | `proptest` |
| Unit | Protocol negotiation table (client 1..=1 vs server 1..=2, client 2..=3 vs server 1..=1, and so on) | table tests |
| Unit | RestartPolicy: backoff sequence, window expiry, stable reset | table tests with fake clock |
| Unit | Notifier: fullscreen suppression matrix, rate limiter | fake `SHQueryUserNotificationState` provider |
| Unit | Autostart reconcile: missing, stale path, disabled by Windows | registry tests under a test hive `HKCU\Software\ClipCheck.Test` |
| Unit | Config validation errors produce `ConfigIssue` entries with keys | `cargo test -p core` |
| Unit | Tray menu model matches `StatusDto` per state | snapshot tests on the menu model (not pixels) |
| Integration | Pipe server and client in-process: handshake, subscribe, 1000 events in order with `seq` | `cargo test -p ipc --features test-pipe` |
| Integration | Second daemon instance: mutex hit, `activate` forwarded, exit 0 | `apps/clipcheckd/tests/singleton.rs` |
| Integration | Pipe squatting: a second FIRST_PIPE_INSTANCE attempt fails, daemon exits 2 | integration |
| Integration | Oversize frame (2 MiB) closes the connection with `FrameTooLarge`; no allocation beyond the cap | integration |
| Integration | Fake server at a different image path: UI refuses with `UntrustedServer` | integration |
| Integration | FFmpeg manifest mismatch (one DLL corrupted in a copy) triggers Degraded and no encoder | integration |
| Security | ACL: a standard local user (created by a CI script on a self-hosted runner) connects to the pipe and gets `ERROR_ACCESS_DENIED` | self-hosted job |
| Security | Impersonation: a helper running as another SID is rejected with `DifferentUser` | self-hosted job |
| Watchdog | Worker `abort()` every 2 s: 5 restarts, then Degraded, Fatal toast, daemon alive | `fault-injection` feature |
| Watchdog | Worker hang (heartbeat thread blocked): killed after the heartbeat timeout | `fault-injection` feature |
| Watchdog | Parent killed: worker dies via KILL_ON_JOB_CLOSE, no orphan (checked with `Get-Process`) | integration |
| Shell | Explorer restart re-adds the tray icon (`taskkill /im explorer.exe`, verify icon) | manual checklist, scripted |
| Packaging | Silent install, uninstall and upgrade from the prior version in Windows Sandbox or a Hyper-V VM | `installer/tests/*.ps1` |
| Packaging | Signature check of every shipped PE and the installer with `signtool verify /pa` | CI |
| Privacy | Diagnostics bundle scan: no username, no window titles by default | integration |
| Privacy | Egress verification (§9.3) | self-hosted nightly |
| Manual | Toasts appear in Action Center; fullscreen suppression works with a fullscreen D3D game | manual checklist per release |
| Manual | SmartScreen reputation check on the release candidate | manual |

---

## 11. Implementation task breakdown

Ordered. Each task is roughly 0.5–2 days. Dependencies in parentheses.

| # | Task | Output | Est. |
|---|---|---|---|
| T1 | Cargo workspace skeleton, toolchain pin, lint config, CI (fmt, clippy `-D warnings`, test, cargo-deny) | Green CI on empty crates | 1 d |
| T2 | `crates/core`: paths, config schema, load and save with `toml_edit` (comment-preserving), validation to `ConfigIssue`, logging init | Config and logs | 1.5 d (T1) |
| T3 | `crates/ipc` protocol: `Frame`, enums, codec, golden fixtures, proptests | Protocol v1 frozen as fixtures | 1.5 d (T1) |
| T4 | `crates/ipc` pipe server: SDDL ACL, FIRST_PIPE_INSTANCE, accept loop, handshake, impersonation peer check, fan-out | Server | 2 d (T3) |
| T5 | `crates/ipc` client: connect, server image check, hello and negotiation, call with timeout, event receiver, reconnect helper | Client | 1.5 d (T4) |
| T6 | Daemon bootstrap: singleton mutex, activate forwarding, startup and shutdown sequence, ShutdownToken, exit codes | `clipcheckd` skeleton | 1 d (T5) |
| T7 | Tray: hidden window, `Shell_NotifyIconW`, icon states with debounce, menu from `StatusDto`, TaskbarCreated | Tray | 2 d (T6) |
| T8 | Worker supervisor: Job object, spawn, anonymous heartbeat pipe, RestartPolicy, CrashRecord, minidump filter | Watchdog | 2 d (T6) |
| T9 | Autostart manager (Run value and StartupApproved reconcile), `clipcheck://` registration | Autostart | 1 d (T2) |
| T10 | Notifier: AUMID, toast XML, fullscreen suppression, rate limiter, sound option | Notifications | 1 d (T7) |
| T11 | Diagnostics: staging, collectors, redaction, zip, `export_diagnostics` | Bundle | 1.5 d (T2, T8) |
| T12 | UI shell: Tauri v2 scaffold, capabilities, CSP, `ipc_client` Tauri commands, single-instance plugin, route parser, connection state machine (§5.11) | Shell | 2 d (T5) |
| T13 | UI screen shells: Welcome, Gallery skeleton, Live (subscriptions), Settings frame, Recorder unavailable | Screen skeletons | 2 d (T12) |
| T14 | `clip://` protocol handler with Range support and ULID-only resolution | Clip serving | 1 d (T12) |
| T15 | NSIS per-user installer: layout, shortcuts and AUMID, uninstall rules, silent mode | Installer | 2 d (T9, T13) |
| T16 | FFmpeg build script, manifest, licenses bundle, integrity check at worker start | FFmpeg packaging | 1.5 d (T15) |
| T17 | Signing pipeline (signtool, timestamps, verify gate) and Tauri updater (Ed25519 manifest, opt-in notify, verified install) | Signed releases and updates | 2 d (T15) |
| T18 | Perf harness, `budgets.toml`, PR gate and nightly soak, egress verification | Budget gates | 1.5 d (T7, T8) |
| T19 | `fault-injection` feature, watchdog and ACL integration tests on the self-hosted runner | Reliability tests | 1.5 d (T8, T4) |

Critical path: T1, T3, T4, T5, T6, T8, T15, T17.

### 11.1 Workspace and CI setup (T1 detail)

Root `Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/ipc", "crates/detect", "crates/capture", "crates/audio",
           "crates/encode", "crates/buffer", "crates/session", "crates/library",
           "crates/clipedit", "crates/hotkeys", "apps/clipcheckd", "apps/ui/src-tauri"]

[workspace.package]
edition = "2021"
rust-version = "1.85"
license = "GPL-3.0-or-later"

[workspace.dependencies]
windows = { version = "0.58", features = [
  "Win32_Foundation", "Win32_Security", "Win32_Security_Authorization", "Win32_System_Pipes",
  "Win32_System_Threading", "Win32_System_Registry", "Win32_System_JobObjects",
  "Win32_System_Diagnostics_Debug", "Win32_System_RemoteDesktop", "Win32_System_Power",
  "Win32_UI_Shell", "Win32_UI_WindowsAndMessaging", "Win32_Storage_FileSystem",
  "UI_Notifications", "Data_Xml_Dom"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml_edit = "0.22"
thiserror = "1"
tracing = "0.1"

[profile.release]
opt-level = 3
lto = "thin"            # fat in the dist profile; measured on the daemon crate
codegen-units = 1
strip = "debuginfo"     # symbols stay for minidump symbolication; PDBs archived per release
panic = "unwind"        # required by catch_unwind in the worker
debug = "line-tables-only"

[profile.dist]
inherits = "release"
lto = "fat"
strip = "symbols"       # PDB archived separately, published to the symbol store

[profile.dev.package."*"]
opt-level = 1           # faster dev builds for deps; the app crates stay at opt-level 0
```

Size notes:
- `clipcheckd` avoids tokio and async runtimes. It uses `std` threads and the `windows` crate directly. `rusqlite` (bundled) is linked into the worker and UI only, not the parent.
- `default-features = false` for `serde_json` is not possible (it needs `std`), so the size work goes into dropping unused features across crates.
- `opt-level = "s"` was evaluated for the parent only. It saved about 0.3 MB and cost measurable CPU in the frame codec. Decision: `opt-level = 3`, measured with `cargo bloat --release -p clipcheckd`. Reconsider if the parent exceeds the 4 MB budget.
- No `target-cpu=native` in distributed builds. `.cargo/config.toml` may set `target-cpu=x86-64-v2` for the x86-64 Windows target, which gives a Windows 10 2004+ compatible baseline.
- Use `cargo nextest` for test runs and `cargo deny` for license and advisory checks. Allowed licenses: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, MPL-2.0, and GPL-3.0-or-later for the project itself. Anything else needs an explicit exception.

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy"]
targets = ["x86_64-pc-windows-msvc"]
```
The toolchain is bumped deliberately, in its own PR.

CI (`.github/workflows/ci.yml`), per PR, on `windows-2025`:
| Job | Steps | Gate |
|---|---|---|
| `fmt` | `cargo fmt --all -- --check` | Required |
| `clippy` | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Required |
| `test` | `cargo nextest run --workspace`, then `cargo test --workspace --doc` | Required |
| `deny` | `cargo deny check` | Required |
| `ui` | `pnpm install --frozen-lockfile`, `pnpm check` (svelte-check), `pnpm test` (vitest), `pnpm lint` | Required |
| `build-release` | `cargo build --profile dist -p clipcheckd -p ui`; size check against budgets (`clipcheckd.exe` under 4 MB) | Required |
| `perf-smoke` | 2-minute idle and UI budget run (§9.2) | Required with 1.5x tolerance |
| `package` | NSIS build, `signtool verify` (unsigned locally, signed on tag only) | On tag only |

Caching: `Swatinem/rust-cache` keyed on `Cargo.lock` and toolchain. Release workflow (`release.yml`, on `v*` tags): sign binaries and installer with the protected environment, verify signatures, generate `THIRD-PARTY-NOTICES.txt` with `cargo-about`, produce `latest.json` with Ed25519 signature, publish SHA-256 sums and a provenance attestation.

---

## 12. Open questions

| # | Question | Proposed answer | Why it matters |
|---|---|---|---|
| Q1 | The brief says `clipcheckd` owns capture and encode. This doc adds an on-demand child `--worker` process. Acceptable? | Yes. The parent keeps idle RSS low, and a worker crash does not take down the tray. The brief's ownership still holds because the daemon starts and supervises the worker. If rejected, drop the supervisor and rely on in-process `catch_unwind`, which cannot survive driver or FFmpeg crashes | Changes the process model and budgets |
| Q2 | The brief fixes the pipe name, so with Fast User Switching only one user's recorder can own the pipe. Should the name include a per-user suffix? | Keep the fixed name for v1 (brief decision). A second user's daemon exits 2 with "Recorder is running for another Windows user" | Multi-user households |
| Q3 | Autostart via HKCU Run key versus Task Scheduler (delayed start, retries) | Run key: simpler, no admin, visible in Task Manager Startup tab. Deferred start is done inside the daemon (5 s) | Logon performance |
| Q4 | NSIS versus WiX MSI | NSIS per-user in v1. MSI only if managed deployment is needed | Installer maintenance cost |
| Q5 | Ship GPL FFmpeg (with x264 and x265) in the base installer, or split into an optional download? | Ship in the base installer. Estimated size fits the 40 MB budget. Split only if measured size exceeds budget | Installer size and licensing simplicity |
| Q6 | Code-signing certificate: Azure Trusted Signing eligibility for an individual developer, or an OV certificate on an HSM? Cost? | Decide before the first public release. Unsigned pre-release builds are for internal testing only | SmartScreen warnings |
| Q7 | Update endpoint domain | Placeholder used here. A real domain with HTTPS static hosting is required before `notify` can ship | Update feature cannot ship without it |
| Q8 | `clip://` is the logical scheme in these docs, but WebView2 serves it as `http://clip.localhost`. Accept this documented difference? | Yes. The URL form inside the webview is an implementation detail. The Rust side keeps the `clip` scheme name | Affects the library and UI contract text |
| Q9 | Elevated UI client: same SID but high integrity. Refuse it? | Accept with a log warning in v1, since refusing would break running the UI as admin for debugging. Revisit if a privilege boundary matters | Security policy |
| Q10 | While Windows is locked, keep recording (black frames only) or pause? | Pause by default (`pause_on_session_lock = true`) to avoid recording the lock screen and wasting power. Confirm with the capture doc | Privacy and battery |
| Q11 | Worker idle-exit grace is 30 s after the last armed state. Compatible with the session doc's split and stop timing? | Yes, provided the session doc confirms no keyframe split is pending when the worker exits | Session continuity |
| Q12 | Should `save_clip` be allowed while the worker is still starting and the buffer is not yet full? | Reject with `invalid_state`, with the seconds available in the message. The UI shows the buffer length | UX edge |
| Q13 | The brief's quality bar says "UI idle < 120 MB with window open". It is unclear whether WebView2 children count. This doc measures the process tree | Yes, measure the tree. If WebView2 alone exceeds the budget, reduce the UI DOM and disable background networking before changing the target | Budget credibility |
| Q14 | The brief says the service owns hotkeys. Hotkey registration (`RegisterHotKey`) requires a message loop, which the tray window already provides. Should hotkeys live in the parent or the worker? | Parent: hotkeys act on recorder state, and the parent stays alive when the worker is restarting, so hotkeys keep working during recovery. The hotkeys doc owns the registry | Hotkey availability during Degraded |

## Settings & status additions

The settings backend (doc 09) adds these messages. All new fields are `serde(default)`, so older clients keep working.

- `get_config` replies `config` with `config_version` and `path`.
- `patch_config { patch, dry_run }` deep-merges the patch (objects merge, arrays and scalars replace, `null` clears an optional field). It replies `config_applied { apply_class, restart_required, diagnostics }` after the file is saved and applied, `config_checked` for a passing dry run, or `config_rejected { diagnostics }` with nothing written. Unknown keys and type errors are `bad_request`.
- `reload_config` re-reads `config.toml` after a hand edit, with the same replies as `patch_config`.
- Event `config_changed { apply_class, restart_required }` (topic `settings`). `restart_required` means part of the change waits until the recorder is idle: no session is recording and no finished part is still being written. A game that is attached without a session does not hold the change. Pending changes apply when the session stops, when the recorder is turned off, and when the game closes. The apply then publishes `config_changed` with `restart_required: false`.
- Event `session_started { session }` (topic `sessions`). `session.started_unix_ms` is the start of the whole session and does not change on split. `session_part_finished` adds `started_unix_ms` and `parts_saved`.
- `status` adds `game_info { name, exe, hwnd, attached_since_unix_ms }`, `buffer_secs`, `settings_pending_restart`, and `server_unix_ms` (clients use it to correct clock skew). `session` adds `parts_saved`.
