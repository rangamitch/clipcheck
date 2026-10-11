//! Filesystem operations for the library (design doc 07 §4.4 naming and §5.3 rename).
//!
//! Pure helpers: [`sanitize_filename`], [`expand_template`], [`unique_path`],
//! [`validate_user_stem`]. Disk + database operation: [`rename_on_disk_and_db`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::db::{Library, RecStatus, RecordingKind, RecordingRow};
use crate::LibError;

/// Maximum length of a sanitized filename, in UTF-16 units.
pub const MAX_FILENAME_UTF16: usize = 200;
/// Maximum length of a user-typed stem (without extension), in UTF-16 units.
pub const MAX_STEM_UTF16: usize = 120;
/// Stem used when sanitization leaves nothing.
const FALLBACK_NAME: &str = "Untitled";
/// Characters Windows does not allow in file names.
const ILLEGAL_CHARS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

fn is_illegal(c: char) -> bool {
    ILLEGAL_CHARS.contains(&c) || c.is_control()
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Returns the longest prefix of `s` whose UTF-16 length is at most `max`.
fn truncate_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (idx, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..idx];
        }
    }
    s
}

/// True for CON, PRN, AUX, NUL, COM1-9, COM¹-³, LPT1-9 and LPT¹-³, with or without an
/// extension (case-insensitive). The superscript digits U+00B9, U+00B2 and U+00B3 are
/// reserved by Windows as well, so they are matched as chars, not ASCII bytes.
fn is_reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    match base.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let mut chars = base.chars();
            let (Some(a), Some(b), Some(c), Some(d), None) = (
                chars.next(),
                chars.next(),
                chars.next(),
                chars.next(),
                chars.next(),
            ) else {
                return false;
            };
            matches!((a, b, c), ('C', 'O', 'M') | ('L', 'P', 'T'))
                && matches!(d, '1'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}')
        }
    }
}

/// Makes arbitrary text safe to use as a single Windows file name component.
///
/// Replaces `\ / : * ? " < > |` with `_`, removes control characters, trims surrounding
/// whitespace, removes trailing dots and spaces, caps the length at 200 UTF-16 units,
/// prefixes `_` to reserved device names (`CON`, `COM1`, ...), and never returns an empty
/// string (it returns `Untitled` instead). The function is idempotent.
pub fn sanitize_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if ILLEGAL_CHARS.contains(&c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let body = trim_tail(truncate_utf16(trimmed, MAX_FILENAME_UTF16));
    let body = if body.is_empty() {
        FALLBACK_NAME.to_string()
    } else {
        body.to_string()
    };
    if !is_reserved(&body) {
        return body;
    }
    let prefixed = format!("_{body}");
    trim_tail(truncate_utf16(&prefixed, MAX_FILENAME_UTF16)).to_string()
}

fn trim_tail(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == '.' || c.is_whitespace())
}

/// Error returned by [`validate_user_stem`] for rejected user input.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum NameError {
    /// The input contains characters Windows does not allow (or control characters).
    #[error("contains illegal characters: {0:?}")]
    IllegalChars(Vec<char>),
    /// The input is a reserved device name such as `CON` or `COM1`.
    #[error("reserved name")]
    Reserved,
    /// The input is empty after trimming.
    #[error("name is empty")]
    Empty,
    /// The input exceeds the stem length limit.
    #[error("name too long (max {max})")]
    TooLong {
        /// Maximum allowed length in UTF-16 units.
        max: usize,
    },
    /// The input ends with a dot or a space.
    #[error("name cannot end with a dot or space")]
    TrailingDotOrSpace,
}

/// Strictly validates a user-typed file stem (no extension). Only trims surrounding whitespace.
///
/// Unlike [`sanitize_filename`], nothing is silently rewritten: invalid input is an error.
pub fn validate_user_stem(raw: &str) -> Result<String, NameError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(NameError::Empty);
    }
    let bad: Vec<char> = s.chars().filter(|&c| is_illegal(c)).collect();
    if !bad.is_empty() {
        return Err(NameError::IllegalChars(bad));
    }
    if s.ends_with('.') || s.ends_with(' ') {
        return Err(NameError::TrailingDotOrSpace);
    }
    if is_reserved(s) {
        return Err(NameError::Reserved);
    }
    if utf16_len(s) > MAX_STEM_UTF16 {
        return Err(NameError::TooLong {
            max: MAX_STEM_UTF16,
        });
    }
    Ok(s.to_string())
}

/// Variables available to [`expand_template`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemplateVars<'a> {
    /// Game display name (empty string if unknown; the caller supplies the fallback text).
    pub game: &'a str,
    /// Recording start time, Unix milliseconds UTC. Rendered in UTC.
    pub timestamp_ms: i64,
    /// Clip or session.
    pub kind: RecordingKind,
    /// 1-based part index for session parts; `None` renders as an empty string.
    pub part: Option<u32>,
}

/// Expands a filename template into a sanitized stem.
///
/// Supported tokens: `{game}`, `{kind}`, `{part}`, and date/time patterns built from
/// `yyyy`, `MM`, `dd`, `HH`, `mm`, `ss`. A brace group containing any date pattern is
/// formatted as a date, so both `{yyyy}-{MM}-{dd}` and `{yyyy-MM-dd HH-mm-ss}` work.
/// Unknown brace groups are copied literally. Timestamps are formatted in UTC.
///
/// The result goes through [`sanitize_filename`]. If the expansion is blank, the stem
/// becomes `ClipCheck yyyy-MM-dd HH-mm-ss`.
pub fn expand_template(template: &str, vars: &TemplateVars<'_>) -> String {
    let civil = civil_from_ms(vars.timestamp_ms);
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        let name = &after[..close];
        match expand_token(name, vars, &civil) {
            Some(v) => out.push_str(&v),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);

    if out.trim().is_empty() {
        let fallback = format_date_pattern("ClipCheck yyyy-MM-dd HH-mm-ss", &civil);
        return sanitize_filename(&fallback);
    }
    sanitize_filename(&out)
}

fn expand_token(name: &str, vars: &TemplateVars<'_>, civil: &Civil) -> Option<String> {
    match name {
        "game" => Some(vars.game.to_string()),
        "kind" => Some(vars.kind.as_str().to_string()),
        "part" => Some(vars.part.map(|p| p.to_string()).unwrap_or_default()),
        _ if DATE_TOKENS.iter().any(|t| name.contains(t)) => Some(format_date_pattern(name, civil)),
        _ => None,
    }
}

const DATE_TOKENS: [&str; 6] = ["yyyy", "MM", "dd", "HH", "mm", "ss"];

/// Broken-down UTC time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

/// Converts Unix milliseconds to a UTC calendar date (proleptic Gregorian, Howard Hinnant's algorithm).
fn civil_from_ms(ms: i64) -> Civil {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);

    Civil {
        year,
        month,
        day,
        hour: rem / 3600,
        minute: (rem % 3600) / 60,
        second: rem % 60,
    }
}

/// Replaces date tokens in `pattern`; all other characters are copied unchanged.
fn format_date_pattern(pattern: &str, c: &Civil) -> String {
    let fields: [(&str, String); 6] = [
        ("yyyy", format!("{:04}", c.year)),
        ("MM", format!("{:02}", c.month)),
        ("dd", format!("{:02}", c.day)),
        ("HH", format!("{:02}", c.hour)),
        ("mm", format!("{:02}", c.minute)),
        ("ss", format!("{:02}", c.second)),
    ];
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let rest = &chars[i..];
        let hit = fields
            .iter()
            .find(|(tok, _)| rest.iter().copied().take(tok.len()).eq(tok.chars()));
        match hit {
            Some((tok, value)) => {
                out.push_str(value);
                i += tok.len();
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Returns `dir\stem.ext` if free, otherwise `dir\stem (2).ext`, `dir\stem (3).ext`, ...
///
/// An empty `ext` produces no dot. Existence is checked with `Path::exists`, which is
/// case-insensitive on Windows, so `Clip.mp4` blocks `clip.mp4`.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let make = |suffix: &str| -> PathBuf {
        let name = if ext.is_empty() {
            format!("{stem}{suffix}")
        } else {
            format!("{stem}{suffix}.{ext}")
        };
        dir.join(name)
    };
    let first = make("");
    if !first.exists() {
        return first;
    }
    for n in 2..=10_000u32 {
        let candidate = make(&format!(" ({n})"));
        if !candidate.exists() {
            return candidate;
        }
    }
    make(&format!(" ({})", now_millis()))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Error returned by [`rename_on_disk_and_db`] and related operations.
#[derive(Debug, thiserror::Error)]
pub enum FsOpError {
    /// The requested name is not valid.
    #[error("invalid name: {0}")]
    Name(#[from] NameError),
    /// No recording with that id.
    #[error("recording not found")]
    NotFound,
    /// The recording is still being written.
    #[error("the recording is still being written")]
    Busy,
    /// The recording's file is not on disk.
    #[error("the file is missing on disk")]
    MissingOnDisk,
    /// Another file already has that name.
    #[error("A file named '{0}' already exists.")]
    Collision(String),
    /// The file is locked by another program, even after retries.
    #[error("The file is in use by another program. Try again after closing it.")]
    InUse,
    /// The database update failed; the file rename was rolled back.
    #[error("database update failed; the rename was rolled back: {0}")]
    DbRolledBack(LibError),
    /// The database update failed and the file could not be moved back. The file is at the
    /// new path, or at a temporary name next to it after a case-only rename;
    /// [`recover_rename_temps`] restores a temporary name once its original name is free.
    #[error("database update failed ({db}) and the file could not be moved back ({io})")]
    RollbackFailed {
        /// The database error.
        db: LibError,
        /// The error from the rollback rename.
        io: io::Error,
    },
    /// A case-only rename failed and the file could not be moved back from its temporary
    /// name. The file is at `temp`, and `original` is the name it had before. The recording
    /// row was not changed. [`recover_rename_temps`] restores the file once `original` is free.
    #[error(
        "rename failed ({rename}) and the file could not be moved back ({back}); \
         it is at '{temp}' and its original name was '{original}'"
    )]
    StrandedTemp {
        /// The original path, which the file should have.
        original: PathBuf,
        /// The temporary path where the file was left.
        temp: PathBuf,
        /// The error from the rename to the requested name.
        rename: io::Error,
        /// The error from moving the file back to `original`.
        back: io::Error,
    },
    /// A database error outside the rename step.
    #[error("library: {0}")]
    Lib(#[from] LibError),
    /// Any other filesystem error.
    #[error("io: {0}")]
    Io(io::Error),
}

/// Result of a successful [`rename_on_disk_and_db`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenameOutcome {
    /// The recording row after the rename (`file_name` and `rel_path` updated).
    pub row: RecordingRow,
    /// The relative path before the rename.
    pub old_rel_path: String,
    /// False if the requested name equalled the current name and nothing changed.
    pub changed: bool,
}

/// Renames a recording's file on disk, then updates the database row.
///
/// `new_name` is the stem typed by the user, without extension; the original extension is
/// kept. Steps: validate the stem, check the row status, reject name collisions (no automatic
/// suffix), rename the file, then call [`Library::rename_recording_record`]. If the database
/// step fails, the file rename is undone.
///
/// Case-only changes (`Clip` to `clip`) go through a temporary name, so they work on
/// case-insensitive NTFS. A sharing or access-denied error is retried with backoff
/// (100, 250, 500, 1000 ms) before `InUse` is returned.
///
/// The disk rename uses `std::fs::rename`, which replaces existing files on Windows. The
/// collision check runs just before it, so a file created in between could be replaced.
/// This race is accepted for v1.
pub fn rename_on_disk_and_db(
    lib: &Library,
    id: &str,
    new_name: &str,
    root: &Path,
) -> Result<RenameOutcome, FsOpError> {
    let stem = validate_user_stem(new_name)?;
    let row = lib.get_recording(id)?.ok_or(FsOpError::NotFound)?;
    if row.status == RecStatus::Recording {
        return Err(FsOpError::Busy);
    }
    if row.status == RecStatus::Missing {
        return Err(FsOpError::MissingOnDisk);
    }

    let new_file_name = if row.ext.is_empty() {
        stem
    } else {
        format!("{stem}.{}", row.ext)
    };
    if new_file_name == row.file_name {
        return Ok(RenameOutcome {
            old_rel_path: row.rel_path.clone(),
            row,
            changed: false,
        });
    }

    let dir_prefix = dir_prefix_of(&row.rel_path);
    let new_rel = format!("{dir_prefix}{new_file_name}");
    let old_abs = root.join(&row.rel_path);
    let new_abs = root.join(&new_rel);
    let case_only = new_file_name.to_lowercase() == row.file_name.to_lowercase();

    if !case_only && new_abs.exists() {
        return Err(FsOpError::Collision(new_file_name));
    }

    move_file(&old_abs, &new_abs, case_only).map_err(|e| e.into_fs_error(&new_file_name))?;

    if let Err(db_err) = lib.rename_recording_record(id, &new_file_name, &new_rel) {
        return match move_file(&new_abs, &old_abs, case_only) {
            Ok(()) => Err(FsOpError::DbRolledBack(db_err)),
            Err(MoveError::Io(io_err)) | Err(MoveError::Stranded { back: io_err, .. }) => {
                Err(FsOpError::RollbackFailed {
                    db: db_err,
                    io: io_err,
                })
            }
        };
    }

    let old_rel_path = row.rel_path.clone();
    let mut updated = row;
    updated.file_name = new_file_name;
    updated.rel_path = new_rel;
    Ok(RenameOutcome {
        row: updated,
        old_rel_path,
        changed: true,
    })
}

/// Returns the directory part of a relative path, including its trailing separator, or "".
fn dir_prefix_of(rel: &str) -> &str {
    match rel.rfind(['/', '\\']) {
        Some(idx) => &rel[..=idx],
        None => "",
    }
}

fn map_io(e: io::Error, name: &str) -> FsOpError {
    if is_busy(&e) {
        FsOpError::InUse
    } else {
        match e.kind() {
            io::ErrorKind::NotFound => FsOpError::MissingOnDisk,
            io::ErrorKind::AlreadyExists => FsOpError::Collision(name.to_string()),
            _ => FsOpError::Io(e),
        }
    }
}

/// Sharing violation (32), lock violation (33) or access denied (5).
fn is_busy(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33))
}

/// Failure of [`move_file`].
#[derive(Debug)]
enum MoveError {
    /// A rename failed. The file is still at its original path, or at the path it had before
    /// the failed step.
    Io(io::Error),
    /// A case-only rename failed and moving the file back also failed. The file is at `temp`.
    Stranded {
        original: PathBuf,
        temp: PathBuf,
        rename: io::Error,
        back: io::Error,
    },
}

impl MoveError {
    fn into_fs_error(self, name: &str) -> FsOpError {
        match self {
            MoveError::Io(e) => map_io(e, name),
            MoveError::Stranded {
                original,
                temp,
                rename,
                back,
            } => FsOpError::StrandedTemp {
                original,
                temp,
                rename,
                back,
            },
        }
    }
}

/// Moves `from` to `to`. Case-only changes go through a temporary sibling name, and a failed
/// second step is rolled back by moving the file from the temporary name to `from`.
fn move_file(from: &Path, to: &Path, case_only: bool) -> Result<(), MoveError> {
    move_file_with(from, to, case_only, &rename_with_retry)
}

/// [`move_file`] with the rename operation injected, so tests can force a failure.
fn move_file_with<R>(from: &Path, to: &Path, case_only: bool, rename: &R) -> Result<(), MoveError>
where
    R: Fn(&Path, &Path) -> io::Result<()>,
{
    if !case_only {
        return rename(from, to).map_err(MoveError::Io);
    }
    let tmp = temp_sibling(from);
    rename(from, &tmp).map_err(MoveError::Io)?;
    let rename_err = match rename(&tmp, to) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    // The original name is free again, so put the file back under it.
    match rename(&tmp, from) {
        Ok(()) => Err(MoveError::Io(rename_err)),
        Err(back) => Err(MoveError::Stranded {
            original: from.to_path_buf(),
            temp: tmp,
            rename: rename_err,
            back,
        }),
    }
}

fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    const DELAYS_MS: [u64; 4] = [100, 250, 500, 1000];
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if is_busy(&e) && attempt < DELAYS_MS.len() => {
                thread::sleep(Duration::from_millis(DELAYS_MS[attempt]));
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Marker between the original file name and the timestamp in a rename temp name.
const RENAME_TEMP_MARKER: &str = ".clipcheck-rename-";
/// Extension of a rename temp name.
const RENAME_TEMP_EXT: &str = ".tmp";

/// Temp name for a case-only rename: `<original file name>.clipcheck-rename-<millis>.tmp`.
/// The temp name carries the original name, so [`recover_rename_temps`] can restore it.
fn temp_sibling(from: &Path) -> PathBuf {
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    from.with_file_name(format!(
        "{name}{RENAME_TEMP_MARKER}{}{RENAME_TEMP_EXT}",
        now_millis()
    ))
}

/// Returns the original file name encoded in a rename temp name, or `None` if `name` does not
/// have the exact `<original>.clipcheck-rename-<digits>.tmp` form.
fn original_name_of_temp(name: &str) -> Option<&str> {
    let idx = name.rfind(RENAME_TEMP_MARKER)?;
    let original = &name[..idx];
    let digits = name[idx + RENAME_TEMP_MARKER.len()..].strip_suffix(RENAME_TEMP_EXT)?;
    if original.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(original)
}

/// Outcome for one leftover rename temp file, as reported by [`recover_rename_temps`].
#[derive(Debug)]
pub enum Recovered {
    /// The temp file was renamed back to the original name it encodes.
    Restored {
        /// The temp file's path before recovery.
        temp: PathBuf,
        /// The original path the file was restored to.
        original: PathBuf,
    },
    /// The original name is already taken, so the temp file was left in place.
    Blocked {
        /// The temp file's path, unchanged.
        temp: PathBuf,
        /// The occupied original path.
        original: PathBuf,
    },
    /// Renaming the temp file back failed, so it was left in place.
    Failed {
        /// The temp file's path, unchanged.
        temp: PathBuf,
        /// The original path the rename targeted.
        original: PathBuf,
        /// The error from the rename.
        error: io::Error,
    },
}

/// Best-effort recovery of leftover case-only rename temp files in `dir` (not recursive).
///
/// A regular file named `<original>.clipcheck-rename-<millis>.tmp` is renamed back to
/// `<original>` when that name is free. If it is taken, the file is left alone. Files with any
/// other name are ignored. Nothing is ever deleted. An unreadable `dir` yields an empty list.
/// The free check and the rename are not atomic, which is the same race accepted elsewhere in
/// this module.
pub fn recover_rename_temps(dir: &Path) -> Vec<Recovered> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let Some(original_name) = name.to_str().and_then(original_name_of_temp) else {
            continue;
        };
        let temp = entry.path();
        let original = dir.join(original_name);
        out.push(if original.exists() {
            Recovered::Blocked { temp, original }
        } else {
            match rename_with_retry(&temp, &original) {
                Ok(()) => Recovered::Restored { temp, original },
                Err(error) => Recovered::Failed {
                    temp,
                    original,
                    error,
                },
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_illegal_and_reserved() {
        assert_eq!(sanitize_filename("a/b:c*d?e\"f<g>h|i"), "a_b_c_d_e_f_g_h_i");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_filename("..."), "Untitled");
        assert_eq!(sanitize_filename("  x. "), "x");
    }

    #[test]
    fn reserved_superscript_device_names_are_caught() {
        for name in [
            "COM\u{00B9}",
            "COM\u{00B2}",
            "COM\u{00B3}",
            "LPT\u{00B9}",
            "LPT\u{00B2}",
            "LPT\u{00B3}",
            "com\u{00B9}.mp4",
            "lpt\u{00B3}.clip.mp4",
        ] {
            assert!(is_reserved(name), "{name} should be reserved");
            assert!(
                validate_user_stem(name).is_err(),
                "{name} should be rejected"
            );
            assert_eq!(sanitize_filename(name), format!("_{name}"));
        }
        assert!(!is_reserved("COM\u{00B9}\u{00B9}"));
        assert!(!is_reserved("COM\u{00BC}"));
        assert!(!is_reserved("COM0"));
        assert!(!is_reserved("LPT\u{00B9}x"));
        assert!(!is_reserved("COMX"));
        assert_eq!(sanitize_filename("COM\u{00B9}.mp4"), "_COM\u{00B9}.mp4");
    }

    #[test]
    fn civil_epoch_and_leap_day() {
        let c = civil_from_ms(0);
        assert_eq!((c.year, c.month, c.day), (1970, 1, 1));
        // 2024-02-29 12:34:56 UTC
        let c = civil_from_ms(1_709_210_096_000);
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute, c.second),
            (2024, 2, 29, 12, 34, 56)
        );
    }

    #[test]
    fn template_expands_both_styles() {
        let vars = TemplateVars {
            game: "Elden Ring",
            timestamp_ms: 1_709_210_096_000,
            kind: RecordingKind::Clip,
            part: None,
        };
        assert_eq!(
            expand_template("{game} {yyyy-MM-dd HH-mm-ss} {kind}", &vars),
            "Elden Ring 2024-02-29 12-34-56 clip"
        );
        assert_eq!(
            expand_template("{game} {yyyy}-{MM}-{dd} {HH}-{mm}-{ss} {kind}", &vars),
            "Elden Ring 2024-02-29 12-34-56 clip"
        );
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn case_only_move_renames_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("Clip.mp4");
        let to = dir.path().join("clip.mp4");
        fs::write(&from, b"data").unwrap();
        move_file(&from, &to, true).unwrap();
        assert_eq!(names_in(dir.path()), vec!["clip.mp4".to_string()]);
        assert_eq!(fs::read(&to).unwrap(), b"data");
    }

    #[test]
    fn failed_second_step_moves_file_back_to_original_name() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("Clip.mp4");
        let to = dir.path().join("clip.mp4");
        fs::write(&from, b"data").unwrap();
        // Fail only the step that targets the new name; the move-back must succeed.
        let inject = |f: &Path, t: &Path| {
            if t == to.as_path() {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::rename(f, t)
            }
        };
        let err = move_file_with(&from, &to, true, &inject).unwrap_err();
        assert!(matches!(err, MoveError::Io(_)), "got {err:?}");
        assert_eq!(names_in(dir.path()), vec!["Clip.mp4".to_string()]);
        assert_eq!(fs::read(&from).unwrap(), b"data");
    }

    #[test]
    fn stranded_temp_is_reported_with_both_paths_and_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("Clip.mp4");
        let to = dir.path().join("clip.mp4");
        fs::write(&from, b"data").unwrap();
        // Fail the step to the new name and the move-back, so the file stays at the temp name.
        let inject = |f: &Path, t: &Path| {
            if t == to.as_path() || t == from.as_path() {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::rename(f, t)
            }
        };
        let err = move_file_with(&from, &to, true, &inject).unwrap_err();
        let MoveError::Stranded { original, temp, .. } = err else {
            panic!("expected a stranded temp file");
        };
        assert_eq!(original, from);
        assert!(temp.exists(), "file must remain at the temp name");
        assert!(!from.exists());

        let recovered = recover_rename_temps(dir.path());
        assert_eq!(recovered.len(), 1, "got {recovered:?}");
        assert!(
            matches!(&recovered[0], Recovered::Restored { original: o, .. } if *o == from),
            "got {recovered:?}"
        );
        assert_eq!(names_in(dir.path()), vec!["Clip.mp4".to_string()]);
        assert_eq!(fs::read(&from).unwrap(), b"data");
    }

    #[test]
    fn original_name_is_parsed_only_from_exact_temp_form() {
        assert_eq!(
            original_name_of_temp("Clip.mp4.clipcheck-rename-1700000000000.tmp"),
            Some("Clip.mp4")
        );
        assert_eq!(
            original_name_of_temp("a.clipcheck-rename-.clipcheck-rename-5.tmp"),
            Some("a.clipcheck-rename-")
        );
        assert_eq!(original_name_of_temp(".clipcheck-rename-5.tmp"), None);
        assert_eq!(original_name_of_temp("x.clipcheck-rename-.tmp"), None);
        assert_eq!(original_name_of_temp("x.clipcheck-rename-12a.tmp"), None);
        assert_eq!(original_name_of_temp("x.clipcheck-rename-5.mp4"), None);
        assert_eq!(original_name_of_temp("random.tmp"), None);
    }

    #[test]
    fn recover_leaves_blocked_temp_alone() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("Clip.mp4.clipcheck-rename-5.tmp");
        let original = dir.path().join("Clip.mp4");
        fs::write(&temp, b"temp").unwrap();
        fs::write(&original, b"other").unwrap();
        let recovered = recover_rename_temps(dir.path());
        assert!(
            matches!(recovered.as_slice(), [Recovered::Blocked { .. }]),
            "got {recovered:?}"
        );
        assert_eq!(fs::read(&temp).unwrap(), b"temp");
        assert_eq!(fs::read(&original).unwrap(), b"other");
    }

    #[test]
    fn recover_ignores_garbage_names() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "notes.txt",
            "b.tmp",
            "a.clipcheck-rename-x.tmp",
            ".clipcheck-rename-5.tmp",
        ] {
            fs::write(dir.path().join(name), b"keep").unwrap();
        }
        assert!(recover_rename_temps(dir.path()).is_empty());
        assert_eq!(names_in(dir.path()).len(), 4);
        assert!(recover_rename_temps(&dir.path().join("missing")).is_empty());
    }
}
