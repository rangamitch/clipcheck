//! Reconciles the storage roots in the library with the configuration (DESIGN.md D24).
//!
//! The primary root (`storage.root`) becomes the active root. Each enabled extra folder
//! (`storage.libraries`) becomes a legacy root. A root that left the configuration is
//! removed from the library with its rows. Files on disk are never touched.

use std::path::{Path, PathBuf};

use clipcheck_core::config::schema::Storage;
use clipcheck_library::{LibError, Library};

use crate::settings_map::expand_vars_with;

/// Longest label kept for an extra folder, in characters.
const MAX_LABEL_CHARS: usize = 40;

/// One root the configuration asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Desired {
    /// Normalized absolute folder path.
    pub path: String,
    /// Display label; empty for the primary root.
    pub label: String,
    /// True if the library must never write into the folder.
    pub read_only: bool,
    /// True for the primary root.
    pub primary: bool,
}

/// What a root sync changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncOutcome {
    /// Ids of every root that the configuration keeps.
    pub ids: Vec<i64>,
    /// True when the primary root row is read-only.
    pub primary_read_only: bool,
    /// Problems that did not stop the sync, for the user.
    pub warnings: Vec<String>,
}

/// Expands `%VAR%` references from the process environment. The configured storage root is
/// stored unexpanded (`%USERPROFILE%VideosClipCheck`), so every consumer must expand it.
pub(crate) fn expand_env(path: &str) -> String {
    expand_vars_with(path, |name| std::env::var(name).ok())
}

/// A copy of `storage` whose root and extra folders have their `%VAR%` references expanded.
pub(crate) fn expand_storage(mut storage: Storage) -> Storage {
    storage.root = PathBuf::from(expand_env(&storage.root.to_string_lossy()));
    for dir in &mut storage.libraries {
        dir.path = expand_env(&dir.path);
    }
    storage
}

/// The roots the configuration asks for, primary first. Disabled extra folders are left out.
/// Duplicate paths are dropped.
pub(crate) fn desired_roots(storage: &Storage) -> Vec<Desired> {
    let mut out: Vec<Desired> = Vec::new();
    let root = normalize(&expand_env(&storage.root.to_string_lossy()));
    if !root.is_empty() {
        out.push(Desired {
            path: root,
            label: String::new(),
            read_only: false,
            primary: true,
        });
    }
    for dir in storage.libraries.iter().filter(|dir| dir.enabled) {
        let path = normalize(&expand_vars_with(&dir.path, |name| {
            std::env::var(name).ok()
        }));
        if path.is_empty() || out.iter().any(|d| d.path.eq_ignore_ascii_case(&path)) {
            continue;
        }
        out.push(Desired {
            path,
            label: dir.label.trim().chars().take(MAX_LABEL_CHARS).collect(),
            read_only: dir.read_only,
            primary: false,
        });
    }
    out
}

/// Makes the library's roots match `desired`. Each root's availability is set from whether
/// its folder can be read. Errors on one root are warnings; database errors are returned.
pub(crate) fn sync_roots(lib: &Library, desired: &[Desired]) -> Result<SyncOutcome, LibError> {
    let before = lib.list_roots_with_stats()?;
    let mut out = SyncOutcome::default();
    for want in desired {
        let id = match lib.upsert_root(&want.path, &want.label, want.read_only) {
            Ok(id) => id,
            Err(e) => {
                out.warnings
                    .push(format!("library folder {} is not used: {e}", want.path));
                continue;
            }
        };
        if want.primary {
            if let Err(e) = lib.set_primary_root(id) {
                out.warnings.push(format!(
                    "{} could not be made the primary library: {e}",
                    want.path
                ));
            }
        }
        lib.set_root_availability(id, Path::new(&want.path).is_dir())?;
        out.ids.push(id);
    }
    for row in &before {
        if out.ids.contains(&row.id) {
            continue;
        }
        if let Err(e) = lib.remove_root_keep_files(row.id) {
            out.warnings.push(format!(
                "library folder {} could not be removed from the library: {e}",
                row.path
            ));
        }
    }
    out.primary_read_only = lib
        .list_roots_with_stats()?
        .iter()
        .any(|row| row.is_primary && row.read_only);
    Ok(out)
}

/// Trims whitespace and trailing separators. A drive root keeps its separator. Rows are stored
/// under this form, so every caller that names a root must use it too.
pub(crate) fn normalize(path: &str) -> String {
    let trimmed = path.trim();
    let stripped = trimmed.trim_end_matches(['\\', '/']);
    if stripped.len() < 3 && trimmed.len() >= 3 {
        return trimmed.chars().take(3).collect();
    }
    stripped.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipcheck_core::config::schema::LibraryDir;
    use std::path::PathBuf;

    #[test]
    fn desired_expands_env_vars_in_the_primary_root() {
        // Regression: the stock root `%USERPROFILE%VideosClipCheck` was stored literally, so
        // the root was offline and nothing was ever scanned.
        std::env::set_var("CC_TEST_SYNC_ROOT", r"C:Users	ester");
        let storage = Storage {
            root: PathBuf::from(r"%CC_TEST_SYNC_ROOT%VideosClipCheck"),
            ..Storage::default()
        };
        let roots = desired_roots(&storage);
        assert_eq!(roots[0].path, r"C:Users	esterVideosClipCheck");
        let expanded = expand_storage(storage);
        assert_eq!(
            expanded.root,
            PathBuf::from(r"C:Users	esterVideosClipCheck")
        );
    }

    #[test]
    fn normalize_strips_trailing_separators() {
        assert_eq!(normalize(r"D:\Clips\"), r"D:\Clips");
        assert_eq!(normalize(r"D:\"), r"D:\");
        assert_eq!(normalize("  E:/Vids/  "), "E:/Vids");
    }

    #[test]
    fn desired_puts_primary_first_and_skips_disabled() {
        let storage = Storage {
            root: PathBuf::from(r"C:\Videos\ClipCheck"),
            libraries: vec![
                LibraryDir {
                    path: r"D:\Medal\".to_owned(),
                    label: "  Medal  ".to_owned(),
                    read_only: true,
                    enabled: true,
                },
                LibraryDir {
                    path: r"E:\Off".to_owned(),
                    enabled: false,
                    ..LibraryDir::default()
                },
                LibraryDir {
                    path: r"c:\videos\clipcheck".to_owned(),
                    enabled: true,
                    ..LibraryDir::default()
                },
            ],
            ..Storage::default()
        };
        let roots = desired_roots(&storage);
        assert_eq!(roots.len(), 2);
        assert!(roots[0].primary);
        assert_eq!(roots[0].path, r"C:\Videos\ClipCheck");
        assert_eq!(roots[1].path, r"D:\Medal");
        assert_eq!(roots[1].label, "Medal");
        assert!(roots[1].read_only);
    }

    #[test]
    fn sync_adds_then_removes_roots_keeping_files() {
        let lib = Library::open_in_memory().expect("open");
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("primary");
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(&primary).expect("mkdir");
        std::fs::create_dir_all(&extra).expect("mkdir");
        let want = |p: &Path, primary: bool| Desired {
            path: normalize(&p.to_string_lossy()),
            label: String::new(),
            read_only: false,
            primary,
        };
        let first = sync_roots(&lib, &[want(&primary, true), want(&extra, false)]).expect("sync");
        assert_eq!(first.ids.len(), 2);
        assert!(!first.primary_read_only);
        let second = sync_roots(&lib, &[want(&primary, true)]).expect("sync");
        assert_eq!(second.ids.len(), 1);
        let roots = lib.list_roots_with_stats().expect("list");
        assert_eq!(roots.len(), 1);
        assert!(roots[0].is_primary);
        assert!(extra.is_dir(), "removing a root must not touch files");
    }
}
