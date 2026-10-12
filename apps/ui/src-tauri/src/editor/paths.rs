//! Pure path and naming helpers for the editor. Owner: worker `ui-paths`.
//! All functions are pure (take env values as arguments) so they can be unit tested.

use std::path::{Path, PathBuf};

/// Reserved Windows device names (compared case-insensitively, with or without an extension).
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Longest file stem produced by [`sanitize_stem`], in chars.
const MAX_STEM_CHARS: usize = 120;

/// Highest ` (n)` suffix tried by [`unique_output_path`] before the fallback name.
const MAX_SUFFIX: u32 = 9999;

/// `%LOCALAPPDATA%\ClipCheck\editor` (or `<override>` when given), parent of everything below.
pub fn editor_root(local_app_data: Option<&str>) -> PathBuf {
    let local = local_app_data.map(str::trim).filter(|s| !s.is_empty());
    match local {
        Some(dir) => PathBuf::from(dir).join("ClipCheck").join("editor"),
        None => std::env::temp_dir().join("ClipCheck").join("editor"),
    }
}

/// `<editor_root>\projects`.
pub fn projects_dir(editor_root: &Path) -> PathBuf {
    editor_root.join("projects")
}

/// True for ULID-like ids: 10 to 40 ASCII alphanumeric chars.
fn valid_id(id: &str) -> bool {
    (10..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `<projects_dir>\<id>.ccproj`. `id` must be a ULID-like token `[0-9A-Za-z]{10,40}`; returns None
/// otherwise (so ids can never escape the directory).
pub fn project_file(projects_dir: &Path, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    Some(projects_dir.join(format!("{id}.ccproj")))
}

/// `<projects_dir>\<id>.autosave`.
pub fn autosave_file(projects_dir: &Path, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    Some(projects_dir.join(format!("{id}.autosave")))
}

/// `<editor_root>\cache` unless `cache_dir_setting` is non-empty.
pub fn cache_dir(editor_root: &Path, cache_dir_setting: &str) -> PathBuf {
    let setting = cache_dir_setting.trim();
    if setting.is_empty() {
        editor_root.join("cache")
    } else {
        PathBuf::from(setting)
    }
}

/// Waveform cache file: `<cache>\peaks\v2\<size>-<mtime_ms>-<stream>-<buckets>.pk`.
/// The `v2` folder invalidates peaks decoded before the per-stream audio fix (older entries could
/// hold the wrong audio stream).
pub fn peaks_file(cache: &Path, size: u64, mtime_ms: i64, stream: u32, buckets: usize) -> PathBuf {
    cache
        .join("peaks")
        .join("v2")
        .join(format!("{size}-{mtime_ms}-{stream}-{buckets}.pk"))
}

/// Resolves the export folder: `storage_export_dir` if non-empty (with `%VARS%` expanded by the
/// caller), else `<storage_root>\Exports`.
pub fn export_dir(storage_root: &Path, storage_export_dir: &str) -> PathBuf {
    let setting = storage_export_dir.trim();
    if !setting.is_empty() {
        return PathBuf::from(setting);
    }
    let root = storage_root.to_string_lossy();
    let trimmed = root.trim_end_matches(['\\', '/']);
    // A bare drive ("D:") must keep its root separator, or the join would become drive-relative.
    if trimmed.is_empty() || trimmed.ends_with(':') {
        PathBuf::from(format!("{trimmed}\\Exports"))
    } else {
        PathBuf::from(trimmed).join("Exports")
    }
}

/// A path under `dir` named `<stem>.<ext>` that does not exist yet: tries `stem`, then
/// `stem (2)`, `stem (3)`, ... (checks the real filesystem via `exists`, passed in for testing).
/// After 9999 candidates it falls back to `stem (10000).<ext>` without checking.
pub fn unique_output_path(
    dir: &Path,
    stem: &str,
    ext: &str,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let ext = ext.trim_start_matches('.');
    let name_for = |n: u32| -> String {
        let base = if n == 1 {
            stem.to_string()
        } else {
            format!("{stem} ({n})")
        };
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    };
    (1..=MAX_SUFFIX)
        .map(|n| dir.join(name_for(n)))
        .find(|candidate| !exists(candidate))
        .unwrap_or_else(|| dir.join(name_for(MAX_SUFFIX + 1)))
}

/// Makes a string safe as a Windows file stem: removes `\ / : * ? " < > |` and control chars,
/// trims dots and spaces, avoids reserved device names, limits to 120 chars, never empty
/// (falls back to "Export").
pub fn sanitize_stem(s: &str) -> String {
    let kept: String = s
        .chars()
        .filter(|c| {
            !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control()
        })
        .collect();
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c| c == '.' || c == ' ');

    let stem_part = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = RESERVED_NAMES.contains(&stem_part.as_str());
    let guarded = if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    };

    let cut: String = guarded.chars().take(MAX_STEM_CHARS).collect();
    let result = cut.trim_matches(|c| c == '.' || c == ' ');
    if result.is_empty() {
        "Export".to_string()
    } else {
        result.to_string()
    }
}

/// Path components, split on both separators, lower-cased, without empty and `.` parts.
/// Returns None when a `..` component is present.
fn components_ci(p: &Path) -> Option<Vec<String>> {
    let text = p.to_string_lossy();
    let mut out = Vec::new();
    for part in text.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => out.push(other.to_lowercase()),
        }
    }
    Some(out)
}

/// True when `path` is inside `root` (case-insensitive, separator-insensitive, no `..`).
/// Equal paths count as inside.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    let (Some(root_parts), Some(path_parts)) = (components_ci(root), components_ci(path)) else {
        return false;
    };
    path_parts.starts_with(&root_parts)
}
