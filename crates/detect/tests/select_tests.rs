//! Selection behaviour of `DetectorCore` for pinned (allowlisted) versus
//! unpinned candidates, auto versus allowlist-only mode, and the runtime
//! allowlist-only flip. Uses only the public API and a fake `SnapshotSource`;
//! time is driven explicitly through `DetectorCore::tick`.

#![cfg(windows)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
use clipcheck_detect::{AllowRule, CandidateSnapshot, CaptureTarget, DetectConfig, Rect};

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

/// Shared state where every snapshot's window and process are alive.
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

/// Adds a snapshot to the shared state (window and process alive).
fn add_snap(state: &Arc<Mutex<State>>, snap: CandidateSnapshot) {
    let mut st = state.lock().unwrap();
    st.alive_hwnds.insert(snap.target.hwnd);
    st.alive_pids.insert(snap.target.pid);
    st.snapshots.push(snap);
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Known-good fullscreen game (unpinned unless allow-listed): score 100.
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

/// Notepad-like desktop app: visible, not minimized, not cloaked, windowed
/// (not fullscreen), no graphics evidence. Only the allowlist lets it through
/// (score is about 5: non-generic title only). Use `process_age_ms` 60_000 for an
/// immediate start, or 0 to see the 3 s start confirm.
fn pinned_app(exe: &str, hwnd: u64, pid: u32, process_age_ms: u64) -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: exe.into(),
        window_title: "Scratch Notes".into(),
        window_rect: Rect::from_size(100, 100, 800, 600),
        monitor_rect: Rect::from_size(0, 0, 1920, 1080),
        target: CaptureTarget { pid, hwnd },
        is_visible: true,
        is_minimized: false,
        is_cloaked: false,
        is_tool_window: false,
        has_caption: true,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: false,
        in_library_scan: false,
        gpu_3d_load_pct: None,
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: Vec::new(),
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms,
    }
}

fn allow(exes: &[&str]) -> Vec<AllowRule> {
    exes.iter()
        .map(|e| AllowRule {
            exe: (*e).into(),
            display_name: None,
        })
        .collect()
}

fn cfg_with(allow_exes: &[&str], allowlist_only: bool) -> DetectConfig {
    DetectConfig {
        allow: allow(allow_exes),
        allowlist_only,
        ..DetectConfig::default()
    }
}

/// Runs ticks from `from` to `to` inclusive, every `step` ms, tagging each event with its tick time.
fn run(core: &mut DetectorCore, from: u64, to: u64, step: u64) -> Vec<(u64, DetectorEvent)> {
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

fn exe_of(ev: &DetectorEvent) -> &str {
    match ev {
        DetectorEvent::GameStarted { exe, .. }
        | DetectorEvent::GameStopped { exe }
        | DetectorEvent::GameRelaunched { exe, .. }
        | DetectorEvent::TargetChanged { exe, .. } => exe,
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

/// 1. Auto mode: a pinned low-score app beats an unpinned score-100 game.
#[test]
fn auto_mode_pinned_app_beats_unpinned_game() {
    let state = make_state(
        vec![
            game_snap("game.exe", 1, 10),
            pinned_app("notes-app.exe", 2, 20, 0),
        ],
        None,
    );
    let mut core = DetectorCore::new(
        cfg_with(&["notes-app.exe"], false),
        Box::new(FakeSource(state)),
    );

    let events = run(&mut core, 0, 5000, 1000);

    let started: Vec<_> = events
        .iter()
        .filter(|(_, e)| matches!(e, DetectorEvent::GameStarted { .. }))
        .collect();
    assert_eq!(started.len(), 1, "expected one GameStarted, got {events:?}");
    let (t, ev) = started[0];
    assert_eq!(
        exe_of(ev),
        "notes-app.exe",
        "wrong game started: {events:?}"
    );
    // process_age 0 means the 3 s start confirm applies.
    assert_eq!(
        *t, 3000,
        "pinned start should wait the 3 s confirm: {events:?}"
    );
    assert!(
        events.iter().all(|(_, e)| exe_of(e) != "game.exe"),
        "unpinned game must produce no events: {events:?}"
    );
}

/// 2. Allowlist mode with only an unpinned game: no events at all.
#[test]
fn allowlist_mode_unpinned_only_produces_no_events() {
    let state = make_state(vec![game_snap("game.exe", 1, 10)], None);
    let mut core = DetectorCore::new(cfg_with(&[], true), Box::new(FakeSource(state)));

    let events = run(&mut core, 0, 19_000, 1000);
    assert!(
        events.is_empty(),
        "allowlist mode must not start an unpinned game: {events:?}"
    );
}

/// 3. Allowlist mode with a pinned app and an unpinned game: only the pinned app is ever mentioned.
#[test]
fn allowlist_mode_only_pinned_app_is_selected() {
    let state = make_state(
        vec![
            game_snap("game.exe", 1, 10),
            pinned_app("notes-app.exe", 2, 20, 60_000),
        ],
        None,
    );
    let mut core = DetectorCore::new(
        cfg_with(&["notes-app.exe"], true),
        Box::new(FakeSource(state)),
    );

    let events = run(&mut core, 0, 10_000, 1000);

    assert_eq!(
        events.len(),
        1,
        "expected exactly one event (GameStarted), got {events:?}"
    );
    assert!(
        matches!(&events[0].1, DetectorEvent::GameStarted { exe, .. } if exe == "notes-app.exe"),
        "expected GameStarted for notes-app.exe, got {events:?}"
    );
    assert!(
        events.iter().all(|(_, e)| exe_of(e) != "game.exe"),
        "no event may mention the unpinned game: {events:?}"
    );
}

/// 4. Two pinned apps: the foreground one wins; with no foreground, the topmost wins.
#[test]
fn two_pinned_apps_foreground_wins_then_topmost_on_tie() {
    // Foreground is the second app: it must win over the topmost first app.
    let state = make_state(
        vec![
            pinned_app("app-a.exe", 1, 10, 60_000),
            pinned_app("app-b.exe", 2, 20, 60_000),
        ],
        Some(2),
    );
    let mut core = DetectorCore::new(
        cfg_with(&["app-a.exe", "app-b.exe"], false),
        Box::new(FakeSource(state)),
    );
    let events = run(&mut core, 0, 0, 1000);
    assert_eq!(events.len(), 1, "expected one GameStarted: {events:?}");
    match &events[0].1 {
        DetectorEvent::GameStarted { exe, hwnd, .. } => {
            assert_eq!(exe, "app-b.exe", "foreground pinned app must win");
            assert_eq!(*hwnd, 2, "foreground window handle must be bound");
        }
        other => panic!("expected GameStarted, got {other:?}"),
    }

    // Full tie (no foreground): topmost (first in snapshot list) wins.
    let state = make_state(
        vec![
            pinned_app("app-a.exe", 1, 10, 60_000),
            pinned_app("app-b.exe", 2, 20, 60_000),
        ],
        None,
    );
    let mut core = DetectorCore::new(
        cfg_with(&["app-a.exe", "app-b.exe"], false),
        Box::new(FakeSource(state)),
    );
    let events = run(&mut core, 0, 0, 1000);
    assert!(
        matches!(events.as_slice(), [(_, DetectorEvent::GameStarted { exe, .. })] if exe == "app-a.exe"),
        "topmost pinned app must win a full tie: {events:?}"
    );
}

/// 5. A bound pinned target is not displaced by an unpinned higher-score game appearing later.
#[test]
fn bound_pinned_target_not_displaced_by_later_unpinned_game() {
    let state = make_state(vec![pinned_app("notes-app.exe", 2, 20, 60_000)], None);
    let mut core = DetectorCore::new(
        cfg_with(&["notes-app.exe"], false),
        Box::new(FakeSource(state.clone())),
    );

    let first = run(&mut core, 0, 0, 1000);
    assert!(
        matches!(first.as_slice(), [(_, DetectorEvent::GameStarted { exe, .. })] if exe == "notes-app.exe"),
        "pinned app should start first: {first:?}"
    );

    // Unpinned score-100 game appears on top, and becomes foreground.
    add_snap(&state, game_snap("game.exe", 1, 10));
    state.lock().unwrap().snapshots.rotate_right(1);
    state.lock().unwrap().foreground = Some(1);

    let later = run(&mut core, 1000, 10_000, 1000);
    assert!(
        later.is_empty(),
        "bound pinned target must not be displaced or stopped: {later:?}"
    );
}

/// 6. Runtime flip: an unpinned recording is released after the stop grace.
#[test]
fn runtime_flip_to_allowlist_releases_unpinned_recording_after_grace() {
    let state = make_state(vec![game_snap("game.exe", 1, 10)], None);
    let mut core = DetectorCore::new(cfg_with(&[], false), Box::new(FakeSource(state)));

    // Start: unpinned confirm is 3 s.
    let started = run(&mut core, 0, 3000, 1000);
    assert!(
        matches!(started.as_slice(), [(3000, DetectorEvent::GameStarted { exe, .. })] if exe == "game.exe"),
        "unpinned game should start at 3 s: {started:?}"
    );

    // Flip on at 4000 ms: release tick must emit nothing.
    core.set_allowlist_only(true);
    let release_tick = core.tick(4000);
    assert!(
        release_tick.is_empty(),
        "no GameStopped on the release tick (grace running): {release_tick:?}"
    );

    // Run to 15 s: exactly one GameStopped, 10 s after release, never a restart.
    let after = run(&mut core, 5000, 15_000, 1000);
    let stops: Vec<_> = after
        .iter()
        .filter(|(_, e)| matches!(e, DetectorEvent::GameStopped { .. }))
        .collect();
    assert_eq!(
        stops.len(),
        1,
        "expected exactly one GameStopped: {after:?}"
    );
    let (stop_t, stop_ev) = stops[0];
    assert_eq!(exe_of(stop_ev), "game.exe");
    assert!(
        *stop_t >= 4000 + 10_000 && *stop_t <= 4000 + 11_000,
        "stop must come after the 10 s grace from release (tick 4000), got {stop_t}"
    );
    assert!(
        after
            .iter()
            .all(|(_, e)| !matches!(e, DetectorEvent::GameStarted { .. })),
        "no GameStarted for the released game afterwards: {after:?}"
    );
}

/// 7. Allowlist mode with an unpinned game in the foreground: it still does not start.
#[test]
fn allowlist_mode_foreground_unpinned_game_does_not_start() {
    let state = make_state(vec![game_snap("game.exe", 1, 10)], Some(1));
    let mut core = DetectorCore::new(cfg_with(&[], true), Box::new(FakeSource(state)));

    let events = run(&mut core, 0, 10_000, 1000);
    assert!(
        events.is_empty(),
        "foreground unpinned game must not start in allowlist mode: {events:?}"
    );
}

/// 8. Extra: the built-in blocklist wins over the allowlist.
///
/// notepad.exe is blocklisted, so pinning it must not start a recording in
/// either mode.
#[test]
fn blocklisted_exe_is_not_selected_even_when_pinned() {
    for allowlist_only in [false, true] {
        let state = make_state(vec![pinned_app("notepad.exe", 2, 20, 60_000)], Some(2));
        let mut core = DetectorCore::new(
            cfg_with(&["notepad.exe"], allowlist_only),
            Box::new(FakeSource(state)),
        );
        let events = run(&mut core, 0, 5000, 1000);
        assert!(
            events.is_empty(),
            "blocklisted notepad.exe must not start (allowlist_only={allowlist_only}): {events:?}"
        );
    }
}
