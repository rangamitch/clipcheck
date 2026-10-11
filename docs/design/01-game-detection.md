# 01 — Game Detection (`crates/detect`)

Status: Draft · Date: 2026-10-09 · Depends on: `00-brief.md`
Owner crate: `crates/detect/` · Consumers: `crates/session/`, `crates/capture/` (via events), `crates/ipc/`, `apps/clipcheckd/`, `apps/ui/`

---

## 1. Goal & scope

### Goal
Decide, continuously and without user action, **which window (HWND/pid/exe) is a game that should be recorded right now**, and emit precise start / retarget / relaunch / stop events that the session state machine uses to start and stop capture. Also provide a manual "this window is a game" path for anything the heuristics miss.

### In scope (v1)
- Process and top-level window enumeration on Windows 10 2004+ / 11, x64.
- Heuristic scoring with a hard gate (geometry, GPU load, graphics modules, known-games DB, library match).
- Allowlist and blocklist of executables. Blocklisted executables are never recorded, even by manual add.
- Library scan for Steam, Epic, GOG and Xbox/Game Pass install folders, to map exe to `GameId` (store id) and to install directory.
- Launcher-vs-game process tree handling, including launcher stubs that spawn the real game.
- Multiple simultaneous candidate windows, deterministic selection.
- Game exit, relaunch within a grace window, and window replacement (same game, new HWND).
- Debouncing of start and stop.
- Manual add from the UI (writes an allow rule into `config.toml`).
- Anti-cheat-friendly observation: no DLL injection, no memory reads, no handle with write or VM access to game processes.

### Out of scope (v1)
- Exclusive-fullscreen detection as a hard fact (we infer it only when capture fails; see Section 5.6).
- Game hooking / injection (brief: v2 stretch).
- Ubisoft Connect, EA app, Battle.net, Riot library scans (see Open questions; manual add covers them).
- Cloud or online game database updates. The known-games DB ships inside the binary and changes only with app releases.
- Linux and macOS.
- Detecting non-game 3D content as "game" on purpose (e.g., 3D modelling apps). These fall out naturally via the scoring gate, and the user can block them.

### Non-goals
- Perfect recall of every game. The user's safety net is manual add and the allowlist.
- Recording audio of games. Audio is owned by `audio/`; detection only supplies PIDs for per-app loopback.

---

## 2. User-facing behavior (precise)

### 2.1 Modes (`detect.mode`)
| Mode | Behavior |
|---|---|
| `auto` (default) | Heuristics plus known DB plus library plus allowlist. Starts recording automatically when an eligible game is confirmed. |
| `allowlist_only` | Only executables present in `detect.allow` (or added manually) are eligible. Heuristics are computed for UI display but never start recording. |
| `off` | No enumeration beyond the cheap idle check; no events emitted. The tray indicates "Detection off". |

### 2.2 Start behavior
- A game starts recording when its candidate has been eligible and is the chosen best candidate for `detect.start_confirm_ms` (default 3 s) without interruption.
- The UI tray tooltip changes to "Recording: <Game Name>" when `GameStarted` is emitted, and the session doc decides what happens to the file.
- Allowlisted or manually added games skip the heuristic gate and use `start_confirm_ms = 0` unless their process was started within the last 5 s, in which case the normal confirm applies (avoids recording a launcher-era window).

### 2.3 Stop behavior
- A game stops only when its target process has exited, its target window has been destroyed, or the allow rule was removed/mode switched to `off`. A game that is minimized, alt-tabbed, or temporarily not fullscreen **does not stop**. Capture handles that with black frames (brief).
- **Process exit** stops the game at the first liveness check that sees it (within `LIVENESS_POLL_MS` = 250 ms). There is no grace: a dead process does not come back, so a relaunch after the process has exited starts a new recording (`GameStopped`, then `GameStarted` after the start debounce).
- **Window destroyed while the process still runs** (loading screens, fullscreen toggles, launcher hand-off) waits `WINDOW_GONE_GRACE_MS` = 1 s before `GameStopped{WindowDestroyed}`. The grace is capped by `detect.stop_grace_ms` when that is smaller. If a window of the same `GameId` (or same install directory) appears in that time, the session continues (`GameRelaunched`). If the window answers again (a failed probe or a flicker), the session continues silently.
- Latencies are from the timing constants and the loop rules, not a live measurement (see 5.20).

### 2.4 Candidate visibility
- The UI "Detection" panel lists current candidates (top 8 by score) with: name, exe, window title, score, verdict (`eligible`, `blocked: <reason>`, `not fullscreen`, `low GPU`, `launcher`, `pinned`).
- Blocked candidates show their reason. Blocked candidates are never offered an "Add as game" action, except for allowlist-eligible non-blocklisted candidates.

### 2.5 Manual add
- Action: "Add as game" on a candidate row, or "Add window…" which lists visible top-level windows.
- Effect: writes `[[detect.allow]]` into `config.toml` (via `toml_edit`, comments preserved), reloads config, and the candidate becomes `pinned`.
- Refused with an explanation if the exe is in the blocklist (built-in or `detect.block_extra`), is a launcher, or is a system process.
- Removing a manual game: "Remove from games" removes the matching allow rule. If the game is running, it stops per 2.3 (`ConfigRemoved`).

### 2.6 Multiple candidate windows
- If several eligible candidates exist, exactly one is the **target**: the highest score, ties broken by (1) foreground, (2) larger window area, (3) lower pid (stable).
- Other eligible candidates are listed as "also running" in the UI and are not recorded.
- Switching target requires the new candidate to beat the current target by ≥ 15 points for `start_confirm_ms` (hysteresis) to avoid flapping between a game and its overlay process.

### 2.7 Startup and shutdown
- On service start: a full scan runs immediately. If a game is already running, `GameStarted` is emitted with `reason = AlreadyRunning` after the normal confirm period.
- On service shutdown: any active game is stopped with `reason = Shutdown`.
- On sleep/resume (WM_POWERBROADCAST `PBT_APMRESUMEAUTOMATIC`): the detector resets Pending state and forces a scan; an Active game whose target is still alive continues.

### 2.8 Logging
- Every verdict change is logged at `info` (candidate eligible/blocked, start, stop) with the reason. Raw per-tick scores log at `trace` only.

---

## 3. Architecture & data flow

```
                 +-------------------------------- clipcheckd (service) --------------------------------+
                 |                                                                                      |
  WinEvent hook  |   +-----------------+    Snapshot     +-------------------+   DetectEvent (mpsc)      |
  (foreground)---+-->| Scheduler       |--------------->| detect::Detector   |-----------------------+   |
  timer tick ----+   | (poll cadence)  |                | state machine      |                       |   |
                 |   +-----------------+                | + classification   |                       v   |
                 |                                      +---------+---------+        +----------------------+
  config reload -+-----------------------------------------------|---------+------->  | session (start/stop, |
                 |                                                |                  | split, relaunch)     |
                 |   +-------------------+   +-----------------+    |                  +----------------------+
                 |   | SystemProbe (Win32)|  | LibraryIndex    |<---+ (cached, scan every N h)
                 |   | processes, windows,|  | steam/epic/gog/ |
                 |   | PDH GPU, modules   |  | xbox            |     ipc: detect.* messages to UI
                 |   +-------------------+   +-----------------+
                 +----------------------------------------------------------------------------------------+
```

### 3.1 Module layout (`crates/detect/src/`)
| Module | Responsibility | Pure? |
|---|---|---|
| `probe/mod.rs` | `SystemProbe` trait; `Snapshot` type | trait |
| `probe/win.rs` | Win32 implementation: Toolhelp, EnumWindows, DWM, Monitor APIs, QueryFullProcessImageName | no |
| `probe/gpu.rs` | PDH `\GPU Engine(*)\Utilization Percentage`, per-pid 3D sum | no |
| `probe/modules.rs` | Toolhelp module snapshot with anti-cheat guard and denial cache | no |
| `classify/gates.rs` | Hard gates (cloaked, tool window, blocklist, launcher, size, title) | yes |
| `classify/score.rs` | Point table, penalties, clamp | yes |
| `classify/tree.rs` | Process tree, launcher ancestry, root grouping, UWP child resolution | yes |
| `library/mod.rs` | `LibraryIndex`, `LibrarySource` trait, path matching | yes (sources are IO) |
| `library/steam.rs`, `epic.rs`, `gog.rs`, `xbox.rs` | Per-store parsers | parsers yes |
| `known.rs` | Built-in known-games DB (`data/known_games.toml`, `include_str!`) | yes |
| `lists.rs` | Built-in blocklist, launchers, anti-cheat names; merge with config | yes |
| `state.rs` | State machine (Idle/Pending/Active/Ending) and debounce | yes |
| `detector.rs` | `Detector` facade: owns state, config, library; `tick()` | glue |
| `manual.rs` | Manual add: validate candidate, produce `AllowRule`, persist via `toml_edit` | IO at edge |
| `scheduler.rs` | Poll cadence, WinEvent hook thread, wakeup channel | thread |

Pure modules take a `Snapshot` and return data; all Win32 access sits behind `SystemProbe`, so the whole classification and state machine is unit-testable with a fake probe.

### 3.2 Threading
- One **detector thread** owns `Detector`. It blocks on `recv_timeout(next_poll_at)` from a wakeup channel.
- The WinEvent hook (`SetWinEventHook`, `EVENT_SYSTEM_FOREGROUND`, `WINEVENT_OUTOFCONTEXT`) runs on its own thread with a message loop and posts a `Wake::Foreground` to the detector channel. It does no other work.
- Config reload posts `Wake::Config(DetectorConfig)`.
- Events leave via `std::sync::mpsc::Sender<DetectEvent>` to the session thread. The IPC layer subscribes to the same stream through a fan-out in `clipcheckd` (detect does not know about the pipe).

### 3.3 Dependencies on other crates
- `core`: `GameId`, `ExeKey`, `StoreRef`, `Store`, `Hwnd`, `CaptureTarget`, error types, config schema fragment types.
- `ipc`: serializes `DetectEvent` to the IPC messages in Section 7 (done in `clipcheckd`, not in `detect`).
- `library`: does not depend on `detect`. The library crate's `recordings` table keys on `GameId`, which `detect` produces.

---

## 4. Key types, traits and function signatures

Signatures below are the intended public surface. Exact `windows` crate type paths will be settled during T2.

### 4.1 Identifiers (in `core`)
```rust
/// Lowercased file name, e.g. "celeste.exe". Never includes a directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExeKey(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Store { Steam, Epic, Gog, Xbox, Manual }

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StoreRef { pub store: Store, pub id: String }

/// GameId = normalized exe + optional store id (brief, section "Naming/ID conventions").
/// Manual games with no store id use exe alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GameId { pub exe: ExeKey, pub store: Option<StoreRef> }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hwnd(pub isize);

/// What capture needs in order to bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTarget {
    pub hwnd: Hwnd,
    pub pid: u32,
    pub process_start_ms: i64,   // distinguishes pid reuse
    pub exe_path: PathBuf,
}
```

### 4.2 Probe layer (`detect::probe`)
```rust
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: u32,
    pub exe: ExeKey,
    pub exe_path: Option<PathBuf>,   // None if access denied (elevated/protected)
    pub start_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectPx { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }
impl RectPx { pub fn w(&self) -> i32; pub fn h(&self) -> i32; pub fn area(&self) -> i64; }

#[derive(Clone, Debug)]
pub struct MonitorInfo { pub rect: RectPx, pub work: RectPx, pub dpi: u32 }

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: Hwnd,
    pub pid: u32,                    // owner pid after UWP child resolution (Section 5.7)
    pub class_name: String,
    pub title: String,
    pub rect: RectPx,                // physical pixels (process is per-monitor DPI aware)
    pub monitor: MonitorInfo,
    pub visible: bool,               // IsWindowVisible
    pub minimized: bool,             // IsIconic
    pub cloaked: bool,               // DWMWA_CLOAKED != 0
    pub tool_window: bool,           // WS_EX_TOOLWINDOW
    pub has_caption: bool,           // WS_CAPTION
    pub popup: bool,                 // WS_POPUP
    pub owner: Option<Hwnd>,
    pub is_uwp_frame: bool,          // owner exe is ApplicationFrameHost.exe
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub at: Option<std::time::Instant>,
    pub processes: Vec<ProcessInfo>,
    pub windows: Vec<WindowInfo>,
    pub gpu_3d_pct_by_pid: HashMap<u32, f32>,  // averaged over last PDH window
    pub foreground: Option<Hwnd>,
    pub anticheat_running: bool,               // any known anti-cheat service/driver process
}

pub enum ModuleProbe {
    Loaded(ModuleSet),
    Denied,      // OpenProcess(VM_READ) refused
    Skipped,     // guard: anti-cheat sensitive or recently denied
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModuleSet { pub d3d11_or_dxgi: bool, pub d3d12: bool, pub vulkan: bool, pub opengl: bool }

pub trait SystemProbe: Send {
    fn snapshot(&mut self, want_modules_for: &[u32]) -> Result<Snapshot, ProbeError>;
    fn modules(&mut self, pid: u32, sensitive: bool) -> ModuleProbe;
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("toolhelp snapshot failed: {0}")] Toolhelp(String),
    #[error("enumwindows failed: {0}")] Windows(String),
    #[error("pdh failed: {0}")] Pdh(String),
}
```

`WinProbe` implements `SystemProbe`. A `FakeProbe` with scripted snapshots implements it for tests.

### 4.3 Library (`detect::library`)
```rust
#[derive(Clone, Debug)]
pub struct LibraryEntry {
    pub store_ref: StoreRef,
    pub display_name: String,
    pub install_dir: PathBuf,        // normalized, lowercase-compare at match time
    pub exes: Vec<ExeKey>,          // executables found under install_dir (depth-limited)
    pub launcher_exe: Option<ExeKey>,
    pub anticheat: bool,
}

pub trait LibrarySource: Send + Sync {
    fn store(&self) -> Store;
    fn scan(&self) -> Result<Vec<LibraryEntry>, LibraryError>;
}

#[derive(Clone, Debug, Default)]
pub struct LibraryIndex { entries: Vec<LibraryEntry>, scanned_at: Option<std::time::Instant> }

impl LibraryIndex {
    /// Path match wins over exe-name match. Returns the entry whose install_dir is an ancestor of exe_path.
    pub fn match_process(&self, p: &ProcessInfo) -> Option<&LibraryEntry>;
    pub fn refresh(&mut self, sources: &[Box<dyn LibrarySource>]) -> Result<(), LibraryError>;
}
```

### 4.4 Known DB and lists (`detect::known`, `detect::lists`)
```rust
#[derive(Clone, Debug, Deserialize)]
pub struct KnownGame {
    pub exe: ExeKey,
    pub name: String,
    pub store: Option<StoreRef>,
    pub anticheat: bool,             // do not touch with module probe
}

pub fn known_db() -> &'static [KnownGame];   // parsed once from include_str!("../data/known_games.toml")

pub struct Lists {
    pub blocklist: HashSet<ExeKey>,      // built-in + detect.block_extra
    pub launchers: HashSet<ExeKey>,      // built-in only
    pub anticheat_services: HashSet<ExeKey>,
}
impl Lists { pub fn merged(extra_block: &[String]) -> Self; }
```

### 4.5 Classification (`detect::classify`)
```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockReason { Blocklisted, Launcher, SystemShell, Cloaked, ToolWindow, NoVisibleWindow, TooSmall, BadTitle, NoGraphicsEvidence, LowScore(u8) }

#[derive(Clone, Debug)]
pub struct Signals {
    pub fullscreen: FullscreenKind,          // Geometry | NearFullscreen | None
    pub gpu_3d_pct: Option<f32>,
    pub modules: Option<ModuleSet>,          // None if Skipped/Denied
    pub known: Option<&'static KnownGame>,
    pub library: Option<LibraryEntry>,
    pub foreground_recent: bool,             // foreground within 10 s
    pub size_ok: bool,
    pub title_ok: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenKind { Geometry, NearFullscreen, None }

#[derive(Clone, Debug)]
pub struct Candidate {
    pub key: CandidateKey,           // (hwnd, pid, process_start_ms)
    pub game: GameId,                // derived: known/library/manual store id, else exe only
    pub display_name: String,
    pub target: CaptureTarget,
    pub root_pid: u32,               // group root (Section 5.5)
    pub signals: Signals,
    pub score: u8,                   // 0..=100
    pub verdict: Verdict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict { Eligible, Pinned, Blocked(BlockReason) }

pub fn classify(snap: &Snapshot, lists: &Lists, library: &LibraryIndex,
                allow: &[AllowRule], cfg: &DetectorConfig, now: Instant) -> Vec<Candidate>;

pub fn score(sig: &Signals, cfg: &DetectorConfig, running: bool, uptime: Duration) -> u8;
```

### 4.6 Config mirror and rules
```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AllowRule {
    pub exe: ExeKey,
    pub display_name: Option<String>,
    pub store: Option<StoreRef>,
}

#[derive(Clone, Debug)]
pub struct DetectorConfig {          // Section 6 keys
    pub mode: DetectMode,
    pub poll_idle_ms: u32,
    pub poll_active_ms: u32,
    pub start_confirm_ms: u32,
    pub stop_grace_ms: u32,
    pub min_score: u8,
    pub fullscreen_tolerance_px: u8,
    pub gpu_probe: bool,
    pub gpu_threshold_pct: f32,
    pub module_probe: bool,
    pub library_scan: bool,
    pub library_scan_interval_h: u32,
    pub extra_library_dirs: Vec<PathBuf>,
    pub allow: Vec<AllowRule>,
    pub block_extra: Vec<String>,
    pub multi_candidate: MultiCandidate,
    pub allow_manual_add: bool,
}
```

### 4.7 State machine and events (`detect::state`, `detect`)
```rust
pub enum DetectState {
    Idle,
    Pending { key: CandidateKey, game: GameId, since: Instant },
    Active { game: GameId, target: CaptureTarget, started: Instant, started_by: StartReason, last_seen: Instant },
    Ending { game: GameId, target: CaptureTarget, since: Instant, reason: StopReason },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartReason { Detected, Allowlisted, Manual, AlreadyRunning }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason { ProcessExited, WindowDestroyed, RuleRemoved, ModeOff, Shutdown, GraceExpired }

#[derive(Clone, Debug)]
pub enum DetectEvent {
    GameStarted { game: GameId, display_name: String, target: CaptureTarget, reason: StartReason, at_ms: i64 },
    TargetChanged { game: GameId, old: CaptureTarget, new: CaptureTarget, at_ms: i64 },
    GameRelaunched { game: GameId, target: CaptureTarget, gap_ms: u32, at_ms: i64 },
    GameStopped { game: GameId, reason: StopReason, at_ms: i64 },
    CandidatesChanged { candidates: Vec<CandidateSummary> },   // rate-limited to 1/s
}

pub struct Detector { /* cfg, lists, library, state, probe, prev_snapshot, tx */ }

impl Detector {
    pub fn new(cfg: DetectorConfig, probe: Box<dyn SystemProbe>, library: LibraryIndex,
               tx: std::sync::mpsc::Sender<DetectEvent>) -> Self;

    /// One scheduler step. Returns the next time it wants to run.
    pub fn tick(&mut self, now: Instant, wake: Option<Wake>) -> Instant;

    pub fn reload(&mut self, cfg: DetectorConfig);

    /// Validates the candidate key against a fresh snapshot, then persists an AllowRule.
    pub fn add_manual(&mut self, key: CandidateKey, now: Instant) -> Result<AllowRule, ManualError>;
}

pub enum Wake { Foreground, Config(DetectorConfig), Resume, Shutdown }
```

---

## 5. Algorithms and state machines

### 5.1 Scheduler cadence
| Situation | Next poll |
|---|---|
| `Idle`, no candidate with score ≥ `min_score - 15` | `poll_idle_ms` (default 3000) |
| `Idle` with a near-candidate (score in [min-15, min)) or `Pending` | `poll_active_ms` (default 1000) |
| `Active` (target bound) | liveness check every `LIVENESS_POLL_MS` (250 ms): window and process probes only, no window enumeration, no PDH. The full scan keeps `poll_active_ms` (default 1000) |
| `Ending` | liveness check every 250 ms. A check that is not alive runs a full scan at once, so a replacement window is found in the same step |
| Any `Wake::Foreground` | immediate tick, then normal cadence |

The loop runs `DetectorCore::step(now, scan_every)`. A full scan runs when `scan_every` has passed since the last one. Otherwise, while a target is bound, `liveness_tick` runs (a full scan only on a loss, or in `Ending`). The thread sleeps `next_delay_ms`, capped at 250 ms while bound. Constants live in `crates/detect/src/timing.rs`.

The idle budget (< 0.3 % CPU, Section 9) is met by: no module probe while idle, no PDH collection while no window is fullscreen-ish, and a cheap process list diff (pid set + start times) before any window enumeration.

### 5.2 One tick (pseudo-code)
```
tick(now, wake):
  if wake == Shutdown: stop_active(ShutdownReason); return far_future
  if wake == Config(c): apply config; if mode==Off: stop_active(ModeOff)
  if wake == Resume:   reset Pending to Idle; force full scan

  snap = probe.snapshot(want_modules_for = pids_needing_modules())
     // pids_needing_modules = top-level pids of windows that are fullscreen-ish
     //                         AND size_ok AND not on anticheat guard list
  cands = classify(snap, lists, library, allow, cfg, now)
  roots = group_by_root(cands)                 // 5.5
  best  = pick_target(roots)                   // 5.6 selection + hysteresis
  emit CandidatesChanged if changed (rate-limited)

  state_step(now, best, cands)                 // 5.3
  return now + cadence()                       // 5.1
```

### 5.3 State machine

```
              best eligible (any)                       confirmed for start_confirm_ms
   Idle ──────────────────────────► Pending ──────────────────────────────────► Active
    ▲                                  │   ▲                                        │
    │   best changed / not eligible    │   │ target alive (liveness)               │ target dead
    │◄─────────────────────────────────┘   │                                        │ (process exit,
    │                                      │                                        │  hwnd destroyed)
    │                                      │  same GameId back within grace         ▼
    │                                  Ending ◄──────────────────────────────────────┘
    │      grace expired: emit GameStopped                 │
    └──────────────────────────────────────────────────────┘
```

Transitions:

| From | Condition | To | Event |
|---|---|---|---|
| Idle | `best` is Eligible or Pinned | Pending{key, game, since=now} | — |
| Pending | `best.key != key` or verdict became Blocked/not eligible | Idle | — |
| Pending | same key held for `start_confirm_ms` (0 for Pinned/Allowlisted-with-age>5 s) | Active | `GameStarted` (`reason`) |
| Active | target liveness OK | Active (last_seen=now) | — |
| Active | same GameId candidate appears in the same root group with higher score, held ≥ 15 pts for 2 s | Active (target swapped) | `TargetChanged` |
| Active | target gone, but a candidate with same GameId/install dir is found immediately (same tick) | Active (retarget) | `TargetChanged` |
| Active | target process exited, no replacement | Idle (at once) | `GameStopped{ProcessExited}` (no grace) |
| Active | target window destroyed, process alive, no replacement | Ending{since=now, reason=WindowDestroyed} | — |
| Ending | target alive again (window and process answer), no replacement | Active (silent) | — |
| Ending | candidate with same GameId / install dir (another window) Eligible or Pinned appears | Active | `GameRelaunched{gap_ms}` |
| Ending | target process exits, no replacement | Idle (at once) | `GameStopped{ProcessExited}` |
| Ending | `now - since ≥ min(stop_grace_ms, WINDOW_GONE_GRACE_MS)` (1 s) | Idle | `GameStopped{WindowDestroyed}` |
| Active or Ending | allowlist-only mode releases an unpinned game (the game still runs) | Ending{reason=RuleRemoved} | `GameStopped{RuleRemoved}` after `stop_grace_ms` (10 s) |
| Ending | `wake=Config` with mode=Off or rule removed | Idle | `GameStopped{ModeOff|RuleRemoved}` |

Key property: **Active never checks the start gate.** Once started, the game stays until liveness fails. This is what keeps a fullscreen game alive while the user alt-tabs (the gate's fullscreen signal disappears, the process does not).

### 5.4 Liveness (for Active and Ending)
A target is alive iff all of:
1. `pid` exists in the process snapshot with the same `process_start_ms` (guards pid reuse).
2. `hwnd` still refers to a window (`IsWindow`) whose owning pid is `pid` (or whose UWP child resolves to `pid`).
3. The window is not in the process of being destroyed (`!cloaked_and_empty_title`; cloaked alone is not dead, since UWP minimized windows are cloaked).

If (1) or (2) fail, the target is dead. If only (3) fails, the target is alive but `cloaked`; capture will show black frames.

The bound target is checked every `LIVENESS_POLL_MS` (250 ms) with two cheap probes. No window enumeration is needed:
- **Window**: `IsWindow(hwnd)` and visible or minimized (`win::window_alive`).
- **Process**: a handle opened with `SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` on the pid (`win::ProcessWatch`), polled with `WaitForSingleObject(h, 0)`. The handle pins the process object, so the OS cannot give the pid to another process while the watch is held (no pid-reuse false positives). The watch grants no read, write or memory rights. If the pid no longer exists when the handle is opened, the process is dead. If the open is refused (protected process), the process is judged by the last snapshot, as before.

Liveness of `Released` (allowlist-only mode released an unpinned game) is separate: the game is still running, so it uses the normal stop grace.

### 5.5 Process tree and grouping (`classify::tree`)
1. Build `parent: HashMap<pid, pid>` and `exe: HashMap<pid, ExeKey>` from `processes`.
2. For each candidate window's pid `P`, walk ancestors. Define `root(P)` = the highest ancestor that is **not** in `{launchers ∪ SystemShell ∪ services.exe ∪ svchost.exe ∪ explorer.exe ∪ cmd/powershell}`. If `P` itself is a launcher, it has no group and is Blocked(Launcher).
3. Candidates with the same `root` form a group. Within a group, only one candidate is eligible as a *target*: the one with (visible window, then GPU 3D %, then deepest pid). Others get `verdict=Blocked(LaunchStub)`-style reason `NotPrimaryInGroup` (not shown as a block reason in the UI, shown as "child of <name>").
4. Launcher stub pattern (Launcher.exe → Game.exe → Shipping.exe): the launcher's window is blocked; the game root is the child that owns a visible window. If the child with the window exits and a sibling/child with the same install dir appears, that is a relaunch (5.3).
5. Library grouping: if a group contains a process whose `exe_path` is under a library `install_dir`, the group's `GameId` is that library entry's `StoreRef` (even if the window belongs to a child exe).

### 5.6 Target selection and hysteresis
```
eligible = [c for c in cands if c.verdict in {Eligible, Pinned}]
best = max(eligible, key = (c.verdict == Pinned, c.score, c.signals.foreground_recent, area(c.target), -pid))
   # Pinned (manual add) outranks score, but still needs to be in a root group that is primary.

if state is Active(game, target):
   current = find(cands, target.key)
   if current is alive and best.game != game:
       switch only if best.score >= current.score + 15 for ≥ 2 s   (hysteresis)
```

`multi_candidate = best_score` (default) uses the above. `prefer_foreground` puts foreground-recent ahead of score (for users with two concurrent eligible games; rare).

### 5.7 Window enumeration and UWP
```
EnumWindows(cb):
  for hwnd:
     if !IsWindowVisible && !IsIconic: skip (cheap reject, but keep minimized)
     collect WindowInfo fields (GetWindowRect, DwmGetWindowAttribute(CLOAKED), styles, owner, monitor)
     if owner exe == ApplicationFrameHost.exe:
         child = EnumChildWindows(hwnd) where class == "Windows.UI.Core.CoreWindow"
         if child: pid = GetWindowThreadProcessId(child); rect = GetWindowRect(child); is_uwp_frame = true
```
Monitor: `MonitorFromWindow(MONITOR_DEFAULTTONEAREST)` then `GetMonitorInfo`. Fullscreen compares `window.rect` with `monitor.rect` within `fullscreen_tolerance_px`.

### 5.8 Fullscreen and geometry signals
```
fs(w):
  if |w.rect - m.rect| <= tol on all four sides  -> Geometry
  elif area(w.rect ∩ m.rect) >= 0.90 * area(m.rect) and !has_caption -> NearFullscreen
  else -> None
```
Borderless-windowed games are `Geometry` (no caption, same rect). Exclusive fullscreen is also reported as `Geometry` by the window APIs; we cannot distinguish it, and capture decides (5.10).

### 5.9 GPU signal (`probe::gpu`)
- Setup once: `PdhOpenQueryW`, `PdhAddEnglishCounterW(q, "\\GPU Engine(*)\\Utilization Percentage")` expanded with `PdhExpandWildCardPathW`. English counter names avoid locale failures.
- Each PDH sample pair (taken at most every 2 s, only if `gpu_probe` and at least one fullscreen-ish candidate exists): parse instance name `pid_<pid>_luid_..._engtype_3D`, sum utilization per pid (clamp 100).
- Output: `gpu_3d_pct_by_pid`. Score uses a rolling average over the last 5 s (3 samples). If PDH is unavailable, `gpu_3d_pct = None` and the GPU points are replaced by module evidence only (if any) — the gate then needs the known/library path (Section 5.11).

### 5.10 Module probe and anti-cheat guard (`probe::modules`)
Purpose: detect `d3d11.dll`/`dxgi.dll`/`d3d12.dll`/`vulkan-1.dll`/`opengl32.dll` in the candidate process.

Guard rules (all must hold to probe):
1. Candidate is a fullscreen-ish or known/library match (cheap signals first).
2. `detect.module_probe = true`.
3. Process is **not** anticheat-sensitive (known DB `anticheat = true`, library `anticheat = true`, or its root group name matches the anticheat list).
4. No anticheat driver/service process is running for that game (`anticheat_running` false), or the game is not in the sensitive list. If an anticheat service is running, the module probe is skipped for every process.
5. Not in denial cache (a `Denied` result suppresses retries for 10 min).
6. Max 1 probe per pid per 30 s.

Implementation: `CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)` — read-only module list; we never call `ReadProcessMemory`, `WriteProcessMemory`, `CreateRemoteThread`, `SuspendThread`, or open a handle with write/injection rights. A denied open is recorded and treated as `Denied`.

Result is cached per `(pid, process_start_ms)` for the process lifetime.

### 5.11 Gates and scoring

**Hard gates** (a failing gate sets `verdict = Blocked`, no score matters):

| # | Gate | Block reason |
|---|---|---|
| G1 | exe not in blocklist (built-in ∪ `block_extra`) | `Blocklisted` |
| G2 | exe not in launchers, not in SystemShell | `Launcher` / `SystemShell` |
| G3 | window not `tool_window`, not `cloaked`-and-untitled | `ToolWindow` / `Cloaked` |
| G4 | `visible || minimized` | `NoVisibleWindow` |
| G5 | window ≥ 640×480 | `TooSmall` |
| G6 | title not in `["", "Default IME", "MSCTFIME UI", "Program Manager"]` or known match | `BadTitle` |
| G7 | group primary (5.5) | `NotPrimaryInGroup` (hidden in UI) |
| G8 | graphics evidence: `Geometry` or `NearFullscreen` **or** known match **or** library match **or** `gpu_3d_pct ≥ gpu_threshold_pct` | `NoGraphicsEvidence` |

Allowlisted (`detect.allow`) and manual-added games skip G5–G8 and get `Pinned`; they still pass G1–G4.

**Score** (clamped to 100; eligible iff score ≥ `min_score` after gates):

| Signal | Points |
|---|---|
| Known DB match (exe name) | +40 |
| Library match (install dir ancestor of exe path) | +40 |
| `FullscreenKind::Geometry` | +20 |
| `FullscreenKind::NearFullscreen` | +10 |
| GPU 3D ≥ `gpu_threshold_pct` (5 s rolling average) | +20 |
| GPU 3D ≥ 2 % but < threshold | +5 |
| D3D11/DXGI or D3D12 module | +10 |
| Vulkan module | +10 |
| OpenGL module | +5 |
| Foreground within 10 s | +5 |
| Window ≥ 1280×720 | +5 |
| Non-empty, non-generic title | +5 |

Penalties (applied to running games only, so a loading screen is not penalized at start):

| Condition | Points |
|---|---|
| Title contains `Setup`, `Installer`, `Uninstall`, `Updater`, `Crash` (case-insensitive) | −30 (also a gate: score ≤ 30 → blocked) |
| Title contains `Launcher` | −15 |
| GPU 3D < 2 % averaged over 60 s while running | −20 |

**Default threshold `min_score = 60`.** Worked examples:
- Unknown fullscreen game with GPU 40 %, D3D11, foreground, 1920×1080, title: 20+20+10+5+5+5 = **65** → eligible.
- Unknown fullscreen YouTube in a browser: browser is blocklisted → **blocked** (G1).
- Unknown fullscreen video player (VLC) → blocklisted (G1).
- Unknown fullscreen app with GPU 3 %, no D3D module, window ≥ 1280×720 with a non-generic title: G8 is satisfied by `Geometry` (the G8 row above lists `Geometry`), so it is **not** blocked. Score: 20 (Geometry) + 5 (GPU 2–threshold) + 5 (size) + 5 (title) = 35 → scored but **not eligible** (35 < 60; `Verdict::Scored { accepted: false }`). It does not start recording. `NoGraphicsEvidence` applies only to windowed candidates with no fullscreen geometry, no known or library match and GPU below threshold.
- Known game in a window with a generic title (menu with no title), not fullscreen, GPU 0 %: 40+5 = 45 → below 60 and not accepted by the known-window rule below; does not start.
- Known game windowed at 1280×720 or larger with a real title, GPU 0 %, in menu: 40+5+5 = 50, **accepted by the known-window rule** (see next). It starts without fullscreen or GPU evidence. This replaces the earlier rule that a windowed known game starts only after GPU rises or when allowlisted.
- Known game windowed below 720p (e.g. 1000×600) with a real title: 40+5 = 45 → not accepted.

**Known-window rule (added for FiveM and other windowed titles).** A candidate whose exe is in the known DB is accepted when all of these hold: it owns a visible window of at least 1280×720, its title is not generic, and no penalty applied (no installer/launcher title penalty and no idle-GPU penalty). The gate still applies first (G1 to G8; a blocklisted or launcher exe never reaches this rule). Before this rule a windowed FiveM window scored 40+5+5+5 = 55 (known DB, large window, title, foreground) and never reached 60, so FiveM was not detected unless the game window was fullscreen. Decision recorded in `DESIGN.md` (see D-entry for FiveM detection); confirm before release because it also lets a known title's windowed menu start recording.

### 5.12 Debounce summary
| Stage | Debounce |
|---|---|
| Start | `start_confirm_ms` (3 s) of stable best candidate (0 for Pinned older than 5 s) |
| Retarget | +15 pts for ≥ 2 s |
| Stop, process exit | none: at the first liveness check that sees it (≤ 250 ms) |
| Stop, window destroyed | `min(stop_grace_ms, WINDOW_GONE_GRACE_MS)` = 1 s after the loss |
| Stop, allowlist release | `stop_grace_ms` (10 s) |
| Liveness poll (bound target) | `LIVENESS_POLL_MS` = 250 ms |
| Relaunch | same window-gone grace; same `GameId` or install dir |
| Foreground wake | coalesced: at most one forced tick per 250 ms |
| `CandidatesChanged` to UI | at most once per second |

### 5.13 Manual add (`manual::add_manual`)
```
add_manual(key, now):
  snap = probe.snapshot(...)   // fresh, not cached
  c = find(classify(snap, ...), key) else Err(NotFound)
  if c.verdict == Blocked(Blocklisted|SystemShell|Launcher): Err(Refused(reason))
  rule = AllowRule { exe: c.game.exe, display_name: Some(c.display_name), store: c.game.store }
  toml_edit: append/replace [[detect.allow]] in config.toml, preserving comments
  config watcher triggers reload (or send Wake::Config directly)
  return Ok(rule)
```
Manual add of a non-primary group member (e.g., `Shipping.exe` when `Game.exe` is primary) is redirected to the group root, and the rule records the root's exe.

### 5.14 Library scan (`library`)
Runs at service start (async, not blocking first tick; initial detection uses known DB only until scan completes), then every `library_scan_interval_h` hours, and on `Wake::Config`.

| Store | Source | Parse | Entry |
|---|---|---|---|
| Steam | `HKCU\Software\Valve\Steam\SteamPath` (fallback `HKLM\SOFTWARE\WOW6432Node\Valve\Steam\InstallPath`) → `steamapps\libraryfolders.vdf` | KeyValues (VDF) text parser, `"path"` entries | for each library: `steamapps\appmanifest_<appid>.acf` → `appid`, `name`, `installdir` → `<lib>\steamapps\common\<installdir>` |
| Epic | `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item` | JSON: `AppName`, `DisplayName`, `InstallLocation`, `LaunchExecutable`, `bIsIncompleteInstall` | skip incomplete; `StoreRef{Epic, AppName}` |
| GOG | `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<id>` | registry values `gameID`, `gameName`, `path`, `exe` | `StoreRef{Gog, gameID}` |
| Xbox / Game Pass | `<drive>:\XboxGames\*\Content\MicrosoftGame.config` on each fixed drive (bounded to drive roots) | XML: `ExecutableList/Executable@Name`, `ShellVisuals@DefaultDisplayName`, `Game@ConfigVersion`; `StoreRef{Xbox, Identity@Name}` | exe paths relative to `Content\` |

Exe index per entry: walk `install_dir` to depth 3, collect `*.exe`, skip `_CommonRedist`, `Engine\Extras`, `redist`, `vcredist`, `DirectX`, `UnityCrashHandler*`, `CrashReport*`, `*UnrealCEFSubProcess*`, `*Setup*`, `*Installer*`, `*Uninstall*`. Cap 400 exes per entry.

Launcher flag for library entries: Steam entries have `launcher_exe = steam.exe`; Epic `EpicGamesLauncher.exe`. These are not used to block; they only inform grouping.

Matching is path-based: `process.exe_path` normalized (lowercased, `\\?\` stripped) starts with `install_dir` (lowercased) + `\`. Exe-name-only matches against library `exes` are a weaker secondary match (+40 still, but only when the exe name is unique across the index, to avoid `game.exe` collisions).

### 5.15 Exit, relaunch and window replacement (examples)
- **Normal exit**: `Active` → target dead (process gone) → `Ending` → no replacement within 10 s → `GameStopped{ProcessExited}`.
- **Crash and relaunch from the launcher within 10 s**: `Ending` → new process with same install dir appears and is Eligible → `GameRelaunched{gap_ms}`; session continues, capture rebinds.
- **Mode switch (fullscreen → windowed, same process, new HWND)**: target hwnd destroyed, same pid has a new top-level window → `TargetChanged` in the same tick, no Ending.
- **Launcher opens game window after it closes its own UI**: launcher blocked; game root candidate becomes primary → `Pending` → normal start.

### 5.16 Blocklist (built-in, merged with `block_extra`)
| Category | Exes |
|---|---|
| Browsers | `chrome.exe`, `msedge.exe`, `firefox.exe`, `brave.exe`, `opera.exe`, `vivaldi.exe`, `arc.exe`, `msedgewebview2.exe` |
| Chat / comms | `discord.exe`, `slack.exe`, `teams.exe`, `ms-teams.exe`, `zoom.exe`, `telegram.exe`, `whatsapp.exe` |
| Media | `vlc.exe`, `mpc-hc.exe`, `mpc-hc64.exe`, `mpv.exe`, `spotify.exe`, `wmplayer.exe`, `netflix.exe` |
| Recorders / self | `obs64.exe`, `obs32.exe`, `medal.exe`, `clipcheck.exe`, `clipcheckd.exe`, `nvidia share.exe`, `gamebar.exe`, `gamebarpresencewriter.exe` |
| Shell / system | `explorer.exe`, `dwm.exe`, `shellexperiencehost.exe`, `searchhost.exe`, `startmenuexperiencehost.exe`, `applicationframehost.exe` (as owner; children resolved in 5.7), `textinputhost.exe`, `taskmgr.exe`, `windowsterminal.exe`, `cmd.exe`, `powershell.exe`, `pwsh.exe`, `notepad.exe`, `code.exe`, `devenv.exe`, `idea64.exe`, `rider64.exe` |

### 5.17 Launchers (built-in; never candidates, used for grouping)
`steam.exe`, `steamwebhelper.exe`, `epicgameslauncher.exe`, `epicwebhelper.exe`, `galaxyclient.exe`, `xboxpcapp.exe`, `battle.net.exe`, `riotclientservices.exe`, `eadesktop.exe`, `ea app.exe`, `upc.exe`, `ubisoftconnect.exe`.

### 5.18 Anti-cheat service names (never probed, never a candidate by themselves)
`easyanticheat.exe`, `easyanticheat_eos.exe`, `beservice.exe`, `vgc.exe`, `vgtray.exe`, `faceit.exe`, `fbc.exe`, `eosoverlayrenderer-win64-shipping.exe` (overlay only). Their presence sets `anticheat_running = true`.

### 5.19 Name patterns (exe lists and known DB)
Every exe name in the built-in blocklist, shell list, launchers, anti-cheat names, the known DB, `detect.allow` and `detect.block` (`block_extra`) may be a **wildcard pattern**:

| Syntax | Meaning |
|---|---|
| `*` | any run of characters, including none |
| `?` | exactly one character |

Rules (implemented in `crates/detect/src/lists.rs` `glob_match`, `NameSet`; and `known_games.rs`):
1. Matching is ASCII case-insensitive on the file name. For lists and patterns, directory parts and surrounding spaces are stripped first (`normalize_exe`), so a full path matches its file name. Exact known-DB names are compared after trim and lowercase only, as before.
2. The whole file name must match. `fivem*` does not match `xfivem.exe`; `fivem*.exe` does not match `fivem.exe.bak`.
3. A name without `*` or `?` is an exact name, as before. Existing lists behave the same.
4. Wildcard-only entries (`*`, `??`) are ignored, so a bad entry cannot block or allow every game.
5. Known DB: exact names are tried first. Then patterns are tried in file order and the first match gives the display name.
6. Allow and block: the blocklist still wins over an allow pattern (5.16 rule). Patterns match in `Lists::is_allowlisted` and `Lists::is_blocked`.
7. Known-DB patterns in the shipped list (`known_games.json`): `FiveM*GTAProcess.exe` and `FiveM*GameProcess.exe` (display name `FiveM`), `RedM*GameProcess.exe` (`RedM`). Every FiveM build (`FiveM_b2802_GTAProcess.exe`, `FiveM_b3258_GTAProcess.exe`, ...) matches, so a new build does not need an app release.
8. Not matched on purpose: `FiveM.exe` (Chromium launcher UI) and `FiveM_ROSLauncher*` (Rockstar social club launcher). `PlayGTAV.exe` (Rockstar launcher) is in the built-in launchers (5.17) and is never a candidate. `GTA5.exe` and `GTA5_Enhanced.exe` are exact known entries.

**Launcher vs game for FiveM.** `FiveM.exe` gets no known-DB credit, so its window is scored like any unknown window; in practice its Chromium UI fails G8 (no fullscreen, no GPU load) and is not recorded. The game window belongs to the build-numbered `FiveM_*GTAProcess.exe`, which matches the known DB. `is_primary_in_group` is not computed yet (5.5 is not wired), so there is no launcher-tree preference; the rules above are what keep the launcher out.

**Access denied.** A process that cannot be opened (elevated, protected) has an empty exe name and is rejected by G0 (`UnknownExe`). Module probing that is denied yields no graphics modules, so the D3D points are lost, but a known-DB hit still passes G8 and the known-window rule (E7).

**Daemon note.** `game_id` is `exe:<exe>` (`apps/clipcheckd/src/service.rs`), so each FiveM build gets its own game id, and `[games."..."]` overrides and allowlist-mode `should_record` compare exact names. Wildcard allow entries therefore pin a game in detection but are not yet honoured by `should_record` in allowlist mode, and per-build game ids are not merged into one `fivem` folder. Both need a follow-up in `service.rs` and `crates/core` (not changed here).

---

### 5.20 Close latency (game exit to `GameStopped`)

Worst case from the code, at the defaults (`detect_poll_ms` = 1000, `stop_grace_s` = 10). Measured from the moment the process or window ends to the `GameStopped` event leaving the detector. Event delivery to the UI is not included.

| Case | Before | After |
|---|---|---|
| Process exit | 11 000 ms (10 s grace + up to one 1 s scan); 10 000 ms best | 250 ms (one liveness poll), no grace |
| Window destroyed, process alive | 11 000 ms (same path) | 1 250 ms (250 ms poll + 1 s window-gone grace) |
| Relaunch of the same game | within 10 s, `GameRelaunched` | window-gone only, within 1 s. After a process exit a relaunch starts a new recording |
| Transient failed probe (window or process answers again) | (not separated) | no event, no stop |

Calculation, before: the scan saw the loss up to 1 s late, then `Ending` waited 10 s, so the worst case is 10 s + 1 s. After: the process watch signals at exit and the liveness poll is 250 ms; `Ending` for a destroyed window adds only the 1 s grace.

Costs: while a game is bound, the loop runs a liveness check every 250 ms (`IsWindow`, `WaitForSingleObject`). The full scan (window enumeration, GPU, modules) still runs once per `detect_poll_ms`. Tests: `crates/detect/tests/close_latency_tests.rs` (simulated time through `DetectorCore::step` and `next_delay_ms`, plus real-OS probes on this process and a short `ping` child).

---

## 6. Config keys owned by this feature

All keys live under `[detect]` in `%APPDATA%\ClipCheck\config.toml`. Hot-reloadable. Settings doc renders these.

| Key | Type | Default | Range / values | Notes |
|---|---|---|---|---|
| `detect.mode` | enum string | `"auto"` | `auto`, `allowlist_only`, `off` | See 2.1 |
| `detect.poll_idle_ms` | u32 | 3000 | 1000–10000 | Idle cadence |
| `detect.poll_active_ms` | u32 | 1000 | 500–5000 | Near-candidate / pending cadence |
| `detect.start_confirm_ms` | u32 | 3000 | 0–30000 | Debounce before start |
| `detect.stop_grace_ms` | u32 | 10000 | 1000–120000 | Caps the window-gone grace (1 s, `WINDOW_GONE_GRACE_MS`). Not used for process exit (none). Used as-is for allowlist release |
| `detect.min_score` | u8 | 60 | 30–95 | Eligibility threshold after gates |
| `detect.fullscreen_tolerance_px` | u8 | 2 | 0–16 | Geometry match tolerance |
| `detect.gpu_probe` | bool | `true` | — | PDH GPU sampling |
| `detect.gpu_threshold_pct` | f32 | 15.0 | 1.0–100.0 | GPU 3D % for full GPU points |
| `detect.module_probe` | bool | `true` | — | Toolhelp module snapshot (not for anticheat-sensitive) |
| `detect.library_scan` | bool | `true` | — | Steam/Epic/GOG/Xbox scan |
| `detect.library_scan_interval_h` | u32 | 6 | 1–168 | Rescan interval |
| `detect.extra_library_dirs` | array of path | `[]` | — | Extra Steam library roots / game folders |
| `detect.allow` | array of tables `{exe, display_name?, store?}` | `[]` | — | Allowlist; written by manual add |
| `detect.block_extra` | array of string | `[]` | exe file names | Added to built-in blocklist; cannot remove built-ins |
| `detect.multi_candidate` | enum string | `"best_score"` | `best_score`, `prefer_foreground` | Target selection (5.6) |
| `detect.allow_manual_add` | bool | `true` | — | Hides the "Add as game" action when false |

Built-in lists (blocklist, launchers, anticheat names, known DB) are **not** config keys; they ship in the binary.

Example:
```toml
[detect]
mode = "auto"
min_score = 60
start_confirm_ms = 3000
stop_grace_ms = 10000
block_extra = ["mygamelauncherhelper.exe"]

[[detect.allow]]
exe = "celeste.exe"
display_name = "Celeste"
store = { store = "steam", id = "504230" }
```

---

## 7. IPC messages and events

### 7.1 Internal events (`DetectEvent`) consumed by `session` and `clipcheckd`
| Event | Consumer action |
|---|---|
| `GameStarted{game, display_name, target, reason, at_ms}` | `session` opens a new recording (per session policy), capture binds `target.hwnd`, tray shows "Recording: name" |
| `TargetChanged{game, old, new}` | `capture` rebinds to `new.hwnd`; `session` continues; no new file |
| `GameRelaunched{game, target, gap_ms}` | `session` continues the same session; capture rebinds; no new file (unless gap > `session.relaunch_split_after_ms`, owned by session doc) |
| `GameStopped{game, reason}` | `session` finalizes/indexes; buffer keeps running per settings; tray returns to idle |
| `CandidatesChanged{candidates}` | UI only, via IPC; rate-limited to 1/s |

### 7.2 IPC over `\\.\pipe\clipcheck-ipc` (length-prefixed JSON; `ipc` crate)
Service → UI (`type` discriminator):

| `type` | Payload | When |
|---|---|---|
| `detect.candidates` | `{ candidates: [{ key, exe, display_name, window_title, pid, score, verdict, reason?, store? }] }` | on `CandidatesChanged` while the UI is connected |
| `detect.game_started` | `{ game_id, display_name, pid, hwnd, reason, at_ms }` | on `GameStarted` |
| `detect.game_retargeted` | `{ game_id, pid, hwnd, at_ms }` | on `TargetChanged` / `GameRelaunched` |
| `detect.game_stopped` | `{ game_id, reason, at_ms }` | on `GameStopped` |
| `detect.library_updated` | `{ count, scanned_at_ms }` | after library scan |

UI → service (commands):

| `type` | Payload | Result |
|---|---|---|
| `detect.list_candidates` | `{}` | immediate `detect.candidates` |
| `detect.add_game` | `{ key: { hwnd, pid, process_start_ms } }` | `{ ok, rule? , error? }` (`error`: `NotFound`, `Refused(reason)`) |
| `detect.remove_game` | `{ exe }` | `{ ok }`; triggers `RuleRemoved` if running |
| `detect.set_mode` | `{ mode }` | `{ ok }`; persisted via config |
| `detect.rescan_library` | `{}` | `{ ok }` |

`CandidateKey` = `(hwnd, pid, process_start_ms)`. Commands with a stale key return `NotFound` rather than acting on a reused HWND.

Detection never sends IPC directly; `clipcheckd` maps `DetectEvent` to these messages. Detection contains no pipe code.

---

## 8. Edge cases & failure modes

| # | Case | Behavior |
|---|---|---|
| E1 | Exclusive fullscreen game that WGC cannot capture | Detection reports it as a normal game (Geometry + GPU). `capture` detects that WGC cannot bind or produces no frames for 5 s and emits black frames plus a session warning "Exclusive fullscreen not capturable; use borderless". Detection does not stop it. |
| E2 | Browser or video player fullscreen (YouTube, Netflix) | Blocklisted by exe (G1). Never a candidate. A new browser-like app not on the list may pass if its GPU use is high; mitigation: user adds it to `block_extra`. |
| E3 | Two real games running concurrently | Both are candidates; one target by 5.6; the other listed as "also running". Switching requires hysteresis. |
| E4 | Launcher stub spawns game and exits (`Launcher.exe` → `Game.exe`) | Launcher blocked by G2; game root becomes primary; stub exit is not a stop because the game is in the same group. |
| E5 | Game restarts its own process (e.g., UE `Game.exe` → `Game-Win64-Shipping.exe`, with a window moved to the child) | Root grouping (5.5) keeps one GameId; `TargetChanged` rebinds without stop. |
| E6 | Pid reuse after a game exits and an unrelated process gets the same pid | Liveness checks `process_start_ms` (5.4); the old target is dead, not reused. |
| E7 | Elevated game process (we are not elevated) | `OpenProcess` for image name fails; `exe_path = None`. Exe name falls back to window class/title and the known DB cannot match by path. Candidate can still match by known exe name if available via `CreateToolhelp32Snapshot` exe name (available unelevated). Otherwise the candidate is `Blocked(NoGraphicsEvidence)`; manual add works (uses exe name from snapshot). Recommend running the service unelevated; the UI explains elevated games cannot be auto-detected. |
| E8 | Anti-cheat present (EAC/BattlEye/Vanguard) | Module probe skipped for sensitive titles (5.10); no handles beyond `PROCESS_QUERY_LIMITED_INFORMATION`. Detection uses geometry, PDH GPU and known/library evidence only. If `OpenProcess` for image name is denied by the anti-cheat driver, falls back as in E7. |
| E9 | UWP / Xbox app window owned by `ApplicationFrameHost.exe` | Child `CoreWindow` pid resolved (5.7); `is_uwp_frame = true`. If child not found, candidate is blocked. |
| E10 | Game window minimized or alt-tabbed during session | Active does not check gates (5.3); no stop. Capture emits black frames. |
| E11 | Window cloaked (Win10 virtual desktop, UWP suspended) | `cloaked` is not dead (5.4); gate G3 blocks new starts while cloaked and untitled. |
| E12 | DPI scaling / mixed monitors | Process is per-monitor DPI aware V2; all rects are physical pixels. Tolerance `fullscreen_tolerance_px` absorbs off-by-one at 125/150 %. |
| E13 | Game on a removed or unmounted drive | Library entry remains but has no live `exe_path`; no false positive. Library rescan removes or marks it `missing` after two failed rescans. |
| E14 | Library parsers see malformed VDF/ACF/JSON/XML | Parse errors are logged per file and skipped; the rest of the store still indexes. A malformed manifest never fails the scan. |
| E15 | Generic exe names (`game.exe`, `launcher.exe`) collide | Name-only matches against library are used only when the exe name is unique across the index (5.14). Path match is always preferred. |
| E16 | PDH counters missing or disabled (`perflib` disabled) | `gpu_3d_pct = None`; eligibility relies on known/library/module evidence and fullscreen geometry; UI shows "GPU probe unavailable". |
| E17 | Windowed (non-fullscreen) unknown game | Not auto-started (G8 fails). Manual add or allowlist handles it. Documented to the user. |
| E18 | Sleep/resume | `Wake::Resume` resets Pending; Active targets are re-validated by liveness; process start times are re-read to avoid stale pids. |
| E19 | Game switches to a Windows "Game Bar" overlay or Steam overlay window | Overlay windows are tool windows or blocklisted (`gamebar`, `gamebarpresencewriter`); not candidates. |
| E20 | Anti-cheat service stays running after game exit | `anticheat_running` true is only used to skip probes; it never keeps a session Active. |
| E21 | High process churn (installers, updaters spawning many exes) | Scheduler coalesces snapshots; library and module caches keyed by `(pid, start_ms)`; CandidatesChanged rate-limited. |
| E22 | Foreground event storm (rapid alt-tab) | Coalesced to one forced tick per 250 ms (5.12). |
| E23 | Manual add of a stale candidate (process exited between UI render and click) | `NotFound`; UI refreshes list. |
| E24 | Config file edited by hand with a bad `detect.allow` entry | Invalid rule is skipped with a logged warning; the others apply. Config reload never clears active state unless mode becomes `off`. |
| E25 | Game uses a different exe from the store launcher's registered exe (Steam `-applaunch` with a renamed exe) | Library match by install directory (path-based) still groups it; `GameId` stays the store id. |

---

## 9. Performance & resource budget

Targets from the brief: service < 15 MB RSS idle, < 0.3 % CPU with no game running, < 3 % one core while recording on HW encode.

| Metric | Budget | How it is met |
|---|---|---|
| Detector memory (steady) | < 2 MB | Snapshot reuses buffers; library index ~ few hundred entries; exe lists capped at 400 per entry |
| Idle CPU (no game, no fullscreen window) | < 0.1 % of one core | Toolhelp process snapshot every 3 s (~1 ms); EnumWindows only if process set changed or a foreground wake occurred; no PDH; no module probe |
| Active-candidate CPU | < 0.3 % of one core | PDH sampled at most every 2 s, only when a fullscreen-ish candidate exists; module probe capped at 1/pid/30 s |
| Library scan (cold) | < 2 s on SSD with 300 Steam manifests; runs in background thread | Streamed file reads; regex-free VDF parser; exe walk depth ≤ 3 |
| Library scan (warm, interval) | < 200 ms | Skips unchanged manifests by mtime |
| Snapshot allocation | 0 steady-state allocations ideal; ≤ 1 MB per tick | Reused Vec capacity |
| Wakeups | ≤ 1 per second while idle when nothing changes (timer only) | Cadence 5.1 |
| Event latency (game window visible → GameStarted) | ≤ 4 s typical (`start_confirm_ms` + one tick) | — |
| Event latency (game exit → GameStopped) | `stop_grace_ms` + ≤ 1 s | — |
| Syscalls per tick | ≤ 40 typical (window and process queries dominate) | — |
| Handles opened to game processes | Only `PROCESS_QUERY_LIMITED_INFORMATION` (image name) and, if module probe is allowed, Toolhelp module snapshot | Section 5.10 |

---

## 10. Test plan

### 10.1 Unit tests (pure, no Win32)
| Area | Cases |
|---|---|
| Gates | each of G1–G8 blocks correctly; allow/pinned skip G5–G8; blocklist beats allow (manual add refused); launcher blocks |
| Score table | the worked examples in 5.11 produce exactly the listed totals; penalties apply only while running; clamp at 100 |
| Fullscreen | exact match, ±tolerance, ±tolerance+1, 90 % area near-fullscreen, caption present (not borderless), multi-monitor with rect on secondary |
| Tree grouping | Launcher→Game→Shipping; stub exits; game child owns window; two unrelated games; `explorer` as parent not treated as root |
| Library parsers | Steam: VDF with 3 libraries, one missing folder, ACF with BOM; Epic: incomplete install skipped; GOG: missing `exe` value; Xbox: two Executable entries, UTF-8 BOM XML |
| Library match | path match beats name match; ambiguous exe name not used for name match; trailing slash and case differences |
| State machine | Idle→Pending→Active with exact `start_confirm_ms`; Pending aborted when best changes; Active survives alt-tab (gate fails, liveness OK); Ending→Active relaunch within grace; Ending→Idle at grace; retarget hysteresis (14 pts no switch, 15 pts switch after 2 s); pid reuse (same pid, new start time = dead) |
| Debounce | foreground storm produces at most 4 forced ticks per second; CandidatesChanged ≤ 1/s |
| Manual add | stale key → NotFound; blocklisted → Refused; non-primary member redirected to root; comments preserved in `toml_edit` round-trip |
| Config | invalid allow entry skipped with warning; ranges clamped and reported; mode=off stops Active |

### 10.2 Integration tests with fake probe (in-process)
Scripted `Snapshot` sequences drive `Detector::tick` and assert the exact event sequence. Scenarios: normal start/stop, relaunch within grace, relaunch after grace (new GameStarted), target window replaced, two games, launcher stub, Resume during Pending, Shutdown during Active.

### 10.3 Windows integration tests (real APIs, CI on Windows runner)
Fixture binaries under `crates/detect/tests/fixtures/` (built by a `[[bin]]` in a dev-only helper crate):

| Fixture | Behavior |
|---|---|
| `fake_game.exe` | Creates a borderless fullscreen window 1920×1080, loads `d3d11.dll` via `D3D11CreateDevice`, renders a spinning clear color (GPU 3D > 0). Accepts `--exit-after`, `--windowed`, `--title`. |
| `fake_launcher.exe` | Shows a 800×600 window, spawns `fake_game.exe` as a child, exits after 2 s (stub pattern). |
| `fake_relaunch.exe` | Crashes after N s; the test harness starts a second instance within `stop_grace_ms`. |
| `fake_browser.exe` | Named `chrome.exe` in a temp dir; fullscreen, GPU active; must never be a candidate. |

Tests:
- `fake_game` starts → `GameStarted` within `start_confirm_ms + 2 s`; exit → `GameStopped{ProcessExited}` within `stop_grace_ms + 2 s`.
- `fake_launcher` → only `fake_game` appears as target; `GameStopped` not emitted when launcher exits.
- `fake_relaunch` restart within grace → single session with `GameRelaunched`.
- `chrome.exe` fixture fullscreen → zero events.
- Windowed `fake_game --windowed` (not known) → no start; after `allow` added via manual add → start.
- Exclusive fullscreen: a fixture using `IDXGIOutput::SetFullscreenState(TRUE)` must be classified as Geometry; the test asserts detection reports it (capture is tested elsewhere).

### 10.4 Library fixtures
Committed sample trees: `fixtures/steam/` (libraryfolders.vdf, 3 ACFs, `common/<dirs>` with dummy exes), `fixtures/epic/*.item`, `fixtures/xbox/XboxGames/<title>/Content/MicrosoftGame.config`. GOG uses an injected `RegistryReader` trait with a fake map.

### 10.5 Performance tests
| Test | Pass criterion |
|---|---|
| Idle soak, 30 min, no game, desktop with ~60 windows | Service CPU average < 0.3 % of one core; no growth in RSS over 30 min (± 1 MB) |
| Active soak, 60 min, fake_game running | Detector CPU < 0.3 %; no `GameStarted`/`GameStopped` flapping |
| Snapshot cost | p99 tick < 15 ms with 300 processes and 120 windows |
| Library cold scan | < 2 s with 300 Steam manifests (fixture generator) |

### 10.6 Real-game validation matrix (manual, before v1 release)
Run each, record: started within 5 s? exited and stopped within 12 s? relaunched within grace OK? Not blocked as "launcher"?

| Game | Type | Expected |
|---|---|---|
| A Steam borderless game (e.g., Celeste) | Known/library, borderless | Start, stop |
| A Steam exclusive-fullscreen game | Exclusive | Start (detected), capture warning (E1) |
| An Epic game with launcher stub | Stub | Start on game window, one session |
| A GOG game | Library | Start, stop |
| An Xbox/Game Pass UWP game | UWP child window | Start (E9) |
| A UE5 game with `Shipping.exe` child | Grouping | One GameId, no TargetChanged flapping |
| An anti-cheat title (e.g., a Valorant/Fortnite-class title) | Anti-cheat | Start via geometry/GPU/known; no module probe; no anti-cheat warnings |
| YouTube fullscreen in Chrome and Edge | Browser | Zero events |
| Discord screen share fullscreen | Blocklisted | Zero events |
| Windowed unknown game | Windowed | No auto-start; manual add works |

---

## 11. Implementation task breakdown

Ordered; each task is roughly 0.5–2 days and ends with passing tests.

| # | Task | Output | Est. |
|---|---|---|---|
| T1 | Core types: `ExeKey`, `Store`, `StoreRef`, `GameId`, `Hwnd`, `CaptureTarget`; `DetectorConfig` + serde with defaults and range clamping | `core` types, config fragment, unit tests | 1 d |
| T2 | `probe::win`: Toolhelp process list (pid, parent, exe name, start time), `QueryFullProcessImageName`, EnumWindows with DWM cloak, styles, owner, monitor rect, DPI | `SystemProbe` real impl (no PDH yet) | 2 d |
| T3 | `classify`: gates G1–G8, group tree, library-less scoring, `Verdict` | pure module + table tests | 1.5 d |
| T4 | `state`: state machine, debounce, liveness, hysteresis, events | `Detector::tick` + scripted integration tests | 1.5 d |
| T5 | `lists` + `known_games.toml` loader; built-in blocklist, launchers, anti-cheat names; `block_extra` merge | data files and tests | 0.5 d |
| T6 | `probe::gpu`: PDH English counters, per-pid 3D sum, rolling average, fallback | GPU signal + fallback tests on a Windows runner | 1 d |
| T7 | `probe::modules`: Toolhelp module snapshot, anti-cheat guard, denial cache, 1/30 s cap | module probe + tests | 1 d |
| T8 | Library Steam: registry path, VDF parser, ACF parser, exe index | `SteamSource` + fixtures | 1 d |
| T9 | Library Epic + GOG (GOG via `RegistryReader` trait) | `EpicSource`, `GogSource` + fixtures | 1 d |
| T10 | Library Xbox + UWP child-window resolution (5.7) | `XboxSource`; `probe` UWP handling; fixtures | 1.5 d |
| T11 | Manual add: validation, `toml_edit` write preserving comments, remove rule, stale key handling | `manual.rs` + round-trip tests | 1 d |
| T12 | `clipcheckd` wiring: detector thread, WinEvent foreground hook, scheduler, config reload, resume/shutdown, fan-out to session and IPC | service integration; `DetectEvent` → IPC mapping | 1.5 d |
| T13 | Test fixtures (`fake_game`, `fake_launcher`, `fake_relaunch`, `fake_browser`) and Windows integration tests (10.3) | fixture binaries + CI job | 1.5 d |
| T14 | Perf soak harness and CPU/RSS measurement (10.5); tune cadence | perf report, adjusted defaults | 1 d |
| T15 | Real-game validation matrix (10.6) and fixes; document E1 warning path with `capture` | validation log in `docs/design/` (only if requested) | 1–2 d |

Dependencies: T1 → T2/T3/T5 → T4 → T12. T6/T7 can run in parallel with T8–T10. T11 after T4 and T12 partially. T13 after T12.

---

## 12. Open questions

1. **Exclusive fullscreen detection.** Window APIs cannot distinguish exclusive from borderless. Proposal: treat as Geometry (as in this doc) and rely on capture-side failure for the warning (E1). Alternative: DXGI `IDXGISwapChain::GetFullscreenState` is only available to the owning process, so it is not viable without hooking. Confirm the proposal is acceptable for v1.
2. **Should a known windowed game auto-start without GPU evidence?** Changed: yes, when it has a visible window of at least 720p with a real title and no penalty (5.11 known-window rule). Confirm this; the alternative is a stricter rule (fullscreen or GPU evidence) that would not detect windowed FiveM. Game-launcher menus with a real title are the known risk.
3. **Module probe at all?** It uses `CreateToolhelp32Snapshot` with `PROCESS_VM_READ` rights, which some anti-cheat stacks may flag even though it is read-only. The GPU and geometry signals cover most cases. Proposal: keep the probe as an opt-in default-on only for non-anti-cheat titles; reconsider if testing shows any anti-cheat complaint. Please confirm.
4. **ETW process start events vs timer polling.** ETW (`Microsoft-Windows-Kernel-Process`) would give instant process start/stop but may require elevation or specific privileges. v1 uses timer polling with a foreground wake. Revisit if idle CPU misses the 0.3 % target.
5. **Xbox / Game Pass coverage.** The `XboxGames` folder scan misses some Microsoft Store packaged titles installed elsewhere. Options: `Get-AppxPackage`-equivalent enumeration via `Windows.Management.Deployment` (heavier, fine to call only on library scan), or manual add. Proposal: manual add only for v1 for store-packaged titles outside `XboxGames`.
6. **Additional stores (Ubisoft Connect, EA app, Battle.net, Riot).** Not scanned in v1; known DB entries and manual add cover popular titles. Confirm scope, or allow the known DB to carry install-dir hints.
7. **Elevated games.** Recommendation: run `clipcheckd` unelevated and surface "elevated game, cannot auto-detect" in the UI (E7). Confirm we do not want an elevated helper process (adds attack surface and service complexity).
8. **Known-games DB size and curation.** Shipping a curated list of a few hundred entries in the binary is proposed. Ownership and update process (app release only) needs a decision, since the product is local-only.
9. **Brief conflict check.** The brief says `GameId` = normalized exe name + optional store id. This doc keeps exactly that and adds `StoreRef` as the optional part. No conflict. The brief's "fullscreen/borderless" focus is consistent with the windowed-unknown-game non-goal in E17, which is a product-visible limitation to document in the UI and help text.
10. **Session relaunch split threshold.** Section 7.1 defers `relaunch_split_after_ms` to the session doc; the brief does not specify it. Needs an entry in the session design.
