//! Repro for the "Not detecting my game" tester report: windowed FiveM next to a
//! fullscreen, always-on-top crosshair overlay. Public API only.

use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
use clipcheck_detect::{known_games, CandidateSnapshot, CaptureTarget, DetectConfig, Rect};

const FIVEM_EXE: &str = "FiveM_b3258_GTAProcess.exe";
const OVERLAY_EXE: &str = "CrosshairX.exe";
const FIVEM_HWND: u64 = 10;
const OVERLAY_HWND: u64 = 20;

fn snap(exe: &str, title: &str, hwnd: u64, rect: Rect, caption: bool) -> CandidateSnapshot {
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
        has_caption: caption,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: known_games::is_known(exe).is_some(),
        in_library_scan: false,
        gpu_3d_load_pct: None,
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: vec!["d3d11.dll".into()],
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms: 60_000,
    }
}

/// Overlay is topmost; its GPU load toggles each tick when `flicker` is set.
struct Scene {
    tick: u32,
    flicker: bool,
    overlay_gpu: Option<f32>,
}

impl SnapshotSource for Scene {
    fn snapshots(&mut self) -> Vec<CandidateSnapshot> {
        self.tick += 1;
        let mut overlay = snap(
            OVERLAY_EXE,
            "__CROSSHAIR_OVERLAY",
            OVERLAY_HWND,
            Rect::from_size(0, 0, 1920, 1080),
            false,
        );
        overlay.is_overlay = true;
        overlay.gpu_3d_load_pct = if self.flicker {
            Some(if self.tick.is_multiple_of(2) {
                20.0
            } else {
                0.0
            })
        } else {
            self.overlay_gpu
        };
        let fivem = snap(
            FIVEM_EXE,
            "FiveM\u{ae} by Cfx.re - Apollo Networks | Australia",
            FIVEM_HWND,
            Rect::from_size(100, 100, 1280, 720),
            true,
        );
        vec![overlay, fivem]
    }
    fn is_window_alive(&mut self, _hwnd: u64) -> bool {
        true
    }
    fn foreground(&mut self) -> Option<u64> {
        None
    }
}

fn started_exes(scene: Scene) -> Vec<String> {
    let mut core = DetectorCore::new(DetectConfig::default(), Box::new(scene));
    (0..40u64)
        .flat_map(|i| core.tick(i * 500))
        .filter_map(|e| match e {
            DetectorEvent::GameStarted { exe, .. } => Some(exe),
            _ => None,
        })
        .collect()
}

#[test]
fn fivem_starts_when_a_fullscreen_overlay_has_gpu_load() {
    let started = started_exes(Scene {
        tick: 0,
        flicker: false,
        overlay_gpu: Some(20.0),
    });
    assert!(
        started.iter().any(|e| e.eq_ignore_ascii_case(FIVEM_EXE)),
        "FiveM never started, started: {started:?}"
    );
}

#[test]
fn fivem_starts_when_overlay_gpu_load_flickers() {
    let started = started_exes(Scene {
        tick: 0,
        flicker: true,
        overlay_gpu: None,
    });
    assert!(
        started.iter().any(|e| e.eq_ignore_ascii_case(FIVEM_EXE)),
        "FiveM never started, started: {started:?}"
    );
}

#[test]
fn fivem_starts_when_overlay_is_idle() {
    let started = started_exes(Scene {
        tick: 0,
        flicker: false,
        overlay_gpu: None,
    });
    assert!(
        started.iter().any(|e| e.eq_ignore_ascii_case(FIVEM_EXE)),
        "FiveM never started, started: {started:?}"
    );
}
