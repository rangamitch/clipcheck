//! Fills the signals that the plain Win32 probe leaves neutral (task D1).
//!
//! [`crate::win::enumerate_candidates`] only reads window metadata. [`Enricher`]
//! adds the rest: library match, known-games match, rolling GPU 3D averages,
//! graphics modules and time since foreground. A cheap pre-filter keeps the
//! expensive probes away from windows that can never be games.
//!
//! Every OS probe is injected through [`Probes`], so tests run without touching
//! the operating system. [`Enricher::new`] wires the real probes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use crate::gpu::{GpuMonitor, GpuTracker};
use crate::known_games;
use crate::library::LibraryCache;
use crate::lists::Lists;
use crate::modules::{self, ModuleCache, ModuleProbe};
use crate::CandidateSnapshot;

/// Minimum interval between two GPU samples.
pub const GPU_SAMPLE_INTERVAL_MS: u64 = 1_000;
/// Window of the short GPU average (`gpu_3d_load_pct`).
pub const GPU_SHORT_WINDOW_MS: u64 = 5_000;
/// Window of the long GPU average (`gpu_3d_avg60_pct`).
pub const GPU_LONG_WINDOW_MS: u64 = 60_000;
/// Samples a pid needs inside a window before an average is reported.
const GPU_MIN_SAMPLES: usize = 2;
/// Pre-filter: smallest window width that can be a game.
const MIN_WIDTH: i64 = 400;
/// Pre-filter: smallest window height that can be a game.
const MIN_HEIGHT: i64 = 300;

/// Returns summed 3D utilization per pid, or `None` when sampling failed.
pub type GpuSampler = Box<dyn FnMut() -> Option<HashMap<u32, f32>> + Send>;
/// Probes the graphics modules loaded in a process.
pub type ModuleProber = Box<dyn FnMut(u32) -> ModuleProbe + Send>;
/// Returns the executable path of a process, if it can be read.
pub type ExePathProber = Box<dyn FnMut(u32) -> Option<PathBuf> + Send>;

/// Injected OS probes. [`Probes::system`] wires the real ones.
pub struct Probes {
    /// Installed-game library cache (rescans at most once a minute).
    pub library: LibraryCache,
    /// GPU sampler. `None` means no GPU data at all.
    pub gpu: Option<GpuSampler>,
    /// Graphics module probe; wrapped in a per-pid cache by the enricher.
    pub modules: ModuleProber,
    /// Executable path lookup by pid.
    pub exe_path: ExePathProber,
    /// Known-games lookup by exe file name.
    pub is_known: Box<dyn Fn(&str) -> bool + Send>,
}

impl Probes {
    /// Real Windows probes. A GPU monitor that cannot be created yields `gpu: None`.
    pub fn system() -> Self {
        let gpu = GpuMonitor::new()
            .ok()
            .map(|mut monitor| -> GpuSampler { Box::new(move || monitor.sample().ok()) });
        Self {
            library: LibraryCache::new(),
            gpu,
            modules: Box::new(modules::graphics_modules),
            exe_path: Box::new(modules::exe_path),
            is_known: Box::new(|exe| known_games::is_known(exe).is_some()),
        }
    }
}

/// Stateful enricher that turns neutral snapshots into scoring-ready ones.
pub struct Enricher {
    library: LibraryCache,
    gpu: Option<GpuSampler>,
    gpu_tracker: GpuTracker,
    gpu_last_attempt_ms: Option<u64>,
    /// Sample times at which each pid was present in a successful GPU sample.
    gpu_seen: HashMap<u32, VecDeque<u64>>,
    modules: ModuleCache<ModuleProber>,
    exe_path: ExePathProber,
    exe_cache: HashMap<u32, Option<PathBuf>>,
    is_known: Box<dyn Fn(&str) -> bool + Send>,
    /// Built-in lists only; the pre-filter never needs user configuration.
    lists: Lists,
    /// Last `now_ms` at which each window handle was the foreground window.
    fg_seen: HashMap<u64, u64>,
}

impl Enricher {
    /// Enricher backed by the real Windows probes.
    pub fn new() -> Self {
        Self::with_probes(Probes::system())
    }

    /// Enricher backed by the given probes.
    pub fn with_probes(probes: Probes) -> Self {
        let Probes {
            library,
            gpu,
            modules,
            exe_path,
            is_known,
        } = probes;
        Self {
            library,
            gpu,
            gpu_tracker: GpuTracker::new(),
            gpu_last_attempt_ms: None,
            gpu_seen: HashMap::new(),
            modules: ModuleCache::with_probe(modules),
            exe_path,
            exe_cache: HashMap::new(),
            is_known,
            lists: Lists::new(&[], &[]),
            fg_seen: HashMap::new(),
        }
    }

    /// Fills the enrichment fields of `snaps` for the poll at `now_ms`.
    ///
    /// The pid of each window is taken from `snap.target.pid`. Only windows that
    /// pass the cheap pre-filter get the library, known-db, module and GPU fields;
    /// every window gets `ms_since_foreground`. Call this once per poll, with a
    /// monotonic clock. Never fails: a missing probe leaves its field neutral.
    pub fn enrich(
        &mut self,
        snaps: &mut [CandidateSnapshot],
        foreground_hwnd: Option<u64>,
        now_ms: u64,
    ) {
        if let Some(hwnd) = foreground_hwnd {
            self.fg_seen.insert(hwnd, now_ms);
        }
        let live_hwnds: HashSet<u64> = snaps.iter().map(|s| s.target.hwnd).collect();
        self.fg_seen.retain(|hwnd, _| live_hwnds.contains(hwnd));

        let live_pids: HashSet<u32> = snaps.iter().map(|s| s.target.pid).collect();
        self.exe_cache.retain(|pid, _| live_pids.contains(pid));
        self.modules.forget_dead(&live_pids);

        self.sample_gpu(now_ms);

        for snap in snaps.iter_mut() {
            snap.ms_since_foreground = self
                .fg_seen
                .get(&snap.target.hwnd)
                .map(|&t| now_ms.saturating_sub(t));

            if !self.is_candidate(snap) {
                continue;
            }
            let pid = snap.target.pid;
            let exe_path = self.exe_path_for(pid);
            snap.in_known_db = (self.is_known)(&snap.exe_name);
            snap.in_library_scan = exe_path
                .as_deref()
                .is_some_and(|path| self.library.get(now_ms).find_for_exe(path).is_some());
            snap.loaded_graphics_modules = match self.modules.get(pid, now_ms) {
                ModuleProbe::Found(names) => names.clone(),
                _ => Vec::new(),
            };
            snap.gpu_3d_load_pct = self.gpu_avg(pid, GPU_SHORT_WINDOW_MS, now_ms);
            snap.gpu_3d_avg60_pct = self.gpu_avg(pid, GPU_LONG_WINDOW_MS, now_ms);
        }
    }

    /// Cheap pre-filter: only windows that could be a game get the probes.
    fn is_candidate(&self, snap: &CandidateSnapshot) -> bool {
        if !snap.is_visible || snap.is_tool_window || snap.is_cloaked || snap.is_launcher {
            return false;
        }
        if snap.window_rect.width() < MIN_WIDTH || snap.window_rect.height() < MIN_HEIGHT {
            return false;
        }
        let exe = snap.exe_name.trim();
        if exe.is_empty() {
            return false;
        }
        !(self.lists.is_blocked(exe)
            || self.lists.is_system_shell(exe)
            || self.lists.is_launcher(exe))
    }

    /// Executable path for `pid`, cached until the pid disappears.
    fn exe_path_for(&mut self, pid: u32) -> Option<PathBuf> {
        if let Some(cached) = self.exe_cache.get(&pid) {
            return cached.clone();
        }
        let path = (self.exe_path)(pid);
        self.exe_cache.insert(pid, path.clone());
        path
    }

    /// Takes a GPU sample if at least [`GPU_SAMPLE_INTERVAL_MS`] has passed.
    /// A failed sample is simply skipped.
    fn sample_gpu(&mut self, now_ms: u64) {
        let due = match self.gpu_last_attempt_ms {
            None => true,
            Some(last) => now_ms < last || now_ms - last >= GPU_SAMPLE_INTERVAL_MS,
        };
        if !due {
            return;
        }
        let Some(sampler) = self.gpu.as_mut() else {
            return;
        };
        self.gpu_last_attempt_ms = Some(now_ms);
        let Some(samples) = sampler() else {
            return;
        };

        self.gpu_tracker.push(now_ms, &samples);
        let cutoff = now_ms.saturating_sub(GPU_LONG_WINDOW_MS);
        for &pid in samples.keys() {
            self.gpu_seen.entry(pid).or_default().push_back(now_ms);
        }
        self.gpu_seen.retain(|_, times| {
            while times.front().is_some_and(|&t| t < cutoff) {
                times.pop_front();
            }
            !times.is_empty()
        });
    }

    /// Rolling GPU average for `pid`, or `None` until it has enough samples in the window.
    fn gpu_avg(&self, pid: u32, window_ms: u64, now_ms: u64) -> Option<f32> {
        let times = self.gpu_seen.get(&pid)?;
        let from = now_ms.saturating_sub(window_ms);
        let count = times.iter().filter(|&&t| t >= from && t <= now_ms).count();
        if count < GPU_MIN_SAMPLES {
            return None;
        }
        self.gpu_tracker.avg(pid, window_ms, now_ms)
    }
}

impl Default for Enricher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryScan;
    use crate::Rect;

    fn snap(pid: u32, hwnd: u64, exe: &str) -> CandidateSnapshot {
        CandidateSnapshot {
            exe_name: exe.into(),
            window_title: "Window".into(),
            window_rect: Rect::from_size(0, 0, 1280, 720),
            monitor_rect: Rect::from_size(0, 0, 1920, 1080),
            target: crate::CaptureTarget { pid, hwnd },
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
            process_age_ms: 0,
        }
    }

    fn empty_probes() -> Probes {
        Probes {
            library: LibraryCache::with_scanner(LibraryScan::default),
            gpu: None,
            modules: Box::new(|_| ModuleProbe::NoneLoaded),
            exe_path: Box::new(|_| None),
            is_known: Box::new(|_| false),
        }
    }

    #[test]
    fn pre_filter_skips_tiny_tool_and_blocked_windows() {
        let enricher = Enricher::with_probes(empty_probes());
        let mut tiny = snap(1, 1, "game.exe");
        tiny.window_rect = Rect::from_size(0, 0, 300, 200);
        assert!(!enricher.is_candidate(&tiny));
        let mut tool = snap(1, 1, "game.exe");
        tool.is_tool_window = true;
        assert!(!enricher.is_candidate(&tool));
        assert!(!enricher.is_candidate(&snap(1, 1, "chrome.exe")));
        assert!(!enricher.is_candidate(&snap(1, 1, "")));
        assert!(enricher.is_candidate(&snap(1, 1, "game.exe")));
    }

    #[test]
    fn gpu_average_needs_two_samples() {
        let mut probes = empty_probes();
        probes.gpu = Some(Box::new(|| Some(HashMap::from([(7u32, 50.0f32)]))));
        let mut enricher = Enricher::with_probes(probes);
        let mut snaps = vec![snap(7, 70, "game.exe")];
        enricher.enrich(&mut snaps, None, 0);
        assert_eq!(snaps[0].gpu_3d_load_pct, None);
        enricher.enrich(&mut snaps, None, 1_000);
        assert_eq!(snaps[0].gpu_3d_load_pct, Some(50.0));
    }

    #[test]
    fn foreground_time_is_tracked() {
        let mut enricher = Enricher::with_probes(empty_probes());
        let mut snaps = vec![snap(1, 10, "game.exe")];
        enricher.enrich(&mut snaps, Some(10), 1_000);
        assert_eq!(snaps[0].ms_since_foreground, Some(0));
        enricher.enrich(&mut snaps, None, 4_000);
        assert_eq!(snaps[0].ms_since_foreground, Some(3_000));
    }
}
