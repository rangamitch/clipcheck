//! Hard gates (5.11 G1-G8) followed by the point table, with the running-only penalties.

use crate::lists::{is_generic_title, Lists};
use crate::{CandidateSnapshot, DetectConfig, Rect};

/// Geometry relation between a window and its monitor (5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenKind {
    /// Window rectangle matches the monitor within tolerance on all four sides.
    Geometry,
    /// Window covers at least 90 % of the monitor and has no caption.
    NearFullscreen,
    /// Neither.
    None,
}

/// Reason a candidate was rejected by a hard gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReason {
    /// G0: exe name is empty or whitespace (owning process could not be opened).
    /// Checked first so unreadable processes never share the game id `""`.
    UnknownExe,
    /// G1: exe is on the built-in or user blocklist.
    Blocklisted,
    /// G2: exe is a system shell / developer-tool process.
    SystemShell,
    /// G2: exe is a launcher.
    Launcher,
    /// G2: exe is an anti-cheat service (never a candidate by itself).
    AntiCheatService,
    /// G3: tool window.
    ToolWindow,
    /// G3: cloaked and untitled.
    Cloaked,
    /// G4: neither visible nor minimized.
    NoVisibleWindow,
    /// G5: window smaller than 640x480.
    TooSmall,
    /// G6: generic title and no known match.
    BadTitle,
    /// G7: not the primary window of its process group.
    NotPrimaryInGroup,
    /// G8: no graphics evidence.
    NoGraphicsEvidence,
    /// Score gate: installer/updater title while running with score <= 30.
    InstallerTitle,
}

/// Individual scoring signal, used as the breakdown key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Known database match (+40).
    KnownDb,
    /// Library scan match (+40).
    LibraryMatch,
    /// Geometry fullscreen (+20).
    FullscreenGeometry,
    /// Near fullscreen (+10).
    FullscreenNearFullscreen,
    /// GPU 3D at or above threshold (+20).
    GpuAboveThreshold,
    /// GPU 3D between 2 % and threshold (+5).
    GpuLowLoad,
    /// D3D11/DXGI or D3D12 module (+10).
    D3dModule,
    /// Vulkan module (+10).
    VulkanModule,
    /// OpenGL module (+5).
    OpenGlModule,
    /// Foreground within 10 s (+5).
    ForegroundRecent,
    /// Window at least 1280x720 (+5).
    LargeWindow,
    /// Non-empty, non-generic title (+5).
    NonGenericTitle,
    /// Installer/updater title keyword while running (-30).
    InstallerTitlePenalty,
    /// "Launcher" in title while running (-15).
    LauncherTitlePenalty,
    /// GPU 3D averaged below 2 % over 60 s while running (-20).
    GpuIdlePenalty,
}

/// Result of evaluating one candidate.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// A hard gate failed; the score is not computed.
    Rejected(GateReason),
    /// Gates passed; the score was computed.
    Scored {
        /// Final score, clamped to 0..=100.
        score: u32,
        /// Each contributing signal with its signed points (penalties are negative).
        breakdown: Vec<(Reason, i32)>,
        /// True when the candidate is eligible: allowlisted, or a known game with a
        /// visible window of at least 720p, a non-generic title and no penalty, or
        /// score >= min_score.
        accepted: bool,
    },
}

const INSTALLER_KEYWORDS: &[&str] = &["setup", "installer", "uninstall", "updater", "crash"];
const RECENT_FOREGROUND_MS: u64 = 10_000;
const LOW_GPU_PCT: f32 = 2.0;
const NEAR_FULLSCREEN_RATIO: f64 = 0.90;
/// Smallest window width for the known-list window rule (720p).
const KNOWN_WINDOW_MIN_W: i64 = 1280;
/// Smallest window height for the known-list window rule (720p).
const KNOWN_WINDOW_MIN_H: i64 = 720;

/// Classifies the window's relation to its monitor (5.8).
pub fn fullscreen_kind(
    window: &Rect,
    monitor: &Rect,
    has_caption: bool,
    tol_px: u8,
) -> FullscreenKind {
    let tol = i32::from(tol_px);
    let same = (window.left - monitor.left).abs() <= tol
        && (window.top - monitor.top).abs() <= tol
        && (window.right - monitor.right).abs() <= tol
        && (window.bottom - monitor.bottom).abs() <= tol;
    if same {
        return FullscreenKind::Geometry;
    }
    let mon_area = monitor.area();
    if mon_area > 0
        && !has_caption
        && (window.intersection_area(monitor) as f64) >= NEAR_FULLSCREEN_RATIO * mon_area as f64
    {
        return FullscreenKind::NearFullscreen;
    }
    FullscreenKind::None
}

/// ASCII case-insensitive substring test with no allocation. `needle` is ASCII.
/// Same result as `hay.to_ascii_lowercase().contains(needle)` for ASCII needles:
/// UTF-8 continuation bytes are never ASCII, so no match can start mid-character.
fn contains_ignore_ascii_case(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// Evaluates a candidate using lists built from `cfg`.
///
/// Rebuilds the blocklist and shell sets on every call. Production code and
/// anything that evaluates more than one candidate should build [`Lists`] once
/// with [`Lists::from_config`] and call [`evaluate_with`] instead.
#[doc(hidden)]
pub fn evaluate(snap: &CandidateSnapshot, cfg: &DetectConfig) -> Verdict {
    evaluate_with(&Lists::from_config(cfg), snap, cfg)
}

/// Evaluates a candidate with pre-built lists (cheaper when evaluating many candidates).
pub fn evaluate_with(lists: &Lists, snap: &CandidateSnapshot, cfg: &DetectConfig) -> Verdict {
    let exe = snap.exe_name.as_str();
    let title = snap.window_title.as_str();

    // G0: an empty exe name (process not readable) is never a candidate. It would
    // otherwise pass G1/G2 and collide on game id "".
    if crate::lists::exe_basename(exe).is_empty() {
        return Verdict::Rejected(GateReason::UnknownExe);
    }

    // G1 / G2: blocklist, shell, launchers, anti-cheat services.
    // Shell entries are also on the blocklist; they report as SystemShell.
    if lists.is_system_shell(exe) {
        return Verdict::Rejected(GateReason::SystemShell);
    }
    if lists.is_blocked(exe) {
        return Verdict::Rejected(GateReason::Blocklisted);
    }
    if snap.is_launcher || lists.is_launcher(exe) {
        return Verdict::Rejected(GateReason::Launcher);
    }
    if lists.is_anticheat_service(exe) {
        return Verdict::Rejected(GateReason::AntiCheatService);
    }
    // G3: tool windows, cloaked untitled windows.
    if snap.is_tool_window {
        return Verdict::Rejected(GateReason::ToolWindow);
    }
    if snap.is_cloaked && title.trim().is_empty() {
        return Verdict::Rejected(GateReason::Cloaked);
    }
    // G4: must be visible or minimized.
    if !(snap.is_visible || snap.is_minimized) {
        return Verdict::Rejected(GateReason::NoVisibleWindow);
    }

    let allowlisted = lists.is_allowlisted(exe);
    let fullscreen = fullscreen_kind(
        &snap.window_rect,
        &snap.monitor_rect,
        snap.has_caption,
        cfg.fullscreen_tolerance_px,
    );
    let gpu_at_threshold = snap
        .gpu_3d_load_pct
        .is_some_and(|g| g >= cfg.gpu_threshold_pct);

    // G5-G8 are skipped for allowlisted (manual or config) games.
    if !allowlisted {
        // G5: size.
        if snap.window_rect.width() < 640 || snap.window_rect.height() < 480 {
            return Verdict::Rejected(GateReason::TooSmall);
        }
        // G6: title.
        if is_generic_title(title) && !snap.in_known_db {
            return Verdict::Rejected(GateReason::BadTitle);
        }
        // G7: group primary.
        if !snap.is_primary_in_group {
            return Verdict::Rejected(GateReason::NotPrimaryInGroup);
        }
        // G8: graphics evidence.
        let evidence = fullscreen != FullscreenKind::None
            || snap.in_known_db
            || snap.in_library_scan
            || gpu_at_threshold;
        if !evidence {
            return Verdict::Rejected(GateReason::NoGraphicsEvidence);
        }
    }

    let mut breakdown: Vec<(Reason, i32)> = Vec::new();
    let mut push = |reason: Reason, points: i32| breakdown.push((reason, points));

    if snap.in_known_db {
        push(Reason::KnownDb, 40);
    }
    if snap.in_library_scan {
        push(Reason::LibraryMatch, 40);
    }
    match fullscreen {
        FullscreenKind::Geometry => push(Reason::FullscreenGeometry, 20),
        FullscreenKind::NearFullscreen => push(Reason::FullscreenNearFullscreen, 10),
        FullscreenKind::None => {}
    }
    if let Some(gpu) = snap.gpu_3d_load_pct {
        if gpu >= cfg.gpu_threshold_pct {
            push(Reason::GpuAboveThreshold, 20);
        } else if gpu >= LOW_GPU_PCT {
            push(Reason::GpuLowLoad, 5);
        }
    }

    let has_module = |names: &[&str]| {
        snap.loaded_graphics_modules
            .iter()
            .any(|m| names.iter().any(|n| m.eq_ignore_ascii_case(n)))
    };
    if has_module(&["d3d11.dll", "dxgi.dll", "d3d12.dll"]) {
        push(Reason::D3dModule, 10);
    }
    if has_module(&["vulkan-1.dll"]) {
        push(Reason::VulkanModule, 10);
    }
    if has_module(&["opengl32.dll"]) {
        push(Reason::OpenGlModule, 5);
    }

    if snap
        .ms_since_foreground
        .is_some_and(|ms| ms <= RECENT_FOREGROUND_MS)
    {
        push(Reason::ForegroundRecent, 5);
    }
    if snap.window_rect.width() >= 1280 && snap.window_rect.height() >= 720 {
        push(Reason::LargeWindow, 5);
    }
    if !is_generic_title(title) {
        push(Reason::NonGenericTitle, 5);
    }

    // Penalties apply only to running games, so loading screens are not penalized at start.
    let mut installer_title = false;
    if snap.is_running_game {
        if INSTALLER_KEYWORDS
            .iter()
            .any(|k| contains_ignore_ascii_case(title, k))
        {
            installer_title = true;
            push(Reason::InstallerTitlePenalty, -30);
        }
        if contains_ignore_ascii_case(title, "launcher") {
            push(Reason::LauncherTitlePenalty, -15);
        }
        if snap.gpu_3d_avg60_pct.is_some_and(|a| a < LOW_GPU_PCT) {
            push(Reason::GpuIdlePenalty, -20);
        }
    }

    let raw: i32 = breakdown.iter().map(|(_, p)| *p).sum();
    let score = raw.clamp(0, 100) as u32;

    if installer_title && score <= 30 {
        return Verdict::Rejected(GateReason::InstallerTitle);
    }

    // Known-list window rule: a known game that owns a visible window of at least
    // 720p with a real title is accepted without fullscreen or GPU evidence. A
    // windowed known game otherwise scores 40 + 5 + 5 + 5 = 55 and never reaches
    // min_score, which is why windowed FiveM was never recorded. Generic titles
    // (menus with no title) still need the normal score. Any penalty (installer,
    // launcher title, idle GPU while running) disables the rule, so the penalties
    // keep their meaning.
    let penalised = breakdown.iter().any(|(_, points)| *points < 0);
    let known_window = snap.in_known_db
        && !penalised
        && snap.window_rect.width() >= KNOWN_WINDOW_MIN_W
        && snap.window_rect.height() >= KNOWN_WINDOW_MIN_H
        && !is_generic_title(title);

    Verdict::Scored {
        score,
        breakdown,
        accepted: allowlisted || known_window || score >= u32::from(cfg.min_score),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowRule, CaptureTarget};

    fn monitor() -> Rect {
        Rect::from_size(0, 0, 1920, 1080)
    }

    /// Unknown, fullscreen, foreground, 1920x1080 game window, no GPU data.
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

    // Worked example 1 (5.11): unknown fullscreen game, GPU 40 %, D3D11, foreground,
    // 1920x1080, title -> 20+20+10+5+5+5 = 65, eligible.
    #[test]
    fn worked_example_unknown_fullscreen_game_is_eligible() {
        let mut s = base();
        s.gpu_3d_load_pct = Some(40.0);
        s.loaded_graphics_modules = vec!["d3d11.dll".into()];
        s.ms_since_foreground = Some(1_000);
        let (score, _, accepted) = scored(evaluate(&s, &cfg()));
        assert_eq!(score, 65);
        assert!(accepted);
    }

    // Worked example 2: unknown fullscreen YouTube in a browser -> blocked (G1).
    #[test]
    fn worked_example_browser_is_blocked() {
        let mut s = base();
        s.exe_name = "chrome.exe".into();
        s.gpu_3d_load_pct = Some(60.0);
        assert_eq!(
            evaluate(&s, &cfg()),
            Verdict::Rejected(GateReason::Blocklisted)
        );
    }

    // Worked example 3: unknown fullscreen video player (VLC) -> blocked (G1).
    #[test]
    fn worked_example_video_player_is_blocked() {
        let mut s = base();
        s.exe_name = "vlc.exe".into();
        assert_eq!(
            evaluate(&s, &cfg()),
            Verdict::Rejected(GateReason::Blocklisted)
        );
    }

    // Worked example 4: GPU 3 %, no D3D module. Geometry satisfies G8, so the
    // candidate is scored: 20 (Geometry) + 5 (low GPU) + 5 (size) + 5 (title) = 35.
    // 35 < 60, so it is not eligible (Scored, accepted = false).
    #[test]
    fn worked_example_low_gpu_no_module_is_not_eligible() {
        let mut s = base();
        s.gpu_3d_load_pct = Some(3.0);
        let (score, _, accepted) = scored(evaluate(&s, &cfg()));
        assert_eq!(score, 35);
        assert!(!accepted);
    }

    // Same candidate with no fullscreen signal and no GPU -> NoGraphicsEvidence (G8).
    #[test]
    fn windowed_unknown_without_gpu_fails_g8() {
        let mut s = base();
        s.window_rect = Rect::from_size(100, 100, 1000, 700);
        s.gpu_3d_load_pct = Some(3.0);
        assert_eq!(
            evaluate(&s, &cfg()),
            Verdict::Rejected(GateReason::NoGraphicsEvidence)
        );
    }

    // Worked example 5: known game windowed, GPU 0 %, in menu -> 40+5+5 = 50,
    // below threshold, not gated; does not start. Title is generic (no title points).
    #[test]
    fn worked_example_known_windowed_menu_scores_50_and_not_accepted() {
        let mut s = base();
        s.exe_name = "known.exe".into();
        s.in_known_db = true;
        s.window_title = String::new();
        s.window_rect = Rect::from_size(100, 100, 1600, 900);
        s.gpu_3d_load_pct = Some(0.0);
        s.ms_since_foreground = Some(500);
        let (score, _, accepted) = scored(evaluate(&s, &cfg()));
        assert_eq!(score, 50);
        assert!(!accepted);
    }

    #[test]
    fn min_score_is_respected() {
        let mut s = base();
        s.gpu_3d_load_pct = Some(40.0);
        s.loaded_graphics_modules = vec!["d3d11.dll".into()];
        s.ms_since_foreground = Some(1_000);
        let mut c = cfg();
        c.min_score = 70;
        let (score, _, accepted) = scored(evaluate(&s, &c));
        assert_eq!(score, 65);
        assert!(!accepted);
    }

    #[test]
    fn exact_breakdown_for_known_library_game() {
        let mut s = base();
        s.in_known_db = true;
        s.in_library_scan = true;
        s.gpu_3d_load_pct = Some(20.0);
        s.loaded_graphics_modules = vec![
            "DXGI.dll".into(),
            "vulkan-1.dll".into(),
            "opengl32.dll".into(),
        ];
        s.ms_since_foreground = Some(10_000);
        let (score, breakdown, accepted) = scored(evaluate(&s, &cfg()));
        assert!(accepted);
        assert_eq!(
            breakdown,
            vec![
                (Reason::KnownDb, 40),
                (Reason::LibraryMatch, 40),
                (Reason::FullscreenGeometry, 20),
                (Reason::GpuAboveThreshold, 20),
                (Reason::D3dModule, 10),
                (Reason::VulkanModule, 10),
                (Reason::OpenGlModule, 5),
                (Reason::ForegroundRecent, 5),
                (Reason::LargeWindow, 5),
                (Reason::NonGenericTitle, 5),
            ]
        );
        assert_eq!(score, 100, "raw sum is 160 and must clamp to 100");
    }

    #[test]
    fn gpu_low_load_gives_partial_points() {
        let mut s = base();
        s.gpu_3d_load_pct = Some(5.0);
        let (_, breakdown, _) = scored(evaluate(&s, &cfg()));
        assert!(breakdown.contains(&(Reason::GpuLowLoad, 5)));
        assert!(!breakdown
            .iter()
            .any(|(r, _)| *r == Reason::GpuAboveThreshold));
    }

    #[test]
    fn near_fullscreen_only_without_caption() {
        let mut s = base();
        s.window_rect = Rect::from_size(0, 0, 1900, 1040);
        s.gpu_3d_load_pct = Some(40.0);
        let (_, breakdown, _) = scored(evaluate(&s, &cfg()));
        assert!(breakdown.contains(&(Reason::FullscreenNearFullscreen, 10)));

        s.has_caption = true;
        let (_, breakdown, _) = scored(evaluate(&s, &cfg()));
        assert!(!breakdown.iter().any(|(r, _)| matches!(
            r,
            Reason::FullscreenNearFullscreen | Reason::FullscreenGeometry
        )));
    }

    #[test]
    fn geometry_tolerance_absorbs_off_by_one() {
        let mon = monitor();
        let off = Rect::from_size(1, 0, 1918, 1080);
        assert_eq!(
            fullscreen_kind(&off, &mon, false, 2),
            FullscreenKind::Geometry
        );
        assert_eq!(
            fullscreen_kind(&off, &mon, false, 0),
            FullscreenKind::NearFullscreen
        );
    }

    #[test]
    fn installer_title_while_running_is_gated() {
        let mut s = base();
        s.window_title = "Game Setup Wizard".into();
        s.is_running_game = true;
        s.ms_since_foreground = Some(1_000);
        // Geometry 20 + foreground 5 + size 5 + title 5 - 30 = 5 <= 30
        assert_eq!(
            evaluate(&s, &cfg()),
            Verdict::Rejected(GateReason::InstallerTitle)
        );
    }

    #[test]
    fn installer_title_before_running_is_not_penalized() {
        let mut s = base();
        s.window_title = "Game Setup Wizard".into();
        s.is_running_game = false;
        let (_, breakdown, _) = scored(evaluate(&s, &cfg()));
        assert!(!breakdown
            .iter()
            .any(|(r, _)| *r == Reason::InstallerTitlePenalty));
    }

    #[test]
    fn launcher_title_and_idle_gpu_penalties_apply_while_running() {
        let mut s = base();
        s.window_title = "Big Game Launcher".into();
        s.is_running_game = true;
        s.gpu_3d_avg60_pct = Some(0.5);
        let (_, breakdown, _) = scored(evaluate(&s, &cfg()));
        assert!(breakdown.contains(&(Reason::LauncherTitlePenalty, -15)));
        assert!(breakdown.contains(&(Reason::GpuIdlePenalty, -20)));
    }

    #[test]
    fn allowlisted_game_skips_g5_to_g8_and_is_accepted() {
        let mut s = base();
        s.exe_name = "Indie.exe".into();
        s.window_rect = Rect::from_size(0, 0, 300, 200);
        s.window_title = String::new();
        s.is_primary_in_group = false;
        let mut c = cfg();
        c.allow.push(AllowRule {
            exe: "indie.EXE".into(),
            display_name: Some("Indie".into()),
        });
        let (_, _, accepted) = scored(evaluate(&s, &c));
        assert!(accepted);
    }

    #[test]
    fn blocklist_wins_over_allow_rule() {
        let mut s = base();
        s.exe_name = "Discord.exe".into();
        let mut c = cfg();
        c.allow.push(AllowRule {
            exe: "discord.exe".into(),
            display_name: None,
        });
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::Blocklisted));
    }

    #[test]
    fn block_extra_blocks_candidate() {
        let mut s = base();
        s.exe_name = "MyLauncherHelper.exe".into();
        let mut c = cfg();
        c.block_extra.push("mylauncherhelper.exe".into());
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::Blocklisted));
    }

    #[test]
    fn shell_launcher_and_anticheat_gates() {
        let c = cfg();
        let mut s = base();
        s.exe_name = "EXPLORER.EXE".into();
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::SystemShell));

        let mut s = base();
        s.exe_name = "steam.exe".into();
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::Launcher));

        let mut s = base();
        s.exe_name = "EasyAntiCheat.exe".into();
        assert_eq!(
            evaluate(&s, &c),
            Verdict::Rejected(GateReason::AntiCheatService)
        );
    }

    #[test]
    fn window_gates_g3_to_g7() {
        let c = cfg();

        let mut s = base();
        s.is_tool_window = true;
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::ToolWindow));

        let mut s = base();
        s.is_cloaked = true;
        s.window_title = "  ".into();
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::Cloaked));

        let mut s = base();
        s.is_visible = false;
        assert_eq!(
            evaluate(&s, &c),
            Verdict::Rejected(GateReason::NoVisibleWindow)
        );

        let mut s = base();
        s.is_visible = false;
        s.is_minimized = true;
        assert!(matches!(evaluate(&s, &c), Verdict::Scored { .. }));

        let mut s = base();
        s.window_rect = Rect::from_size(0, 0, 639, 1080);
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::TooSmall));

        let mut s = base();
        s.window_title = "Program Manager".into();
        assert_eq!(evaluate(&s, &c), Verdict::Rejected(GateReason::BadTitle));

        let mut s = base();
        s.window_title = "Program Manager".into();
        s.in_known_db = true;
        assert!(matches!(evaluate(&s, &c), Verdict::Scored { .. }));

        let mut s = base();
        s.is_primary_in_group = false;
        assert_eq!(
            evaluate(&s, &c),
            Verdict::Rejected(GateReason::NotPrimaryInGroup)
        );
    }

    #[test]
    fn exe_matching_is_case_insensitive_for_gates() {
        let mut s = base();
        s.exe_name = "CHROME.EXE".into();
        assert_eq!(
            evaluate(&s, &cfg()),
            Verdict::Rejected(GateReason::Blocklisted)
        );
    }

    #[test]
    fn empty_or_blank_exe_is_rejected_before_scoring() {
        // Unreadable process: process_info returns an empty exe name.
        for exe in ["", "   ", "\t"] {
            let mut s = base();
            s.exe_name = exe.into();
            s.in_known_db = true;
            s.gpu_3d_load_pct = Some(60.0);
            assert_eq!(
                evaluate(&s, &cfg()),
                Verdict::Rejected(GateReason::UnknownExe),
                "exe {exe:?} must be rejected even with strong evidence"
            );
        }
    }

    #[test]
    fn title_keyword_scan_matches_lowercased_contains() {
        assert!(contains_ignore_ascii_case("Game SETUP Wizard", "setup"));
        assert!(contains_ignore_ascii_case("Big GAME Launcher", "launcher"));
        assert!(contains_ignore_ascii_case("ゲーム Updater", "updater"));
        assert!(contains_ignore_ascii_case("anything", ""));
        assert!(!contains_ignore_ascii_case("Elden Ring", "setup"));
        assert!(!contains_ignore_ascii_case("upd", "updater"));
        // Non-ASCII text next to the keyword does not disturb the match.
        assert!(contains_ignore_ascii_case("ÄÖÜ crash", "crash"));
    }

    #[test]
    fn valid_exe_is_not_affected_by_unknown_exe_gate() {
        let s = base();
        assert!(matches!(evaluate(&s, &cfg()), Verdict::Scored { .. }));
    }
}
