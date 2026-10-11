//! IPC handlers for recording management: rename, delete and register an edited file.
//!
//! The handlers run on the IPC connection thread. Each call opens its own connection to
//! `library.db`, the same pattern the import and scan jobs use; SQLite serialises the writers
//! with a 5 s busy timeout. Every change that alters the library publishes `LibraryChanged`.
//!
//! Only folders in the configured storage settings are touched. Read-only library folders
//! refuse rename and delete. Paths are rebuilt from the stored root and a relative path that
//! may not contain `..` or a root component.

use std::fs;
use std::path::{Component, Path, PathBuf};

use clipcheck_engine::catalog::{register_recording, CatalogError, RegisterMeta};
use clipcheck_ipc::proto::{ErrorCode, Event, Reply};
use clipcheck_library::fsops::FsOpError;
use clipcheck_library::schema::now_ms;
use clipcheck_library::{rename_on_disk_and_db, Library, RecStatus, RecordingKind, RecordingRow};

use crate::app::{AppState, LIBRARY_FILE};
use crate::library_service::error;

/// Thumbnail folder under the daemon data directory (same name as the recording service uses).
const THUMBS_DIR: &str = "thumbs";

/// Renames a recording's file and its database row. Replies `Ok` on success.
pub(crate) fn rename(state: &AppState, recording_id: &str, name: &str) -> Reply {
    let lib = match open(state) {
        Ok(lib) => lib,
        Err(reply) => return reply,
    };
    let row = match lookup(&lib, recording_id) {
        Ok(row) => row,
        Err(reply) => return reply,
    };
    if let Err(reply) = check_writable(state, &row) {
        return reply;
    }
    if is_open_for_writing(&row) {
        return busy();
    }
    let root = PathBuf::from(&row.root_path);
    match rename_on_disk_and_db(&lib, recording_id, name, &root) {
        Ok(outcome) => {
            if outcome.changed {
                state.bus.publish(Event::LibraryChanged {});
            }
            Reply::Ok
        }
        Err(e) => fs_error(e),
    }
}

/// Deletes a recording. The database row is removed first, then the file goes to the Recycle
/// Bin (or is deleted permanently when `storage.delete_to_recycle_bin` is off). Replies `Ok`.
///
/// If the file operation fails after the row was removed, the file stays on disk and the next
/// scan lists it again. The reply then carries an `internal` error that says so.
pub(crate) fn delete(state: &AppState, recording_id: &str) -> Reply {
    let lib = match open(state) {
        Ok(lib) => lib,
        Err(reply) => return reply,
    };
    let row = match lookup(&lib, recording_id) {
        Ok(row) => row,
        Err(reply) => return reply,
    };
    if let Err(reply) = check_writable(state, &row) {
        return reply;
    }
    if is_open_for_writing(&row) {
        return busy();
    }
    let path = match resolve(&row) {
        Ok(path) => path,
        Err(reply) => return reply,
    };
    if path.exists() && !path.is_file() {
        return error(
            ErrorCode::InvalidState,
            "the recording path is not a file",
            false,
        );
    }

    if let Err(e) = lib.delete_recording_record(&row.id) {
        return error(
            ErrorCode::Internal,
            format!("cannot remove the recording from the library: {e}"),
            false,
        );
    }
    state.bus.publish(Event::LibraryChanged {});
    remove_thumbnails(state, &row.id);

    let file_result = if row.status == RecStatus::Missing || !path.exists() {
        Ok(())
    } else if state.config.storage.delete_to_recycle_bin {
        recycle(&path)
    } else {
        fs::remove_file(&path)
    };
    match file_result {
        Ok(()) => Reply::Ok,
        Err(e) => error(
            ErrorCode::Internal,
            format!(
                "the library entry was removed but the file could not be deleted ({e}); \
                 the next scan will list it again"
            ),
            true,
        ),
    }
}

/// Sets or clears the library favourite flag of a recording. Only the library row changes, so
/// read-only folders are allowed. Publishes `LibraryChanged` and replies `Ok`.
pub(crate) fn set_favorite(state: &AppState, recording_id: &str, favorite: bool) -> Reply {
    let lib = match open(state) {
        Ok(lib) => lib,
        Err(reply) => return reply,
    };
    if let Err(reply) = lookup(&lib, recording_id) {
        return reply;
    }
    match lib.set_favorite(recording_id, favorite) {
        Ok(()) => {
            state.bus.publish(Event::LibraryChanged {});
            Reply::Ok
        }
        Err(e) => error(ErrorCode::Internal, format!("library: {e}"), false),
    }
}

/// Registers a finished file, typically a trimmer output, as a recording. The file must lie
/// under a configured library folder. `source_recording_id` supplies the game of the parent
/// recording. Replies `RecordingRegistered` with the new id.
///
/// When `keep_source_time` is true and the source recording exists, the new recording takes the
/// source's creation time, and its favourite flag if the source is a favourite.
///
/// The recording kind is `clip`. The library schema has no parent link column, so the link to
/// `source_recording_id` is not stored; only the game is taken from it.
pub(crate) fn register_edited(
    state: &AppState,
    source_recording_id: Option<&str>,
    path: &str,
    keep_source_time: bool,
) -> Reply {
    let file = PathBuf::from(path);
    if !file.is_absolute() || !file.is_file() {
        return error(
            ErrorCode::BadRequest,
            "path must be an existing absolute file path",
            false,
        );
    }
    let lib = match open(state) {
        Ok(lib) => lib,
        Err(reply) => return reply,
    };
    let mut source_time: Option<(i64, bool)> = None;
    let game = match source_recording_id {
        Some(id) => match lookup(&lib, id) {
            Ok(row) => {
                if keep_source_time {
                    source_time = Some((row.created_at_ms, row.is_favorite));
                }
                row.game_id.unwrap_or_default()
            }
            Err(reply) => return reply,
        },
        None => String::new(),
    };
    let Some(root) = configured_root_for(state, path) else {
        return error(
            ErrorCode::BadRequest,
            "the file is not inside a configured library folder",
            false,
        );
    };
    if !state.service.library().is_ready() {
        return error(
            ErrorCode::WorkerUnavailable,
            "The recorder is still starting",
            true,
        );
    }

    let thumbs = state.service.library().data_dir().join(THUMBS_DIR);
    let meta = RegisterMeta {
        kind: RecordingKind::Clip,
        game: &game,
        session_id: None,
        part_index: None,
        created_ms: source_time.map_or_else(now_ms, |(created, _)| created),
    };
    match register_recording(&lib, &thumbs, &root, &file, meta) {
        Ok(recording_id) => {
            if let Some((_, true)) = source_time {
                if let Err(e) = lib.set_favorite(&recording_id, true) {
                    eprintln!("cannot copy the favourite flag to {recording_id}: {e}");
                }
            }
            state.bus.publish(Event::LibraryChanged {});
            Reply::RecordingRegistered { recording_id }
        }
        Err(CatalogError::Lib(e)) => error(
            ErrorCode::Internal,
            format!("cannot register the recording: {e}"),
            false,
        ),
        Err(CatalogError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData => error(
            ErrorCode::BadRequest,
            format!("the file is not a readable video: {e}"),
            false,
        ),
        Err(CatalogError::Io(e)) => error(
            ErrorCode::Internal,
            format!("cannot read the file: {e}"),
            true,
        ),
        Err(e @ (CatalogError::NotUnderRoot { .. } | CatalogError::UnsupportedExt(_))) => {
            error(ErrorCode::BadRequest, e.to_string(), false)
        }
    }
}

/// Opens this call's own connection to `library.db`.
fn open(state: &AppState) -> Result<Library, Reply> {
    let db = state.service.library().data_dir().join(LIBRARY_FILE);
    Library::open(&db).map_err(|e| {
        error(
            ErrorCode::Internal,
            format!("cannot open the library: {e}"),
            true,
        )
    })
}

/// Loads one recording row, or a `bad_request` reply when the id is unknown.
fn lookup(lib: &Library, recording_id: &str) -> Result<RecordingRow, Reply> {
    match lib.get_recording(recording_id) {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(error(
            ErrorCode::BadRequest,
            "no recording has that id",
            false,
        )),
        Err(e) => Err(error(ErrorCode::Internal, format!("library: {e}"), false)),
    }
}

/// True while the recording is still being written or probed.
fn is_open_for_writing(row: &RecordingRow) -> bool {
    matches!(row.status, RecStatus::Recording | RecStatus::Pending)
}

fn busy() -> Reply {
    error(
        ErrorCode::Busy,
        "the recording is still being written",
        true,
    )
}

/// Maps a rename failure to an IPC error.
fn fs_error(e: FsOpError) -> Reply {
    match e {
        FsOpError::Name(name) => error(
            ErrorCode::BadRequest,
            format!("invalid name: {name}"),
            false,
        ),
        FsOpError::NotFound => error(ErrorCode::BadRequest, "no recording has that id", false),
        FsOpError::Busy => busy(),
        FsOpError::MissingOnDisk => error(
            ErrorCode::InvalidState,
            "the file is missing on disk",
            false,
        ),
        FsOpError::Collision(name) => error(
            ErrorCode::InvalidState,
            format!("a file named '{name}' already exists"),
            false,
        ),
        FsOpError::InUse => error(
            ErrorCode::Busy,
            "the file is in use by another program",
            true,
        ),
        other => error(ErrorCode::Internal, other.to_string(), false),
    }
}

/// Lower-cases a folder path and drops trailing separators, so stored and configured
/// spellings compare equal.
fn norm(path: &str) -> String {
    crate::library_service::sync::expand_env(path)
        .trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Refuses a recording whose root is not a configured folder, or is a read-only folder.
fn check_writable(state: &AppState, row: &RecordingRow) -> Result<(), Reply> {
    let root = norm(&row.root_path);
    let storage = &state.config.storage;
    if norm(&storage.root.to_string_lossy()) == root {
        return Ok(());
    }
    for dir in &storage.libraries {
        if norm(&dir.path) == root {
            return if dir.read_only {
                Err(error(
                    ErrorCode::InvalidState,
                    "this library folder is read-only",
                    false,
                ))
            } else {
                Ok(())
            };
        }
    }
    Err(error(
        ErrorCode::InvalidState,
        "the recording is outside the configured library folders",
        false,
    ))
}

/// The configured folder (primary or extra, read-only or not) that contains `path`. The
/// longest match wins. Returns the folder as configured.
fn configured_root_for(state: &AppState, path: &str) -> Option<PathBuf> {
    let file = norm(path);
    let storage = &state.config.storage;
    std::iter::once(storage.root.clone())
        .chain(storage.libraries.iter().map(|dir| PathBuf::from(&dir.path)))
        .filter(|root| {
            let root = norm(&root.to_string_lossy());
            !root.is_empty() && file.starts_with(&format!("{root}\\"))
        })
        .max_by_key(|root| norm(&root.to_string_lossy()).len())
}

/// Builds the absolute file path of a row. Rejects a root that is not absolute and a relative
/// path with anything but plain names, so the result always lies under the root.
fn resolve(row: &RecordingRow) -> Result<PathBuf, Reply> {
    let root = Path::new(&row.root_path);
    let rel = Path::new(&row.rel_path);
    let plain = !rel.as_os_str().is_empty()
        && rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !root.is_absolute() || !plain {
        return Err(error(
            ErrorCode::Internal,
            "the recording's stored path is invalid",
            false,
        ));
    }
    Ok(root.join(rel))
}

/// Removes the recording's thumbnail folder. Best effort; the id is a database key.
fn remove_thumbnails(state: &AppState, recording_id: &str) {
    if recording_id.is_empty() || !recording_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return;
    }
    let dir = state
        .service
        .library()
        .data_dir()
        .join(THUMBS_DIR)
        .join(recording_id);
    let _ = fs::remove_dir_all(dir);
}

/// Sends a file to the Windows Recycle Bin.
#[cfg(windows)]
fn recycle(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOL, HWND};
    use windows::Win32::UI::Shell::{SHFileOperationW, FO_DELETE, SHFILEOPSTRUCTW};

    // Values of FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT and FOF_NOERRORUI.
    const FOF_SILENT: u16 = 0x0004;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_ALLOWUNDO: u16 = 0x0040;
    const FOF_NOERRORUI: u16 = 0x0400;

    // The source list is a double-NUL-terminated list of paths.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND(std::ptr::null_mut()),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI,
        fAnyOperationsAborted: BOOL(0),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };
    // SAFETY: `op` and the wide path buffer outlive the call, and the buffer is double-NUL
    // terminated as SHFileOperationW requires.
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        return Err(std::io::Error::other(format!(
            "Recycle Bin operation failed with code {rc}"
        )));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err(std::io::Error::other("Recycle Bin operation was aborted"));
    }
    Ok(())
}

/// Non-Windows fallback: the library is Windows-only, so this is a plain removal.
#[cfg(not(windows))]
fn recycle(path: &Path) -> std::io::Result<()> {
    fs::remove_file(path)
}
