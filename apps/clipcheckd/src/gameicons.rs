//! Best-effort caching of game exe icons under `<data dir>/icons`, so the UI can show
//! the game's own icon. The daemon calls [`ensure_async`] when it sees a game; the work
//! (resolving the exe path and extracting the icon) runs on a background thread.

use std::path::Path;

use clipcheck_detect::modules;
use clipcheck_library::icons::{has_icon, write_icon};

/// True when an icon should be extracted for this game: the process id is known and no
/// icon file is cached yet.
fn needs_icon(data_dir: &Path, game_id: &str, pid: u32) -> bool {
    pid != 0 && !has_icon(data_dir, game_id)
}

/// Ensures an icon file exists for `game_id` (`exe:<name>`) by extracting it from the
/// running process `pid`. Returns immediately; extraction and the file write happen on a
/// detached thread. Failures are ignored (the UI falls back to a letter tile).
pub fn ensure_async(data_dir: &Path, game_id: &str, pid: u32) {
    if !needs_icon(data_dir, game_id, pid) {
        return;
    }
    let data_dir = data_dir.to_path_buf();
    let game_id = game_id.to_owned();
    std::thread::spawn(move || {
        let Some(exe) = modules::exe_path(pid) else {
            return;
        };
        let Some(png) = crate::appinfo::icon_png(&exe) else {
            return;
        };
        let _ = write_icon(&data_dir, &game_id, &png);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cc-gameicons-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn needs_icon_is_false_without_pid_or_with_existing_icon() {
        let d = temp("pid0");
        assert!(!needs_icon(&d, "exe:game.exe", 0));

        let d2 = temp("exists");
        write_icon(&d2, "exe:game.exe", &PNG).unwrap();
        assert!(!needs_icon(&d2, "exe:game.exe", 5));

        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn needs_icon_is_true_when_icon_missing_and_pid_known() {
        let d = temp("missing");
        assert!(needs_icon(&d, "exe:game.exe", 5));
        let _ = fs::remove_dir_all(&d);
    }
}
