//! On-disk cache of game icons: one small PNG per game id under `<data dir>/icons`.
//!
//! The daemon writes icons when it sees a game; the UI reads them. Both sides use this module
//! so they agree on file names. All functions are best-effort: a missing or unreadable icon is
//! `None`, never an error, because the UI falls back to a letter tile.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Folder name inside the data directory.
pub const ICONS_DIR: &str = "icons";
/// Icons larger than this are ignored on read (guards against a corrupt or foreign file).
pub const MAX_ICON_BYTES: u64 = 256 * 1024;

/// Directory holding the cached icons for `data_dir`.
pub fn icons_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(ICONS_DIR)
}

/// File name for a game id: lower-cased, with every character outside `a-z 0-9 . _ -` replaced
/// by `_`, plus `.png`. Distinct ids can collide only if they differ solely in such characters,
/// which does not happen for `exe:<name>.exe` ids.
pub fn icon_file_name(game_id: &str) -> String {
    let stem: String = game_id
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = if stem.is_empty() {
        "_".to_owned()
    } else {
        stem
    };
    format!("{stem}.png")
}

/// Full path of the cached icon for `game_id`.
pub fn icon_path(data_dir: &Path, game_id: &str) -> PathBuf {
    icons_dir(data_dir).join(icon_file_name(game_id))
}

/// True when an icon file for `game_id` exists.
pub fn has_icon(data_dir: &Path, game_id: &str) -> bool {
    icon_path(data_dir, game_id).is_file()
}

/// Writes `png` as the icon for `game_id` (temp file, then rename, so readers never see a
/// partial file). Rejects bytes that do not start with the PNG signature.
pub fn write_icon(data_dir: &Path, game_id: &str, png: &[u8]) -> io::Result<()> {
    if !png.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "icon is not a PNG",
        ));
    }
    let dir = icons_dir(data_dir);
    fs::create_dir_all(&dir)?;
    let target = dir.join(icon_file_name(game_id));
    // Unique per write so concurrent writers never share (and rename away) one temp file.
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        "{}.{}-{seq}.tmp",
        icon_file_name(game_id),
        std::process::id()
    ));
    fs::write(&tmp, png)?;
    fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Reads the cached icon for `game_id`. `None` when absent, oversized or unreadable.
pub fn read_icon(data_dir: &Path, game_id: &str) -> Option<Vec<u8>> {
    let path = icon_path(data_dir, game_id);
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_ICON_BYTES {
        return None;
    }
    fs::read(path).ok()
}

/// Reads the icons for `game_ids`; ids without an icon are left out.
pub fn read_icons(data_dir: &Path, game_ids: &[String]) -> HashMap<String, Vec<u8>> {
    game_ids
        .iter()
        .filter_map(|id| read_icon(data_dir, id).map(|png| (id.clone(), png)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-icons-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn file_name_is_lowercase_and_safe() {
        assert_eq!(icon_file_name("exe:Terraria.exe"), "exe_terraria.exe.png");
        assert_eq!(icon_file_name("a/b\\c:d"), "a_b_c_d.png");
        assert_eq!(icon_file_name("  "), "_.png");
        assert_eq!(icon_file_name("Ünï"), "_n_.png");
    }

    #[test]
    fn write_then_read_round_trips() {
        let d = temp("rt");
        assert!(!has_icon(&d, "exe:a.exe"));
        write_icon(&d, "exe:a.exe", &PNG).unwrap();
        assert!(has_icon(&d, "exe:a.exe"));
        assert_eq!(read_icon(&d, "exe:a.exe").unwrap(), PNG);
        let all = read_icons(&d, &["exe:a.exe".to_owned(), "exe:none.exe".to_owned()]);
        assert_eq!(all.len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_non_png_and_ignores_oversized() {
        let d = temp("bad");
        assert!(write_icon(&d, "x", b"nope").is_err());
        fs::create_dir_all(icons_dir(&d)).unwrap();
        fs::write(icon_path(&d, "big"), vec![0u8; MAX_ICON_BYTES as usize + 1]).unwrap();
        assert!(read_icon(&d, "big").is_none());
        let _ = fs::remove_dir_all(&d);
    }
}
