//! Log access for the UI: where the logs live, and a copyable diagnostics bundle.
//!
//! The log folder mirrors the daemon: `CLIPCHECK_DATA_DIR\logs` when that variable is set (the
//! UI forwards it to the daemon as `--data-dir`), otherwise `%APPDATA%\ClipCheck\logs` (the
//! daemon's default data folder, see `clipcheckd::app::parse_args`).

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Name of the daemon's active log file. Mirrors `clipcheckd::logging::LOG_FILE_NAME`.
const LOG_FILE_NAME: &str = "clipcheckd.log";
/// Most daemon-log bytes a diagnostics report includes (active file plus rotation combined).
const LOG_BUDGET: usize = 300 * 1024;
/// Environment variable naming the data folder. Same name as `commands::DATA_DIR_ENV`.
const DATA_DIR_ENV: &str = "CLIPCHECK_DATA_DIR";

/// Folder holding `clipcheckd.log` and its rotations.
#[tauri::command]
pub fn get_log_dir() -> Result<String, String> {
    let dir = log_dir_from(
        env_var(DATA_DIR_ENV).as_deref(),
        env_var("APPDATA").as_deref(),
    )?;
    Ok(dir.display().to_string())
}

/// Text bundle for bug reports: versions, OS, and the tail of the daemon log.
#[tauri::command]
pub fn collect_diagnostics() -> Result<String, String> {
    let dir = log_dir_from(
        env_var(DATA_DIR_ENV).as_deref(),
        env_var("APPDATA").as_deref(),
    )?;
    Ok(build_report(&dir, &user_names(), now_unix()))
}

/// Writes the diagnostics bundle to a `.txt` file in the user's Downloads folder, returns its path.
#[tauri::command]
pub fn export_diagnostics() -> Result<String, String> {
    let dir = log_dir_from(
        env_var(DATA_DIR_ENV).as_deref(),
        env_var("APPDATA").as_deref(),
    )?;
    let now = now_unix();
    let report = build_report(&dir, &user_names(), now);
    let downloads = downloads_dir(env_var("USERPROFILE").as_deref())?;
    fs::create_dir_all(&downloads)
        .map_err(|e| format!("cannot create {}: {e}", downloads.display()))?;
    let path = downloads.join(format!("ClipCheck-diagnostics-{now}.txt"));
    fs::write(&path, report).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Resolves the log folder from the two environment values (pure, for tests). Blank counts as
/// unset. `CLIPCHECK_DATA_DIR` wins; otherwise `%APPDATA%\ClipCheck`. The folder need not exist.
fn log_dir_from(data_dir: Option<&str>, appdata: Option<&str>) -> Result<PathBuf, String> {
    if let Some(dir) = data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("logs"));
    }
    if let Some(app) = appdata.map(str::trim).filter(|a| !a.is_empty()) {
        return Ok(PathBuf::from(app).join("ClipCheck").join("logs"));
    }
    Err(format!(
        "cannot locate the log folder: neither {DATA_DIR_ENV} nor APPDATA is set"
    ))
}

/// The Downloads folder under `%USERPROFILE%` (pure, for tests).
fn downloads_dir(profile: Option<&str>) -> Result<PathBuf, String> {
    profile
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(p).join("Downloads"))
        .ok_or_else(|| "cannot locate Downloads: USERPROFILE is not set".to_owned())
}

/// Windows user names to redact: `USERNAME` and the final component of `USERPROFILE`.
fn user_names() -> Vec<String> {
    let mut names = Vec::new();
    if let Some(name) = env_var("USERNAME") {
        names.push(name);
    }
    if let Some(profile) = env_var("USERPROFILE") {
        if let Some(name) = Path::new(&profile).file_name().and_then(|n| n.to_str()) {
            names.push(name.to_owned());
        }
    }
    names.retain(|n| !n.trim().is_empty());
    names
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Drops the first, possibly partial, line of `buf`: everything through its first newline.
/// Without a newline the whole buffer is one partial line and is kept.
fn cut_partial_line(buf: &[u8]) -> &[u8] {
    match buf.iter().position(|&b| b == b'\n') {
        Some(i) => &buf[i + 1..],
        None => buf,
    }
}

/// Given `window` = the byte just before a tail, then the tail, returns the tail starting at a
/// line boundary. If the byte before is a newline the tail is already aligned.
fn drop_leading_partial(window: &[u8]) -> &[u8] {
    let tail = window.get(1..).unwrap_or(&[]);
    if window.first() == Some(&b'\n') {
        tail
    } else {
        cut_partial_line(tail)
    }
}

/// The last `max` bytes of `data`, starting at a line boundary when cut. Data no longer than
/// `max` is returned whole.
fn tail_bytes(data: &[u8], max: usize) -> &[u8] {
    if data.len() <= max {
        data
    } else {
        // len > max, so `len - max - 1` cannot underflow.
        drop_leading_partial(&data[data.len() - max - 1..])
    }
}

/// Reads the tail of a file by seeking past the rest. `Ok(None)` when the file does not exist.
fn read_tail(path: &Path, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let max_u64 = max as u64;
    if len <= max_u64 {
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        return Ok(Some(buf));
    }
    // Seek one byte early so the byte before the tail decides whether the cut is aligned.
    file.seek(SeekFrom::Start(len - max_u64 - 1))?;
    let mut buf = Vec::with_capacity(max + 1);
    file.by_ref().take(max_u64 + 1).read_to_end(&mut buf)?;
    Ok(Some(tail_bytes(&buf, max).to_vec()))
}

/// Appends one file's tail under a heading. Read errors are written into the report instead.
fn push_file_section(out: &mut String, name: &str, read: io::Result<Option<Vec<u8>>>) {
    match read {
        Ok(Some(bytes)) => {
            out.push_str(&format!("--- {name} (last {} bytes) ---\n", bytes.len()));
            out.push_str(&String::from_utf8_lossy(&bytes));
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(None) => {}
        Err(e) => out.push_str(&format!("(could not read {name}: {e})\n")),
    }
}

/// The daemon-log part of the report: the rotation's tail first (if budget remains), then the
/// active file's tail. Both are read by seeking.
fn log_section(dir: &Path) -> String {
    let active = read_tail(&dir.join(LOG_FILE_NAME), LOG_BUDGET);
    let active_len = match &active {
        Ok(Some(bytes)) => bytes.len(),
        _ => 0,
    };
    let room = LOG_BUDGET - active_len;
    let rotated_name = format!("{LOG_FILE_NAME}.1");
    let rotated = if room > 0 {
        read_tail(&dir.join(&rotated_name), room)
    } else {
        Ok(None)
    };

    let found_any =
        matches!(active, Ok(Some(_)) | Err(_)) || matches!(rotated, Ok(Some(_)) | Err(_));
    if !found_any {
        return "(no log file found)\n".to_owned();
    }
    let mut out = String::new();
    push_file_section(&mut out, &rotated_name, rotated);
    push_file_section(&mut out, LOG_FILE_NAME, active);
    out
}

/// Builds the plain-text report: header, then the daemon log tail. Every Windows user name in
/// `user_names` is redacted from `C:\Users\<name>` paths.
fn build_report(log_dir: &Path, user_names: &[String], now_unix: u64) -> String {
    let header = format!(
        "ClipCheck diagnostics\n\
         UI version: {}\n\
         OS: {} ({})\n\
         Generated: {} ({})\n\
         Log folder: {}\n\n\
         == Daemon log (newest last) ==\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        now_unix,
        format_utc(now_unix),
        log_dir.display(),
    );
    let mut out = header;
    out.push_str(&log_section(log_dir));
    redact(&out, user_names)
}

/// Formats unix seconds as `YYYY-MM-DD HH:MM:SS UTC` (proleptic Gregorian, civil-from-days).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// Replaces `C:\Users\<name>` (case-insensitive, `\` or `/` separators) with `C:\Users\<user>`
/// for each name in `user_names`. The name must end at a path boundary, so `mitch` does not
/// match `mitchell`.
fn redact(text: &str, user_names: &[String]) -> String {
    if user_names.is_empty() {
        return text.to_owned();
    }
    let lower = text.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < lower_bytes.len() {
        match match_user_path(lower_bytes, i, user_names) {
            Some(end) => {
                out.push_str(&text[copied..i]);
                out.push_str(r"C:\Users\<user>");
                copied = end;
                i = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// If `lower[at..]` starts with `c:<sep>users<sep><name>` for a listed name, returns the index
/// just past that name. `lower` is ASCII-lowercased text, so byte offsets match the original.
fn match_user_path(lower: &[u8], at: usize, user_names: &[String]) -> Option<usize> {
    if lower.get(at) != Some(&b'c') || lower.get(at + 1) != Some(&b':') {
        return None;
    }
    let p = skip_sep(lower, at + 2)?;
    if !lower[p..].starts_with(b"users") {
        return None;
    }
    let p = skip_sep(lower, p + 5)?;
    user_names.iter().find_map(|name| {
        let wanted = name.to_ascii_lowercase();
        let end = p + wanted.len();
        let matches = end <= lower.len()
            && &lower[p..end] == wanted.as_bytes()
            && boundary_ok(lower.get(end));
        matches.then_some(end)
    })
}

/// Index after one `\` or `/` at `p`, or `None` if there is no separator there.
fn skip_sep(bytes: &[u8], p: usize) -> Option<usize> {
    match bytes.get(p) {
        Some(b'\\') | Some(b'/') => Some(p + 1),
        _ => None,
    }
}

/// True when the byte after a name cannot continue it (end, separator, space, punctuation).
fn boundary_ok(next: Option<&u8>) -> bool {
    match next {
        None => true,
        Some(&c) => !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "clipcheck-logs-test-{tag}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn tail_returns_short_data_whole() {
        assert_eq!(tail_bytes(b"abc\ndef\n", 100), b"abc\ndef\n");
        assert_eq!(tail_bytes(b"", 10), b"");
    }

    #[test]
    fn tail_cuts_at_line_boundary() {
        // Last 8 bytes are "e2\nline3": the partial "e2" line is dropped.
        let data = b"line1\nline2\nline3";
        assert_eq!(tail_bytes(data, 8), b"line3");
        // Cut lands right after a newline, so "bbbb" is a whole line and is kept.
        assert_eq!(tail_bytes(b"aaaa\nbbbb\n", 5), b"bbbb\n");
        // Cut lands mid-line with no newline in the window: the window is kept as is.
        assert_eq!(tail_bytes(b"abcdefgh", 4), b"efgh");
    }

    #[test]
    fn tail_keeps_single_long_line_and_zero_budget() {
        assert_eq!(tail_bytes(b"xxxxxxxxxx", 4), b"xxxx");
        assert_eq!(tail_bytes(b"a\nb\n", 0), b"");
    }

    #[test]
    fn redact_handles_case_and_both_separators() {
        let names = vec!["Mitch".to_owned()];
        let text = r"at C:\Users\Mitch\AppData and c:/users/MITCH/x and C:\Users\mitchell\y";
        assert_eq!(
            redact(text, &names),
            r"at C:\Users\<user>\AppData and C:\Users\<user>/x and C:\Users\mitchell\y"
        );
    }

    #[test]
    fn redact_without_names_is_identity() {
        let text = r"C:\Users\Mitch";
        assert_eq!(redact(text, &[]), text);
    }

    #[test]
    fn redact_keeps_non_ascii_text() {
        let names = vec!["jo".to_owned()];
        assert_eq!(
            redact("café C:\\Users\\jo ü", &names),
            "café C:\\Users\\<user> ü"
        );
    }

    #[test]
    fn utc_formatting_matches_known_values() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
    }

    #[test]
    fn log_dir_precedence() {
        assert_eq!(
            log_dir_from(Some("D:\\data"), Some("C:\\AppData")).unwrap(),
            PathBuf::from("D:\\data").join("logs")
        );
        assert_eq!(
            log_dir_from(Some("  "), Some("C:\\AppData")).unwrap(),
            PathBuf::from("C:\\AppData").join("ClipCheck").join("logs")
        );
        assert!(log_dir_from(None, Some("")).is_err());
    }

    #[test]
    fn report_without_log_files_says_so() {
        let dir = unique_dir("missing");
        let report = build_report(&dir, &[], 0);
        assert!(report.contains("(no log file found)"), "{report}");
        assert!(report.contains("UI version:"));
    }

    #[test]
    fn report_includes_tail_of_active_and_rotated_logs() {
        let dir = unique_dir("tail");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("clipcheckd.log.1"), "old line\n").unwrap();
        let big: String = (0..40_000).map(|i| format!("line {i}\n")).collect();
        fs::write(dir.join(LOG_FILE_NAME), &big).unwrap();
        let report = build_report(&dir, &["nobody".to_owned()], 0);
        assert!(report.contains("line 39999\n"));
        assert!(!report.contains("line 0\n"));
        assert!(
            !report.contains("old line"),
            "rotation should not fit the budget"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_redacts_user_name_in_log_folder_path() {
        let dir = unique_dir("redact");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(LOG_FILE_NAME), "hello from C:\\Users\\Zed\\x\n").unwrap();
        let fake = Path::new(r"C:\Users\Zed\AppData\Roaming\ClipCheck\logs");
        let report = build_report(fake, &["zed".to_owned()], 0);
        assert!(!report.contains("Zed"), "{report}");
        assert!(report.contains(r"C:\Users\<user>\AppData"));
        let _ = fs::remove_dir_all(&dir);
    }
}
