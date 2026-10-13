//! Editor cache: sizes, clearing and the `cache_max_gb` limit. Owner: worker `ui-cache`.
//!
//! The cache root is `paths::cache_dir` (the settings override or `<editor_root>\cache`). Only the
//! three subfolders in [`CACHE_SUBDIRS`] belong to the editor: `thumbs`, `peaks` (including
//! `peaks\v2`) and `audio`. Nothing else under the root is ever listed, deleted or measured.
//! Symlinks and Windows reparse points (junctions) are never followed and never deleted.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use super::export::{ExportManager, JobState};
use super::settings::{self, EditorSettings};
use super::{paths, EditorState};

/// Editor-owned subfolders of the cache root. Nothing else is touched.
const CACHE_SUBDIRS: [&str; 3] = ["thumbs", "peaks", "audio"];

/// Share of the limit that an enforcement pass cleans down to, in percent.
const TARGET_PERCENT: u64 = 90;

/// Minimum time between two automatic limit checks.
const LIMIT_CHECK_INTERVAL: Duration = Duration::from_secs(600);

/// Bytes in one GB (GiB, as Explorer reports sizes).
const GIB: u64 = 1024 * 1024 * 1024;

/// Windows `FILE_ATTRIBUTE_REPARSE_POINT`: symlinks and junctions.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Last time an automatic limit check was started (process wide).
static LAST_LIMIT_CHECK: Mutex<Option<Instant>> = Mutex::new(None);

/// Sizes of the editor cache, as the Settings page shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    /// Cache root folder.
    pub dir: String,
    /// Bytes under `thumbs`.
    pub thumbs_bytes: u64,
    /// Bytes under `peaks` (including `peaks\v2`).
    pub peaks_bytes: u64,
    /// Bytes under `audio`.
    pub audio_bytes: u64,
    /// Sum of the three folders.
    pub total_bytes: u64,
}

/// One cache file for the size limit: path, size in bytes and modification time in ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheFile {
    /// Full path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Last modification, ms since the Unix epoch.
    pub mtime_ms: i64,
}

/// Chooses the files to delete so the total drops under the target.
///
/// Returns nothing when the total is at or below `limit_bytes`. Otherwise the least recently
/// modified files come first (ties broken by path) until the remaining total is at or below 90 %
/// of the limit. An empty list gives an empty plan.
pub fn eviction_plan(files: &[CacheFile], limit_bytes: u64) -> Vec<PathBuf> {
    let total = files.iter().fold(0u64, |acc, f| acc.saturating_add(f.size));
    if total <= limit_bytes {
        return Vec::new();
    }
    let target = u64::try_from(u128::from(limit_bytes) * u128::from(TARGET_PERCENT) / 100)
        .unwrap_or(u64::MAX);

    let mut order: Vec<&CacheFile> = files.iter().collect();
    order.sort_by(|a, b| {
        a.mtime_ms
            .cmp(&b.mtime_ms)
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut remaining = total;
    let mut plan = Vec::new();
    for file in order {
        if remaining <= target {
            break;
        }
        remaining = remaining.saturating_sub(file.size);
        plan.push(file.path.clone());
    }
    plan
}

/// Limit in bytes for a `cache_max_gb` value.
pub fn limit_bytes(max_gb: u32) -> u64 {
    u64::from(max_gb) * GIB
}

/// Cache root for `settings` (the override when set, else `<editor_root>\cache`).
fn cache_root(settings: &EditorSettings) -> PathBuf {
    let root = paths::editor_root(std::env::var("LOCALAPPDATA").ok().as_deref());
    paths::cache_dir(&root, &settings.cache_dir)
}

/// True for symlinks and reparse points (junctions). Never followed, never deleted.
fn is_link(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || reparse_point(meta)
}

#[cfg(windows)]
fn reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

/// Modification time in ms since the Unix epoch (0 when unknown).
fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// A file found under a cache subfolder. `partial` marks in-progress files (`.part`).
struct Found {
    file: CacheFile,
    partial: bool,
}

/// Regular files under `dir`, recursively. Links are skipped; errors are ignored.
fn scan(dir: &Path, out: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if is_link(&meta) {
            continue;
        }
        if meta.is_dir() {
            scan(&path, out);
        } else if meta.is_file() {
            let partial = path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".part"));
            out.push(Found {
                file: CacheFile {
                    size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                    path,
                },
                partial,
            });
        }
    }
}

/// Files under one editor subfolder of the cache root. Empty when the folder is missing, or is a
/// link (never followed).
fn subdir_files(dir: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if meta.is_dir() && !is_link(&meta) {
            scan(dir, &mut out);
        }
    }
    out
}

/// Sum of the sizes of the files under one editor subfolder.
fn subdir_bytes(root: &Path, name: &str) -> u64 {
    subdir_files(&root.join(name))
        .iter()
        .fold(0u64, |acc, f| acc.saturating_add(f.file.size))
}

/// Sizes of the editor cache under `root`. Missing folders count as 0.
pub fn info_at(root: &Path) -> CacheInfo {
    let thumbs_bytes = subdir_bytes(root, "thumbs");
    let peaks_bytes = subdir_bytes(root, "peaks");
    let audio_bytes = subdir_bytes(root, "audio");
    CacheInfo {
        dir: root.to_string_lossy().into_owned(),
        thumbs_bytes,
        peaks_bytes,
        audio_bytes,
        total_bytes: thumbs_bytes
            .saturating_add(peaks_bytes)
            .saturating_add(audio_bytes),
    }
}

/// Deletes everything inside `dir` (subfolders included, `dir` itself kept). Links are skipped.
/// Best effort: a file that cannot be deleted (for example one in use) is skipped.
fn clear_contents(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if is_link(&meta) {
            continue;
        }
        if meta.is_dir() {
            clear_contents(&path);
            let _ = fs::remove_dir(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Clears the three editor subfolders under `root`. Other folders and `root` itself are untouched.
pub fn clear_at(root: &Path) {
    for name in CACHE_SUBDIRS {
        let dir = root.join(name);
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if meta.is_dir() && !is_link(&meta) {
            clear_contents(&dir);
        }
    }
}

/// Enforces `limit_bytes` on the editor subfolders under `root`. In-progress (`.part`) files are
/// never candidates. Returns how many files were deleted.
pub fn enforce_limit(root: &Path, limit_bytes: u64) -> usize {
    let files: Vec<CacheFile> = CACHE_SUBDIRS
        .iter()
        .flat_map(|name| subdir_files(&root.join(name)))
        .filter(|f| !f.partial)
        .map(|f| f.file)
        .collect();
    eviction_plan(&files, limit_bytes)
        .iter()
        .filter(|path| fs::remove_file(path).is_ok())
        .count()
}

/// True while an export is queued, running or paused. Cache deletion waits for that.
pub fn export_busy(exports: &ExportManager) -> bool {
    let jobs = exports.jobs.lock().unwrap_or_else(|e| e.into_inner());
    jobs.iter().any(|j| {
        matches!(
            j.state,
            JobState::Queued | JobState::Running | JobState::Paused
        )
    })
}

/// Starts a best-effort limit check on a background thread, at most once per 10 minutes. Skipped
/// while an export is active. Never blocks the caller.
pub fn schedule_limit_check(exports: &ExportManager) {
    if export_busy(exports) {
        return;
    }
    {
        let mut last = LAST_LIMIT_CHECK.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < LIMIT_CHECK_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
    }
    let spawned = std::thread::Builder::new()
        .name("clipcheck-cache-limit".into())
        .spawn(|| {
            let settings = settings::load(&settings::settings_path());
            let root = cache_root(&settings);
            enforce_limit(&root, limit_bytes(settings.cache_max_gb));
        });
    if let Err(e) = spawned {
        eprintln!("editor cache: cannot start limit check: {e}");
    }
}

/// Tauri: sizes of the editor cache.
#[tauri::command]
pub async fn editor_cache_info() -> Result<CacheInfo, String> {
    let root = cache_root(&settings::load(&settings::settings_path()));
    tauri::async_runtime::spawn_blocking(move || info_at(&root))
        .await
        .map_err(|e| e.to_string())
}

/// Tauri: deletes the editor cache (thumbs, peaks, audio only) and returns the fresh sizes.
/// Refused while an export is queued or running.
#[tauri::command]
pub async fn editor_clear_cache(state: State<'_, EditorState>) -> Result<CacheInfo, String> {
    if export_busy(&state.exports) {
        return Err(
            "An export is queued or running. Clear the cache when the exports have finished."
                .to_string(),
        );
    }
    let root = cache_root(&settings::load(&settings::settings_path()));
    tauri::async_runtime::spawn_blocking(move || {
        clear_at(&root);
        info_at(&root)
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::SystemTime;

    fn cf(path: &str, size: u64, mtime_ms: i64) -> CacheFile {
        CacheFile {
            path: PathBuf::from(path),
            size,
            mtime_ms,
        }
    }

    /// Writes `size` bytes to `path` (creating parents) and sets its modification time.
    fn make_file(path: &Path, size: usize, mtime: Option<SystemTime>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&vec![7u8; size]).unwrap();
        if let Some(t) = mtime {
            f.set_modified(t).unwrap();
        }
    }

    fn at(secs: u64) -> Option<SystemTime> {
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn eviction_removes_oldest_first_and_stops_under_target() {
        // Limit 100, total 150, target 90: deleting the oldest 60 leaves 90 (<= target), stop.
        let files = vec![
            cf("new.pk", 50, 300),
            cf("old.pk", 60, 100),
            cf("mid.pk", 40, 200),
        ];
        let plan = eviction_plan(&files, 100);
        assert_eq!(plan, vec![PathBuf::from("old.pk")]);
    }

    #[test]
    fn eviction_continues_until_target_is_reached() {
        // Total 300, limit 100, target 90: delete 100, 100 (remaining 100), then 100 (0 left).
        let files = vec![cf("a", 100, 1), cf("b", 100, 2), cf("c", 100, 3)];
        let plan = eviction_plan(&files, 100);
        assert_eq!(
            plan,
            vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]
        );
    }

    #[test]
    fn eviction_on_empty_list_is_empty() {
        assert!(eviction_plan(&[], 100).is_empty());
        assert!(eviction_plan(&[], 0).is_empty());
    }

    #[test]
    fn eviction_with_limit_larger_than_total_is_empty() {
        let files = vec![cf("a", 10, 1), cf("b", 20, 2)];
        assert!(eviction_plan(&files, 1000).is_empty());
        // Exactly at the limit is not over it.
        assert!(eviction_plan(&files, 30).is_empty());
    }

    #[test]
    fn eviction_ties_break_by_path() {
        let files = vec![cf("b", 50, 5), cf("a", 50, 5), cf("c", 50, 5)];
        // Total 150, limit 100, target 90: deleting "a" leaves 100, then "b" leaves 50.
        let plan = eviction_plan(&files, 100);
        assert_eq!(plan, vec![PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[test]
    fn limit_bytes_converts_gb() {
        assert_eq!(limit_bytes(1), GIB);
        assert_eq!(limit_bytes(5), 5 * GIB);
    }

    #[test]
    fn cache_info_serialises_camel_case_keys() {
        let info = CacheInfo {
            dir: "D:\\cache".to_string(),
            thumbs_bytes: 1,
            peaks_bytes: 2,
            audio_bytes: 3,
            total_bytes: 6,
        };
        let v = serde_json::to_value(&info).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "audioBytes",
                "dir",
                "peaksBytes",
                "thumbsBytes",
                "totalBytes"
            ]
        );
        assert_eq!(v["thumbsBytes"], 1);
        assert_eq!(v["totalBytes"], 6);
    }

    #[test]
    fn info_sums_the_three_folders_and_ignores_others() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_file(&root.join("thumbs").join("t.jpg"), 10, None);
        make_file(&root.join("peaks").join("v2").join("p.pk"), 20, None);
        make_file(&root.join("audio").join("a-v2.m4a"), 30, None);
        make_file(&root.join("other").join("keep.bin"), 100, None);
        make_file(&root.join("root.txt"), 1000, None);

        let info = info_at(root);
        assert_eq!(info.thumbs_bytes, 10);
        assert_eq!(info.peaks_bytes, 20);
        assert_eq!(info.audio_bytes, 30);
        assert_eq!(info.total_bytes, 60);
        assert_eq!(info.dir, root.to_string_lossy());
    }

    #[test]
    fn info_of_missing_folders_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let info = info_at(&tmp.path().join("does-not-exist"));
        assert_eq!(info.total_bytes, 0);
        assert_eq!(info.thumbs_bytes, 0);
    }

    #[test]
    fn clear_removes_only_the_three_folders_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_file(&root.join("thumbs").join("t.jpg"), 10, None);
        make_file(&root.join("peaks").join("v2").join("p.pk"), 20, None);
        make_file(&root.join("audio").join("a-v2.m4a"), 30, None);
        make_file(&root.join("audio").join("a-v2.part.m4a"), 5, None);
        make_file(&root.join("other").join("keep.bin"), 100, None);
        make_file(&root.join("root.txt"), 1000, None);

        clear_at(root);

        let info = info_at(root);
        assert_eq!(info.total_bytes, 0);
        // Sibling folder and root file survive.
        assert!(root.join("other").join("keep.bin").is_file());
        assert!(root.join("root.txt").is_file());
        // The three folders themselves are kept, emptied.
        assert!(root.join("thumbs").is_dir());
        assert!(root.join("peaks").is_dir());
        assert!(root.join("audio").is_dir());
        assert!(!root.join("peaks").join("v2").exists());
    }

    #[test]
    fn clear_on_missing_root_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");
        clear_at(&missing);
        assert!(!missing.exists());
    }

    #[test]
    fn clear_does_not_follow_a_symlinked_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let outside = tmp.path().join("outside");
        make_file(&outside.join("precious.bin"), 50, None);
        fs::create_dir_all(&root).unwrap();
        if std::os::windows::fs::symlink_dir(&outside, root.join("thumbs")).is_err() {
            // Creating symlinks needs developer mode or privileges on Windows; skip then.
            return;
        }
        clear_at(&root);
        assert!(outside.join("precious.bin").is_file());
    }

    #[test]
    fn enforce_deletes_least_recent_until_under_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Total 120 against a limit of 100 (target 90): the oldest 60-byte file goes.
        make_file(&root.join("thumbs").join("old.jpg"), 60, at(1_000));
        make_file(&root.join("audio").join("new-v2.m4a"), 60, at(2_000));
        // An in-progress file is neither counted as a candidate nor deleted.
        make_file(&root.join("audio").join("busy-v2.part.m4a"), 500, at(500));

        let removed = enforce_limit(root, 100);

        assert_eq!(removed, 1);
        assert!(!root.join("thumbs").join("old.jpg").exists());
        assert!(root.join("audio").join("new-v2.m4a").is_file());
        assert!(root.join("audio").join("busy-v2.part.m4a").is_file());
    }

    #[test]
    fn enforce_does_nothing_under_limit() {
        let tmp = tempfile::tempdir().unwrap();
        make_file(&tmp.path().join("thumbs").join("a.jpg"), 60, at(1));
        assert_eq!(enforce_limit(tmp.path(), 100), 0);
        assert!(tmp.path().join("thumbs").join("a.jpg").is_file());
    }
}
