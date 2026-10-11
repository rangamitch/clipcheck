//! Behaviour tests for issues #7 (process cache), #8 (scoring with prebuilt lists)
//! and #9 (DetectorCore selection). Public API only; a fake `SnapshotSource` drives
//! `DetectorCore::tick`. Expectations come from the doc comments and the design
//! doc (docs/design/01-game-detection.md 5.6, 5.11, 5.12), not from the code.
//!
//! Windows-only: the `win` module and `runner` module are compiled on Windows only.

#![cfg(windows)]

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipcheck_detect::lists::{normalize_exe, Lists};
use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
use clipcheck_detect::score::{evaluate, evaluate_with};
use clipcheck_detect::win::{enumerate_candidates, enumerate_candidates_with, ProcessCache};
use clipcheck_detect::{
    AllowRule, CandidateSnapshot, CaptureTarget, DetectConfig, GateReason, Reason, Rect, Verdict,
};

// ===========================================================================
// shared fixtures
// ===========================================================================

/// Fullscreen 1920x1080 window, visible, primary, no database match, no GPU.
/// Unknown-game score: geometry 20 + non-generic title 5 + large window 5 = 30.
fn game(exe: &str, title: &str) -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: exe.into(),
        window_title: title.into(),
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
        process_age_ms: 60_000,
    }
}

/// Same as `game` with a known-database match: 40 + 20 + 5 + 5 = 70 (accepted at 60).
fn known(exe: &str, title: &str) -> CandidateSnapshot {
    let mut s = game(exe, title);
    s.in_known_db = true;
    s
}

fn cfg() -> DetectConfig {
    DetectConfig::default()
}

fn gate_of(v: &Verdict) -> GateReason {
    match v {
        Verdict::Rejected(r) => *r,
        other => panic!("expected Rejected, got {other:?}"),
    }
}

fn scored_of(v: &Verdict) -> (u32, bool) {
    match v {
        Verdict::Scored {
            score, accepted, ..
        } => (*score, *accepted),
        other => panic!("expected Scored, got {other:?}"),
    }
}

fn breakdown_of(v: &Verdict) -> Vec<(Reason, i32)> {
    match v {
        Verdict::Scored { breakdown, .. } => breakdown.clone(),
        other => panic!("expected Scored, got {other:?}"),
    }
}

// ===========================================================================
// #7 process cache / enumeration
// ===========================================================================

fn key_sets(v: &[CandidateSnapshot]) -> (BTreeSet<String>, BTreeSet<u32>) {
    let exes = v.iter().map(|s| normalize_exe(&s.exe_name)).collect();
    let pids = v.iter().map(|s| s.target.pid).collect();
    (exes, pids)
}

/// |A and B| / |A or B|; two empty sets overlap fully.
fn overlap<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 1.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

/// Two enumerations are "the same candidate set" when exe names and pids overlap
/// at least 80 %. Windows may appear or disappear between calls.
fn assert_same_candidates(label: &str, a: &[CandidateSnapshot], b: &[CandidateSnapshot]) {
    let (ea, pa) = key_sets(a);
    let (eb, pb) = key_sets(b);
    let (oe, op) = (overlap(&ea, &eb), overlap(&pa, &pb));
    eprintln!(
        "[#7 {label}] candidates {} vs {}, exe overlap {:.2}, pid overlap {:.2}",
        a.len(),
        b.len(),
        oe,
        op
    );
    assert!(
        oe >= 0.8,
        "{label}: exe-name overlap {oe:.2} < 0.80\n a={ea:?}\n b={eb:?}"
    );
    assert!(
        op >= 0.8,
        "{label}: pid overlap {op:.2} < 0.80\n a={pa:?}\n b={pb:?}"
    );
}

#[test]
fn enumerating_twice_in_a_row_matches_plain_enumeration() {
    let plain = enumerate_candidates();
    let mut cache = ProcessCache::default();
    let first = enumerate_candidates_with(&mut cache);
    let second = enumerate_candidates_with(&mut cache);
    assert_same_candidates("plain vs cached #1", &plain, &first);
    assert_same_candidates("cached #1 vs cached #2", &first, &second);
    assert_same_candidates("plain vs cached #2", &plain, &second);
}

#[test]
fn fresh_cache_and_warm_cache_agree() {
    let mut fresh = ProcessCache::default();
    let fresh_result = enumerate_candidates_with(&mut fresh);

    let mut warm = ProcessCache::default();
    for _ in 0..3 {
        let _ = enumerate_candidates_with(&mut warm);
    }
    let warm_result = enumerate_candidates_with(&mut warm);
    assert_same_candidates("fresh vs warm", &fresh_result, &warm_result);
}

#[test]
fn caches_are_independent_per_instance() {
    let mut a = ProcessCache::default();
    let mut b = ProcessCache::default();
    let ra = enumerate_candidates_with(&mut a);
    let ra2 = enumerate_candidates_with(&mut a);
    // Using `a` must not change what `b` reports for the same moment.
    let rb = enumerate_candidates_with(&mut b);
    let plain = enumerate_candidates();
    assert_same_candidates("cache a vs cache a (2nd)", &ra, &ra2);
    assert_same_candidates("cache b vs plain", &rb, &plain);
    assert_same_candidates("cache a vs cache b", &ra2, &rb);
}

fn min_duration<F: FnMut()>(runs: usize, mut f: F) -> Duration {
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap_or_default()
}

#[test]
fn warm_enumeration_is_not_much_slower_than_cold() {
    // Minimum over 5 runs each, to damp scheduler noise. Cold = new cache per call.
    let cold = min_duration(5, || {
        let _ = enumerate_candidates_with(&mut ProcessCache::default());
    });
    let mut warm = ProcessCache::default();
    let _ = enumerate_candidates_with(&mut warm);
    let warm_t = min_duration(5, || {
        let _ = enumerate_candidates_with(&mut warm);
    });
    eprintln!(
        "[#7 timing] cold (fresh cache) min = {cold:?}, warm (reused cache) min = {warm_t:?}"
    );
    // Spec: not slower than 3x. A 1 ms absolute floor is added because the
    // whole enumeration can take well under a millisecond on an idle machine.
    assert!(
        warm_t <= cold * 3 + Duration::from_millis(1),
        "warm {warm_t:?} slower than 3x cold {cold:?} (+1ms floor)"
    );
}

// ===========================================================================
// #8 scoring with pre-built lists (gate table in docs/design/01-game-detection.md 5.11)
// ===========================================================================

#[test]
fn launchers_are_rejected_in_any_case_and_path() {
    let c = cfg();
    for exe in [
        "steam.exe",
        "STEAM.EXE",
        "Steam.Exe",
        "steamwebhelper.exe",
        "EpicGamesLauncher.exe",
        "epicgameslauncher.exe",
        "C:\\Program Files (x86)\\Steam\\steam.exe",
    ] {
        let v = evaluate(&known(exe, "Some Game"), &c);
        assert_eq!(gate_of(&v), GateReason::Launcher, "exe {exe:?}");
    }
}

#[test]
fn launcher_flag_rejects_any_exe() {
    let mut s = known("game.exe", "Some Game");
    s.is_launcher = true;
    assert_eq!(gate_of(&evaluate(&s, &cfg())), GateReason::Launcher);
}

#[test]
fn anticheat_helpers_are_never_candidates() {
    for exe in [
        "EasyAntiCheat.exe",
        "EASYANTICHEAT.EXE",
        "BEService.exe",
        "vgc.exe",
        "FACEIT.exe",
        "EOSOverlayRenderer-Win64-Shipping.exe",
    ] {
        let v = evaluate(&known(exe, "Some Game"), &cfg());
        assert_eq!(gate_of(&v), GateReason::AntiCheatService, "exe {exe:?}");
    }
}

#[test]
fn shells_and_blocklisted_apps_are_rejected() {
    let c = cfg();
    for exe in ["explorer.exe", "EXPLORER.EXE", "DWM.exe", "pwsh.exe"] {
        assert_eq!(
            gate_of(&evaluate(&known(exe, "Some Game"), &c)),
            GateReason::SystemShell,
            "exe {exe:?}"
        );
    }
    for exe in [
        "CHROME.EXE",
        "C:\\Program Files\\Google\\Chrome\\chrome.exe",
        "obs64.exe",
        "Discord.EXE",
        "nvidia share.exe",
        "clipcheckd.exe",
    ] {
        assert_eq!(
            gate_of(&evaluate(&known(exe, "Some Game"), &c)),
            GateReason::Blocklisted,
            "exe {exe:?}"
        );
    }
}

#[test]
fn block_extra_matches_case_insensitively() {
    let c = DetectConfig {
        block_extra: vec!["MyGame.exe".into()],
        ..DetectConfig::default()
    };
    assert_eq!(
        gate_of(&evaluate(&known("mygame.EXE", "Some Game"), &c)),
        GateReason::Blocklisted
    );
    assert_eq!(
        gate_of(&evaluate(&known("C:\\x\\MYGAME.exe", "Some Game"), &c)),
        GateReason::Blocklisted
    );
    assert!(matches!(
        evaluate(&known("othergame.exe", "Some Game"), &c),
        Verdict::Scored { .. }
    ));
}

#[test]
fn empty_exe_is_unknown_exe_before_any_other_gate() {
    let c = cfg();
    for exe in ["", "   ", "C:\\Games\\", "\\", "/"] {
        // Fully valid window otherwise, so only G0 can reject it.
        let v = evaluate(&known(exe, "Some Game"), &c);
        assert_eq!(gate_of(&v), GateReason::UnknownExe, "exe {exe:?}");
    }
    // Also first when the window would fail other gates.
    let mut bad = game("", "");
    bad.is_tool_window = true;
    bad.is_visible = false;
    assert_eq!(gate_of(&evaluate(&bad, &c)), GateReason::UnknownExe);
}

#[test]
fn window_visibility_and_tool_window_gates() {
    let c = cfg();
    let mut tool = known("game.exe", "Some Game");
    tool.is_tool_window = true;
    assert_eq!(gate_of(&evaluate(&tool, &c)), GateReason::ToolWindow);

    for title in ["", "   "] {
        let mut cloaked = known("game.exe", title);
        cloaked.is_cloaked = true;
        assert_eq!(gate_of(&evaluate(&cloaked, &c)), GateReason::Cloaked);
    }
    // Cloaked but titled: G3 does not apply.
    let mut titled = known("game.exe", "Some Game");
    titled.is_cloaked = true;
    assert!(matches!(evaluate(&titled, &c), Verdict::Scored { .. }));

    let mut hidden = known("game.exe", "Some Game");
    hidden.is_visible = false;
    assert_eq!(gate_of(&evaluate(&hidden, &c)), GateReason::NoVisibleWindow);

    let mut minimized = game("game.exe", "Some Game");
    minimized.is_visible = false;
    minimized.is_minimized = true;
    // G4 passes (minimized); the fullscreen geometry gives G8 evidence: 30 points, not accepted.
    assert_eq!(scored_of(&evaluate(&minimized, &c)), (30, false));
}

#[test]
fn minimum_size_gate_is_640_by_480() {
    let c = cfg();
    let mut narrow = known("game.exe", "Some Game");
    narrow.window_rect = Rect::from_size(0, 0, 639, 1080);
    assert_eq!(gate_of(&evaluate(&narrow, &c)), GateReason::TooSmall);

    let mut short = known("game.exe", "Some Game");
    short.window_rect = Rect::from_size(0, 0, 1920, 479);
    assert_eq!(gate_of(&evaluate(&short, &c)), GateReason::TooSmall);

    // 640x480 passes G5: known 40 + title 5 = 45, not large, not accepted.
    let mut exact = known("game.exe", "Some Game");
    exact.window_rect = Rect::from_size(0, 0, 640, 480);
    assert_eq!(scored_of(&evaluate(&exact, &c)), (45, false));
}

#[test]
fn generic_title_gate_respects_known_match() {
    let c = cfg();
    for title in [
        "",
        "Default IME",
        "MSCTFIME UI",
        "Program Manager",
        "  program manager  ",
    ] {
        let v = evaluate(&game("game.exe", title), &c);
        assert_eq!(gate_of(&v), GateReason::BadTitle, "title {title:?}");
    }
    // Known match lets a generic title through; no NonGenericTitle points: 40 + 20 + 5 = 65.
    let v = evaluate(&known("game.exe", "Program Manager"), &c);
    assert_eq!(scored_of(&v), (65, true));
    assert!(!breakdown_of(&v)
        .iter()
        .any(|(r, _)| *r == Reason::NonGenericTitle));
}

#[test]
fn primary_in_group_gate() {
    let mut s = known("game.exe", "Some Game");
    s.is_primary_in_group = false;
    assert_eq!(
        gate_of(&evaluate(&s, &cfg())),
        GateReason::NotPrimaryInGroup
    );
}

#[test]
fn graphics_evidence_gate_uses_threshold_inclusively() {
    let c = cfg();
    let windowed = |gpu: Option<f32>| {
        let mut s = game("game.exe", "Some Game");
        s.window_rect = Rect::from_size(100, 100, 800, 600);
        s.gpu_3d_load_pct = gpu;
        s
    };
    // No evidence at all.
    assert_eq!(
        gate_of(&evaluate(&windowed(None), &c)),
        GateReason::NoGraphicsEvidence
    );
    // 14.9 % is below the 15 % threshold: G8 still fails.
    assert_eq!(
        gate_of(&evaluate(&windowed(Some(14.9)), &c)),
        GateReason::NoGraphicsEvidence
    );
    // 15.0 % meets the threshold: G8 passes; +20 GPU, +5 title = 25, not accepted.
    assert_eq!(scored_of(&evaluate(&windowed(Some(15.0)), &c)), (25, false));
    // Threshold from config is used.
    let c10 = DetectConfig {
        gpu_threshold_pct: 10.0,
        ..DetectConfig::default()
    };
    assert_eq!(
        scored_of(&evaluate(&windowed(Some(12.0)), &c10)),
        (25, false)
    );
}

#[test]
fn near_fullscreen_only_without_caption() {
    let c = cfg();
    let mut s = game("game.exe", "Some Game");
    s.window_rect = Rect::from_size(10, 10, 1900, 1000); // 91.8 % of the monitor
                                                         // NearFullscreen +10, title +5, large window +5 = 20; G8 passes via NearFullscreen.
    let v = evaluate(&s, &c);
    assert_eq!(scored_of(&v), (20, false));
    assert!(breakdown_of(&v).contains(&(Reason::FullscreenNearFullscreen, 10)));

    s.has_caption = true;
    assert_eq!(gate_of(&evaluate(&s, &c)), GateReason::NoGraphicsEvidence);
}

#[test]
fn documented_worked_example_scores_65() {
    // docs 5.11: unknown fullscreen game, GPU 40 %, D3D11, foreground, 1920x1080, title.
    let mut s = game("game.exe", "Some Game");
    s.gpu_3d_load_pct = Some(40.0);
    s.loaded_graphics_modules = vec!["d3d11.dll".into()];
    s.ms_since_foreground = Some(0);
    let v = evaluate(&s, &cfg());
    assert_eq!(scored_of(&v), (65, true));
    let bd = breakdown_of(&v);
    assert!(bd.contains(&(Reason::FullscreenGeometry, 20)));
    assert!(bd.contains(&(Reason::GpuAboveThreshold, 20)));
    assert!(bd.contains(&(Reason::D3dModule, 10)));
    assert!(bd.contains(&(Reason::ForegroundRecent, 5)));
    assert!(bd.contains(&(Reason::LargeWindow, 5)));
    assert!(bd.contains(&(Reason::NonGenericTitle, 5)));
}

#[test]
fn running_penalties_and_installer_gate() {
    let c = cfg();
    // Unknown fullscreen, running, title with an installer keyword: 20+5+5-30 = 0 -> gate.
    for title in [
        "Game Setup",
        "GAME INSTALLER",
        "uninstall me",
        "Updater 2",
        "crash report",
    ] {
        let mut s = game("game.exe", title);
        s.is_running_game = true;
        assert_eq!(
            gate_of(&evaluate(&s, &c)),
            GateReason::InstallerTitle,
            "title {title:?}"
        );
    }
    // Same title, not running: no penalty, score 30, no gate.
    let s = game("game.exe", "Game Setup");
    assert_eq!(scored_of(&evaluate(&s, &c)), (30, false));

    // Known fullscreen, running, installer title: 70-30 = 40 (above the gate at 30).
    let mut known_setup = known("game.exe", "Game Setup");
    known_setup.is_running_game = true;
    assert_eq!(scored_of(&evaluate(&known_setup, &c)), (40, false));

    // "Launcher" in title while running: 70-15 = 55, not accepted. Not running: 70, accepted.
    let mut lt = known("game.exe", "Launcher Mode");
    lt.is_running_game = true;
    assert_eq!(scored_of(&evaluate(&lt, &c)), (55, false));
    lt.is_running_game = false;
    assert_eq!(scored_of(&evaluate(&lt, &c)), (70, true));

    // GPU averaged under 2 % over 60 s while running: 70-20 = 50.
    let mut idle = known("game.exe", "Some Game");
    idle.is_running_game = true;
    idle.gpu_3d_avg60_pct = Some(1.0);
    assert_eq!(scored_of(&evaluate(&idle, &c)), (50, false));
}

#[test]
fn allowlist_skips_gates_five_to_eight_only() {
    let c = DetectConfig {
        allow: vec![AllowRule {
            exe: "Game.EXE".into(),
            display_name: Some("Game".into()),
        }],
        ..DetectConfig::default()
    };
    // Small, untitled, unknown, not primary, no GPU: would fail G5-G8.
    let mut weak = game("game.exe", "");
    weak.window_rect = Rect::from_size(0, 0, 300, 200);
    weak.is_primary_in_group = false;
    assert_eq!(scored_of(&evaluate(&weak, &c)), (0, true));

    let mut weak_upper = weak.clone();
    weak_upper.exe_name = "GAME.exe".into();
    assert_eq!(scored_of(&evaluate(&weak_upper, &c)), (0, true));

    // G1-G4 still apply to allowlisted games.
    let mut invisible = weak.clone();
    invisible.is_visible = false;
    assert_eq!(
        gate_of(&evaluate(&invisible, &c)),
        GateReason::NoVisibleWindow
    );
    let mut tool = weak.clone();
    tool.is_tool_window = true;
    assert_eq!(gate_of(&evaluate(&tool, &c)), GateReason::ToolWindow);
    let mut cloaked = weak.clone();
    cloaked.is_cloaked = true;
    assert_eq!(gate_of(&evaluate(&cloaked, &c)), GateReason::Cloaked);
}

#[test]
fn allow_rule_cannot_override_blocklist_launcher_shell_or_anticheat() {
    let allow: Vec<AllowRule> = ["chrome.exe", "steam.exe", "explorer.exe", "vgc.exe", ""]
        .iter()
        .map(|e| AllowRule {
            exe: (*e).into(),
            display_name: None,
        })
        .collect();
    let c = DetectConfig {
        allow,
        ..DetectConfig::default()
    };
    assert_eq!(
        gate_of(&evaluate(&known("CHROME.EXE", "Some Game"), &c)),
        GateReason::Blocklisted
    );
    assert_eq!(
        gate_of(&evaluate(&known("steam.exe", "Some Game"), &c)),
        GateReason::Launcher
    );
    assert_eq!(
        gate_of(&evaluate(&known("explorer.exe", "Some Game"), &c)),
        GateReason::SystemShell
    );
    assert_eq!(
        gate_of(&evaluate(&known("vgc.exe", "Some Game"), &c)),
        GateReason::AntiCheatService
    );
}

#[test]
fn min_score_decides_acceptance_for_scored_candidates() {
    // Unknown fullscreen titled window scores 30.
    let s = game("game.exe", "Some Game");
    let c30 = DetectConfig {
        min_score: 30,
        ..DetectConfig::default()
    };
    let c31 = DetectConfig {
        min_score: 31,
        ..DetectConfig::default()
    };
    assert_eq!(scored_of(&evaluate(&s, &c30)), (30, true));
    assert_eq!(scored_of(&evaluate(&s, &c31)), (30, false));
}

#[test]
fn unicode_long_and_path_exe_names_match_the_plain_name() {
    let c = cfg();
    let plain = evaluate(&known("game.exe", "Some Game"), &c);
    assert_eq!(scored_of(&plain), (70, true));

    for exe in [
        "游戏.exe",
        "Ünïcödé Game.EXE",
        "C:\\Games\\Foo\\game.exe",
        "D:/Games/Foo/GAME.EXE",
        "game.exe  ",
        "\t game.exe",
    ] {
        let v = evaluate(&known(exe, "Some Game"), &c);
        assert_eq!(v, plain, "exe {exe:?} should score like game.exe");
    }

    let long = format!("{}.exe", "g".repeat(300));
    let long_v = evaluate(&known(&long, "Some Game"), &c);
    assert_eq!(scored_of(&long_v), (70, true));
    assert_eq!(long_v, plain, "300-char name should score like game.exe");

    // Long directory part, short launcher name: still a launcher.
    let dir_long = format!("{}\\steam.exe", "d".repeat(300));
    assert_eq!(
        gate_of(&evaluate(&known(&dir_long, "Some Game"), &c)),
        GateReason::Launcher
    );
    // Long file name that merely ends in a launcher name is not a launcher.
    let fake = format!("{}steam.exe", "x".repeat(300));
    assert!(matches!(
        evaluate(&known(&fake, "Some Game"), &c),
        Verdict::Scored { .. }
    ));
    // Long path ending in a blocklisted name is blocked.
    let blocked = format!("{}\\CHROME.exe", "x".repeat(300));
    assert_eq!(
        gate_of(&evaluate(&known(&blocked, "Some Game"), &c)),
        GateReason::Blocklisted
    );
}

#[test]
fn evaluate_and_evaluate_with_prebuilt_lists_agree() {
    let c = DetectConfig {
        block_extra: vec!["Extra.EXE".into()],
        allow: vec![AllowRule {
            exe: "Pinned.exe".into(),
            display_name: None,
        }],
        ..DetectConfig::default()
    };
    let lists = Lists::from_config(&c);
    for s in table_snapshots() {
        assert_eq!(
            evaluate(&s, &c),
            evaluate_with(&lists, &s, &c),
            "{}",
            s.exe_name
        );
    }
}

/// Mixed table used by the determinism and rebuild tests.
fn table_snapshots() -> Vec<CandidateSnapshot> {
    let mut v = vec![
        known("steam.exe", "Some Game"),
        known("STEAM.EXE", "Some Game"),
        known("EpicGamesLauncher.exe", "Some Game"),
        known("EasyAntiCheat.exe", "Some Game"),
        known("explorer.exe", "Some Game"),
        known("CHROME.EXE", "Some Game"),
        known("Extra.EXE", "Some Game"),
        known("", "Some Game"),
        known(&format!("{}.exe", "z".repeat(300)), "Some Game"),
        known("C:\\Games\\Foo\\Game.EXE", "Some Game"),
        known("游戏.exe", "Some Game"),
        game("game.exe", "Game Setup"),
        game("game.exe", "Program Manager"),
        game("pinned.EXE", ""),
        game("Pinned.exe", "Some Game"),
    ];
    let mut tiny = game("pinned.exe", "");
    tiny.window_rect = Rect::from_size(0, 0, 100, 100);
    v.push(tiny);
    v
}

#[test]
fn prebuilt_lists_give_identical_verdicts_over_10000_evaluations() {
    let c = DetectConfig {
        block_extra: vec!["Extra.EXE".into()],
        allow: vec![AllowRule {
            exe: "Pinned.exe".into(),
            display_name: None,
        }],
        ..DetectConfig::default()
    };
    let snaps = table_snapshots();
    let expected: Vec<Verdict> = snaps.iter().map(|s| evaluate(s, &c)).collect();
    let lists = Lists::from_config(&c);

    for i in 0..10_000usize {
        let idx = i % snaps.len();
        let v = evaluate_with(&lists, &snaps[idx], &c);
        assert_eq!(v, expected[idx], "evaluation {i} (snapshot {idx}) changed");
    }
}

#[test]
fn lists_built_twice_from_the_same_config_behave_the_same() {
    let c = DetectConfig {
        block_extra: vec!["Extra.EXE".into()],
        allow: vec![AllowRule {
            exe: "Pinned.exe".into(),
            display_name: Some("Pinned".into()),
        }],
        ..DetectConfig::default()
    };
    let a = Lists::from_config(&c);
    let b = Lists::from_config(&c);
    let via_new = Lists::new(&c.block_extra, &c.allow);

    let names = [
        "steam.exe",
        "STEAM.EXE",
        "chrome.exe",
        "extra.exe",
        "EXTRA.EXE",
        "pinned.exe",
        "PINNED.EXE",
        "explorer.exe",
        "vgc.exe",
        "game.exe",
        "",
    ];
    let probe = |l: &Lists, n: &str| {
        (
            l.is_blocked(n),
            l.is_system_shell(n),
            l.is_launcher(n),
            l.is_anticheat_service(n),
            l.is_allowlisted(n),
        )
    };
    for n in names {
        assert_eq!(probe(&a, n), probe(&b, n), "exe {n:?}");
        assert_eq!(probe(&a, n), probe(&via_new, n), "exe {n:?} via Lists::new");
    }
    for s in table_snapshots() {
        assert_eq!(evaluate_with(&a, &s, &c), evaluate_with(&b, &s, &c));
    }
    // Blocklist wins over the allow rule for a blocked exe, in both builds.
    assert!(!a.is_allowlisted("chrome.exe"));
    let allow_chrome = DetectConfig {
        allow: vec![AllowRule {
            exe: "chrome.exe".into(),
            display_name: None,
        }],
        ..DetectConfig::default()
    };
    assert!(!Lists::from_config(&allow_chrome).is_allowlisted("chrome.exe"));
}

// ===========================================================================
// #9 DetectorCore: fake source and helpers
// ===========================================================================

#[derive(Default)]
struct World {
    snapshots: Vec<CandidateSnapshot>,
    alive_hwnds: HashSet<u64>,
    alive_pids: HashSet<u32>,
    foreground: Option<u64>,
}

struct FakeSource(Arc<Mutex<World>>);

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

/// World where every given snapshot's window and process are alive.
fn world(snaps: Vec<CandidateSnapshot>, fg: Option<u64>) -> Arc<Mutex<World>> {
    let mut w = World {
        foreground: fg,
        ..Default::default()
    };
    for s in &snaps {
        w.alive_hwnds.insert(s.target.hwnd);
        w.alive_pids.insert(s.target.pid);
    }
    w.snapshots = snaps;
    Arc::new(Mutex::new(w))
}

/// Known-database fullscreen window: 70 points, plus the optional extras set by the caller.
fn scored_window(exe: &str, title: &str, hwnd: u64, pid: u32) -> CandidateSnapshot {
    let mut s = known(exe, title);
    s.target = CaptureTarget { pid, hwnd };
    s
}

fn core_with(cfg: DetectConfig, w: &Arc<Mutex<World>>) -> DetectorCore {
    DetectorCore::new(cfg, Box::new(FakeSource(w.clone())))
}

/// Runs ticks from `from` to `to` inclusive, every `step` ms, returning (time, event).
fn run_ticks(core: &mut DetectorCore, from: u64, to: u64, step: u64) -> Vec<(u64, DetectorEvent)> {
    let mut out = Vec::new();
    let mut t = from;
    while t <= to {
        out.extend(core.tick(t).into_iter().map(|e| (t, e)));
        t += step;
    }
    out
}

fn only(v: &[(u64, DetectorEvent)]) -> Vec<DetectorEvent> {
    v.iter().map(|(_, e)| e.clone()).collect()
}

// ===========================================================================
// #9 selection
// ===========================================================================

/// Five distinct games whose scores strictly increase in list order:
/// 70 (base), 75 (+recent fg), 80 (+d3d), 85 (+d3d +recent fg), 90 (+GPU 50 %).
fn ladder() -> Vec<CandidateSnapshot> {
    let base = |exe: &str, hwnd: u64| scored_window(exe, "Some Game", hwnd, 100 + hwnd as u32);
    let a = base("a.exe", 1);
    let mut b = base("b.exe", 2);
    b.ms_since_foreground = Some(0);
    let mut c = base("c.exe", 3);
    c.loaded_graphics_modules = vec!["d3d11.dll".into()];
    let mut d = base("d.exe", 4);
    d.loaded_graphics_modules = vec!["d3d11.dll".into()];
    d.ms_since_foreground = Some(0);
    let mut e = base("e.exe", 5);
    e.gpu_3d_load_pct = Some(50.0);
    vec![a, b, c, d, e]
}

#[test]
fn ladder_scores_are_what_the_test_assumes() {
    let scores: Vec<u32> = ladder()
        .iter()
        .map(|s| scored_of(&evaluate(s, &cfg())).0)
        .collect();
    assert_eq!(scores, vec![70, 75, 80, 85, 90]);
}

#[test]
fn best_of_several_improving_candidates_is_started_in_both_orders() {
    for reverse in [false, true] {
        let mut snaps = ladder();
        if reverse {
            snaps.reverse();
        }
        let w = world(snaps, None);
        let mut core = core_with(cfg(), &w);
        let evs = run_ticks(&mut core, 0, 3000, 500);
        let starts: Vec<&DetectorEvent> = evs
            .iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, DetectorEvent::GameStarted { .. }))
            .collect();
        assert_eq!(starts.len(), 1, "reverse={reverse}: {evs:?}");
        assert_eq!(
            starts[0],
            &DetectorEvent::GameStarted {
                hwnd: 5,
                exe: "e.exe".into(),
                title: "Some Game".into(),
                pid: Some(105),
            },
            "reverse={reverse}"
        );
        assert_eq!(evs[0].0, 3000, "start confirm is 3000 ms");
    }
}

#[test]
fn pinned_candidate_beats_higher_score_as_documented() {
    // docs 5.6: best = max(Pinned, score, ...). A weak allowlisted window
    // (score 5, small, untitled-free) must beat the 90-point game.
    let c = DetectConfig {
        allow: vec![AllowRule {
            exe: "pinned.exe".into(),
            display_name: None,
        }],
        ..DetectConfig::default()
    };
    let mut snaps = ladder();
    let mut pinned = game("pinned.exe", "Some Game");
    pinned.window_rect = Rect::from_size(0, 0, 300, 200);
    pinned.target = CaptureTarget { pid: 200, hwnd: 9 };
    snaps.push(pinned);
    let w = world(snaps, None);
    let mut core = core_with(c, &w);
    // Pinned process older than 5 s starts with no confirm delay.
    let evs = run_ticks(&mut core, 0, 0, 500);
    assert_eq!(
        only(&evs),
        vec![DetectorEvent::GameStarted {
            hwnd: 9,
            exe: "pinned.exe".into(),
            title: "Some Game".into(),
            pid: Some(200),
        }]
    );
}

#[test]
fn full_tie_keeps_the_topmost_window() {
    // Two games with score 70; list order is z-order (topmost first). The code
    // comment documents: "on a full tie the topmost window wins". Pids are chosen
    // ascending in z-order so the design doc's tie-break (-pid) agrees too.
    let snaps = vec![
        scored_window("t1.exe", "Some Game", 10, 100),
        scored_window("t2.exe", "Some Game", 11, 200),
    ];
    let w = world(snaps, None);
    let mut core = core_with(cfg(), &w);
    let evs = run_ticks(&mut core, 0, 3000, 500);
    assert_eq!(
        only(&evs),
        vec![DetectorEvent::GameStarted {
            hwnd: 10,
            exe: "t1.exe".into(),
            title: "Some Game".into(),
            pid: Some(100),
        }]
    );
}

#[test]
fn equal_score_challenger_does_not_displace_bound_window() {
    // docs 5.6 / 5.12: switch to another window of the same game only at +15 points.
    // The challenger is topmost with an equal score, so it is "best" but must not win.
    let bound = scored_window("game.exe", "Some Game", 1, 10);
    let challenger = scored_window("game.exe", "Some Game", 2, 10);
    let w = world(vec![challenger.clone(), bound.clone()], None);
    // Keep the bound window only in the first snapshot list so both stay alive.
    let mut core = core_with(cfg(), &w);
    // Bind the first window alone, then add the tie.
    w.lock().unwrap().snapshots = vec![bound.clone()];
    let started = run_ticks(&mut core, 0, 3000, 500);
    assert_eq!(started.len(), 1, "{started:?}");
    w.lock().unwrap().snapshots = vec![challenger.clone(), bound.clone()];
    let evs = run_ticks(&mut core, 3500, 60_000, 500);
    assert!(
        evs.is_empty(),
        "equal-score challenger displaced bound window: {evs:?}"
    );
}

#[test]
fn fifteen_point_challenger_retargets_after_two_seconds() {
    // Bound at 70. Challenger at 85 (+15) appears at 4000 and must be stable 2 s.
    let bound = scored_window("game.exe", "Some Game", 1, 10);
    let w = world(vec![bound.clone()], None);
    let mut core = core_with(cfg(), &w);
    assert_eq!(only(&run_ticks(&mut core, 0, 3000, 500)).len(), 1);

    let mut challenger = scored_window("game.exe", "Some Game", 2, 10);
    challenger.loaded_graphics_modules = vec!["d3d11.dll".into()];
    challenger.ms_since_foreground = Some(0);
    {
        let mut st = w.lock().unwrap();
        st.snapshots = vec![challenger.clone(), bound.clone()];
        st.alive_hwnds.insert(2);
    }
    let evs = run_ticks(&mut core, 3500, 7000, 500);
    let changed: Vec<(u64, DetectorEvent)> = evs
        .into_iter()
        .filter(|(_, e)| matches!(e, DetectorEvent::TargetChanged { .. }))
        .collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    // Challenger first seen at 3500; 2000 ms of stability ends at 5500.
    assert_eq!(changed[0].0, 5500, "switch after 2000 ms stable");
    assert_eq!(
        changed[0].1,
        DetectorEvent::TargetChanged {
            exe: "game.exe".into(),
            old_hwnd: 1,
            new_hwnd: 2,
            pid: Some(10),
        }
    );
}

// ===========================================================================
// #9 event payloads
// ===========================================================================

#[test]
fn started_event_uses_normalized_exe_and_window_title() {
    let w = world(
        vec![scored_window(
            "C:\\Games\\Foo\\Game.EXE",
            "Some Game",
            7,
            42,
        )],
        None,
    );
    let mut core = core_with(cfg(), &w);
    let evs = only(&run_ticks(&mut core, 0, 3000, 500));
    assert_eq!(
        evs,
        vec![DetectorEvent::GameStarted {
            hwnd: 7,
            exe: "game.exe".into(),
            title: "Some Game".into(),
            pid: Some(42),
        }]
    );
}

#[test]
fn empty_title_falls_back_to_exe_name_in_started_event() {
    // Doc comment on GameStarted.title: "or the exe name when the title is empty".
    // The raw exe name is used (not the normalized one): this is an ambiguity, see report.
    for title in ["", "   "] {
        let w = world(vec![scored_window("Game.EXE", title, 8, 43)], None);
        let mut core = core_with(cfg(), &w);
        let evs = only(&run_ticks(&mut core, 0, 3000, 500));
        match &evs[..] {
            [DetectorEvent::GameStarted {
                hwnd,
                exe,
                title: t,
                pid,
            }] => {
                assert_eq!(*hwnd, 8);
                assert_eq!(exe, "game.exe");
                assert!(t.eq_ignore_ascii_case("game.exe"), "title {t:?}");
                assert_eq!(*pid, Some(43));
            }
            other => panic!("title {title:?}: unexpected events {other:?}"),
        }
    }
}

#[test]
fn relaunch_and_stop_carry_exe_title_and_gap() {
    let first = scored_window("game.exe", "Some Game", 1, 10);
    let w = world(vec![first], None);
    let mut core = core_with(cfg(), &w);
    assert_eq!(only(&run_ticks(&mut core, 0, 3000, 500)).len(), 1);

    // Window destroyed at 4000, process alive: enters Ending with gap starting at 4000.
    {
        let mut st = w.lock().unwrap();
        st.snapshots.clear();
        st.alive_hwnds.clear();
    }
    assert!(core.tick(4000).is_empty());

    // The same game comes back as a new process at 4500 (500 ms after the loss,
    // inside the 1 s window-gone grace).
    {
        let mut st = w.lock().unwrap();
        st.snapshots = vec![scored_window("GAME.EXE", "Some Game", 7, 20)];
        st.alive_hwnds.insert(7);
        st.alive_pids.insert(20);
    }
    assert_eq!(
        core.tick(4500),
        vec![DetectorEvent::GameRelaunched {
            hwnd: 7,
            exe: "game.exe".into(),
            title: "Some Game".into(),
            pid: Some(20),
            gap_ms: 500,
        }]
    );
}

#[test]
fn stop_after_grace_carries_normalized_exe() {
    let w = world(vec![scored_window("Game.EXE", "Some Game", 1, 10)], None);
    let mut core = core_with(cfg(), &w);
    assert_eq!(only(&run_ticks(&mut core, 0, 3000, 500)).len(), 1);
    // Window gone, process alive: the window-gone grace (1 s) applies.
    {
        let mut st = w.lock().unwrap();
        st.snapshots.clear();
        st.alive_hwnds.clear();
    }
    let evs = run_ticks(&mut core, 3500, 14_500, 500);
    assert_eq!(
        only(&evs),
        vec![DetectorEvent::GameStopped {
            exe: "game.exe".into(),
        }],
        "stop 1 s after the loss at 3500"
    );
    assert_eq!(
        evs[0].0, 4500,
        "grace expires 1000 ms after the first missing tick"
    );
}

#[test]
fn window_destroyed_with_same_process_reports_target_changed_at_once() {
    let w = world(vec![scored_window("game.exe", "Some Game", 1, 10)], None);
    let mut core = core_with(cfg(), &w);
    assert_eq!(only(&run_ticks(&mut core, 0, 3000, 500)).len(), 1);
    {
        let mut st = w.lock().unwrap();
        st.alive_hwnds.remove(&1);
        st.snapshots = vec![scored_window("game.exe", "Some Game", 2, 10)];
        st.alive_hwnds.insert(2);
    }
    assert_eq!(
        core.tick(3500),
        vec![DetectorEvent::TargetChanged {
            exe: "game.exe".into(),
            old_hwnd: 1,
            new_hwnd: 2,
            pid: Some(10),
        }]
    );
}

// ===========================================================================
// #9 deterministic random ticks
// ===========================================================================

/// Simple LCG; deterministic across runs.
struct Lcg(u64);

impl Lcg {
    fn step(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.step() % n
    }
    /// True about 3 times in 4.
    fn likely(&mut self) -> bool {
        self.below(4) != 0
    }
}

const EXES: [&str; 4] = ["a.exe", "B.EXE", "c.exe", "Game D.exe"];
const TITLES: [&str; 4] = ["Some Game", "", "Game Setup", "Launcher Hub"];

fn random_snapshot(rng: &mut Lcg, hwnd: u64) -> CandidateSnapshot {
    let pid = 100 + hwnd as u32;
    let exe = EXES[(hwnd as usize) % EXES.len()];
    let mut s = scored_window(exe, TITLES[rng.below(4) as usize], hwnd, pid);
    s.in_known_db = rng.likely();
    s.is_visible = rng.likely();
    s.is_minimized = rng.below(5) == 0;
    s.is_tool_window = rng.below(12) == 0;
    s.is_cloaked = rng.below(12) == 0;
    s.is_primary_in_group = rng.likely();
    s.window_rect = match rng.below(3) {
        0 => Rect::from_size(0, 0, 1920, 1080),
        1 => Rect::from_size(0, 0, 800, 600),
        _ => Rect::from_size(10, 10, 1900, 1000),
    };
    s.has_caption = rng.below(5) == 0;
    s.gpu_3d_load_pct = if rng.likely() {
        Some(rng.below(60) as f32)
    } else {
        None
    };
    s.gpu_3d_avg60_pct = if rng.below(3) == 0 {
        Some(rng.below(10) as f32)
    } else {
        None
    };
    if rng.likely() {
        s.loaded_graphics_modules = vec!["d3d11.dll".into()];
    }
    s.ms_since_foreground = if rng.below(2) == 0 {
        Some(rng.below(20_000))
    } else {
        None
    };
    s.process_age_ms = rng.below(120_000);
    s
}

#[test]
fn random_ticks_never_panic_and_never_double_start() {
    let mut rng = Lcg(0x5EED_0007_0008_0009);
    let w = world(vec![], None);
    let mut core = core_with(cfg(), &w);

    let mut now: u64 = 0;
    let mut open: Option<String> = None;
    let (mut starts, mut stops, mut relaunches, mut retargets) = (0u32, 0u32, 0u32, 0u32);

    for tick in 0..200u32 {
        now += 100 + rng.below(900);
        {
            let mut st = w.lock().unwrap();
            // Mostly keep the same candidate set (so a game can stay best for the
            // 3 s start confirm); every few ticks regenerate it entirely.
            if rng.below(10) < 2 {
                st.snapshots.clear();
                st.alive_hwnds.clear();
                st.alive_pids.clear();
                let n = 1 + rng.below(4);
                let mut hwnds: Vec<u64> = (1..=6).collect();
                // Shuffle the window handles, then take the first n as this tick's candidates.
                for i in (1..hwnds.len()).rev() {
                    let j = rng.below((i + 1) as u64) as usize;
                    hwnds.swap(i, j);
                }
                for &h in hwnds.iter().take(n as usize) {
                    let snap = random_snapshot(&mut rng, h);
                    st.snapshots.push(snap);
                    st.alive_hwnds.insert(h);
                    st.alive_pids.insert(100 + h as u32);
                }
            } else {
                // Flip liveness of individual windows and processes now and then.
                let hwnds: Vec<u64> = st.snapshots.iter().map(|s| s.target.hwnd).collect();
                for h in hwnds {
                    if rng.below(15) == 0 {
                        if st.alive_hwnds.contains(&h) {
                            st.alive_hwnds.remove(&h);
                        } else {
                            st.alive_hwnds.insert(h);
                        }
                    }
                    if rng.below(40) == 0 {
                        let pid = 100 + h as u32;
                        if st.alive_pids.contains(&pid) {
                            st.alive_pids.remove(&pid);
                        } else {
                            st.alive_pids.insert(pid);
                        }
                    }
                }
            }
            st.foreground = if rng.below(3) == 0 {
                Some(1 + rng.below(6))
            } else {
                None
            };
        }

        for ev in core.tick(now) {
            match ev {
                DetectorEvent::GameStarted { exe, .. } => {
                    assert!(
                        open.is_none(),
                        "tick {tick}: GameStarted({exe}) while {open:?} still open"
                    );
                    open = Some(exe);
                    starts += 1;
                }
                DetectorEvent::GameStopped { exe } => {
                    assert_eq!(
                        open.as_deref(),
                        Some(exe.as_str()),
                        "tick {tick}: stop mismatch"
                    );
                    open = None;
                    stops += 1;
                }
                DetectorEvent::GameRelaunched { exe, .. } => {
                    assert_eq!(
                        open.as_deref(),
                        Some(exe.as_str()),
                        "tick {tick}: relaunch mismatch"
                    );
                    relaunches += 1;
                }
                DetectorEvent::TargetChanged { exe, .. } => {
                    assert_eq!(
                        open.as_deref(),
                        Some(exe.as_str()),
                        "tick {tick}: retarget mismatch"
                    );
                    retargets += 1;
                }
            }
        }
    }

    let tail = core.shutdown(now + 1);
    for ev in tail {
        if let DetectorEvent::GameStopped { exe } = ev {
            assert_eq!(
                open.as_deref(),
                Some(exe.as_str()),
                "shutdown stop mismatch"
            );
            open = None;
            stops += 1;
        }
    }
    assert!(open.is_none(), "a game was still open after shutdown");
    eprintln!(
        "[#9 random] 200 ticks: starts={starts} stops={stops} relaunches={relaunches} retargets={retargets}"
    );
    assert!(
        starts >= 1,
        "random run never started a game; the test exercised nothing"
    );
}
