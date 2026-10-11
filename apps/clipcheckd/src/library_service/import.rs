//! Import jobs: plan, copy and register files in the primary root (DESIGN.md D24).
//!
//! The plan is made on the IPC thread, which only walks the sources and reads free space.
//! [`run_job`] runs on its own thread with its own library connection. Each copied file is
//! registered as a clip with the same catalog call the recorder uses.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clipcheck_core::config::schema::sanitize_game_folder;
use clipcheck_engine::catalog::{probe_media, register_imported, MediaInfo, RegisterMeta};
use clipcheck_engine::import::{
    disk_free_bytes, plan_import, run_import, ImportItem, ImportPlan, ImportProgress,
    ImportRequest, SkipReason,
};
use clipcheck_ipc::proto::{Event, ImportFailDto, ImportItemDto};
use clipcheck_library::{Library, RecordingKind};

use crate::app::LIBRARY_FILE;
use crate::events::EventBus;
use crate::logging;

/// Thumbnail folder inside the data directory.
const THUMBS_DIR: &str = "thumbs";

/// Everything an import job needs. Built on the IPC thread, consumed by [`run_job`].
pub(crate) struct JobSpec {
    /// Job id carried by every event of the job.
    pub job_id: String,
    /// The plan that was checked for space and read-only state.
    pub plan: ImportPlan,
    /// Primary root. Files go under `<root>\Clips\<Game>\`.
    pub root: PathBuf,
    /// Daemon data directory.
    pub data_dir: PathBuf,
    /// Bus for progress and completion events.
    pub bus: Arc<EventBus>,
    /// Set by `import_cancel`.
    pub cancel: Arc<AtomicBool>,
}

/// Plans an import into `root`. Walks the sources and reads free space. Copies nothing.
pub(crate) fn plan_request(root: &Path, paths: &[String], game: Option<&str>) -> ImportPlan {
    let request = ImportRequest {
        sources: paths.iter().map(PathBuf::from).collect(),
        game: game.map(str::to_owned),
        dest_root: root.to_path_buf(),
        copy: true,
    };
    // Duplicates already in the library are not looked up here: the destination folder
    // check in the planner covers files that were imported before.
    plan_import(&request, disk_free_bytes, |_, _, _| false)
}

/// Converts one planned item to its IPC form.
pub(crate) fn item_dto(item: &ImportItem) -> ImportItemDto {
    ImportItemDto {
        src: item.src.display().to_string(),
        name: file_name(&item.src),
        size: item.size,
        game: item.game.clone(),
        dest: item.dest.display().to_string(),
        skip_reason: item.skip_reason.as_ref().map(skip_text),
    }
}

/// Runs an import job to the end. `release` runs before the done event, so a new import can
/// start as soon as the client sees it finish.
pub(crate) fn run_job(spec: JobSpec, release: impl FnOnce()) {
    let JobSpec {
        job_id,
        plan,
        root,
        data_dir,
        bus,
        cancel,
    } = spec;
    let outcome = execute(&job_id, &plan, &root, &data_dir, &bus, &cancel);
    release();
    if outcome.imported > 0 {
        bus.publish(Event::LibraryChanged {});
    }
    logging::info(&format!(
        "import {job_id} finished: {} imported, {} skipped, {} failed{}",
        outcome.imported,
        outcome.skipped,
        outcome.failed.len(),
        if outcome.cancelled { ", cancelled" } else { "" }
    ));
    bus.publish(Event::ImportDone {
        job_id,
        imported: outcome.imported,
        skipped: outcome.skipped,
        failed: outcome.failed,
        cancelled: outcome.cancelled,
    });
}

/// Totals of one job, ready for the done event.
struct Outcome {
    imported: u64,
    skipped: u64,
    failed: Vec<ImportFailDto>,
    cancelled: bool,
}

fn execute(
    job_id: &str,
    plan: &ImportPlan,
    root: &Path,
    data_dir: &Path,
    bus: &EventBus,
    cancel: &AtomicBool,
) -> Outcome {
    let lib = match Library::open(data_dir.join(LIBRARY_FILE)) {
        Ok(lib) => lib,
        Err(e) => {
            return Outcome {
                imported: 0,
                skipped: 0,
                failed: vec![ImportFailDto {
                    name: String::new(),
                    error: format!("cannot open the library: {e}"),
                }],
                cancelled: false,
            };
        }
    };
    let thumbs = data_dir.join(THUMBS_DIR);
    let probe = |path: &Path| -> std::io::Result<MediaInfo> { probe_media(path) };
    let mut register =
        |dest: &Path, game: &str, _info: &MediaInfo, mtime_ms: i64| -> Result<String, String> {
            let game_id = game_id_for(&lib, game)?;
            register_imported(
                &lib,
                &thumbs,
                root,
                dest,
                RegisterMeta {
                    kind: RecordingKind::Clip,
                    game: &game_id,
                    session_id: None,
                    part_index: None,
                    created_ms: mtime_ms,
                },
            )
            .map_err(|e| e.to_string())
        };
    let mut progress = |p: &ImportProgress| {
        bus.publish(Event::ImportProgress {
            job_id: job_id.to_owned(),
            file_index: p.file_index as u64,
            file_count: p.file_count as u64,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            current_name: p.current_name.clone(),
        });
    };
    let report = run_import(plan, &probe, &mut register, &mut progress, cancel);
    Outcome {
        imported: report.imported.len() as u64,
        skipped: report.skipped.len() as u64,
        failed: report
            .failed
            .iter()
            .map(|(src, error)| ImportFailDto {
                name: file_name(src),
                error: error.clone(),
            })
            .collect(),
        cancelled: report.cancelled,
    }
}

/// The game id to register an import under. An existing game with the same folder keeps
/// its id. Otherwise the display name becomes the id, so the folder and name match.
fn game_id_for(lib: &Library, game: &str) -> Result<String, String> {
    let folder = sanitize_game_folder(game);
    match lib.game_id_for_folder(&folder) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Ok(game.to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

fn skip_text(reason: &SkipReason) -> String {
    match reason {
        SkipReason::Unsupported(_) => "unsupported_extension".to_owned(),
        SkipReason::AlreadyInLibrary => "already_in_library".to_owned(),
        SkipReason::DuplicateInImport => "duplicate_in_import".to_owned(),
        SkipReason::Unreadable(msg) => format!("unreadable: {msg}"),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
