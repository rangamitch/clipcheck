//! Tests for device enumeration, session tracking and the meter math.
//!
//! Pure tests run on every platform. Live tests (real WASAPI endpoints and a
//! child tone-player process) live in the `live` module, which is Windows-only.

mod common;

use clipcheck_audio::meter::{peak_db, rms_db, SILENCE_DB};
use clipcheck_audio::{
    sort_endpoints, AudioSessionInfo, Endpoint, EndpointKind, SeenApps, SessionState,
};

// ---------------------------------------------------------------------------
// Pure: meter math
// ---------------------------------------------------------------------------

#[test]
fn empty_and_silent_buffers_are_exactly_silence() {
    assert_eq!(rms_db(&[]), -100.0);
    assert_eq!(peak_db(&[]), -100.0);
    assert_eq!(rms_db(&[0.0; 64]), -100.0);
    assert_eq!(peak_db(&[0.0; 64]), -100.0);
    assert_eq!(SILENCE_DB, -100.0);
}

#[test]
fn full_scale_square_peak_is_zero_db() {
    let p = peak_db(&[1.0, -1.0]);
    assert!((p - 0.0).abs() < 1e-4, "peak_db = {p}");
}

#[test]
fn full_scale_sine_rms_is_minus_3db_and_peak_is_zero_db() {
    let samples: Vec<f32> = (0..1000)
        .map(|k| (2.0 * std::f64::consts::PI * k as f64 / 100.0).sin() as f32)
        .collect();
    let rms = rms_db(&samples);
    let peak = peak_db(&samples);
    assert!((rms - (-3.01)).abs() < 0.05, "rms_db = {rms}");
    assert!((peak - 0.0).abs() < 0.01, "peak_db = {peak}");
}

#[test]
fn half_amplitude_constant_peak_is_minus_6db() {
    let p = peak_db(&[0.5; 32]);
    assert!((p - (-6.02)).abs() < 0.01, "peak_db = {p}");
}

#[test]
fn values_below_floor_clamp_to_silence() {
    assert_eq!(rms_db(&[1e-9; 128]), -100.0);
    assert_eq!(peak_db(&[1e-9; 128]), -100.0);
}

#[test]
fn nan_samples_do_not_panic_and_result_is_finite() {
    let samples = [0.5, f32::NAN, -0.25, 0.1];
    let r = rms_db(&samples);
    let p = peak_db(&samples);
    assert!(r.is_finite(), "rms_db = {r}");
    assert!(p.is_finite(), "peak_db = {p}");
    let all_nan = [f32::NAN; 8];
    assert!(rms_db(&all_nan).is_finite());
    assert!(peak_db(&all_nan).is_finite());
}

// ---------------------------------------------------------------------------
// Pure: endpoint ordering
// ---------------------------------------------------------------------------

fn endpoint(id: &str, name: &str, is_default: bool) -> Endpoint {
    Endpoint {
        id: id.to_owned(),
        name: name.to_owned(),
        is_default,
        kind: EndpointKind::Render,
    }
}

#[test]
fn default_endpoint_sorts_first_even_when_name_sorts_later() {
    let mut eps = vec![endpoint("1", "Alpha", false), endpoint("2", "Zeta", true)];
    sort_endpoints(&mut eps);
    assert_eq!(eps[0].id, "2");
    assert!(eps[0].is_default);
    assert_eq!(eps[1].id, "1");
}

#[test]
fn non_default_endpoints_sort_by_name_case_insensitively() {
    let mut eps = vec![endpoint("1", "Gamma", false), endpoint("2", "beta", false)];
    sort_endpoints(&mut eps);
    assert_eq!(eps[0].name, "beta");
    assert_eq!(eps[1].name, "Gamma");
}

#[test]
fn endpoint_ties_on_name_break_by_id() {
    let mut eps = vec![endpoint("b", "Same", false), endpoint("a", "same", false)];
    sort_endpoints(&mut eps);
    assert_eq!(eps[0].id, "a");
    assert_eq!(eps[1].id, "b");
}

// ---------------------------------------------------------------------------
// Pure: SeenApps tracker
// ---------------------------------------------------------------------------

fn session(exe: &str, state: SessionState, peak: f32, system: bool) -> AudioSessionInfo {
    AudioSessionInfo {
        pid: 1,
        exe_name: exe.to_owned(),
        display_name: exe.to_owned(),
        exe_path: None,
        state,
        peak,
        is_system_sounds: system,
    }
}

#[test]
fn observe_ignores_quiet_inactive_sessions() {
    let mut seen = SeenApps::new();
    seen.observe(
        &[session("quiet.exe", SessionState::Inactive, 0.001, false)],
        100,
    );
    assert!(seen.list(100, u64::MAX).is_empty());
}

#[test]
fn observe_counts_inactive_session_above_threshold() {
    let mut seen = SeenApps::new();
    seen.observe(
        &[session("loud.exe", SessionState::Inactive, 0.002, false)],
        100,
    );
    let list = seen.list(100, u64::MAX);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].exe_name, "loud.exe");
}

#[test]
fn observe_ignores_empty_exe_name_system_sounds() {
    let mut seen = SeenApps::new();
    seen.observe(&[session("", SessionState::Active, 0.9, true)], 100);
    assert!(seen.list(100, u64::MAX).is_empty());
}

#[test]
fn active_session_counts_even_at_zero_peak() {
    let mut seen = SeenApps::new();
    seen.observe(
        &[session("idle.exe", SessionState::Active, 0.0, false)],
        100,
    );
    assert_eq!(seen.list(100, u64::MAX).len(), 1);
}

#[test]
fn case_insensitive_merge_keeps_first_seen_and_updates_last_seen() {
    let mut seen = SeenApps::new();
    seen.observe(
        &[session("Discord.exe", SessionState::Active, 0.5, false)],
        100,
    );
    seen.observe(
        &[session("discord.exe", SessionState::Active, 0.5, false)],
        500,
    );
    let list = seen.list(500, u64::MAX);
    assert_eq!(list.len(), 1, "entries: {list:?}");
    assert_eq!(list[0].first_seen_ms, 100);
    assert_eq!(list[0].last_seen_ms, 500);
}

#[test]
fn list_is_sorted_most_recent_first() {
    let mut seen = SeenApps::new();
    seen.observe(&[session("a.exe", SessionState::Active, 0.5, false)], 100);
    seen.observe(&[session("b.exe", SessionState::Active, 0.5, false)], 300);
    seen.observe(&[session("c.exe", SessionState::Active, 0.5, false)], 200);
    let names: Vec<String> = seen
        .list(300, u64::MAX)
        .into_iter()
        .map(|a| a.exe_name)
        .collect();
    assert_eq!(names, ["b.exe", "c.exe", "a.exe"]);
}

#[test]
fn list_max_age_boundary_is_inclusive() {
    let mut seen = SeenApps::new();
    seen.observe(&[session("x.exe", SessionState::Active, 0.5, false)], 1000);
    assert_eq!(seen.list(1500, 500).len(), 1, "equal age must be included");
    assert!(
        seen.list(1501, 500).is_empty(),
        "older than max_age must be excluded"
    );
}

#[test]
fn forget_is_case_insensitive_and_unknown_is_noop() {
    let mut seen = SeenApps::new();
    seen.observe(
        &[session("Discord.exe", SessionState::Active, 0.5, false)],
        100,
    );
    seen.forget("nothere.exe");
    assert_eq!(seen.list(100, u64::MAX).len(), 1);
    seen.forget("DISCORD.EXE");
    assert!(seen.list(100, u64::MAX).is_empty());
}

// ---------------------------------------------------------------------------
// Live (Windows only): real endpoints, sessions, meter and a child tone player.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod live {
    use std::time::{Duration, Instant};

    use clipcheck_audio::meter::MeterReading;
    use clipcheck_audio::{
        list_endpoints, list_sessions, sort_endpoints, AudioError, EndpointKind, MicMeter,
        SessionState,
    };
    use clipcheck_testkit::tone::TONE_PLAYER_EXE;

    use crate::common;

    #[test]
    fn tone_session_is_listed_active_with_peak() {
        let renders = list_endpoints(EndpointKind::Render).expect("list render endpoints");
        if !renders.iter().any(|e| e.is_default) {
            eprintln!("no default render endpoint, skipping tone session test");
            return;
        }

        let mut tone = common::start_tone(660.0, 30);
        let tone_pid = tone.pid();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_active = false;
        let mut saw_peak = false;
        let mut last_row = None;
        while Instant::now() < deadline {
            if !tone.player.is_running() {
                panic!("tone player (pid {tone_pid}) exited early");
            }
            let rows = list_sessions().expect("list_sessions");
            // Match on the player's pid: other PowerShell windows may hold sessions too.
            for row in rows.into_iter().filter(|r| r.pid == tone_pid) {
                if row.state == SessionState::Active {
                    saw_active = true;
                }
                if row.peak > 0.0 {
                    saw_peak = true;
                }
                last_row = Some(row);
            }
            if saw_active && saw_peak {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        assert!(
            saw_active,
            "no Active session for tone player pid {tone_pid} within 5 s"
        );
        assert!(
            saw_peak,
            "tone player session (pid {tone_pid}) never reported peak > 0"
        );
        let row = last_row.expect("tone player row");
        assert!(
            row.exe_name.eq_ignore_ascii_case(TONE_PLAYER_EXE),
            "tone row has exe_name {:?}, expected {TONE_PLAYER_EXE}",
            row.exe_name
        );
        assert!(
            !row.is_system_sounds,
            "tone row flagged as system sounds: {row:?}"
        );
        assert!(row.exe_path.is_some(), "tone row has no exe_path: {row:?}");
        // `tone` drops here and kills the tone player.
    }

    #[test]
    fn render_endpoints_have_one_default_and_are_in_display_order() {
        let eps = list_endpoints(EndpointKind::Render).expect("list render endpoints");
        assert!(!eps.is_empty(), "no render endpoints");
        let defaults = eps.iter().filter(|e| e.is_default).count();
        assert_eq!(
            defaults, 1,
            "expected exactly one default render endpoint: {eps:?}"
        );
        assert!(
            eps.iter().all(|e| !e.name.is_empty()),
            "empty endpoint name: {eps:?}"
        );
        assert!(
            eps.iter().all(|e| e.kind == EndpointKind::Render),
            "non-render kind in render list: {eps:?}"
        );
        let mut ids: Vec<&str> = eps.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), eps.len(), "duplicate endpoint ids");
        assert!(eps[0].is_default, "default endpoint is not first: {eps:?}");

        let mut resorted = eps.clone();
        sort_endpoints(&mut resorted);
        assert_eq!(
            eps, resorted,
            "list_endpoints output is not in display order"
        );
    }

    #[test]
    fn capture_endpoints_list_ok_and_at_most_one_default() {
        let eps = list_endpoints(EndpointKind::Capture).expect("list capture endpoints");
        if eps.is_empty() {
            println!("no capture endpoint, skipping");
            return;
        }
        let defaults = eps.iter().filter(|e| e.is_default).count();
        assert_eq!(
            defaults, 1,
            "expected exactly one default capture endpoint: {eps:?}"
        );
        assert!(eps.iter().all(|e| e.kind == EndpointKind::Capture));
    }

    #[test]
    fn mic_meter_streams_sane_readings_for_one_second() {
        let captures = list_endpoints(EndpointKind::Capture).expect("list capture endpoints");
        if captures.is_empty() {
            println!("no capture endpoint, skipping MicMeter test");
            return;
        }

        let (mut meter, rx) = match MicMeter::start(None) {
            Ok(pair) => pair,
            Err(AudioError::NoDevice) => {
                println!("MicMeter::start returned NoDevice, skipping");
                return;
            }
            Err(e) => panic!("MicMeter::start failed: {e:?}"),
        };

        let end = Instant::now() + Duration::from_secs(1);
        let mut readings: Vec<MeterReading> = Vec::new();
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(r) => readings.push(r),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        meter.stop();

        assert!(
            readings.len() >= 5,
            "only {} readings in 1 s",
            readings.len()
        );
        for (i, r) in readings.iter().enumerate() {
            assert!(r.rms_db.is_finite(), "reading {i} rms not finite: {r:?}");
            assert!(r.peak_db.is_finite(), "reading {i} peak not finite: {r:?}");
            assert!(
                (-100.0..=0.0).contains(&r.rms_db),
                "reading {i} rms out of range: {r:?}"
            );
            assert!(
                (-100.0..=0.0).contains(&r.peak_db),
                "reading {i} peak out of range: {r:?}"
            );
            assert!(
                r.peak_db >= r.rms_db - 1e-3,
                "reading {i} peak below rms: {r:?}"
            );
        }
    }
}
