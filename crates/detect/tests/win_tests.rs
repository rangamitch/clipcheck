#![cfg(windows)]
//! Smoke tests for the Windows snapshot provider (`clipcheck_detect::win`).
//!
//! Tests that call `enumerate_candidates()` run against the live desktop session
//! and need at least one top-level window, so they are `#[ignore]`d and do not
//! run in headless CI. Run them on an interactive desktop with:
//!
//! `cargo test -p clipcheck-detect --test win_tests -- --ignored`
//!
//! They assert invariants rather than specific windows. Add `--nocapture` to see
//! the SKIP notices.

use clipcheck_detect::win::{enumerate_candidates, foreground_hwnd};
use clipcheck_detect::{evaluate, DetectConfig, GateReason, Rect, Verdict};

const DESKTOP: &str = "needs interactive desktop";

fn zero_rect() -> Rect {
    Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }
}

#[test]
#[ignore = "needs interactive desktop"]
fn enumerate_returns_at_least_one_window() {
    let snaps = enumerate_candidates();
    assert!(
        !snaps.is_empty(),
        "enumerate_candidates() returned no windows on the desktop session"
    );
}

#[test]
#[ignore = "needs interactive desktop"]
fn all_snapshots_have_valid_rects() {
    for s in enumerate_candidates() {
        assert!(
            s.window_rect.right >= s.window_rect.left,
            "{:?}: window_rect right {} < left {}",
            s.exe_name,
            s.window_rect.right,
            s.window_rect.left
        );
        assert!(
            s.window_rect.bottom >= s.window_rect.top,
            "{:?}: window_rect bottom {} < top {}",
            s.exe_name,
            s.window_rect.bottom,
            s.window_rect.top
        );
        assert!(
            s.monitor_rect.area() > 0 || s.monitor_rect == zero_rect(),
            "{:?}: monitor_rect {:?} is neither positive-area nor the zero-rect fallback",
            s.exe_name,
            s.monitor_rect
        );
    }
}

#[test]
#[ignore = "needs interactive desktop"]
fn only_visible_or_minimized_windows_returned() {
    for s in enumerate_candidates() {
        assert!(
            s.is_visible || s.is_minimized,
            "{:?} (hwnd {}): is_visible={} is_minimized={}, expected one of them true",
            s.exe_name,
            s.target.hwnd,
            s.is_visible,
            s.is_minimized
        );
    }
}

#[test]
#[ignore = "needs interactive desktop"]
fn neutral_defaults_are_set() {
    for s in enumerate_candidates() {
        let ctx = format!("{:?} (hwnd {})", s.exe_name, s.target.hwnd);
        assert!(
            s.gpu_3d_load_pct.is_none(),
            "{ctx}: gpu_3d_load_pct not None"
        );
        assert!(
            s.gpu_3d_avg60_pct.is_none(),
            "{ctx}: gpu_3d_avg60_pct not None"
        );
        assert!(
            s.loaded_graphics_modules.is_empty(),
            "{ctx}: loaded_graphics_modules not empty: {:?}",
            s.loaded_graphics_modules
        );
        assert!(!s.in_known_db, "{ctx}: in_known_db is true");
        assert!(!s.in_library_scan, "{ctx}: in_library_scan is true");
        assert!(!s.is_running_game, "{ctx}: is_running_game is true");
        assert!(
            s.ms_since_foreground.is_none(),
            "{ctx}: ms_since_foreground not None: {:?}",
            s.ms_since_foreground
        );
        assert!(!s.is_launcher, "{ctx}: is_launcher is true");
        assert!(s.is_primary_in_group, "{ctx}: is_primary_in_group is false");
    }
}

/// Renamed from `own_process_window_handled_without_panic`: the second pass is
/// now checked too, not discarded.
#[test]
#[ignore = "needs interactive desktop"]
fn own_process_windows_have_nonzero_hwnd_on_repeat_enumeration() {
    let me = std::process::id();
    for pass in 0..2 {
        let snaps = enumerate_candidates();
        for s in snaps.iter().filter(|s| s.target.pid == me) {
            assert_ne!(
                s.target.hwnd, 0,
                "pass {pass}: own-process window has hwnd 0: {:?}",
                s.exe_name
            );
        }
    }
}

#[test]
fn foreground_hwnd_is_nonzero_when_present() {
    if let Some(h) = foreground_hwnd() {
        assert_ne!(h, 0, "foreground_hwnd() returned Some(0)");
    }
}

/// Renamed from `snapshots_feed_evaluate_without_panic`. A cleared exe name must
/// be rejected at G0 (`UnknownExe`), and every real verdict must have a clamped score.
#[test]
#[ignore = "needs interactive desktop"]
fn cleared_exe_name_is_unknown_exe_and_scores_are_clamped() {
    let cfg = DetectConfig::default();
    for snap in enumerate_candidates() {
        let mut cleared = snap.clone();
        cleared.exe_name = String::new();
        assert_eq!(
            evaluate(&cleared, &cfg),
            Verdict::Rejected(GateReason::UnknownExe),
            "cleared exe_name for hwnd {} must be UnknownExe",
            snap.target.hwnd
        );

        if let Verdict::Scored { score, .. } = evaluate(&snap, &cfg) {
            assert!(
                score <= 100,
                "{:?} (hwnd {}): score {} exceeds 100",
                snap.exe_name,
                snap.target.hwnd,
                score
            );
        }
    }
}

/// Renamed from `process_age_is_nonnegative_and_empty_exe_when_pid_zero`. The
/// "nonnegative" claim was dropped because `process_age_ms` is `u64`.
#[test]
#[ignore = "needs interactive desktop"]
fn pid_zero_snapshots_have_empty_exe_and_zero_age() {
    let snaps = enumerate_candidates();
    let zero_pid: Vec<_> = snaps.iter().filter(|s| s.target.pid == 0).collect();
    if zero_pid.is_empty() {
        eprintln!(
            "SKIP pid_zero_snapshots_have_empty_exe_and_zero_age: no pid-0 window on this desktop ({DESKTOP})"
        );
        return;
    }
    for s in zero_pid {
        assert!(
            s.exe_name.is_empty(),
            "pid 0 snapshot has non-empty exe_name {:?} (hwnd {})",
            s.exe_name,
            s.target.hwnd
        );
        assert_eq!(
            s.process_age_ms, 0,
            "pid 0 snapshot has process_age_ms {} (hwnd {})",
            s.process_age_ms, s.target.hwnd
        );
    }
}

#[test]
#[ignore = "needs interactive desktop"]
fn exe_name_has_no_path_separators() {
    for s in enumerate_candidates() {
        if s.exe_name.is_empty() {
            continue;
        }
        assert!(
            !s.exe_name.contains('\\') && !s.exe_name.contains('/'),
            "exe_name {:?} contains a path separator",
            s.exe_name
        );
    }
}
