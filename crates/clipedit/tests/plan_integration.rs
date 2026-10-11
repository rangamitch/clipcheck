use clipcheck_clipedit::*;

const KEYFRAMES: [i64; 4] = [0, 2000, 4000, 6000];
const DURATION: i64 = 60_000;

fn input(keyframes: &[i64], start: i64, end: i64, duration: i64, mode: TrimMode) -> TrimInput<'_> {
    TrimInput {
        keyframes_ms: keyframes,
        duration_ms: duration,
        start_ms: start,
        end_ms: end,
        mode,
    }
}

// 1. Lossless snaps the start back to the keyframe at or before the request.
#[test]
fn lossless_snaps_start_back_to_previous_keyframe() {
    let plan = plan_trim(&input(
        &KEYFRAMES,
        5500,
        10000,
        DURATION,
        TrimMode::Lossless,
    ))
    .expect("plan should succeed");
    assert_eq!(plan.mode, TrimMode::Lossless);
    assert_eq!(plan.requested_start_ms, 5500);
    assert_eq!(plan.requested_end_ms, 10000);
    assert_eq!(
        plan.actual_start_ms, 4000,
        "start should snap to keyframe 4000"
    );
    assert_eq!(plan.snap_delta_ms, 1500);
    assert_eq!(plan.actual_end_ms, 10000);
    assert_eq!(plan.reencode_from_keyframe_ms, None);
    assert_eq!(plan.discard_head_ms, 0);
    assert_eq!(plan.output_duration_ms(), 6000);
}

// 2. Request starting exactly on a keyframe has zero snap delta.
#[test]
fn lossless_start_on_keyframe_has_zero_snap_delta() {
    let plan = plan_trim(&input(&KEYFRAMES, 2000, 8000, DURATION, TrimMode::Lossless))
        .expect("plan should succeed");
    assert_eq!(plan.actual_start_ms, 2000);
    assert_eq!(plan.snap_delta_ms, 0);
    assert_eq!(plan.reencode_from_keyframe_ms, None);
    assert_eq!(plan.discard_head_ms, 0);
}

// 3. Start before the first keyframe snaps to implicit 0.
#[test]
fn lossless_start_before_first_keyframe_snaps_to_zero() {
    let plan = plan_trim(&input(
        &[1000, 3000],
        500,
        5000,
        DURATION,
        TrimMode::Lossless,
    ))
    .expect("plan should succeed");
    assert_eq!(plan.actual_start_ms, 0);
    assert_eq!(plan.snap_delta_ms, 500);
    assert_eq!(plan.actual_end_ms, 5000);
}

// 4. Lossless with no keyframes is an error.
#[test]
fn lossless_empty_keyframes_is_no_keyframes_error() {
    let result = plan_trim(&input(&[], 5500, 10000, DURATION, TrimMode::Lossless));
    assert_eq!(result, Err(TrimError::NoKeyframes));
}

// 5. Precise keeps the requested start and re-encodes from the previous keyframe.
#[test]
fn precise_keeps_requested_start_and_reencodes_from_keyframe() {
    let plan = plan_trim(&input(&KEYFRAMES, 5500, 10000, DURATION, TrimMode::Precise))
        .expect("plan should succeed");
    assert_eq!(plan.mode, TrimMode::Precise);
    assert_eq!(plan.actual_start_ms, 5500);
    assert_eq!(plan.snap_delta_ms, 0);
    assert_eq!(plan.reencode_from_keyframe_ms, Some(4000));
    assert_eq!(plan.discard_head_ms, 1500);
    assert_eq!(plan.actual_end_ms, 10000);
    assert_eq!(plan.output_duration_ms(), 4500);
}

// 6. Precise with no keyframes re-encodes from 0 and discards the head up to start.
#[test]
fn precise_empty_keyframes_reencodes_from_zero() {
    let plan = plan_trim(&input(&[], 5500, 10000, DURATION, TrimMode::Precise))
        .expect("precise with empty keyframes should succeed");
    assert_eq!(plan.reencode_from_keyframe_ms, Some(0));
    assert_eq!(plan.discard_head_ms, plan.requested_start_ms);
    assert_eq!(plan.discard_head_ms, 5500);
    assert_eq!(plan.actual_start_ms, 5500);
}

// 7. Property-style check over pseudo-random inputs, both modes.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, lo: i64, hi_inclusive: i64) -> i64 {
        let span = (hi_inclusive - lo + 1) as u64;
        lo + (self.next() % span) as i64
    }
}

#[test]
fn property_random_cases_hold_invariants_in_both_modes() {
    const DUR: i64 = 600_000;
    let mut rng = Lcg(0x5EED_CAFE_F00D_1234);
    let mut checked = 0usize;

    for _ in 0..2000 {
        // Ascending keyframes, 0..=8 of them, spaced 1..=5000 ms apart.
        let n = rng.range(0, 8) as usize;
        let mut keyframes = Vec::with_capacity(n);
        let mut t = 0i64;
        for _ in 0..n {
            t += rng.range(1, 5000);
            if t >= DUR {
                break;
            }
            keyframes.push(t);
        }

        let start = rng.range(0, DUR - MIN_SELECTION_MS);
        let end = rng.range(start + MIN_SELECTION_MS, DUR);

        for mode in [TrimMode::Lossless, TrimMode::Precise] {
            let inp = input(&keyframes, start, end, DUR, mode);
            let res = plan_trim(&inp);

            if mode == TrimMode::Lossless && keyframes.is_empty() {
                assert_eq!(res, Err(TrimError::NoKeyframes), "start={start} end={end}");
                continue;
            }
            let plan = res.unwrap_or_else(|e| {
                panic!("unexpected error {e:?} for mode={mode:?} start={start} end={end} kf={keyframes:?}")
            });
            checked += 1;
            let ctx =
                format!("mode={mode:?} start={start} end={end} kf={keyframes:?} plan={plan:?}");

            assert!(plan.actual_start_ms <= plan.requested_start_ms, "{ctx}");
            assert!(
                plan.actual_start_ms == 0
                    || keyframes.contains(&plan.actual_start_ms)
                    || mode == TrimMode::Precise,
                "{ctx}"
            );
            assert_eq!(plan.actual_end_ms, plan.requested_end_ms, "{ctx}");
            assert!(plan.output_duration_ms() >= end - start, "{ctx}");
            assert_eq!(
                plan.snap_delta_ms,
                plan.requested_start_ms - plan.actual_start_ms,
                "{ctx}"
            );
            assert!(plan.snap_delta_ms >= 0, "{ctx}");

            match mode {
                TrimMode::Lossless => {
                    assert!(
                        plan.actual_start_ms == 0 || keyframes.contains(&plan.actual_start_ms),
                        "{ctx}"
                    );
                    assert_eq!(plan.reencode_from_keyframe_ms, None, "{ctx}");
                    assert_eq!(plan.discard_head_ms, 0, "{ctx}");
                }
                TrimMode::Precise => {
                    let from = plan
                        .reencode_from_keyframe_ms
                        .expect("precise must set reencode_from");
                    assert!(from <= plan.requested_start_ms, "{ctx}");
                    assert_eq!(
                        plan.discard_head_ms,
                        plan.requested_start_ms - from,
                        "{ctx}"
                    );
                }
            }
        }
    }
    assert!(checked > 1000, "too few valid cases exercised: {checked}");
}

// 8. Validation errors.
#[test]
fn start_equal_to_end_is_not_before_end() {
    let r = plan_trim(&input(&KEYFRAMES, 5000, 5000, DURATION, TrimMode::Lossless));
    assert_eq!(
        r,
        Err(TrimError::StartNotBeforeEnd {
            start_ms: 5000,
            end_ms: 5000
        })
    );
}

#[test]
fn start_after_end_is_not_before_end() {
    let r = plan_trim(&input(&KEYFRAMES, 9000, 5000, DURATION, TrimMode::Precise));
    assert_eq!(
        r,
        Err(TrimError::StartNotBeforeEnd {
            start_ms: 9000,
            end_ms: 5000
        })
    );
}

#[test]
fn negative_start_is_outside_duration() {
    let r = plan_trim(&input(&KEYFRAMES, -1, 1000, DURATION, TrimMode::Lossless));
    assert!(
        matches!(
            r,
            Err(TrimError::OutsideDuration {
                start_ms: -1,
                end_ms: 1000,
                duration_ms: DURATION
            })
        ),
        "got {r:?}"
    );
}

#[test]
fn end_beyond_duration_is_outside_duration() {
    let r = plan_trim(&input(
        &KEYFRAMES,
        1000,
        DURATION + 1,
        DURATION,
        TrimMode::Precise,
    ));
    assert!(
        matches!(r, Err(TrimError::OutsideDuration { .. })),
        "got {r:?}"
    );
}

#[test]
fn zero_duration_is_invalid() {
    let r = plan_trim(&input(&KEYFRAMES, 0, 1000, 0, TrimMode::Lossless));
    assert_eq!(r, Err(TrimError::InvalidDuration { duration_ms: 0 }));
}

#[test]
fn negative_duration_is_invalid() {
    let r = plan_trim(&input(&KEYFRAMES, 0, 1000, -5, TrimMode::Precise));
    assert_eq!(r, Err(TrimError::InvalidDuration { duration_ms: -5 }));
}

#[test]
fn fifty_ms_selection_is_too_short() {
    let r = plan_trim(&input(&KEYFRAMES, 1000, 1050, DURATION, TrimMode::Lossless));
    assert_eq!(
        r,
        Err(TrimError::TooShort {
            min_ms: 100,
            actual_ms: 50
        })
    );
}

#[test]
fn exactly_min_selection_is_ok() {
    let r = plan_trim(&input(&KEYFRAMES, 1000, 1100, DURATION, TrimMode::Precise));
    assert!(r.is_ok(), "got {r:?}");
}

#[test]
fn ninety_nine_ms_selection_is_too_short() {
    let r = plan_trim(&input(&KEYFRAMES, 1000, 1099, DURATION, TrimMode::Precise));
    assert_eq!(
        r,
        Err(TrimError::TooShort {
            min_ms: 100,
            actual_ms: 99
        })
    );
}

#[test]
fn unsorted_keyframes_rejected_in_lossless() {
    let r = plan_trim(&input(&[5, 1], 1000, 5000, DURATION, TrimMode::Lossless));
    assert_eq!(r, Err(TrimError::UnsortedKeyframes));
}

#[test]
fn unsorted_keyframes_rejected_in_precise() {
    let r = plan_trim(&input(&[5, 1], 1000, 5000, DURATION, TrimMode::Precise));
    assert_eq!(r, Err(TrimError::UnsortedKeyframes));
}

// 9. Error Display is non-empty and Error is implemented.
#[test]
fn every_error_variant_has_display_and_implements_error() {
    let errors = [
        TrimError::InvalidDuration { duration_ms: 0 },
        TrimError::OutsideDuration {
            start_ms: -1,
            end_ms: 5,
            duration_ms: 10,
        },
        TrimError::StartNotBeforeEnd {
            start_ms: 5,
            end_ms: 5,
        },
        TrimError::TooShort {
            min_ms: 100,
            actual_ms: 50,
        },
        TrimError::UnsortedKeyframes,
        TrimError::NoKeyframes,
    ];
    for e in errors {
        let s = e.to_string();
        assert!(!s.is_empty(), "empty Display for {e:?}");
        let _: &dyn std::error::Error = &e;
    }
}

// 10. format_hms.
#[test]
fn format_hms_known_values() {
    assert_eq!(format_hms(0), "00:00:00.000");
    assert_eq!(format_hms(12204), "00:00:12.204");
    assert_eq!(format_hms(3723005), "01:02:03.005");
    assert_eq!(format_hms(360_000_000), "100:00:00.000");
    assert_eq!(format_hms(-5), "00:00:00.000");
}

// 11-14. suggest_output_name.
#[test]
fn output_name_from_spec_example() {
    assert_eq!(
        suggest_output_name(
            "Game 2026-10-09 14-02-11 session-part2.mp4",
            754000,
            1862500
        ),
        "Game 2026-10-09 14-02-11 00-12-34-00-31-02 clip-trim"
    );
}

#[test]
fn output_name_strips_windows_path() {
    assert_eq!(
        suggest_output_name("C:\\rec\\My Clip.mkv", 0, 1000),
        "My Clip 00-00-00-00-00-01 clip-trim"
    );
}

#[test]
fn output_name_strips_forbidden_characters() {
    assert_eq!(
        suggest_output_name("a:b?c.mp4", 0, 1000),
        "abc 00-00-00-00-00-01 clip-trim"
    );
}

#[test]
fn output_name_never_contains_forbidden_chars_or_extension_dot() {
    let inputs = [
        "a:b?c.mp4",
        "C:\\rec\\My Clip.mkv",
        "weird<>name|*\"x\".webm",
        "dir/sub/file.name.mov",
        "plain",
        "  spaced . out ",
    ];
    let forbidden = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    for name in inputs {
        let out = suggest_output_name(name, 0, 1000);
        for c in forbidden {
            assert!(
                !out.contains(c),
                "output {out:?} for {name:?} contains {c:?}"
            );
        }
        // The extension itself must be dropped (internal dots in the stem are allowed).
        for ext in [".mp4", ".mkv", ".mov", ".webm"] {
            assert!(
                !out.contains(ext),
                "output {out:?} for {name:?} contains extension {ext:?}"
            );
        }
    }
}
