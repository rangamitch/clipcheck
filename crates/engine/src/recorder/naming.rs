//! File naming helpers for recorder outputs: sanitized game names, UTC
//! timestamps for file names, and collision-free output paths.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Longest game name kept in a file or folder name, in characters (enforced by the shared
/// sanitizer in clipcheck-core; kept here for the tests).
#[cfg(test)]
const MAX_NAME_CHARS: usize = 80;
/// Upper bound on numbered alternatives tried by [`unique_path`].
const MAX_SUFFIX: u32 = 10_000;

/// Makes a game name safe to use as a folder name and as a file-name prefix.
///
/// Rules, in order:
/// - Characters in `\ / : * ? " < > |` are removed. Control characters become
///   spaces.
/// - Runs of whitespace collapse to one space, and leading or trailing spaces
///   and dots are removed.
/// - A reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`,
///   `LPT1`-`LPT9`, with or without an extension) gets a `_` prefix.
/// - The result is capped at 80 characters.
/// - An empty result becomes `Unsorted`.
pub fn sanitize(name: &str) -> String {
    // Single source of truth shared with the library catalog and the config crate.
    clipcheck_core::config::schema::sanitize_game_folder(name)
}

/// Folder that recordings for `game` are written to. With `per_game` the
/// folder is `base/<game>`, otherwise `base` itself. `game` must already be
/// sanitized.
pub fn game_dir(base: &Path, game: &str, per_game: bool) -> PathBuf {
    if per_game {
        base.join(game)
    } else {
        base.to_path_buf()
    }
}

/// A fresh run directory name, unique per attach.
pub fn run_id() -> String {
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        // A clock set before 1970 is reported as its real negative offset, not as 0,
        // so ids and stamps stay distinct and never collapse onto the epoch.
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    format!("run-{nanos}")
}

/// Current Unix time in whole seconds. Negative when the clock is before 1970; the
/// real offset is kept rather than mapped to 0 (the epoch).
pub fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Formats Unix seconds as `yyyy-MM-dd HH-mm-ss` in UTC. Colons are avoided so
/// the result can be used in a file name. The formatter is local to this crate
/// (no time-zone database), so stamps are UTC.
pub fn utc_stamp(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Converts days since 1970-01-01 to (year, month, day). Howard Hinnant's
/// `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    // March-based month (mp 0..=9 is Mar..Dec, 10..=11 is Jan..Feb), converted once.
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Paths handed out by [`unique_path`] in this process. A path is reserved
/// when it is returned, before any file exists, so two requests made before
/// the first file is written cannot receive the same name. Entries are never
/// released: a reserved name that was never written only costs a skipped
/// number.
static RESERVED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Returns `dir/stem.ext`, or `dir/stem (n).ext` for the first free `n >= 2`
/// when that name is taken.
///
/// A name is taken when a file already exists there or an earlier call in this
/// process reserved it, so the returned path is distinct from every path this
/// process returned before. The reservation does not create a file. Writers
/// that replace the file atomically (rename onto the path) are unaffected.
///
/// Only a name the process has not reserved and that does not exist is
/// returned. If all `MAX_SUFFIX` numbered names are taken, the result carries a
/// per-run unique token instead of an existing number, so a clip is never
/// overwritten.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut reserved = RESERVED.lock().unwrap_or_else(|e| e.into_inner());
    let is_free = |p: &PathBuf| !reserved.contains(p) && !p.exists();

    let first = dir.join(format!("{stem}.{ext}"));
    let chosen = if is_free(&first) {
        first
    } else {
        (2..MAX_SUFFIX)
            .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
            .find(|p| is_free(p))
            .unwrap_or_else(|| dir.join(format!("{stem} ({}).{ext}", run_id())))
    };
    reserved.insert(chosen.clone());
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_forbidden_characters() {
        assert_eq!(sanitize("Game: A/B?*"), "Game AB");
        assert_eq!(sanitize("a<b>c|d\"e\\f"), "abcdef");
        assert_eq!(sanitize("  trailing dots...  "), "trailing dots");
        assert_eq!(sanitize("..leading"), "leading");
        assert_eq!(sanitize("two   spaces\tand\ttabs"), "two spaces and tabs");
        assert_eq!(sanitize("tab\there"), "tab here");
        assert_eq!(sanitize("Ünïcode 日本"), "Ünïcode 日本");
    }

    #[test]
    fn sanitize_empty_becomes_unsorted() {
        assert_eq!(sanitize("   "), "Unsorted");
        assert_eq!(sanitize(""), "Unsorted");
        assert_eq!(sanitize("???"), "Unsorted");
        assert_eq!(sanitize("..."), "Unsorted");
    }

    #[test]
    fn sanitize_prefixes_reserved_names() {
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul"), "_nul");
        assert_eq!(sanitize("COM1"), "_COM1");
        assert_eq!(sanitize("lpt9.txt"), "_lpt9.txt");
        assert_eq!(sanitize("COM10"), "COM10");
        assert_eq!(sanitize("CONSOLE"), "CONSOLE");
    }

    #[test]
    fn sanitize_caps_length_and_keeps_no_trailing_space() {
        assert_eq!(sanitize(&"x".repeat(200)).chars().count(), MAX_NAME_CHARS);
        let name = format!("{} tail", "y".repeat(79));
        let out = sanitize(&name);
        assert!(out.chars().count() <= MAX_NAME_CHARS);
        assert!(!out.ends_with(' '));
        // Idempotent for already clean names.
        assert_eq!(sanitize(&out), out);
    }

    #[test]
    fn game_dir_per_game_and_flat() {
        let base = Path::new(r"D:\Root\Clips");
        assert_eq!(game_dir(base, "Elden Ring", true), base.join("Elden Ring"));
        assert_eq!(game_dir(base, "Elden Ring", false), base.to_path_buf());
    }

    #[test]
    fn utc_stamp_known_values() {
        assert_eq!(utc_stamp(0), "1970-01-01 00-00-00");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14 22-13-20");
        // 2024-02-29 (leap day) 12:34:56 UTC
        assert_eq!(utc_stamp(1_709_210_096), "2024-02-29 12-34-56");
    }

    #[test]
    fn utc_stamp_pre_1970_uses_negative_offsets() {
        // One second before the epoch is the last second of 1969.
        assert_eq!(utc_stamp(-1), "1969-12-31 23-59-59");
        // 1970-01-01 00:00:00 minus one day.
        assert_eq!(utc_stamp(-86_400), "1969-12-31 00-00-00");
        // 1900-03-01 00:00:00 UTC (day -25_508, 1900 is not a leap year).
        assert_eq!(utc_stamp(-25_508 * 86_400), "1900-03-01 00-00-00");
    }

    #[test]
    fn utc_stamp_century_and_leap_boundaries() {
        // 2000 is a leap year (divisible by 400); the day after 2000-02-28 is 02-29.
        assert_eq!(utc_stamp(11_016 * 86_400), "2000-02-29 00-00-00");
        assert_eq!(utc_stamp(11_017 * 86_400), "2000-03-01 00-00-00");
        // 2100 is not a leap year (divisible by 100, not 400): 02-28 is followed by 03-01.
        assert_eq!(utc_stamp(47_540 * 86_400), "2100-02-28 00-00-00");
        assert_eq!(utc_stamp(47_541 * 86_400), "2100-03-01 00-00-00");
    }

    #[test]
    fn utc_stamp_latest_four_digit_year() {
        // 9999-12-31 00:00:00 UTC, the last day of year 9999.
        assert_eq!(utc_stamp(2_932_896 * 86_400), "9999-12-31 00-00-00");
        // 9999-12-31 23:59:59 UTC.
        assert_eq!(utc_stamp(253_402_300_799), "9999-12-31 23-59-59");
    }

    #[test]
    fn utc_stamp_extremes_do_not_overflow() {
        // Far outside the calendar range: must not panic, and the time of day
        // stays within a valid clock time.
        for secs in [i64::MIN, i64::MAX] {
            let s = utc_stamp(secs);
            let time = &s[s.len() - 8..];
            let parts: Vec<u32> = time
                .split('-')
                .map(|p| p.parse().expect("digits"))
                .collect();
            assert_eq!(parts.len(), 3, "stamp {s}");
            assert!(parts[0] < 24 && parts[1] < 60 && parts[2] < 60, "stamp {s}");
        }
    }

    #[test]
    fn unique_path_appends_number_when_taken() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = unique_path(dir.path(), "clip", "mp4");
        assert_eq!(first, dir.path().join("clip.mp4"));
        std::fs::write(&first, b"x").expect("write");
        assert_eq!(
            unique_path(dir.path(), "clip", "mp4"),
            dir.path().join("clip (2).mp4")
        );
    }

    #[test]
    fn unique_path_distinct_for_back_to_back_requests() {
        // Nothing is written between the calls, as when two clips are requested
        // in the same second before the first worker creates its file.
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = "Game 2026-01-01 00-00-00 clip-30s";
        let a = unique_path(dir.path(), stem, "mp4");
        let b = unique_path(dir.path(), stem, "mp4");
        let c = unique_path(dir.path(), stem, "mp4");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        assert_eq!(
            b,
            dir.path().join("Game 2026-01-01 00-00-00 clip-30s (2).mp4")
        );
    }
}
