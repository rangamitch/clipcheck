//! Recording details for the player's details panel (D60). Contract: docs/impl/player-v2.md.

use std::path::{Path, PathBuf};

use clipcheck_library::Library;
use serde::Serialize;

use crate::media::{resolve_recording_path, MediaError};

/// File facts only the backend knows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingDetailsDto {
    pub id: String,
    pub path: String,
    pub dir: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub exists: bool,
}

/// Resolves the recording's path. A known row whose file is missing still yields its path
/// (taken from the database) with `exists == false`; an unknown id is an error.
fn locate(db: &Path, id: &str) -> Result<(PathBuf, bool), String> {
    match resolve_recording_path(db, id) {
        Ok(path) => Ok((path, true)),
        Err(MediaError::NotFound) => {
            // Either the id is unknown or the file is missing. Only the database can tell.
            let lib = Library::open_readonly(db)
                .map_err(|e| format!("could not open the library: {e}"))?;
            let row = lib
                .get_recording(id)
                .map_err(|e| format!("could not read the library: {e}"))?
                .ok_or_else(|| "could not find the recording".to_string())?;
            Ok((Path::new(&row.root_path).join(&row.rel_path), false))
        }
        Err(MediaError::BadId) => Err("could not find the recording: bad recording id".into()),
        Err(e) => Err(format!("could not find the recording: {e}")),
    }
}

/// Builds the details for recording `id` from the library at `db`.
pub fn details_at(db: &Path, id: &str) -> Result<RecordingDetailsDto, String> {
    let (path, exists) = locate(db, id)?;
    let size_bytes = if exists {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(RecordingDetailsDto {
        id: id.to_string(),
        path: path.display().to_string(),
        dir,
        file_name,
        size_bytes,
        exists: path.is_file(),
    })
}

/// Tauri command: full path and on-disk facts for a recording.
#[tauri::command]
pub fn get_recording_details(id: String) -> Result<RecordingDetailsDto, String> {
    let db = crate::commands::try_default_db_path().map_err(|e| e.to_string())?;
    details_at(&db, &id)
}
