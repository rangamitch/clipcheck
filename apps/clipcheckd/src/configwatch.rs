//! Watches `config.toml` on disk so hand edits take effect without a restart.
//!
//! The watcher polls the file's modified time and length. When the signature
//! changes and then stays the same for one full poll (so an editor that writes
//! in several steps is read once), it asks the service to reload. The service
//! ignores a file whose parsed config equals the one in effect, so writes made
//! by the Settings UI cause no extra work. A file that fails validation is not
//! re-read until it changes again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipcheck_ipc::proto::ErrorCode;

use crate::logging;
use crate::service::{ConfigOutcome, Service};

/// How often the file signature is checked.
const POLL: Duration = Duration::from_millis(300);
/// Step used while sleeping between polls, so a stop request is seen quickly.
const STEP: Duration = Duration::from_millis(50);

/// Modified time and length of the config file. `None` when the file is missing.
type Signature = Option<(SystemTime, u64)>;

/// What the watcher does after one poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Nothing to do.
    Nothing,
    /// The file changed; wait one more poll to see whether it is stable.
    Wait,
    /// The file changed and is stable: ask the service to reload.
    Reload,
}

/// Decides the next step from the signature applied last, the signature seen
/// on the previous poll, and the signature seen now.
fn next_action(applied: Signature, previous: Signature, now: Signature) -> Action {
    if now == applied {
        Action::Nothing
    } else if now.is_none() {
        // The file was removed: keep the current settings and do not reload.
        Action::Nothing
    } else if now != previous {
        Action::Wait
    } else {
        Action::Reload
    }
}

/// Reads the signature of `path`. A missing or unreadable file yields `None`.
fn read_signature(path: &Path) -> Signature {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
    Some((modified, meta.len()))
}

/// Background watcher for `config.toml`. Dropping it, or calling [`stop`](Self::stop),
/// ends the thread within one poll.
pub struct ConfigWatcher {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ConfigWatcher {
    /// Polls `path` metadata (modified time + length) every 300 ms on a thread
    /// named "config-watch". Reloads go through `service`.
    ///
    /// Returns an error only when the thread cannot be spawned.
    pub fn start(path: PathBuf, service: Service) -> io::Result<ConfigWatcher> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("config-watch".to_owned())
            .spawn(move || watch_loop(&path, &service, &thread_stop))?;
        Ok(ConfigWatcher {
            stop,
            join: Some(join),
        })
    }

    /// Stops the watcher and waits for its thread to exit. The wait is at most
    /// one poll unless a reload is already in progress.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // Only signal here: a drop must not block. stop() has already joined
        // when it ran, which leaves `join` empty.
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Body of the watcher thread.
fn watch_loop(path: &Path, service: &Service, stop: &AtomicBool) {
    let mut applied = read_signature(path);
    let mut previous = applied;
    loop {
        if !sleep_unless_stopped(stop) {
            return;
        }
        let now = read_signature(path);
        match next_action(applied, previous, now) {
            Action::Nothing | Action::Wait => {
                if now.is_none() {
                    // Record the removal so a later re-creation is seen as a change.
                    applied = None;
                }
            }
            Action::Reload => {
                match service.reload_config() {
                    Ok(ConfigOutcome::Rejected(_)) => logging::warn(
                        "config.toml changed on disk but has errors; keeping the current settings",
                    ),
                    Ok(ConfigOutcome::Applied { .. }) => {
                        logging::info("config.toml changed on disk; new settings applied");
                    }
                    Ok(ConfigOutcome::Checked { .. }) => {}
                    Err(e) if matches!(e.code, ErrorCode::ShuttingDown) => return,
                    Err(e) => logging::warn(&format!("config reload failed: {}", e.message)),
                }
                // Applied or not, this signature is done: a rejected file is not re-read.
                applied = now;
            }
        }
        previous = now;
    }
}

/// Sleeps one poll in small steps. Returns false when a stop was requested.
fn sleep_unless_stopped(stop: &AtomicBool) -> bool {
    let steps = POLL.as_millis() / STEP.as_millis();
    for _ in 0..steps {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(STEP);
    }
    !stop.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(secs: u64, len: u64) -> Signature {
        Some((UNIX_EPOCH + Duration::from_secs(secs), len))
    }

    #[test]
    fn unchanged_file_does_nothing() {
        assert_eq!(
            next_action(sig(1, 10), sig(1, 10), sig(1, 10)),
            Action::Nothing
        );
    }

    #[test]
    fn first_change_waits_for_stability() {
        assert_eq!(
            next_action(sig(1, 10), sig(1, 10), sig(2, 12)),
            Action::Wait
        );
    }

    #[test]
    fn stable_change_reloads() {
        assert_eq!(
            next_action(sig(1, 10), sig(2, 12), sig(2, 12)),
            Action::Reload
        );
    }

    #[test]
    fn removed_file_does_nothing() {
        assert_eq!(next_action(sig(1, 10), sig(1, 10), None), Action::Nothing);
        assert_eq!(next_action(sig(1, 10), None, None), Action::Nothing);
    }
}
