//! Import of existing video files into the recordings layout (task L1c, part 2).
//!
//! Work is split in two. [`plan_import`] walks the sources, works out each
//! file's game folder and destination, and checks for duplicates and free
//! space. It touches nothing. [`run_import`] copies the planned files, checks
//! that each copy is a real video, and registers it through a caller closure.
//!
//! Source files are only ever read. Files go to `<dest_root>\Clips\<Game>\`.
//! Copies are written to `<name>.part` and renamed into place only when complete.
//! Windows API calls are limited to the free-space query.

use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::catalog::MediaInfo;
use crate::recorder::naming;

/// Folder under the storage root that holds clips. Imports go to `<root>\Clips\<Game>\`.
pub const CLIPS_FOLDER: &str = "Clips";
/// Extensions that can be imported, lower case and without the dot. Same set as the scanner
/// (`scan.rs`): webm is not stored by the library and is skipped as unsupported.
pub const SUPPORTED_EXTENSIONS: [&str; 4] = ["mp4", "m4v", "mov", "mkv"];
/// Free space that must remain on the destination volume after the import.
pub const FREE_SPACE_MARGIN: u64 = 1024 * 1024 * 1024;
/// Registration callback for [`run_import`]: `(dest, game, info, mtime_ms)` to
/// a library id, or an error text.
pub type RegisterFn<'a> = dyn FnMut(&Path, &str, &MediaInfo, i64) -> Result<String, String> + 'a;
/// Game folder used when no game name can be derived.
pub const IMPORTED_GAME: &str = "Imported";
/// Error text for a file that does not probe as a video.
pub const NOT_A_VIDEO: &str = "Not a valid video";

/// Bytes read and written per step while copying.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Shortest time between two progress callbacks, except the final one.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Deepest folder level walked below each source.
const MAX_DEPTH: usize = 32;
/// Upper bound on numbered alternatives tried for a destination name.
const MAX_SUFFIX: u32 = 10_000;
/// Parent folder names that say nothing about the game.
const GENERIC_FOLDERS: [&str; 7] = [
    "Downloads",
    "Videos",
    "Desktop",
    "Clips",
    "Imports",
    "Temp",
    "Documents",
];
/// Path length above which Windows calls get the `\\?\` prefix.
#[cfg(windows)]
const LONG_PATH_UNITS: usize = 240;

/// What the user asked to import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    /// Files or folders. Folders are walked recursively.
    pub sources: Vec<PathBuf>,
    /// Game folder for every file. `None` derives it from each file's parent folder.
    pub game: Option<String>,
    /// Primary storage root. Files go under `dest_root\Clips\<Game>\`.
    pub dest_root: PathBuf,
    /// Only `true` is implemented. Sources are copied, never moved, and are never modified.
    pub copy: bool,
}

/// Why a file is not copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The extension is not one of [`SUPPORTED_EXTENSIONS`]. Holds the extension.
    Unsupported(String),
    /// The same name, size and modified time already exist in the destination
    /// folder, or the library reports them.
    AlreadyInLibrary,
    /// An identical file (name, size and modified time) appears earlier in this import.
    DuplicateInImport,
    /// The source could not be read. Holds the reason.
    Unreadable(String),
}

/// One file in a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportItem {
    /// Source file. It is only read.
    pub src: PathBuf,
    /// Source size in bytes.
    pub size: u64,
    /// Source modified time in Unix milliseconds, or 0 when unknown.
    pub mtime_ms: i64,
    /// Game folder name used for the destination.
    pub game: String,
    /// Destination path. For a skipped item this is where it would have gone.
    pub dest: PathBuf,
    /// Why the item is skipped. `None` means it will be copied.
    pub skip_reason: Option<SkipReason>,
}

/// Result of [`plan_import`]. Nothing has been copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    /// Every candidate file in walk order. Junk and zero-byte files are not listed.
    pub items: Vec<ImportItem>,
    /// Sum of the sizes of the items that will be copied.
    pub total_bytes: u64,
    /// Free bytes on the destination volume. 0 when the query failed.
    pub free_bytes: u64,
    /// True when `free_bytes >= total_bytes + FREE_SPACE_MARGIN`.
    pub enough_space: bool,
}

/// Progress from [`run_import`]. `bytes_done` never decreases between calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProgress {
    /// One-based index of the file being copied.
    pub file_index: usize,
    /// Number of files that will be copied.
    pub file_count: usize,
    /// Bytes finished or in progress across all files.
    pub bytes_done: u64,
    /// Total bytes to copy, from the plan.
    pub bytes_total: u64,
    /// Destination file name of the current file.
    pub current_name: String,
}

/// A file that was copied, checked and registered.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedFile {
    /// Source path.
    pub src: PathBuf,
    /// Destination path.
    pub dest: PathBuf,
    /// Game folder name.
    pub game: String,
    /// Probe result for the copy.
    pub info: MediaInfo,
    /// Modified time copied to the destination, Unix milliseconds.
    pub mtime_ms: i64,
    /// Id returned by the `register` closure.
    pub library_id: String,
}

/// Result of [`run_import`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    /// Files copied and registered.
    pub imported: Vec<ImportedFile>,
    /// Files from the plan that were skipped.
    pub skipped: Vec<(PathBuf, SkipReason)>,
    /// Files that failed, with the reason. A failed file leaves no copy behind.
    pub failed: Vec<(PathBuf, String)>,
    /// True when the run stopped early because of `cancel`.
    pub cancelled: bool,
}

/// Plans an import. Walks the sources, derives game folders, skips duplicates
/// and unsupported files, and checks free space. Writes nothing.
///
/// `free_bytes` reports free bytes for a path. Use [`disk_free_bytes`] in the
/// daemon. `exists_in_library(name, size, mtime_ms)` reports whether the library
/// already has the file.
pub fn plan_import<F, E>(req: &ImportRequest, free_bytes: F, exists_in_library: E) -> ImportPlan
where
    F: Fn(&Path) -> io::Result<u64>,
    E: Fn(&str, u64, i64) -> bool,
{
    let forced_game = req
        .game
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(naming::sanitize);

    let mut candidates = Vec::new();
    for src in &req.sources {
        walk(src, 0, &mut candidates);
    }

    let mut items = Vec::with_capacity(candidates.len());
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut seen: HashSet<(String, u64, i64)> = HashSet::new();
    for cand in candidates {
        let name = file_name_of(&cand.path);
        let game = forced_game
            .clone()
            .unwrap_or_else(|| derive_game(&cand.path));
        let dest_dir = req.dest_root.join(CLIPS_FOLDER).join(&game);
        let key = (name.clone(), cand.size, cand.mtime_ms);

        let skip = if let Some(msg) = cand.broken {
            Some(SkipReason::Unreadable(msg))
        } else if !is_supported(&cand.path) {
            Some(SkipReason::Unsupported(ext_of(&cand.path)))
        } else if seen.contains(&key) {
            Some(SkipReason::DuplicateInImport)
        } else if same_file_at(&dest_dir.join(&name), cand.size, cand.mtime_ms)
            || exists_in_library(&name, cand.size, cand.mtime_ms)
        {
            Some(SkipReason::AlreadyInLibrary)
        } else {
            None
        };

        let dest = if skip.is_none() {
            seen.insert(key);
            let dest = pick_dest(&dest_dir, &name, &taken);
            taken.insert(dest.clone());
            dest
        } else {
            dest_dir.join(&name)
        };
        items.push(ImportItem {
            src: cand.path,
            size: cand.size,
            mtime_ms: cand.mtime_ms,
            game,
            dest,
            skip_reason: skip,
        });
    }

    let total_bytes: u64 = items
        .iter()
        .filter(|i| i.skip_reason.is_none())
        .map(|i| i.size)
        .sum();
    let (free, enough_space) = match free_bytes(&req.dest_root) {
        Ok(free) => (free, free >= total_bytes.saturating_add(FREE_SPACE_MARGIN)),
        Err(_) => (0, false),
    };
    ImportPlan {
        items,
        total_bytes,
        free_bytes: free,
        enough_space,
    }
}

/// Copies the planned files, checks each copy, and registers it.
///
/// For each file: copy to `<dest>.part` in 4 MiB chunks, set the copy's modified
/// time to the source's, check the size, rename into place, then probe. The
/// probe must show a video stream and a duration above zero. Otherwise the copy
/// is deleted and the file fails with [`NOT_A_VIDEO`]. A valid copy goes to
/// `register(dest, game, info, mtime_ms)`. If `register` fails, the copy is
/// deleted too, so a failed file leaves nothing behind.
///
/// `progress` is called on each step, at most about 10 times a second, with one
/// final call at the end. Setting `cancel` stops the run. The partial copy is
/// removed, and completed files are kept. Sources are never modified.
pub fn run_import(
    plan: &ImportPlan,
    probe: &dyn Fn(&Path) -> io::Result<MediaInfo>,
    register: &mut RegisterFn<'_>,
    progress: &mut dyn FnMut(&ImportProgress),
    cancel: &AtomicBool,
) -> ImportReport {
    let mut report = ImportReport::default();
    let file_count = plan
        .items
        .iter()
        .filter(|i| i.skip_reason.is_none())
        .count();
    let mut rep = Reporter {
        cb: progress,
        last: None,
        file_count,
        bytes_total: plan.total_bytes,
    };
    let mut base = 0u64;
    let mut index = 0usize;
    let mut last_name = String::new();

    for item in &plan.items {
        if let Some(reason) = &item.skip_reason {
            report.skipped.push((item.src.clone(), reason.clone()));
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        index += 1;
        last_name = file_name_of(&item.dest);
        match import_one(item, index, base, probe, register, &mut rep, cancel) {
            ItemResult::Imported(file) => report.imported.push(file),
            ItemResult::Failed(error) => report.failed.push((item.src.clone(), error)),
            ItemResult::Cancelled => {
                report.cancelled = true;
                break;
            }
        }
        // Count the whole file even when it failed, so bytes_done never drops.
        base += item.size;
    }

    if file_count > 0 {
        rep.emit(index.max(1), base, &last_name);
    }
    report
}

/// Free bytes available to the caller on the volume holding `path`. The query
/// uses the nearest existing ancestor of `path`. Needs Windows.
pub fn disk_free_bytes(path: &Path) -> io::Result<u64> {
    let mut at = path;
    while !at.exists() {
        match at.parent() {
            Some(parent) => at = parent,
            None => break,
        }
    }
    free_bytes_available(at)
}

/// Outcome of one file in [`run_import`].
enum ItemResult {
    Imported(ImportedFile),
    Failed(String),
    Cancelled,
}

/// Copies, checks and registers one planned file.
fn import_one(
    item: &ImportItem,
    index: usize,
    base: u64,
    probe: &dyn Fn(&Path) -> io::Result<MediaInfo>,
    register: &mut RegisterFn<'_>,
    rep: &mut Reporter<'_>,
    cancel: &AtomicBool,
) -> ItemResult {
    let name = file_name_of(&item.dest);
    rep.send(index, base, &name);

    if let Some(dir) = item.dest.parent() {
        if let Err(e) = fs::create_dir_all(long(dir)) {
            return ItemResult::Failed(format!("cannot create folder: {e}"));
        }
    }
    if long(&item.dest).exists() {
        return ItemResult::Failed("Destination already exists".to_string());
    }

    let part = part_path(&item.dest);
    let mut copied = 0u64;
    let finished = copy_chunks(&item.src, &part, cancel, &mut |n| {
        copied += n;
        rep.send(index, base + copied, &name);
    });
    match finished {
        Ok(true) => {}
        Ok(false) => {
            let _ = fs::remove_file(long(&part));
            return ItemResult::Cancelled;
        }
        Err(e) => {
            let _ = fs::remove_file(long(&part));
            return ItemResult::Failed(format!("copy failed: {e}"));
        }
    }

    let fail = |msg: String| {
        let _ = fs::remove_file(long(&part));
        ItemResult::Failed(msg)
    };
    match fs::metadata(long(&part)) {
        Ok(m) if m.len() == item.size => {}
        Ok(m) => {
            return fail(format!(
                "size mismatch: wrote {} of {} bytes",
                m.len(),
                item.size
            ))
        }
        Err(e) => return fail(format!("copy check failed: {e}")),
    }
    if let Err(e) = set_mtime(&part, item.mtime_ms) {
        return fail(format!("could not set modified time: {e}"));
    }
    if long(&item.dest).exists() {
        return fail("Destination already exists".to_string());
    }
    if let Err(e) = fs::rename(long(&part), long(&item.dest)) {
        return fail(format!("rename failed: {e}"));
    }

    let info = match probe(&item.dest) {
        Ok(info) if info.width > 0 && info.height > 0 && info.duration_ms > 0 => info,
        _ => {
            let _ = fs::remove_file(long(&item.dest));
            return ItemResult::Failed(NOT_A_VIDEO.to_string());
        }
    };

    match register(&item.dest, &item.game, &info, item.mtime_ms) {
        Ok(library_id) => ItemResult::Imported(ImportedFile {
            src: item.src.clone(),
            dest: item.dest.clone(),
            game: item.game.clone(),
            info,
            mtime_ms: item.mtime_ms,
            library_id,
        }),
        Err(e) => {
            let _ = fs::remove_file(long(&item.dest));
            ItemResult::Failed(format!("copied but not added to the library: {e}"))
        }
    }
}

/// Copies `src` to `part` in chunks. Returns `Ok(false)` when `cancel` is set
/// before the copy finishes. The caller removes the partial file. `on_bytes`
/// gets each chunk's size.
fn copy_chunks(
    src: &Path,
    part: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> io::Result<bool> {
    let mut input = File::open(long(src))?;
    let mut output = File::create(long(part))?;
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        on_bytes(n as u64);
    }
    output.sync_all()?;
    Ok(true)
}

/// Sets the modified time of `path` to `mtime_ms` (Unix milliseconds).
fn set_mtime(path: &Path, mtime_ms: i64) -> io::Result<()> {
    let when = if mtime_ms > 0 {
        UNIX_EPOCH + Duration::from_millis(mtime_ms as u64)
    } else {
        UNIX_EPOCH
    };
    let file = fs::OpenOptions::new().write(true).open(long(path))?;
    file.set_modified(when)
}

/// Progress sender that drops calls closer together than [`PROGRESS_INTERVAL`].
struct Reporter<'a> {
    cb: &'a mut dyn FnMut(&ImportProgress),
    last: Option<Instant>,
    file_count: usize,
    bytes_total: u64,
}

impl Reporter<'_> {
    /// Sends a progress update unless one went out less than [`PROGRESS_INTERVAL`] ago.
    fn send(&mut self, file_index: usize, bytes_done: u64, name: &str) {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < PROGRESS_INTERVAL)
        {
            return;
        }
        self.last = Some(now);
        self.emit(file_index, bytes_done, name);
    }

    /// Sends a progress update without throttling.
    fn emit(&mut self, file_index: usize, bytes_done: u64, name: &str) {
        (self.cb)(&ImportProgress {
            file_index,
            file_count: self.file_count,
            bytes_done,
            bytes_total: self.bytes_total,
            current_name: name.to_string(),
        });
    }
}

/// A file or unreadable entry found while walking the sources.
struct Candidate {
    path: PathBuf,
    size: u64,
    mtime_ms: i64,
    /// Set when the entry could not be read.
    broken: Option<String>,
}

/// Walks `path`. Junk and zero-byte files are skipped. Symlinks found inside
/// folders are not followed.
fn walk(path: &Path, depth: usize, out: &mut Vec<Candidate>) {
    if is_junk(&file_name_of(path)) {
        return;
    }
    let meta = match fs::metadata(long(path)) {
        Ok(m) => m,
        Err(e) => {
            out.push(broken(path, format!("cannot read: {e}")));
            return;
        }
    };
    if meta.is_dir() {
        if depth >= MAX_DEPTH {
            return;
        }
        let entries = match fs::read_dir(long(path)) {
            Ok(entries) => entries,
            Err(e) => {
                out.push(broken(path, format!("cannot list folder: {e}")));
                return;
            }
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            walk(&path.join(entry.file_name()), depth + 1, out);
        }
    } else if meta.is_file() && meta.len() > 0 {
        out.push(Candidate {
            path: path.to_path_buf(),
            size: meta.len(),
            mtime_ms: mtime_ms(&meta),
            broken: None,
        });
    }
}

/// A candidate that could not be read.
fn broken(path: &Path, msg: String) -> Candidate {
    Candidate {
        path: path.to_path_buf(),
        size: 0,
        mtime_ms: 0,
        broken: Some(msg),
    }
}

/// True for entries the scanner also ignores: recycle bin items, thumbnail
/// caches and temp folders.
fn is_junk(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("$r") || lower.starts_with("$i") || lower == "thumbnails" || lower == "temp"
}

/// Game folder for a file. Uses the parent folder name, or `Imported` when that
/// folder is generic or missing.
fn derive_game(path: &Path) -> String {
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str());
    match parent {
        Some(name)
            if !GENERIC_FOLDERS
                .iter()
                .any(|g| g.eq_ignore_ascii_case(name.trim())) =>
        {
            naming::sanitize(name)
        }
        _ => IMPORTED_GAME.to_string(),
    }
}

/// True when the extension is one of [`SUPPORTED_EXTENSIONS`] (case-insensitive).
fn is_supported(path: &Path) -> bool {
    let ext = ext_of(path);
    SUPPORTED_EXTENSIONS.contains(&ext.as_str())
}

/// Lower-case extension without the dot, or an empty string.
fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// File name as text, lossy for non-UTF-8 names.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Modified time in Unix milliseconds, or 0 when unknown.
fn mtime_ms(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// True when `path` is a file of the given size and modified time.
fn same_file_at(path: &Path, size: u64, mtime: i64) -> bool {
    fs::metadata(long(path))
        .map(|m| m.is_file() && m.len() == size && mtime_ms(&m) == mtime)
        .unwrap_or(false)
}

/// First free destination for `name` in `dir`. Tries `name`, then `stem (2).ext`
/// and up. A name is free when it is not on disk and not already planned.
fn pick_dest(dir: &Path, name: &str, taken: &HashSet<PathBuf>) -> PathBuf {
    let free = |p: &PathBuf| !taken.contains(p) && !long(p).exists();
    let first = dir.join(name);
    if free(&first) {
        return first;
    }
    let (stem, ext) = split_ext(name);
    for n in 2..=MAX_SUFFIX {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if free(&candidate) {
            return candidate;
        }
    }
    dir.join(format!("{stem} ({MAX_SUFFIX}){ext}"))
}

/// Splits `name` at its last dot. The dot stays with the extension, which is
/// empty when there is no dot.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// `<dest>.part`, the temporary name for a copy in progress.
fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Adds the `\\?\` prefix (`\\?\UNC\` for network paths) to a long absolute
/// path on Windows, so the Win32 file calls accept it. Other paths are returned
/// unchanged.
#[cfg(windows)]
fn long(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if !path.is_absolute()
        || text.starts_with(r"\\?\")
        || text.encode_utf16().count() <= LONG_PATH_UNITS
    {
        return path.to_path_buf();
    }
    match text.strip_prefix(r"\\") {
        Some(rest) => PathBuf::from(format!(r"\\?\UNC\{rest}")),
        None => PathBuf::from(format!(r"\\?\{text}")),
    }
}

/// Non-Windows: paths are never prefixed.
#[cfg(not(windows))]
fn long(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Free bytes on the volume holding `path`, through `GetDiskFreeSpaceExW`.
#[cfg(windows)]
fn free_bytes_available(path: &Path) -> io::Result<u64> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    let mut avail: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that lives across the
    // call. `avail` is a valid, writable u64 that outlives the call and is
    // passed as a raw pointer. The two out-parameters we do not need are None.
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut avail as *mut u64),
            None,
            None,
        )
    }
    .map_err(io::Error::other)?;
    Ok(avail)
}

/// Non-Windows: free space is not available.
#[cfg(not(windows))]
fn free_bytes_available(_path: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "free space query needs Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ext_keeps_dot_with_extension() {
        assert_eq!(split_ext("clip.mp4"), ("clip", ".mp4"));
        assert_eq!(split_ext("a.b.mkv"), ("a.b", ".mkv"));
        assert_eq!(split_ext("noext"), ("noext", ""));
        assert_eq!(split_ext(".hidden"), (".hidden", ""));
    }

    #[test]
    fn junk_names_are_recognised() {
        assert!(is_junk("$RECYCLE.BIN"));
        assert!(is_junk("$I12AB.mp4"));
        assert!(is_junk("Thumbnails"));
        assert!(is_junk("temp"));
        assert!(!is_junk("Temporary.mp4"));
        assert!(!is_junk("Elden Ring"));
    }

    #[test]
    fn generic_parents_give_imported() {
        assert_eq!(
            derive_game(Path::new(r"C:\Users\x\Videos\a.mp4")),
            "Imported"
        );
        assert_eq!(derive_game(Path::new(r"C:\a.mp4")), "Imported");
        assert_eq!(
            derive_game(Path::new(r"D:\Old\Elden Ring\a.mp4")),
            "Elden Ring"
        );
    }

    #[test]
    fn long_paths_get_prefix() {
        let short = Path::new(r"C:\short.mp4");
        assert_eq!(long(short), short.to_path_buf());
        let deep = format!(r"C:\{}\a.mp4", "d".repeat(300));
        if cfg!(windows) {
            assert!(long(Path::new(&deep))
                .to_string_lossy()
                .starts_with(r"\\?\C:\"));
        }
    }
}
