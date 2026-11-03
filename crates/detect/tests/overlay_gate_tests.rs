//! Overlay gate (G3b): click-through layered windows are never games unless allowlisted.
//! Public API only. Helpers are copied from overlay_repro_tests.rs and fivem_tests.rs.

use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
use clipcheck_detect::score::{evaluate_with, GateReason, Verdict};
use clipcheck_detect::{
    known_games, AllowRule, CandidateSnapshot, CaptureTarget, DetectConfig, Lists, Rect,
};

const FIVEM_EXE: &str = "FiveM_b3258_GTAProcess.exe";
const OVERLAY_EXE: &str = "CrosshairX.exe";
const FIVEM_TITLE: &str = "FiveM\u{ae} by Cfx.re - Example Server";

fn base(exe: &str, title: &str, rect: Rect, hwnd: u64) -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: exe.into(),
        window_title: title.into(),
        window_rect: rect,
        monitor_rect: Rect::from_size(0, 0, 1920, 1080),
        target: CaptureTarget {
            pid: hwnd as u32 * 10,
            hwnd,
        },
        is_visible: true,
        is_minimized: false,
        is_cloaked: false,
        is_tool_window: false,
        is_overlay: false,
        has_caption: false,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: known_games::is_known(exe).is_some(),
        in_library_scan: false,
        gpu_3d_load_pct: Some(60.0),
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: vec!["d3d11.dll".into()],
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms: 60_000,
    }
}

/// A perfect fullscreen known game: fullscreen geometry, known db, high GPU, graphics module.
fn fullscreen_fivem(hwnd: u64) -> CandidateSnapshot {
    base(
        FIVEM_EXE,
        FIVEM_TITLE,
        Rect::from_size(0, 0, 1920, 1080),
        hwnd,
    )
}

fn overlay_of(mut s: CandidateSnapshot) -> CandidateSnapshot {
    s.is_overlay = true;
    s
}

fn cfg() -> DetectConfig {
    DetectConfig::default()
}

fn cfg_allowing(exe: &str) -> DetectConfig {
    DetectConfig {
        allow: vec![AllowRule {
            exe: exe.into(),
            display_name: None,
        }],
        ..DetectConfig::default()
    }
}

fn eval(snap: &CandidateSnapshot, c: &DetectConfig) -> Verdict {
    evaluate_with(&Lists::from_config(c), snap, c)
}

// ---------------------------------------------------------------------------
// a-d: verdicts
// ---------------------------------------------------------------------------

#[test]
fn a_overlay_fullscreen_known_game_is_rejected_as_overlay() {
    // Control: the same snapshot without the overlay flag is accepted.
    let plain = fullscreen_fivem(1);
    match eval(&plain, &cfg()) {
        Verdict::Scored { accepted: true, .. } => {}
        other => panic!("control (not overlay) should be accepted, got {other:?}"),
    }

    let snap = overlay_of(fullscreen_fivem(1));
    assert_eq!(
        eval(&snap, &cfg()),
        Verdict::Rejected(GateReason::Overlay),
        "overlay fullscreen known game must be rejected"
    );
}

#[test]
fn b_overlay_is_scored_when_its_exe_is_allowlisted() {
    let snap = overlay_of(base(
        OVERLAY_EXE,
        "CrosshairX",
        Rect::from_size(0, 0, 1920, 1080),
        5,
    ));
    // Without the allowlist entry it is an overlay rejection.
    assert_eq!(eval(&snap, &cfg()), Verdict::Rejected(GateReason::Overlay));
    // With the allowlist entry it is not rejected as an overlay.
    let verdict = eval(&snap, &cfg_allowing(OVERLAY_EXE));
    assert!(
        !matches!(verdict, Verdict::Rejected(GateReason::Overlay)),
        "allowlisted overlay must not be rejected as Overlay, got {verdict:?}"
    );
    assert!(
        matches!(verdict, Verdict::Scored { .. }),
        "allowlisted overlay must be Scored, got {verdict:?}"
    );
}

#[test]
fn b2_allowlisted_overlay_match_is_case_insensitive() {
    let snap = overlay_of(base(
        OVERLAY_EXE,
        "CrosshairX",
        Rect::from_size(0, 0, 1920, 1080),
        5,
    ));
    let verdict = eval(&snap, &cfg_allowing("crosshairx.EXE"));
    assert!(
        !matches!(verdict, Verdict::Rejected(GateReason::Overlay)),
        "allowlist match should ignore case, got {verdict:?}"
    );
}

#[test]
fn c_layered_only_fullscreen_window_is_not_an_overlay() {
    // is_overlay is the layered AND click-through result; a plain layered window
    // (is_overlay = false) must still be accepted.
    let snap = fullscreen_fivem(2);
    assert!(!snap.is_overlay);
    match eval(&snap, &cfg()) {
        Verdict::Scored { accepted, .. } => {
            assert!(
                accepted,
                "layered-only fullscreen known game must be accepted"
            )
        }
        other => panic!("layered-only window must be Scored, got {other:?}"),
    }
}

#[test]
fn d_overlay_that_is_also_a_tool_window_is_rejected_as_tool_window() {
    let mut snap = overlay_of(fullscreen_fivem(3));
    snap.is_tool_window = true;
    assert_eq!(
        eval(&snap, &cfg()),
        Verdict::Rejected(GateReason::ToolWindow),
        "tool-window gate must run before the overlay gate"
    );
}

// ---------------------------------------------------------------------------
// e-f: DetectorCore scenes
// ---------------------------------------------------------------------------

/// Fixed scene: the same snapshots are returned on every scan.
struct Scene {
    snaps: Vec<CandidateSnapshot>,
}

impl SnapshotSource for Scene {
    fn snapshots(&mut self) -> Vec<CandidateSnapshot> {
        self.snaps.clone()
    }
    fn is_window_alive(&mut self, hwnd: u64) -> bool {
        self.snaps.iter().any(|s| s.target.hwnd == hwnd)
    }
    fn foreground(&mut self) -> Option<u64> {
        None
    }
}

/// Runs 40 ticks (20 s at 500 ms) and returns every GameStarted exe.
fn started_exes(cfg: DetectConfig, snaps: Vec<CandidateSnapshot>) -> Vec<String> {
    let mut core = DetectorCore::new(cfg, Box::new(Scene { snaps }));
    (0..40u64)
        .flat_map(|i| core.tick(i * 500))
        .filter_map(|e| match e {
            DetectorEvent::GameStarted { exe, .. } => Some(exe),
            _ => None,
        })
        .collect()
}

fn windowed_fivem(hwnd: u64) -> CandidateSnapshot {
    let mut s = base(
        FIVEM_EXE,
        FIVEM_TITLE,
        Rect::from_size(100, 100, 1280, 720),
        hwnd,
    );
    s.has_caption = true;
    s
}

#[test]
fn e_steady_gpu_overlay_never_starts_and_windowed_fivem_does() {
    let mut overlay = overlay_of(base(
        OVERLAY_EXE,
        "__CROSSHAIR_OVERLAY",
        Rect::from_size(0, 0, 1920, 1080),
        20,
    ));
    overlay.gpu_3d_load_pct = Some(40.0);
    let started = started_exes(cfg(), vec![overlay, windowed_fivem(10)]);
    assert!(
        started.iter().all(|e| !e.eq_ignore_ascii_case(OVERLAY_EXE)),
        "overlay exe must never start, started: {started:?}"
    );
    assert!(
        started.iter().any(|e| e.eq_ignore_ascii_case(FIVEM_EXE)),
        "FiveM never started, started: {started:?}"
    );
    for e in &started {
        assert_eq!(e, &e.to_lowercase(), "GameStarted exe must be lowercase");
    }
}

#[test]
fn f_allowlisted_overlay_as_only_window_can_start() {
    let mut overlay = overlay_of(base(
        OVERLAY_EXE,
        "CrosshairX",
        Rect::from_size(0, 0, 1920, 1080),
        20,
    ));
    overlay.gpu_3d_load_pct = Some(40.0);
    let started = started_exes(cfg_allowing(OVERLAY_EXE), vec![overlay]);
    assert!(
        started.iter().any(|e| e.eq_ignore_ascii_case(OVERLAY_EXE)),
        "allowlisted overlay (only window) never started, started: {started:?}"
    );
}
