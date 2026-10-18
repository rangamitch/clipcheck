//! "Start with Windows": the per-user Run entry that launches the UI at sign-in.
//!
//! The entry lives in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` under the value
//! name [`VALUE_NAME`] (the product name, so the NSIS uninstaller removes it). Writes go
//! through `reg.exe` so no registry crate is needed. Every registry call goes through the
//! [`RegRunner`] trait; tests use a fake runner and never touch the real registry.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::logging;
use crate::service::Service;

/// Per-user Run key.
pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name under [`RUN_KEY`]. Matches the NSIS installer's uninstall cleanup.
pub const VALUE_NAME: &str = "ClipCheck";
/// File name of the UI executable that the Run entry starts.
pub const UI_EXE_NAME: &str = "ClipCheck.exe";

/// Last value this process reconciled: 0 unknown, 1 disabled, 2 enabled.
static LAST_APPLIED: AtomicU8 = AtomicU8::new(0);

/// Runs `reg.exe` and reports whether it succeeded, plus its combined output.
pub trait RegRunner {
    /// Runs `reg` with `args`. Returns `(success, output)`.
    fn exec(&self, args: &[String]) -> io::Result<(bool, String)>;
}

/// The real `reg.exe`, started without a console window.
pub struct SystemReg;

impl RegRunner for SystemReg {
    fn exec(&self, args: &[String]) -> io::Result<(bool, String)> {
        let mut cmd = Command::new("reg.exe");
        cmd.args(args);
        clipcheck_util::proc::no_window(&mut cmd);
        let out = cmd.output()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.success(), text))
    }
}

/// The Run value data: the quoted UI exe path, e.g. `"C:\Program Files\ClipCheck\ClipCheck.exe"`.
pub fn run_command(ui_exe: &Path) -> String {
    format!("\"{}\"", ui_exe.display())
}

/// Arguments for `reg add` that write the Run value.
pub fn add_args(command: &str) -> Vec<String> {
    [
        "add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d", command, "/f",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// Arguments for `reg delete` that remove the Run value.
pub fn delete_args() -> Vec<String> {
    ["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Arguments for `reg query` that look up the Run value.
pub fn query_args() -> Vec<String> {
    ["query", RUN_KEY, "/v", VALUE_NAME]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Writes (`enabled`) or removes the Run value through `runner`. Removing a value that is
/// not there is success.
pub fn apply_with<R: RegRunner>(runner: &R, enabled: bool, ui_exe: &Path) -> io::Result<()> {
    if enabled {
        let command = run_command(ui_exe);
        let (ok, text) = runner.exec(&add_args(&command))?;
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(format!("reg add failed: {}", text.trim())))
        }
    } else {
        if !is_registered_with(runner) {
            return Ok(());
        }
        let (ok, text) = runner.exec(&delete_args())?;
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "reg delete failed: {}",
                text.trim()
            )))
        }
    }
}

/// True when the Run value exists, whatever its data.
pub fn is_registered_with<R: RegRunner>(runner: &R) -> bool {
    matches!(runner.exec(&query_args()), Ok((true, _)))
}

/// Writes or removes the Run value with the real `reg.exe`.
pub fn apply(enabled: bool, ui_exe: &Path) -> io::Result<()> {
    apply_with(&SystemReg, enabled, ui_exe)
}

/// True when the Run value exists, checked with the real `reg.exe`. `get_status` is polled, so the
/// answer is cached for a few seconds instead of spawning `reg.exe` on every call.
pub fn is_registered() -> bool {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, bool)>> =
        std::sync::Mutex::new(None);
    const TTL: std::time::Duration = std::time::Duration::from_secs(5);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, value)) = *cache {
        if at.elapsed() < TTL {
            return value;
        }
    }
    let value = is_registered_with(&SystemReg);
    *cache = Some((std::time::Instant::now(), value));
    value
}

/// The UI exe expected next to this process (`ClipCheck.exe` beside `clipcheckd.exe`, or
/// beside the running `ClipCheck.exe` when the daemon runs inside it). `None` if it is missing.
pub fn ui_exe_path() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = dir.join(UI_EXE_NAME);
    candidate.is_file().then_some(candidate)
}

/// Brings the Run value in line with `desired`. Never fails: problems are logged.
/// Enabling without a UI exe next to the daemon (dev builds) is skipped with a warning.
pub fn reconcile(desired: bool) {
    let result = if desired {
        match ui_exe_path() {
            Some(exe) => apply(true, &exe),
            None => {
                logging::warn(&format!(
                    "start with Windows: {UI_EXE_NAME} not found next to the daemon; not registered"
                ));
                LAST_APPLIED.store(2, Ordering::SeqCst);
                return;
            }
        }
    } else {
        apply(false, Path::new(""))
    };
    match result {
        Ok(()) => logging::info(&format!(
            "start with Windows {}",
            if desired { "registered" } else { "removed" }
        )),
        Err(e) => logging::warn(&format!("start with Windows: cannot update Run entry: {e}")),
    }
    LAST_APPLIED.store(if desired { 2 } else { 1 }, Ordering::SeqCst);
}

/// Called after a config reload. Reconciles only when `general.autostart` differs from the
/// value this process last applied.
pub fn sync_from_service(service: &Service) {
    let desired = match service.config() {
        Ok(view) => view
            .config
            .get("general")
            .and_then(|g| g.get("autostart"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        Err(_) => return,
    };
    let code = if desired { 2 } else { 1 };
    if LAST_APPLIED.load(Ordering::SeqCst) != code {
        reconcile(desired);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records every call and answers from a canned list of results.
    struct FakeReg {
        calls: RefCell<Vec<Vec<String>>>,
        /// Result for `query`: whether the value exists.
        exists: bool,
        /// Whether add and delete succeed.
        writes_ok: bool,
    }

    impl RegRunner for FakeReg {
        fn exec(&self, args: &[String]) -> io::Result<(bool, String)> {
            self.calls.borrow_mut().push(args.to_vec());
            Ok(match args[0].as_str() {
                "query" => (self.exists, String::new()),
                _ => (self.writes_ok, "ERROR".to_owned()),
            })
        }
    }

    fn fake(exists: bool, writes_ok: bool) -> FakeReg {
        FakeReg {
            calls: RefCell::new(Vec::new()),
            exists,
            writes_ok,
        }
    }

    #[test]
    fn run_command_quotes_the_path() {
        let cmd = run_command(Path::new(r"C:\Program Files\ClipCheck\ClipCheck.exe"));
        assert_eq!(cmd, r#""C:\Program Files\ClipCheck\ClipCheck.exe""#);
    }

    #[test]
    fn add_args_write_the_run_value() {
        let args = add_args("\"X.exe\"");
        assert_eq!(args[0], "add");
        assert!(args.contains(&RUN_KEY.to_owned()));
        assert!(args.windows(2).any(|w| w == ["/v", "ClipCheck"]));
        assert!(args.windows(2).any(|w| w == ["/t", "REG_SZ"]));
        assert!(args.windows(2).any(|w| w == ["/d", "\"X.exe\""]));
        assert_eq!(args.last().map(String::as_str), Some("/f"));
    }

    #[test]
    fn enable_writes_when_exe_given() {
        let reg = fake(false, true);
        apply_with(&reg, true, Path::new(r"C:\a\ClipCheck.exe")).unwrap();
        let calls = reg.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], "add");
    }

    #[test]
    fn disable_skips_delete_when_absent() {
        let reg = fake(false, true);
        apply_with(&reg, false, Path::new("")).unwrap();
        let calls = reg.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], "query");
    }

    #[test]
    fn disable_deletes_when_present() {
        let reg = fake(true, true);
        apply_with(&reg, false, Path::new("")).unwrap();
        let calls = reg.calls.borrow();
        assert_eq!(calls[1][0], "delete");
    }

    #[test]
    fn failed_write_is_an_error() {
        let reg = fake(false, false);
        assert!(apply_with(&reg, true, Path::new(r"C:\a\ClipCheck.exe")).is_err());
    }
}
