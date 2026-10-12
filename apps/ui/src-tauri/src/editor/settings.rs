//! Editor / export user settings (UI-owned JSON file, decision D72). Owner: worker `ui-settings`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use clipcheck_timeline::export::Preset;
use serde::{Deserialize, Serialize};

use super::paths;

/// Largest settings file accepted by [`load`], in bytes. Larger files count as invalid.
const MAX_SETTINGS_BYTES: u64 = 2 * 1024 * 1024;

/// Most custom presets kept by [`normalize`].
const MAX_CUSTOM_PRESETS: usize = 50;

/// Longest file name template kept by [`normalize`], in chars.
const MAX_TEMPLATE_CHARS: usize = 200;

/// Default file name template, also used when the stored one is blank.
const DEFAULT_TEMPLATE: &str = "{project} {yyyy-MM-dd HH-mm-ss}";

/// Default export preset id.
const DEFAULT_PRESET: &str = "balanced_1080p30";

/// Stored in `<editor_root>\settings.json`. Missing fields take defaults; unknown fields ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditorSettings {
    /// Autosave interval in seconds (10..=600).
    pub autosave_s: u32,
    /// Magnetic mode default for new projects.
    pub magnetic_default: bool,
    /// Snapping default for new projects.
    pub snap_default: bool,
    /// "match_first_clip" or a preset id ("16:9", "9:16", "1:1", "4:5").
    pub default_canvas: String,
    /// "auto", "full", "half", "quarter".
    pub preview_quality: String,
    /// Cache folder override ("" = default).
    pub cache_dir: String,
    /// Cache size limit in GB (1..=100).
    pub cache_max_gb: u32,
    /// Decoder cache budget in MB (128..=4096).
    pub decode_cache_mb: u32,
    /// Default export preset id.
    pub default_preset: String,
    /// Default encoder ("auto", "software", or a name).
    pub encoder: String,
    /// Pause exports while a game recording is active.
    pub pause_while_recording: bool,
    /// Open the export folder when an export finishes.
    pub open_folder_when_done: bool,
    /// Default file name template.
    pub filename_template: String,
    /// Run export threads at below-normal priority.
    pub low_priority: bool,
    /// Ask for a location on every export.
    pub always_ask_location: bool,
    /// Last export folder chosen in the dialog ("" = configured default).
    pub last_export_dir: String,
    /// Keep exports and project media out of auto-delete.
    pub protect_exports_from_quota: bool,
    /// User presets.
    pub custom_presets: Vec<Preset>,
}

impl Default for EditorSettings {
    fn default() -> Self {
        EditorSettings {
            autosave_s: 30,
            magnetic_default: true,
            snap_default: true,
            default_canvas: "match_first_clip".to_string(),
            preview_quality: "auto".to_string(),
            cache_dir: String::new(),
            cache_max_gb: 5,
            decode_cache_mb: 512,
            default_preset: "balanced_1080p30".to_string(),
            encoder: "auto".to_string(),
            pause_while_recording: true,
            open_folder_when_done: false,
            filename_template: "{project} {yyyy-MM-dd HH-mm-ss}".to_string(),
            low_priority: true,
            always_ask_location: false,
            last_export_dir: String::new(),
            protect_exports_from_quota: true,
            custom_presets: Vec::new(),
        }
    }
}

/// Clamps every field into its documented range and replaces unknown enum strings with defaults.
/// Strings are trimmed; custom presets without an id, or flagged `builtin`, are dropped, duplicate
/// ids keep the last occurrence, and at most 50 presets are kept.
pub fn normalize(s: &mut EditorSettings) {
    s.autosave_s = s.autosave_s.clamp(10, 600);
    s.cache_max_gb = s.cache_max_gb.clamp(1, 100);
    s.decode_cache_mb = s.decode_cache_mb.clamp(128, 4096);

    s.preview_quality = match s.preview_quality.trim() {
        q @ ("auto" | "full" | "half" | "quarter") => q.to_string(),
        _ => "auto".to_string(),
    };
    s.default_canvas = match s.default_canvas.trim() {
        c @ ("match_first_clip" | "16:9" | "9:16" | "1:1" | "4:5") => c.to_string(),
        _ => "match_first_clip".to_string(),
    };

    s.cache_dir = s.cache_dir.trim().to_string();
    s.last_export_dir = s.last_export_dir.trim().to_string();

    let template = s.filename_template.trim();
    s.filename_template = if template.is_empty() {
        DEFAULT_TEMPLATE.to_string()
    } else {
        template.chars().take(MAX_TEMPLATE_CHARS).collect()
    };

    let encoder = s.encoder.trim();
    s.encoder = if encoder.is_empty() {
        "auto".to_string()
    } else {
        encoder.to_string()
    };

    let preset = s.default_preset.trim();
    s.default_preset = if preset.is_empty() {
        DEFAULT_PRESET.to_string()
    } else {
        preset.to_string()
    };

    let mut seen = HashSet::new();
    let mut kept: Vec<Preset> = Vec::new();
    for mut preset in std::mem::take(&mut s.custom_presets).into_iter().rev() {
        preset.id = preset.id.trim().to_string();
        if preset.id.is_empty() || preset.builtin {
            continue;
        }
        if seen.insert(preset.id.clone()) {
            kept.push(preset);
        }
    }
    kept.reverse();
    kept.truncate(MAX_CUSTOM_PRESETS);
    s.custom_presets = kept;
}

/// Loads settings from `path`; a missing or unreadable/invalid file gives defaults. Always
/// normalised.
pub fn load(path: &Path) -> EditorSettings {
    let mut settings = read_settings(path).unwrap_or_default();
    normalize(&mut settings);
    settings
}

/// Reads and parses the file; any failure (missing, too large, bad JSON) gives None.
fn read_settings(path: &Path) -> Option<EditorSettings> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > MAX_SETTINGS_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Saves settings atomically (`.part` then rename). Normalises first.
pub fn save(path: &Path, s: &EditorSettings) -> Result<(), String> {
    let mut normalised = s.clone();
    normalize(&mut normalised);
    let json = serde_json::to_string_pretty(&normalised).map_err(|e| e.to_string())?;

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("create settings folder: {e}"))?;
    }

    let mut part_name = path.as_os_str().to_owned();
    part_name.push(".part");
    let part = PathBuf::from(part_name);
    fs::write(&part, json).map_err(|e| format!("write settings: {e}"))?;
    if let Err(e) = fs::rename(&part, path) {
        fs::remove_file(&part).ok();
        return Err(format!("replace settings: {e}"));
    }
    Ok(())
}

/// Path of the settings file: `<editor_root>\settings.json`, recomputed on each call.
pub fn settings_path() -> PathBuf {
    paths::editor_root(std::env::var("LOCALAPPDATA").ok().as_deref()).join("settings.json")
}

/// Tauri: current settings.
#[tauri::command]
pub fn editor_get_settings() -> Result<EditorSettings, String> {
    Ok(load(&settings_path()))
}

/// Tauri: saves settings and returns the normalised result.
#[tauri::command]
pub fn editor_set_settings(settings: EditorSettings) -> Result<EditorSettings, String> {
    let mut normalised = settings;
    normalize(&mut normalised);
    save(&settings_path(), &normalised)?;
    Ok(normalised)
}
