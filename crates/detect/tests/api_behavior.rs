//! Behavioural tests for clipcheck-detect, written against the public API only.

use clipcheck_detect::debounce::Debouncer;
use clipcheck_detect::lists::{self, Lists, BUILTIN_BLOCKLIST, BUILTIN_LAUNCHERS, BUILTIN_SHELL};
use clipcheck_detect::score::fullscreen_kind;
use clipcheck_detect::{
    evaluate, evaluate_with, AllowRule, CandidateSnapshot, CaptureTarget, DetectConfig,
    DetectEvent, FullscreenKind, GateReason, Liveness, Pick, Reason, Rect, StartReason, StopReason,
    Verdict,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn base() -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: "game.exe".into(),
        window_title: "Some Game".into(),
        window_rect: Rect::from_size(0, 0, 1920, 1080),
        monitor_rect: Rect::from_size(0, 0, 1920, 1080),
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
        process_age_ms: 60000,
    }
}

/// Small, non-fullscreen window: no fullscreen evidence, not large.
fn small_window(mut s: CandidateSnapshot) -> CandidateSnapshot {
    s.window_rect = Rect::from_size(100, 100, 800, 600);
    s
}

fn cfg() -> DetectConfig {
    DetectConfig::default()
}

fn parts(v: &Verdict) -> (u32, Vec<(Reason, i32)>, bool) {
    match v {
        Verdict::Scored {
            score,
            breakdown,
            accepted,
        } => (*score, breakdown.clone(), *accepted),
        Verdict::Rejected(r) => panic!("expected Scored, got Rejected({:?})", r),
    }
}

fn bd(v: &Verdict) -> Vec<(Reason, i32)> {
    parts(v).1
}

fn value_of(v: &Verdict, r: Reason) -> Option<i32> {
    bd(v).iter().find(|(x, _)| *x == r).map(|(_, p)| *p)
}

fn count_of(v: &Verdict, r: Reason) -> usize {
    bd(v).iter().filter(|(x, _)| *x == r).count()
}

fn pick(game: &str, hwnd: u64) -> Pick {
    Pick {
        game: game.into(),
        display_name: game.into(),
        target: CaptureTarget { pid: 100, hwnd },
        reason: StartReason::Detected,
        pinned: false,
        process_age_ms: 60000,
    }
}

fn t(hwnd: u64) -> CaptureTarget {
    CaptureTarget { pid: 100, hwnd }
}

fn has_started(evs: &[DetectEvent], game: &str) -> bool {
    evs.iter()
        .any(|e| matches!(e, DetectEvent::GameStarted { game: g, .. } if g == game))
}

fn has_stopped(evs: &[DetectEvent], game: &str, reason: StopReason) -> bool {
    evs.iter().any(|e| {
        matches!(e, DetectEvent::GameStopped { game: g, reason: r, .. } if g == game && *r == reason)
    })
}

fn any_stopped(evs: &[DetectEvent]) -> bool {
    evs.iter()
        .any(|e| matches!(e, DetectEvent::GameStopped { .. }))
}

fn any_relaunched(evs: &[DetectEvent]) -> bool {
    evs.iter()
        .any(|e| matches!(e, DetectEvent::GameRelaunched { .. }))
}

/// Starts "game.exe" on hwnd 1 with the default 3000 ms confirm window.
fn started_debouncer(start_ms: u32, grace_ms: u32) -> Debouncer {
    let mut d = Debouncer::new(start_ms, grace_ms);
    d.update(0, Some(pick("game.exe", 1)), Liveness::Alive);
    let evs = d.update(start_ms as i64, Some(pick("game.exe", 1)), Liveness::Alive);
    assert!(
        has_started(&evs, "game.exe"),
        "setup: game should have started"
    );
    d
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// G1 table: one case per distinct rule (built-in blocklist, `block_extra`,
/// allow-rule override, system shell). Replaces the former pairs of
/// near-identical tests.
#[test]
fn gate_g1_blocklist_and_shell_table() {
    let allow = |exe: &str| {
        vec![AllowRule {
            exe: exe.into(),
            display_name: None,
        }]
    };
    let cases: Vec<(&str, String, DetectConfig, GateReason)> = vec![
        (
            "built-in blocklist",
            BUILTIN_BLOCKLIST[0].into(),
            cfg(),
            GateReason::Blocklisted,
        ),
        (
            "block_extra (case-insensitive)",
            "Blocked.EXE".into(),
            DetectConfig {
                block_extra: vec!["blocked.exe".into()],
                ..Default::default()
            },
            GateReason::Blocklisted,
        ),
        (
            "allow rule does not override built-in blocklist",
            BUILTIN_BLOCKLIST[0].into(),
            DetectConfig {
                allow: allow(BUILTIN_BLOCKLIST[0]),
                ..Default::default()
            },
            GateReason::Blocklisted,
        ),
        (
            "built-in system shell",
            BUILTIN_SHELL[0].into(),
            cfg(),
            GateReason::SystemShell,
        ),
    ];
    for (name, exe, c, expected) in cases {
        let mut s = base();
        s.exe_name = exe;
        assert_eq!(
            evaluate(&s, &c),
            Verdict::Rejected(expected),
            "case: {name}"
        );
    }
}

#[test]
fn gate_g2_launcher_by_name() {
    let mut s = base();
    s.exe_name = BUILTIN_LAUNCHERS[0].into();
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::Launcher)
    );
}

#[test]
fn gate_g2_launcher_flag() {
    let mut s = base();
    s.is_launcher = true;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::Launcher)
    );
}

#[test]
fn gate_g2_anticheat_service() {
    let mut s = base();
    s.exe_name = "EasyAntiCheat.exe".into();
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::AntiCheatService)
    );
}

#[test]
fn gate_g3_tool_window() {
    let mut s = base();
    s.is_tool_window = true;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::ToolWindow)
    );
}

#[test]
fn gate_g3_cloaked_empty_title() {
    let mut s = base();
    s.is_cloaked = true;
    s.window_title = String::new();
    assert_eq!(evaluate(&s, &cfg()), Verdict::Rejected(GateReason::Cloaked));
}

#[test]
fn gate_g4_not_visible_not_minimized() {
    let mut s = base();
    s.is_visible = false;
    s.is_minimized = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::NoVisibleWindow)
    );
}

#[test]
fn gate_g4_minimized_passes_visibility_gate() {
    let mut s = base();
    s.is_visible = false;
    s.is_minimized = true;
    let v = evaluate(&s, &cfg());
    assert!(
        !matches!(v, Verdict::Rejected(GateReason::NoVisibleWindow)),
        "minimized window should pass G4, got {:?}",
        v
    );
}

#[test]
fn gate_g5_too_small() {
    let mut s = base();
    s.window_rect = Rect::from_size(0, 0, 639, 1080);
    s.monitor_rect = s.window_rect;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::TooSmall)
    );
}

#[test]
fn gate_g5_exact_minimum_size_passes() {
    let mut s = base();
    s.window_rect = Rect::from_size(0, 0, 640, 480);
    s.monitor_rect = s.window_rect;
    let v = evaluate(&s, &cfg());
    assert!(
        !matches!(v, Verdict::Rejected(GateReason::TooSmall)),
        "640x480 should pass G5"
    );
}

#[test]
fn gate_g6_generic_titles_rejected() {
    for title in [
        "",
        "Default IME",
        "MSCTFIME UI",
        "Program Manager",
        "program manager  ",
    ] {
        let mut s = base();
        s.window_title = title.into();
        assert_eq!(
            evaluate(&s, &cfg()),
            Verdict::Rejected(GateReason::BadTitle),
            "title {:?} should be BadTitle",
            title
        );
    }
}

#[test]
fn gate_g6_empty_title_with_known_db_passes() {
    let mut s = base();
    s.window_title = String::new();
    s.in_known_db = true;
    let v = evaluate(&s, &cfg());
    assert!(
        !matches!(v, Verdict::Rejected(GateReason::BadTitle)),
        "known db should pass G6"
    );
    assert!(matches!(v, Verdict::Scored { .. }));
}

#[test]
fn gate_g7_not_primary_in_group() {
    let mut s = base();
    s.is_primary_in_group = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::NotPrimaryInGroup)
    );
}

#[test]
fn gate_g8_no_graphics_evidence() {
    let s = small_window(base());
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::NoGraphicsEvidence)
    );
}

#[test]
fn gate_g8_gpu_at_threshold_is_evidence() {
    let mut s = small_window(base());
    s.gpu_3d_load_pct = Some(15.0);
    let v = evaluate(&s, &cfg());
    assert!(
        matches!(v, Verdict::Scored { .. }),
        "gpu at threshold should be evidence, got {:?}",
        v
    );
}

#[test]
fn gate_g8_known_db_is_evidence() {
    let mut s = small_window(base());
    s.in_known_db = true;
    assert!(matches!(evaluate(&s, &cfg()), Verdict::Scored { .. }));
}

#[test]
fn gate_g8_library_is_evidence() {
    let mut s = small_window(base());
    s.in_library_scan = true;
    assert!(matches!(evaluate(&s, &cfg()), Verdict::Scored { .. }));
}

#[test]
fn gate_order_blocklist_beats_invisible() {
    let mut s = base();
    s.exe_name = BUILTIN_BLOCKLIST[0].into();
    s.is_visible = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::Blocklisted)
    );
}

#[test]
fn gate_order_tool_window_beats_invisible() {
    let mut s = base();
    s.is_tool_window = true;
    s.is_visible = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::ToolWindow)
    );
}

#[test]
fn gate_order_invisible_beats_too_small() {
    let mut s = base();
    s.is_visible = false;
    s.window_rect = Rect::from_size(0, 0, 100, 100);
    s.monitor_rect = s.window_rect;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::NoVisibleWindow)
    );
}

#[test]
fn gate_order_too_small_beats_bad_title() {
    let mut s = base();
    s.window_rect = Rect::from_size(0, 0, 100, 100);
    s.monitor_rect = s.window_rect;
    s.window_title = "Program Manager".into();
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::TooSmall)
    );
}

#[test]
fn gate_order_bad_title_beats_not_primary() {
    let mut s = base();
    s.window_title = "Program Manager".into();
    s.is_primary_in_group = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::BadTitle)
    );
}

#[test]
fn gate_order_not_primary_beats_no_evidence() {
    let mut s = small_window(base());
    s.is_primary_in_group = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::NotPrimaryInGroup)
    );
}

#[test]
fn gate_order_anticheat_beats_visibility() {
    let mut s = base();
    s.exe_name = "EasyAntiCheat.exe".into();
    s.is_visible = false;
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::AntiCheatService)
    );
}

#[test]
fn base_snapshot_is_scored_and_accepted() {
    // Passes every gate (not Rejected); acceptance then follows score >= min_score.
    let v = evaluate(&base(), &cfg());
    let (score, _, accepted) = parts(&v);
    assert_eq!(accepted, score >= cfg().min_score as u32, "score {}", score);
}

// ---------------------------------------------------------------------------
// Name matching, blocklist vs allowlist
// ---------------------------------------------------------------------------

#[test]
fn exe_match_is_case_and_directory_insensitive() {
    let mut s = small_window(base());
    s.exe_name = "C:\\Games\\GAME.EXE".into();
    // Allowlist should match the normalised name, so the tiny window passes.
    let c = DetectConfig {
        allow: vec![AllowRule {
            exe: "game.exe".into(),
            display_name: None,
        }],
        ..Default::default()
    };
    assert!(matches!(
        evaluate(&s, &c),
        Verdict::Scored { accepted: true, .. }
    ));
}

#[test]
fn allowlisted_skips_size_primary_title_and_evidence_gates() {
    let mut s = small_window(base());
    s.window_rect = Rect::from_size(0, 0, 100, 100);
    s.monitor_rect = Rect::from_size(0, 0, 1920, 1080);
    s.is_primary_in_group = false;
    s.window_title = String::new();
    let c = DetectConfig {
        allow: vec![AllowRule {
            exe: "game.exe".into(),
            display_name: None,
        }],
        ..Default::default()
    };
    let v = evaluate(&s, &c);
    assert!(
        matches!(v, Verdict::Scored { accepted: true, .. }),
        "got {:?}",
        v
    );
}

#[test]
fn allowlisted_still_subject_to_visibility_gate() {
    let mut s = base();
    s.is_visible = false;
    let c = DetectConfig {
        allow: vec![AllowRule {
            exe: "game.exe".into(),
            display_name: None,
        }],
        ..Default::default()
    };
    assert_eq!(
        evaluate(&s, &c),
        Verdict::Rejected(GateReason::NoVisibleWindow)
    );
}

// ---------------------------------------------------------------------------
// Score, accept threshold
// ---------------------------------------------------------------------------

#[test]
fn accepted_is_score_at_least_min_score() {
    let mut s = base();
    s.is_running_game = false;
    s.gpu_3d_load_pct = Some(50.0);
    for min in [0u8, 30, 60, 95, 100] {
        let c = DetectConfig {
            min_score: min,
            ..Default::default()
        };
        let (score, _, accepted) = parts(&evaluate(&s, &c));
        assert_eq!(
            accepted,
            score >= min as u32,
            "min_score={} score={}",
            min,
            score
        );
    }
}

#[test]
fn accepted_at_exact_min_score_boundary() {
    let s = base();
    let (score, _, _) = parts(&evaluate(&s, &cfg()));
    assert!(
        score < 100,
        "need headroom for boundary test, score={}",
        score
    );
    let at = DetectConfig {
        min_score: score as u8,
        ..Default::default()
    };
    let above = DetectConfig {
        min_score: score as u8 + 1,
        ..Default::default()
    };
    assert!(matches!(
        evaluate(&s, &at),
        Verdict::Scored { accepted: true, .. }
    ));
    assert!(matches!(
        evaluate(&s, &above),
        Verdict::Scored {
            accepted: false,
            ..
        }
    ));
}

#[test]
fn score_clamped_to_100_for_fully_loaded_game() {
    let mut s = base();
    s.in_known_db = true;
    s.in_library_scan = true;
    s.gpu_3d_load_pct = Some(80.0);
    s.gpu_3d_avg60_pct = Some(50.0);
    s.loaded_graphics_modules = vec![
        "d3d11.dll".into(),
        "vulkan-1.dll".into(),
        "opengl32.dll".into(),
    ];
    s.ms_since_foreground = Some(0);
    s.window_rect = Rect::from_size(0, 0, 1920, 1080);
    let v = evaluate(&s, &cfg());
    let (score, breakdown, accepted) = parts(&v);
    assert_eq!(score, 100);
    assert!(accepted);
    let raw: i64 = breakdown.iter().map(|(_, p)| *p as i64).sum();
    assert!(
        raw >= 100,
        "raw breakdown sum {} should be >= clamped score",
        raw
    );
    for r in [
        Reason::KnownDb,
        Reason::LibraryMatch,
        Reason::FullscreenGeometry,
        Reason::GpuAboveThreshold,
        Reason::D3dModule,
        Reason::VulkanModule,
        Reason::OpenGlModule,
        Reason::ForegroundRecent,
        Reason::LargeWindow,
    ] {
        assert!(
            count_of(&v, r) == 1,
            "expected exactly one {:?} in breakdown: {:?}",
            r,
            breakdown
        );
    }
}

#[test]
fn breakdown_has_no_signals_that_did_not_fire() {
    let v = evaluate(&base(), &cfg());
    for r in [
        Reason::KnownDb,
        Reason::LibraryMatch,
        Reason::FullscreenNearFullscreen,
        Reason::GpuAboveThreshold,
        Reason::GpuLowLoad,
        Reason::D3dModule,
        Reason::VulkanModule,
        Reason::OpenGlModule,
        Reason::ForegroundRecent,
        Reason::InstallerTitlePenalty,
        Reason::LauncherTitlePenalty,
        Reason::GpuIdlePenalty,
    ] {
        assert_eq!(
            count_of(&v, r),
            0,
            "{:?} should not fire for base snapshot: {:?}",
            r,
            bd(&v)
        );
    }
    assert!(value_of(&v, Reason::FullscreenGeometry).is_some());
    assert_eq!(value_of(&v, Reason::LargeWindow), Some(5));
}

#[test]
fn evaluate_with_matches_evaluate() {
    let c = DetectConfig {
        allow: vec![AllowRule {
            exe: "x.exe".into(),
            display_name: None,
        }],
        block_extra: vec!["y.exe".into()],
        ..Default::default()
    };
    let lists = Lists::from_config(&c);
    let s = base();
    assert_eq!(evaluate(&s, &c), evaluate_with(&lists, &s, &c));
}

// ---------------------------------------------------------------------------
// Running-only penalties
// ---------------------------------------------------------------------------

#[test]
fn installer_title_penalty_applies_when_running() {
    for title in [
        "Setup",
        "Installer",
        "Uninstall",
        "Updater",
        "Crash",
        "my SETUP wizard",
        "game crash log",
    ] {
        let mut s = base();
        s.is_running_game = true;
        s.window_title = title.into();
        let v = evaluate(&s, &cfg());
        // Penalty may cause InstallerTitle rejection; otherwise it must be recorded.
        match &v {
            Verdict::Rejected(GateReason::InstallerTitle) => {}
            Verdict::Scored { .. } => assert_eq!(
                value_of(&v, Reason::InstallerTitlePenalty),
                Some(-30),
                "title {:?}: {:?}",
                title,
                bd(&v)
            ),
            other => panic!("title {:?}: unexpected {:?}", title, other),
        }
    }
}

#[test]
fn installer_title_penalty_not_applied_when_not_running() {
    let mut s = base();
    s.window_title = "Setup".into();
    s.is_running_game = false;
    let v = evaluate(&s, &cfg());
    assert_eq!(count_of(&v, Reason::InstallerTitlePenalty), 0);
    assert!(matches!(v, Verdict::Scored { .. }));
}

#[test]
fn installer_title_low_score_rejected() {
    let mut s = small_window(base());
    s.is_running_game = true;
    s.window_title = "Setup".into();
    s.gpu_3d_load_pct = Some(50.0); // evidence + +20, penalty -30 => <= 30
    assert_eq!(
        evaluate(&s, &cfg()),
        Verdict::Rejected(GateReason::InstallerTitle)
    );
}

#[test]
fn installer_title_high_score_penalised_but_scored() {
    let mut s = base();
    s.is_running_game = true;
    s.window_title = "Setup".into();
    s.in_known_db = true;
    s.in_library_scan = true;
    s.gpu_3d_load_pct = Some(50.0);
    s.loaded_graphics_modules = vec!["d3d11.dll".into()];
    s.ms_since_foreground = Some(0);
    let v = evaluate(&s, &cfg());
    assert_eq!(value_of(&v, Reason::InstallerTitlePenalty), Some(-30));
    assert!(matches!(v, Verdict::Scored { .. }), "got {:?}", v);
}

#[test]
fn launcher_title_penalty_when_running() {
    let mut s = base();
    s.is_running_game = true;
    s.window_title = "Launcher".into();
    let v = evaluate(&s, &cfg());
    assert_eq!(value_of(&v, Reason::LauncherTitlePenalty), Some(-15));
    let mut s2 = base();
    s2.window_title = "Launcher".into();
    assert_eq!(
        count_of(&evaluate(&s2, &cfg()), Reason::LauncherTitlePenalty),
        0
    );
}

#[test]
fn gpu_idle_penalty_when_running() {
    let mut s = base();
    s.is_running_game = true;
    s.gpu_3d_avg60_pct = Some(1.0);
    let v = evaluate(&s, &cfg());
    assert_eq!(value_of(&v, Reason::GpuIdlePenalty), Some(-20));

    let mut s2 = base();
    s2.is_running_game = false;
    s2.gpu_3d_avg60_pct = Some(1.0);
    assert_eq!(count_of(&evaluate(&s2, &cfg()), Reason::GpuIdlePenalty), 0);
}

// ---------------------------------------------------------------------------
// GPU scoring
// ---------------------------------------------------------------------------

#[test]
fn gpu_above_threshold_plus_20() {
    let mut s = base();
    s.gpu_3d_load_pct = Some(50.0);
    assert_eq!(
        value_of(&evaluate(&s, &cfg()), Reason::GpuAboveThreshold),
        Some(20)
    );
}

#[test]
fn gpu_exactly_at_threshold_gets_full_points() {
    let mut s = base();
    s.gpu_3d_load_pct = Some(15.0);
    let v = evaluate(&s, &cfg());
    assert_eq!(value_of(&v, Reason::GpuAboveThreshold), Some(20));
    assert_eq!(count_of(&v, Reason::GpuLowLoad), 0);
}

#[test]
fn gpu_low_load_plus_5() {
    let mut s = base();
    s.gpu_3d_load_pct = Some(10.0);
    let v = evaluate(&s, &cfg());
    assert_eq!(value_of(&v, Reason::GpuLowLoad), Some(5));
    assert_eq!(count_of(&v, Reason::GpuAboveThreshold), 0);
}

#[test]
fn gpu_exactly_two_gives_plus_5() {
    let mut s = base();
    s.gpu_3d_load_pct = Some(2.0);
    assert_eq!(value_of(&evaluate(&s, &cfg()), Reason::GpuLowLoad), Some(5));
}

#[test]
fn gpu_below_two_gives_nothing() {
    let mut s = base();
    s.gpu_3d_load_pct = Some(1.9);
    let v = evaluate(&s, &cfg());
    assert_eq!(count_of(&v, Reason::GpuLowLoad), 0);
    assert_eq!(count_of(&v, Reason::GpuAboveThreshold), 0);
}

#[test]
fn gpu_none_gives_nothing() {
    let v = evaluate(&base(), &cfg());
    assert_eq!(count_of(&v, Reason::GpuLowLoad), 0);
    assert_eq!(count_of(&v, Reason::GpuAboveThreshold), 0);
}

// ---------------------------------------------------------------------------
// Graphics modules
// ---------------------------------------------------------------------------

#[test]
fn d3d_module_plus_10_counted_once() {
    let mut s = base();
    s.loaded_graphics_modules = vec!["D3D11.DLL".into(), "dxgi.dll".into(), "d3d12.dll".into()];
    let v = evaluate(&s, &cfg());
    assert_eq!(count_of(&v, Reason::D3dModule), 1);
    assert_eq!(value_of(&v, Reason::D3dModule), Some(10));
}

#[test]
fn vulkan_module_plus_10() {
    let mut s = base();
    s.loaded_graphics_modules = vec!["Vulkan-1.DLL".into()];
    assert_eq!(
        value_of(&evaluate(&s, &cfg()), Reason::VulkanModule),
        Some(10)
    );
}

#[test]
fn opengl_module_plus_5() {
    let mut s = base();
    s.loaded_graphics_modules = vec!["OPENGL32.dll".into()];
    assert_eq!(
        value_of(&evaluate(&s, &cfg()), Reason::OpenGlModule),
        Some(5)
    );
}

// ---------------------------------------------------------------------------
// Fullscreen, foreground, size
// ---------------------------------------------------------------------------

#[test]
fn fullscreen_kind_exact_geometry() {
    let m = Rect::from_size(0, 0, 1920, 1080);
    assert_eq!(fullscreen_kind(&m, &m, false, 2), FullscreenKind::Geometry);
}

#[test]
fn fullscreen_kind_off_by_one_within_tolerance() {
    let m = Rect::from_size(0, 0, 1920, 1080);
    let w = Rect::from_size(0, 0, 1919, 1079);
    assert_eq!(fullscreen_kind(&w, &m, false, 2), FullscreenKind::Geometry);
}

#[test]
fn fullscreen_kind_near_fullscreen_no_caption() {
    let m = Rect::from_size(0, 0, 1920, 1080);
    let w = Rect::from_size(0, 0, 1900, 1000); // off by >2 px, ~92% area
    assert_eq!(
        fullscreen_kind(&w, &m, false, 2),
        FullscreenKind::NearFullscreen
    );
}

#[test]
fn fullscreen_kind_caption_disqualifies_near_fullscreen() {
    let m = Rect::from_size(0, 0, 1920, 1080);
    let w = Rect::from_size(0, 0, 1900, 1000);
    assert_eq!(fullscreen_kind(&w, &m, true, 2), FullscreenKind::None);
}

#[test]
fn fullscreen_kind_unrelated_window_is_none() {
    let m = Rect::from_size(0, 0, 1920, 1080);
    let w = Rect::from_size(100, 100, 800, 600);
    assert_eq!(fullscreen_kind(&w, &m, false, 2), FullscreenKind::None);
}

#[test]
fn fullscreen_geometry_and_near_reasons_in_breakdown() {
    let v = evaluate(&base(), &cfg());
    assert_eq!(count_of(&v, Reason::FullscreenGeometry), 1);
    assert_eq!(count_of(&v, Reason::FullscreenNearFullscreen), 0);

    let mut s = base();
    s.window_rect = Rect::from_size(0, 0, 1900, 1000);
    let v2 = evaluate(&s, &cfg());
    assert_eq!(count_of(&v2, Reason::FullscreenNearFullscreen), 1);
    assert_eq!(count_of(&v2, Reason::FullscreenGeometry), 0);
}

#[test]
fn foreground_within_10s_inclusive_plus_5() {
    let mut s = base();
    s.ms_since_foreground = Some(10000);
    assert_eq!(
        value_of(&evaluate(&s, &cfg()), Reason::ForegroundRecent),
        Some(5)
    );
}

#[test]
fn foreground_just_outside_10s_no_points() {
    let mut s = base();
    s.ms_since_foreground = Some(10001);
    assert_eq!(count_of(&evaluate(&s, &cfg()), Reason::ForegroundRecent), 0);
}

#[test]
fn foreground_none_no_points() {
    assert_eq!(
        count_of(&evaluate(&base(), &cfg()), Reason::ForegroundRecent),
        0
    );
}

#[test]
fn large_window_1280x720_plus_5() {
    let mut s = base();
    s.window_rect = Rect::from_size(0, 0, 1280, 720);
    s.monitor_rect = s.window_rect;
    assert_eq!(
        value_of(&evaluate(&s, &cfg()), Reason::LargeWindow),
        Some(5)
    );
}

#[test]
fn just_under_large_window_no_points() {
    let mut s = base();
    s.window_rect = Rect::from_size(0, 0, 1279, 720);
    s.monitor_rect = s.window_rect;
    assert_eq!(count_of(&evaluate(&s, &cfg()), Reason::LargeWindow), 0);
}

#[test]
fn known_and_library_signals_appear_in_breakdown() {
    let mut s = base();
    s.in_known_db = true;
    s.in_library_scan = true;
    let v = evaluate(&s, &cfg());
    assert_eq!(count_of(&v, Reason::KnownDb), 1);
    assert_eq!(count_of(&v, Reason::LibraryMatch), 1);
}

#[test]
fn negative_rect_does_not_panic() {
    let r = Rect::from_size(0, 0, -100, -100);
    assert_eq!(r.width(), 0);
    assert_eq!(r.height(), 0);
    assert_eq!(r.area(), 0);
    let mut s = base();
    s.window_rect = r;
    let _ = evaluate(&s, &cfg());
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[test]
fn normalize_exe_strips_dir_and_lowercases() {
    assert_eq!(lists::normalize_exe("C:\\Games\\Foo.EXE"), "foo.exe");
    assert_eq!(lists::normalize_exe("foo.exe"), "foo.exe");
}

#[test]
fn generic_title_case_insensitive_and_trimmed() {
    assert!(lists::is_generic_title("  program manager "));
    assert!(lists::is_generic_title("default ime"));
    assert!(lists::is_generic_title("MSCTFIME UI"));
    assert!(lists::is_generic_title(""));
    assert!(!lists::is_generic_title("Some Game"));
}

#[test]
fn anticheat_service_detected() {
    let l = Lists::new(&[], &[]);
    assert!(l.is_anticheat_service("EasyAntiCheat.exe"));
    assert!(l.is_anticheat_service("easyanticheat.EXE"));
    assert!(!l.is_anticheat_service("game.exe"));
}

#[test]
fn is_allowlisted_false_when_blocked() {
    let l = Lists::new(
        &["x.exe".to_string()],
        &[AllowRule {
            exe: "x.exe".into(),
            display_name: None,
        }],
    );
    assert!(!l.is_allowlisted("x.exe"));
    let l2 = Lists::new(
        &[],
        &[AllowRule {
            exe: "Game.exe".into(),
            display_name: None,
        }],
    );
    assert!(l2.is_allowlisted("D:\\games\\GAME.exe"));
}

#[test]
fn is_blocked_matches_block_extra_case_insensitively() {
    let l = Lists::new(&["Blocked.EXE".to_string()], &[]);
    assert!(l.is_blocked("C:\\x\\blocked.exe"));
}

// ---------------------------------------------------------------------------
// Debouncer: start
// ---------------------------------------------------------------------------

#[test]
fn start_after_exactly_confirm_window() {
    let mut d = Debouncer::new(3000, 10000);
    assert!(d
        .update(0, Some(pick("game.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(d
        .update(2999, Some(pick("game.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(!d.is_active());
    let evs = d.update(3000, Some(pick("game.exe", 1)), Liveness::Alive);
    assert!(has_started(&evs, "game.exe"), "events: {:?}", evs);
    assert!(d.is_active());
    assert_eq!(d.active_target(), Some(t(1)));
}

#[test]
fn start_event_carries_pick_fields() {
    let mut d = Debouncer::new(0, 10000);
    let evs = d.update(0, Some(pick("game.exe", 1)), Liveness::Alive);
    match evs.as_slice() {
        [DetectEvent::GameStarted {
            game,
            target,
            reason,
            ..
        }] => {
            assert_eq!(game, "game.exe");
            assert_eq!(*target, t(1));
            assert_eq!(*reason, StartReason::Detected);
        }
        other => panic!("expected single GameStarted, got {:?}", other),
    }
}

#[test]
fn zero_confirm_starts_on_first_update() {
    let mut d = Debouncer::new(0, 10000);
    let evs = d.update(0, Some(pick("game.exe", 1)), Liveness::Alive);
    assert!(has_started(&evs, "game.exe"));
}

#[test]
fn pinned_old_process_starts_immediately() {
    let mut d = Debouncer::new(3000, 10000);
    let mut p = pick("game.exe", 1);
    p.pinned = true;
    p.process_age_ms = 5001;
    let evs = d.update(0, Some(p), Liveness::Alive);
    assert!(has_started(&evs, "game.exe"), "events: {:?}", evs);
}

#[test]
fn pinned_young_process_uses_normal_confirm() {
    let mut d = Debouncer::new(3000, 10000);
    let mut p = pick("game.exe", 1);
    p.pinned = true;
    p.process_age_ms = 5000;
    assert!(d.update(0, Some(p.clone()), Liveness::Alive).is_empty());
    assert!(d.update(2999, Some(p.clone()), Liveness::Alive).is_empty());
    assert!(has_started(
        &d.update(3000, Some(p), Liveness::Alive),
        "game.exe"
    ));
}

#[test]
fn pending_restarts_when_best_game_changes() {
    let mut d = Debouncer::new(3000, 10000);
    assert!(d
        .update(0, Some(pick("a.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(d
        .update(2500, Some(pick("b.exe", 2)), Liveness::Alive)
        .is_empty());
    // If the B pending timer had not restarted, B would start at 3000.
    assert!(d
        .update(3000, Some(pick("b.exe", 2)), Liveness::Alive)
        .is_empty());
    assert!(d
        .update(5499, Some(pick("b.exe", 2)), Liveness::Alive)
        .is_empty());
    let evs = d.update(5500, Some(pick("b.exe", 2)), Liveness::Alive);
    assert!(has_started(&evs, "b.exe"), "events: {:?}", evs);
    assert!(!has_started(&evs, "a.exe"));
}

#[test]
fn pending_cancelled_when_best_becomes_none() {
    let mut d = Debouncer::new(3000, 10000);
    assert!(d
        .update(0, Some(pick("a.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(d.update(1000, None, Liveness::Alive).is_empty());
    // Without cancellation a.exe would have started at 3000.
    assert!(d
        .update(3000, Some(pick("a.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(d
        .update(5999, Some(pick("a.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(has_started(
        &d.update(6000, Some(pick("a.exe", 1)), Liveness::Alive),
        "a.exe"
    ));
}

#[test]
fn zero_grace_stops_on_next_update_without_candidate() {
    let mut d = started_debouncer(3000, 0);
    let mut evs = d.update(10000, None, Liveness::ProcessExited);
    evs.extend(d.update(10001, None, Liveness::ProcessExited));
    assert!(
        has_stopped(&evs, "game.exe", StopReason::ProcessExited),
        "events: {:?}",
        evs
    );
    assert!(!d.is_active());
}

#[test]
fn backwards_time_does_not_panic() {
    let mut d = Debouncer::new(3000, 10000);
    assert!(d
        .update(10000, Some(pick("game.exe", 1)), Liveness::Alive)
        .is_empty());
    let evs = d.update(9000, Some(pick("game.exe", 1)), Liveness::Alive);
    assert!(
        !has_started(&evs, "game.exe"),
        "backwards time should not start early"
    );
    let evs2 = d.update(13000, Some(pick("game.exe", 1)), Liveness::Alive);
    assert!(has_started(&evs2, "game.exe"));
    let _ = d.update(4000, None, Liveness::ProcessExited);
}

// ---------------------------------------------------------------------------
// Debouncer: stop / grace / relaunch
// ---------------------------------------------------------------------------

#[test]
fn grace_relaunch_same_game_reports_gap_no_stop() {
    // A destroyed window waits the 1 s window-gone grace (capped by stop_grace_ms).
    let mut d = started_debouncer(3000, 10000);
    let evs = d.update(10000, None, Liveness::WindowDestroyed);
    assert!(!any_stopped(&evs), "should not stop yet: {:?}", evs);
    let evs2 = d.update(10500, Some(pick("game.exe", 5)), Liveness::WindowDestroyed);
    match evs2.as_slice() {
        [DetectEvent::GameRelaunched {
            game,
            target,
            gap_ms,
            ..
        }] => {
            assert_eq!(game, "game.exe");
            assert_eq!(*target, t(5));
            assert_eq!(*gap_ms, 500);
        }
        other => panic!("expected single GameRelaunched, got {:?}", other),
    }
    assert!(!any_stopped(&evs2));
    assert!(d.is_active());
}

#[test]
fn process_exit_stops_at_once_with_process_exited_reason() {
    let mut d = started_debouncer(3000, 10000);
    let evs = d.update(10000, None, Liveness::ProcessExited);
    assert!(
        has_stopped(&evs, "game.exe", StopReason::ProcessExited),
        "events: {:?}",
        evs
    );
    assert!(!d.is_active());
    assert_eq!(d.active_target(), None);
}

#[test]
fn grace_expiry_stops_with_window_destroyed_reason() {
    let mut d = started_debouncer(3000, 10000);
    d.update(10000, None, Liveness::WindowDestroyed);
    let evs = d.update(20001, None, Liveness::WindowDestroyed);
    assert!(
        has_stopped(&evs, "game.exe", StopReason::WindowDestroyed),
        "events: {:?}",
        evs
    );
}

#[test]
fn different_game_during_grace_is_not_relaunch() {
    let mut d = started_debouncer(3000, 10000);
    d.update(10000, None, Liveness::WindowDestroyed);
    let evs = d.update(10500, Some(pick("other.exe", 9)), Liveness::WindowDestroyed);
    assert!(
        !any_relaunched(&evs),
        "different game must not relaunch: {:?}",
        evs
    );
    let evs2 = d.update(11000, Some(pick("other.exe", 9)), Liveness::WindowDestroyed);
    assert!(
        has_stopped(&evs2, "game.exe", StopReason::WindowDestroyed),
        "original game should stop after the window-gone grace: {:?}",
        evs2
    );
}

#[test]
fn explicit_stop_mode_off_emits_stopped() {
    let mut d = started_debouncer(3000, 10000);
    let evs = d.stop(20000, StopReason::ModeOff);
    assert_eq!(
        evs,
        vec![DetectEvent::GameStopped {
            game: "game.exe".into(),
            reason: StopReason::ModeOff,
            at_ms: 20000
        }]
    );
    assert!(!d.is_active());
    assert_eq!(d.active_target(), None);
}

#[test]
fn stop_while_idle_emits_nothing() {
    let mut d = Debouncer::new(3000, 10000);
    assert!(d.stop(0, StopReason::ModeOff).is_empty());
    assert!(!d.is_active());
}

#[test]
fn alt_tab_alive_target_no_candidate_keeps_game() {
    let mut d = started_debouncer(3000, 10000);
    assert!(!any_stopped(&d.update(10000, None, Liveness::Alive)));
    assert!(!any_stopped(&d.update(60000, None, Liveness::Alive)));
    assert!(d.is_active());
    assert_eq!(d.active_target(), Some(t(1)));
}

// ---------------------------------------------------------------------------
// Debouncer: retarget
// ---------------------------------------------------------------------------

#[test]
fn retarget_fires_only_after_2000ms_stable() {
    let mut d = started_debouncer(3000, 10000);
    assert!(d
        .update(10000, Some(pick("game.exe", 2)), Liveness::Alive)
        .is_empty());
    assert!(d
        .update(11999, Some(pick("game.exe", 2)), Liveness::Alive)
        .is_empty());
    let evs = d.update(12000, Some(pick("game.exe", 2)), Liveness::Alive);
    match evs.as_slice() {
        [DetectEvent::TargetChanged { game, old, new, .. }] => {
            assert_eq!(game, "game.exe");
            assert_eq!(*old, t(1));
            assert_eq!(*new, t(2));
        }
        other => panic!("expected TargetChanged, got {:?}", other),
    }
    assert_eq!(d.active_target(), Some(t(2)));
}

#[test]
fn retarget_flip_back_resets_timer() {
    let mut d = started_debouncer(3000, 10000);
    d.update(10000, Some(pick("game.exe", 2)), Liveness::Alive);
    d.update(11000, Some(pick("game.exe", 1)), Liveness::Alive); // flip back
    assert!(d
        .update(12000, Some(pick("game.exe", 2)), Liveness::Alive)
        .is_empty());
    assert!(d
        .update(13999, Some(pick("game.exe", 2)), Liveness::Alive)
        .is_empty());
    let evs = d.update(14000, Some(pick("game.exe", 2)), Liveness::Alive);
    assert!(
        evs.iter()
            .any(|e| matches!(e, DetectEvent::TargetChanged { .. })),
        "events: {:?}",
        evs
    );
}

#[test]
fn dead_target_same_game_new_window_retargets_immediately() {
    let mut d = started_debouncer(3000, 10000);
    let evs = d.update(5000, Some(pick("game.exe", 2)), Liveness::WindowDestroyed);
    assert!(
        evs.iter()
            .any(|e| matches!(e, DetectEvent::TargetChanged { new, .. } if *new == t(2))),
        "expected immediate TargetChanged, got {:?}",
        evs
    );
    assert_eq!(d.active_target(), Some(t(2)));
}

// ---------------------------------------------------------------------------
// Debouncer: misc
// ---------------------------------------------------------------------------

#[test]
fn from_config_uses_config_timings() {
    let c = DetectConfig {
        start_confirm_ms: 1000,
        stop_grace_ms: 500,
        ..Default::default()
    };
    let mut d = Debouncer::from_config(&c);
    assert!(d
        .update(0, Some(pick("game.exe", 1)), Liveness::Alive)
        .is_empty());
    assert!(has_started(
        &d.update(1000, Some(pick("game.exe", 1)), Liveness::Alive),
        "game.exe"
    ));
    // A destroyed window waits min(stop_grace_ms, window-gone grace) = 500 ms.
    d.update(2000, None, Liveness::WindowDestroyed);
    assert!(has_stopped(
        &d.update(2501, None, Liveness::WindowDestroyed),
        "game.exe",
        StopReason::WindowDestroyed
    ));
}

#[test]
fn is_active_and_active_target_reflect_state() {
    let mut d = Debouncer::new(0, 10000);
    assert!(!d.is_active());
    assert_eq!(d.active_target(), None);
    d.update(0, Some(pick("game.exe", 7)), Liveness::Alive);
    assert!(d.is_active());
    assert_eq!(d.active_target(), Some(t(7)));
}
