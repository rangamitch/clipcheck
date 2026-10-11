//! FiveM / RedM / GTA V detection and the wildcard name patterns behind it.
//! Written against the public API only.

use clipcheck_detect::known_games;
use clipcheck_detect::lists::{glob_match, Lists};
use clipcheck_detect::score::{evaluate_with, GateReason, Reason, Verdict};
use clipcheck_detect::{AllowRule, CandidateSnapshot, CaptureTarget, DetectConfig, Rect};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

const FIVEM_TITLE: &str = "FiveM\u{ae} by Cfx.re - Example Server";

/// A windowed 1280x720 window on a 1920x1080 monitor: no fullscreen geometry.
fn windowed(exe: &str, title: &str) -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: exe.into(),
        window_title: title.into(),
        window_rect: Rect::from_size(100, 100, 1280, 720),
        monitor_rect: Rect::from_size(0, 0, 1920, 1080),
        target: CaptureTarget {
            pid: 4242,
            hwnd: 77,
        },
        is_visible: true,
        is_minimized: false,
        is_cloaked: false,
        is_tool_window: false,
        has_caption: true,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: known_games::is_known(exe).is_some(),
        in_library_scan: false,
        gpu_3d_load_pct: None,
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: Vec::new(),
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms: 60_000,
    }
}

fn cfg() -> DetectConfig {
    DetectConfig::default()
}

fn scored(v: Verdict) -> (u32, Vec<(Reason, i32)>, bool) {
    match v {
        Verdict::Scored {
            score,
            breakdown,
            accepted,
        } => (score, breakdown, accepted),
        other => panic!("expected Scored, got {other:?}"),
    }
}

fn evaluate_one(snap: &CandidateSnapshot, c: &DetectConfig) -> Verdict {
    evaluate_with(&Lists::from_config(c), snap, c)
}

const BUILD_NUMBERED: &[&str] = &[
    "FiveM_b2802_GTAProcess.exe",
    "FiveM_b3095_GTAProcess.exe",
    "FiveM_b3258_GTAProcess.exe",
    "FiveM_GTAProcess.exe",
    "FiveM_GameProcess.exe",
    "FiveM_b2802_GameProcess.exe",
];

// ---------------------------------------------------------------------------
// known list: build-numbered FiveM processes
// ---------------------------------------------------------------------------

#[test]
fn build_numbered_fivem_processes_match_the_known_list() {
    for exe in BUILD_NUMBERED {
        assert!(
            known_games::is_known(exe).is_some(),
            "{exe} should be a known FiveM process"
        );
    }
}

#[test]
fn fivem_processes_have_display_name_fivem_in_any_case_or_path() {
    for exe in BUILD_NUMBERED {
        assert_eq!(known_games::is_known(exe), Some("FiveM"), "{exe}");
        assert_eq!(
            known_games::is_known(&exe.to_ascii_uppercase()),
            Some("FiveM"),
            "upper-case {exe}"
        );
        let full = format!("C:\\Users\\me\\AppData\\Local\\FiveM\\FiveM.app\\{exe}");
        assert_eq!(
            known_games::is_known(&full),
            Some("FiveM"),
            "full path {exe}"
        );
    }
}

#[test]
fn redm_game_process_matches_as_redm() {
    assert_eq!(
        known_games::is_known("RedM_b1491_GameProcess.exe"),
        Some("RedM")
    );
    assert_eq!(known_games::is_known("RedM_GameProcess.exe"), Some("RedM"));
}

#[test]
fn gta_v_builds_are_known() {
    assert_eq!(
        known_games::is_known("GTA5.exe"),
        Some("Grand Theft Auto V")
    );
    assert_eq!(
        known_games::is_known("GTA5_Enhanced.exe"),
        Some("Grand Theft Auto V Enhanced")
    );
}

#[test]
fn unrelated_names_do_not_match_fivem_patterns() {
    for exe in [
        "FiveM_ROSLauncher.exe",
        "FiveMLauncher.exe",
        "FiveM_b2802_GTAProcess.exe.bak",
        "NotFiveM_b2802_GTAProcess.exe",
        "GTAProcess.exe",
        "fivem",
    ] {
        assert_eq!(known_games::is_known(exe), None, "{exe} must not match");
    }
}

#[test]
fn launcher_ui_exe_is_not_a_known_game() {
    // FiveM.exe is the Chromium launcher UI. It gets no known-list credit, so it is
    // scored like any unknown window and cannot be recorded on its own.
    assert_eq!(known_games::is_known("FiveM.exe"), None);
}

#[test]
fn rockstar_launcher_is_a_builtin_launcher_not_a_candidate() {
    let lists = Lists::from_config(&cfg());
    assert!(lists.is_launcher("PlayGTAV.exe"));
    assert!(lists.is_launcher("playgtav.EXE"));
    assert!(!lists.is_launcher("GTA5.exe"));
    let mut snap = windowed("PlayGTAV.exe", "Grand Theft Auto V");
    snap.in_known_db = false;
    assert_eq!(
        evaluate_one(&snap, &cfg()),
        Verdict::Rejected(GateReason::Launcher)
    );
}

#[test]
fn fivem_build_processes_are_not_blocked_or_launchers() {
    let lists = Lists::from_config(&cfg());
    for exe in BUILD_NUMBERED {
        assert!(!lists.is_blocked(exe), "{exe} must not be blocked");
        assert!(!lists.is_launcher(exe), "{exe} must not be a launcher");
        assert!(!lists.is_system_shell(exe), "{exe} must not be a shell");
        assert!(
            !lists.is_anticheat_service(exe),
            "{exe} must not be anti-cheat"
        );
    }
}

// ---------------------------------------------------------------------------
// score gate: known-list hit without fullscreen
// ---------------------------------------------------------------------------

#[test]
fn known_windowed_fivem_passes_the_gate_without_fullscreen() {
    let snap = windowed("FiveM_b2802_GTAProcess.exe", FIVEM_TITLE);
    let (score, breakdown, accepted) = scored(evaluate_one(&snap, &cfg()));
    assert!(accepted, "a known game with a 720p window must be accepted");
    assert!(!breakdown.iter().any(|(r, _)| matches!(
        r,
        Reason::FullscreenGeometry | Reason::FullscreenNearFullscreen
    )));
    // 40 known + 5 large window + 5 non-generic title, below min_score on its own.
    assert_eq!(score, 50);
    assert!(score < u32::from(cfg().min_score));
}

#[test]
fn windowed_known_game_below_720p_is_not_accepted() {
    let mut snap = windowed("FiveM_b2802_GTAProcess.exe", FIVEM_TITLE);
    snap.window_rect = Rect::from_size(100, 100, 1000, 600);
    let (score, _, accepted) = scored(evaluate_one(&snap, &cfg()));
    assert_eq!(score, 45);
    assert!(!accepted);
}

#[test]
fn known_game_with_generic_title_still_needs_the_score() {
    let mut snap = windowed("FiveM_b2802_GTAProcess.exe", "");
    snap.window_rect = Rect::from_size(0, 0, 1600, 900);
    let (_, _, accepted) = scored(evaluate_one(&snap, &cfg()));
    assert!(
        !accepted,
        "a menu window with no title is not a known window"
    );
}

#[test]
fn known_window_rule_does_not_bypass_the_blocklist() {
    let mut snap = windowed("chrome.exe", "YouTube");
    snap.in_known_db = true;
    assert_eq!(
        evaluate_one(&snap, &cfg()),
        Verdict::Rejected(GateReason::Blocklisted)
    );
}

#[test]
fn launcher_ui_windowed_without_gpu_evidence_is_rejected() {
    // FiveM.exe window: unknown, windowed, no GPU load. No graphics evidence, so G8 rejects it.
    let snap = windowed("FiveM.exe", FIVEM_TITLE);
    assert_eq!(
        evaluate_one(&snap, &cfg()),
        Verdict::Rejected(GateReason::NoGraphicsEvidence)
    );
}

#[test]
fn launcher_ui_never_outscores_the_known_game_window() {
    // Same geometry and title: the game process gets the known-list points, the
    // launcher UI does not.
    let game = scored(evaluate_one(
        &windowed("FiveM_b2802_GTAProcess.exe", FIVEM_TITLE),
        &cfg(),
    ));
    let mut ui = windowed("FiveM.exe", FIVEM_TITLE);
    ui.gpu_3d_load_pct = Some(3.0);
    let ui = evaluate_one(&ui, &cfg());
    assert!(game.0 > 0);
    if let Verdict::Scored { score, .. } = ui {
        assert!(score < game.0, "launcher {score} vs game {}", game.0);
    }
}

#[test]
fn known_game_windowed_with_gpu_load_is_accepted_at_any_build() {
    let mut snap = windowed("FiveM_b3258_GTAProcess.exe", FIVEM_TITLE);
    snap.gpu_3d_load_pct = Some(60.0);
    snap.loaded_graphics_modules = vec!["d3d11.dll".into()];
    snap.ms_since_foreground = Some(1_000);
    let (score, _, accepted) = scored(evaluate_one(&snap, &cfg()));
    assert!(accepted);
    assert!(score >= 60);
}

// ---------------------------------------------------------------------------
// glob matching
// ---------------------------------------------------------------------------

#[test]
fn star_matches_any_run_including_empty() {
    assert!(glob_match(
        "fivem*gtaprocess.exe",
        "fivem_b2802_gtaprocess.exe"
    ));
    assert!(glob_match("fivem*gtaprocess.exe", "fivemgtaprocess.exe"));
    assert!(glob_match("*", ""));
    assert!(glob_match("*", "anything.exe"));
    assert!(glob_match("*.exe", "game.exe"));
}

#[test]
fn question_mark_matches_exactly_one_character() {
    assert!(glob_match(
        "fivem_b????_gtaprocess.exe",
        "fivem_b2802_gtaprocess.exe"
    ));
    assert!(!glob_match(
        "fivem_b???_gtaprocess.exe",
        "fivem_b2802_gtaprocess.exe"
    ));
    assert!(!glob_match(
        "fivem_b?_gtaprocess.exe",
        "fivem_b2802_gtaprocess.exe"
    ));
    assert!(!glob_match("?.exe", ".exe"));
}

#[test]
fn match_is_case_insensitive_and_whole_name() {
    assert!(glob_match(
        "FiveM*GTAProcess.exe",
        "FIVEM_B2802_GTAPROCESS.EXE"
    ));
    assert!(!glob_match("fivem*", "xfivem"));
    assert!(!glob_match("fivem*.exe", "fivem.exe.bak"));
    assert!(!glob_match("a*", ""));
}

#[test]
fn backtracking_star_cases() {
    assert!(glob_match("*a*b*c", "xaybzc"));
    assert!(glob_match("a*a*b", "aaaab"));
    assert!(!glob_match("a*b*c", "acb"));
    assert!(glob_match("**", "x"));
    assert!(glob_match("*?*", "x"));
    assert!(!glob_match("*?*", ""));
}

#[test]
fn pattern_without_wildcards_is_plain_equality() {
    assert!(glob_match("cs2.exe", "CS2.EXE"));
    assert!(!glob_match("cs2.exe", "cs2.exe.bak"));
}

#[test]
fn non_ascii_names_match_by_character() {
    assert!(glob_match("ゲーム*.exe", "ゲーム_2.exe"));
    assert!(glob_match("?.exe", "ゲ.exe"));
    assert!(!glob_match("??.exe", "ゲ.exe"));
}

// ---------------------------------------------------------------------------
// wildcard entries in the built-in and user lists
// ---------------------------------------------------------------------------

#[test]
fn user_block_pattern_blocks_matching_names_only() {
    let lists = Lists::new(&["FiveM_ROS*.exe".to_string()], &[]);
    assert!(lists.is_blocked("FiveM_ROSLauncher.exe"));
    assert!(lists.is_blocked("fivem_ros_helper.exe"));
    assert!(!lists.is_blocked("FiveM_b2802_GTAProcess.exe"));
}

#[test]
fn user_allow_pattern_allowlists_matching_names() {
    let allow = vec![AllowRule {
        exe: "FiveM_*GTAProcess.exe".into(),
        display_name: Some("FiveM".into()),
    }];
    let lists = Lists::new(&[], &allow);
    assert!(lists.is_allowlisted("FiveM_b2802_GTAProcess.exe"));
    assert!(lists.is_allowlisted("C:\\x\\FiveM_GTAProcess.exe"));
    assert!(!lists.is_allowlisted("FiveM.exe"));
}

#[test]
fn blocklist_wins_over_an_allow_pattern() {
    let allow = vec![AllowRule {
        exe: "FiveM*".into(),
        display_name: None,
    }];
    let lists = Lists::new(&["FiveM_b*_GTAProcess.exe".to_string()], &allow);
    assert!(!lists.is_allowlisted("FiveM_b2802_GTAProcess.exe"));
    assert!(lists.is_allowlisted("FiveM.exe"));
}

#[test]
fn allow_pattern_passes_the_gate_without_fullscreen() {
    let allow = vec![AllowRule {
        exe: "FiveM_*GTAProcess.exe".into(),
        display_name: None,
    }];
    let mut c = cfg();
    c.allow = allow;
    let mut snap = windowed("FiveM_b2802_GTAProcess.exe", "");
    snap.in_known_db = false;
    snap.window_rect = Rect::from_size(0, 0, 300, 200);
    let (_, _, accepted) = scored(evaluate_one(&snap, &c));
    assert!(accepted, "an allowlisted pattern skips G5 to G8");
}

#[test]
fn wildcard_only_entries_are_ignored() {
    let lists = Lists::new(
        &["*".to_string(), "??".to_string()],
        &[AllowRule {
            exe: "*".into(),
            display_name: None,
        }],
    );
    assert!(!lists.is_blocked("Game.exe"));
    assert!(!lists.is_allowlisted("Game.exe"));
    // The built-in entries still work.
    assert!(lists.is_blocked("chrome.exe"));
}

#[test]
fn plain_names_keep_exact_behaviour() {
    let lists = Lists::new(&["MyLauncher.exe".to_string()], &[]);
    assert!(lists.is_blocked("mylauncher.exe"));
    assert!(lists.is_blocked("C:\\x\\MyLauncher.EXE"));
    assert!(!lists.is_blocked("MyLauncher2.exe"));
    assert!(!lists.is_blocked("   "));
}

#[test]
fn penalised_known_window_is_not_accepted_by_the_window_rule() {
    // While running, a launcher-style title costs 15 points and disables the window rule.
    let mut snap = windowed("FiveM_b2802_GTAProcess.exe", "FiveM Launcher");
    snap.is_running_game = true;
    let (score, breakdown, accepted) = scored(evaluate_one(&snap, &cfg()));
    assert!(breakdown.contains(&(Reason::LauncherTitlePenalty, -15)));
    assert_eq!(score, 35);
    assert!(!accepted);
}
