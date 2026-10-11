# 09 — Settings and Config

| Field | Value |
|---|---|
| Status | Draft v1 (canonical config schema) |
| Owns | `config.toml` schema, load/validate/save/reload, settings UI, import/export |
| Depends on | `00-brief.md` (all sections) |
| Consumed by | Every feature doc. Feature docs must not add config keys that are missing here; they request a change to this doc instead. |

This document is the **single canonical schema** for `%APPDATA%\ClipCheck\config.toml`. Where a feature doc describes behavior that a key controls, that doc refers to the key by its path here (for example `video.fps`). Keys were derived from the features described in the brief. Where a brief decision is ambiguous, it is listed in Section 12.

---

## 1. Goal & scope

### 1.1 Goal
Give users one human-editable, comment-preserving TOML file plus a Settings UI that edits the same file. Both must produce identical results. The service (`clipcheckd`) is the only writer and the only applier of changes. The UI reads and writes through IPC.

### 1.2 In scope
- Full schema for all v1 settings: general, storage, detection, video, audio, replay buffer, clip hotkeys, session hotkeys, notifications, advanced, and per-game overrides.
- Versioning (`config_version`) and forward migrations.
- Parse, type check, range check, and cross-field validation with stable diagnostic codes and human messages.
- Hot-reload classification for every key (live, next part, pipeline reinit, audio reinit, replay flush, restart).
- Settings UI tab layout, control types, and apply badges.
- Import and export of the config file (with machine-specific keys optionally stripped).
- Atomic save, backup, and the single-writer rule.

### 1.3 Out of scope
- Encoder calibration values (CQ-to-quality mapping) belong to `encode/`. This doc only fixes the preset names and the placeholder CQ numbers used as defaults.
- Device enumeration UI for audio endpoints belongs to `audio/`. This doc only stores the chosen device ID.
- Library settings (thumbnail size, sort order) are UI state in SQLite, not in `config.toml`.
- Cloud sync, accounts, telemetry: not in the product.
- Per-game codec and encoder overrides (Section 12, Q2).

---

## 2. User-facing behavior

### 2.1 Where settings live
- `%APPDATA%\ClipCheck\config.toml` is the source of truth.
- The Settings UI is the primary editor. Advanced users may edit the file directly. The service picks up external edits within about 300 ms (debounced file watch).
- A "Open config file" button in Advanced opens the file in the default editor.

### 2.2 Settings page layout

```
┌ Settings ───────────────────────────────────────────────────────────────────────┐
│ General │ Storage │ Detection │ Video │ Audio │ Replay │ Hotkeys │ Notifications │
│ Games │ Advanced                                                                │
├─────────────────────────────────────────────────────────────────────────────────┤
│ Recording root      [C:\Users\mitch\Videos\ClipCheck            ] [Browse...]   │
│                     [x] Create a folder per game                   (applies: next part)
│ Filename template   [{game} {yyyy-MM-dd HH-mm-ss} {kind}        ]              │
│                     Preview: Cyberpunk 2077 2026-10-09 21-14-03 clip-30s        │
│ Quota               [ 0 ] GB   (0 = unlimited)                     (applies: live)
│ When quota is full  (•) Warn only  ( ) Delete oldest clips  ( ) Stop recording  │
│ Delete to Recycle Bin [x]                                                       │
├─────────────────────────────────────────────────────────────────────────────────┤
│ [Import...] [Export...] [Reset this tab]         Changes: 2 unsaved  [Revert] [Apply] │
└─────────────────────────────────────────────────────────────────────────────────┘
```

Tab content summary:

| Tab | Controls |
|---|---|
| General | Start with Windows (toggle), start minimized (toggle), recording badge on tray icon (toggle), language (dropdown: System, English) |
| Storage | Recording root (path + Browse), per-game folders, quota (number GB + action radio), filename template (text + live preview), delete to Recycle Bin |
| Detection | Mode radio (Auto-detect games / Only listed games), allow list and block list (editable chip lists with "Add running process" picker), auto-record toggle, after-manual-stop radio, stop grace seconds |
| Video | Encoder dropdown (Auto, NVENC, AMF, QSV, Software) with live "detected on this PC" hint, codec, container, preset (Low, Medium, High, Ultra, Custom), resolution, fps, cursor toggle. When preset = Custom, a rate panel appears: rate control, CQ slider or bitrate fields, max bitrate |
| Audio | Codec, bitrate, mic device dropdown, output device dropdown, track table (see 2.4) |
| Replay | Enabled toggle, max length (number + unit selector: seconds or minutes), storage (Disk / RAM), location (path + Browse), RAM size, estimated memory or disk usage line |
| Hotkeys | Clip hotkey list (name, key capture, seconds), session hotkeys (key capture each), mute mic mode |
| Notifications | Master toggle, per-event toggles, low disk threshold, sound toggle |
| Games | List of known games (from library and running processes). Click a game to edit its overrides. Inherited values are shown greyed with "from global" |
| Advanced | Log level, open log folder, detection poll interval, IPC timeout, encoder probe cache toggle, "Re-probe encoders" button, extra encoder args (allowlisted), debug overlay toggle, "Open config file" |

### 2.3 Apply badges
Every control shows a badge that matches its apply class (Section 5.4):

| Badge | Meaning |
|---|---|
| (none) | Applies live |
| `applies: next part` | Takes effect when the next session part file starts |
| `applies: next keyframe` | Pipeline (capture or encoder) rebuilds at the next keyframe. A session part split and a replay epoch boundary occur. No frames are dropped |
| `applies: audio restart` | Audio graph rebuilds. Up to about 100 ms of audio gap on affected tracks |
| `clears replay buffer` | Switching replay storage clears the ring. The UI asks for confirmation |
| `restart required` | Takes effect after `clipcheckd` restarts. The UI offers "Restart recorder now", which is refused during an active recording unless the user confirms and a session split is performed |

### 2.4 Audio track editor
- Table with columns: `#`, Name, Sources, Gain (dB), Enabled, Bitrate override.
- Default rows: Mix, Game only, Microphone, Chat / Discord.
- "Add track" is disabled at 8 tracks. Each track has a source picker with these choices: Game, All apps, All except game, Microphone, Specific app (picks a running process or types an exe name).
- A track may combine several sources. Sources are summed.
- Reordering rows renumbers tracks. Track numbers appear in the Gallery and in the editor's audio selector.

### 2.5 Hotkey capture
- Click a key field, press a chord, release. Esc clears the binding. Backspace clears too.
- Modifiers accepted: Ctrl, Alt, Shift, Win. One non-modifier key is required.
- Conflicts are checked against this config and reserved keys before save. Conflicts with other applications are only detected at registration time (see edge case 9).

### 2.6 Per-game overrides
- Any game known to the library gets an entry on the Games tab.
- An override only stores fields the user changed. Unchanged fields are not written to the file.
- Overridable keys are listed in Section 6.3 (the `[games.<id>]` table). Anything else is global only.
- Deleting an override removes its table from the file. "Reset game" removes all its keys.

### 2.7 Import and export
- **Export**: writes a TOML copy to a path the user picks. Options:
  - Include per-game overrides (default on).
  - Strip machine-specific keys (default on). Stripped keys: `storage.root`, `replay.location`, `audio.mic_device`, `audio.output_device`, `advanced.log_dir`, `games.*.folder`. Stripped keys are removed, not blanked, so importing falls back to defaults.
  - A header comment is added: `# Exported by ClipCheck <version>, config_version = 1`. No user name, no machine name, no timestamp beyond the export date.
- **Import**: parses the chosen file, runs migrations, validates. If there are errors, the import is refused and all errors are listed. If there are only warnings, the diff dialog is shown.
- The diff dialog lists each changed key with old value, new value, and apply badge. The user confirms or cancels.
- On confirm, the service backs up the current file to `config.toml.pre-import-<yyyyMMdd-HHmmss>.bak` and then applies the new file atomically.
- Import never silently merges. It replaces the whole file. Per-tab "Reset this tab" is the partial reset.

### 2.8 Validation feedback
- Field-level errors appear inline next to the control, in red, with the diagnostic code in small text (for example `CFG-V004`).
- Save and Apply are disabled while any error is present. Warnings do not block.
- The file view (Advanced > Open config file) shows errors with line and column.

---

## 3. Architecture & data flow

```
                     ┌───────────────────────────── apps/ui (Tauri) ──────────────────────────┐
 Settings page ──▶   │ src-tauri: uses core::config for parse/validate/diff/export (no writes) │
                     └───────────┬─────────────────────────────────────────┬──────────────────┘
                                 │ named pipe (IPC)                        │ (service down: direct
                                 │ GetConfig / PatchConfig / ReplaceConfig │  write under config.lock)
                                 ▼                                         ▼
 ┌──────────────────────────── apps/clipcheckd ─────────────────────────────────────────────┐
 │  ConfigStore                                                                             │
 │   ├─ current: ArcSwap<Config>            (readers take Arc<Config> snapshots, lock-free) │
 │   ├─ path, last_hash, DocumentMut cache                                                  │
 │   ├─ FileWatcher (ReadDirectoryChangesW on %APPDATA%\ClipCheck, 300 ms debounce)         │
 │   └─ ChangeDispatcher ──▶ detect | capture | encode | audio | buffer | session | hotkeys │
 │                          | notifications | logging                                       │
 └──────────────────────────────────────────────────────────────────────────────────────────┘
          ▲ reads                                                 │ events: ConfigChanged, ConfigRejected
          └──────────────────────── core::config (crate `core`, module `config`) ─────────────┘
```

Crate ownership:

| Concern | Location |
|---|---|
| Types, defaults, serde | `crates/core/src/config/schema.rs` |
| Parse, migrate, validate, diff, export | `crates/core/src/config/{load,migrate,validate,diff,export}.rs` |
| Comment-preserving patch and atomic save | `crates/core/src/config/edit.rs` (uses `toml_edit`) |
| Store, watcher, reload dispatch | `apps/clipcheckd/src/config_store.rs` |
| IPC handlers | `apps/clipcheckd/src/ipc/config.rs` |
| Settings UI | `apps/ui/web/src/settings/` (Svelte), `apps/ui/src-tauri/src/settings_cmd.rs` |

Dependencies: `serde`, `toml_edit` (edit and round-trip), `toml` (typed read), `arc-swap`, `windows` (paths, `ReplaceFileW`, `LockFileEx`, `ReadDirectoryChangesW`, Recycle Bin via `SHFileOperationW`).

---

## 4. Key types, traits and function signatures

### 4.1 Schema tree (serde)

Every struct uses `#[serde(default)]`, so any missing key takes its default. Unknown keys are not rejected anywhere (top level, every section, and each `[games."<id>"]` table with its `video` sub-table): they are dropped on load and reported as one warning CFG-P004 per key (`Config::from_toml_str_checked`). The loader finds them by comparing the file with the config written back after parsing, not through `serde_ignored`; the legacy `audio.mic_device` key is read and does not warn. Non-zero defaults are given by `impl Default` (the table in 6.2 is authoritative; a golden test enforces it).

```rust
// crates/core/src/config/schema.rs
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub const CONFIG_VERSION: u32 = 1;
pub const MAX_FILE_BYTES: u64 = 1 << 20; // 1 MiB

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub config_version: u32,
    pub general: General,
    pub storage: Storage,
    pub detection: Detection,
    pub video: Video,
    pub audio: Audio,
    pub replay: Replay,
    pub hotkeys: Hotkeys,
    pub notifications: Notifications,
    pub advanced: Advanced,
    /// Key: canonical GameId string, e.g. "cs2.exe" or "cs2.exe@steam:730".
    pub games: BTreeMap<String, GameOverride>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub autostart: bool,            // false
    pub start_minimized: bool,      // true
    pub tray_badge_recording: bool, // true
    pub locale: String,             // "system"
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Storage {
    pub root: PathBuf,              // %USERPROFILE%\Videos\ClipCheck
    pub per_game_folders: bool,     // true
    pub quota_gb: u32,              // 0 = unlimited
    pub quota_action: QuotaAction,  // Warn
    pub filename_template: String,  // "{game} {yyyy-MM-dd HH-mm-ss} {kind}"
    pub delete_to_recycle_bin: bool,// true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detection {
    pub mode: DetectionMode,            // Auto
    pub allow: Vec<ExeName>,            // []
    pub block: Vec<ExeName>,            // []
    pub auto_record: bool,              // true
    pub after_manual_stop: AfterManualStop, // UntilNextLaunch
    pub stop_grace_s: u32,              // 10
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    pub encoder: EncoderChoice,     // Auto
    pub codec: Codec,               // H264
    pub container: Container,       // Mp4
    pub preset: Preset,             // High
    pub resolution: Resolution,     // Native
    pub fps: Fps,                   // F60
    pub rate_control: RateControl,  // Quality
    pub quality_cq: u8,             // 23   (custom + quality)
    pub bitrate_kbps: u32,          // 12000 (custom + cbr/vbr)
    pub max_bitrate_kbps: u32,      // 15000 (custom + vbr)
    pub keyframe_interval_s: u8,    // 2 (also replay segment length)
    pub cursor: bool,               // true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub codec: AudioCodec,          // Aac
    pub bitrate_kbps: u32,          // 192
    pub mic_device: String,         // "default"
    pub output_device: String,      // "default"
    /// Replaces the default track list entirely when present in the file.
    pub tracks: Vec<Track>,         // 4 default tracks, see 6.2
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub name: String,               // 1..=32 chars
    pub enabled: bool,              // true
    pub sources: Vec<Source>,       // 1..=16 entries
    pub gain_db: f32,               // 0.0, range -30.0..=12.0
    pub bitrate_kbps: Option<u32>,  // None = audio.bitrate_kbps
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replay {
    pub enabled: bool,              // true
    pub max_seconds: u32,           // 300
    pub storage: ReplayStorage,     // Disk
    pub location: PathBuf,          // %LOCALAPPDATA%\ClipCheck\buffer
    pub ram_max_mb: u32,            // 512
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub clips: Vec<ClipHotkey>,     // see 6.2
    pub session: SessionHotkeys,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipHotkey {
    pub name: String,               // 1..=32 chars
    pub keys: KeyChord,             // e.g. "Alt+F9"
    pub seconds: u32,               // 5..=3600, must be <= replay.max_seconds
    pub press: Press,               // "tap" (default) or "long" (held LONG_PRESS_MS = 400 ms)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionHotkeys {
    #[serde(with = "opt_chord")] pub split: Option<KeyChord>,        // Alt+F11
    #[serde(with = "opt_chord")] pub stop_start: Option<KeyChord>,  // Alt+F12
    #[serde(with = "opt_chord")] pub toggle_buffer: Option<KeyChord>, // Alt+F8
    #[serde(with = "opt_chord")] pub mute_mic: Option<KeyChord>,    // Alt+F7
    pub mute_mic_mode: MuteMode,    // Toggle
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub enabled: bool,              // true
    pub clip_saved: bool,           // true
    pub session_split: bool,        // true
    pub recording_started: bool,    // false
    pub errors: bool,               // true
    pub low_disk: bool,             // true
    pub low_disk_gb: u32,           // 5
    pub sound: bool,                // false
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    pub log_level: LogLevel,        // Info
    pub log_dir: PathBuf,           // %LOCALAPPDATA%\ClipCheck\logs
    pub detect_poll_ms: u32,        // 1000
    pub ipc_timeout_ms: u32,        // 2000
    pub encoder_probe_cache: bool,  // true
    pub encoder_extra_args: Vec<String>, // []  (allowlisted)
    pub debug_overlay: bool,        // false
}

/// Per-game override. Every field is optional; None means "inherit global".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GameOverride {
    pub display_name: Option<String>,
    pub folder: Option<PathBuf>,
    pub auto_record: Option<bool>,
    pub replay_enabled: Option<bool>,
    pub replay_max_seconds: Option<u32>,
    pub video: Option<VideoOverride>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoOverride {
    pub preset: Option<Preset>,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub rate_control: Option<RateControl>,
    pub quality_cq: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    pub max_bitrate_kbps: Option<u32>,
}
```

Note: `GameOverride` and `VideoOverride` do not reject unknown keys. A misspelled key inside a game table is dropped and reported as warning CFG-P004 (`Config::from_toml_str_checked`), the same as an unknown key anywhere else in the file.

Retired keys (D63): `advanced.ffmpeg_dir` and `video.native_encoder` were removed with FFmpeg. Old files that still contain them load normally; the keys are dropped without a CFG-P004 warning and are not written back on the next save.

### 4.2 Enums and value types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMode { #[default] Auto, Allowlist }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AfterManualStop { #[default] UntilNextLaunch, UntilReenabled }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuotaAction { #[default] Warn, DeleteOldestClips, StopRecording }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EncoderChoice { #[default] Auto, Nvenc, Amf, Qsv, Software }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Codec { #[default] H264, Hevc, Av1 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Container { #[default] Mp4, Mkv }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Preset { Low, Medium, #[default] High, Ultra, Custom }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Resolution { #[default] Native, P1440, P1080, P720 }   // "native", "1440p", "1080p", "720p"

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RateControl { #[default] Quality, Cbr, Vbr }

/// Serialized as the integer 30 | 60 | 120.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(try_from = "u16", into = "u16")]
pub enum Fps { F30, #[default] F60, F120 }
impl TryFrom<u16> for Fps { type Error = String; fn try_from(v: u16) -> Result<Self, String> { match v { 30 => Ok(Fps::F30), 60 => Ok(Fps::F60), 120 => Ok(Fps::F120), o => Err(format!("fps must be 30, 60 or 120, found {o}")) } } }
impl From<Fps> for u16 { fn from(f: Fps) -> u16 { match f { Fps::F30 => 30, Fps::F60 => 60, Fps::F120 => 120 } } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec { #[default] Aac, Opus }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStorage { #[default] Disk, Ram }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MuteMode { #[default] Toggle, PushToTalk }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel { Error, Warn, #[default] Info, Debug, Trace }
```

Serialized string forms for `Source`, `ExeName`, `GameId`, `KeyChord` use custom `Serialize`/`Deserialize` through `FromStr`:

```rust
/// "game" | "all_apps" | "all_except_game" | "mic" | "process:<name>.exe"
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source { Game, AllApps, AllExceptGame, Mic, Process(ExeName) }

/// Lowercase basename ending in ".exe". Example: "Discord.exe" -> "discord.exe".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExeName(String);

/// Canonical form: "<exe>" or "<exe>@<store>:<id>". Example: "cs2.exe@steam:730".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameId { pub exe: ExeName, pub store: Option<(String, String)> }

/// Parsed from "Ctrl+Alt+F9". Modifiers: Ctrl, Alt, Shift, Win. Exactly one non-modifier key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord { pub mods: Modifiers, pub vk: u16 }

/// serde adapter: "" means None (unbound).
mod opt_chord { /* serialize None as "", Some as chord string */ }
```

### 4.3 Store, diagnostics, classification

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity { Error, Warning, Info }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,         // "CFG-V004"
    pub severity: Severity,
    pub path: String,               // "video.quality_cq" or "games.\"cs2.exe\".video.fps"
    pub line: Option<u32>,          // 1-based, from toml_edit spans
    pub col: Option<u32>,
    pub message: String,            // human text, see 6.4
}

/// How a changed key reaches the running pipeline. See 5.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyClass { Live, NextPart, PipelineReinit, AudioReinit, ReplayFlush, Restart }

pub fn classify(path: &str) -> ApplyClass;              // single match, table in 6.2

pub struct KeyChange { pub path: String, pub class: ApplyClass }
pub struct ConfigChange { pub old: std::sync::Arc<Config>, pub new: std::sync::Arc<Config>, pub keys: Vec<KeyChange> }

pub trait ConfigListener: Send + Sync {
    /// Called on the dispatcher thread after the new snapshot is published.
    fn on_config_change(&self, change: &ConfigChange);
}

pub struct LoadOutcome {
    pub config: Config,             // always usable (defaults on total failure)
    pub diagnostics: Vec<Diagnostic>,
    pub read_only: bool,            // true when version too new or migration failed
    pub hash: u64,                  // FNV-1a 64 of the raw bytes, 0 if no file
}
```

### 4.4 Functions

```rust
// load.rs
pub fn load(path: &Path) -> LoadOutcome;                       // parse + migrate + validate
pub fn parse_text(text: &str) -> Result<(Config, DocumentMut, Vec<Diagnostic>), Vec<Diagnostic>>;

// migrate.rs
pub type MigrationFn = fn(&mut DocumentMut) -> Result<(), String>;
pub const MIGRATIONS: &[(u32, MigrationFn)] = &[];            // (from_version, step); none for v1
pub fn migrate(doc: &mut DocumentMut) -> Result<u32, MigrateError>; // returns final version

// validate.rs
pub fn validate(cfg: &Config, ctx: &ValidateCtx) -> Vec<Diagnostic>;
pub struct ValidateCtx { pub probe: Option<EncoderProbe>, pub mic_ids: Vec<String>, pub roots_writable: bool }
// Runtime checks (probe, devices, writability) run only in the service, not in the UI pre-check.

// resolve.rs
pub struct VideoSpec { pub encoder: EncoderChoice, pub codec: Codec, pub container: Container,
                       pub width: u32, pub height: u32, pub fps: u16, pub rate: RateSpec }
pub enum RateSpec { Quality { cq: u8 }, Cbr { kbps: u32 }, Vbr { kbps: u32, max_kbps: u32 } }
pub fn resolve_video(v: &Video, ov: Option<&VideoOverride>) -> VideoSpec;
pub struct Effective { pub display_name: String, pub folder: PathBuf, pub auto_record: bool,
                       pub replay_enabled: bool, pub replay_max_seconds: u32, pub video: VideoSpec }
pub fn effective_for(cfg: &Config, game: &GameId) -> Effective;

// edit.rs (comment-preserving)
pub struct PatchOp { pub op: PatchKind, pub path: String, pub value: Option<toml_edit::Item> }
pub enum PatchKind { Set, Unset }
pub fn apply_patch(doc: &mut DocumentMut, ops: &[PatchOp]) -> Result<(), String>;
pub fn save_atomic(path: &Path, text: &str) -> std::io::Result<u64 /* new hash */>;

// diff.rs
pub fn diff(old: &Config, new: &Config) -> Vec<KeyChange>;

// export.rs
pub struct ExportOptions { pub include_games: bool, pub strip_machine_keys: bool }
pub fn export(doc: &DocumentMut, opts: &ExportOptions) -> String;
```

### 4.5 ConfigStore (service side)

```rust
pub struct ConfigStore {
    current: arc_swap::ArcSwap<Config>,
    path: PathBuf,
    doc: parking_lot::Mutex<DocumentMut>,   // last good document, keeps comments
    hash: std::sync::atomic::AtomicU64,
    listeners: Vec<std::sync::Arc<dyn ConfigListener>>,
}

impl ConfigStore {
    pub fn open(path: PathBuf, listeners: Vec<Arc<dyn ConfigListener>>) -> (Self, Vec<Diagnostic>);
    pub fn snapshot(&self) -> Arc<Config>;
    pub fn text(&self) -> String;                                     // current file text
    pub fn validate_text(&self, text: &str) -> Vec<Diagnostic>;       // dry run, no write
    pub fn replace_text(&self, text: &str, base_hash: Option<u64>) -> Result<ConfigChange, Vec<Diagnostic>>;
    pub fn patch(&self, ops: &[PatchOp], base_hash: u64) -> Result<ConfigChange, Vec<Diagnostic>>;
    pub fn reload_from_disk(&self, reason: ReloadReason) -> Result<Option<ConfigChange>, Vec<Diagnostic>>;
}
pub enum ReloadReason { FileWatch, IpcReload, Startup }
```

Every write path (`replace_text`, `patch`) follows the same sequence: apply to a copy of the document, parse, migrate if needed, validate with runtime context, `save_atomic`, publish the new snapshot, dispatch the change. A write that fails validation changes nothing on disk.

---

## 5. Algorithms / state machines

### 5.1 Startup load

```
1. path = %APPDATA%\ClipCheck\config.toml
2. if file missing:
     write defaults (full commented file from 6.1) atomically; diagnostics = [CFG-P010 info "created"]
3. if size > 1 MiB: diagnostics += CFG-P002; use defaults in memory; read_only = true
4. bytes -> UTF-8 (strip BOM, remember BOM and CRLF style for save)
5. toml_edit::DocumentMut::parse(text)
     on error -> CFG-P001 with line:col; use last good snapshot if one exists, else defaults; read_only = true
6. read config_version (default 1 if absent, with info CFG-P011)
     if > CONFIG_VERSION: CFG-P003; read_only = true; deserialize best-effort, no migration
     if < CONFIG_VERSION: backup file to config.toml.v{from}.bak; run MIGRATIONS in order
       on migration error: restore backup content in memory, use defaults, read_only = true, CFG-P012
7. serde deserialize into Config with serde_ignored; collect unknown paths -> CFG-P004 (warning)
8. expand %VARS% in path fields; unset var -> CFG-P006 (error on that field, uses default)
9. validate(cfg, ctx) with runtime context (probe may be None at this point; runtime checks deferred)
10. errors present (not read-only) -> keep defaults for the failing fields only, not the whole file
11. publish snapshot; hash = fnv1a(bytes)
```

Per-field fallback in step 10: a bad `video.fps` falls back to `F60` with an error that stays visible. Other valid fields still apply. The alternative (reject the whole file) was rejected because one typo should not stop recording.

A field with an error stays reported as an error on the Settings page, and the recorder runs with the fallback value for that field. Save from the UI is blocked until the field is fixed.

### 5.2 Live reload (file watch)

```
FileWatcher (ReadDirectoryChangesW on %APPDATA%\ClipCheck, filter config.toml):
  on any change event -> restart 300 ms debounce timer
  timer fires:
    read bytes (retry up to 3 times, 50 ms apart, if the sharing violation error occurs:
                 some editors write via a temp file and rename)
    h = fnv1a(bytes)
    if h == store.hash: return                       # our own write, or no real change
    result = store.reload_from_disk(FileWatch)
      Ok(Some(change)) -> dispatch(change); emit ConfigChanged(source=external)
      Ok(None)         -> nothing
      Err(diags)       -> keep old snapshot; emit ConfigRejected(diags); tray notification (errors)
```

Self-write suppression is by hash: the service stores the hash of the bytes it just wrote, so the watcher event it causes is ignored.

### 5.3 Write from UI (PatchConfig / ReplaceConfig)

```
handle PatchConfig { ops, base_hash }:
  if base_hash != store.hash: return Conflict { current_text }
     (UI shows "file changed on disk": Reload, or Overwrite, which sends base_hash = current hash)
  doc = clone(store.doc)
  apply_patch(doc, ops)                           # comments and key order preserved
  text = doc.to_string()
  cfg = parse + migrate(no-op at current version) + validate(cfg, runtime ctx)
  if errors: return Rejected { diagnostics }     # nothing written
  lock config.lock (LockFileEx, exclusive, 2 s timeout)
  save_atomic(path, text)                         # see 5.6
  unlock
  store.publish(cfg, doc, new_hash)
  change = diff(old, new) with classes
  dispatch(change)
  return Ok { change }
```

`ReplaceConfig` (import) is the same, except the new text replaces the document entirely and backups use the `.pre-import-` name.

### 5.4 Apply dispatch

| Class | Meaning | Who reacts | Frame / audio impact |
|---|---|---|---|
| `Live` | Changes read from the snapshot on next use | Detector, hotkey registrar, notifier, logger, quota monitor | None |
| `NextPart` | Path, filename or container for a new session part file | Session writer (on next part open) | None; applies when a part starts |
| `PipelineReinit` | Capture or encoder or audio encoder rebuilt | Session, buffer, encode | Pipeline is rebuilt at the next keyframe. No frames dropped. Session split and replay epoch boundary forced. Replay clips do not span the boundary |
| `AudioReinit` | WASAPI graph rebuilt | Audio | Up to about 100 ms audio gap on affected tracks. Video continues |
| `ReplayFlush` | Replay ring cleared and recreated | Buffer | Buffered footage lost. UI requires confirmation first |
| `Restart` | Process restart needed | Service supervisor (only on user request) | Requires a session stop-and-restart. Refused while recording unless the user confirms |

Classification is per key path. When a change touches several keys, the strongest class wins in this order: `Restart` > `ReplayFlush` > `PipelineReinit` > `AudioReinit` > `NextPart` > `Live`.

Track count changes count as `PipelineReinit` (the number of audio streams is part of the container header).

### 5.5 Per-game resolution

```
effective_for(cfg, game):
  ov = cfg.games.get(game.canonical()) or cfg.games.get(game.exe_only()) (store-specific match wins)
  display_name = ov.display_name or exe stem (title-cased by library)
  folder = ov.folder or (cfg.storage.root / sanitize(display_name)  if per_game_folders else cfg.storage.root)
  auto_record = ov.auto_record or cfg.detection.auto_record
  replay_enabled = ov.replay_enabled or cfg.replay.enabled
  replay_max_seconds = ov.replay_max_seconds or cfg.replay.max_seconds
  video = resolve_video(cfg.video, ov.video)
  re-run per-game validation on the merged result (CFG-G002)
```

Override merge rule: a field present in the game table replaces the global value for that game only. Nothing is merged inside arrays. There are no per-game audio tracks in v1.

### 5.6 Atomic save

```
save_atomic(path, text):
  tmp = path + ".tmp"
  write tmp with the original BOM/CRLF style
  FlushFileBuffers(tmp)
  if path exists: ReplaceFileW(path, tmp, backup = path + ".bak")    # keeps one backup
  else: MoveFileExW(tmp, path, MOVEFILE_WRITE_THROUGH)
  return fnv1a(text)
```

`config.toml.bak` is the single rolling backup of the previous saved version. Migration backups use `config.toml.v{n}.bak` and import backups use `config.toml.pre-import-<ts>.bak`. A cleanup keeps at most 5 of each kind.

### 5.7 Migration

```
migrate(doc):
  v = doc["config_version"] or 1
  while v < CONFIG_VERSION:
     step = MIGRATIONS.find(from == v) else error CFG-P013 "no migration from v{v}"
     step(doc)?
     v += 1
     doc["config_version"] = v
  return v
```

Rules:
- Migrations work on the `DocumentMut` so comments survive.
- Each step is pure: it renames, moves, or defaults keys. It never reads the machine.
- Migrated files are written back on the next save, not at load time. A load never writes to a file except to create a missing file.
- v1 is the first released schema, so `MIGRATIONS` is empty. The v0 pre-release files (no `config_version` key) are treated as v1 with CFG-P011 info.

Illustrative future example (not in v1): a v2 that renames `video.quality_cq` to `video.cq` would add `(1, migrate_v1_to_v2)` where the function moves the key and leaves a comment in place.

### 5.8 Import and export flow

```
Export:
  text = store.text()                                     # from the service, or the file when offline
  doc = DocumentMut::parse(text)
  out = export(doc, opts)                                 # strip machine keys, drop [games] if unchecked
  save via native file dialog; write file (user-chosen path, no atomic rename needed)

Import:
  bytes = read(chosen file); reject if > 1 MiB
  (cfg, doc, diags) = parse_text(text) + migrate
  if any Error: show list; stop
  change = diff(store.snapshot(), cfg) with classes
  show diff dialog; on Confirm -> IPC ReplaceConfig { toml: text, base_hash: store.hash }
  service re-validates (runtime checks now apply), writes backup, saves, publishes, dispatches
```

Reset tab: builds a `PatchOp::Unset` for each key in that tab, then sends `PatchConfig`. Unset keys are removed from the file, so defaults apply.

### 5.9 Validation pipeline

Validation runs in three passes. Each pass collects all diagnostics it finds; no pass stops early.

```
Pass A (parse/shape):   TOML syntax, types, unknown keys, file size, version         -> CFG-P*
Pass B (static rules):  ranges, enums, cross-field, string grammars                   -> CFG-S*, D*, V*, A*, R*, H*, N*, X*, G*
Pass C (runtime ctx):   encoder availability, mic device exists, path writable,       -> CFG-V0xx runtime, A009/A010, R004, S002, H006, X002
                        RegisterHotKey success
```

Pass A and B run in the UI pre-check and in the service. Pass C runs only in the service, and its results are warnings or runtime errors that do not reject a save. Exception: an encoder that is forced by the user but missing is reported as a runtime error at record start, not at save time.

---

## 6. Config keys owned by this feature

### 6.1 Full example file (`%APPDATA%\ClipCheck\config.toml`)

This is the default file written on first run. It contains every v1 key with its default value. Commented lines show examples.

```toml
# ClipCheck configuration (config_version 1)
# Location: %APPDATA%\ClipCheck\config.toml
# Every key is optional. Omitted keys take the defaults shown here.
# The Settings UI writes this file. Hand edits are picked up within ~300 ms.
config_version = 1

[general]
autostart = false               # start clipcheckd with Windows (HKCU Run)
start_minimized = true          # launch the UI minimized to tray
tray_badge_recording = true     # show a red dot on the tray icon while recording
locale = "system"               # "system" | "en"

[storage]
root = '%USERPROFILE%\Videos\ClipCheck'   # recordings root; %VARS% expanded at load
per_game_folders = true         # root\<GameName>\ for each game
quota_gb = 0                    # 0 = unlimited; otherwise total size cap for recordings
quota_action = "warn"           # "warn" | "delete_oldest_clips" | "stop_recording"
filename_template = "{game} {yyyy-MM-dd HH-mm-ss} {kind}"
delete_to_recycle_bin = true    # deletions (quota, UI) go to the Recycle Bin

# Extra library folders: scanned and shown in the gallery, never moved. New recordings still
# go to root. At most 16 entries. Uncomment to add one.
# [[storage.libraries]]
# path = 'D:\Medal\Clips'         # absolute, or starts with %VAR%
# label = "Medal D"               # "" = show the folder name; 40 characters at most
# read_only = false               # true = never import into or delete from this folder
# enabled = true                  # false = keep the entry but do not scan it

[detection]
mode = "auto"                   # "auto" = any detected game; "allowlist" = only games in allow
allow = []                      # e.g. ["cs2.exe", "eldenring.exe"]
block = []                      # e.g. ["launcher.exe"]; never recorded
auto_record = true              # start a session when an eligible game is detected
after_manual_stop = "until_next_launch"   # or "until_reenabled"
stop_grace_s = 10               # keep recording this long after the game window closes

[video]
encoder = "auto"                # "auto" | "nvenc" | "amf" | "qsv" | "software"
codec = "h264"                  # "h264" | "hevc" | "av1" (av1 needs a hardware encoder)
container = "mp4"               # "mp4" | "mkv"  (opus audio needs "mkv")
preset = "high"                 # "low" (Low) | "medium" (Standard) | "high" | "ultra" | "custom"
resolution = "1080p"            # "native" | "360p" | "480p" | "720p" | "1080p" | "1440p" | "2160p"
fps = 60                        # 24 | 30 | 60 | 90 | 120 | 144
rate_control = "quality"        # "quality" | "cbr" | "vbr"   (only read when preset = "custom")
quality_cq = 23                 # 0..=51 h264/hevc, 0..=63 av1 (custom + quality)
bitrate_kbps = 14000            # 1000..=100000 (custom + cbr/vbr)
max_bitrate_kbps = 15000        # custom + vbr only, must be >= bitrate_kbps
keyframe_interval_s = 2         # 1..=4; also the replay segment length
cursor = true                   # draw the mouse cursor in captured frames
gpu = "auto"                    # "auto" or the adapter name as the GPU list shows it
encoder_speed = "balanced"      # "fastest" | "balanced" | "quality"
b_frames = 2                    # 0..=4

[audio]
codec = "aac"                   # "aac" | "opus" (opus is rejected in v1)
bitrate_kbps = 192              # 128..=320, per track
separate_tracks = true          # one extra track per enabled source, after the Mix track
output_device = "default"       # render endpoint for game and other-app capture

[audio.game]
enabled = true
gain_db = 0.0                   # -60.0..=12.0

[audio.mic]
enabled = true
device = "default"              # "default" or a capture endpoint id from the device picker
gain_db = 0.0                   # -60.0..=12.0
gate_enabled = false            # noise gate
gate_db = -50.0                 # -100.0..=-20.0

[audio.other_apps]
enabled = false                 # everything that is not the game or a listed app
gain_db = 0.0

# Apps recorded on their own. At most 16 entries.
[[audio.apps]]
exe = "discord.exe"
name = "Discord"
enabled = true
gain_db = 0.0

[replay]
enabled = true
max_seconds = 300               # 30..=3600 (5 minutes default, up to 60 minutes)
storage = "disk"                # "disk" | "ram"
location = '%LOCALAPPDATA%\ClipCheck\buffer'
ram_max_mb = 512                # used when storage = "ram"; 64..=4096

[[hotkeys.clips]]
name = "Clip 30s"
keys = "Alt+F9"
seconds = 30
press = "tap"                   # "tap" (default) or "long"

[[hotkeys.clips]]
name = "Clip 60s"
keys = "Alt+F10"
seconds = 60
press = "tap"

[hotkeys.session]               # empty string "" = unbound
split = "Alt+F11"
stop_start = "Alt+F12"
toggle_buffer = "Alt+F8"
mute_mic = "Alt+F7"
mute_mic_mode = "toggle"        # "toggle" | "push_to_talk"

[notifications]
enabled = true
clip_saved = true
session_split = true
recording_started = false
errors = true
low_disk = true
low_disk_gb = 5                 # 1..=100
sound = false

[advanced]
log_level = "info"              # "error" | "warn" | "info" | "debug" | "trace"
log_dir = '%LOCALAPPDATA%\ClipCheck\logs'
detect_poll_ms = 1000           # 250..=5000
ipc_timeout_ms = 2000           # 500..=10000
encoder_probe_cache = true
encoder_extra_args = []         # allowlisted options only
debug_overlay = false

[updates]
check = "notify"                # "never" (no background network) | "notify" (check on an interval)
channel = "stable"              # "stable"
interval_hours = 12             # 1..=168; used only when check = "notify"
endpoint = ""                   # "" = built-in manifest URL; https:// only (http://127.0.0.1 for tests)

# Per-game overrides. Only the fields you set are stored. Nothing is overridden by default.
# [games."cs2.exe"]
# display_name = "Counter-Strike 2"
# folder = 'D:\Clips\CS2'
# auto_record = true
# replay_enabled = true
# replay_max_seconds = 120
# [games."cs2.exe".video]
# preset = "ultra"
# resolution = "1080p"
# fps = 120
#
# Store-specific entries use the form "exe@store:id":
# [games."cs2.exe@steam:730"]
# display_name = "CS2 (Steam)"
```

### 6.2 Complete key table

Apply class: L = Live, NP = NextPart, PR = PipelineReinit, AR = AudioReinit, RF = ReplayFlush, R = Restart.

**general**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `general.autostart` | bool | `false` | | L (writes HKCU Run) |
| `general.start_minimized` | bool | `true` | | L |
| `general.tray_badge_recording` | bool | `true` | | L |
| `general.locale` | string | `"system"` | `system`, `en` | L |
| `general.close_behavior` | enum | `"minimize_to_tray"` | `minimize_to_tray` (hide to tray, keep running), `exit` (close and exit) | L |
| `general.confirm_exit_while_recording` | bool | `true` | | L |

**storage**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `storage.root` | path | `%USERPROFILE%\Videos\ClipCheck` | absolute after expansion | NP |
| `storage.per_game_folders` | bool | `true` | | NP |
| `storage.quota_gb` | u32 | `0` | 0 (unlimited) or 1..=100000 | L |
| `storage.quota_action` | enum | `warn` | `warn`, `delete_oldest_clips`, `stop_recording` | L |
| `storage.filename_template` | string | `{game} {yyyy-MM-dd HH-mm-ss} {kind}` | 1..=180 chars rendered; see CFG-S003..S005 | NP |
| `storage.delete_to_recycle_bin` | bool | `true` | | L |
| `storage.libraries` | array of tables | `[]` | at most 16 entries; codes CFG-S009..S014 | L |
| `storage.libraries[].path` | string | `""` | absolute (`D:\…`, `\\server\share\…`) or starts with `%VAR%`; unique after normalization | L |
| `storage.libraries[].label` | string | `""` | 0..=40 chars; empty shows the folder name | L |
| `storage.libraries[].read_only` | bool | `false` | | L |
| `storage.libraries[].enabled` | bool | `true` | | L |

**detection**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `detection.mode` | enum | `auto` | `auto`, `allowlist` | L |
| `detection.allow` | array<exe> | `[]` | valid exe names, no duplicates | L |
| `detection.block` | array<exe> | `[]` | valid exe names; must not overlap allow | L |
| `detection.auto_record` | bool | `true` | | L |
| `detection.record_sessions` | bool | `true` | false = replay buffer and clips only; no automatic full-session recording (manual start still works) | L |
| `detection.after_manual_stop` | enum | `until_next_launch` | `until_next_launch`, `until_reenabled` | L |
| `detection.stop_grace_s` | u32 | `10` | 0..=300 | L |

**video**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `video.encoder` | enum | `auto` | `auto`, `nvenc`, `amf`, `qsv`, `software` | PR |
| `video.codec` | enum | `h264` | `h264`, `hevc`, `av1`; av1 not with `software` | PR |
| `video.container` | enum | `mp4` | `mp4`, `mkv` | NP |
| `video.preset` | enum | `high` | `low` (Low), `medium` (Standard), `high`, `ultra`, `custom`. Non-custom presets fix resolution, fps and bitrate (see "Audio/Video model v2") | PR |
| `video.resolution` | enum | `1080p` | `native`, `360p`, `480p`, `720p`, `1080p`, `1440p`, `2160p` | PR |
| `video.fps` | u16 | `60` | `24`, `30`, `60`, `90`, `120`, `144` (CFG-V007) | PR |
| `video.gpu` | string | `"auto"` | `auto`, or the adapter name as the UI reports it; non-empty (CFG-V012) | PR |
| `video.encoder_speed` | enum | `balanced` | `fastest`, `balanced`, `quality` | PR |
| `video.b_frames` | u8 | `2` | 0..=4 (CFG-V013) | PR |
| `video.rate_control` | enum | `quality` | `quality`, `cbr`, `vbr` (custom only) | PR |
| `video.quality_cq` | u8 | `23` | 0..=51 (h264, hevc), 0..=63 (av1); custom + quality | PR |
| `video.bitrate_kbps` | u32 | `14000` | 1000..=100000; custom + cbr/vbr | PR |
| `video.max_bitrate_kbps` | u32 | `15000` | 1000..=100000, >= bitrate; custom + vbr | PR |
| `video.keyframe_interval_s` | u8 | `2` | 1..=4 | PR |
| `video.cursor` | bool | `true` | | PR |

**audio**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `audio.codec` | enum | `aac` | `aac`, `opus` (opus rejected in v1, CFG-A011) | AR |
| `audio.bitrate_kbps` | u32 | `192` | 128..=320, per track (CFG-A008) | AR |
| `audio.separate_tracks` | bool | `true` | one extra track per enabled source (see "Audio/Video model v2") | AR |
| `audio.output_device` | string | `"default"` | `default` or endpoint id | AR |
| `audio.game.enabled` | bool | `true` | | AR |
| `audio.game.gain_db` | f32 | `0.0` | -60.0..=12.0 (CFG-A006) | AR |
| `audio.mic.enabled` | bool | `true` | | AR |
| `audio.mic.device` | string | `"default"` | `default` or capture endpoint id. Pre-v2 `audio.mic_device` is read into this key | AR |
| `audio.mic.gain_db` | f32 | `0.0` | -60.0..=12.0 (CFG-A006) | AR |
| `audio.mic.gate_enabled` | bool | `false` | | AR |
| `audio.mic.gate_db` | f32 | `-50.0` | -100.0..=-20.0 (CFG-A013) | AR |
| `audio.other_apps.enabled` | bool | `false` | | AR |
| `audio.other_apps.gain_db` | f32 | `0.0` | -60.0..=12.0 (CFG-A006) | AR |
| `audio.apps` | array<table> | Discord (`discord.exe`) | 0..=16 entries (CFG-A001) | AR |
| `audio.apps[].exe` | exe | per entry | valid basename ending in .exe; unique, case-insensitive (CFG-A012) | AR |
| `audio.apps[].name` | string | per entry | 1..=32 chars; unique, case-insensitive (CFG-A002, CFG-A003) | AR |
| `audio.apps[].enabled` | bool | `true` | | AR |
| `audio.apps[].gain_db` | f32 | `0.0` | -60.0..=12.0 (CFG-A006) | AR |

`[[audio.tracks]]` from the v1 model is ignored when a file is loaded. The defaults apply.

**replay**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `replay.enabled` | bool | `true` | | L |
| `replay.max_seconds` | u32 | `300` | 30..=3600 | L |
| `replay.storage` | enum | `disk` | `disk`, `ram` | RF |
| `replay.location` | path | `%LOCALAPPDATA%\ClipCheck\buffer` | absolute, writable (runtime) | L |
| `replay.ram_max_mb` | u32 | `512` | 64..=4096; must cover estimate (CFG-R002) | L |

**hotkeys**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `hotkeys.clips` | array<table> | 2 defaults (6.1) | 0..=8 entries | L |
| `hotkeys.clips[].name` | string | per entry | 1..=32 chars | L |
| `hotkeys.clips[].keys` | chord | per entry | valid chord, not reserved; shared only by one tap and one long clip | L |
| `hotkeys.clips[].seconds` | u32 | per entry | 5..=3600, <= replay.max_seconds | L |
| `hotkeys.clips[].press` | `"tap"` or `"long"` | `"tap"` | long = held 400 ms (LONG_PRESS_MS) | L |
| `hotkeys.session.split` | chord or "" | `Alt+F11` | | L |
| `hotkeys.session.stop_start` | chord or "" | `Alt+F12` | | L |
| `hotkeys.session.toggle_buffer` | chord or "" | `Alt+F8` | | L |
| `hotkeys.session.mute_mic` | chord or "" | `Alt+F7` | | L |
| `hotkeys.session.mute_mic_mode` | enum | `toggle` | `toggle`, `push_to_talk` (needs mute_mic set) | L |

**notifications**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `notifications.enabled` | bool | `true` | | L |
| `notifications.clip_saved` | bool | `true` | | L |
| `notifications.session_split` | bool | `true` | | L |
| `notifications.recording_started` | bool | `false` | | L |
| `notifications.errors` | bool | `true` | | L |
| `notifications.low_disk` | bool | `true` | | L |
| `notifications.low_disk_gb` | u32 | `5` | 1..=100 | L |
| `notifications.sound` | bool | `false` | | L |

**advanced**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `advanced.log_level` | enum | `info` | `error`, `warn`, `info`, `debug`, `trace` | L |
| `advanced.log_dir` | path | `%LOCALAPPDATA%\ClipCheck\logs` | absolute | R |
| `advanced.detect_poll_ms` | u32 | `1000` | 250..=5000 | L |
| `advanced.ipc_timeout_ms` | u32 | `2000` | 500..=10000 | L |
| `advanced.encoder_probe_cache` | bool | `true` | | L |
| `advanced.encoder_extra_args` | array<string> | `[]` | allowlist only (CFG-X004) | R |
| `advanced.debug_overlay` | bool | `false` | | L |

**updates** (section 15)

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `updates.check` | enum | `notify` | `never`, `notify` | L |
| `updates.channel` | enum | `stable` | `stable` | L |
| `updates.interval_hours` | u32 | `12` | 1..=168 (CFG-X006) | L |
| `updates.endpoint` | string | `""` (built-in URL) | empty, `https://...`, or `http://127.0.0.1` / `http://localhost` for tests; no spaces; 300 chars at most (CFG-X007) | L |

**games (overrides, all optional)**

| Key | Type | Default | Range / allowed | Apply |
|---|---|---|---|---|
| `games.<id>.display_name` | string | inherit | 1..=64 chars | L |
| `games.<id>.folder` | path | inherit | absolute | NP |
| `games.<id>.auto_record` | bool | inherit | | L |
| `games.<id>.replay_enabled` | bool | inherit | | L |
| `games.<id>.replay_max_seconds` | u32 | inherit | 30..=3600 | L |
| `games.<id>.video.preset` | enum | inherit | as `video.preset` | PR |
| `games.<id>.video.resolution` | enum | inherit | as `video.resolution` | PR |
| `games.<id>.video.fps` | u16 | inherit | 30, 60, 120 | PR |
| `games.<id>.video.rate_control` | enum | inherit | as `video.rate_control` | PR |
| `games.<id>.video.quality_cq` | u8 | inherit | as `video.quality_cq` | PR |
| `games.<id>.video.bitrate_kbps` | u32 | inherit | as `video.bitrate_kbps` | PR |
| `games.<id>.video.max_bitrate_kbps` | u32 | inherit | as `video.max_bitrate_kbps` | PR |

Per-game changes that alter the recorded game only apply when that game next starts a session. The class column shows what a running session does when the override changes during recording.


### 6.3 Per-game override rules
- `<id>` is a canonical GameId: `exe` or `exe@store:id`. Lookup prefers the store-specific entry, then the exe-only entry.
- Overrides cannot set encoder, codec, container, audio, replay storage, hotkeys, or notification settings. Those stay global in v1.
- An override with every field unset is removed on save so the file stays clean.

### 6.4 Validation rules and messages

Format for display: `config.toml:<line>:<col>: error CFG-XXXX: <message>`. Without a line (for example a runtime check), the path is shown instead.

| Code | Sev | Rule | Message (template) |
|---|---|---|---|
| CFG-P001 | E | TOML syntax | `TOML syntax error: {detail}` |
| CFG-P002 | E | file > 1 MiB | `Config file is {size} bytes; the limit is 1 MiB` |
| CFG-P003 | E | config_version > 1 | `config_version {n} is newer than this ClipCheck supports (max 1). The file is read-only until you update ClipCheck` |
| CFG-P004 | W | unknown key | `Unknown key '{path}' is ignored and kept in the file` |
| CFG-P005 | E | type mismatch | `'{path}' expects {expected}, found {found}` |
| CFG-P006 | E | unset env var in path | `'{path}' uses %{var}%, which is not set on this PC` |
| CFG-P010 | I | file created | `Created default config.toml` |
| CFG-P011 | I | version missing | `config_version missing; treated as 1` |
| CFG-P012 | E | migration failed | `Migration from v{from} failed: {detail}. Your backup is {backup}` |
| CFG-P013 | E | no migration step | `No migration from config_version {v}` |
| CFG-S001 | E | storage.root not absolute | `storage.root '{path}' must be an absolute path` |
| CFG-S002 | E (runtime) | root not creatable/writable | `Recording folder '{path}' is not writable: {os_error}. Choose another folder in Storage` |
| CFG-S003 | E | illegal char in template | `storage.filename_template contains '{ch}'. Windows file names cannot contain < > : " / \ \| ? *` |
| CFG-S004 | E | rendered name too long | `storage.filename_template renders to {len} characters; the limit is 180` |
| CFG-S005 | E | unknown token | `storage.filename_template has unknown token {{{tok}}}. Allowed: {game}, {kind}, {id} and date tokens yyyy MM dd HH mm ss` |
| CFG-S006 | E | quota out of range | `storage.quota_gb = {v}. Use 0 for unlimited or 1..=100000` |
| CFG-S007 | W | template without {kind} | `Without {kind}, session and clip files may look alike. Collisions are numbered (2), (3)` |
| CFG-S009 | E | library path empty | `storage.libraries[{i}].path is empty. Enter a folder or remove the entry` |
| CFG-S010 | E | library path not absolute | `storage.libraries[{i}].path '{path}' must be absolute, such as D:\Clips, \\server\share\Clips or %USERPROFILE%\Videos` |
| CFG-S011 | E | library duplicates another library or storage.root | `storage.libraries[{i}].path '{path}' is the same folder as storage.root` (or "already listed as library {j}") |
| CFG-S012 | E | more than 16 libraries | `storage.libraries has {n} entries. The maximum is 16` |
| CFG-S013 | E | label over 40 characters | `storage.libraries[{i}].label is {n} characters. The limit is 40` |
| CFG-S014 | W | library nested in another scanned folder | `'{inner}' is inside '{outer}'. Files in it could be indexed twice` |
| CFG-D001 | E | bad exe name | `'{x}' is not a valid exe name. Use a file name such as 'game.exe'` |
| CFG-D002 | E | exe in allow and block | `'{x}' is in both detection.allow and detection.block` |
| CFG-D003 | W | allowlist empty | `detection.mode = "allowlist" with no entries records nothing` |
| CFG-D004 | E | stop_grace_s range | `detection.stop_grace_s = {v} is outside 0..=300` |
| CFG-V001 | E | av1 with software | `video.codec = "av1" needs a hardware encoder. Set encoder to auto, nvenc, amf or qsv` |
| CFG-V004 | E | cq range | `video.quality_cq = {v} is outside 0..={max} for codec {codec}` |
| CFG-V005 | E | bitrate range | `video.bitrate_kbps = {v} is outside 1000..=100000` |
| CFG-V006 | E | max < bitrate | `video.max_bitrate_kbps ({max}) must be at least video.bitrate_kbps ({b})` |
| CFG-V007 | E | fps value | `video.fps = {v}. Allowed values are 30, 60 and 120` |
| CFG-V008 | E | keyframe range | `video.keyframe_interval_s = {v} is outside 1..=4` |
| CFG-V009 | W | ignored rate keys | `{keys} is ignored because video.preset = "{preset}". Set preset = "custom" to use them` |
| CFG-V010 | W | software 120 fps native | `Software encoding at 120 fps and native resolution may exceed the CPU budget and drop frames` |
| CFG-V011 | E (runtime) | forced encoder missing | `video.encoder = "{enc}" is not available on this PC. Choose auto or another encoder` |
| CFG-A001 | E | track count | `audio.tracks has {n} entries. Allowed: 1..=8` |
| CFG-A002 | E | track name | `audio.tracks[{i}].name must be 1..=32 characters` |
| CFG-A003 | E | duplicate name | `audio.tracks[{i}].name '{n}' is already used by track {j}` |
| CFG-A004 | E | no sources | `audio.tracks[{i}].sources is empty. Add at least one source` |
| CFG-A005 | E | bad source | `audio.tracks[{i}].sources[{k}] = "{s}" is not valid. Use game, all_apps, all_except_game, mic or process:<name>.exe` |
| CFG-A006 | E | gain range | `audio.tracks[{i}].gain_db = {v} is outside -30.0..=12.0` |
| CFG-A007 | E | track bitrate range | `audio.tracks[{i}].bitrate_kbps = {v} is outside 128..=320` |
| CFG-A008 | E | global bitrate range | `audio.bitrate_kbps = {v} is outside 128..=320` |
| CFG-A009 | W (runtime) | mic not found | `Microphone '{id}' is not connected. Track '{name}' is silent until it returns` |
| CFG-A010 | W (runtime) | mic track, no mic | `Track {i} uses the microphone but no capture device exists` |
| CFG-A011 | E | opus in mp4 | `audio.codec = "opus" requires video.container = "mkv"` |
| CFG-R001 | E | max_seconds range | `replay.max_seconds = {v} is outside 30..=3600` |
| CFG-R002 | E | ram too small | `replay.storage = "ram" needs about {need} MB for {s} s at {rate} Mbps. Raise replay.ram_max_mb to {need} or lower replay.max_seconds` |
| CFG-R003 | E | ram_max_mb range | `replay.ram_max_mb = {v} is outside 64..=4096` |
| CFG-R004 | E (runtime) | buffer location | `Replay buffer folder '{path}' is not writable: {os_error}` |
| CFG-H001 | E | bad chord | `hotkeys: '{k}' is not a valid key chord. Example: Alt+F9. Use Ctrl, Alt, Shift or Win plus one key` |
| CFG-H002 | E | duplicate chord | `hotkeys: '{k}' is assigned to both {a} and {b}` |
| CFG-H003 | E | reserved chord | `hotkeys: '{k}' is reserved by Windows or ClipCheck (for example Alt+F4, Win+L, Ctrl+Alt+Del)` |
| CFG-H004 | E | seconds > replay max | `hotkeys.clips[{i}].seconds = {v} is more than replay.max_seconds = {m}. Lower seconds or raise replay.max_seconds` |
| CFG-H005 | E | too many clip hotkeys | `hotkeys.clips has {n} entries. The maximum is 8` |
| CFG-H006 | W (runtime) | chord in use elsewhere | `'{k}' is already used by another application. The {name} hotkey is inactive` |
| CFG-H007 | E | clip name | `hotkeys.clips[{i}].name must be 1..=32 characters` |
| CFG-H008 | E | ptt without key | `mute_mic_mode = "push_to_talk" needs hotkeys.session.mute_mic to be set` |
| CFG-N001 | E | low_disk_gb range | `notifications.low_disk_gb = {v} is outside 1..=100` |
| CFG-X001 | E | poll range | `advanced.detect_poll_ms = {v} is outside 250..=5000` |
| CFG-X002 | - | retired | Was the `advanced.ffmpeg_dir` check; the key was removed (D63). Code not reused. |
| CFG-X003 | E | log level | `advanced.log_level must be error, warn, info, debug or trace` |
| CFG-X004 | E | extra arg not allowed | `advanced.encoder_extra_args contains '{o}', which is not on the allowlist` |
| CFG-X005 | E | ipc range | `advanced.ipc_timeout_ms = {v} is outside 500..=10000` |
| CFG-X006 | E | updates interval range | `updates.interval_hours = {v} is outside 1..=168` |
| CFG-X007 | E | updates endpoint format | `updates.endpoint '{v}' is not allowed. Use an https:// URL with no spaces, at most 300 characters (http://127.0.0.1 or http://localhost are accepted for local testing), or leave it empty for the built-in URL` |
| CFG-G001 | E | bad game id | `games key '{k}' is not a valid game id. Use 'name.exe' or 'name.exe@store:id'` |
| CFG-G002 | E | merged value invalid | `games.'{id}' results in an invalid {key}: {detail}` |
| CFG-P004 | W | unknown override key (raised as CFG-P004, not CFG-G003) | `Unknown key '{path}' is ignored` |
| CFG-G004 | W | duplicate exe, different store | `games.'{id}' and '{other}' both match exe '{exe}'. The store-specific entry is used when the store matches` |

Severity meaning: E blocks save (UI) and blocks the change at the service. W is shown but never blocks. I is informational.

### 6.5 Versioning

- `config_version` is an integer at the top of the file. Current value: `1`.
- A file with a higher version is read-only and never overwritten. The app runs on in-memory defaults, shows CFG-P003, and offers "Open config folder".
- A file with a lower version is migrated on the next save (see 5.7). Backups are kept as described in 5.6.
- Adding an optional key is not a version change. Renaming, removing, or changing the meaning of a key is a version change and needs a migration step.

### 6.6 Hot reload summary

- Live keys apply on the next read. No pipeline work is needed.
- NextPart keys apply when a session part file opens. This includes filename, folder, and container.
- PipelineReinit keys rebuild capture, encode or audio encode at the next keyframe. The session splits and the replay epoch advances. This is zero frame drop by design.
- AudioReinit keys rebuild the WASAPI graph. Expect up to about 100 ms of audio gap on affected tracks only.
- ReplayFlush (replay.storage) clears the buffer. The UI asks first.
- Restart keys (log_dir, encoder_extra_args) take effect after `clipcheckd` restarts. The UI offers "Restart recorder now", which first stops the current session part cleanly and asks for confirmation during an active recording.

---

## 7. IPC messages produced or consumed

Named pipe `\\.\pipe\clipcheck-ipc`, length-prefixed JSON (per brief). Config messages use these names.

### 7.1 Requests (UI to service)

| Message | Payload | Response | Notes |
|---|---|---|---|
| `GetConfig` | none | `ConfigSnapshot { text, config_json, hash, diagnostics, read_only }` | `config_json` for the form, `text` for the file view |
| `ValidateConfig` | `{ text }` | `Diagnostics { items }` | Dry run, no write. Pass A and B only, plus runtime checks when the service is up |
| `PatchConfig` | `{ ops: [PatchOp], base_hash }` | `Ok { change }` or `Rejected { diagnostics }` or `Conflict { current_text, current_hash }` | Normal Apply path |
| `ReplaceConfig` | `{ text, base_hash }` | same as PatchConfig | Import; backup named `.pre-import-` |
| `ReloadConfig` | none | `Ok { change? }` or `Rejected { diagnostics }` | Forces reload from disk; used after an external edit |
| `ResetSection` | `{ section }` | same as PatchConfig | Builds unset ops for one tab |

`PatchOp` is `{ "op": "set" | "unset", "path": "video.fps", "value": <toml value> }`. Paths use dotted keys, with quoted segments for game ids, for example `games."cs2.exe".video.fps`.

### 7.2 Events (service to UI)

| Event | Payload | When |
|---|---|---|
| `ConfigChanged` | `{ hash, source: "ui" \| "file", keys: [{path, class}] }` | Any accepted change |
| `ConfigRejected` | `{ diagnostics }` | External edit failed validation; old config still active |
| `ConfigRestartRequired` | `{ keys }` | A Restart-class key changed; recorder not yet restarted |
| `ConfigReadOnly` | `{ reason: "too_new" \| "migration_failed" \| "parse_error" }` | Load found a file it will not write |

Tray notifications are not IPC. They come from the notifications module when `notifications.errors` is on.

### 7.3 Consumed from other features
- Encoder probe result (from `encode/`) feeds CFG-V011 and V010 runtime checks.
- Device list (from `audio/`) feeds CFG-A009 and A010 and the device dropdowns.
- Hotkey registration result (from `hotkeys/`) feeds CFG-H006.
- Session state (from `session/`) decides whether a Restart-class key is refused.

---

## 8. Edge cases & failure modes

| # | Case | Behavior |
|---|---|---|
| 1 | Hand edit leaves TOML syntax error mid-save | External change rejected (CFG-P001). Last good snapshot stays active. Tray error notification. File view shows line and column |
| 2 | Editor saves via temp file and rename, watcher sees a sharing violation | Read retries 3 times at 50 ms, then waits for the next event. Debounce absorbs duplicate events |
| 3 | UI save and external edit race | `base_hash` check. UI gets Conflict and offers Reload or Overwrite. Service never writes over an unseen change |
| 4 | Service crashes during write | `ReplaceFileW` is atomic on NTFS. Worst case: `.tmp` left behind. Startup deletes stale `.tmp` after a successful load |
| 5 | File is newer than this build (`config_version = 2`) | Read-only mode with in-memory defaults. CFG-P003. The file is never overwritten, so a newer build keeps its data |
| 6 | Migration step fails | Backup restored in memory, defaults used, read-only, CFG-P012 with backup path |
| 7 | Unknown key (typo or newer feature) | Warning CFG-P004. The key is preserved in the file on save, so newer builds keep their keys |
| 8 | Preset set to non-custom while rate keys exist | Rate keys ignored with warning CFG-V009. They are not deleted, so switching back to custom restores them |
| 9 | Hotkey chord already taken by another app | Registration fails. Config value kept. CFG-H006 shown. The UI marks the field "in use" |
| 10 | Forced encoder (for example NVENC on an AMD GPU) | Config valid. Start of recording fails with CFG-V011 and a notification. Auto is not silently substituted for a forced value |
| 11 | Mic device unplugged | Track stays, silent, CFG-A009 warning. Other tracks continue. Reconnect is detected by the audio module |
| 12 | Recording folder on a removed drive or read-only media | CFG-S002 at session start. Recording does not start. Settings page offers Browse |
| 13 | Quota delete races with an active recording | Quota never deletes the active session part or any buffer segment. It deletes only completed clips (clips in the library) and only oldest first. Deletion uses the Recycle Bin when delete_to_recycle_bin is on |
| 14 | Import from another PC with machine paths | Import shows the warning and offers "Strip machine-specific settings". Without stripping, paths that do not exist produce CFG-S002 at record time |
| 15 | Game exe renamed or updated, override stops matching | Override kept. Shown on the Games tab as "not seen recently". Never auto-deleted |
| 16 | Two entries that match the same exe with different stores | CFG-G004 warning. Store-specific entry wins when the store matches |
| 17 | File with BOM, CRLF or LF | Read accepted. Save keeps the original BOM and line-ending style |
| 18 | File larger than 1 MiB | Rejected (CFG-P002). Defaults used in memory, read-only. Protects against accidental pastes |
| 19 | Clip hotkey seconds larger than replay.max_seconds after lowering max | CFG-H004 error at save. At load, the entry is clamped to max_seconds with a warning so recording is not blocked |
| 20 | RAM replay with a long max, memory estimate too low | Save blocked by CFG-R002. The estimate uses the current video bitrate plus track bitrates at the configured max |

---

## 9. Performance & resource budget

| Item | Budget | Notes |
|---|---|---|
| Config load at startup (service) | < 5 ms for a 20 KB file | Measured with the full example and 8 tracks |
| Reload after file change (debounce to publish) | < 20 ms, excluding debounce | Parse, migrate-no-op, validate, diff |
| Dispatch to listeners | < 2 ms | Listeners only swap pointers or send messages |
| Service RSS added by config | < 200 KB | `Arc<Config>`, `DocumentMut` cache, watcher |
| Idle CPU (no game running) | 0 polls | Watcher is event-driven (`ReadDirectoryChangesW`). No timer except the 300 ms debounce, which only runs after an event |
| Snapshot reads on the hot path | lock-free (`ArcSwap::load`) | Capture and encode threads never block on config |
| UI settings page first paint | < 150 ms | Config JSON from one IPC call |
| UI tab switch | < 16 ms | No IPC on tab switch; form state is local until Apply |
| Patch round trip (UI Apply to published) | < 100 ms typical | Dominated by `save_atomic` fsync |
| Export / import of a full file | < 50 ms | String work only |
| Disk writes | one file write per accepted change | Writes are not issued for rejected or unchanged input |

---

## 10. Test plan

### 10.1 Unit tests (crate `core`)

| ID | Test | Expected |
|---|---|---|
| T-01 | Golden: parse 6.1 full example | Equals `Config::default()`. Every default in 6.2 matches. Serialized default equals example values |
| T-02 | Round trip with comments | `parse -> apply_patch(no-op) -> to_string` is byte-identical |
| T-03 | Patch set/unset preserves comments and order | Comments outside touched keys remain |
| T-04 | Each CFG-* code | One positive and one negative fixture per code in 6.4 |
| T-05 | Enum and fps parsing | `fps = 45` gives CFG-V007. `codec = "vp9"` gives serde error mapped to CFG-P005 |
| T-06 | Source grammar | Valid and invalid strings, process names lowercased |
| T-07 | KeyChord grammar | `Alt+F9` ok, `F9` (no modifier) rejected by CFG-H001, `Alt+F4` reserved (H003), `Ctrl+Shift+Alt+A` ok |
| T-08 | Template renderer | Tokens, collision suffixes, 180-char cap, reserved names (CON, NUL) |
| T-09 | Preset expansion | Each preset returns the table values; custom returns stored values |
| T-10 | Per-game resolution | Store-specific match wins; unset fields inherit; folder sanitized |
| T-11 | Migration | v0 file (no version) becomes v1 with CFG-P011; future v2 file read-only with CFG-P003 |
| T-12 | Unknown key collection | Unknown top-level and nested keys reported, preserved on save |
| T-13 | Classification | `classify()` returns the class in 6.2 for every key path (table-driven, generated from 6.2) |
| T-14 | Diff | Changing two keys yields two KeyChange entries with correct classes; strongest class ordering in 5.4 |
| T-15 | Export strip | Machine keys removed; games kept or removed by option |

### 10.2 Service tests (`clipcheckd`, with a temp `%APPDATA%`)

| ID | Test | Expected |
|---|---|---|
| S-01 | Startup with missing file | Default file created, no diagnostics beyond info |
| S-02 | External edit valid | Snapshot changes within 400 ms; ConfigChanged with source file |
| S-03 | External edit invalid | Old snapshot kept; ConfigRejected; file untouched by service |
| S-04 | Self-write suppression | Patch causes exactly one publish, no second reload |
| S-05 | Base hash conflict | Patch with stale hash returns Conflict; disk unchanged |
| S-06 | Crash injection during save | Either old or new file, never a partial file; `.tmp` cleaned on next start |
| S-07 | Read-only mode | Newer version: writes refused, reads work |
| S-08 | Restart-class key during recording | Refused until confirmed; after confirm, session split and restart |

### 10.3 Integration tests (with capture and encode stubs)

| ID | Test | Expected |
|---|---|---|
| I-01 | Change `video.fps` while recording | PipelineReinit: session part split at keyframe; zero dropped packets in both parts; replay clip spanning boundary not produced |
| I-02 | Change `audio.mic_device` while recording | AudioReinit: video frames continuous; audio gap <= 100 ms on mic track only |
| I-03 | Change `storage.filename_template` while recording | Current part keeps name; next part uses new template |
| I-04 | Change `replay.storage` disk to ram | ReplayFlush: confirmation required; ring cleared; new segments in RAM |
| I-05 | Hotkey change while running | Old chord unregistered, new registered within 100 ms; conflict surfaces CFG-H006 |
| I-06 | Quota reached with delete_oldest_clips | Oldest clip sent to Recycle Bin; active session untouched |

### 10.4 UI tests
- Playwright (or WebView2 harness) for every tab: controls write the expected patch ops (snapshot test per tab).
- Badge correctness: each control's badge equals classify() of its key.
- Validation: each E-level rule shows inline with the code. Save disabled while any E exists.
- Import: a file with one E is refused and lists all errors. A file with only W shows the diff dialog.
- Export: stripped keys absent; header comment present; no user name or machine name in output.
- Keyboard: hotkey capture accepts modifiers, Esc clears, Tab moves focus.

### 10.5 Manual checks
- Edit config.toml in Notepad, VS Code and PowerShell `Set-Content`. Each saves and is picked up.
- Save from UI while editing the file in another editor. Conflict dialog appears.
- Lowering `replay.max_seconds` below a clip hotkey value shows CFG-H004 before saving.

---

## 11. Implementation task breakdown

Ordered. Each task is about 0.5 to 2 days.

| # | Task | Days | Depends on | Done when |
|---|---|---|---|---|
| 1 | Schema types, enums, defaults, serde adapters (`ExeName`, `GameId`, `KeyChord`, `Source`, `Fps`) | 1.5 | none | T-01, T-05, T-06, T-07 pass |
| 2 | `toml_edit` load, `serde_ignored` unknown keys, BOM/CRLF handling, file size guard | 1.5 | 1 | T-02, T-12 pass |
| 3 | Pass A and B validation engine with all codes in 6.4 (static rules) | 2 | 1 | T-04 passes |
| 4 | Migration framework and v0 handling, read-only states | 0.5 | 2 | T-11 passes |
| 5 | Preset expansion, `resolve_video`, `effective_for`, template renderer | 1 | 1 | T-08, T-09, T-10 pass |
| 6 | `apply_patch`, `save_atomic` (ReplaceFileW, backups, config.lock), `diff`, `classify` | 1.5 | 2, 3 | T-03, T-13, T-14 pass; S-06 passes |
| 7 | ConfigStore, FileWatcher (debounce, self-write hash), ConfigListener dispatch | 1.5 | 6 | S-01 to S-05, S-07 pass |
| 8 | Runtime validation hooks (encoder probe, mic list, path writability, RegisterHotKey results) | 1 | 3, 7 | CFG-V011, A009, A010, R004, S002, H006 emitted in tests |
| 9 | IPC handlers and events (7.1, 7.2) | 1 | 7 | Round-trip tests pass; UI can call GetConfig and PatchConfig |
| 10 | Settings shell and General, Storage, Detection tabs (Svelte, apply badges, inline errors) | 2 | 9 | UI tests for these tabs pass |
| 11 | Video and Audio tabs including track editor and custom rate panel | 2 | 10 | Track editor round-trip; preset-custom toggling tested |
| 12 | Replay, Hotkeys (chord capture widget), Notifications, Advanced tabs | 2 | 10 | Capture widget tests; Restart-class confirm flow works |
| 13 | Games tab (library list, override editor, inherited-value display) | 1.5 | 10 | Override add, edit, delete, reset tested |
| 14 | Import, export, diff dialog, strip machine keys, ResetSection | 1 | 9, 12 | Import and export tests in 10.4 pass |
| 15 | Autostart HKCU Run writer and Recycle Bin delete helper | 0.5 | 7 | Autostart toggled live; delete goes to bin |
| 16 | Integration tests I-01 to I-06 with capture and encode stubs | 1.5 | 7, 8 | All pass in CI on a Windows runner |

Total: about 20 to 22 engineering days.

---

## 12. Open questions

1. **Replay length unit.** The brief says the replay buffer is "default 5 min, up to 60". This doc reads "60" as 60 minutes (`replay.max_seconds` max 3600). If the brief meant 60 seconds, the range becomes 30..=60 and the default stays 300 seconds, which is inconsistent. Confirm before the first release.
2. **Per-game encoder and codec overrides.** Deferred. An encoder per game would require a per-game pipeline and a separate probe. The override table can add `codec` later without a version change.
3. **Built-in ignore list.** Processes that are never games (explorer, shell, browsers, the recorder itself) are a detection concern. This doc does not add a config key for them. The detect doc should decide whether they are hard-coded or a read-only list shipped with the app.
4. **Preset calibration.** The CQ numbers in 6.1 and the preset semantics are placeholders. The encode doc must confirm them against NVENC, AMF, QSV and x264 quality measurements. Changing the table is not a schema change, but the default file on disk does not change either.
5. **Opus in MP4.** The current rule rejects `audio.codec = "opus"` with `container = "mp4"` (CFG-A011). An alternative is to silently select MKV. Rejecting is chosen because the user should see the consequence.
6. **Encoder extra args.** `advanced.encoder_extra_args` is a power-user escape hatch with an allowlist. The allowlist content must come from the encode doc. If no safe list is possible, remove the key before v1 freeze.
7. **Quota deletion destination.** Recycle Bin is the default. For very large clip libraries the Recycle Bin may be slow or full. Confirm whether quota deletion should bypass the bin with a warning.
8. **config.toml under cloud-synced folders.** `%APPDATA%` is not normally synced, but users may redirect it. `ReplaceFileW` and `config.lock` behave differently on OneDrive-placeholder paths. Decide whether to warn when the config path is under a sync root.
9. **Locale support.** `general.locale` exists for a future translation effort. If v1 ships English only, the key can be removed, which is a schema change before release. Decide now.
10. **Track limit.** The limit of 8 tracks comes from the encoder stream budget assumed in this doc. The audio doc must confirm the mixer and per-track encoder cost at 8 tracks at 320 kbps.
11. **Hotkey defaults.** Alt+F7 to Alt+F12 may collide with some games and with other tools. The defaults are placeholders until the hotkeys doc finishes a conflict survey.
12. **Live disk-space threshold.** `notifications.low_disk_gb` is an absolute threshold. A percentage option may be wanted for large drives. Decide if v1 needs it.

## 13. Audio/Video model v2

This section replaces the v1 `[[audio.tracks]]` model and the v1 video preset values. Keys
not named here keep the meaning given in sections 6.1 and 6.2.

### 13.1 Audio sources and track layout

Sources are the game, the microphone, each listed app (`[[audio.apps]]`), and "other apps"
(everything that is not the game or a listed app). Each source has an enabled flag and a gain.

The recorded tracks are derived from the settings by `Audio::track_plan()`:

1. Track 1 is always "Mix". It holds every enabled source, so a normal player hears everything.
2. If `audio.separate_tracks` is true, one more track follows per enabled source, in this order:
   "Game", "Microphone", each enabled app (titled by its `name`), "Other apps".
3. If no source is enabled, the result is one silent "Mix" with no sources.

The limit is 8 tracks (D9). With `separate_tracks` on, that allows at most 7 enabled sources
(CFG-A001). With it off, the only limit is 16 `[[audio.apps]]` entries.

Gains are applied per source with `Audio::gain_for()`. The range is -60.0..=12.0 dB.

### 13.2 Video presets

Non-custom presets fix resolution, fps and bitrate. Custom uses the explicit fields.

| Preset | Display name | Resolution | FPS | Bitrate (kbps) |
|---|---|---|---|---|
| `low` | Low | 480p | 30 | 4000 |
| `medium` | Standard | 720p | 60 | 8000 |
| `high` | High (default) | 1080p | 60 | 14000 |
| `ultra` | Ultra | 1440p | 60 | 30000 |
| `custom` | Custom | (explicit fields) | (explicit) | (explicit) |

`Preset::values()` returns this table. The explicit `video.resolution`, `video.fps` and
`video.bitrate_kbps` defaults equal the High row, so switching to Custom keeps the same quality.

New video keys: `video.gpu` (default `"auto"`), `video.encoder_speed` (`fastest`, `balanced`
(default), `quality`) and `video.b_frames` (0..=4, default 2). Resolutions and fps values are
listed in section 6.2.

### 13.3 Apply classes

Any `audio.*` change is AudioReinit. It takes effect when the next game attaches and never
disrupts a running recording. Any `video.*` change except `video.container` is PipelineReinit,
with the same deferral.

### 13.4 Validation codes added or changed in v2

| Code | Meaning |
|---|---|
| CFG-A001 | Recorded tracks above 8, or more than 16 `[[audio.apps]]` entries |
| CFG-A002 | App name is not 1..=32 characters (also catches empty names) |
| CFG-A003 | Two apps have the same name (case-insensitive) |
| CFG-A006 | Gain outside -60.0..=12.0 (game, mic, other apps, each app) |
| CFG-A012 | Two apps share an executable (case-insensitive) |
| CFG-A013 | `audio.mic.gate_db` outside -100.0..=-20.0 |
| CFG-V012 | `video.gpu` is empty |
| CFG-V013 | `video.b_frames` outside 0..=4 |

Retired: CFG-A004 (no sources), CFG-A005 (bad source string) and CFG-A007 (per-track bitrate).
The v2 layout has none of these. An empty executable name is rejected while the TOML is parsed,
so it has no CFG code of its own.

## 14. Storage layout and libraries (DESIGN.md D24)

- `storage.root` is the PRIMARY location. New recordings go to `<root>\Clips\<Game>\` (clips)
  and `<root>\Sessions\<Game>\` (sessions). `Storage::layout_dir` builds these paths. The game
  name is sanitized first by `sanitize_game_folder`: illegal characters removed, reserved device
  names prefixed with `_`, 80 characters at most, empty becomes `Unsorted`.
- `[[storage.libraries]]` lists EXTRA folders. They are scanned and shown in the gallery, and
  never moved or written to by recording. `read_only = true` also blocks import copies and
  deletes. Disabled entries are kept in the file but not scanned. `Storage::all_library_paths`
  returns the primary root first, then the enabled entries in list order.
- Paths are compared after normalization: `/` becomes `\`, trailing separators are dropped, and
  case is ignored. Duplicates, including a copy of `storage.root`, are errors. A library inside
  another scanned folder, or the reverse, is a warning (CFG-S014): the scanner dedups by path,
  but the user should know.
- Apply class: any change to `storage.libraries` is Live. The scanner reacts to it. `storage.root`
  keeps its class (NextPart).
- Old files without the key load with an empty list.

### 13.5 Migration

- `[[audio.tracks]]` tables in a v1 file are ignored. The v2 defaults apply.
- `audio.mic_device` is read into `audio.mic.device` when `mic.device` is still `"default"`.
  The key is never written back.
- Resolution strings `1440p`, `1080p` and `720p` keep working. The new strings are `native`,
  `360p`, `480p` and `2160p`.
- Fps accepts 24, 30, 60, 90, 120 and 144. Other values are rejected with CFG-V007 in the message.

## 15. Updates section (`[updates]`, doc 11 section 8)

- `[updates]` is a `Config` group `updates: Updates` (`crates/core/src/config/schema.rs`).
  Fields: `check: UpdateCheck` (`never` | `notify`, default `notify`), `channel: UpdateChannel`
  (`stable`, default `stable`, enum is extensible), `interval_hours: u32` (default `12`,
  1..=168), `endpoint: String` (default `""`).
- `Updates::effective_endpoint(default)` returns `endpoint`, or `default` when it is empty.
  `Updates::checks_enabled()` is true only for `check = "notify"`.
- Validation: CFG-X006 (interval range) and CFG-X007 (endpoint format; `https://` with a host,
  or `http://127.0.0.1` / `http://localhost` for local testing; no whitespace; 300 characters
  at most). An empty endpoint is always valid.
- Apply class: every `updates.*` key is Live.
- `updates.interval_hours` replaces the `updates.check_interval_hours` (6..=168, default 24)
  listed in doc 10 section 5. Doc 11 section 8 is the authority for this group.
- Update state (`last_checked_unix_ms`, `skipped_version`) is not config. It lives in
  `%APPDATA%\ClipCheck\update-state.json`.
