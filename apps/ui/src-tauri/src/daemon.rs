//! Recorder daemon glue for `ClipCheck.exe`.
//!
//! One executable plays two roles. `ClipCheck.exe --daemon [daemon args]` runs
//! the recorder (`clipcheckd`) in-process with no window. A normal launch opens
//! the UI and calls [`ensure_daemon_running`], which starts a detached copy of
//! this same exe with `--daemon` unless a daemon already answers on the pipe.
//! Closing the UI therefore leaves recording running (DESIGN.md D3).

use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_ipc::{default_pipe_name, PipeClient};

use crate::commands::DATA_DIR_ENV;

/// Argument that switches `ClipCheck.exe` into daemon mode.
pub const DAEMON_FLAG: &str = "--daemon";

/// Environment variable that overrides the daemon pipe name.
pub const PIPE_NAME_ENV: &str = "CLIPCHECK_PIPE_NAME";

/// Exit code `clipcheckd` uses when another daemon already holds the instance mutex.
const EXIT_ALREADY_RUNNING: i32 = clipcheckd::app::EXIT_ALREADY_RUNNING;

/// How long one probe waits for the pipe before concluding no daemon answers.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Total time allowed for a freshly spawned daemon to start answering on the pipe.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause between readiness probes after a spawn.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Outcome of [`ensure_daemon_running`] when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStart {
    /// A daemon was already answering on the pipe, or a spawned copy reported
    /// that another instance holds the mutex. Nothing was started.
    Already,
    /// A new detached daemon was spawned and is answering on the pipe.
    Spawned {
        /// Process id of the spawned daemon.
        pid: u32,
    },
}

/// Failures from [`ensure_daemon_running`].
#[derive(Debug)]
pub enum DaemonError {
    /// The pipe name could not be determined, or the process could not be spawned.
    Io(io::Error),
    /// The spawned daemon exited before it started answering.
    ExitedEarly {
        /// Exit code, or `None` when the process was terminated by a signal.
        code: Option<i32>,
    },
    /// The spawned daemon did not answer on the pipe within the start timeout.
    NotReady,
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::Io(e) => write!(f, "cannot start recorder daemon: {e}"),
            DaemonError::ExitedEarly { code: Some(code) } => {
                write!(f, "recorder daemon exited early with code {code}")
            }
            DaemonError::ExitedEarly { code: None } => {
                write!(f, "recorder daemon exited early")
            }
            DaemonError::NotReady => write!(f, "recorder daemon did not answer in time"),
        }
    }
}

impl From<io::Error> for DaemonError {
    fn from(e: io::Error) -> Self {
        DaemonError::Io(e)
    }
}

/// True when `args` (argv, program name first) contains the `--daemon` flag.
/// The program name itself is never treated as a flag.
pub fn is_daemon_mode(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg == DAEMON_FLAG)
}

/// Returns `args` without any `--daemon` flags. The program name is kept.
pub fn strip_daemon_flag(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(index, arg)| *index == 0 || arg.as_str() != DAEMON_FLAG)
        .map(|(_, arg)| arg.clone())
        .collect()
}

/// Runs the recorder in-process when `args` asks for daemon mode.
///
/// Returns `None` for a normal UI launch. In daemon mode returns the daemon's
/// exit code; the caller must exit with it before building the window.
pub fn maybe_run_daemon(args: Vec<String>) -> Option<i32> {
    if !is_daemon_mode(&args) {
        return None;
    }
    Some(clipcheckd::run_with_args(strip_daemon_flag(&args)))
}

/// Picks the pipe name every part of the UI uses: the explicit argument, then
/// `CLIPCHECK_PIPE_NAME`, then the per-user default. An empty value counts as unset.
///
/// This is the only place that reads the override, so the spawn path, the
/// connection manager and the CLI always agree on the pipe.
pub fn resolve_pipe_name(explicit: Option<&str>) -> io::Result<String> {
    let env = std::env::var(PIPE_NAME_ENV).ok();
    pick_pipe_name(explicit, env.as_deref(), default_pipe_name)
}

/// Pure form of [`resolve_pipe_name`]: the environment value and the default are passed in.
fn pick_pipe_name(
    explicit: Option<&str>,
    env: Option<&str>,
    default: impl FnOnce() -> io::Result<String>,
) -> io::Result<String> {
    match explicit
        .filter(|s| !s.is_empty())
        .or_else(|| env.filter(|s| !s.is_empty()))
    {
        Some(name) => Ok(name.to_owned()),
        None => default(),
    }
}

/// Makes sure a recorder daemon answers on the pipe, spawning one if needed.
///
/// `pipe_name` overrides the pipe (see [`resolve_pipe_name`]). The spawned copy is
/// this exe with `--daemon`, forwarding `--data-dir` (from `CLIPCHECK_DATA_DIR`) and
/// the resolved `--pipe-name`. Blocks for up to [`START_TIMEOUT`], so call it off the UI thread.
pub fn ensure_daemon_running(pipe_name: Option<&str>) -> Result<DaemonStart, DaemonError> {
    let exe = std::env::current_exe()?;
    let data_dir = std::env::var(DATA_DIR_ENV).ok();
    ensure_daemon_running_from(&exe, data_dir.as_deref(), pipe_name)
}

/// Same as [`ensure_daemon_running`] with the daemon executable and data dir passed
/// in explicitly. Used by tests that cannot rely on `current_exe` being the UI binary.
pub fn ensure_daemon_running_from(
    exe: &Path,
    data_dir: Option<&str>,
    pipe_name: Option<&str>,
) -> Result<DaemonStart, DaemonError> {
    let name = resolve_pipe_name(pipe_name)?;
    if probe(&name, PROBE_TIMEOUT) {
        return Ok(DaemonStart::Already);
    }

    let mut cmd = Command::new(exe);
    // The resolved name is forwarded, so the child listens where this process probes.
    cmd.args(daemon_args(data_dir, Some(&name)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    let mut child = cmd.spawn()?;
    let pid = child.id();

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return match status.code() {
                Some(EXIT_ALREADY_RUNNING) => Ok(DaemonStart::Already),
                code => Err(DaemonError::ExitedEarly { code }),
            };
        }
        if probe(&name, PROBE_TIMEOUT) {
            return Ok(DaemonStart::Spawned { pid });
        }
        if Instant::now() >= deadline {
            return Err(DaemonError::NotReady);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Builds the `--daemon` argument list. The program name is not included.
/// Only the values that are set are forwarded.
pub fn daemon_args(data_dir: Option<&str>, pipe_name: Option<&str>) -> Vec<String> {
    let mut args = vec![DAEMON_FLAG.to_owned()];
    if let Some(dir) = data_dir {
        args.push("--data-dir".to_owned());
        args.push(dir.to_owned());
    }
    if let Some(pipe) = pipe_name {
        args.push("--pipe-name".to_owned());
        args.push(pipe.to_owned());
    }
    args
}

/// Returns true when a client connects to `name` within `timeout`.
fn probe(name: &str, timeout: Duration) -> bool {
    PipeClient::connect(name, timeout).is_ok()
}

/// Makes the child outlive this process: no visible console, its own process group
/// (so this process's Ctrl+C does not reach it), and no window.
///
/// `CREATE_NO_WINDOW` must be used alone. The Win32 docs say it is ignored when combined
/// with `DETACHED_PROCESS`, and `DETACHED_PROCESS` on a console-subsystem exe (debug builds)
/// left a console that was not hidden. This is the flag that stops a console from appearing.
#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(clipcheck_util::proc::CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(windows))]
fn detach(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_flag_after_program_name() {
        assert!(is_daemon_mode(&argv(&["ClipCheck.exe", "--daemon"])));
        assert!(is_daemon_mode(&argv(&[
            "ClipCheck.exe",
            "--data-dir",
            "x",
            "--daemon"
        ])));
        assert!(!is_daemon_mode(&argv(&["ClipCheck.exe"])));
        assert!(!is_daemon_mode(&argv(&["--daemon"])));
    }

    #[test]
    fn strips_flag_and_keeps_program_name() {
        assert_eq!(
            strip_daemon_flag(&argv(&["a.exe", "--daemon", "--pipe-name", "p"])),
            argv(&["a.exe", "--pipe-name", "p"])
        );
        assert_eq!(strip_daemon_flag(&argv(&["--daemon"])), argv(&["--daemon"]));
    }

    #[test]
    fn empty_pipe_values_count_as_unset() {
        let default = || Ok("default".to_owned());
        assert_eq!(
            pick_pipe_name(Some(""), Some(""), default).unwrap(),
            "default"
        );
        assert_eq!(pick_pipe_name(None, Some(""), default).unwrap(), "default");
        assert_eq!(
            pick_pipe_name(Some(""), Some("env"), default).unwrap(),
            "env"
        );
        assert_eq!(
            pick_pipe_name(Some("x"), Some("env"), default).unwrap(),
            "x"
        );
        assert_eq!(pick_pipe_name(None, Some("env"), default).unwrap(), "env");
    }

    #[test]
    fn daemon_args_forward_only_set_values() {
        assert_eq!(daemon_args(None, None), argv(&["--daemon"]));
        assert_eq!(
            daemon_args(Some("d"), Some("p")),
            argv(&["--daemon", "--data-dir", "d", "--pipe-name", "p"])
        );
    }
}
