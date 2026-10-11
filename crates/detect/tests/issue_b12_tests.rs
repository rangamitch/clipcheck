//! Regression tests for detect issues #3 (relaunch carries display_name),
//! #54 (stop reason follows liveness), and #6 (worked example 4 is scored 35
//! and not eligible). Public API only. Runner tests are Windows-only and use a
//! fake `SnapshotSource`; no games, processes or windows are touched.
//!
//! Not covered here (see the report): #57 (`evaluate` is now `#[doc(hidden)]`,
//! behaviour unchanged, equivalence already asserted by
//! `api_behavior::evaluate_with_matches_evaluate`) and #66 (an example binary,
//! not a behaviour).

use clipcheck_detect::{
    evaluate, CandidateSnapshot, CaptureTarget, Debouncer, DetectConfig, DetectEvent, Liveness,
    Pick, Reason, Rect, StartReason, StopReason, Verdict,
};

// ---------------------------------------------------------------------------
// helpers (debouncer / score)
// ---------------------------------------------------------------------------

fn named_pick(game: &str, display: &str, hwnd: u64) -> Pick {
    Pick {
        game: game.into(),
        display_name: display.into(),
        target: CaptureTarget { pid: 100, hwnd },
        reason: StartReason::Detected,
        pinned: false,
        process_age_ms: 60_000,
    }
}

/// Debouncer that has already reported a start for `game.exe` with the given name.
fn started_named(display: &str) -> Debouncer {
    let mut d = Debouncer::new(3000, 10_000);
    d.update(0, Some(named_pick("game.exe", display, 1)), Liveness::Alive);
    let evs = d.update(
        3000,
        Some(named_pick("game.exe", display, 1)),
        Liveness::Alive,
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, DetectEvent::GameStarted { .. })),
        "setup: expected GameStarted, got {evs:?}"
    );
    d
}

fn monitor() -> Rect {
    Rect::from_size(0, 0, 1920, 1080)
}

/// Same fixture as the detect score tests' `base()`: a fullscreen window with no
/// known-db, no GPU, no graphics modules.
fn base() -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: "game.exe".into(),
        window_title: "Some Game".into(),
        window_rect: monitor(),
        monitor_rect: monitor(),
        target: CaptureTarget { pid: 100, hwnd: 1 },
        is_visible: true,
        is_minimized: false,
        is_cloaked: false,
        is_tool_window: false,
        has_caption: false,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: false,
        in_library_scan: false,
        gpu_3d_load_pct: None,
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: vec![],
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms: 60_000,
    }
}

// ---------------------------------------------------------------------------
// #3: GameRelaunched carries the display name (debouncer level)
// ---------------------------------------------------------------------------

#[test]
fn issue_3_debouncer_relaunch_carries_display_name_not_exe() {
    let mut d = started_named("Some Game");
    // Target window lost, process alive: enters Ending (1 s window-gone grace).
    assert!(d.update(10_000, None, Liveness::WindowDestroyed).is_empty());
    // Same game comes back in a new window within grace.
    let evs = d.update(
        10_500,
        Some(named_pick("game.exe", "Some Game", 5)),
        Liveness::Alive,
    );
    match evs.as_slice() {
        [DetectEvent::GameRelaunched {
            game,
            display_name,
            gap_ms,
            ..
        }] => {
            assert_eq!(game, "game.exe");
            assert_eq!(display_name, "Some Game");
            assert_eq!(*gap_ms, 500);
        }
        other => panic!("expected single GameRelaunched, got {other:?}"),
    }
}

#[test]
fn issue_3_relaunch_display_name_matches_start_display_name() {
    let mut d = started_named("Named Window");
    let start_name = "Named Window";
    d.update(10_000, None, Liveness::WindowDestroyed);
    let evs = d.update(
        10_500,
        Some(named_pick("game.exe", start_name, 5)),
        Liveness::Alive,
    );
    let relaunch_name = evs.iter().find_map(|e| match e {
        DetectEvent::GameRelaunched { display_name, .. } => Some(display_name.clone()),
        _ => None,
    });
    assert_eq!(
        relaunch_name.as_deref(),
        Some(start_name),
        "events: {evs:?}"
    );
}

// ---------------------------------------------------------------------------
// #54: stop reason is derived from liveness; a live target never stops
// ---------------------------------------------------------------------------

#[test]
fn issue_54_process_exited_stops_at_once_with_process_exited_reason() {
    let mut d = started_named("Some Game");
    let evs = d.update(10_000, None, Liveness::ProcessExited);
    assert!(
        evs.iter().any(|e| matches!(
            e,
            DetectEvent::GameStopped {
                reason: StopReason::ProcessExited,
                ..
            }
        )),
        "events: {evs:?}"
    );
    assert!(!d.is_active());
}

#[test]
fn issue_54_window_destroyed_stops_after_window_gone_grace() {
    let mut d = started_named("Some Game");
    d.update(10_000, None, Liveness::WindowDestroyed);
    let evs = d.update(11_000, None, Liveness::WindowDestroyed);
    assert!(
        evs.iter().any(|e| matches!(
            e,
            DetectEvent::GameStopped {
                reason: StopReason::WindowDestroyed,
                ..
            }
        )),
        "events: {evs:?}"
    );
    assert!(!d.is_active());
}

#[test]
fn issue_54_window_destroyed_stops_with_window_destroyed_reason() {
    let mut d = started_named("Some Game");
    d.update(10_000, None, Liveness::WindowDestroyed);
    let evs = d.update(20_001, None, Liveness::WindowDestroyed);
    assert!(
        evs.iter().any(|e| matches!(
            e,
            DetectEvent::GameStopped {
                reason: StopReason::WindowDestroyed,
                ..
            }
        )),
        "events: {evs:?}"
    );
    assert!(!d.is_active());
}

#[test]
fn issue_54_alive_target_with_no_candidate_never_stops() {
    let mut d = started_named("Some Game");
    for now in (4_000..=120_000).step_by(4_000) {
        let evs = d.update(now, None, Liveness::Alive);
        assert!(
            !evs.iter()
                .any(|e| matches!(e, DetectEvent::GameStopped { .. })),
            "live target must not stop at {now}: {evs:?}"
        );
    }
    assert!(d.is_active());
}

// ---------------------------------------------------------------------------
// #6: worked example 4 (GPU 3 %, no D3D module) is scored 35 and not eligible
// ---------------------------------------------------------------------------

#[test]
fn issue_6_worked_example_4_scores_35_and_is_not_eligible() {
    let mut s = base();
    s.gpu_3d_load_pct = Some(3.0);
    match evaluate(&s, &DetectConfig::default()) {
        Verdict::Scored {
            score,
            breakdown,
            accepted,
        } => {
            assert_eq!(score, 35, "breakdown: {breakdown:?}");
            assert!(!accepted, "score 35 is below min_score 60");
            assert!(
                breakdown
                    .iter()
                    .any(|(r, p)| *r == Reason::FullscreenGeometry && *p == 20),
                "Geometry must satisfy G8 and contribute +20: {breakdown:?}"
            );
        }
        other => panic!("expected Scored (G8 satisfied by Geometry), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Runner level (Windows only): relaunch title equals the start title
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod runner_relaunch {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
    use clipcheck_detect::{CandidateSnapshot, CaptureTarget, DetectConfig, Rect};

    #[derive(Default)]
    struct State {
        snapshots: Vec<CandidateSnapshot>,
        alive_hwnds: HashSet<u64>,
        alive_pids: HashSet<u32>,
        foreground: Option<u64>,
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
            self.0.lock().unwrap().foreground
        }
        fn is_process_alive(&mut self, pid: u32) -> bool {
            self.0.lock().unwrap().alive_pids.contains(&pid)
        }
    }

    fn make_state(snaps: Vec<CandidateSnapshot>) -> Arc<Mutex<State>> {
        let mut st = State::default();
        for s in &snaps {
            st.alive_hwnds.insert(s.target.hwnd);
            st.alive_pids.insert(s.target.pid);
        }
        st.snapshots = snaps;
        Arc::new(Mutex::new(st))
    }

    /// Fullscreen known game (score 100), window title as given.
    fn game_snap(exe: &str, title: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
        CandidateSnapshot {
            exe_name: exe.into(),
            window_title: title.into(),
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
            gpu_3d_load_pct: Some(50.0),
            gpu_3d_avg60_pct: None,
            loaded_graphics_modules: vec!["d3d11.dll".into()],
            ms_since_foreground: Some(0),
            is_running_game: false,
            process_age_ms: 60_000,
        }
    }

    fn ticks(core: &mut DetectorCore, from: u64, to: u64, step: u64) -> Vec<(u64, DetectorEvent)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            out.extend(core.tick(t).into_iter().map(|e| (t, e)));
            t += step;
        }
        out
    }

    /// Starts `exe` with `start_title`, drops it at 4000, brings back a new
    /// window at 9000 with `relaunch_title`. Returns (start title, relaunch title).
    fn start_then_relaunch(exe: &str, start_title: &str, relaunch_title: &str) -> (String, String) {
        let st = make_state(vec![game_snap(exe, start_title, 100, 10)]);
        let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

        let mut all = ticks(&mut core, 0, 3000, 1000);
        {
            let mut s = st.lock().unwrap();
            s.snapshots.clear();
            s.alive_hwnds.remove(&100);
        }
        // The process lives on (pid 10); its window is back 500 ms later, inside
        // the 1 s window-gone grace.
        all.extend(ticks(&mut core, 4000, 4000, 1000));
        {
            let mut s = st.lock().unwrap();
            s.alive_hwnds.insert(200);
            s.snapshots = vec![game_snap(exe, relaunch_title, 200, 10)];
        }
        all.extend(ticks(&mut core, 4500, 12_000, 500));

        let start = all
            .iter()
            .find_map(|(_, e)| match e {
                DetectorEvent::GameStarted { title, .. } => Some(title.clone()),
                _ => None,
            })
            .expect("GameStarted expected");
        let relaunch = all
            .iter()
            .find_map(|(_, e)| match e {
                DetectorEvent::GameRelaunched { title, .. } => Some(title.clone()),
                _ => None,
            })
            .expect("GameRelaunched expected");
        (start, relaunch)
    }

    #[test]
    fn issue_3_runner_relaunch_title_equals_start_title() {
        let (start, relaunch) = start_then_relaunch("game.exe", "Some Game", "Some Game");
        assert_eq!(start, "Some Game");
        assert_eq!(relaunch, start);
    }

    #[test]
    fn issue_3_runner_relaunch_title_falls_back_to_exe_like_start() {
        // Empty window title: both start and relaunch fall back to the exe name,
        // never to an empty string.
        let (start, relaunch) = start_then_relaunch("game.exe", "", "");
        assert_eq!(start, "game.exe");
        assert_eq!(relaunch, "game.exe");
        assert!(!relaunch.is_empty());
    }
}
