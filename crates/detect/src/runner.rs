//! Detection polling loop (docs/design/01-game-detection.md 5.4, 5.6, 5.12).
//!
//! [`DetectorCore::tick`] is the whole per-tick decision procedure and takes
//! its time as an argument, so tests drive it directly with fake sources.
//! [`DetectorLoop`] only wraps the core in a polling thread.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::debounce::{Debouncer, DetectEvent, Liveness, Pick, StartReason, StopReason};
use crate::enrich::Enricher;
use crate::lists::{normalize_exe, Lists};
use crate::score::{evaluate_with, Verdict};
use crate::timing::LIVENESS_POLL_MS;
use crate::{win, CandidateSnapshot, DetectConfig};

/// Points a challenger must beat the bound target by before a switch (5.6).
/// The debouncer then holds the switch for its own 2 s retarget confirm.
const RETARGET_MARGIN: u32 = 15;

/// Source of window snapshots and liveness facts.
///
/// The two liveness methods are called on every liveness check while a game is
/// bound (every [`LIVENESS_POLL_MS`]), so they must be cheap. They must not
/// depend on the last snapshot.
pub trait SnapshotSource {
    /// Candidate snapshots for this tick, topmost window first.
    fn snapshots(&mut self) -> Vec<CandidateSnapshot>;
    /// True while the window handle refers to a live window.
    fn is_window_alive(&mut self, hwnd: u64) -> bool;
    /// Handle of the current foreground window, if any.
    fn foreground(&mut self) -> Option<u64>;
    /// True while the process exists. Defaults to `true`.
    fn is_process_alive(&mut self, _pid: u32) -> bool {
        true
    }
}

/// Events emitted by the detection loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectorEvent {
    /// A game started recording.
    GameStarted {
        /// Bound window handle.
        hwnd: isize,
        /// Normalized exe name (the game identifier).
        exe: String,
        /// Window title, or the exe name when the title is empty.
        title: String,
        /// Owning process id.
        pid: Option<u32>,
    },
    /// A game stopped recording.
    GameStopped {
        /// Normalized exe name.
        exe: String,
    },
    /// The same game came back within the stop grace period.
    GameRelaunched {
        /// Newly bound window handle.
        hwnd: isize,
        /// Normalized exe name.
        exe: String,
        /// Window title, or the exe name when the title is empty.
        title: String,
        /// Owning process id.
        pid: Option<u32>,
        /// Time between loss and relaunch, in ms.
        gap_ms: u32,
    },
    /// The bound window of the same game changed without a stop.
    TargetChanged {
        /// Normalized exe name.
        exe: String,
        /// Previous window handle.
        old_hwnd: isize,
        /// New window handle.
        new_hwnd: isize,
        /// Owning process id of the new window.
        pid: Option<u32>,
    },
}

/// Pure per-tick detection state machine.
pub struct DetectorCore {
    cfg: DetectConfig,
    lists: Lists,
    debouncer: Debouncer,
    source: Box<dyn SnapshotSource + Send>,
    /// Target the debouncer currently records (set on start, relaunch and
    /// target change; cleared on stop).
    bound: Option<Pick>,
    /// Last accepted score of the bound window, as `(hwnd, score)`. Used as the
    /// hysteresis base on ticks where the bound window fails a gate, so a transient
    /// rejection does not drop the margin to zero.
    last_bound_score: Option<(u64, u32)>,
    /// Time of the last full scan (`tick`), if any.
    last_scan_ms: Option<u64>,
}

impl DetectorCore {
    /// Creates an idle core.
    pub fn new(cfg: DetectConfig, source: Box<dyn SnapshotSource + Send>) -> Self {
        let lists = Lists::from_config(&cfg);
        let debouncer = Debouncer::from_config(&cfg);
        Self {
            cfg,
            lists,
            debouncer,
            source,
            bound: None,
            last_bound_score: None,
            last_scan_ms: None,
        }
    }

    /// Switches allowlist-only mode. Takes effect on the next tick: an unpinned
    /// recording is released then if `on` is true.
    pub fn set_allowlist_only(&mut self, on: bool) {
        self.cfg.allowlist_only = on;
    }

    /// One scheduler step at `now_ms`. Runs a full scan when `scan_every_ms` has
    /// passed since the last one (or none ran yet). Otherwise, while a game is
    /// bound, runs the cheap liveness check ([`DetectorCore::liveness_tick`]).
    pub fn step(&mut self, now_ms: u64, scan_every_ms: u64) -> Vec<DetectorEvent> {
        let scan_due = match self.last_scan_ms {
            None => true,
            Some(t) => now_ms.saturating_sub(t) >= scan_every_ms,
        };
        if scan_due {
            self.tick(now_ms)
        } else if self.bound.is_some() {
            self.liveness_tick(now_ms)
        } else {
            Vec::new()
        }
    }

    /// Cheap liveness check of the bound target. Does not enumerate windows.
    ///
    /// Nothing happens while the bound target is alive and the game is recording.
    /// Any other state (a lost target, or a game already ending) runs a full
    /// [`DetectorCore::tick`] at once, so a replacement window is found in the same
    /// step and the stop decision uses fresh facts.
    pub fn liveness_tick(&mut self, now_ms: u64) -> Vec<DetectorEvent> {
        if self.bound.is_none() {
            return Vec::new();
        }
        if self.liveness_of() == Liveness::Alive && self.debouncer.is_active() {
            return Vec::new();
        }
        self.tick(now_ms)
    }

    /// Milliseconds until the next [`DetectorCore::step`] should run, given the
    /// time `now_ms` and the full-scan interval. While a game is bound this is
    /// capped at [`LIVENESS_POLL_MS`]. Always at least 1.
    pub fn next_delay_ms(&self, now_ms: u64, scan_every_ms: u64) -> u64 {
        let until_scan = match self.last_scan_ms {
            None => 0,
            Some(t) => scan_every_ms.saturating_sub(now_ms.saturating_sub(t)),
        };
        let delay = if self.bound.is_some() {
            until_scan.min(LIVENESS_POLL_MS)
        } else {
            until_scan
        };
        delay.max(1)
    }

    /// Liveness of the bound target, or `Alive` while nothing is bound. An
    /// unpinned recording released by allowlist-only mode reports `Released`.
    fn liveness_of(&mut self) -> Liveness {
        let Some(target) = self.bound.as_ref().map(|p| p.target) else {
            return Liveness::Alive;
        };
        if self.cfg.allowlist_only && self.bound.as_ref().is_some_and(|b| !b.pinned) {
            return Liveness::Released;
        }
        match (
            self.source.is_window_alive(target.hwnd),
            self.source.is_process_alive(target.pid),
        ) {
            (true, true) => Liveness::Alive,
            (false, true) => Liveness::WindowDestroyed,
            _ => Liveness::ProcessExited,
        }
    }

    /// Runs one full detection tick at `now_ms` and returns the resulting events.
    pub fn tick(&mut self, now_ms: u64) -> Vec<DetectorEvent> {
        self.last_scan_ms = Some(now_ms);
        let mut snaps = self.source.snapshots();
        let fg = self.source.foreground();
        let bound_hwnd = self.bound.as_ref().map(|p| p.target.hwnd);

        // Best eligible candidate. Ranking: pinned (allowlisted) first, then
        // foreground, then highest score; on a full tie the topmost window wins
        // (strict comparison keeps the first seen). In allowlist-only mode,
        // unpinned candidates are not eligible at all.
        // Winner as (index into `snaps`, score, is_fg, pinned). Its `Pick` is built
        // once after the scan, not for every candidate that beat the previous one.
        let mut best: Option<(usize, u32, bool, bool)> = None;
        // Score of the bound window this tick. If it fails a gate, fall back to its
        // last accepted score (same hwnd only) so the hysteresis margin still applies.
        let mut bound_scored: Option<u32> = None;
        // Every accepted candidate this tick, so a window that becomes the bound
        // target can have its score remembered at bind time.
        let mut accepted_scores: Vec<(u64, u32)> = Vec::new();
        for (idx, snap) in snaps.iter_mut().enumerate() {
            snap.is_running_game = Some(snap.target.hwnd) == bound_hwnd;
            let Verdict::Scored {
                score,
                accepted: true,
                ..
            } = evaluate_with(&self.lists, snap, &self.cfg)
            else {
                continue;
            };
            accepted_scores.push((snap.target.hwnd, score));
            let pinned = self.lists.is_allowlisted(&snap.exe_name);
            if self.cfg.allowlist_only && !pinned {
                continue;
            }
            if snap.is_running_game {
                bound_scored = Some(score);
                self.last_bound_score = Some((snap.target.hwnd, score));
            }
            let is_fg = fg == Some(snap.target.hwnd);
            let better = match &best {
                None => true,
                Some((_, s, f, p)) => (pinned, is_fg, score) > (*p, *f, *s),
            };
            if better {
                best = Some((idx, score, is_fg, pinned));
            }
        }
        // `snaps` is not used after this point, so the winner is moved out, not cloned.
        let best = best.map(|(idx, score, is_fg, pinned)| {
            (
                pick_from(snaps.swap_remove(idx), pinned),
                score,
                is_fg,
                pinned,
            )
        });

        let liveness = self.liveness_of();

        // Hysteresis (5.6): while the bound target is alive, the same game is
        // only switched to another window that beats it by RETARGET_MARGIN.
        // Other games never displace a recording; the debouncer keeps it alive.
        // The bound pick is cloned only when it is kept (the debouncer takes ownership).
        let chosen = match self.bound.as_ref() {
            Some(b) if liveness == Liveness::Alive => match best {
                Some((p, score, _, _)) if p.game == b.game && p.target != b.target => {
                    let remembered = match self.last_bound_score {
                        Some((hwnd, s)) if hwnd == b.target.hwnd => s,
                        _ => 0,
                    };
                    let bound_score = bound_scored.unwrap_or(remembered);
                    if score >= bound_score + RETARGET_MARGIN {
                        Some(p)
                    } else {
                        Some(b.clone())
                    }
                }
                Some((p, _, _, _)) if p.game == b.game => Some(p),
                _ => Some(b.clone()),
            },
            _ => best.map(|(p, _, _, _)| p),
        };

        let now = to_i64(now_ms);
        // `chosen` moves into the debouncer, which keeps it as its active pick.
        let raw = self.debouncer.update(now, chosen, liveness);
        let mut events = Vec::with_capacity(raw.len());
        // Last bound-changing event of this tick: Some(true) if a window became
        // bound, Some(false) if the game stopped.
        let mut rebound: Option<bool> = None;
        for ev in raw {
            events.push(match ev {
                DetectEvent::GameStarted {
                    game,
                    display_name,
                    target,
                    ..
                } => {
                    rebound = Some(true);
                    DetectorEvent::GameStarted {
                        hwnd: target.hwnd as isize,
                        exe: game,
                        title: display_name,
                        pid: Some(target.pid),
                    }
                }
                DetectEvent::GameRelaunched {
                    game,
                    display_name,
                    target,
                    gap_ms,
                    ..
                } => {
                    rebound = Some(true);
                    DetectorEvent::GameRelaunched {
                        hwnd: target.hwnd as isize,
                        exe: game,
                        title: display_name,
                        pid: Some(target.pid),
                        gap_ms,
                    }
                }
                DetectEvent::TargetChanged { game, old, new, .. } => {
                    rebound = Some(true);
                    DetectorEvent::TargetChanged {
                        exe: game,
                        old_hwnd: old.hwnd as isize,
                        new_hwnd: new.hwnd as isize,
                        pid: Some(new.pid),
                    }
                }
                DetectEvent::GameStopped { game, .. } => {
                    rebound = Some(false);
                    DetectorEvent::GameStopped { exe: game }
                }
            });
        }
        match rebound {
            Some(true) => {
                // The debouncer now holds the pick that became bound: one clone here,
                // instead of one per event arm and one per tick.
                self.bound = self.debouncer.active_pick().cloned();
                self.remember_bound_score(&accepted_scores);
            }
            Some(false) => {
                self.bound = None;
                self.last_bound_score = None;
            }
            None => {}
        }
        events
    }

    /// Remembers the score of the window that just became the bound target, so the
    /// hysteresis base exists even if it is rejected on the very next tick.
    fn remember_bound_score(&mut self, accepted: &[(u64, u32)]) {
        self.last_bound_score = self.bound.as_ref().and_then(|p| {
            accepted
                .iter()
                .find(|(hwnd, _)| *hwnd == p.target.hwnd)
                .copied()
        });
    }

    /// Stops any recording (service shutdown) and returns the resulting events.
    pub fn shutdown(&mut self, now_ms: u64) -> Vec<DetectorEvent> {
        let raw = self.debouncer.stop(to_i64(now_ms), StopReason::Shutdown);
        self.bound = None;
        self.last_bound_score = None;
        raw.into_iter()
            .filter_map(|ev| match ev {
                DetectEvent::GameStopped { game, .. } => {
                    Some(DetectorEvent::GameStopped { exe: game })
                }
                _ => None,
            })
            .collect()
    }
}

/// Builds the debouncer pick for an accepted candidate, consuming the snapshot so
/// its title or exe name is moved rather than cloned. `pinned` is the allowlist
/// result the caller already computed for `snap`.
fn pick_from(snap: CandidateSnapshot, pinned: bool) -> Pick {
    let game = normalize_exe(&snap.exe_name);
    let display_name = if snap.window_title.trim().is_empty() {
        snap.exe_name
    } else {
        snap.window_title
    };
    Pick {
        game,
        display_name,
        target: snap.target,
        reason: if pinned {
            StartReason::Allowlisted
        } else {
            StartReason::Detected
        },
        pinned,
        process_age_ms: snap.process_age_ms,
    }
}

fn to_i64(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// Handle to the polling thread started by [`DetectorLoop::spawn`].
pub struct DetectorLoop {
    stop_tx: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl DetectorLoop {
    /// Spawns the polling thread using the monotonic wall clock.
    pub fn spawn(
        cfg: DetectConfig,
        poll: Duration,
        source: Box<dyn SnapshotSource + Send>,
        tx: Sender<DetectorEvent>,
    ) -> DetectorLoop {
        let start = Instant::now();
        let clock =
            Box::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
        Self::spawn_with_clock(cfg, poll, source, tx, clock)
    }

    /// Spawns the polling thread with an injected millisecond clock.
    pub fn spawn_with_clock(
        cfg: DetectConfig,
        poll: Duration,
        source: Box<dyn SnapshotSource + Send>,
        tx: Sender<DetectorEvent>,
        clock: Box<dyn Fn() -> u64 + Send>,
    ) -> DetectorLoop {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let mut core = DetectorCore::new(cfg, source);
            let scan_every = u64::try_from(poll.as_millis()).unwrap_or(u64::MAX);
            loop {
                for ev in core.step(clock(), scan_every) {
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                let delay = Duration::from_millis(core.next_delay_ms(clock(), scan_every));
                match stop_rx.recv_timeout(delay) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            for ev in core.shutdown(clock()) {
                let _ = tx.send(ev);
            }
        });
        DetectorLoop {
            stop_tx: Some(stop_tx),
            handle: Some(handle),
        }
    }

    /// Signals the thread to stop, waits for it to finish, and consumes the handle.
    pub fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Production snapshot source backed by the Win32 probe layer.
///
/// Each snapshot is enriched by an [`Enricher`] (library, known-db, GPU, modules,
/// foreground time) before it reaches the scorer.
///
/// Liveness is cheap and does not depend on the last scan:
/// - the window is alive while `IsWindow` holds and it is visible or minimized
///   ([`win::window_alive`]);
/// - the process is alive while a handle opened on its pid is not signalled
///   ([`win::ProcessWatch`]). The handle pins the process object, so a reused pid
///   cannot be mistaken for the game. When the handle cannot be opened for a
///   reason other than the pid being gone (access denied), the process is judged
///   by the most recent snapshot instead, as before.
pub struct WinSource {
    pids: Vec<u32>,
    enricher: Enricher,
    procs: win::ProcessCache,
    started: Instant,
    /// Watch on the pid asked about most recently. Replaced when the pid changes.
    watch: Option<win::ProcessWatch>,
}

impl Default for WinSource {
    fn default() -> Self {
        Self {
            pids: Vec::new(),
            enricher: Enricher::new(),
            procs: win::ProcessCache::default(),
            started: Instant::now(),
            watch: None,
        }
    }
}

impl WinSource {
    /// Running state of `pid` from a process watch. `Some(false)` when the pid is
    /// gone, `None` when no answer is available (the caller then falls back to the
    /// snapshot).
    fn watched_process_running(&mut self, pid: u32) -> Option<bool> {
        if self.watch.as_ref().map(win::ProcessWatch::pid) != Some(pid) {
            // Dropping the old watch closes its handle.
            self.watch = None;
            match win::ProcessWatch::open(pid) {
                Ok(watch) => self.watch = Some(watch),
                Err(win::WatchError::Gone) => return Some(false),
                Err(win::WatchError::Denied) => return None,
            }
        }
        self.watch.as_ref().and_then(win::ProcessWatch::is_running)
    }
}

impl SnapshotSource for WinSource {
    fn snapshots(&mut self) -> Vec<CandidateSnapshot> {
        let mut snaps = win::enumerate_candidates_with(&mut self.procs);
        let now_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.enricher
            .enrich(&mut snaps, win::foreground_hwnd(), now_ms);
        self.pids = snaps.iter().map(|s| s.target.pid).collect();
        snaps
    }

    fn is_window_alive(&mut self, hwnd: u64) -> bool {
        win::window_alive(hwnd)
    }

    fn foreground(&mut self) -> Option<u64> {
        win::foreground_hwnd()
    }

    fn is_process_alive(&mut self, pid: u32) -> bool {
        self.watched_process_running(pid)
            .unwrap_or_else(|| self.pids.contains(&pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CandidateSnapshot, CaptureTarget, Rect};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const HWND_A: u64 = 1;
    const HWND_B: u64 = 2;

    /// Source with two windows of the same game. Window A can be made to fail
    /// the size gate (G5) through a shared flag.
    struct TwoWindows {
        a_rejected: Arc<AtomicBool>,
    }

    fn window(hwnd: u64, width: i32, height: i32) -> CandidateSnapshot {
        let monitor = Rect::from_size(0, 0, 1920, 1080);
        CandidateSnapshot {
            exe_name: "game.exe".into(),
            window_title: "Some Game".into(),
            window_rect: Rect::from_size(0, 0, width, height),
            monitor_rect: monitor,
            target: CaptureTarget {
                pid: 100 + hwnd as u32,
                hwnd,
            },
            is_visible: true,
            is_minimized: false,
            is_cloaked: false,
            is_tool_window: false,
            has_caption: false,
            is_launcher: false,
            is_primary_in_group: true,
            in_known_db: false,
            in_library_scan: false,
            gpu_3d_load_pct: Some(40.0),
            gpu_3d_avg60_pct: None,
            loaded_graphics_modules: vec!["d3d11.dll".into()],
            ms_since_foreground: Some(1_000),
            is_running_game: false,
            process_age_ms: 60_000,
        }
    }

    impl SnapshotSource for TwoWindows {
        fn snapshots(&mut self) -> Vec<CandidateSnapshot> {
            // Fullscreen 1920x1080 scores 65 (see score.rs worked example 1).
            let a = if self.a_rejected.load(Ordering::SeqCst) {
                window(HWND_A, 300, 200)
            } else {
                window(HWND_A, 1920, 1080)
            };
            vec![a, window(HWND_B, 1920, 1080)]
        }
        fn is_window_alive(&mut self, _hwnd: u64) -> bool {
            true
        }
        fn foreground(&mut self) -> Option<u64> {
            Some(HWND_A)
        }
    }

    /// Ticks every 500 ms and returns all events until `n` ticks have run.
    fn run(core: &mut DetectorCore, from_ms: u64, n: u64) -> Vec<DetectorEvent> {
        (0..n).flat_map(|i| core.tick(from_ms + i * 500)).collect()
    }

    #[test]
    fn rejected_bound_window_keeps_hysteresis_margin() {
        let a_rejected = Arc::new(AtomicBool::new(false));
        let mut core = DetectorCore::new(
            DetectConfig::default(),
            Box::new(TwoWindows {
                a_rejected: a_rejected.clone(),
            }),
        );

        // Start the recording on A (the foreground, highest-ranked window).
        let started = run(&mut core, 0, 40);
        assert!(
            started.iter().any(|e| matches!(
                e,
                DetectorEvent::GameStarted { hwnd, .. } if *hwnd == HWND_A as isize
            )),
            "expected GameStarted on window A, got {started:?}"
        );

        // A fails the size gate for several ticks. B scores 65, which is not
        // 15 points above A's last accepted score of 65, so no switch may happen.
        a_rejected.store(true, Ordering::SeqCst);
        let events = run(&mut core, 20_000, 16);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, DetectorEvent::TargetChanged { .. })),
            "no TargetChanged expected while bound window is rejected, got {events:?}"
        );
    }
}
