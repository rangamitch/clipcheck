//! Exact time arithmetic (docs/design/13-video-editor.md section 14.2). Public API only.

use clipcheck_timeline::time::{Rate, Tick, MAX_PROJECT_TICKS, MAX_TICK, TICKS_PER_SECOND};

/// Common video rates the editor must represent exactly.
const COMMON_RATES: [Rate; 9] = [
    Rate {
        num: 24_000,
        den: 1001,
    },
    Rate { num: 24, den: 1 },
    Rate { num: 25, den: 1 },
    Rate {
        num: 30_000,
        den: 1001,
    },
    Rate { num: 30, den: 1 },
    Rate { num: 50, den: 1 },
    Rate {
        num: 60_000,
        den: 1001,
    },
    Rate { num: 60, den: 1 },
    Rate { num: 120, den: 1 },
];

fn rate_of(num: u32, den: u32) -> Rate {
    Rate { num, den }
}

const _: () = assert!(MAX_PROJECT_TICKS < MAX_TICK);
const _: () = assert!(MAX_PROJECT_TICKS == 4 * 3600 * TICKS_PER_SECOND);

#[test]
fn ticks_per_second_is_the_documented_constant() {
    assert_eq!(TICKS_PER_SECOND, 254_016_000_000);
}

#[test]
fn max_tick_is_the_largest_js_safe_integer() {
    assert_eq!(MAX_TICK, 9_007_199_254_740_991);
    assert_eq!(MAX_TICK as f64 as i64, MAX_TICK);
}

#[test]
fn common_video_rates_divide_ticks_exactly() {
    let tps = i128::from(TICKS_PER_SECOND);
    for r in COMMON_RATES {
        assert_eq!(
            (tps * i128::from(r.den)) % i128::from(r.num),
            0,
            "rate {}/{} is not exact",
            r.num,
            r.den
        );
    }
}

#[test]
fn audio_sample_rates_divide_ticks_exactly() {
    for sr in [44_100_i64, 48_000, 90_000] {
        assert_eq!(TICKS_PER_SECOND % sr, 0, "sample rate {sr}");
    }
}

#[test]
fn from_secs_scales_by_ticks_per_second() {
    assert_eq!(Tick::from_secs(3), Tick(3 * TICKS_PER_SECOND));
    assert_eq!(Tick::from_secs(0), Tick::ZERO);
    assert_eq!(Tick::from_secs(-2), Tick(-2 * TICKS_PER_SECOND));
}

#[test]
fn from_secs_saturates_instead_of_wrapping() {
    assert_eq!(Tick::from_secs(i64::MAX), Tick(i64::MAX));
    assert_eq!(Tick::from_secs(i64::MIN), Tick(i64::MIN));
}

#[test]
fn from_ms_agrees_with_seconds() {
    assert_eq!(Tick::from_ms(1000), Tick::from_secs(1));
    assert_eq!(Tick::from_ms(1500), Tick::from_secs_f64(1.5));
    assert_eq!(Tick::from_ms(1), Tick(TICKS_PER_SECOND / 1000));
}

#[test]
fn from_secs_f64_non_finite_values_give_zero() {
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Tick::from_secs_f64(v), Tick::ZERO, "input {v}");
    }
}

#[test]
fn from_secs_f64_out_of_range_clamps_to_max_tick() {
    assert_eq!(Tick::from_secs_f64(1e300), Tick(MAX_TICK));
    assert_eq!(Tick::from_secs_f64(-1e300), Tick(-MAX_TICK));
}

#[test]
fn from_secs_f64_rounds_to_nearest_tick() {
    assert_eq!(Tick::from_secs_f64(0.1), Tick::from_ms(100));
    assert_eq!(Tick::from_secs_f64(0.5), Tick(TICKS_PER_SECOND / 2));
    assert_eq!(Tick::from_secs_f64(-0.25), Tick(-TICKS_PER_SECOND / 4));
}

#[test]
fn as_ms_rounds_to_nearest_millisecond() {
    let half_ms = TICKS_PER_SECOND / 1000 / 2;
    assert_eq!(Tick::from_ms(1234).as_ms(), 1234);
    assert_eq!(Tick(half_ms).as_ms(), 1);
    assert_eq!(Tick(half_ms - 1).as_ms(), 0);
    assert_eq!(Tick(-half_ms).as_ms(), -1);
    assert_eq!(Tick(-(half_ms - 1)).as_ms(), 0);
}

#[test]
fn as_secs_f64_reports_seconds() {
    assert!((Tick::from_ms(2500).as_secs_f64() - 2.5).abs() < 1e-12);
}

#[test]
fn checked_add_and_sub_report_overflow_as_none() {
    assert_eq!(Tick(i64::MAX).checked_add(Tick(1)), None);
    assert_eq!(Tick(i64::MIN).checked_sub(Tick(1)), None);
    assert_eq!(Tick(1).checked_add(Tick(2)), Some(Tick(3)));
    assert_eq!(Tick(5).checked_sub(Tick(7)), Some(Tick(-2)));
}

#[test]
fn saturating_add_and_sub_stop_at_the_i64_limits() {
    assert_eq!(Tick(i64::MAX).saturating_add(Tick(5)), Tick(i64::MAX));
    assert_eq!(Tick(i64::MIN).saturating_sub(Tick(5)), Tick(i64::MIN));
    assert_eq!(Tick(3).saturating_sub(Tick(10)), Tick(-7));
}

#[test]
fn arithmetic_operators_saturate() {
    assert_eq!(Tick(i64::MAX) + Tick(1), Tick(i64::MAX));
    assert_eq!(Tick(i64::MIN) - Tick(1), Tick(i64::MIN));
    assert_eq!(Tick(10) + Tick(5), Tick(15));
    assert_eq!(Tick(10) - Tick(15), Tick(-5));
}

#[test]
fn min_and_max_pick_the_expected_tick() {
    assert_eq!(Tick(3).min(Tick(7)), Tick(3));
    assert_eq!(Tick(3).max(Tick(7)), Tick(7));
    assert_eq!(Tick(-4).min(Tick(-9)), Tick(-9));
    assert_eq!(Tick(4).max(Tick(4)), Tick(4));
}

#[test]
fn clamp_limits_into_range() {
    assert_eq!(Tick(15).clamp(Tick(0), Tick(10)), Tick(10));
    assert_eq!(Tick(-5).clamp(Tick::ZERO, Tick(10)), Tick::ZERO);
    assert_eq!(Tick(4).clamp(Tick(0), Tick(10)), Tick(4));
}

#[test]
fn clamp_with_inverted_bounds_returns_the_lower_bound() {
    assert_eq!(Tick(5).clamp(Tick(10), Tick(0)), Tick(10));
}

#[test]
fn ticks_order_by_time_and_default_is_zero() {
    assert!(Tick(1) < Tick(2));
    assert!(Tick(-1) < Tick::ZERO);
    assert_eq!(Tick::default(), Tick::ZERO);
}

#[test]
fn to_frame_floors_to_the_containing_frame() {
    let r = Rate::whole(30);
    let f10 = Tick::from_frame(10, r);
    assert_eq!(f10.to_frame(r), 10);
    assert_eq!((f10 + Tick(1)).to_frame(r), 10);
    assert_eq!((Tick::from_frame(11, r) - Tick(1)).to_frame(r), 10);
}

#[test]
fn to_frame_of_non_positive_time_is_zero() {
    let r = Rate::whole(30);
    assert_eq!(Tick(-1).to_frame(r), 0);
    assert_eq!(Tick::ZERO.to_frame(r), 0);
}

#[test]
fn from_frame_is_exact_at_common_rates() {
    for r in COMMON_RATES {
        for n in [0_i64, 1, 2, 23, 24, 1000, 800_000] {
            let t = Tick::from_frame(n, r);
            assert_eq!(
                i128::from(t.0) * i128::from(r.num),
                i128::from(n) * i128::from(r.den) * i128::from(TICKS_PER_SECOND),
                "rate {}/{} frame {n}",
                r.num,
                r.den
            );
        }
    }
}

/// Frame indices go up to 800,000: at 23.976 fps that is about 33,366 s, which still fits under
/// MAX_TICK (about 35,461 s). Larger indices at 24 or 25 fps would clamp by design.
#[test]
fn frame_index_round_trips_through_ticks_at_common_rates() {
    for r in COMMON_RATES {
        for n in (0..=800_000_i64)
            .step_by(997)
            .chain(std::iter::once(800_000))
        {
            assert_eq!(
                Tick::from_frame(n, r).to_frame(r),
                n,
                "rate {}/{} frame {n}",
                r.num,
                r.den
            );
        }
    }
}

#[test]
fn frame_duration_matches_the_rate() {
    assert_eq!(Rate::whole(24).frame_duration(), Tick(10_584_000_000));
    assert_eq!(rate_of(24_000, 1001).frame_duration(), Tick(10_594_584_000));
    assert_eq!(
        Rate::whole(30).frame_duration(),
        Tick::from_frame(1, Rate::whole(30))
    );
}

#[test]
fn snap_to_frame_returns_the_frame_boundary_for_small_offsets() {
    for r in COMMON_RATES {
        let f = Tick::from_frame(7, r);
        assert_eq!(f.snap_to_frame(r), f);
        assert_eq!((f + Tick(1)).snap_to_frame(r), f);
        assert_eq!((f - Tick(1)).snap_to_frame(r), f);
    }
}

#[test]
fn snap_to_frame_rounds_past_half_a_frame_up() {
    let r = Rate::whole(25);
    let fd = r.frame_duration();
    let t = Tick::from_frame(7, r) + Tick(fd.0 * 3 / 4);
    assert_eq!(t.snap_to_frame(r), Tick::from_frame(8, r));
}

#[test]
fn rate_from_f64_recognises_standard_rates() {
    assert_eq!(Rate::from_f64(23.976), rate_of(24_000, 1001));
    assert_eq!(Rate::from_f64(29.97), rate_of(30_000, 1001));
    assert_eq!(Rate::from_f64(59.94), rate_of(60_000, 1001));
    assert_eq!(Rate::from_f64(24.0), rate_of(24, 1));
    assert_eq!(Rate::from_f64(25.0), rate_of(25, 1));
    assert_eq!(Rate::from_f64(30.0), rate_of(30, 1));
    assert_eq!(Rate::from_f64(60.0), rate_of(60, 1));
    assert_eq!(Rate::from_f64(120.0), rate_of(120, 1));
}

#[test]
fn rate_from_f64_falls_back_to_a_whole_rate_when_no_standard_is_close() {
    assert_eq!(Rate::from_f64(59.9), Rate::whole(60));
    assert_eq!(Rate::from_f64(35.0), Rate::whole(35));
}

#[test]
fn rate_as_f64_and_default() {
    assert!((rate_of(30_000, 1001).as_f64() - 29.97).abs() < 0.001);
    assert_eq!(Rate::default(), Rate::whole(30));
}

#[test]
fn sample_index_round_trips_at_audio_rates() {
    for sr in [44_100_u32, 48_000, 90_000] {
        for n in (0..=2_000_000_i64).step_by(1_237) {
            assert_eq!(
                Tick::from_sample(n, sr).to_sample(sr),
                n,
                "rate {sr} sample {n}"
            );
        }
    }
}

#[test]
fn one_second_of_samples_is_one_second() {
    assert_eq!(Tick::from_sample(48_000, 48_000), Tick::from_secs(1));
    assert_eq!(Tick::from_sample(44_100, 44_100), Tick::from_secs(1));
    assert_eq!(Tick::from_secs(1).to_sample(44_100), 44_100);
}

#[test]
fn to_sample_floors_and_clamps_negative_time_to_zero() {
    assert_eq!(
        (Tick::from_sample(5, 48_000) + Tick(1)).to_sample(48_000),
        5
    );
    assert_eq!(Tick(-1).to_sample(48_000), 0);
}

#[test]
fn tick_serialises_as_a_plain_integer() {
    let text = serde_json::to_string(&Tick(42)).expect("serialise tick");
    assert_eq!(text, "42");
    let back: Tick = serde_json::from_str("42").expect("parse tick");
    assert_eq!(back, Tick(42));
}
