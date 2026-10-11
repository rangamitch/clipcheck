//! Game close-detection latency (docs/design/01-game-detection.md 5.1, 5.3, 5.4, 5.12).
//!
//! Public API only, with simulated time. `drive` runs the detector the way its
//! thread does (`step`, then sleep `next_delay_ms`), so the measured latencies
//! follow the real cadence rules rather than a hand-picked tick grid.

use clipcheck_detect::debounce::{Debouncer, DetectEvent, Liveness, Pick, StartReason, StopReason};
use clipcheck_detect::timing::{LIVENESS_POLL_MS, WINDOW_GONE_GRACE_MS};
use clipcheck_detect::CaptureTarget;

const T1: CaptureTarget = CaptureTarget { pid: 10, hwnd: 100 };

fn pick(target: CaptureTarget) -> Pick {
    Pick {
        game: "g".into(),
        display_name: "G".into(),
        target,
        reason: StartReason::Detected,
        pinned: false,
        process_age_ms: 60_000,
    }
}

#[test]
fn timing_constants_are_the_documented_values() {
    assert_eq!(LIVENESS_POLL_MS, 250);
    assert_eq!(WINDOW_GONE_GRACE_MS, 1_000);
}

#[test]
fn process_exit_stops_at_the_first_observation_without_grace() {
    let mut d = Debouncer::new(0, 10_000);
    d.update(0, Some(pick(T1)), Liveness::Alive);
    let ev = d.update(250, None, Liveness::ProcessExited);
    assert_eq!(
        ev,
        vec![DetectEvent::GameStopped {
            game: "g".into(),
            reason: StopReason::ProcessExited,
            at_ms: 250,
        }]
    );
    assert!(!d.is_active());
}

#[test]
fn window_destroyed_with_live_process_stops_only_after_the_grace() {
    let grace = i64::from(WINDOW_GONE_GRACE_MS);
    let mut d = Debouncer::new(0, 10_000);
    d.update(0, Some(pick(T1)), Liveness::Alive);
    d.update(1_000, None, Liveness::WindowDestroyed);
    assert!(d
        .update(1_000 + grace - 1, None, Liveness::WindowDestroyed)
        .is_empty());
    let ev = d.update(1_000 + grace, None, Liveness::WindowDestroyed);
    assert!(matches!(
        ev.as_slice(),
        [DetectEvent::GameStopped {
            reason: StopReason::WindowDestroyed,
            ..
        }]
    ));
}

#[test]
fn transient_failed_probe_inside_the_grace_does_not_stop() {
    let mut d = Debouncer::new(0, 10_000);
    d.update(0, Some(pick(T1)), Liveness::Alive);
    d.update(1_000, None, Liveness::WindowDestroyed);
    assert!(d.update(1_250, None, Liveness::Alive).is_empty());
    assert!(d.update(60_000, None, Liveness::Alive).is_empty());
    assert!(d.is_active());
}

// ---------------------------------------------------------------------------
// detector loop: simulated time through DetectorCore (Windows-only module)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod loop_latency {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use clipcheck_detect::runner::{DetectorCore, DetectorEvent, SnapshotSource};
    use clipcheck_detect::timing::{LIVENESS_POLL_MS, WINDOW_GONE_GRACE_MS};
    use clipcheck_detect::{CandidateSnapshot, CaptureTarget, DetectConfig, Rect};

    /// Full-scan interval: the service default `advanced.detect_poll_ms`.
    const SCAN_EVERY_MS: u64 = 1_000;
    const PID: u32 = 10;
    const HWND: u64 = 100;

    #[derive(Default)]
    struct World {
        snapshots: Vec<CandidateSnapshot>,
        alive_hwnds: HashSet<u64>,
        alive_pids: HashSet<u32>,
        /// Next window probes that report "not alive" while the window stays listed.
        failing_window_probes: u32,
        scans: u32,
        window_probes: u32,
    }

    type Shared = Arc<Mutex<World>>;

    struct Fake(Shared);

    impl SnapshotSource for Fake {
        fn snapshots(&mut self) -> Vec<CandidateSnapshot> {
            let mut w = self.0.lock().unwrap();
            w.scans += 1;
            w.snapshots.clone()
        }
        fn is_window_alive(&mut self, hwnd: u64) -> bool {
            let mut w = self.0.lock().unwrap();
            w.window_probes += 1;
            if w.failing_window_probes > 0 {
                w.failing_window_probes -= 1;
                return false;
            }
            w.alive_hwnds.contains(&hwnd)
        }
        fn foreground(&mut self) -> Option<u64> {
            Some(HWND)
        }
        fn is_process_alive(&mut self, pid: u32) -> bool {
            self.0.lock().unwrap().alive_pids.contains(&pid)
        }
    }

    /// Known-game fullscreen window: eligible, same fixture as the runner tests.
    fn game_snap(hwnd: u64, pid: u32) -> CandidateSnapshot {
        CandidateSnapshot {
            exe_name: "game.exe".into(),
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

    fn add_window(w: &mut World, hwnd: u64, pid: u32) {
        w.snapshots.push(game_snap(hwnd, pid));
        w.alive_hwnds.insert(hwnd);
        w.alive_pids.insert(pid);
    }

    fn remove_window(w: &mut World, hwnd: u64) {
        w.snapshots.retain(|s| s.target.hwnd != hwnd);
        w.alive_hwnds.remove(&hwnd);
    }

    /// Removes every window of `pid` and the process itself.
    fn exit_process(w: &mut World, pid: u32) {
        let hwnds: Vec<u64> = w
            .snapshots
            .iter()
            .filter(|s| s.target.pid == pid)
            .map(|s| s.target.hwnd)
            .collect();
        for hwnd in hwnds {
            remove_window(w, hwnd);
        }
        w.alive_pids.remove(&pid);
    }

    fn running_world() -> Shared {
        let mut w = World::default();
        add_window(&mut w, HWND, PID);
        Arc::new(Mutex::new(w))
    }

    /// Runs the detector the way its thread does: `step`, then sleep the reported
    /// delay. `script` runs at each sample time before the step and may change the
    /// world. Returns every event with the sample time it was emitted at.
    fn drive(
        core: &mut DetectorCore,
        from: u64,
        until: u64,
        mut script: impl FnMut(u64),
    ) -> Vec<(u64, DetectorEvent)> {
        let mut out = Vec::new();
        let mut now = from;
        while now <= until {
            script(now);
            for ev in core.step(now, SCAN_EVERY_MS) {
                out.push((now, ev));
            }
            now += core.next_delay_ms(now, SCAN_EVERY_MS);
        }
        out
    }

    fn stop_times(events: &[(u64, DetectorEvent)]) -> Vec<u64> {
        events
            .iter()
            .filter(|(_, e)| matches!(e, DetectorEvent::GameStopped { .. }))
            .map(|(t, _)| *t)
            .collect()
    }

    #[test]
    fn process_exit_ends_within_one_liveness_poll() {
        let world = running_world();
        let mut core = DetectorCore::new(DetectConfig::default(), Box::new(Fake(world.clone())));
        let exit_seen: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let (w, seen) = (world.clone(), exit_seen.clone());
        let events = drive(&mut core, 0, 9_000, move |now| {
            let mut seen = seen.lock().unwrap();
            if now >= 6_000 && seen.is_none() {
                exit_process(&mut w.lock().unwrap(), PID);
                *seen = Some(now);
            }
        });

        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, DetectorEvent::GameStarted { .. })),
            "game should have started before the exit: {events:?}"
        );
        let exit_at = exit_seen.lock().unwrap().expect("exit was scripted");
        let stops = stop_times(&events);
        assert_eq!(stops.len(), 1, "exactly one GameStopped: {events:?}");
        let latency = stops[0] - exit_at;
        assert!(
            latency <= LIVENESS_POLL_MS,
            "process exit at {exit_at} ms stopped at {} ms ({latency} ms later), limit {LIVENESS_POLL_MS} ms",
            stops[0]
        );
    }

    #[test]
    fn window_destroyed_with_live_process_ends_only_after_the_grace() {
        let world = running_world();
        let mut core = DetectorCore::new(DetectConfig::default(), Box::new(Fake(world.clone())));
        let loss_seen: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let (w, seen) = (world.clone(), loss_seen.clone());
        let events = drive(&mut core, 0, 9_000, move |now| {
            let mut seen = seen.lock().unwrap();
            if now >= 6_000 && seen.is_none() {
                // The window goes; its process keeps running (no window listed for it).
                remove_window(&mut w.lock().unwrap(), HWND);
                *seen = Some(now);
            }
        });

        let loss = loss_seen.lock().unwrap().expect("loss was scripted");
        let stops = stop_times(&events);
        assert_eq!(stops.len(), 1, "exactly one GameStopped: {events:?}");
        let grace = u64::from(WINDOW_GONE_GRACE_MS);
        assert!(
            stops[0] >= loss + grace,
            "stopped at {} before the {grace} ms grace after {loss}",
            stops[0]
        );
        assert!(
            stops[0] <= loss + grace + LIVENESS_POLL_MS,
            "stopped at {} later than grace plus one poll after {loss}",
            stops[0]
        );
    }

    #[test]
    fn window_relaunch_within_the_grace_keeps_the_session() {
        let world = running_world();
        let mut core = DetectorCore::new(DetectConfig::default(), Box::new(Fake(world.clone())));
        let stage = Arc::new(Mutex::new(0u8));
        let w = world.clone();
        let st = stage.clone();
        let events = drive(&mut core, 0, 9_000, move |now| {
            let mut stage = st.lock().unwrap();
            let mut w = w.lock().unwrap();
            if *stage == 0 && now >= 6_000 {
                remove_window(&mut w, HWND);
                *stage = 1;
            } else if *stage == 1 && now >= 6_500 {
                // A window of the same game from another process appears 500 ms later.
                add_window(&mut w, 200, 11);
                *stage = 2;
            }
        });

        assert!(
            stop_times(&events).is_empty(),
            "no stop expected: {events:?}"
        );
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                DetectorEvent::GameRelaunched { hwnd, .. } if *hwnd == 200
            )),
            "expected GameRelaunched on the new window: {events:?}"
        );
    }

    #[test]
    fn transient_failed_window_probes_do_not_stop_a_live_process() {
        for failing in 1..=3 {
            let world = running_world();
            let mut core =
                DetectorCore::new(DetectConfig::default(), Box::new(Fake(world.clone())));
            let w = world.clone();
            let mut done = false;
            let events = drive(&mut core, 0, 9_000, move |now| {
                if !done && now >= 6_000 {
                    w.lock().unwrap().failing_window_probes = failing;
                    done = true;
                }
            });
            let started = events
                .iter()
                .filter(|(_, e)| matches!(e, DetectorEvent::GameStarted { .. }))
                .count();
            assert_eq!(started, 1, "failing={failing}: {events:?}");
            assert_eq!(
                events.len(),
                1,
                "failing={failing}: only the start is expected, got {events:?}"
            );
        }
    }

    #[test]
    fn liveness_is_checked_every_poll_while_full_scans_keep_their_interval() {
        let world = running_world();
        let mut core = DetectorCore::new(DetectConfig::default(), Box::new(Fake(world.clone())));
        let events = drive(&mut core, 0, 6_000, |_| {});
        assert!(events
            .iter()
            .any(|(_, e)| matches!(e, DetectorEvent::GameStarted { .. })));

        let w = world.lock().unwrap();
        // Bound from 3 s to 6 s: about one liveness check per 250 ms, but full scans
        // (window enumerations) stay at the 1 s scan interval.
        assert!(
            w.window_probes >= 12,
            "expected >= 12 liveness probes, got {}",
            w.window_probes
        );
        assert!(w.scans <= 8, "expected <= 8 full scans, got {}", w.scans);
    }
}

// ---------------------------------------------------------------------------
// Win32 probes (real OS, harmless processes only: this process and a `cmd /C exit`)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win_probes {
    use clipcheck_detect::win::{window_alive, ProcessWatch, WatchError};

    #[test]
    fn watch_on_own_process_reports_running() {
        let watch = ProcessWatch::open(std::process::id()).expect("own pid is watchable");
        assert_eq!(watch.pid(), std::process::id());
        assert_eq!(watch.is_running(), Some(true));
    }

    #[test]
    fn watch_on_exited_child_reports_exit() {
        // About two seconds of ping keeps the child alive while the watch opens.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 3 127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cmd starts");
        let watch = ProcessWatch::open(child.id()).expect("child pid is watchable");
        assert_eq!(watch.is_running(), Some(true));
        child.wait().expect("child exits");
        assert_eq!(watch.is_running(), Some(false));
    }

    #[test]
    fn watch_on_missing_pid_is_gone() {
        // Pid values are multiples of 4; this one is far outside any live pid.
        assert_eq!(
            ProcessWatch::open(0xFFFF_FFF0).err(),
            Some(WatchError::Gone)
        );
    }

    #[test]
    fn missing_window_is_not_alive() {
        assert!(!window_alive(0));
    }
}
