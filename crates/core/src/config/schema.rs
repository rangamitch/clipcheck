//! Config schema: serde structs, enums, value types, defaults and TOML load/save.
//!
//! Defaults follow `docs/design/09-settings-and-config.md` section 6.2. Range
//! checks are not done here; they belong to `config::validate`.

use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::validate::{Diagnostic, Severity};
use super::vk::{VK_APPS, VK_ESCAPE, VK_F10, VK_F11, VK_F12, VK_F7, VK_F8, VK_F9, VK_SPACE};
use crate::ids::normalize_exe;

/// Current `config_version`.
pub const CONFIG_VERSION: u32 = 1;
/// Maximum accepted size of a config file in bytes (1 MiB). Diagnostic CFG-P002.
pub const MAX_FILE_BYTES: u64 = 1 << 20;

/// Errors from reading or writing the configuration TOML.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The TOML text is malformed or does not match the schema.
    #[error("invalid config TOML: {0}")]
    Parse(#[from] toml::de::Error),
    /// The config could not be serialized to TOML.
    #[error("cannot serialize config to TOML: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The file is larger than [`MAX_FILE_BYTES`]. `size` is the file size, or at least
    /// `limit + 1` when the size is only known from the bytes read.
    #[error("Config file is {size} bytes; the limit is {limit} bytes")]
    TooLarge {
        /// Size of the file in bytes.
        size: u64,
        /// The limit that was exceeded, [`MAX_FILE_BYTES`].
        limit: u64,
    },
    /// The file could not be read, or is not UTF-8.
    #[error("cannot read config file: {0}")]
    Io(#[from] io::Error),
}

impl ConfigError {
    /// The doc 09 diagnostic code for this error, when it has one: CFG-P001 for TOML syntax
    /// and CFG-P002 for an oversized file.
    pub fn diagnostic_code(&self) -> Option<&'static str> {
        match self {
            ConfigError::Parse(_) => Some("CFG-P001"),
            ConfigError::TooLarge { .. } => Some("CFG-P002"),
            ConfigError::Serialize(_) | ConfigError::Io(_) => None,
        }
    }
}

/// Reads a config file, refusing files larger than [`MAX_FILE_BYTES`].
///
/// The length is checked from the file metadata first, so an oversized file is never read.
/// Otherwise at most `MAX_FILE_BYTES + 1` bytes are read, which also catches a file that grows
/// while it is read. The bytes must be UTF-8. A BOM and CRLF line endings are left in place for
/// the TOML parser.
pub fn read_config_file(path: &Path) -> Result<String, ConfigError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > MAX_FILE_BYTES {
        return Err(ConfigError::TooLarge {
            size: len,
            limit: MAX_FILE_BYTES,
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ConfigError::TooLarge {
            size: bytes.len() as u64,
            limit: MAX_FILE_BYTES,
        });
    }
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e).into())
}

/// Root of `%APPDATA%\ClipCheck\config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version of the file. A missing key reads as [`CONFIG_VERSION`]. Values above
    /// [`CONFIG_VERSION`] are rejected by `validate` (CFG-P003); lower values are accepted.
    pub config_version: u32,
    /// `[general]`
    pub general: General,
    /// `[storage]`
    pub storage: Storage,
    /// `[detection]`
    pub detection: Detection,
    /// `[video]`
    pub video: Video,
    /// `[audio]`
    pub audio: Audio,
    /// `[replay]`
    pub replay: Replay,
    /// `[hotkeys]`
    pub hotkeys: Hotkeys,
    /// `[notifications]`
    pub notifications: Notifications,
    /// `[advanced]`
    pub advanced: Advanced,
    /// `[updates]`
    pub updates: Updates,
    /// `[games."<id>"]` tables. Key: a game key as written in the file, e.g. "cs2.exe" or
    /// "cs2.exe@steam:730". [`normalize_game_key`] gives its canonical form, which is what
    /// [`Config::game_override`] compares.
    pub games: BTreeMap<String, GameOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            general: General::default(),
            storage: Storage::default(),
            detection: Detection::default(),
            video: Video::default(),
            audio: Audio::default(),
            replay: Replay::default(),
            hotkeys: Hotkeys::default(),
            notifications: Notifications::default(),
            advanced: Advanced::default(),
            updates: Updates::default(),
            games: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Parses a config from TOML text.
    ///
    /// Missing keys take their defaults. Unknown keys are ignored, at every level. Text longer
    /// than [`MAX_FILE_BYTES`] is refused with [`ConfigError::TooLarge`]. Use
    /// [`Config::from_toml_str_checked`] to also get the CFG-P004 warnings.
    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        check_text_size(text)?;
        let mut config: Config = toml::from_str(text)?;
        config.audio.apply_legacy_keys();
        Ok(config)
    }

    /// Like [`Config::from_toml_str`], and also returns one CFG-P004 warning for each unknown
    /// key: at any level of the file, including keys inside a `[games."<id>"]` table or its
    /// `video` sub-table. Such keys are dropped, and the file still loads. The legacy
    /// `audio.mic_device` key does not warn, and neither do the retired keys `advanced.ffmpeg_dir`
    /// and `video.native_encoder`.
    pub fn from_toml_str_checked(text: &str) -> Result<(Config, Vec<Diagnostic>), ConfigError> {
        let config = Config::from_toml_str(text)?;
        let mut warnings = unknown_key_warnings(text, &config);
        warnings.extend(unknown_game_key_warnings(text));
        Ok((config, warnings))
    }

    /// Serializes this config to TOML text (no comments).
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string(self)?)
    }

    /// The commented first-run file (doc 09, section 6.1). Parses back to `Config::default()`.
    pub fn default_toml() -> &'static str {
        DEFAULT_TOML
    }

    /// The per-game override that applies to `game`, or `None` when no `[games."<id>"]`
    /// table matches.
    ///
    /// `game` is an executable name (`CS2.exe`) or a full key (`cs2.exe@steam:730`). Both
    /// sides are compared in canonical form (see [`normalize_game_key`]). Priority:
    /// 1. an entry whose normalized key equals `game`,
    /// 2. the bare `name.exe` entry for the executable,
    /// 3. for a bare `game`, the first `name.exe@store:id` entry for the executable.
    ///
    /// This is the one lookup the runtime should use for game overrides.
    pub fn game_override(&self, game: &str) -> Option<&GameOverride> {
        let wanted = normalize_game_key(game)?;
        let (exe, store) = split_game_key(&wanted);
        let mut bare = None;
        let mut qualified = None;
        for (key, over) in &self.games {
            let Some(norm) = normalize_game_key(key) else {
                continue;
            };
            if norm == wanted {
                return Some(over);
            }
            let (key_exe, key_store) = split_game_key(&norm);
            if key_exe != exe {
                continue;
            }
            if key_store.is_none() {
                bare.get_or_insert(over);
            } else {
                qualified.get_or_insert(over);
            }
        }
        bare.or(if store.is_none() { qualified } else { None })
    }
}

/// Canonical form of a `[games."<id>"]` key: `name.exe` or `name.exe@store:id`.
///
/// The exe name and the store name are lowercased, surrounding whitespace is removed from
/// every part, and the store id is kept as written. Returns `None` when the key is not a
/// valid game id: the exe part must end in `.exe` and contain no path separator or `:`, and
/// an `@` suffix must be `store:id` with both parts non-empty. This is the only definition of
/// a valid game key. Validation and [`Config::game_override`] both use it.
pub fn normalize_game_key(key: &str) -> Option<String> {
    let (exe, store) = match key.split_once('@') {
        Some((exe, store)) => (exe.trim(), Some(store)),
        None => (key.trim(), None),
    };
    let exe_ok = exe.to_ascii_lowercase().ends_with(".exe") && !exe.contains(['\\', '/', ':']);
    let base = normalize_exe(exe);
    if !exe_ok || base.is_empty() {
        return None;
    }
    let exe = format!("{base}.exe");
    match store {
        None => Some(exe),
        Some(store) => {
            let (store_name, store_id) = store.split_once(':')?;
            let (store_name, store_id) = (store_name.trim(), store_id.trim());
            if store_name.is_empty() || store_id.is_empty() {
                return None;
            }
            Some(format!(
                "{exe}@{}:{store_id}",
                store_name.to_ascii_lowercase()
            ))
        }
    }
}

/// Rejects text longer than [`MAX_FILE_BYTES`] before it is parsed.
fn check_text_size(text: &str) -> Result<(), ConfigError> {
    let size = text.len() as u64;
    if size > MAX_FILE_BYTES {
        return Err(ConfigError::TooLarge {
            size,
            limit: MAX_FILE_BYTES,
        });
    }
    Ok(())
}

/// Keys removed from the schema that old files may still contain. They are dropped on load
/// without a CFG-P004 warning (D63).
const RETIRED_KEYS: &[(&str, &str)] = &[("advanced", "ffmpeg_dir"), ("video", "native_encoder")];

/// Keys accepted in a `[games."<id>"]` table. Keep in sync with [`GameOverride`].
const GAME_OVERRIDE_KEYS: &[&str] = &[
    "display_name",
    "folder",
    "auto_record",
    "replay_enabled",
    "replay_max_seconds",
    "video",
];

/// Keys accepted in a `[games."<id>".video]` table. Keep in sync with [`VideoOverride`].
const VIDEO_OVERRIDE_KEYS: &[&str] = &[
    "preset",
    "resolution",
    "fps",
    "rate_control",
    "quality_cq",
    "bitrate_kbps",
    "max_bitrate_kbps",
];

/// One CFG-P004 warning per unknown key in a game override or its `video` table.
///
/// Text that does not parse as TOML gives no warnings here; [`Config::from_toml_str`] reports
/// that error.
fn unknown_game_key_warnings(text: &str) -> Vec<Diagnostic> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(games) = table.get("games").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, value) in games {
        let Some(game) = value.as_table() else {
            continue;
        };
        for (key, field) in game {
            if !GAME_OVERRIDE_KEYS.contains(&key.as_str()) {
                push_unknown_key(&mut out, format!("games.\"{id}\".{key}"));
            } else if key == "video" {
                let Some(video) = field.as_table() else {
                    continue;
                };
                for video_key in video.keys() {
                    if !VIDEO_OVERRIDE_KEYS.contains(&video_key.as_str()) {
                        push_unknown_key(&mut out, format!("games.\"{id}\".video.{video_key}"));
                    }
                }
            }
        }
    }
    out
}

/// One CFG-P004 warning for each unknown key at any level except inside `[games]`, which
/// [`unknown_game_key_warnings`] checks. Serde drops unknown keys on load, so a key that the
/// file has but the written-back config lacks is unknown. Known keys are always written back
/// (an unset chord is written as `""`). The legacy `audio.mic_device` key is read on load and
/// never written back, so it is skipped. The retired keys `advanced.ffmpeg_dir` and
/// `video.native_encoder` (see `RETIRED_KEYS`) are skipped too. Array entries are compared by
/// index. Text that does not parse as TOML gives no warnings here.
fn unknown_key_warnings(text: &str, config: &Config) -> Vec<Diagnostic> {
    let Ok(input) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Ok(written_text) = config.to_toml_string() else {
        return Vec::new();
    };
    let Ok(written) = written_text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_unknown_keys(&input, &written, "", &mut out);
    out
}

/// Walks one table of the input against the matching table of the written-back config, and
/// pushes a CFG-P004 warning for each key that the written table lacks. `path` is the dotted
/// path of `input`, or empty at the root. Keys under `[games]` at the root are skipped.
fn walk_unknown_keys(
    input: &toml::Table,
    written: &toml::Table,
    path: &str,
    out: &mut Vec<Diagnostic>,
) {
    for (key, value) in input {
        if path.is_empty() && key == "games" {
            continue;
        }
        if path == "audio" && key == "mic_device" {
            continue;
        }
        if RETIRED_KEYS.contains(&(path, key.as_str())) {
            continue;
        }
        let child = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match written.get(key) {
            None => push_unknown_key(out, child),
            Some(kept) => walk_unknown_value(value, kept, &child, out),
        }
    }
}

/// Descends into a value that exists in both the input and the written-back config. Tables are
/// walked by key, and arrays are walked by index. Scalars have no children to check.
/// `path` is the dotted path of `input`.
fn walk_unknown_value(
    input: &toml::Value,
    written: &toml::Value,
    path: &str,
    out: &mut Vec<Diagnostic>,
) {
    match (input, written) {
        (toml::Value::Table(i), toml::Value::Table(w)) => walk_unknown_keys(i, w, path, out),
        (toml::Value::Array(i), toml::Value::Array(w)) => {
            for (index, (i_item, w_item)) in i.iter().zip(w).enumerate() {
                walk_unknown_value(i_item, w_item, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

fn push_unknown_key(out: &mut Vec<Diagnostic>, path: String) {
    out.push(Diagnostic {
        code: "CFG-P004",
        severity: Severity::Warning,
        message: format!("Unknown key '{path}' is ignored"),
        path,
    });
}

/// Splits a canonical game key into its exe part and optional `store:id` part.
fn split_game_key(norm: &str) -> (&str, Option<&str>) {
    match norm.split_once('@') {
        Some((exe, store)) => (exe, Some(store)),
        None => (norm, None),
    }
}

/// `[general]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Start clipcheckd with Windows (HKCU Run).
    pub autostart: bool,
    /// Launch the UI minimized to tray.
    pub start_minimized: bool,
    /// Show a red dot on the tray icon while recording.
    pub tray_badge_recording: bool,
    /// "system" or "en".
    pub locale: String,
    /// What the window close (X) button does.
    pub close_behavior: CloseBehavior,
    /// Ask before exiting while a session is being recorded.
    pub confirm_exit_while_recording: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            autostart: false,
            start_minimized: true,
            tray_badge_recording: true,
            locale: "system".to_owned(),
            close_behavior: CloseBehavior::MinimizeToTray,
            confirm_exit_while_recording: true,
        }
    }
}

/// Maximum number of `[[storage.libraries]]` entries (CFG-S012).
pub const MAX_LIBRARIES: usize = 16;
/// Maximum length of a library label in characters (CFG-S013).
pub const MAX_LIBRARY_LABEL_CHARS: usize = 40;
/// Maximum length of a sanitized game folder name in characters.
pub const MAX_GAME_FOLDER_CHARS: usize = 80;
/// Folder name used when a game name sanitizes to nothing.
pub const UNSORTED_FOLDER: &str = "Unsorted";

/// `[storage]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Storage {
    /// PRIMARY recordings root: new recordings are written under `root\Clips\` and
    /// `root\Sessions\` (DESIGN.md D24). `%VARS%` are expanded at load.
    pub root: PathBuf,
    /// Store each game under `root\<GameName>\`.
    pub per_game_folders: bool,
    /// 0 = unlimited.
    pub quota_gb: u32,
    /// What to do when the quota is reached.
    pub quota_action: QuotaAction,
    /// Filename template for recordings.
    pub filename_template: String,
    /// Send deletions to the Recycle Bin.
    pub delete_to_recycle_bin: bool,
    /// EXTRA library folders: scanned and shown in the gallery, never written to by recording.
    /// `[[storage.libraries]]`. Kept last so the TOML serializer emits the plain keys first.
    pub libraries: Vec<LibraryDir>,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            root: PathBuf::from(r"%USERPROFILE%\Videos\ClipCheck"),
            per_game_folders: true,
            quota_gb: 0,
            quota_action: QuotaAction::Warn,
            filename_template: "{game} {yyyy-MM-dd HH-mm-ss} {kind}".to_owned(),
            delete_to_recycle_bin: true,
            libraries: Vec::new(),
        }
    }
}

/// One `[[storage.libraries]]` entry: an extra folder that is indexed in place (never moved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryDir {
    /// Folder to scan. Absolute (`D:\...`, `\\server\share\...`) or starting with `%VAR%`.
    pub path: String,
    /// Display label. Empty means the UI shows the folder name. At most 40 characters.
    pub label: String,
    /// The app never writes to this folder: no import copies, no deletes.
    pub read_only: bool,
    /// Scan this folder. A disabled entry is kept in the file but not scanned.
    pub enabled: bool,
}

impl Default for LibraryDir {
    fn default() -> Self {
        Self {
            path: String::new(),
            label: String::new(),
            read_only: false,
            enabled: true,
        }
    }
}

/// The two recording folders kept under the primary root (DESIGN.md D24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// `<root>\Clips\<Game>`: saved clips.
    Clips,
    /// `<root>\Sessions\<Game>`: recorded sessions.
    Sessions,
}

impl Storage {
    /// Every location to scan: the primary root first, then each enabled library in list order.
    ///
    /// Each item is `(path, is_primary, library)`. The primary item has `is_primary = true` and
    /// `library = None`. Disabled libraries are left out.
    pub fn all_library_paths(&self) -> Vec<(PathBuf, bool, Option<&LibraryDir>)> {
        let mut out = vec![(self.root.clone(), true, None)];
        out.extend(
            self.libraries
                .iter()
                .filter(|lib| lib.enabled)
                .map(|lib| (PathBuf::from(&lib.path), false, Some(lib))),
        );
        out
    }

    /// Folder for new recordings of one kind and game: `<root>\Clips\<game>` or
    /// `<root>\Sessions\<game>`. The root's trailing separator is dropped first.
    ///
    /// `game` must already be sanitized by the caller with [`sanitize_game_folder`]. This
    /// function does not check the result or create it.
    pub fn layout_dir(&self, kind: LayoutKind, game: &str) -> String {
        let root = self.root.to_string_lossy();
        let root = root.trim_end_matches(['\\', '/']);
        let folder = match kind {
            LayoutKind::Clips => "Clips",
            LayoutKind::Sessions => "Sessions",
        };
        format!(r"{root}\{folder}\{game}")
    }
}

/// Characters removed from a game folder name (Windows forbids them in file names).
const ILLEGAL_FOLDER_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Turns a game name into one safe folder name under `Clips\` or `Sessions\` (DESIGN.md D24).
///
/// Steps: control characters become spaces; the Windows-illegal characters `\ / : * ? " < > |`
/// are removed; whitespace runs collapse to one space; leading and trailing dots and spaces are
/// trimmed; a reserved device name (CON, PRN, AUX, NUL, COM1-9, LPT1-9, with or without an
/// extension) gets a `_` prefix; the result is cut to 80 characters and trailing dots and spaces
/// are trimmed again. An empty result becomes `Unsorted`.
pub fn sanitize_game_folder(game: &str) -> String {
    let spaced: String = game
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !ILLEGAL_FOLDER_CHARS.contains(c))
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(['.', ' ']);
    if trimmed.is_empty() {
        return UNSORTED_FOLDER.to_owned();
    }
    let mut name = if is_reserved_device_name(trimmed) {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    };
    if name.chars().count() > MAX_GAME_FOLDER_CHARS {
        name = name.chars().take(MAX_GAME_FOLDER_CHARS).collect();
    }
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() {
        UNSORTED_FOLDER.to_owned()
    } else {
        name.to_owned()
    }
}

/// True for CON, PRN, AUX, NUL, COM1-9 and LPT1-9, with or without an extension, any case.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_uppercase();
    let numbered = stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9');
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered
}

/// `[detection]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detection {
    /// Auto-detect any game, or only games in `allow`.
    pub mode: DetectionMode,
    /// Executables eligible in allowlist mode.
    pub allow: Vec<ExeName>,
    /// Executables never recorded.
    pub block: Vec<ExeName>,
    /// Start a session when an eligible game is detected.
    pub auto_record: bool,
    /// Save a full session of each game. Off: only the replay buffer runs, so clips are the
    /// only output. A session can still be started by hand (hotkey or UI).
    pub record_sessions: bool,
    /// Behaviour after the user stops a session manually.
    pub after_manual_stop: AfterManualStop,
    /// Seconds to keep recording after the game window closes.
    pub stop_grace_s: u32,
}

impl Default for Detection {
    fn default() -> Self {
        Self {
            mode: DetectionMode::Auto,
            allow: Vec::new(),
            block: Vec::new(),
            auto_record: true,
            record_sessions: true,
            after_manual_stop: AfterManualStop::UntilNextLaunch,
            stop_grace_s: 10,
        }
    }
}

/// `[video]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    /// Encoder backend.
    pub encoder: EncoderChoice,
    /// Video codec.
    pub codec: Codec,
    /// Container (MP4 only in v1).
    pub container: Container,
    /// Quality preset.
    pub preset: Preset,
    /// Output resolution.
    pub resolution: Resolution,
    /// Frames per second.
    pub fps: Fps,
    /// Rate control mode (used only when preset = custom).
    pub rate_control: RateControl,
    /// Constant-quality value (custom + quality).
    pub quality_cq: u8,
    /// Target bitrate in kbps (custom + cbr/vbr).
    pub bitrate_kbps: u32,
    /// Maximum bitrate in kbps (custom + vbr).
    pub max_bitrate_kbps: u32,
    /// Keyframe interval in seconds; also the replay segment length.
    pub keyframe_interval_s: u8,
    /// Draw the mouse cursor in captured frames.
    pub cursor: bool,
    /// Encoding GPU: "auto", or the adapter name as the UI reports it. Free text, non-empty.
    pub gpu: String,
    /// Encoder speed versus quality trade-off.
    pub encoder_speed: EncoderSpeed,
    /// Consecutive B-frames, 0..=4.
    pub b_frames: u8,
}

impl Default for Video {
    fn default() -> Self {
        Self {
            encoder: EncoderChoice::Auto,
            codec: Codec::H264,
            container: Container::Mp4,
            // The explicit fields start at the High row of `Preset::values`, so switching to
            // Custom keeps the same quality.
            preset: Preset::High,
            resolution: Resolution::P1080,
            fps: Fps::F60,
            rate_control: RateControl::Quality,
            quality_cq: 23,
            bitrate_kbps: 14_000,
            max_bitrate_kbps: 15_000,
            keyframe_interval_s: 2,
            cursor: true,
            gpu: "auto".to_owned(),
            encoder_speed: EncoderSpeed::Balanced,
            b_frames: 2,
        }
    }
}

/// `[audio]` (model v2, see doc 09 "Audio/Video model v2").
///
/// The recorded layout is derived from these fields by [`Audio::track_plan`]. Pre-v2 files
/// with `[[audio.tracks]]` still load; those tables are ignored and the defaults apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Audio codec.
    pub codec: AudioCodec,
    /// Bitrate per track in kbps, 128..=320.
    pub bitrate_kbps: u32,
    /// Add one track per enabled source, after the Mix track.
    pub separate_tracks: bool,
    /// Render endpoint id, or "default". Used for game and other-app capture.
    pub output_device: String,
    /// `[audio.game]`
    pub game: GameAudio,
    /// `[audio.mic]`
    pub mic: MicAudio,
    /// `[audio.other_apps]`: everything that is not the game or a listed app.
    pub other_apps: OtherAppsAudio,
    /// `[[audio.apps]]`: apps recorded on their own. At most 16 entries.
    pub apps: Vec<AudioApp>,
    /// Pre-v2 `audio.mic_device`. Moved into `mic.device` at load and never written back.
    #[serde(default, rename = "mic_device", skip_serializing)]
    legacy_mic_device: Option<String>,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            codec: AudioCodec::Aac,
            bitrate_kbps: 128,
            separate_tracks: true,
            output_device: "default".to_owned(),
            game: GameAudio::default(),
            mic: MicAudio::default(),
            other_apps: OtherAppsAudio::default(),
            apps: vec![AudioApp::default()],
            legacy_mic_device: None,
        }
    }
}

impl Audio {
    /// Moves a pre-v2 `mic_device` key into `mic.device` when `mic.device` is still the default.
    fn apply_legacy_keys(&mut self) {
        let Some(device) = self.legacy_mic_device.take() else {
            return;
        };
        if self.mic.device == "default" {
            self.mic.device = device;
        }
    }

    /// Every enabled source in track order: game, microphone, enabled apps in list order,
    /// other apps.
    pub fn enabled_sources(&self) -> Vec<SourceRef> {
        let mut out = Vec::new();
        if self.game.enabled {
            out.push(SourceRef::Game);
        }
        if self.mic.enabled {
            out.push(SourceRef::Mic);
        }
        out.extend(
            self.apps
                .iter()
                .filter(|app| app.enabled)
                .map(|app| SourceRef::App(app.exe.clone())),
        );
        if self.other_apps.enabled {
            out.push(SourceRef::OtherApps);
        }
        out
    }

    /// The recorded tracks, derived from the settings.
    ///
    /// Track 1 is always "Mix" and holds every enabled source. With `separate_tracks`, one more
    /// track follows per enabled source, in the order of [`Audio::enabled_sources`], titled
    /// "Game", "Microphone", the app's `name`, or "Other apps". With no enabled source the
    /// result is a single silent "Mix" with no sources.
    ///
    /// The result can exceed [`MAX_AUDIO_TRACKS`]; `validate` reports that case (CFG-A001).
    pub fn track_plan(&self) -> Vec<TrackPlan> {
        let enabled = self.enabled_sources();
        if enabled.is_empty() {
            return vec![TrackPlan {
                title: "Mix".to_owned(),
                sources: Vec::new(),
            }];
        }
        let mut plan = Vec::with_capacity(enabled.len() + 1);
        plan.push(TrackPlan {
            title: "Mix".to_owned(),
            sources: enabled.clone(),
        });
        if self.separate_tracks {
            for source in enabled {
                plan.push(TrackPlan {
                    title: self.source_title(&source),
                    sources: vec![source],
                });
            }
        }
        plan
    }

    /// Gain in dB for one source. An app that is not in the list gets 0.0.
    pub fn gain_for(&self, source: &SourceRef) -> f32 {
        match source {
            SourceRef::Game => self.game.gain_db,
            SourceRef::Mic => self.mic.gain_db,
            SourceRef::OtherApps => self.other_apps.gain_db,
            SourceRef::App(exe) => self
                .apps
                .iter()
                .find(|app| &app.exe == exe)
                .map_or(0.0, |app| app.gain_db),
        }
    }

    /// Track title for one separate track.
    fn source_title(&self, source: &SourceRef) -> String {
        match source {
            SourceRef::Game => "Game".to_owned(),
            SourceRef::Mic => "Microphone".to_owned(),
            SourceRef::OtherApps => "Other apps".to_owned(),
            SourceRef::App(exe) => self
                .apps
                .iter()
                .find(|app| &app.exe == exe)
                .map_or_else(|| exe.to_string(), |app| app.name.clone()),
        }
    }
}

/// `audio.game`: the captured game's audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameAudio {
    /// Record the game's audio.
    pub enabled: bool,
    /// Gain in dB, -60.0..=12.0.
    pub gain_db: f32,
}

impl Default for GameAudio {
    fn default() -> Self {
        Self {
            enabled: true,
            gain_db: 0.0,
        }
    }
}

/// `audio.mic`: the microphone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicAudio {
    /// Record the microphone.
    pub enabled: bool,
    /// Capture endpoint id, or "default".
    pub device: String,
    /// Gain in dB, -60.0..=12.0.
    pub gain_db: f32,
    /// Apply a noise gate.
    pub gate_enabled: bool,
    /// Noise gate threshold in dB, -100.0..=-20.0.
    pub gate_db: f32,
}

impl Default for MicAudio {
    fn default() -> Self {
        Self {
            enabled: true,
            device: "default".to_owned(),
            gain_db: 0.0,
            gate_enabled: false,
            gate_db: -50.0,
        }
    }
}

/// `audio.other_apps`: everything that is not the game or a listed app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OtherAppsAudio {
    /// Record all other apps.
    pub enabled: bool,
    /// Gain in dB, -60.0..=12.0.
    pub gain_db: f32,
}

impl Default for OtherAppsAudio {
    fn default() -> Self {
        Self {
            enabled: false,
            gain_db: 0.0,
        }
    }
}

/// One `[[audio.apps]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioApp {
    /// Executable whose audio is recorded. Lowercase, ends in ".exe".
    pub exe: ExeName,
    /// Display name, also the track title. 1..=32 characters.
    pub name: String,
    /// Record this app.
    pub enabled: bool,
    /// Gain in dB, -60.0..=12.0.
    pub gain_db: f32,
}

impl Default for AudioApp {
    /// The default entry is Discord.
    fn default() -> Self {
        Self {
            exe: ExeName("discord.exe".to_owned()),
            name: "Discord".to_owned(),
            enabled: true,
            gain_db: 0.0,
        }
    }
}

/// A source that can be mixed into a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRef {
    /// The captured game.
    Game,
    /// The microphone.
    Mic,
    /// Everything that is not the game or a listed app.
    OtherApps,
    /// One listed app, by executable.
    App(ExeName),
}

/// One recorded audio track, derived by [`Audio::track_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackPlan {
    /// Track title.
    pub title: String,
    /// Sources mixed into this track. Empty only for a silent Mix.
    pub sources: Vec<SourceRef>,
}

/// Maximum number of audio tracks in one recording (decision D9).
pub const MAX_AUDIO_TRACKS: usize = 8;
/// Maximum number of `[[audio.apps]]` entries.
pub const MAX_AUDIO_APPS: usize = 16;

/// `[replay]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replay {
    /// Run the replay buffer.
    pub enabled: bool,
    /// Buffer length in seconds.
    pub max_seconds: u32,
    /// Where the buffer lives.
    pub storage: ReplayStorage,
    /// Buffer folder. `%VARS%` are expanded at load.
    pub location: PathBuf,
    /// RAM budget in MB when `storage = ram`.
    pub ram_max_mb: u32,
}

impl Default for Replay {
    fn default() -> Self {
        Self {
            enabled: true,
            max_seconds: 300,
            storage: ReplayStorage::Disk,
            location: PathBuf::from(r"%LOCALAPPDATA%\ClipCheck\buffer"),
            ram_max_mb: 512,
        }
    }
}

/// `[hotkeys]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// `[[hotkeys.clips]]` entries.
    pub clips: Vec<ClipHotkey>,
    /// `[hotkeys.session]`
    pub session: SessionHotkeys,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            clips: vec![
                ClipHotkey {
                    name: "Clip 30s".to_owned(),
                    keys: KeyChord::alt(VK_F9),
                    seconds: 30,
                    press: Press::Tap,
                },
                ClipHotkey {
                    name: "Clip 60s".to_owned(),
                    keys: KeyChord::alt(VK_F10),
                    seconds: 60,
                    press: Press::Tap,
                },
            ],
            session: SessionHotkeys::default(),
        }
    }
}

/// Hold threshold in milliseconds for a `press = "long"` clip hotkey. A press shorter than
/// this is a tap. This is the single source for the hotkey gesture logic.
pub const LONG_PRESS_MS: u32 = 400;

/// `press`: how a `[[hotkeys.clips]]` key must be pressed to fire its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Press {
    /// Fires on a quick press (shorter than `LONG_PRESS_MS`).
    #[default]
    Tap,
    /// Fires once the key is held for `LONG_PRESS_MS`.
    Long,
}

impl fmt::Display for Press {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Press::Tap => f.write_str("tap"),
            Press::Long => f.write_str("long"),
        }
    }
}

impl FromStr for Press {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tap" => Ok(Press::Tap),
            "long" => Ok(Press::Long),
            _ => Err(format!("unknown press `{s}`: use \"tap\" or \"long\"")),
        }
    }
}

/// One `[[hotkeys.clips]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipHotkey {
    /// Display name, 1..=32 chars.
    pub name: String,
    /// Key chord, e.g. "Alt+F9".
    pub keys: KeyChord,
    /// Clip length in seconds, 5..=3600.
    pub seconds: u32,
    /// How the key is pressed. `tap` (default) fires on a quick press. `long` fires once the
    /// key is held for `LONG_PRESS_MS`. A chord may be shared by one tap clip and one long clip.
    pub press: Press,
}

impl Default for ClipHotkey {
    fn default() -> Self {
        Self {
            name: String::new(),
            keys: KeyChord::alt(VK_F9),
            seconds: 30,
            press: Press::Tap,
        }
    }
}

/// `[hotkeys.session]`. An empty string in the file means unbound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionHotkeys {
    /// Split the session. Default Alt+F11.
    #[serde(with = "opt_chord")]
    pub split: Option<KeyChord>,
    /// Start or stop the session. Default Alt+F12.
    #[serde(with = "opt_chord")]
    pub stop_start: Option<KeyChord>,
    /// Toggle the replay buffer. Default Alt+F8.
    #[serde(with = "opt_chord")]
    pub toggle_buffer: Option<KeyChord>,
    /// Mute the microphone. Default Alt+F7.
    #[serde(with = "opt_chord")]
    pub mute_mic: Option<KeyChord>,
    /// Toggle or push-to-talk behaviour for `mute_mic`.
    pub mute_mic_mode: MuteMode,
}

impl Default for SessionHotkeys {
    fn default() -> Self {
        Self {
            split: Some(KeyChord::alt(VK_F11)),
            stop_start: Some(KeyChord::alt(VK_F12)),
            toggle_buffer: Some(KeyChord::alt(VK_F8)),
            mute_mic: Some(KeyChord::alt(VK_F7)),
            mute_mic_mode: MuteMode::Toggle,
        }
    }
}

/// `[notifications]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Master switch for toast notifications.
    pub enabled: bool,
    /// Notify when a clip is saved.
    pub clip_saved: bool,
    /// Notify when a session is split.
    pub session_split: bool,
    /// Notify when recording starts.
    pub recording_started: bool,
    /// Notify on errors.
    pub errors: bool,
    /// Notify on low disk space.
    pub low_disk: bool,
    /// Low disk threshold in GB, 1..=100.
    pub low_disk_gb: u32,
    /// Play a sound with notifications.
    pub sound: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            clip_saved: true,
            session_split: true,
            recording_started: false,
            errors: true,
            low_disk: true,
            low_disk_gb: 5,
            sound: false,
        }
    }
}

/// `[advanced]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    /// Log verbosity.
    pub log_level: LogLevel,
    /// Log folder. `%VARS%` are expanded at load.
    pub log_dir: PathBuf,
    /// Game detection poll interval in ms, 250..=5000.
    pub detect_poll_ms: u32,
    /// IPC timeout in ms, 500..=10000.
    pub ipc_timeout_ms: u32,
    /// Cache encoder probe results.
    pub encoder_probe_cache: bool,
    /// Extra encoder arguments. Allowlisted only.
    pub encoder_extra_args: Vec<String>,
    /// Show the debug overlay.
    pub debug_overlay: bool,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            log_dir: PathBuf::from(r"%LOCALAPPDATA%\ClipCheck\logs"),
            detect_poll_ms: 1000,
            ipc_timeout_ms: 2000,
            encoder_probe_cache: true,
            encoder_extra_args: Vec::new(),
            debug_overlay: false,
        }
    }
}

/// Smallest accepted `updates.interval_hours` (CFG-X006).
pub const UPDATE_INTERVAL_HOURS_MIN: u32 = 1;
/// Largest accepted `updates.interval_hours` (CFG-X006).
pub const UPDATE_INTERVAL_HOURS_MAX: u32 = 168;
/// Longest accepted `updates.endpoint` in characters (CFG-X007).
pub const MAX_UPDATE_ENDPOINT_CHARS: usize = 300;

/// `[updates]`: optional update checks (doc 11, section 8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    /// `"never"` (no background network) or `"notify"` (check on an interval and notify).
    pub check: UpdateCheck,
    /// Release channel. Only `"stable"` exists today.
    pub channel: UpdateChannel,
    /// Hours between background checks, 1..=168. Used only when `check = "notify"`.
    pub interval_hours: u32,
    /// Manifest URL override. Empty = the built-in default. Must be `https://`, or
    /// `http://127.0.0.1` / `http://localhost` for local testing.
    pub endpoint: String,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check: UpdateCheck::default(),
            channel: UpdateChannel::default(),
            interval_hours: 12,
            endpoint: String::new(),
        }
    }
}

impl Updates {
    /// The manifest URL to use: `endpoint` when it is set, otherwise `default`.
    pub fn effective_endpoint(&self, default: &str) -> String {
        if self.endpoint.is_empty() {
            default.to_owned()
        } else {
            self.endpoint.clone()
        }
    }

    /// True when background update checks are enabled (`check = "notify"`).
    pub fn checks_enabled(&self) -> bool {
        self.check == UpdateCheck::Notify
    }
}

/// `updates.check`: whether ClipCheck checks for updates in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheck {
    /// No background network calls. Only a user-started check goes out.
    Never,
    /// Check on the interval and show a notification when a newer version exists.
    #[default]
    Notify,
}

/// `updates.channel`: which release stream to follow. Extensible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Tagged stable releases.
    #[default]
    Stable,
}

/// Per-game override (`[games."<id>"]`). Every field is optional; `None` means "inherit global".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameOverride {
    /// Display name, 1..=64 chars.
    pub display_name: Option<String>,
    /// Recordings folder for this game. Absolute.
    pub folder: Option<PathBuf>,
    /// Override `detection.auto_record`.
    pub auto_record: Option<bool>,
    /// Override `replay.enabled`.
    pub replay_enabled: Option<bool>,
    /// Override `replay.max_seconds`.
    pub replay_max_seconds: Option<u32>,
    /// `[games."<id>".video]`
    pub video: Option<VideoOverride>,
}

/// Per-game video override (`[games."<id>".video]`). Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoOverride {
    /// Override `video.preset`.
    pub preset: Option<Preset>,
    /// Override `video.resolution`.
    pub resolution: Option<Resolution>,
    /// Override `video.fps`.
    pub fps: Option<Fps>,
    /// Override `video.rate_control`.
    pub rate_control: Option<RateControl>,
    /// Override `video.quality_cq`.
    pub quality_cq: Option<u8>,
    /// Override `video.bitrate_kbps`.
    pub bitrate_kbps: Option<u32>,
    /// Override `video.max_bitrate_kbps`.
    pub max_bitrate_kbps: Option<u32>,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// `detection.mode`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMode {
    /// Any detected game is eligible.
    #[default]
    Auto,
    /// Only games in `detection.allow` are eligible.
    Allowlist,
}

/// `detection.after_manual_stop`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AfterManualStop {
    /// Stay off until the next game launch.
    #[default]
    UntilNextLaunch,
    /// Stay off until auto-record is re-enabled.
    UntilReenabled,
}

/// `storage.quota_action`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuotaAction {
    /// Warn only.
    #[default]
    Warn,
    /// Delete the oldest clips.
    DeleteOldestClips,
    /// Stop recording.
    StopRecording,
}

/// `video.encoder`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EncoderChoice {
    /// Pick the best available encoder.
    #[default]
    Auto,
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel Quick Sync.
    Qsv,
    /// Software encoder.
    Software,
}

/// `video.codec`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.264 / AVC.
    #[default]
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1.
    Av1,
}

/// `video.container`. MP4 is the only v1 container (DESIGN.md D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    /// Fragmented MP4 (the only v1 container).
    #[default]
    Mp4,
}

/// `video.preset`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// Low quality preset.
    Low,
    /// Medium quality preset.
    Medium,
    /// High quality preset.
    #[default]
    High,
    /// Ultra quality preset.
    Ultra,
    /// Use the explicit rate-control fields.
    Custom,
}

/// The fixed settings of a non-custom [`Preset`] (see [`Preset::values`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetValues {
    /// Output resolution.
    pub resolution: Resolution,
    /// Frames per second.
    pub fps: Fps,
    /// Target bitrate in kbps.
    pub bitrate_kbps: u32,
}

impl Preset {
    /// The fixed table for this preset, or `None` for [`Preset::Custom`].
    ///
    /// Display names: Low, Standard (the `medium` value), High, Ultra, Custom.
    /// Low = 480p / 30 fps / 4000 kbps, Standard = 720p / 60 / 8000,
    /// High = 1080p / 60 / 14000, Ultra = 1440p / 60 / 30000.
    pub fn values(self) -> Option<PresetValues> {
        let (resolution, fps, bitrate_kbps) = match self {
            Preset::Low => (Resolution::P480, Fps::F30, 4_000),
            Preset::Medium => (Resolution::P720, Fps::F60, 8_000),
            Preset::High => (Resolution::P1080, Fps::F60, 14_000),
            Preset::Ultra => (Resolution::P1440, Fps::F60, 30_000),
            Preset::Custom => return None,
        };
        Some(PresetValues {
            resolution,
            fps,
            bitrate_kbps,
        })
    }
}

/// `video.resolution`. Serialized as "native", "360p", "480p", "720p", "1080p", "1440p", "2160p".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Resolution {
    /// Capture resolution.
    #[default]
    #[serde(rename = "native")]
    Native,
    /// 640x360.
    #[serde(rename = "360p")]
    P360,
    /// 854x480.
    #[serde(rename = "480p")]
    P480,
    /// 1280x720.
    #[serde(rename = "720p")]
    P720,
    /// 1920x1080.
    #[serde(rename = "1080p")]
    P1080,
    /// 2560x1440.
    #[serde(rename = "1440p")]
    P1440,
    /// 3840x2160.
    #[serde(rename = "2160p")]
    P2160,
}

/// `video.encoder_speed`: encoder speed versus quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EncoderSpeed {
    /// Fastest encode, lowest quality per bit.
    Fastest,
    /// Default trade-off.
    #[default]
    Balanced,
    /// Slowest encode, best quality per bit.
    Quality,
}

/// `video.rate_control`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RateControl {
    /// Constant quality (`quality_cq`).
    #[default]
    Quality,
    /// Constant bitrate.
    Cbr,
    /// Variable bitrate.
    Vbr,
}

/// `video.fps`. Serialized as the integer 24, 30, 60, 90, 120 or 144.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(try_from = "u16", into = "u16")]
pub enum Fps {
    /// 24 frames per second.
    F24,
    /// 30 frames per second.
    F30,
    /// 60 frames per second.
    #[default]
    F60,
    /// 90 frames per second.
    F90,
    /// 120 frames per second.
    F120,
    /// 144 frames per second.
    F144,
}

impl TryFrom<u16> for Fps {
    type Error = String;

    fn try_from(v: u16) -> Result<Self, String> {
        match v {
            24 => Ok(Fps::F24),
            30 => Ok(Fps::F30),
            60 => Ok(Fps::F60),
            90 => Ok(Fps::F90),
            120 => Ok(Fps::F120),
            144 => Ok(Fps::F144),
            other => Err(format!(
                "fps must be 24, 30, 60, 90, 120 or 144 (CFG-V007), found {other}"
            )),
        }
    }
}

impl From<Fps> for u16 {
    fn from(f: Fps) -> u16 {
        match f {
            Fps::F24 => 24,
            Fps::F30 => 30,
            Fps::F60 => 60,
            Fps::F90 => 90,
            Fps::F120 => 120,
            Fps::F144 => 144,
        }
    }
}

/// `audio.codec`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    /// AAC-LC.
    #[default]
    Aac,
    /// Opus.
    Opus,
}

/// `replay.storage`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStorage {
    /// Segments on disk.
    #[default]
    Disk,
    /// Segments in RAM.
    Ram,
}

/// `hotkeys.session.mute_mic_mode`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MuteMode {
    /// Press to toggle.
    #[default]
    Toggle,
    /// Hold to talk.
    PushToTalk,
}

/// `general.close_behavior`: what the window close (X) button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    /// Hide the window to the system tray and keep running.
    #[default]
    MinimizeToTray,
    /// Close the window and exit the application.
    Exit,
}

/// `advanced.log_level`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Normal logging.
    #[default]
    Info,
    /// Debug logging.
    Debug,
    /// Everything.
    Trace,
}

// ---------------------------------------------------------------------------
// Value types with string forms
// ---------------------------------------------------------------------------

/// Lowercase executable basename ending in ".exe". Example: "Discord.exe" becomes "discord.exe".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExeName(String);

impl ExeName {
    /// The normalized executable name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ExeName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let trimmed = s.trim();
        let has_path_sep = trimmed.contains(['\\', '/', ':']);
        if has_path_sep || !trimmed.to_ascii_lowercase().ends_with(".exe") {
            return Err(format!(
                "`{s}` is not an executable basename ending in .exe"
            ));
        }
        let base = normalize_exe(trimmed);
        if base.is_empty() {
            return Err(format!(
                "`{s}` is not an executable basename ending in .exe"
            ));
        }
        Ok(ExeName(format!("{base}.exe")))
    }
}

impl Serialize for ExeName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ExeName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        parse_string(d)
    }
}

/// Keyboard modifiers of a [`KeyChord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Control.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Windows key.
    pub win: bool,
}

/// A key chord such as "Ctrl+Alt+F9". Exactly one non-modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    /// Modifier keys.
    pub mods: Modifiers,
    /// Win32 virtual-key code of the non-modifier key.
    pub vk: u16,
}

impl KeyChord {
    /// Alt plus the given virtual-key code.
    pub const fn alt(vk: u16) -> Self {
        KeyChord {
            mods: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
                win: false,
            },
            vk,
        }
    }
}

/// Named keys, with the canonical names used by the hotkeys crate (`Key::name`) and their Win32
/// virtual-key codes. Escape is not here: it is always refused (see [`KeyChord::refusal`]).
/// `Alt+Space` is refused too, but plain Space and Space with other modifiers are accepted.
const NAMED_KEYS: &[(&str, u16)] = &[
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Numpad0", 0x60),
    ("Numpad1", 0x61),
    ("Numpad2", 0x62),
    ("Numpad3", 0x63),
    ("Numpad4", 0x64),
    ("Numpad5", 0x65),
    ("Numpad6", 0x66),
    ("Numpad7", 0x67),
    ("Numpad8", 0x68),
    ("Numpad9", 0x69),
    ("NumpadMultiply", 0x6A),
    ("NumpadAdd", 0x6B),
    ("NumpadSeparator", 0x6C),
    ("NumpadSubtract", 0x6D),
    ("NumpadDecimal", 0x6E),
    ("NumpadDivide", 0x6F),
    ("Semicolon", 0xBA),
    ("Equals", 0xBB),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backquote", 0xC0),
    ("LeftBracket", 0xDB),
    ("Backslash", 0xDC),
    ("RightBracket", 0xDD),
    ("Quote", 0xDE),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("PrintScreen", 0x2C),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
    ("NumLock", 0x90),
    ("CapsLock", 0x14),
    ("Space", 0x20),
    ("AppsKey", 0x5D),
    ("VolumeMute", 0xAD),
    ("VolumeDown", 0xAE),
    ("VolumeUp", 0xAF),
    ("MediaNext", 0xB0),
    ("MediaPrev", 0xB1),
    ("MediaStop", 0xB2),
    ("MediaPlayPause", 0xB3),
    ("LaunchMail", 0xB4),
    ("LaunchMediaSelect", 0xB5),
    ("LaunchApp1", 0xB6),
    ("LaunchApp2", 0xB7),
    ("BrowserBack", 0xA6),
    ("BrowserForward", 0xA7),
    ("BrowserRefresh", 0xA8),
    ("BrowserStop", 0xA9),
    ("BrowserSearch", 0xAA),
    ("BrowserFavorites", 0xAB),
    ("BrowserHome", 0xAC),
];

/// True for a modifier name, which is never a key on its own.
fn is_modifier_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "ctrl" | "control" | "alt" | "shift" | "win" | "windows"
    )
}

/// Virtual-key code for a key name: "A".."Z", "0".."9", "F1".."F24", or a name in
/// [`NAMED_KEYS`] (all case-insensitive). `Menu` is an alias for `AppsKey`.
fn vk_from_name(name: &str) -> Option<u16> {
    let up = name.to_ascii_uppercase();
    if let [b] = up.as_bytes() {
        if matches!(b, b'A'..=b'Z' | b'0'..=b'9') {
            return Some(u16::from(*b));
        }
    }
    if let Some(digits) = up.strip_prefix('F') {
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            let n: u16 = digits.parse().ok()?;
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
            return None;
        }
    }
    if name.eq_ignore_ascii_case("Menu") {
        return Some(VK_APPS);
    }
    NAMED_KEYS
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|&(_, vk)| vk)
}

/// Writes the key name for a virtual-key code, the inverse of [`vk_from_name`].
/// Fails with `fmt::Error` for a code that has no name.
fn write_key_name(out: &mut impl fmt::Write, vk: u16) -> fmt::Result {
    match vk {
        0x70..=0x87 => write!(out, "F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => out.write_char(char::from(vk as u8)),
        _ => match NAMED_KEYS.iter().find(|(_, code)| *code == vk) {
            Some((name, _)) => out.write_str(name),
            None => Err(fmt::Error),
        },
    }
}

impl FromStr for KeyChord {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let Some((key, mod_parts)) = parts.split_last() else {
            return Err(format!("empty key chord `{s}`"));
        };
        let mut mods = Modifiers::default();
        for m in mod_parts {
            let slot = match m.to_ascii_lowercase().as_str() {
                "ctrl" => &mut mods.ctrl,
                "alt" => &mut mods.alt,
                "shift" => &mut mods.shift,
                "win" => &mut mods.win,
                _ => return Err(format!("unknown modifier `{m}` in `{s}`")),
            };
            if *slot {
                return Err(format!("duplicate modifier `{m}` in `{s}`"));
            }
            *slot = true;
        }
        if key.eq_ignore_ascii_case("Escape") || key.eq_ignore_ascii_case("Esc") {
            return Err(format!("Escape is reserved and cannot be a hotkey: `{s}`"));
        }
        if is_modifier_name(key) {
            return Err(format!(
                "`{key}` is a modifier and cannot be a hotkey on its own: `{s}`"
            ));
        }
        let vk = vk_from_name(key).ok_or_else(|| format!("unsupported key `{key}` in `{s}`"))?;
        let chord = KeyChord { mods, vk };
        match chord.refusal() {
            Some(reason) => Err(format!("{reason}: `{s}`")),
            None => Ok(chord),
        }
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mods;
        for (on, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.win, "Win"),
        ] {
            if on {
                f.write_str(name)?;
                f.write_str("+")?;
            }
        }
        if self.has_key_name() {
            write_key_name(f, self.vk)
        } else {
            // Display never fails: an unnamed code gets a fallback for humans and logs only.
            // Serialization and the parser never accept this text (see `text`).
            write!(f, "VK 0x{:X}", self.vk)
        }
    }
}

impl KeyChord {
    /// Chord text such as "Ctrl+F9", or `None` when the key code has no name.
    /// Used by serialization, so the display fallback for unnamed keys is never written.
    fn text(&self) -> Option<String> {
        if !self.has_key_name() {
            return None;
        }
        let mut out = String::with_capacity(12);
        fmt::Write::write_fmt(&mut out, format_args!("{self}")).ok()?;
        Some(out)
    }

    /// Why this chord can never be a hotkey, for the two refused chords: Escape (any
    /// modifiers) and Alt+Space (the Windows window menu). `None` for every other chord.
    pub(super) fn refusal(&self) -> Option<&'static str> {
        let m = self.mods;
        if self.vk == VK_ESCAPE {
            return Some("Escape is reserved and cannot be a hotkey");
        }
        let alt_only = m.alt && !m.ctrl && !m.shift && !m.win;
        if self.vk == VK_SPACE && alt_only {
            return Some("Alt+Space is reserved by Windows (it opens the window menu)");
        }
        None
    }

    /// True when the key code has a name that [`write_key_name`] writes, so the chord has text.
    /// A refused chord (see [`KeyChord::refusal`]) has no text, so it cannot be saved.
    pub(super) fn has_key_name(&self) -> bool {
        if self.refusal().is_some() {
            return false;
        }
        matches!(self.vk, 0x70..=0x87 | 0x30..=0x39 | 0x41..=0x5A)
            || NAMED_KEYS.iter().any(|&(_, vk)| vk == self.vk)
    }
}

/// The serde error for a chord whose key code has no name. Refused as an error, never a panic.
fn unnamed_key_error<E: serde::ser::Error>(vk: u16) -> E {
    E::custom(format!(
        "key code 0x{vk:02X} has no name ClipCheck can write. Use A-Z, 0-9 or F1-F24"
    ))
}

impl Serialize for KeyChord {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let text = self
            .text()
            .ok_or_else(|| unnamed_key_error::<S::Error>(self.vk))?;
        s.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for KeyChord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        parse_string(d)
    }
}

/// Reads a string and parses it with `FromStr`, mapping the error to a serde error.
fn parse_string<'de, T, D>(d: D) -> Result<T, D::Error>
where
    T: FromStr<Err = String>,
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    s.parse::<T>()
        .map_err(<D::Error as serde::de::Error>::custom)
}

/// serde adapter for optional chords: "" means unbound (None).
mod opt_chord {
    use super::KeyChord;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<KeyChord>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(chord) => {
                let text = chord
                    .text()
                    .ok_or_else(|| super::unnamed_key_error::<S::Error>(chord.vk))?;
                s.serialize_str(&text)
            }
            None => s.serialize_str(""),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<KeyChord>, D::Error> {
        let s = String::deserialize(d)?;
        if s.trim().is_empty() {
            return Ok(None);
        }
        s.parse::<KeyChord>()
            .map(Some)
            .map_err(<D::Error as serde::de::Error>::custom)
    }
}

/// The commented first-run file (doc 09, section 6.1), with every v1 key at its default.
const DEFAULT_TOML: &str = r##"# ClipCheck configuration (config_version 1)
# Location: %APPDATA%\ClipCheck\config.toml
# Every key is optional. Omitted keys take the defaults shown here.
# The Settings UI writes this file. Hand edits are picked up within ~300 ms.
config_version = 1

[general]
autostart = false               # start clipcheckd with Windows (HKCU Run)
start_minimized = true          # launch the UI minimized to tray
tray_badge_recording = true     # show a red dot on the tray icon while recording
locale = "system"               # "system" | "en"
close_behavior = "minimize_to_tray"   # window X button: "minimize_to_tray" | "exit"
confirm_exit_while_recording = true   # ask before exiting while a session is recording

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
record_sessions = true          # false = replay buffer and clips only; no full-session recording
after_manual_stop = "until_next_launch"   # or "until_reenabled"
stop_grace_s = 10               # keep recording this long after the game window closes

[video]
encoder = "auto"                # "auto" | "nvenc" | "amf" | "qsv" | "software"
codec = "h264"                  # "h264" | "hevc" | "av1" (av1 needs a hardware encoder)
container = "mp4"               # "mp4" (the only v1 container)
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
bitrate_kbps = 128              # 128..=320, per track
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
press = "tap"                   # "tap" | "long" (long = hold the key for 400 ms)

[[hotkeys.clips]]
name = "Clip 60s"
keys = "Alt+F10"
seconds = 60
press = "tap"                   # "tap" | "long"

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
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_game_key_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("cs2.exe", Some("cs2.exe")),
            ("CS2.EXE", Some("cs2.exe")),
            (" Cs2.exe ", Some("cs2.exe")),
            ("cs2.exe@steam:730", Some("cs2.exe@steam:730")),
            ("cs2.exe@STEAM:730", Some("cs2.exe@steam:730")),
            ("cs2.exe@Steam: 730 ", Some("cs2.exe@steam:730")),
            ("cs2", None),
            (".exe", None),
            ("cs2.exe@steam", None),
            ("cs2.exe@:730", None),
            ("cs2.exe@steam:", None),
            ("exe:cs2.exe", None),
            ("C:\\Games\\cs2.exe", None),
            ("games/cs2.exe", None),
        ];
        for (input, want) in cases {
            assert_eq!(
                normalize_game_key(input).as_deref(),
                *want,
                "normalize_game_key({input:?})"
            );
        }
    }

    #[test]
    fn game_override_matches_any_spelling_and_prefers_bare_key() {
        let over = |name: &str| GameOverride {
            display_name: Some(name.to_owned()),
            ..Default::default()
        };
        let mut cfg = Config::default();
        cfg.games.insert("Cs2.EXE".to_owned(), over("bare"));
        cfg.games
            .insert("cs2.exe@steam:730".to_owned(), over("steam"));
        cfg.games
            .insert("hades.exe@steam:1145360".to_owned(), over("hades"));

        let name = |g: &str| {
            cfg.game_override(g)
                .and_then(|o| o.display_name.clone())
                .unwrap_or_default()
        };
        assert_eq!(name("CS2.exe"), "bare");
        assert_eq!(name("cs2.exe"), "bare");
        assert_eq!(name("cs2.exe@steam:730"), "steam");
        assert_eq!(name("hades.exe"), "hades");
        assert_eq!(name("hades.exe@epic:1"), "");
        assert_eq!(name("missing.exe"), "");
        assert_eq!(name("cs2"), "");
    }

    #[test]
    fn default_toml_round_trips_to_default() {
        let parsed = Config::from_toml_str(Config::default_toml()).expect("template parses");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn retired_keys_load_without_warning() {
        let text = "[video]\nnative_encoder = true\n[advanced]\nffmpeg_dir = 'C:\\ff'\n";
        let (config, warnings) = Config::from_toml_str_checked(text).expect("parses");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn truly_unknown_key_still_warns_next_to_retired_keys() {
        let text = "[advanced]\nffmpeg_dir = ''\nbogus = 1\n";
        let (_, warnings) = Config::from_toml_str_checked(text).expect("parses");
        assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
        assert_eq!(warnings[0].code, "CFG-P004");
        assert_eq!(warnings[0].path, "advanced.bogus");
    }

    #[test]
    fn serialized_default_round_trips() {
        let text = Config::default().to_toml_string().expect("serializes");
        let parsed = Config::from_toml_str(&text).expect("re-parses");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(
            Config::from_toml_str("").expect("parses"),
            Config::default()
        );
    }

    #[test]
    fn key_chord_round_trip() {
        let c: KeyChord = "ctrl+alt+f9".parse().expect("parses");
        assert_eq!(c.to_string(), "Ctrl+Alt+F9");
        assert!("Alt+Alt+F9".parse::<KeyChord>().is_err());
    }

    fn all_mods(mask: u8) -> Modifiers {
        Modifiers {
            ctrl: mask & 1 != 0,
            alt: mask & 2 != 0,
            shift: mask & 4 != 0,
            win: mask & 8 != 0,
        }
    }

    #[test]
    fn key_chord_named_keys_round_trip_through_text_and_json() {
        let keys: Vec<u16> = (0x30..=0x39)
            .chain(0x41..=0x5A)
            .chain(0x70..=0x87)
            .collect();
        for mask in 0..16u8 {
            for &vk in &keys {
                let chord = KeyChord {
                    mods: all_mods(mask),
                    vk,
                };
                let text = chord.to_string();
                assert_eq!(
                    text.parse::<KeyChord>().expect("text parses"),
                    chord,
                    "{text}"
                );
                let json = serde_json::to_string(&chord).expect("serializes");
                assert_eq!(json, format!("\"{text}\""));
                let back: KeyChord = serde_json::from_str(&json).expect("deserializes");
                assert_eq!(back, chord);
            }
        }
    }

    #[test]
    fn key_chord_serialization_is_stable() {
        assert_eq!(
            serde_json::to_string(&KeyChord::alt(0x41)).expect("serializes"),
            "\"Alt+A\""
        );
        let f24 = KeyChord::alt(0x87);
        assert_eq!(f24.to_string(), "Alt+F24");
        assert_eq!(f24.text().as_deref(), Some("Alt+F24"));
    }

    #[test]
    fn key_chord_unnamed_keys_fail_without_panic() {
        for vk in [0x07u16, 0x1B, 0xE8, 0xFF, 0x0000, 0xFFFF] {
            let chord = KeyChord::alt(vk);
            assert!(!chord.has_key_name(), "vk {vk:#X}");
            assert!(chord.text().is_none(), "vk {vk:#X}");
            assert!(serde_json::to_string(&chord).is_err(), "vk {vk:#X}");
        }
    }

    #[test]
    fn key_chord_unnamed_key_refused_by_config_save() {
        let mut config = Config::default();
        config.hotkeys.session.split = Some(KeyChord::alt(0x1B));
        assert!(config.to_toml_string().is_err());
    }

    #[test]
    fn key_chord_display_of_unnamed_keys_never_panics() {
        for vk in [0x07u16, 0x1B, 0xE8, 0xFF, 0xFFFF] {
            let text = KeyChord::alt(vk).to_string();
            assert!(text.contains("0x"), "vk {vk:#X}: {text}");
            assert_eq!(text, format!("Alt+VK 0x{vk:X}"));
        }
        let bare = KeyChord {
            mods: Modifiers::default(),
            vk: 0x07,
        };
        assert_eq!(bare.to_string(), "VK 0x7");
    }

    #[test]
    fn key_chord_unnamed_fallback_is_not_parseable_or_serializable() {
        for vk in [0x07u16, 0x1B, 0xE8, 0xFF] {
            let fallback = KeyChord::alt(vk).to_string();
            assert!(fallback.parse::<KeyChord>().is_err(), "{fallback}");
            assert!(serde_json::to_string(&KeyChord::alt(vk)).is_err());
        }
    }

    #[test]
    fn key_chord_has_key_name_matches_writer_for_every_code() {
        for vk in 0..=u16::MAX {
            let mut out = String::new();
            let written = write_key_name(&mut out, vk).is_ok();
            let bare = KeyChord {
                mods: Modifiers::default(),
                vk,
            };
            assert_eq!(bare.has_key_name(), written, "vk {vk:#X}");
            assert_eq!(bare.text().is_some(), written, "vk {vk:#X}");
            // Alt alone is refused for Space (the window menu), so Alt+Space has no name.
            let alt = KeyChord::alt(vk);
            assert_eq!(alt.has_key_name(), written && vk != 0x20, "vk {vk:#X}");
        }
    }
}
