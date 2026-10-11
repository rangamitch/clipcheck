//! Issue #2 (empty exe name is rejected before other gates) and issue #4
//! (runner hysteresis while the bound window fails a gate).
//!
//! Public API only. Runner parts use a fake `SnapshotSource` and drive time
//! explicitly through `DetectorCore::tick`.

use clipcheck_detect::{
    evaluate, evaluate_with, CandidateSnapshot, CaptureTarget, DetectConfig, GateReason, Lists,
    Rect, Verdict,
};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Full-screen known game: geometry 20 + known db 40 + title 5 + large 5 = 70.
fn snap(exe: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: exe.into(),
        window_title: "Some Game".into(),
        window_rect: Rect::from_size(0, 0, 1920, 1080),
        monitor_rect: Rect::from_size(0, 0, 1920, 1080),
        target: CaptureTarget { pid, hwnd },
        is_visible: true,
        is_minimized: false,
        is_cloaked: false,
        is_tool_window: false,
        has_caption: false,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: true,
        in_library_scan: false,
        gpu_3d_load_pct: None,
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: vec![],
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms: 60_000,
    }
}

/// Same window as `snap` but 800x600 (not large, not fullscreen), with GPU 50 %:
/// known db 40 + GPU above threshold 20 + title 5 = 65.
fn weak(exe: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
    let mut s = snap(exe, hwnd, pid);
    s.window_rect = Rect::from_size(100, 100, 800, 600);
    s.gpu_3d_load_pct = Some(50.0);
    s
}

/// `weak` plus D3D module 10 and recent foreground 5 = 80.
fn strong(exe: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
    let mut s = weak(exe, hwnd, pid);
    s.loaded_graphics_modules = vec!["d3d11.dll".into()];
    s.ms_since_foreground = Some(0);
    s
}

/// `snap` plus D3D module 10 and recent foreground 5 = 85.
fn snap85(exe: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
    let mut s = snap(exe, hwnd, pid);
    s.loaded_graphics_modules = vec!["d3d11.dll".into()];
    s.ms_since_foreground = Some(0);
    s
}

/// Fails G5 (window smaller than 640x480).
fn too_small(mut s: CandidateSnapshot) -> CandidateSnapshot {
    s.window_rect = Rect::from_size(0, 0, 400, 300);
    s
}

fn scored(v: &Verdict) -> (u32, bool) {
    match v {
        Verdict::Scored {
            score, accepted, ..
        } => (*score, *accepted),
        Verdict::Rejected(r) => panic!("expected Scored, got Rejected({r:?})"),
    }
}

// ---------------------------------------------------------------------------
// Issue #2: empty / separator-only exe names
// ---------------------------------------------------------------------------

#[test]
fn empty_or_separator_only_exe_is_rejected_as_unknown_exe() {
    let cfg = DetectConfig::default();
    let lists = Lists::from_config(&cfg);
    let names = [
        "", " ", "   ", "\t", "\\", "/", "  ", "\\\\", "/\\/", "C:\\", "C:/", "\\ \\", "\\  ",
    ];
    for exe in names {
        let s = snap(exe, 1, 10);
        assert_eq!(
            evaluate(&s, &cfg),
            Verdict::Rejected(GateReason::UnknownExe),
            "evaluate: exe {exe:?}"
        );
        assert_eq!(
            evaluate_with(&lists, &s, &cfg),
            Verdict::Rejected(GateReason::UnknownExe),
            "evaluate_with: exe {exe:?}"
        );
    }
}

#[test]
fn unknown_exe_is_checked_before_other_gates() {
    // Would otherwise fail G3 (tool window), G4 (not visible), G5 (too small),
    // G2 (launcher) and pass the known-db match: the empty exe must still win.
    let cfg = DetectConfig::default();
    let mut s = too_small(snap("", 1, 10));
    s.is_tool_window = true;
    s.is_visible = false;
    s.is_launcher = true;
    assert_eq!(
        evaluate(&s, &cfg),
        Verdict::Rejected(GateReason::UnknownExe)
    );
}

#[test]
fn valid_exe_names_keep_their_verdict() {
    let cfg = DetectConfig::default();
    let (base_score, base_accepted) = scored(&evaluate(&snap("game.exe", 1, 10), &cfg));
    assert!(base_accepted, "fixture must be accepted");
    assert_eq!(base_score, 70);

    let names = [
        "GAME.EXE",
        "Game.Exe",
        "My Game.exe",
        "ゲーム.exe",
        "C:\\Games\\My Game\\game.exe",
        "D:/Games/game.exe",
    ];
    for exe in names {
        let v = evaluate(&snap(exe, 1, 10), &cfg);
        assert_eq!(scored(&v), (base_score, base_accepted), "exe {exe:?}");
    }
}

#[test]
fn blocked_exe_still_reports_blocklisted() {
    let cfg = DetectConfig::default();
    assert_eq!(
        evaluate(&snap("chrome.exe", 1, 10), &cfg),
        Verdict::Rejected(GateReason::Blocklisted)
    );
}

// ---------------------------------------------------------------------------
// Runner parts (Windows-only module)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod runner_parts {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
    use clipcheck_detect::{evaluate, CandidateSnapshot, DetectConfig, GateReason, Verdict};

    use super::{snap, snap85, strong, too_small, weak};

    #[derive(Default)]
    struct State {
        snapshots: Vec<CandidateSnapshot>,
        alive_hwnds: HashSet<u64>,
        alive_pids: HashSet<u32>,
    }

    struct FakeSource(Arc<Mutex<State>>);

    impl SnapshotSource for FakeSource {
        fn snapshots(&mut self) -> Vec<CandidateSnapshot> {
            self.0.lock().unwrap().snapshots.clone()
        }
        fn is_window_alive(&mut self, hwnd: u64) -> bool {
            self.0.lock().unwrap().alive_hwnds.contains(&hwnd)
        }
        fn foreground(&mut self) -> Option<u64> {
            None
        }
        fn is_process_alive(&mut self, pid: u32) -> bool {
            self.0.lock().unwrap().alive_pids.contains(&pid)
        }
    }

    /// Replaces the candidate list and marks each window and process alive.
    fn present(st: &Arc<Mutex<State>>, snaps: Vec<CandidateSnapshot>) {
        let mut s = st.lock().unwrap();
        for c in &snaps {
            s.alive_hwnds.insert(c.target.hwnd);
            s.alive_pids.insert(c.target.pid);
        }
        s.snapshots = snaps;
    }

    fn new_core(st: &Arc<Mutex<State>>) -> DetectorCore {
        DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())))
    }

    /// Runs ticks from `from` to `to` inclusive, every `step` ms.
    fn ticks(core: &mut DetectorCore, from: u64, to: u64, step: u64) -> Vec<(u64, DetectorEvent)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            for ev in core.tick(t) {
                out.push((t, ev));
            }
            t += step;
        }
        out
    }

    fn evs_only(v: &[(u64, DetectorEvent)]) -> Vec<DetectorEvent> {
        v.iter().map(|(_, e)| e.clone()).collect()
    }

    fn target_changes(v: &[(u64, DetectorEvent)]) -> Vec<(u64, isize, isize)> {
        v.iter()
            .filter_map(|(t, e)| match e {
                DetectorEvent::TargetChanged {
                    exe,
                    old_hwnd,
                    new_hwnd,
                    ..
                } if exe == "game.exe" || exe == "other.exe" => Some((*t, *old_hwnd, *new_hwnd)),
                _ => None,
            })
            .collect()
    }

    fn stopped_exe(v: &[(u64, DetectorEvent)], exe: &str) -> bool {
        v.iter()
            .any(|(_, e)| matches!(e, DetectorEvent::GameStopped { exe: x } if x == exe))
    }

    fn started_exe(v: &[(u64, DetectorEvent)], exe: &str) -> Vec<(u64, isize, Option<u32>)> {
        v.iter()
            .filter_map(|(t, e)| match e {
                DetectorEvent::GameStarted {
                    exe: x, hwnd, pid, ..
                } if x == exe => Some((*t, *hwnd, *pid)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fixture_scores_match_hysteresis_derivation() {
        let cfg = DetectConfig::default();
        let scored = |s: &CandidateSnapshot| match evaluate(s, &cfg) {
            Verdict::Scored {
                score, accepted, ..
            } => (score, accepted),
            v => panic!("expected Scored, got {v:?}"),
        };
        assert_eq!(scored(&snap("game.exe", 1, 10)), (70, true));
        assert_eq!(scored(&weak("game.exe", 2, 11)), (65, true));
        assert_eq!(scored(&strong("game.exe", 2, 11)), (80, true));
        assert_eq!(scored(&snap85("game.exe", 3, 12)), (85, true));
        assert_eq!(
            evaluate(&too_small(snap("game.exe", 1, 10)), &cfg),
            Verdict::Rejected(GateReason::TooSmall)
        );
    }

    // -- #2: empty exe through the selection API ---------------------------

    #[test]
    fn empty_exe_windows_never_win_selection() {
        // Empty-exe windows score 85 and are foreground-like, so they would
        // beat the valid 70-point game if they were not rejected.
        let st = Arc::new(Mutex::new(State::default()));
        let empty_a = snap85("", 200, 20);
        let empty_b = snap85("\\", 201, 21);
        present(&st, vec![empty_a, empty_b, snap("game.exe", 1, 10)]);
        let mut core = new_core(&st);

        let all = ticks(&mut core, 0, 6000, 500);
        let started = started_exe(&all, "game.exe");
        assert_eq!(started.len(), 1, "expected one start: {all:?}");
        assert_eq!(started[0].1, 1, "wrong bound window: {all:?}");
        assert_eq!(started[0].2, Some(10));
        for (_, e) in &all {
            let exe = match e {
                DetectorEvent::GameStarted { exe, .. }
                | DetectorEvent::GameRelaunched { exe, .. }
                | DetectorEvent::GameStopped { exe }
                | DetectorEvent::TargetChanged { exe, .. } => exe,
            };
            assert!(!exe.is_empty(), "event recorded with empty game id: {e:?}");
        }
    }

    #[test]
    fn only_empty_exe_windows_never_record() {
        let st = Arc::new(Mutex::new(State::default()));
        present(
            &st,
            vec![
                snap85("", 200, 20),
                snap85("   ", 201, 21),
                snap85("/", 202, 22),
            ],
        );
        let mut core = new_core(&st);
        let all = ticks(&mut core, 0, 30_000, 500);
        assert!(evs_only(&all).is_empty(), "unexpected events: {all:?}");
    }

    // -- #4: hysteresis while the bound window fails a gate ----------------

    #[test]
    fn a_rejected_while_same_game_scores_65_does_not_switch() {
        let st = Arc::new(Mutex::new(State::default()));
        present(&st, vec![snap("game.exe", 1, 10)]);
        let mut core = new_core(&st);
        let first = ticks(&mut core, 0, 3000, 500);
        assert_eq!(started_exe(&first, "game.exe").len(), 1, "setup: {first:?}");

        // Bound window A now fails G5; B (65, same game) is present for 12 s.
        present(
            &st,
            vec![too_small(snap("game.exe", 1, 10)), weak("game.exe", 2, 11)],
        );
        let all = ticks(&mut core, 3500, 15_000, 500);
        assert!(
            target_changes(&all).is_empty(),
            "switched to 65-point window while bound was rejected: {all:?}"
        );
        assert!(!stopped_exe(&all, "game.exe"), "unexpected stop: {all:?}");
    }

    #[test]
    fn a_rejected_while_other_game_scores_far_higher_keeps_recording() {
        // Documented in runner.rs: other games never displace a recording, the
        // debouncer keeps it alive while the bound process is alive.
        let st = Arc::new(Mutex::new(State::default()));
        present(&st, vec![snap("game.exe", 1, 10)]);
        let mut core = new_core(&st);
        let first = ticks(&mut core, 0, 3000, 500);
        assert_eq!(started_exe(&first, "game.exe").len(), 1, "setup: {first:?}");

        present(
            &st,
            vec![too_small(snap("game.exe", 1, 10)), snap("other.exe", 3, 30)],
        );
        let all = ticks(&mut core, 3500, 20_000, 500);
        assert!(target_changes(&all).is_empty(), "unexpected: {all:?}");
        assert!(!stopped_exe(&all, "game.exe"), "unexpected stop: {all:?}");
        assert!(
            started_exe(&all, "other.exe").is_empty(),
            "other game displaced recording: {all:?}"
        );
    }

    #[test]
    fn destroyed_bound_hwnd_does_not_leak_score_to_reused_hwnd() {
        let st = Arc::new(Mutex::new(State::default()));
        present(&st, vec![snap("game.exe", 1, 10)]);
        let mut core = new_core(&st);
        let mut all = ticks(&mut core, 0, 3000, 500);

        // Bound A is rejected for a few ticks, then destroyed (process alive).
        present(&st, vec![too_small(snap("game.exe", 1, 10))]);
        all.extend(ticks(&mut core, 3500, 6000, 500));
        {
            let mut s = st.lock().unwrap();
            s.snapshots.clear();
            s.alive_hwnds.remove(&1);
        }
        all.extend(ticks(&mut core, 6500, 25_000, 500));
        assert!(
            stopped_exe(&all, "game.exe"),
            "game.exe never stopped: {all:?}"
        );

        // Hwnd 1 is reused by a different game: accepted at 65 and started.
        present(&st, vec![weak("other.exe", 1, 30)]);
        all.extend(ticks(&mut core, 25_500, 28_500, 500));
        let started = started_exe(&all, "other.exe");
        assert_eq!(started.len(), 1, "other.exe did not start: {all:?}");
        assert_eq!(started[0].1, 1);

        // The reused window is rejected from now on; a same-game challenger
        // scores 80. The only valid base is that window's own score (<= 65),
        // so 80 beats it by the 15-point margin. A base of 70 (the stale
        // score of the destroyed game.exe window) would block the switch.
        present(
            &st,
            vec![
                too_small(weak("other.exe", 1, 30)),
                strong("other.exe", 2, 31),
            ],
        );
        all.extend(ticks(&mut core, 29_000, 36_000, 500));

        let changes = target_changes(&all);
        assert_eq!(changes.len(), 1, "expected one TargetChanged: {all:?}");
        assert_eq!(changes[0].1, 1);
        assert_eq!(changes[0].2, 2);
        assert!(
            !all.iter().any(|(_, e)| matches!(
                e,
                DetectorEvent::TargetChanged { exe, .. } if exe == "game.exe"
            )),
            "game.exe event after reuse: {all:?}"
        );
    }

    #[test]
    fn bound_window_recovers_and_normal_hysteresis_resumes() {
        let st = Arc::new(Mutex::new(State::default()));
        present(&st, vec![snap("game.exe", 1, 10)]);
        let mut core = new_core(&st);
        let mut all = ticks(&mut core, 0, 3000, 500);

        present(
            &st,
            vec![too_small(snap("game.exe", 1, 10)), weak("game.exe", 2, 11)],
        );
        all.extend(ticks(&mut core, 3500, 8000, 500));

        // A recovers (70); B (65) still present: no switch.
        present(&st, vec![snap("game.exe", 1, 10), weak("game.exe", 2, 11)]);
        all.extend(ticks(&mut core, 8500, 12_000, 500));
        assert!(
            target_changes(&all).is_empty(),
            "unexpected switch: {all:?}"
        );
        assert!(!stopped_exe(&all, "game.exe"), "unexpected stop: {all:?}");

        // Challenger at the margin (85 = 70 + 15) switches after the 2 s confirm.
        present(
            &st,
            vec![
                snap("game.exe", 1, 10),
                weak("game.exe", 2, 11),
                snap85("game.exe", 3, 12),
            ],
        );
        all.extend(ticks(&mut core, 12_500, 18_000, 500));
        let changes = target_changes(&all);
        assert_eq!(changes.len(), 1, "expected one TargetChanged: {all:?}");
        let (t, old, new) = changes[0];
        assert_eq!((old, new), (1, 3));
        assert!(t - 12_500 >= 2000, "switched before 2 s confirm at {t}");
    }
}
