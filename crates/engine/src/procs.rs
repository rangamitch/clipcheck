//! Process listing and per-application audio capture (task A3).
//!
//! [`list_processes`] takes a snapshot of the running processes. The pure
//! [`root_pids_for_exe`] reduces the matches of one executable to its roots, so
//! that a multi-process app such as Discord yields one process-loopback capture
//! per root. [`AppWatcher`] rescans every [`SCAN_INTERVAL`] and starts or stops
//! captures to follow the roots.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use clipcheck_audio::AudioChunk;
#[cfg(windows)]
use clipcheck_audio::{LoopbackMode, ProcessLoopback};

/// How often an [`AppWatcher`] rescans the process list.
pub const SCAN_INTERVAL: Duration = Duration::from_millis(1500);

/// Locks `m`, ignoring poisoning. The protected state stays usable after a
/// panic elsewhere.
pub(crate) use clipcheck_util::sync::lock_unpoisoned as lock;

/// One running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    /// Process ID.
    pub pid: u32,
    /// Parent process ID. May name a process that has already exited.
    pub parent_pid: u32,
    /// Executable file name, for example `discord.exe`.
    pub exe_name: String,
}

/// Snapshot of every running process. Empty when the snapshot fails or on a
/// platform without Toolhelp.
#[cfg(windows)]
pub fn list_processes() -> Vec<ProcInfo> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    // SAFETY: the snapshot handle is closed on every path below. The entry is
    // zero-initialised and its `dwSize` is set before the first call, as the
    // Toolhelp API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name_len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                out.push(ProcInfo {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    exe_name: String::from_utf16_lossy(&entry.szExeFile[..name_len]),
                });
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// Snapshot of every running process. Always empty off Windows.
#[cfg(not(windows))]
pub fn list_processes() -> Vec<ProcInfo> {
    Vec::new()
}

/// Returns the root process IDs among the processes named `exe`.
///
/// `exe` is compared case-insensitively against the executable file name. A
/// match is a root when its parent is not itself a match. A tree of matching
/// processes therefore yields one root. The result is sorted and unique.
pub fn root_pids_for_exe(procs: &[ProcInfo], exe: &str) -> Vec<u32> {
    let matches: Vec<&ProcInfo> = procs
        .iter()
        .filter(|p| p.exe_name.eq_ignore_ascii_case(exe))
        .collect();
    let match_ids: HashSet<u32> = matches.iter().map(|p| p.pid).collect();
    let mut roots: Vec<u32> = matches
        .iter()
        .filter(|p| !(p.parent_pid != p.pid && match_ids.contains(&p.parent_pid)))
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// Works out which captures to start and which to stop.
///
/// Returns `(to_start, to_stop)`. Both are the PIDs that differ between
/// `current` and `wanted`, in the order of `wanted` and `current`.
pub fn plan_scan(current: &[u32], wanted: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let cur: HashSet<u32> = current.iter().copied().collect();
    let want: HashSet<u32> = wanted.iter().copied().collect();
    let to_start = wanted
        .iter()
        .copied()
        .filter(|p| !cur.contains(p))
        .collect();
    let to_stop = current
        .iter()
        .copied()
        .filter(|p| !want.contains(p))
        .collect();
    (to_start, to_stop)
}

/// One process-loopback capture of a root process.
struct Capture {
    pid: u32,
    rx: Receiver<AudioChunk>,
    #[cfg(windows)]
    loopback: ProcessLoopback,
}

impl Capture {
    fn stop(self) {
        #[cfg(windows)]
        {
            let mut loopback = self.loopback;
            loopback.stop();
        }
    }
}

/// Starts a process-loopback capture of `pid` and its children.
#[cfg(windows)]
fn open_capture(pid: u32) -> Option<Capture> {
    ProcessLoopback::start(pid, LoopbackMode::IncludeTree)
        .ok()
        .map(|(loopback, rx)| Capture { pid, rx, loopback })
}

/// Process loopback is Windows-only, so no capture starts elsewhere.
#[cfg(not(windows))]
fn open_capture(_pid: u32) -> Option<Capture> {
    None
}

/// Follows the running processes named `exe` and collects their audio.
///
/// Call [`AppWatcher::pump`] from the mixing loop. Each call drains the
/// captures and, once every [`SCAN_INTERVAL`], rescans the process list. An
/// app that is not running has no captures and produces no samples. That is
/// not a warning. Dropping the watcher stops every capture.
pub struct AppWatcher {
    exe: String,
    captures: Vec<Capture>,
    last_scan: Option<Instant>,
}

impl AppWatcher {
    /// Creates a watcher for the executable file name `exe`. Nothing starts
    /// until the first [`AppWatcher::pump`].
    pub fn new(exe: impl Into<String>) -> Self {
        Self {
            exe: exe.into(),
            captures: Vec::new(),
            last_scan: None,
        }
    }

    /// The executable file name this watcher follows.
    pub fn exe(&self) -> &str {
        &self.exe
    }

    /// PIDs with a live capture, in start order.
    pub fn active_pids(&self) -> Vec<u32> {
        self.captures.iter().map(|c| c.pid).collect()
    }

    /// Rescans if [`SCAN_INTERVAL`] has passed since the last scan, then
    /// returns the samples captured since the previous call, one `Vec` per
    /// chunk.
    pub fn pump(&mut self, now: Instant) -> Vec<Vec<f32>> {
        let due = self
            .last_scan
            .is_none_or(|t| now.saturating_duration_since(t) >= SCAN_INTERVAL);
        if due {
            self.last_scan = Some(now);
            self.rescan();
        }
        let mut out = Vec::new();
        let mut finished = Vec::new();
        for (i, capture) in self.captures.iter().enumerate() {
            loop {
                match capture.rx.try_recv() {
                    Ok(chunk) => out.push(chunk.samples),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished.push(i);
                        break;
                    }
                }
            }
        }
        // Captures whose process exited are dropped here. A later scan restarts
        // them if the process is still a root.
        for i in finished.into_iter().rev() {
            self.captures.remove(i).stop();
        }
        out
    }

    /// Stops every capture. The watcher stays usable and rescans on the next
    /// [`AppWatcher::pump`].
    pub fn stop(&mut self) {
        for capture in self.captures.drain(..) {
            capture.stop();
        }
        self.last_scan = None;
    }

    fn rescan(&mut self) {
        let procs = list_processes();
        let wanted = root_pids_for_exe(&procs, &self.exe);
        let current = self.active_pids();
        let (to_start, to_stop) = plan_scan(&current, &wanted);
        for pid in to_stop {
            if let Some(i) = self.captures.iter().position(|c| c.pid == pid) {
                self.captures.remove(i).stop();
            }
        }
        for pid in to_start {
            if let Some(capture) = open_capture(pid) {
                self.captures.push(capture);
            }
        }
    }
}

impl Drop for AppWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent_pid: u32, exe: &str) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid,
            exe_name: exe.to_string(),
        }
    }

    #[test]
    fn tree_of_matches_has_one_root() {
        let procs = vec![
            p(100, 1, "discord.exe"),
            p(101, 100, "discord.exe"),
            p(102, 100, "discord.exe"),
            p(103, 101, "discord.exe"),
            p(104, 103, "discord.exe"),
        ];
        assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![100]);
    }

    #[test]
    fn separate_roots_are_all_returned() {
        let procs = vec![
            p(10, 1, "app.exe"),
            p(20, 2, "app.exe"),
            p(30, 20, "app.exe"),
        ];
        assert_eq!(root_pids_for_exe(&procs, "app.exe"), vec![10, 20]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let procs = vec![p(7, 1, "Discord.EXE"), p(8, 7, "DISCORD.exe")];
        assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![7]);
    }

    #[test]
    fn no_match_gives_no_roots() {
        let procs = vec![p(1, 0, "chrome.exe")];
        assert!(root_pids_for_exe(&procs, "discord.exe").is_empty());
        assert!(root_pids_for_exe(&[], "discord.exe").is_empty());
    }

    #[test]
    fn plan_scan_diffs_both_ways() {
        let (start, stop) = plan_scan(&[1, 2], &[2, 3]);
        assert_eq!(start, vec![3]);
        assert_eq!(stop, vec![1]);
    }

    #[test]
    fn watcher_without_app_is_silent() {
        let mut w = AppWatcher::new("definitely-not-running-clipcheck.exe");
        assert!(w.pump(Instant::now()).is_empty());
        assert!(w.active_pids().is_empty());
    }
}
