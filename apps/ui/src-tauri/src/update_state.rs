//! `update-state.json`: small app-owned state for the in-app updater (doc 11, section 8).
//!
//! The file lives in the data folder (`%APPDATA%\ClipCheck`, or `CLIPCHECK_DATA_DIR`).
//! It is written by the app, not edited by users, and it is not configuration. Reads
//! are tolerant: a missing or corrupt file yields the defaults. Writes go to a temporary
//! file that is then renamed over the target, so a crash never leaves a half-written file.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::commands::DATA_DIR_ENV;

/// File name inside the data folder.
pub const STATE_FILE_NAME: &str = "update-state.json";

/// Contents of `update-state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// Unix time (milliseconds) of the last successful update check.
    pub last_checked_unix_ms: Option<u64>,
    /// Version the user chose to skip. Background checks do not announce it.
    pub skipped_version: Option<String>,
    /// Version that was running before the last update was started from the app.
    pub last_installed_from: Option<String>,
    /// Newest update found by the last check, so a restart still shows it. Cleared when a
    /// check finds nothing newer.
    pub pending: Option<PendingUpdate>,
}

/// An update that was found but not installed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    /// Version of the update.
    pub version: String,
    /// Release notes from the manifest.
    pub notes: String,
    /// Publication date from the manifest (RFC 3339), if present.
    pub pub_date: Option<String>,
}

/// Returns the data folder: `CLIPCHECK_DATA_DIR` when set, else `%APPDATA%\ClipCheck`.
/// `None` when neither is available.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_from(
        std::env::var(DATA_DIR_ENV).ok().as_deref(),
        std::env::var("APPDATA").ok().as_deref(),
    )
}

/// Pure form of [`data_dir`], for tests.
pub fn data_dir_from(data_dir: Option<&str>, appdata: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    appdata
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(|a| PathBuf::from(a).join("ClipCheck"))
}

/// Path of `update-state.json` in the data folder, if there is one.
pub fn state_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(STATE_FILE_NAME))
}

/// Reads the state at `path`. A missing, unreadable or corrupt file yields the defaults.
pub fn load_from(path: &Path) -> UpdateState {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => UpdateState::default(),
    }
}

/// Writes `state` to `path` atomically: a temporary sibling file, then a rename over the
/// target. Creates the parent folder when needed.
pub fn save_to(path: &Path, state: &UpdateState) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| STATE_FILE_NAME.into());
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);

    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Loads the current state from the data folder. Defaults when there is no folder.
pub fn load() -> UpdateState {
    state_path().map(|p| load_from(&p)).unwrap_or_default()
}

/// Applies `change` to the stored state and writes the result back.
pub fn update<F: FnOnce(&mut UpdateState)>(change: F) -> io::Result<()> {
    let path = state_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no data folder (APPDATA is not set)",
        )
    })?;
    let mut state = load_from(&path);
    change(&mut state);
    save_to(&path, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_from(&dir.path().join(STATE_FILE_NAME)),
            UpdateState::default()
        );
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE_NAME);
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_from(&path), UpdateState::default());
    }

    #[test]
    fn round_trip_and_overwrite_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(STATE_FILE_NAME);
        let first = UpdateState {
            last_checked_unix_ms: Some(1_700_000_000_000),
            skipped_version: Some("0.3.0".into()),
            last_installed_from: None,
            pending: None,
        };
        save_to(&path, &first).unwrap();
        assert_eq!(load_from(&path), first);

        let second = UpdateState {
            last_installed_from: Some("0.2.0".into()),
            ..first.clone()
        };
        save_to(&path, &second).unwrap();
        assert_eq!(load_from(&path), second);
        assert!(!path.with_file_name("update-state.json.tmp").exists());
    }

    #[test]
    fn data_dir_prefers_env_then_appdata() {
        assert_eq!(
            data_dir_from(Some(" D:\\x "), Some("C:\\a")),
            Some(PathBuf::from("D:\\x"))
        );
        assert_eq!(
            data_dir_from(Some(""), Some("C:\\a")),
            Some(PathBuf::from("C:\\a").join("ClipCheck"))
        );
        assert_eq!(data_dir_from(None, None), None);
    }
}
