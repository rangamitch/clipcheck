//! Behavioural tests for `clipcheck_detect::runner` (Windows-only module).
//!
//! Uses only the public API and a fake `SnapshotSource`. Time is driven
//! explicitly through `DetectorCore::tick`, except for the two thread tests.

#![cfg(windows)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipcheck_detect::runner::{DetectorCore, DetectorEvent, DetectorLoop, SnapshotSource};
use clipcheck_detect::{evaluate, CandidateSnapshot, CaptureTarget, DetectConfig, Rect, Verdict};

// ---------------------------------------------------------------------------
// fake source
// ---------------------------------------------------------------------------

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

/// Builds shared state where every snapshot's window and process are alive.
fn make_state(snaps: Vec<CandidateSnapshot>, fg: Option<u64>) -> Arc<Mutex<State>> {
    let mut st = State {
        foreground: fg,
        ..Default::default()
    };
    for s in &snaps {
        st.alive_hwnds.insert(s.target.hwnd);
        st.alive_pids.insert(s.target.pid);
    }
    st.snapshots = snaps;
    Arc::new(Mutex::new(st))
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Known-good fullscreen game: score clamps to 100 (known db 40 + geometry 20
/// + gpu 20 + d3d 10 + recent fg 5 + large window 5 + non-generic title 5).
fn game_snap(exe: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
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
        gpu_3d_load_pct: Some(50.0),
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: vec!["d3d11.dll".into()],
        ms_since_foreground: Some(0),
        is_running_game: false,
        process_age_ms: 60_000,
    }
}

/// Bound window for the retarget tests: known db 40 + geometry 20 + title 5
/// + large window 5 = 70, no GPU, graphics modules or recent-foreground points.
fn retarget_bound() -> CandidateSnapshot {
    let mut s = game_snap("game.exe", 1, 10);
    s.gpu_3d_load_pct = None;
    s.loaded_graphics_modules = Vec::new();
    s.ms_since_foreground = None;
    s
}

/// Same game, new window: +10 over the bound window (d3d module only) = 80.
fn retarget_challenger_plus10() -> CandidateSnapshot {
    let mut s = retarget_bound();
    s.target = CaptureTarget { pid: 11, hwnd: 2 };
    s.loaded_graphics_modules = vec!["d3d11.dll".into()];
    s
}

/// Same game, new window: +15 over the bound window (d3d 10 + recent fg 5) = 85.
fn retarget_challenger_plus15() -> CandidateSnapshot {
    let mut s = retarget_challenger_plus10();
    s.ms_since_foreground = Some(0);
    s
}

/// Runs ticks from `from` to `to` inclusive, every `step` ms, tagging each event with its tick time.
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

fn only_events(v: &[(u64, DetectorEvent)]) -> Vec<DetectorEvent> {
    v.iter().map(|(_, e)| e.clone()).collect()
}

fn started_game() -> DetectorEvent {
    DetectorEvent::GameStarted {
        hwnd: 100,
        exe: "game.exe".into(),
        title: "Some Game".into(),
        pid: Some(10),
    }
}

// ---------------------------------------------------------------------------
// 1. start debounce
// ---------------------------------------------------------------------------

#[test]
fn game_appears_and_starts_exactly_at_3000ms() {
    let snap = game_snap("game.exe", 100, 10);
    assert!(matches!(
        evaluate(&snap, &DetectConfig::default()),
        Verdict::Scored { accepted: true, .. }
    ));

    let st = make_state(vec![snap], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st)));

    assert!(core.tick(0).is_empty());
    assert!(core.tick(2999).is_empty());
    let ev = core.tick(3000);
    assert_eq!(ev, vec![started_game()]);
    assert!(core.tick(3500).is_empty());
}

// ---------------------------------------------------------------------------
// 2. blocklisted exe never starts
// ---------------------------------------------------------------------------

#[test]
fn blocklisted_chrome_never_starts_recording() {
    let snap = game_snap("chrome.exe", 100, 10);
    assert!(
        matches!(
            evaluate(&snap, &DetectConfig::default()),
            Verdict::Rejected(_)
        ),
        "fixture must be rejected by the gates"
    );

    let st = make_state(vec![snap], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st)));
    let evs = ticks(&mut core, 0, 60_000, 500);
    assert!(evs.is_empty(), "unexpected events: {evs:?}");
}

// ---------------------------------------------------------------------------
// 3. alt-tab does not stop
// ---------------------------------------------------------------------------

#[test]
fn alt_tab_away_does_not_stop_game() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let mut evs = ticks(&mut core, 0, 3000, 500);
    assert_eq!(only_events(&evs), vec![started_game()]);

    // Another window takes foreground; the game window stays alive and a candidate.
    st.lock().unwrap().foreground = Some(999);
    evs = ticks(&mut core, 3500, 63_000, 500);
    assert!(evs.is_empty(), "unexpected events: {evs:?}");
}

#[test]
fn game_window_dropped_from_snapshots_but_alive_does_not_stop() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let evs = ticks(&mut core, 0, 3000, 500);
    assert_eq!(only_events(&evs), vec![started_game()]);

    // Candidate disappears from enumeration, but window and process are alive.
    st.lock().unwrap().snapshots.clear();
    let evs = ticks(&mut core, 3500, 63_000, 500);
    assert!(evs.is_empty(), "unexpected events: {evs:?}");
}

// ---------------------------------------------------------------------------
// 4. window / process gone: stop only after the grace period
// ---------------------------------------------------------------------------

#[test]
fn window_destroyed_with_process_alive_stops_only_after_grace() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let evs = ticks(&mut core, 0, 3500, 500);
    assert_eq!(only_events(&evs), vec![started_game()]);

    // Window goes away before the tick at T=4000 (first tick observing the loss); pid stays alive.
    {
        let mut s = st.lock().unwrap();
        s.snapshots.clear();
        s.alive_hwnds.remove(&100);
    }
    let evs = ticks(&mut core, 4000, 4000, 1);
    assert!(evs.is_empty(), "unexpected events at loss: {evs:?}");

    // Every millisecond up to 4999: still no stop (window-gone grace is 1 s).
    let evs = ticks(&mut core, 4001, 4999, 1);
    assert!(evs.is_empty(), "stopped before grace expired: {evs:?}");

    let evs = ticks(&mut core, 5000, 5000, 1);
    assert_eq!(
        only_events(&evs),
        vec![DetectorEvent::GameStopped {
            exe: "game.exe".into()
        }],
        "expected exactly one GameStopped at loss + 1000 ms"
    );
    assert!(ticks(&mut core, 5001, 20_000, 500).is_empty());
}

#[test]
fn process_gone_stops_at_once() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let evs = ticks(&mut core, 0, 3500, 500);
    assert_eq!(only_events(&evs), vec![started_game()]);

    // Window and process both gone before the tick at T=4000.
    {
        let mut s = st.lock().unwrap();
        s.snapshots.clear();
        s.alive_hwnds.remove(&100);
        s.alive_pids.remove(&10);
    }
    let evs = ticks(&mut core, 4000, 4000, 1);
    assert_eq!(
        only_events(&evs),
        vec![DetectorEvent::GameStopped {
            exe: "game.exe".into()
        }],
        "process exit stops on the first tick that sees it"
    );
    assert!(ticks(&mut core, 4001, 20_000, 500).is_empty());
}

// ---------------------------------------------------------------------------
// 5. relaunch within grace
// ---------------------------------------------------------------------------

#[test]
fn relaunch_within_grace_emits_relaunched_not_stop_or_restart() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let mut all = ticks(&mut core, 0, 3000, 1000);

    // Window lost at T=4000.
    {
        let mut s = st.lock().unwrap();
        s.snapshots.clear();
        s.alive_hwnds.remove(&100);
    }
    all.extend(ticks(&mut core, 4000, 4000, 1000));

    // New window of the same exe appears 500 ms later (same process), inside the 1 s grace.
    {
        let mut s = st.lock().unwrap();
        let new = game_snap("game.exe", 200, 10);
        s.alive_hwnds.insert(200);
        s.snapshots = vec![new];
    }
    all.extend(ticks(&mut core, 4500, 6000, 500));

    assert!(
        !all.iter()
            .any(|(_, e)| matches!(e, DetectorEvent::GameStopped { .. })),
        "no GameStopped expected: {all:?}"
    );
    let starts = all
        .iter()
        .filter(|(_, e)| matches!(e, DetectorEvent::GameStarted { .. }))
        .count();
    assert_eq!(starts, 1, "exactly one GameStarted expected: {all:?}");

    let relaunches: Vec<_> = all
        .iter()
        .filter_map(|(t, e)| match e {
            DetectorEvent::GameRelaunched {
                hwnd,
                exe,
                pid,
                gap_ms,
                ..
            } => Some((*t, *hwnd, exe.clone(), *pid, *gap_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(relaunches.len(), 1, "expected one GameRelaunched: {all:?}");
    let (t, hwnd, exe, pid, gap) = relaunches[0].clone();
    assert_eq!(t, 4500);
    assert_eq!(hwnd, 200);
    assert_eq!(exe, "game.exe");
    assert_eq!(pid, Some(10));
    assert_eq!(gap, 500);
}

// ---------------------------------------------------------------------------
// 6. multiple candidates
// ---------------------------------------------------------------------------

#[test]
fn foreground_candidate_beats_higher_score_background_candidate() {
    // A: 70 and foreground. B: 100 and not foreground.
    let mut a = game_snap("a.exe", 100, 10);
    a.gpu_3d_load_pct = None;
    a.loaded_graphics_modules = Vec::new();
    a.ms_since_foreground = None;
    let b = game_snap("b.exe", 200, 20);
    assert!(matches!(
        evaluate(&a, &DetectConfig::default()),
        Verdict::Scored {
            score: 70,
            accepted: true,
            ..
        }
    ));
    assert!(matches!(
        evaluate(&b, &DetectConfig::default()),
        Verdict::Scored {
            score: 100,
            accepted: true,
            ..
        }
    ));

    let st = make_state(vec![a, b], Some(100));
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st)));
    let evs = ticks(&mut core, 0, 3000, 500);
    let starts: Vec<_> = only_events(&evs)
        .into_iter()
        .filter(|e| matches!(e, DetectorEvent::GameStarted { .. }))
        .collect();
    assert_eq!(
        starts,
        vec![DetectorEvent::GameStarted {
            hwnd: 100,
            exe: "a.exe".into(),
            title: "Some Game".into(),
            pid: Some(10),
        }]
    );
}

#[test]
fn higher_score_wins_when_neither_is_foreground() {
    let mut a = game_snap("a.exe", 100, 10);
    a.gpu_3d_load_pct = None;
    a.loaded_graphics_modules = Vec::new();
    a.ms_since_foreground = None;
    let b = game_snap("b.exe", 200, 20);

    let st = make_state(vec![a, b], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st)));
    let evs = ticks(&mut core, 0, 3000, 500);
    assert_eq!(
        only_events(&evs),
        vec![DetectorEvent::GameStarted {
            hwnd: 200,
            exe: "b.exe".into(),
            title: "Some Game".into(),
            pid: Some(20),
        }]
    );
}

// ---------------------------------------------------------------------------
// 7. retarget hysteresis (RETARGET_MARGIN = 15, RETARGET_CONFIRM = 2 s)
//
// Note: every signal and penalty is a multiple of 5 points, so a +14 gap is
// not constructible from real snapshots. The "below margin" case therefore
// uses +10, which is the closest reachable value below the 15-point margin.
// ---------------------------------------------------------------------------

#[test]
fn fixture_scores_are_70_80_85() {
    let cfg = DetectConfig::default();
    assert!(matches!(
        evaluate(&retarget_bound(), &cfg),
        Verdict::Scored {
            score: 70,
            accepted: true,
            ..
        }
    ));
    assert!(matches!(
        evaluate(&retarget_challenger_plus10(), &cfg),
        Verdict::Scored {
            score: 80,
            accepted: true,
            ..
        }
    ));
    assert!(matches!(
        evaluate(&retarget_challenger_plus15(), &cfg),
        Verdict::Scored {
            score: 85,
            accepted: true,
            ..
        }
    ));
}

#[test]
fn retarget_gap_below_margin_never_switches() {
    let st = make_state(vec![retarget_bound()], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let mut all = ticks(&mut core, 0, 3000, 500);
    assert_eq!(all.len(), 1);

    // Challenger (+10) first present at tick 5000 and stays for 10 s of ticks.
    all.extend(ticks(&mut core, 3500, 4500, 500));
    {
        let mut s = st.lock().unwrap();
        s.snapshots.push(retarget_challenger_plus10());
        s.alive_hwnds.insert(2);
        s.alive_pids.insert(11);
    }
    all.extend(ticks(&mut core, 5000, 15_000, 500));

    assert!(
        !all.iter()
            .any(|(_, e)| matches!(e, DetectorEvent::TargetChanged { .. })),
        "TargetChanged emitted for a gap below the margin: {all:?}"
    );
    assert!(
        !all.iter()
            .any(|(_, e)| matches!(e, DetectorEvent::GameStopped { .. })),
        "unexpected GameStopped: {all:?}"
    );
}

#[test]
fn retarget_gap_at_margin_switches_after_2s_confirm() {
    let st = make_state(vec![retarget_bound()], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st.clone())));

    let mut all = ticks(&mut core, 0, 3000, 500);
    assert_eq!(all.len(), 1);

    // Challenger (+15) first present at tick 5000.
    all.extend(ticks(&mut core, 3500, 4500, 500));
    {
        let mut s = st.lock().unwrap();
        s.snapshots.push(retarget_challenger_plus15());
        s.alive_hwnds.insert(2);
        s.alive_pids.insert(11);
    }
    all.extend(ticks(&mut core, 5000, 9000, 500));

    let changes: Vec<_> = all
        .iter()
        .filter_map(|(t, e)| match e {
            DetectorEvent::TargetChanged {
                exe,
                old_hwnd,
                new_hwnd,
                pid,
            } => Some((*t, exe.clone(), *old_hwnd, *new_hwnd, *pid)),
            _ => None,
        })
        .collect();
    assert_eq!(changes.len(), 1, "expected one TargetChanged: {all:?}");
    let (t, exe, old, new, pid) = changes[0].clone();
    assert!(
        t - 5000 >= 2000,
        "TargetChanged at {t} came before 2 s of held margin (first seen at 5000)"
    );
    assert_eq!(t, 7000, "expected switch exactly at first-seen + 2000 ms");
    assert_eq!(exe, "game.exe");
    assert_eq!(old, 1);
    assert_eq!(new, 2);
    assert_eq!(pid, Some(11));
    assert!(!all
        .iter()
        .any(|(_, e)| matches!(e, DetectorEvent::GameStopped { .. })));
}

// ---------------------------------------------------------------------------
// 8. shutdown
// ---------------------------------------------------------------------------

#[test]
fn shutdown_stops_active_game_once() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(FakeSource(st)));

    let evs = ticks(&mut core, 0, 3000, 500);
    assert_eq!(only_events(&evs), vec![started_game()]);

    assert_eq!(
        core.shutdown(3500),
        vec![DetectorEvent::GameStopped {
            exe: "game.exe".into()
        }]
    );
    assert!(core.shutdown(3600).is_empty());
}

// ---------------------------------------------------------------------------
// 9. real thread
// ---------------------------------------------------------------------------

#[test]
fn spawn_real_thread_stops_cleanly_without_stop_event() {
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let (tx, rx) = mpsc::channel();
    let loop_handle = DetectorLoop::spawn(
        DetectConfig::default(),
        Duration::from_millis(50),
        Box::new(FakeSource(st)),
        tx,
    );
    std::thread::sleep(Duration::from_secs(1));
    loop_handle.stop();

    let events: Vec<DetectorEvent> = rx.try_iter().collect();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DetectorEvent::GameStopped { .. })),
        "unexpected GameStopped: {events:?}"
    );
}

#[test]
fn spawn_with_clock_emits_start_when_fake_clock_reaches_3000() {
    let clock = Arc::new(AtomicU64::new(0));
    let clock_for_thread = clock.clone();
    let st = make_state(vec![game_snap("game.exe", 100, 10)], None);
    let (tx, rx) = mpsc::channel();
    let loop_handle = DetectorLoop::spawn_with_clock(
        DetectConfig::default(),
        Duration::from_millis(10),
        Box::new(FakeSource(st)),
        tx,
        Box::new(move || clock_for_thread.load(Ordering::SeqCst)),
    );

    // Let the thread tick at clock=0 so the pending debounce starts there.
    std::thread::sleep(Duration::from_millis(100));
    clock.store(3000, Ordering::SeqCst);

    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("GameStarted should arrive after the fake clock reaches 3000");
    assert_eq!(ev, started_game());

    loop_handle.stop();
}
