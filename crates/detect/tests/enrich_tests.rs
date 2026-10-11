#![cfg(windows)]

//! Tests for `clipcheck_detect::enrich` (fake probes only, except the live smoke test).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipcheck_detect::enrich::{Enricher, ExePathProber, GpuSampler, Probes};
use clipcheck_detect::library::{LibEntry, LibraryCache, LibraryScan, Store};
use clipcheck_detect::modules::ModuleProbe;
use clipcheck_detect::runner::{SnapshotSource, WinSource};
use clipcheck_detect::score::{evaluate, GateReason, Reason, Verdict};
use clipcheck_detect::{CandidateSnapshot, CaptureTarget, DetectConfig, Rect};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn snap(exe: &str, title: &str, rect: Rect, pid: u32, hwnd: u64) -> CandidateSnapshot {
    CandidateSnapshot {
        exe_name: exe.into(),
        window_title: title.into(),
        window_rect: rect,
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
        process_age_ms: 60_000,
    }
}

fn lib_entry(dir: &str) -> LibEntry {
    LibEntry {
        store: Store::Steam,
        id: "1".into(),
        name: "Game".into(),
        install_dir: PathBuf::from(dir),
    }
}

/// Probes with everything empty: no library, no GPU, no modules, no exe path, nothing known.
fn probes_with_library(entries: Vec<LibEntry>) -> Probes {
    Probes {
        library: LibraryCache::with_scanner(move || LibraryScan {
            entries: entries.clone(),
        }),
        gpu: None,
        modules: Box::new(|_| ModuleProbe::NoneLoaded),
        exe_path: Box::new(|_| None),
        is_known: Box::new(|_| false),
    }
}

fn empty_probes() -> Probes {
    probes_with_library(Vec::new())
}

/// GPU sampler that reports `value` for `pid` on every call; `value` can be changed by the test.
fn gpu_for(pid: u32, value: Arc<Mutex<f32>>) -> GpuSampler {
    Box::new(move || Some(HashMap::from([(pid, *value.lock().unwrap())])))
}

fn approx(actual: Option<f32>, expected: f32) -> bool {
    match actual {
        Some(v) => (v - expected).abs() < 1e-3,
        None => false,
    }
}

fn exe_prober(path: &'static str) -> ExePathProber {
    Box::new(move |_| Some(PathBuf::from(path)))
}

// ---------------------------------------------------------------------------
// Pre-filter and probe call counts
// ---------------------------------------------------------------------------

#[test]
fn prefilter_rejects_non_game_windows_without_probing() {
    let exe_calls = Arc::new(Mutex::new(Vec::<u32>::new()));
    let mod_calls = Arc::new(Mutex::new(Vec::<u32>::new()));
    let lib_calls = Arc::new(AtomicUsize::new(0));

    let mut probes = empty_probes();
    probes.is_known = Box::new(|_| true); // would mark everything known if probed
    let e = exe_calls.clone();
    probes.exe_path = Box::new(move |pid| {
        e.lock().unwrap().push(pid);
        Some(PathBuf::from(r"C:\Games\Game\game.exe"))
    });
    let m = mod_calls.clone();
    probes.modules = Box::new(move |pid| {
        m.lock().unwrap().push(pid);
        ModuleProbe::Found(vec!["d3d11.dll".into()])
    });
    let l = lib_calls.clone();
    probes.library = LibraryCache::with_scanner(move || {
        l.fetch_add(1, Ordering::SeqCst);
        LibraryScan {
            entries: vec![lib_entry(r"C:\Games\Game")],
        }
    });
    let mut enricher = Enricher::with_probes(probes);

    let big = Rect::from_size(0, 0, 1280, 720);
    let tiny = snap("game.exe", "Game", Rect::from_size(0, 0, 300, 200), 1, 1);
    let narrow = snap("game.exe", "Game", Rect::from_size(0, 0, 399, 600), 2, 2);
    let mut invisible = snap("game.exe", "Game", big, 3, 3);
    invisible.is_visible = false;
    let mut tool = snap("game.exe", "Game", big, 4, 4);
    tool.is_tool_window = true;
    let mut cloaked = snap("game.exe", "Game", big, 5, 5);
    cloaked.is_cloaked = true;
    let mut launcher = snap("game.exe", "Game", big, 6, 6);
    launcher.is_launcher = true;
    let empty_exe = snap("   ", "Game", big, 7, 7);
    let chrome = snap("chrome.exe", "Chrome", big, 8, 8);
    let discord = snap("discord.exe", "Discord", big, 9, 9);
    let explorer = snap("explorer.exe", "Explorer", big, 10, 10);
    let mut snaps = vec![
        tiny, narrow, invisible, tool, cloaked, launcher, empty_exe, chrome, discord, explorer,
    ];

    enricher.enrich(&mut snaps, None, 0);

    assert!(
        exe_calls.lock().unwrap().is_empty(),
        "exe_path must not be probed"
    );
    assert!(
        mod_calls.lock().unwrap().is_empty(),
        "modules must not be probed"
    );
    assert_eq!(
        lib_calls.load(Ordering::SeqCst),
        0,
        "library must not be scanned"
    );
    for s in &snaps {
        assert!(!s.in_known_db, "{} must not be marked known", s.exe_name);
        assert!(!s.in_library_scan, "{} must not match library", s.exe_name);
        assert!(s.loaded_graphics_modules.is_empty());
        assert_eq!(s.gpu_3d_load_pct, None);
        assert_eq!(s.gpu_3d_avg60_pct, None);
    }
}

#[test]
fn prefilter_size_boundary_400x300_is_candidate() {
    let exe_calls = Arc::new(Mutex::new(Vec::<u32>::new()));
    let mut probes = empty_probes();
    let e = exe_calls.clone();
    probes.exe_path = Box::new(move |pid| {
        e.lock().unwrap().push(pid);
        None
    });
    let mut enricher = Enricher::with_probes(probes);

    let mut snaps = vec![
        snap("game.exe", "Game", Rect::from_size(0, 0, 400, 300), 11, 11),
        snap("game.exe", "Game", Rect::from_size(0, 0, 399, 600), 12, 12),
    ];
    enricher.enrich(&mut snaps, None, 0);
    assert_eq!(
        *exe_calls.lock().unwrap(),
        vec![11],
        "only the 400x300 window is probed"
    );
}

#[test]
fn exe_path_is_cached_per_pid_while_pid_is_alive() {
    let calls = Arc::new(Mutex::new(Vec::<u32>::new()));
    let mut probes = empty_probes();
    let c = calls.clone();
    probes.exe_path = Box::new(move |pid| {
        c.lock().unwrap().push(pid);
        Some(PathBuf::from(r"C:\Games\Game\game.exe"))
    });
    let mut enricher = Enricher::with_probes(probes);

    let mut snaps = vec![snap(
        "game.exe",
        "Game",
        Rect::from_size(0, 0, 1280, 720),
        42,
        1,
    )];
    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 1_000);
    enricher.enrich(&mut snaps, None, 2_000);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![42],
        "one probe while pid 42 stays alive"
    );

    // Pid disappears, then a new pid 42 shows up: the cache must be dropped.
    let mut empty: Vec<CandidateSnapshot> = Vec::new();
    enricher.enrich(&mut empty, None, 3_000);
    enricher.enrich(&mut snaps, None, 4_000);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![42, 42],
        "re-probed after the pid went away"
    );
}

// ---------------------------------------------------------------------------
// Library, known-db, modules
// ---------------------------------------------------------------------------

#[test]
fn library_match_by_install_dir() {
    let mut probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    probes.exe_path = exe_prober(r"C:\Games\Game\game.exe");
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap(
        "game.exe",
        "Game",
        Rect::from_size(0, 0, 1280, 720),
        1,
        1,
    )];
    enricher.enrich(&mut snaps, None, 0);
    assert!(snaps[0].in_library_scan);
}

#[test]
fn library_component_boundary_is_respected() {
    let mut probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    probes.exe_path = exe_prober(r"C:\Games\Gamebar\x.exe");
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap(
        "x.exe",
        "Game",
        Rect::from_size(0, 0, 1280, 720),
        1,
        1,
    )];
    enricher.enrich(&mut snaps, None, 0);
    assert!(!snaps[0].in_library_scan);
}

#[test]
fn library_no_match_and_missing_exe_path_are_false() {
    let mut probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    probes.exe_path = exe_prober(r"D:\Other\app.exe");
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap(
        "app.exe",
        "App",
        Rect::from_size(0, 0, 1280, 720),
        1,
        1,
    )];
    enricher.enrich(&mut snaps, None, 0);
    assert!(!snaps[0].in_library_scan);

    let probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    let mut enricher = Enricher::with_probes(probes); // exe_path returns None
    let mut snaps = vec![snap(
        "game.exe",
        "Game",
        Rect::from_size(0, 0, 1280, 720),
        2,
        2,
    )];
    enricher.enrich(&mut snaps, None, 0);
    assert!(!snaps[0].in_library_scan);
}

#[test]
fn known_db_uses_is_known_for_candidates_only() {
    let mut probes = empty_probes();
    probes.is_known = Box::new(|e| e.eq_ignore_ascii_case("terraria.exe"));
    let mut enricher = Enricher::with_probes(probes);

    let mut snaps = vec![
        snap(
            "Terraria.EXE",
            "Terraria",
            Rect::from_size(0, 0, 1280, 720),
            1,
            1,
        ),
        snap("other.exe", "Other", Rect::from_size(0, 0, 1280, 720), 2, 2),
        snap(
            "Terraria.EXE",
            "Terraria",
            Rect::from_size(0, 0, 300, 200),
            3,
            3,
        ),
    ];
    enricher.enrich(&mut snaps, None, 0);
    assert!(snaps[0].in_known_db);
    assert!(!snaps[1].in_known_db);
    assert!(
        !snaps[2].in_known_db,
        "tiny window is pre-filtered, so not marked known"
    );
}

#[test]
fn module_probe_results_map_to_loaded_modules() {
    let mut probes = empty_probes();
    probes.modules = Box::new(|pid| match pid {
        1 => ModuleProbe::Found(vec!["d3d11.dll".into(), "dxgi.dll".into()]),
        2 => ModuleProbe::NoneLoaded,
        3 => ModuleProbe::Denied,
        _ => ModuleProbe::Gone,
    });
    let mut enricher = Enricher::with_probes(probes);
    let big = Rect::from_size(0, 0, 1280, 720);
    let mut snaps = vec![
        snap("a.exe", "A", big, 1, 1),
        snap("b.exe", "B", big, 2, 2),
        snap("c.exe", "C", big, 3, 3),
        snap("d.exe", "D", big, 4, 4),
    ];
    enricher.enrich(&mut snaps, None, 0);
    assert_eq!(
        snaps[0].loaded_graphics_modules,
        vec!["d3d11.dll", "dxgi.dll"]
    );
    assert!(snaps[1].loaded_graphics_modules.is_empty());
    assert!(snaps[2].loaded_graphics_modules.is_empty());
    assert!(snaps[3].loaded_graphics_modules.is_empty());
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

#[test]
fn gpu_sampler_called_at_most_once_per_second() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let mut probes = empty_probes();
    probes.gpu = Some(Box::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        Some(HashMap::from([(7u32, 20.0f32)]))
    }));
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 7, 70)];

    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 500);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    enricher.enrich(&mut snaps, None, 1_000);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn gpu_failed_sample_is_skipped_without_panic() {
    let mut probes = empty_probes();
    probes.gpu = Some(Box::new(|| None));
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 7, 70)];
    for t in [0u64, 1_000, 2_000, 3_000] {
        enricher.enrich(&mut snaps, None, t);
        assert_eq!(snaps[0].gpu_3d_load_pct, None);
        assert_eq!(snaps[0].gpu_3d_avg60_pct, None);
    }
}

#[test]
fn gpu_none_probe_leaves_gpu_fields_none() {
    let mut enricher = Enricher::with_probes(empty_probes());
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 7, 70)];
    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 1_000);
    assert_eq!(snaps[0].gpu_3d_load_pct, None);
    assert_eq!(snaps[0].gpu_3d_avg60_pct, None);
}

#[test]
fn gpu_average_needs_two_samples_then_reports_value() {
    let value = Arc::new(Mutex::new(20.0f32));
    let mut probes = empty_probes();
    probes.gpu = Some(gpu_for(7, value.clone()));
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 7, 70)];

    enricher.enrich(&mut snaps, None, 0);
    assert_eq!(snaps[0].gpu_3d_load_pct, None);
    assert_eq!(snaps[0].gpu_3d_avg60_pct, None);

    enricher.enrich(&mut snaps, None, 1_000);
    assert!(
        approx(snaps[0].gpu_3d_load_pct, 20.0),
        "{:?}",
        snaps[0].gpu_3d_load_pct
    );
    assert!(
        approx(snaps[0].gpu_3d_avg60_pct, 20.0),
        "{:?}",
        snaps[0].gpu_3d_avg60_pct
    );
}

#[test]
fn gpu_absent_pid_is_not_zero_filled() {
    let mut probes = empty_probes();
    probes.gpu = Some(Box::new(|| Some(HashMap::from([(7u32, 50.0f32)]))));
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![
        snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 7, 70),
        snap("h.exe", "H", Rect::from_size(0, 0, 1280, 720), 8, 80),
    ];
    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 1_000);
    assert!(approx(snaps[0].gpu_3d_load_pct, 50.0));
    assert_eq!(snaps[1].gpu_3d_load_pct, None);
    assert_eq!(snaps[1].gpu_3d_avg60_pct, None);
}

#[test]
fn gpu_short_window_tracks_last_5s_long_window_tracks_60s() {
    // t = 0..=3000 at 0 %, t = 4000..=9000 at 40 %, sampled once per second.
    let value = Arc::new(Mutex::new(0.0f32));
    let mut probes = empty_probes();
    probes.gpu = Some(gpu_for(7, value.clone()));
    let mut enricher = Enricher::with_probes(probes);
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 7, 70)];

    for t in (0..=9_000u64).step_by(1_000) {
        *value.lock().unwrap() = if t < 4_000 { 0.0 } else { 40.0 };
        enricher.enrich(&mut snaps, None, t);
    }
    // Short window [4000, 9000]: six samples, all 40 -> 40.
    assert!(
        approx(snaps[0].gpu_3d_load_pct, 40.0),
        "{:?}",
        snaps[0].gpu_3d_load_pct
    );
    // Long window covers all ten samples: (4 * 0 + 6 * 40) / 10 = 24.
    assert!(
        approx(snaps[0].gpu_3d_avg60_pct, 24.0),
        "{:?}",
        snaps[0].gpu_3d_avg60_pct
    );
}

// ---------------------------------------------------------------------------
// Foreground tracking
// ---------------------------------------------------------------------------

#[test]
fn foreground_time_is_recorded_and_ages() {
    let mut enricher = Enricher::with_probes(empty_probes());
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 1, 10)];
    enricher.enrich(&mut snaps, Some(10), 1_000);
    assert_eq!(snaps[0].ms_since_foreground, Some(0));
    enricher.enrich(&mut snaps, None, 4_000);
    assert_eq!(snaps[0].ms_since_foreground, Some(3_000));
}

#[test]
fn never_foreground_window_has_none() {
    let mut enricher = Enricher::with_probes(empty_probes());
    let mut snaps = vec![snap("g.exe", "G", Rect::from_size(0, 0, 1280, 720), 1, 10)];
    enricher.enrich(&mut snaps, None, 1_000);
    enricher.enrich(&mut snaps, Some(99), 2_000); // a different hwnd, not in the list
    assert_eq!(snaps[0].ms_since_foreground, None);
}

#[test]
fn window_keeps_last_foreground_time_after_focus_moves_away() {
    let mut enricher = Enricher::with_probes(empty_probes());
    let mut snaps = vec![
        snap("a.exe", "A", Rect::from_size(0, 0, 1280, 720), 1, 10),
        snap("b.exe", "B", Rect::from_size(0, 0, 1280, 720), 2, 20),
    ];
    enricher.enrich(&mut snaps, Some(10), 1_000);
    enricher.enrich(&mut snaps, Some(20), 2_000);
    enricher.enrich(&mut snaps, None, 4_000);
    assert_eq!(snaps[0].ms_since_foreground, Some(3_000));
    assert_eq!(snaps[1].ms_since_foreground, Some(2_000));
}

// ---------------------------------------------------------------------------
// End-to-end scoring with fakes
// ---------------------------------------------------------------------------

#[test]
fn e2e_borderless_fullscreen_game_is_accepted() {
    let mut probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    probes.exe_path = exe_prober(r"C:\Games\Game\Game.exe");
    let mut enricher = Enricher::with_probes(probes);

    let full = Rect::from_size(0, 0, 1920, 1080);
    let mut snaps = vec![CandidateSnapshot {
        has_caption: false,
        monitor_rect: full,
        ..snap("Game.exe", "My Game", full, 100, 1000)
    }];
    enricher.enrich(&mut snaps, None, 0);

    match evaluate(&snaps[0], &DetectConfig::default()) {
        Verdict::Scored {
            score,
            breakdown,
            accepted,
        } => {
            assert!(accepted, "score {score}, breakdown {breakdown:?}");
            assert!(score >= 60, "score {score}");
            assert!(breakdown.iter().any(|(r, _)| *r == Reason::LibraryMatch));
        }
        other => panic!("expected Scored, got {other:?}"),
    }
}

#[test]
fn e2e_windowed_game_with_gpu_and_module_is_accepted() {
    let value = Arc::new(Mutex::new(20.0f32));
    let mut probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    probes.exe_path = exe_prober(r"C:\Games\Game\Game.exe");
    probes.gpu = Some(gpu_for(100, value.clone()));
    probes.modules = Box::new(|_| ModuleProbe::Found(vec!["d3d11.dll".into()]));
    let mut enricher = Enricher::with_probes(probes);

    let rect = Rect::from_size(100, 100, 1280, 720);
    let mut snaps = vec![snap("Game.exe", "My Game", rect, 100, 1000)];
    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 1_000);

    match evaluate(&snaps[0], &DetectConfig::default()) {
        Verdict::Scored {
            score,
            breakdown,
            accepted,
        } => {
            assert!(accepted, "score {score}, breakdown {breakdown:?}");
            assert!(breakdown.iter().any(|(r, _)| *r == Reason::LibraryMatch));
            assert!(breakdown.iter().any(|(r, _)| *r == Reason::D3dModule));
            assert!(breakdown
                .iter()
                .any(|(r, _)| *r == Reason::GpuAboveThreshold));
        }
        other => panic!("expected Scored, got {other:?}"),
    }
}

#[test]
fn e2e_blocklisted_apps_are_rejected_even_with_all_evidence() {
    let value = Arc::new(Mutex::new(50.0f32));
    let chrome_dir = r"C:\Program Files\Google\Chrome";
    let mut probes = probes_with_library(vec![lib_entry(chrome_dir)]);
    probes.exe_path = exe_prober(r"C:\Program Files\Google\Chrome\chrome.exe");
    probes.gpu = Some(gpu_for(200, value.clone()));
    probes.modules = Box::new(|_| ModuleProbe::Found(vec!["d3d11.dll".into()]));
    probes.is_known = Box::new(|_| true);
    let mut enricher = Enricher::with_probes(probes);

    let full = Rect::from_size(0, 0, 1920, 1080);
    let big_title = "A Very Large Visible Window Title";
    let mut snaps = vec![
        CandidateSnapshot {
            has_caption: false,
            ..snap("chrome.exe", big_title, full, 200, 2000)
        },
        CandidateSnapshot {
            has_caption: false,
            ..snap("discord.exe", big_title, full, 201, 2001)
        },
    ];
    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 1_000);

    for s in &snaps {
        assert_eq!(
            evaluate(s, &DetectConfig::default()),
            Verdict::Rejected(GateReason::Blocklisted),
            "{}",
            s.exe_name
        );
    }
}

#[test]
fn e2e_unrecognized_windowed_app_is_not_accepted() {
    let mut enricher = Enricher::with_probes(empty_probes());
    let mut snaps = vec![snap(
        "notepadx.exe",
        "Untitled",
        Rect::from_size(0, 0, 1280, 720),
        300,
        3000,
    )];
    enricher.enrich(&mut snaps, None, 0);
    enricher.enrich(&mut snaps, None, 1_000);

    match evaluate(&snaps[0], &DetectConfig::default()) {
        Verdict::Rejected(_) => {}
        Verdict::Scored {
            score, accepted, ..
        } => {
            assert!(!accepted, "must not be accepted");
            assert!(score < 60, "score {score}");
        }
    }
}

#[test]
fn e2e_tiny_library_game_is_not_probed_and_not_accepted() {
    let exe_calls = Arc::new(Mutex::new(0usize));
    let mut probes = probes_with_library(vec![lib_entry(r"C:\Games\Game")]);
    let c = exe_calls.clone();
    probes.exe_path = Box::new(move |_| {
        *c.lock().unwrap() += 1;
        Some(PathBuf::from(r"C:\Games\Game\game.exe"))
    });
    let mut enricher = Enricher::with_probes(probes);

    let mut snaps = vec![snap(
        "game.exe",
        "Game",
        Rect::from_size(0, 0, 300, 200),
        400,
        4000,
    )];
    enricher.enrich(&mut snaps, None, 0);

    assert_eq!(
        *exe_calls.lock().unwrap(),
        0,
        "probes must not run for a tiny window"
    );
    assert!(!snaps[0].in_library_scan);
    let verdict = evaluate(&snaps[0], &DetectConfig::default());
    match verdict {
        Verdict::Rejected(_) => {}
        Verdict::Scored { accepted, .. } => assert!(!accepted),
    }
}

// ---------------------------------------------------------------------------
// Timing and live smoke
// ---------------------------------------------------------------------------

#[test]
fn enrich_50_fake_snapshots_is_fast() {
    let mut probes = probes_with_library(Vec::new());
    probes.exe_path = Box::new(|_| None);
    probes.modules = Box::new(|_| ModuleProbe::NoneLoaded);
    probes.is_known = Box::new(|_| false);
    let mut enricher = Enricher::with_probes(probes);

    let big = Rect::from_size(0, 0, 1280, 720);
    let mut snaps: Vec<CandidateSnapshot> = (0..50u32)
        .map(|i| {
            snap(
                &format!("Game{i}.exe"),
                "Game",
                big,
                1000 + i,
                5000 + u64::from(i),
            )
        })
        .collect();

    let start = Instant::now();
    enricher.enrich(&mut snaps, None, 0);
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(300), "took {elapsed:?}");
}

#[test]
fn live_win_source_smoke() {
    let mut src = WinSource::default();

    for round in 0..2 {
        let start = Instant::now();
        let snaps = src.snapshots();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(300),
            "snapshots() round {round} took {elapsed:?}"
        );
        for s in snaps.iter().filter(|s| !s.exe_name.is_empty()) {
            for v in [s.gpu_3d_load_pct, s.gpu_3d_avg60_pct]
                .into_iter()
                .flatten()
            {
                assert!(
                    (0.0..=100.0).contains(&v),
                    "gpu value {v} out of range for {}",
                    s.exe_name
                );
            }
        }
    }
}
