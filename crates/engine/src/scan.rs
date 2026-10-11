//! Indexing of external library folders in place (DESIGN.md D24, 07 §5.5.5).
//!
//! Three phases keep database access on one thread:
//!
//! 1. [`walk_root`]: read-only, iterative directory walk. Pure I/O, no database.
//! 2. [`plan_scan`]: pure diff of the walk against the rows of a root.
//! 3. [`apply_scan`]: probes new and changed files on a small worker pool (read-only,
//!    in process) and writes the results in batches on the calling thread.
//!
//! [`scan_root`] runs the three phases for one root. Nothing here writes to, renames or deletes
//! files under the scanned root.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf, MAIN_SEPARATOR};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use clipcheck_library::schema::now_ms;
use clipcheck_library::{
    ExternalRecording, LibError, Library, NewGame, PathRow, RecStatus, RecordingKind,
};
use ulid::Ulid;

use crate::catalog::{make_thumbnail, probe_media, MediaInfo};

/// Rows written to the library per transaction.
const BATCH: usize = 100;
/// Files between walk progress reports.
const WALK_PROGRESS_EVERY: usize = 200;

/// Settings for a scan. [`ScanOptions::default`] is the Medal-shaped default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// Lower-case extensions to index (without the dot).
    pub extensions: Vec<String>,
    /// Lower-case directory names that are never entered (compared case-insensitively).
    pub ignore_dirs: Vec<String>,
    /// Lower-case file name prefixes to skip (recycle-bin leftovers and temp files).
    pub ignore_file_prefixes: Vec<String>,
    /// Files smaller than this are skipped.
    pub min_bytes: u64,
    /// Number of concurrent probe worker threads during [`apply_scan`].
    pub probe_concurrency: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        // `webm` is not listed: the library stores only mp4 and mkv containers.
        ScanOptions {
            extensions: ["mp4", "m4v", "mov", "mkv"].map(str::to_owned).to_vec(),
            ignore_dirs: [
                "$recycle.bin",
                "system volume information",
                "thumbnails",
                "temp",
                "video-editor",
                "databases",
                ".thumbnails",
                "node_modules",
            ]
            .map(str::to_owned)
            .to_vec(),
            ignore_file_prefixes: ["$r", "$i", "~"].map(str::to_owned).to_vec(),
            min_bytes: 64 * 1024,
            probe_concurrency: 3,
        }
    }
}

/// A regular file found by [`walk_root`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundFile {
    /// Path relative to the scanned root, using the platform separator.
    pub rel_path: String,
    /// Size in bytes at walk time.
    pub size: i64,
    /// Modification time in UTC milliseconds at walk time.
    pub mtime_ms: i64,
}

/// Result of [`walk_root`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Walk {
    /// Matching files, sorted by `rel_path`.
    pub files: Vec<FoundFile>,
    /// Relative paths (directories or files) that could not be read. Rows under them must not
    /// be marked missing, because their absence from `files` says nothing about the disk.
    pub unreadable: Vec<String>,
}

/// The diff between a walk and the rows of a root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanPlan {
    /// Files on disk with no row.
    pub new: Vec<FoundFile>,
    /// Rows whose file changed (size or mtime), or that were missing, pending or in error and
    /// are on disk again. Paired with the row id.
    pub changed: Vec<(String, FoundFile)>,
    /// Row ids whose file is no longer on disk.
    pub missing: Vec<String>,
    /// Rows that matched a file with the same size and mtime (or are being recorded).
    pub unchanged: usize,
}

/// Progress reported by [`scan_root`] and [`apply_scan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanProgress {
    /// Walking the folder; `found` matching files so far.
    Walking {
        /// Matching files found so far.
        found: usize,
    },
    /// Probing new and changed files; `done` of `total` processed.
    Probing {
        /// Files processed.
        done: usize,
        /// Files to probe.
        total: usize,
    },
}

/// Outcome of a scan of one root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// The root that was scanned.
    pub root_id: i64,
    /// Rows inserted.
    pub added: usize,
    /// Rows refreshed (changed, or restored from missing).
    pub updated: usize,
    /// Rows marked missing.
    pub marked_missing: usize,
    /// Rows left alone because nothing changed.
    pub unchanged: usize,
    /// Files whose probe failed, as `(path, error)`. Retried on the next scan.
    pub failed: Vec<(String, String)>,
    /// True if the root directory was not reachable. Nothing was marked missing.
    pub offline: bool,
    /// True if the scan stopped early because of cancellation.
    pub cancelled: bool,
    /// Wall-clock duration of the scan.
    pub elapsed: Duration,
}

/// Errors from the scan functions.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// A library call failed.
    #[error("library: {0}")]
    Lib(#[from] LibError),
    /// A filesystem error outside the per-file probe.
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// Walks `root` read-only and returns every file with an accepted extension, size and name.
///
/// Iterative (no recursion limit). Directories in `ignore_dirs` are not entered. Symbolic links
/// and junctions are not followed, and each real directory is visited once. A directory or file
/// that cannot be read is recorded in [`Walk::unreadable`] and the walk continues. Progress is
/// reported every 200 files. If `cancel` is set the walk stops with `ErrorKind::Interrupted`.
/// If `root` itself cannot be read the error is returned.
pub fn walk_root(
    root: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize),
) -> io::Result<Walk> {
    if !fs::metadata(root)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "root is not a directory",
        ));
    }
    let mut out = Walk::default();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel_dir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "walk cancelled"));
        }
        let key = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !visited.insert(key) {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if rel_dir.is_empty() => return Err(e),
            Err(_) => {
                out.unreadable.push(rel_dir);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    out.unreadable.push(rel_dir.clone());
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = join_rel(&rel_dir, &name);
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => {
                    out.unreadable.push(rel);
                    continue;
                }
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if !is_ignored_dir(&name, opts) {
                    stack.push((entry.path(), rel));
                }
                continue;
            }
            if !ft.is_file() || !accepted_file(&name, opts) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    out.unreadable.push(rel);
                    continue;
                }
            };
            if meta.len() < opts.min_bytes {
                continue;
            }
            out.files.push(FoundFile {
                rel_path: rel,
                size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
                mtime_ms: mtime_ms(&meta),
            });
            if out.files.len() % WALK_PROGRESS_EVERY == 0 {
                progress(out.files.len());
            }
        }
    }
    progress(out.files.len());
    out.files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out.unreadable.sort();
    out.unreadable.dedup();
    Ok(out)
}

/// Diffs a walk against the rows of one root.
///
/// `existing` is `(id, rel_path, size_bytes, file_mtime_ms, status)`. Paths match
/// case-insensitively on Windows. A row is `changed` when its file's size or mtime differs, or
/// when its status is `missing`, `pending` or `error` (a file that reappeared is restored to
/// ready by the apply step). Rows with status `recording` are never touched. A row whose file is
/// absent and whose status is not `missing` or `recording` is `missing`.
pub fn plan_scan(found: &[FoundFile], existing: &[PathRow]) -> ScanPlan {
    plan_scan_inner(found, existing, &|_| false)
}

/// Same diff as [`plan_scan`], but a row absent from `found` is `missing` only if
/// `exists(rel_path)` is false at decision time.
///
/// `exists` receives the row's relative path. A walk can miss a file that was registered after
/// the directory listing was read, so a row whose file is still on disk must never be marked
/// missing. Every other rule matches [`plan_scan`].
pub fn plan_scan_checked(
    found: &[FoundFile],
    existing: &[PathRow],
    exists: &dyn Fn(&str) -> bool,
) -> ScanPlan {
    plan_scan_inner(found, existing, exists)
}

fn plan_scan_inner(
    found: &[FoundFile],
    existing: &[PathRow],
    exists: &dyn Fn(&str) -> bool,
) -> ScanPlan {
    let by_key: HashMap<String, &PathRow> =
        existing.iter().map(|row| (path_key(&row.1), row)).collect();
    let mut plan = ScanPlan::default();
    let mut seen: HashSet<String> = HashSet::with_capacity(found.len());
    for f in found {
        let key = path_key(&f.rel_path);
        seen.insert(key.clone());
        match by_key.get(&key) {
            None => plan.new.push(f.clone()),
            Some((id, _, size, mtime, status)) => match status {
                RecStatus::Recording => plan.unchanged += 1,
                RecStatus::Missing | RecStatus::Pending | RecStatus::Error => {
                    plan.changed.push((id.clone(), f.clone()));
                }
                RecStatus::Ready if *size != f.size || *mtime != f.mtime_ms => {
                    plan.changed.push((id.clone(), f.clone()));
                }
                RecStatus::Ready => plan.unchanged += 1,
            },
        }
    }
    for (id, rel, _, _, status) in existing {
        if !seen.contains(&path_key(rel))
            && !matches!(status, RecStatus::Missing | RecStatus::Recording)
            && !exists(rel)
        {
            plan.missing.push(id.clone());
        }
    }
    plan
}

/// The root being scanned: its database id and its folder on disk.
#[derive(Clone, Copy, Debug)]
pub struct RootTarget<'a> {
    /// .
    pub root_id: i64,
    /// Absolute path of the folder.
    pub root: &'a Path,
}

/// Probes new and changed files and writes the results.
///
/// Probes run on `opts.probe_concurrency` worker threads, each reading the file in process.
/// Results are written on the calling thread in transactions of about 100 rows. A probe failure
/// writes nothing and is listed in [`ScanReport::failed`]. Missing rows are marked and the root
/// is stamped with the scan time only when the scan was not cancelled.
///
/// Game and kind come from [`derive_game_and_kind`] for new rows. Changed rows keep their game.
pub fn apply_scan(
    lib: &Library,
    target: RootTarget<'_>,
    plan: &ScanPlan,
    opts: &ScanOptions,
    progress: &mut dyn FnMut(ScanProgress),
    cancel: &AtomicBool,
) -> Result<ScanReport, ScanError> {
    let started = Instant::now();
    let RootTarget { root_id, root } = target;
    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut items: Vec<(Option<String>, &FoundFile)> =
        Vec::with_capacity(plan.new.len() + plan.changed.len());
    items.extend(plan.new.iter().map(|f| (None, f)));
    items.extend(plan.changed.iter().map(|(id, f)| (Some(id.clone()), f)));
    let total = items.len();

    let mut report = ScanReport {
        root_id,
        unchanged: plan.unchanged,
        ..ScanReport::default()
    };
    let now = now_ms();
    let next = AtomicUsize::new(0);
    let workers = opts.probe_concurrency.clamp(1, 16).min(total.max(1));
    let (tx, rx) = mpsc::channel::<(usize, Result<MediaInfo, String>)>();
    let mut games: HashMap<String, String> = HashMap::new();
    let mut new_batch: Vec<ExternalRecording> = Vec::new();
    let mut upd_batch: Vec<ExternalRecording> = Vec::new();
    let mut done = 0usize;
    let mut consumer: Result<(), ScanError> = Ok(());

    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let items = &items;
            let next = &next;
            s.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let path = root.join(&items[i].1.rel_path);
                let res = probe_media(&path).map_err(|e| e.to_string());
                if tx.send((i, res)).is_err() {
                    break;
                }
            });
        }
        drop(tx);

        consumer = (|| -> Result<(), ScanError> {
            for (i, res) in rx.iter() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                done += 1;
                progress(ScanProgress::Probing { done, total });
                let (existing_id, f) = &items[i];
                let path_text = root.join(&f.rel_path).display().to_string();
                let info = match res {
                    Ok(info) => info,
                    Err(msg) => {
                        report.failed.push((path_text, msg));
                        continue;
                    }
                };
                let Some((db_ext, playable)) = container_for(&f.rel_path) else {
                    report
                        .failed
                        .push((path_text, "unsupported container".to_owned()));
                    continue;
                };
                let (kind, game_id) = match existing_id {
                    Some(_) => (RecordingKind::Clip, None),
                    None => {
                        let (game, kind) = derive_game_and_kind(&root_name, &f.rel_path);
                        let gid = resolve_game(lib, &mut games, &game, now)?;
                        (kind, Some(gid))
                    }
                };
                let record = ExternalRecording {
                    id: existing_id
                        .clone()
                        .unwrap_or_else(|| Ulid::new().to_string()),
                    kind,
                    game_id,
                    root_id,
                    rel_path: f.rel_path.clone(),
                    file_name: file_name_of(&f.rel_path),
                    ext: db_ext.to_owned(),
                    size_bytes: f.size,
                    file_mtime_ms: f.mtime_ms,
                    created_at_ms: f.mtime_ms,
                    added_at_ms: now,
                    duration_ms: info.duration_ms,
                    width: (info.width > 0).then_some(i64::from(info.width)),
                    height: (info.height > 0).then_some(i64::from(info.height)),
                    fps: (info.fps > 0.0)
                        .then(|| ((f64::from(info.fps) * 1000.0).round() as i64, 1000)),
                    video_codec: (!info.video_codec.is_empty()).then(|| info.video_codec.clone()),
                    audio_track_count: i64::try_from(info.audio_tracks.len()).unwrap_or(0),
                    playable,
                };
                if existing_id.is_some() {
                    upd_batch.push(record);
                } else {
                    new_batch.push(record);
                }
                if new_batch.len() + upd_batch.len() >= BATCH {
                    flush(lib, &mut new_batch, &mut upd_batch, &mut report)?;
                }
            }
            Ok(())
        })();
    });
    consumer?;
    flush(lib, &mut new_batch, &mut upd_batch, &mut report)?;

    if !cancel.load(Ordering::Relaxed) {
        report.marked_missing = lib.mark_missing(root_id, &plan.missing)?;
        lib.touch_scan(root_id, now_ms())?;
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    report.elapsed = started.elapsed();
    Ok(report)
}

/// Runs walk, plan and apply for one root.
///
/// If the root directory is not reachable, the root is set offline, nothing is marked missing,
/// and the report has `offline == true`. Otherwise the root is set online and scanned. Rows
/// under a directory that could not be read are excluded from the diff, so they are not marked
/// missing.
pub fn scan_root(
    lib: &Library,
    root_id: i64,
    root: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanReport, ScanError> {
    let started = Instant::now();
    let offline = |elapsed| ScanReport {
        root_id,
        offline: true,
        elapsed,
        ..ScanReport::default()
    };
    if !root.is_dir() {
        lib.set_root_availability(root_id, false)?;
        return Ok(offline(started.elapsed()));
    }
    let walk = match walk_root(root, opts, cancel, &mut |found| {
        progress(ScanProgress::Walking { found })
    }) {
        Ok(w) => w,
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            return Ok(ScanReport {
                root_id,
                cancelled: true,
                elapsed: started.elapsed(),
                ..ScanReport::default()
            });
        }
        Err(_) => {
            lib.set_root_availability(root_id, false)?;
            return Ok(offline(started.elapsed()));
        }
    };
    lib.set_root_availability(root_id, true)?;
    let existing: Vec<_> = lib
        .list_paths_for_root(root_id)?
        .into_iter()
        .filter(|row| !walk.unreadable.iter().any(|u| path_under(&row.1, u)))
        .collect();
    let plan = plan_scan_checked(&walk.files, &existing, &|rel| root.join(rel).exists());
    apply_scan(
        lib,
        RootTarget { root_id, root },
        &plan,
        opts,
        progress,
        cancel,
    )
}

/// Creates posters for externally indexed recordings that lack one, at most `limit` attempts
/// per call. Posters go to `thumbs_dir/<id>/poster.jpg`, as the recorder writes them. Returns the
/// number of posters written. A failed attempt is not recorded, so it is retried on the next call.
pub fn generate_missing_thumbnails(
    lib: &Library,
    thumbs_dir: &Path,
    limit: usize,
) -> Result<usize, ScanError> {
    let mut made = 0;
    let mut attempts = 0;
    for f in lib.external_ready_files()? {
        if attempts >= limit {
            break;
        }
        let out = thumbs_dir.join(&f.id).join("poster.jpg");
        if out.exists() {
            continue;
        }
        attempts += 1;
        let at = (f.duration_ms as f64 / 1000.0 * 0.25).min(3.0);
        let file = Path::new(&f.root_path).join(&f.rel_path);
        if make_thumbnail(&file, &out, at, 320).is_ok() {
            made += 1;
        }
    }
    Ok(made)
}

/// Derives the game name and kind of a file from its path under a root.
///
/// The game is the first folder below the root. A leading `Clips` or `Sessions` folder is
/// skipped, so a root of `…\Medal` and a root of `…\Medal\Clips` behave the same. A file with
/// no folder below the root is `Unsorted`. The kind is `session` when any folder on the path is
/// named `Sessions`, otherwise `clip`.
pub fn derive_game_and_kind(root_name: &str, rel_path: &str) -> (String, RecordingKind) {
    let comps: Vec<&str> = rel_path
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .collect();
    let dirs = &comps[..comps.len().saturating_sub(1)];
    let is_session = root_name.eq_ignore_ascii_case("sessions")
        || dirs.iter().any(|d| d.eq_ignore_ascii_case("sessions"));
    let dirs = match dirs.first() {
        Some(first) if is_container_name(first) => &dirs[1..],
        _ => dirs,
    };
    let game = dirs
        .first()
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "Unsorted".to_owned());
    let kind = if is_session {
        RecordingKind::Session
    } else {
        RecordingKind::Clip
    };
    (game, kind)
}

/// Writes one batch: inserts `new` and refreshes `updated`, then clears both.
fn flush(
    lib: &Library,
    new: &mut Vec<ExternalRecording>,
    updated: &mut Vec<ExternalRecording>,
    report: &mut ScanReport,
) -> Result<(), ScanError> {
    if new.is_empty() && updated.is_empty() {
        return Ok(());
    }
    lib.write_external_batch(new, updated)?;
    report.added += new.len();
    report.updated += updated.len();
    new.clear();
    updated.clear();
    Ok(())
}

/// Returns the id of the game for a folder name, creating the game if needed. Games created here
/// use the id `folder:<name>`. The cache avoids repeated lookups within one scan.
fn resolve_game(
    lib: &Library,
    cache: &mut HashMap<String, String>,
    name: &str,
    now: i64,
) -> Result<String, LibError> {
    if let Some(id) = cache.get(name) {
        return Ok(id.clone());
    }
    let id = match lib.game_id_for_folder(name)? {
        Some(id) => id,
        None => {
            let id = format!("folder:{name}");
            let display: String = name.chars().take(200).collect();
            let folder: String = name.chars().take(80).collect();
            lib.upsert_game(
                &NewGame {
                    id: id.clone(),
                    display_name: display,
                    folder_name: folder,
                    exe_name: None,
                    store_id: None,
                },
                now,
            )?;
            id
        }
    };
    cache.insert(name.to_owned(), id.clone());
    Ok(id)
}

/// Maps a file to the library's container column and playability. `None` if unsupported.
fn container_for(rel_path: &str) -> Option<(&'static str, bool)> {
    match ext_of(rel_path).as_str() {
        "mp4" | "m4v" | "mov" => Some(("mp4", true)),
        "mkv" => Some(("mkv", false)),
        _ => None,
    }
}

fn ext_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn file_name_of(rel_path: &str) -> String {
    rel_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(rel_path)
        .to_owned()
}

fn is_container_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("clips") || name.eq_ignore_ascii_case("sessions")
}

fn is_ignored_dir(name: &str, opts: &ScanOptions) -> bool {
    let lower = name.to_ascii_lowercase();
    opts.ignore_dirs
        .iter()
        .any(|d| d.eq_ignore_ascii_case(&lower))
}

fn accepted_file(name: &str, opts: &ScanOptions) -> bool {
    let lower = name.to_ascii_lowercase();
    if opts
        .ignore_file_prefixes
        .iter()
        .any(|p| lower.starts_with(&p.to_ascii_lowercase()))
    {
        return false;
    }
    let ext = ext_of(&lower);
    opts.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext))
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}{MAIN_SEPARATOR}{name}")
    }
}

fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Comparison key for relative paths: lower-cased on Windows, exact elsewhere.
fn path_key(rel: &str) -> String {
    if cfg!(windows) {
        rel.to_lowercase()
    } else {
        rel.to_owned()
    }
}

/// True if `rel` is `prefix` or lies below it.
fn path_under(rel: &str, prefix: &str) -> bool {
    let (r, p) = (path_key(rel), path_key(prefix));
    r == p || r.starts_with(&format!("{p}{MAIN_SEPARATOR}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_from_folder_under_clips_root() {
        let (g, k) = derive_game_and_kind("Clips", "Apex Legends\\MedalTVApex.mp4");
        assert_eq!((g.as_str(), k), ("Apex Legends", RecordingKind::Clip));
    }

    #[test]
    fn game_from_medal_root_skips_clips_folder() {
        let (g, _) = derive_game_and_kind("Medal", "Clips\\Apex\\a.mp4");
        assert_eq!(g, "Apex");
    }

    #[test]
    fn file_at_root_is_unsorted() {
        let (g, _) = derive_game_and_kind("Clips", "a.mp4");
        assert_eq!(g, "Unsorted");
    }

    #[test]
    fn sessions_folder_means_session_kind() {
        let (g, k) = derive_game_and_kind("Medal", "Sessions\\Apex\\Part1.mp4");
        assert_eq!((g.as_str(), k), ("Apex", RecordingKind::Session));
    }

    #[test]
    fn path_under_matches_whole_components() {
        assert!(path_under("Apex\\a.mp4", "Apex"));
        assert!(!path_under("Apexy\\a.mp4", "Apex"));
    }

    fn prow(id: &str, rel: &str, status: RecStatus) -> PathRow {
        (id.to_owned(), rel.to_owned(), 100, 5, status)
    }

    fn ffound(rel: &str) -> FoundFile {
        FoundFile {
            rel_path: rel.to_owned(),
            size: 100,
            mtime_ms: 5,
        }
    }

    #[test]
    fn checked_row_absent_from_walk_but_on_disk_is_not_missing() {
        let rows = [prow("1", "Apex\\a.mp4", RecStatus::Ready)];
        let plan = plan_scan_checked(&[], &rows, &|rel| rel == "Apex\\a.mp4");
        assert!(plan.missing.is_empty());
        assert!(plan.changed.is_empty());
    }

    #[test]
    fn checked_row_absent_and_gone_is_missing() {
        let rows = [prow("1", "Apex\\a.mp4", RecStatus::Ready)];
        let plan = plan_scan_checked(&[], &rows, &|_| false);
        assert_eq!(plan.missing, vec!["1".to_owned()]);
    }

    #[test]
    fn checked_row_present_in_walk_is_untouched() {
        let rows = [prow("1", "Apex\\a.mp4", RecStatus::Ready)];
        let found = [ffound("Apex\\a.mp4")];
        let plan = plan_scan_checked(&found, &rows, &|_| false);
        assert!(plan.missing.is_empty());
        assert!(plan.changed.is_empty());
        assert_eq!(plan.unchanged, 1);
    }

    #[test]
    fn checked_mixed_set_marks_only_the_truly_gone_rows() {
        let rows = [
            prow("kept", "Apex\\kept.mp4", RecStatus::Ready),
            prow("raced", "Apex\\raced.mp4", RecStatus::Ready),
            prow("gone", "Apex\\gone.mp4", RecStatus::Pending),
            prow("already", "Apex\\old.mp4", RecStatus::Missing),
            prow("rec", "Apex\\rec.mp4", RecStatus::Recording),
        ];
        let found = [ffound("Apex\\kept.mp4")];
        let plan = plan_scan_checked(&found, &rows, &|rel| rel == "Apex\\raced.mp4");
        assert_eq!(plan.missing, vec!["gone".to_owned()]);
        assert_eq!(plan.unchanged, 1);
        assert!(plan.new.is_empty());
    }

    #[test]
    fn plan_scan_without_check_still_marks_absent_rows_missing() {
        let rows = [prow("1", "Apex\\a.mp4", RecStatus::Ready)];
        let plan = plan_scan(&[], &rows);
        assert_eq!(plan.missing, vec!["1".to_owned()]);
    }
}
