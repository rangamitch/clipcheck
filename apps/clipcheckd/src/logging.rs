//! Minimal std-only file logger for the daemon.
//!
//! Lines go to `<data-dir>/logs/clipcheckd.log`. When the file would grow past
//! [`MAX_LOG_BYTES`], it is rotated to `clipcheckd.log.1` and older rotations
//! shift up, keeping [`KEEP_ROTATED`] files. Logging never panics; write errors
//! fall back to stderr.
//!
//! Lines below the minimum level are dropped before any formatting. The minimum
//! is [`DEFAULT_LEVEL`] (Info) unless the `CLIPCHECK_LOG` environment variable
//! names another level. Writes are buffered: warning and error lines are flushed
//! at once, other lines reach the file within [`FLUSH_INTERVAL`], and [`flush`]
//! forces it. A process that exits without calling [`flush`] can lose the last
//! second of info and debug lines.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Size at which the active log file is rotated.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Number of rotated files kept next to the active one.
pub const KEEP_ROTATED: u32 = 3;

/// Name of the active log file inside the logs directory.
pub const LOG_FILE_NAME: &str = "clipcheckd.log";

/// Environment variable that sets the minimum level (`debug`, `info`, `warn`, `error`).
pub const LOG_ENV: &str = "CLIPCHECK_LOG";

/// Minimum level when [`LOG_ENV`] is unset.
pub const DEFAULT_LEVEL: Level = Level::Info;

/// Longest time a buffered info or debug line waits before it reaches the file.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Severity of a log line. Declaration order runs from most to least severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Something failed and the operation was abandoned.
    Error,
    /// Something is wrong but the daemon keeps running.
    Warn,
    /// Normal lifecycle events.
    Info,
    /// Detailed diagnostics for support sessions.
    Debug,
}

impl Level {
    /// Fixed-width label used in the log line.
    pub fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    fn from_index(index: u8) -> Level {
        match index {
            0 => Level::Error,
            1 => Level::Warn,
            2 => Level::Info,
            _ => Level::Debug,
        }
    }
}

/// Parses a level name for [`LOG_ENV`]. Case and surrounding spaces are ignored.
/// Returns `None` for any other text.
pub fn parse_level(text: &str) -> Option<Level> {
    match text.trim().to_ascii_lowercase().as_str() {
        "error" => Some(Level::Error),
        "warn" | "warning" => Some(Level::Warn),
        "info" => Some(Level::Info),
        "debug" => Some(Level::Debug),
        _ => None,
    }
}

/// True when a line at `level` is written while the minimum level is `min`.
pub fn is_enabled(level: Level, min: Level) -> bool {
    level <= min
}

static MIN_LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL as u8);

/// Current minimum level. Lines less severe than this are dropped.
pub fn min_level() -> Level {
    Level::from_index(MIN_LEVEL.load(Ordering::Relaxed))
}

/// Sets the minimum level for all later lines.
pub fn set_min_level(level: Level) {
    MIN_LEVEL.store(level as u8, Ordering::Relaxed);
}

struct Logger {
    dir: PathBuf,
    file: Option<BufWriter<File>>,
    size: u64,
}

static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Logger>> {
    LOGGER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Opens (creating if needed) the log file inside `log_dir` and makes it the
/// destination for [`log`]. Calling it again replaces the previous destination.
/// Applies [`LOG_ENV`] when it is set and starts the periodic flush thread once.
pub fn init(log_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(log_dir)?;
    let file = open_append(&log_dir.join(LOG_FILE_NAME))?;
    let size = file.metadata()?.len();
    *lock() = Some(Logger {
        dir: log_dir.to_path_buf(),
        file: Some(BufWriter::new(file)),
        size,
    });
    apply_env_level();
    start_flush_thread();
    Ok(())
}

/// Reads [`LOG_ENV`]. An unrecognised value is reported on stderr and ignored.
fn apply_env_level() {
    if let Ok(value) = std::env::var(LOG_ENV) {
        match parse_level(&value) {
            Some(level) => set_min_level(level),
            None => eprintln!(
                "clipcheckd: ignoring {LOG_ENV}={value}: expected debug, info, warn or error"
            ),
        }
    }
}

/// Starts the thread that flushes buffered lines every [`FLUSH_INTERVAL`].
fn start_flush_thread() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = thread::Builder::new()
            .name("log-flush".to_owned())
            .spawn(|| loop {
                thread::sleep(FLUSH_INTERVAL);
                flush();
            });
        if let Err(e) = spawned {
            eprintln!("clipcheckd: log flush thread not started: {e}");
        }
    });
}

/// Writes any buffered lines to the log file now.
pub fn flush() {
    let mut guard = lock();
    if let Some(file) = guard.as_mut().and_then(|logger| logger.file.as_mut()) {
        if let Err(e) = file.flush() {
            eprintln!("clipcheckd: log flush failed: {e}");
        }
    }
}

/// Writes one line at `level`, if it is at or above the minimum level. Falls
/// back to stderr if logging is not initialised.
pub fn log(level: Level, msg: &str) {
    if !is_enabled(level, min_level()) {
        return;
    }
    let line = format!(
        "{} {:<5} pid={} {}\n",
        timestamp_utc(),
        level.label(),
        process::id(),
        msg
    );
    let mut guard = lock();
    match guard.as_mut() {
        Some(logger) => {
            if let Err(e) = logger.write_line(level, &line) {
                eprintln!("clipcheckd: log write failed: {e}");
            }
        }
        None => eprint!("{line}"),
    }
}

/// Logs at [`Level::Error`].
pub fn error(msg: &str) {
    log(Level::Error, msg);
}

/// Logs at [`Level::Warn`].
pub fn warn(msg: &str) {
    log(Level::Warn, msg);
}

/// Logs at [`Level::Info`].
pub fn info(msg: &str) {
    log(Level::Info, msg);
}

/// Logs at [`Level::Debug`].
pub fn debug(msg: &str) {
    log(Level::Debug, msg);
}

impl Logger {
    /// Appends `line`, rotating first when it would pass [`MAX_LOG_BYTES`].
    /// Error and warning lines are flushed to the file before returning.
    fn write_line(&mut self, level: Level, line: &str) -> io::Result<()> {
        let incoming = line.len() as u64;
        if self.size > 0 && self.size + incoming > MAX_LOG_BYTES {
            self.rotate()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is not open"))?;
        file.write_all(line.as_bytes())?;
        self.size += incoming;
        if level <= Level::Warn {
            file.flush()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        // Flush and close the active file first so Windows allows renaming it.
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        let active = self.dir.join(LOG_FILE_NAME);
        let rotated = |n: u32| self.dir.join(format!("{LOG_FILE_NAME}.{n}"));

        let oldest = rotated(KEEP_ROTATED);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for n in (1..KEEP_ROTATED).rev() {
            let from = rotated(n);
            if from.exists() {
                fs::rename(&from, rotated(n + 1))?;
            }
        }
        if active.exists() {
            fs::rename(&active, rotated(1))?;
        }
        self.file = Some(BufWriter::new(open_append(&active)?));
        self.size = 0;
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// Current time as `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC), without external crates.
fn timestamp_utc() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Converts days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn env_value_table() {
        let cases: [(&str, Option<Level>); 9] = [
            ("debug", Some(Level::Debug)),
            ("INFO", Some(Level::Info)),
            (" warn ", Some(Level::Warn)),
            ("Warning", Some(Level::Warn)),
            ("error", Some(Level::Error)),
            ("", None),
            ("verbose", None),
            ("3", None),
            ("debugger", None),
        ];
        for (value, expected) in cases {
            assert_eq!(parse_level(value), expected, "value {value:?}");
        }
    }

    #[test]
    fn default_level_drops_debug_only() {
        assert_eq!(DEFAULT_LEVEL, Level::Info);
        assert!(is_enabled(Level::Error, DEFAULT_LEVEL));
        assert!(is_enabled(Level::Warn, DEFAULT_LEVEL));
        assert!(is_enabled(Level::Info, DEFAULT_LEVEL));
        assert!(!is_enabled(Level::Debug, DEFAULT_LEVEL));
        assert!(is_enabled(Level::Debug, Level::Debug));
        assert!(!is_enabled(Level::Info, Level::Error));
    }

    #[test]
    fn error_line_is_flushed_and_info_stays_buffered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(LOG_FILE_NAME);
        let mut logger = Logger {
            dir: dir.path().to_path_buf(),
            file: Some(BufWriter::new(open_append(&path).expect("open"))),
            size: 0,
        };
        logger.write_line(Level::Info, "info line\n").expect("info");
        assert_eq!(fs::read_to_string(&path).expect("read"), "");
        logger
            .write_line(Level::Error, "error line\n")
            .expect("error");
        let on_disk = fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, "info line\nerror line\n");
    }
}
