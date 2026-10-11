use clipcheck_detect::gpu::{parse_instance, GpuInstance, GpuTracker};
use std::collections::HashMap;

fn map(pairs: &[(u32, f32)]) -> HashMap<u32, f32> {
    pairs.iter().copied().collect()
}

fn assert_avg(actual: Option<f32>, expected: f32) {
    match actual {
        Some(v) => assert!(
            (v - expected).abs() < 1e-4,
            "expected avg {expected}, got {v}"
        ),
        None => panic!("expected Some({expected}), got None"),
    }
}

// ---------- parse_instance ----------

#[test]
fn parse_full_name_3d() {
    let got = parse_instance("pid_12345_luid_0x00000000_0x0000F3A1_phys_0_eng_0_engtype_3D");
    assert_eq!(
        got,
        Some(GpuInstance {
            pid: 12345,
            engtype: "3D".to_string()
        })
    );
}

#[test]
fn parse_compute_engtype() {
    let got = parse_instance("pid_7_luid_0x0_0x1_phys_1_eng_2_engtype_Compute");
    assert_eq!(
        got,
        Some(GpuInstance {
            pid: 7,
            engtype: "Compute".to_string()
        })
    );
}

#[test]
fn parse_video_decode_and_copy() {
    let v = parse_instance("pid_42_luid_0x00000000_0x00001234_phys_0_eng_5_engtype_VideoDecode");
    assert_eq!(v.map(|i| i.engtype), Some("VideoDecode".to_string()));

    let c = parse_instance("pid_42_luid_0x00000000_0x00001234_phys_0_eng_6_engtype_Copy");
    assert_eq!(c.map(|i| i.engtype), Some("Copy".to_string()));
}

#[test]
fn parse_pid_zero_accepted() {
    let got = parse_instance("pid_0_luid_0x0_0x0_phys_0_eng_0_engtype_3D");
    assert_eq!(
        got,
        Some(GpuInstance {
            pid: 0,
            engtype: "3D".to_string()
        })
    );
}

#[test]
fn parse_without_engtype_segment_gives_empty() {
    let got = parse_instance("pid_99_luid_0x0_0x0");
    assert_eq!(
        got,
        Some(GpuInstance {
            pid: 99,
            engtype: String::new()
        })
    );
}

#[test]
fn parse_marker_is_case_insensitive() {
    let got = parse_instance("PID_5_engtype_3D");
    assert_eq!(got.map(|i| i.pid), Some(5));
}

#[test]
fn parse_garbage_is_none() {
    for input in [
        "",
        "garbage",
        "pid_",
        "pid_abc_engtype_3D",
        "pid__engtype_3D",
        "xpid_5_engtype_3D",
        "  ",
    ] {
        assert_eq!(parse_instance(input), None, "input {input:?}");
    }
}

#[test]
fn parse_pid_overflow_is_none() {
    assert_eq!(parse_instance("pid_99999999999_engtype_3D"), None);
}

#[test]
fn parse_surrounding_whitespace_trimmed() {
    let got = parse_instance("  pid_3_engtype_3D  ");
    assert_eq!(
        got,
        Some(GpuInstance {
            pid: 3,
            engtype: "3D".to_string()
        })
    );
}

#[test]
fn parse_non_ascii_does_not_panic() {
    let _ = parse_instance("pid_é");
    let got = parse_instance("pid_1_engtype_é");
    assert_eq!(got.map(|i| i.pid), Some(1));
}

#[test]
fn parse_trailing_segment_after_engtype_ignored() {
    let got = parse_instance("pid_8_engtype_3D_0");
    assert_eq!(got.map(|i| i.engtype), Some("3D".to_string()));
}

// ---------- GpuTracker ----------

#[test]
fn single_push_avg() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 40.0)]));
    assert_avg(t.avg(1, 5000, 0), 40.0);
}

#[test]
fn multiple_pushes_avg() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 10.0)]));
    t.push(1000, &map(&[(1, 20.0)]));
    t.push(2000, &map(&[(1, 30.0)]));
    assert_avg(t.avg(1, 5000, 2000), 20.0);
}

#[test]
fn window_excludes_old_samples() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 100.0)]));
    t.push(6000, &map(&[(1, 10.0)]));
    t.push(7000, &map(&[(1, 20.0)]));
    assert_avg(t.avg(1, 5000, 7000), 15.0);
    assert_avg(t.avg(1, 60000, 7000), 130.0 / 3.0);
}

#[test]
fn partial_window_uses_only_in_window_samples() {
    // Old sample at t=0 falls outside a 5 s window ending at now=10_000;
    // only the t=8000 sample is in-window.
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 10.0)]));
    t.push(8000, &map(&[(1, 70.0)]));
    assert_avg(t.avg(1, 5000, 8000), 70.0);
}

#[test]
fn unknown_pid_is_none() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 40.0)]));
    assert_eq!(t.avg(2, 5000, 0), None);
}

#[test]
fn empty_window_is_none() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 40.0)]));
    assert_eq!(t.avg(1, 5000, 10_000), None);
}

#[test]
fn missing_pid_not_zero_filled() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 50.0)]));
    t.push(1000, &HashMap::new());
    assert_avg(t.avg(1, 5000, 1000), 50.0);
}

#[test]
fn values_clamped_to_0_100() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 150.0)]));
    assert_avg(t.avg(1, 5000, 0), 100.0);

    t.push(0, &map(&[(2, -5.0)]));
    assert_avg(t.avg(2, 5000, 0), 0.0);
}

#[test]
fn non_finite_values_skipped() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(2, f32::NAN)]));
    assert_eq!(t.avg(2, 5000, 0), None);

    t.push(0, &map(&[(2, f32::INFINITY), (3, f32::NEG_INFINITY)]));
    assert_eq!(t.avg(2, 5000, 0), None);
    assert_eq!(t.avg(3, 5000, 0), None);
}

#[test]
fn pruning_forgets_stale_pid() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(3, 10.0)]));
    t.push(61_000, &map(&[(4, 20.0)]));
    assert_eq!(t.avg(3, 60_000, 61_000), None);
    assert_avg(t.avg(4, 60_000, 61_000), 20.0);
}

#[test]
fn pid_unseen_for_60s_is_forgotten() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(5, 40.0)]));
    t.push(30_000, &map(&[(6, 1.0)]));
    t.push(60_001, &map(&[(7, 1.0)]));
    assert_eq!(t.avg(5, 60_000, 60_001), None);
}

#[test]
fn pid_seen_recently_is_retained() {
    let mut t = GpuTracker::new();
    t.push(30_000, &map(&[(5, 70.0)]));
    t.push(60_001, &map(&[(8, 1.0)]));
    assert_avg(t.avg(5, 60_000, 60_001), 70.0);
}

#[test]
fn sample_at_exact_window_start_is_included() {
    // Window is inclusive at its start: t=0 is within [0, 60_000] when
    // now=60_000 and window=60_000, so both samples are averaged.
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(6, 90.0)]));
    t.push(60_000, &map(&[(6, 10.0)]));
    assert_avg(t.avg(6, 60_000, 60_000), 50.0);
}

#[test]
fn empty_tracker_avg_is_none() {
    let t = GpuTracker::new();
    assert_eq!(t.avg(1, 5000, 0), None);
}

#[test]
fn now_smaller_than_window_does_not_panic() {
    let mut t = GpuTracker::new();
    t.push(0, &map(&[(1, 33.0)]));
    assert_avg(t.avg(1, 60_000, 0), 33.0);
}

// ---------- LIVE (Windows only) ----------

#[cfg(windows)]
mod live {
    use clipcheck_detect::gpu::GpuMonitor;
    use std::time::Duration;

    fn check_range(label: &str, m: &std::collections::HashMap<u32, f32>) {
        for (pid, v) in m {
            assert!(v.is_finite(), "{label}: pid {pid} value {v} not finite");
            assert!(
                (0.0..=100.0).contains(v),
                "{label}: pid {pid} value {v} out of 0..=100"
            );
        }
    }

    #[test]
    fn gpu_monitor_live_sample() {
        let mut monitor = match GpuMonitor::new() {
            Ok(m) => m,
            Err(e) => {
                println!("GPU counters unavailable: {e}");
                return;
            }
        };

        let first = monitor.sample().expect("first sample should succeed");
        std::thread::sleep(Duration::from_secs(1));
        let second = monitor.sample().expect("second sample should succeed");

        check_range("first", &first);
        check_range("second", &second);

        if second.is_empty() {
            println!("no GPU engine instances reported; skipped non-empty check");
        } else {
            println!("GPU engine pids reported: {}", second.len());
        }
    }
}
