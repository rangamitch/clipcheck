//! Tests for `clipcheck_engine::procs`: root selection, process listing and the
//! app watcher's idle behaviour.

use std::time::Instant;

use clipcheck_engine::procs::{
    list_processes, root_pids_for_exe, AppWatcher, ProcInfo, SCAN_INTERVAL,
};

fn p(pid: u32, parent_pid: u32, exe: &str) -> ProcInfo {
    ProcInfo {
        pid,
        parent_pid,
        exe_name: exe.to_string(),
    }
}

#[test]
fn scan_interval_is_1500_ms() {
    assert_eq!(SCAN_INTERVAL.as_millis(), 1500);
}

#[test]
fn root_of_a_five_process_tree_is_one() {
    // 100 is the root. 101..=104 are descendants, each parented inside the set.
    let procs = vec![
        p(100, 1, "discord.exe"),
        p(101, 100, "discord.exe"),
        p(102, 100, "discord.exe"),
        p(103, 101, "discord.exe"),
        p(104, 103, "discord.exe"),
        p(500, 1, "explorer.exe"),
    ];
    assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![100]);
}

#[test]
fn two_separate_roots_are_both_returned_sorted() {
    let procs = vec![
        p(300, 1, "discord.exe"),
        p(200, 1, "discord.exe"),
        p(201, 200, "discord.exe"),
    ];
    assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![200, 300]);
}

#[test]
fn match_is_case_insensitive() {
    let procs = vec![p(10, 1, "Discord.EXE"), p(11, 10, "DISCORD.exe")];
    assert_eq!(root_pids_for_exe(&procs, "DiScOrD.EXE"), vec![10]);
    assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![10]);
}

#[test]
fn no_match_gives_empty() {
    let procs = vec![p(1, 0, "chrome.exe"), p(2, 1, "code.exe")];
    assert!(root_pids_for_exe(&procs, "discord.exe").is_empty());
    assert!(root_pids_for_exe(&[], "discord.exe").is_empty());
}

#[test]
fn other_exe_parent_does_not_hide_root() {
    // chrome.exe (pid 50) parents discord.exe (pid 60). Only matches count, so
    // 60 is still a root.
    let procs = vec![
        p(50, 1, "chrome.exe"),
        p(60, 50, "discord.exe"),
        p(61, 60, "discord.exe"),
    ];
    assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![60]);
}

#[test]
fn self_parented_process_is_a_root() {
    let procs = vec![p(70, 70, "discord.exe")];
    assert_eq!(root_pids_for_exe(&procs, "discord.exe"), vec![70]);
}

#[cfg(windows)]
#[test]
fn live_snapshot_contains_this_process() {
    let me = std::process::id();
    let procs = list_processes();
    assert!(!procs.is_empty(), "process snapshot is empty");
    let entry = procs
        .iter()
        .find(|p| p.pid == me)
        .unwrap_or_else(|| panic!("pid {me} not in snapshot of {} processes", procs.len()));
    assert!(!entry.exe_name.is_empty(), "exe_name empty for own pid");
    let exe_file = std::env::current_exe()
        .expect("current_exe")
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        entry.exe_name.eq_ignore_ascii_case(&exe_file),
        "exe_name {:?} for own pid {me} is not the test exe {exe_file:?}",
        entry.exe_name
    );
}

#[test]
fn watcher_for_missing_app_stays_empty() {
    let mut w = AppWatcher::new("definitely-not-running-clipcheck.exe");
    assert_eq!(w.exe(), "definitely-not-running-clipcheck.exe");
    let chunks = w.pump(Instant::now());
    assert!(chunks.is_empty(), "samples from an app that is not running");
    assert!(
        w.active_pids().is_empty(),
        "captures for an app that is not running"
    );
    w.stop();
    assert!(w.active_pids().is_empty());
}
