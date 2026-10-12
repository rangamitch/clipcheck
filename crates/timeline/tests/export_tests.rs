//! Export settings, presets, size maths, file-name templates, validation and serde shapes
//! (public API only). Helpers are inline on purpose (other testers write sibling files at the
//! same time).

use clipcheck_timeline::export::{
    apply_preset, builtin_presets, estimate_size, expand_name, find_preset, quality_to_kbps,
    solve_target_bitrate, validate_settings, AudioExport, AudioLayout, ExportKind, ExportRange,
    ExportSettings, Loudness, Preset, QualityMode, VideoCodec,
};
use clipcheck_timeline::time::{Rate, Tick};

// ----- helpers ---------------------------------------------------------------------------------

fn secs(n: i64) -> Tick {
    Tick::from_secs(n)
}

fn preset(id: &str) -> Preset {
    builtin_presets()
        .into_iter()
        .find(|p| p.id == id)
        .expect("builtin preset exists")
}

fn stamp() -> (i32, u32, u32, u32, u32, u32) {
    (2026, 3, 7, 9, 5, 4)
}

fn within(actual: f64, expected: f64, rel: f64) -> bool {
    (actual - expected).abs() <= expected.abs() * rel
}

fn illegal_free(s: &str) -> bool {
    !s.chars().any(|c| {
        c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    })
}

// ----- built-in presets ------------------------------------------------------------------------

#[test]
fn builtin_presets_has_exactly_fourteen_entries() {
    assert_eq!(builtin_presets().len(), 14);
}

#[test]
fn builtin_preset_ids_are_unique() {
    let presets = builtin_presets();
    let mut ids: Vec<String> = presets.iter().map(|p| p.id.clone()).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before);
}

#[test]
fn builtin_preset_ids_match_the_documented_set() {
    let expected = [
        "original_quality",
        "high_1080p60",
        "balanced_1080p30",
        "res_1440p",
        "res_4k",
        "small_720p30",
        "discord_10mb",
        "discord_50mb",
        "discord_500mb",
        "youtube",
        "vertical_1080x1920",
        "twitter_x",
        "audio_only",
        "still_png",
    ];
    let presets = builtin_presets();
    for id in expected {
        assert!(presets.iter().any(|p| p.id == id), "missing preset {id}");
    }
}

#[test]
fn every_builtin_is_flagged_builtin_and_its_settings_carry_its_id() {
    for p in builtin_presets() {
        assert!(p.builtin, "{} must be builtin", p.id);
        assert_eq!(p.settings.preset, p.id);
        assert!(!p.label.is_empty());
    }
}

#[test]
fn discord_10mb_max_is_ten_megabytes() {
    assert_eq!(preset("discord_10mb").max_mb, Some(10.0));
}

#[test]
fn discord_50mb_and_500mb_max_sizes() {
    assert_eq!(preset("discord_50mb").max_mb, Some(50.0));
    assert_eq!(preset("discord_500mb").max_mb, Some(500.0));
}

#[test]
fn presets_without_size_limit_have_no_max_mb() {
    for id in ["youtube", "original_quality", "still_png", "audio_only"] {
        assert_eq!(preset(id).max_mb, None, "{id}");
    }
}

#[test]
fn discord_presets_have_fixed_size_and_youtube_does_not() {
    assert!(preset("discord_10mb").fixed_size);
    assert!(!preset("youtube").fixed_size);
}

#[test]
fn audio_only_and_still_presets_have_their_kinds() {
    assert_eq!(preset("audio_only").settings.kind, ExportKind::AudioOnly);
    assert!(matches!(
        preset("still_png").settings.kind,
        ExportKind::Still { ref format, .. } if format == "png"
    ));
}

// ----- find_preset -----------------------------------------------------------------------------

#[test]
fn find_preset_prefers_user_preset_with_same_id() {
    let mut mine = preset("discord_10mb");
    mine.label = "My Discord".to_string();
    mine.builtin = false;
    let found = find_preset("discord_10mb", std::slice::from_ref(&mine)).expect("found");
    assert_eq!(found.label, "My Discord");
    assert!(!found.builtin);
}

#[test]
fn find_preset_falls_back_to_builtin() {
    let user = [Preset {
        id: "mine".to_string(),
        ..preset("youtube")
    }];
    let found = find_preset("youtube", &user).expect("found");
    assert!(found.builtin);
    assert_eq!(found.id, "youtube");
}

#[test]
fn find_preset_returns_user_preset_by_its_id() {
    let user = [Preset {
        id: "mine".to_string(),
        builtin: false,
        ..preset("youtube")
    }];
    let found = find_preset("mine", &user).expect("found");
    assert!(!found.builtin);
}

#[test]
fn find_preset_unknown_id_is_none() {
    assert!(find_preset("nope", &[]).is_none());
}

// ----- apply_preset ----------------------------------------------------------------------------

/// A fixed-size variant of a built-in preset with the given box.
fn boxed(id: &str, width: u32, height: u32) -> Preset {
    let mut p = Preset {
        fixed_size: true,
        ..preset(id)
    };
    p.settings.width = width;
    p.settings.height = height;
    p
}

#[test]
fn apply_fixed_preset_fits_a_wide_canvas_inside_the_box_keeping_aspect() {
    // Doc: width x height is a bounding box; the canvas is scaled to fit, aspect kept.
    // 2560x1080 into 1280x720: scale 0.5, so 1280x540.
    let base = ExportSettings::default();
    let p = preset("discord_10mb");
    assert!(p.fixed_size);
    let out = apply_preset(&base, &p, 2560, 1080);
    assert_eq!((out.width, out.height), (1280, 540));
    assert_eq!(out.preset, "discord_10mb");
}

#[test]
fn apply_fixed_preset_portrait_canvas_example_from_the_design() {
    // Preset 1920x1080, canvas 1080x1920: scale 0.5625, so 608x1080.
    let base = ExportSettings::default();
    let out = apply_preset(&base, &boxed("discord_50mb", 1920, 1080), 1080, 1920);
    assert_eq!((out.width, out.height), (608, 1080));
}

#[test]
fn apply_fixed_preset_never_upscales_a_smaller_canvas() {
    // Preset 1920x1080, canvas 1280x720: the canvas keeps its size.
    let base = ExportSettings::default();
    let out = apply_preset(&base, &boxed("discord_50mb", 1920, 1080), 1280, 720);
    assert_eq!((out.width, out.height), (1280, 720));
}

#[test]
fn apply_fixed_preset_square_canvas_is_kept_at_box_height() {
    // Preset 1920x1080, canvas 1080x1080: the height limit gives scale 1, so 1080x1080.
    let base = ExportSettings::default();
    let out = apply_preset(&base, &boxed("discord_50mb", 1920, 1080), 1080, 1080);
    assert_eq!((out.width, out.height), (1080, 1080));
}

#[test]
fn apply_fixed_preset_rounds_sides_down_to_even() {
    // Canvas 1001x1001 fits unscaled in 1920x1080 (scale 1), and 1001 rounds down to 1000.
    let base = ExportSettings::default();
    let out = apply_preset(&base, &boxed("discord_50mb", 1920, 1080), 1001, 1001);
    assert_eq!((out.width, out.height), (1000, 1000));
}

#[test]
fn apply_fixed_preset_takes_the_preset_fps_and_keeps_other_settings() {
    let base = ExportSettings {
        fps: Some(Rate::whole(24)),
        name_template: "Keep {n}".to_string(),
        ..ExportSettings::default()
    };
    let p = preset("discord_10mb");
    let out = apply_preset(&base, &p, 1920, 1080);
    assert_eq!(out.fps, p.settings.fps, "fixed presets apply their own fps");
    assert_eq!(out.name_template, "Keep {n}");
}

#[test]
fn apply_non_fixed_preset_uses_canvas_rounded_down_to_even() {
    let base = ExportSettings {
        fps: Some(Rate::whole(24)),
        ..ExportSettings::default()
    };
    let p = preset("youtube");
    assert!(!p.fixed_size);
    let out = apply_preset(&base, &p, 1281, 721);
    assert_eq!(out.width, 1280);
    assert_eq!(out.height, 720);
    assert_eq!(out.fps, Some(Rate::whole(24)), "settings' own fps is kept");
}

#[test]
fn apply_preset_keeps_name_template_dir_range_and_encoder() {
    let base = ExportSettings {
        name_template: "Mine {n}".to_string(),
        dir: Some("D:/out".to_string()),
        range: ExportRange::SelectedClips {
            ids: vec!["a".to_string()],
        },
        encoder: "software".to_string(),
        ..ExportSettings::default()
    };
    let out = apply_preset(&base, &preset("youtube"), 1920, 1080);
    assert_eq!(out.name_template, "Mine {n}");
    assert_eq!(out.dir.as_deref(), Some("D:/out"));
    assert_eq!(out.range, base.range);
    assert_eq!(out.encoder, "software");
}

#[test]
fn apply_preset_copies_codec_quality_and_audio_from_preset() {
    let base = ExportSettings::default();
    let p = preset("discord_10mb");
    let out = apply_preset(&base, &p, 1920, 1080);
    assert_eq!(out.codec, p.settings.codec);
    assert_eq!(out.quality, p.settings.quality);
    assert_eq!(out.audio, p.settings.audio);
}

// ----- solve_target_bitrate --------------------------------------------------------------------

#[test]
fn ten_mb_over_sixty_seconds_with_128_kbps_audio_is_near_expected() {
    // 10 MB = 80 Mbit, times 0.98 overhead, over 60 s = 1306.67 kbps, minus 128 kbps audio.
    let v = solve_target_bitrate(10.0, secs(60), 128);
    assert!(within(f64::from(v), 1178.67, 0.01), "got {v}");
}

#[test]
fn target_bitrate_never_goes_below_100_kbps() {
    assert_eq!(solve_target_bitrate(0.001, secs(1000), 0), 100);
}

#[test]
fn target_bitrate_zero_duration_is_floor() {
    assert_eq!(solve_target_bitrate(10.0, Tick::ZERO, 128), 100);
}

#[test]
fn target_bitrate_non_finite_target_is_floor() {
    assert_eq!(solve_target_bitrate(f64::NAN, secs(60), 128), 100);
    assert_eq!(solve_target_bitrate(f64::INFINITY, secs(60), 128), 100);
}

#[test]
fn target_bitrate_grows_with_target_size() {
    let small = solve_target_bitrate(10.0, secs(60), 128);
    let big = solve_target_bitrate(50.0, secs(60), 128);
    assert!(big > small);
}

#[test]
fn target_bitrate_shrinks_as_duration_grows() {
    let short = solve_target_bitrate(50.0, secs(30), 128);
    let long = solve_target_bitrate(50.0, secs(300), 128);
    assert!(short > long);
}

// ----- quality_to_kbps -------------------------------------------------------------------------

#[test]
fn quality_to_kbps_is_monotonic_in_value() {
    let mut last = 0;
    for v in 1..=100 {
        let k = quality_to_kbps(v, 1920, 1080, 30.0, VideoCodec::H264);
        assert!(k >= last, "value {v} dropped below {last}");
        last = k;
    }
}

#[test]
fn quality_to_kbps_hevc_is_cheaper_than_h264() {
    let h264 = quality_to_kbps(70, 1920, 1080, 30.0, VideoCodec::H264);
    let hevc = quality_to_kbps(70, 1920, 1080, 30.0, VideoCodec::Hevc);
    assert!(hevc < h264);
}

#[test]
fn quality_to_kbps_av1_uses_same_factor_as_hevc() {
    let hevc = quality_to_kbps(70, 1920, 1080, 30.0, VideoCodec::Hevc);
    let av1 = quality_to_kbps(70, 1920, 1080, 30.0, VideoCodec::Av1);
    assert_eq!(hevc, av1);
}

#[test]
fn quality_to_kbps_clamps_slider_value() {
    assert_eq!(
        quality_to_kbps(0, 1920, 1080, 30.0, VideoCodec::H264),
        quality_to_kbps(1, 1920, 1080, 30.0, VideoCodec::H264)
    );
    assert_eq!(
        quality_to_kbps(500, 1920, 1080, 30.0, VideoCodec::H264),
        quality_to_kbps(100, 1920, 1080, 30.0, VideoCodec::H264)
    );
}

#[test]
fn quality_to_kbps_endpoints_follow_bits_per_pixel_curve() {
    // 1920 x 1080 x 30 fps = 62,208,000 pixels per second.
    let low = quality_to_kbps(1, 1920, 1080, 30.0, VideoCodec::H264);
    let high = quality_to_kbps(100, 1920, 1080, 30.0, VideoCodec::H264);
    assert!(within(f64::from(low), 1866.24, 0.01), "low {low}");
    assert!(within(f64::from(high), 15552.0, 0.01), "high {high}");
}

#[test]
fn quality_to_kbps_has_a_floor_of_100() {
    assert_eq!(quality_to_kbps(1, 128, 128, 1.0, VideoCodec::Av1), 100);
}

#[test]
fn quality_to_kbps_has_a_ceiling_of_200_000() {
    assert_eq!(
        quality_to_kbps(100, 8192, 8192, 120.0, VideoCodec::H264),
        200_000
    );
}

// ----- estimate_size ---------------------------------------------------------------------------

#[test]
fn quality_mode_estimate_uses_quality_bitrate_and_audio_total() {
    let s = ExportSettings::default();
    let e = estimate_size(&s, secs(10), 1);
    let expected_video = quality_to_kbps(70, 1920, 1080, 30.0, VideoCodec::H264);
    assert_eq!(e.video_kbps, expected_video);
    assert_eq!(e.audio_kbps, 192);
    let expected_bytes = f64::from(expected_video + 192) * 125.0 * 10.0 * 1.02;
    assert!(
        within(e.bytes as f64, expected_bytes, 0.005),
        "bytes {}",
        e.bytes
    );
}

#[test]
fn bitrate_mode_estimate_uses_given_kbps() {
    let s = ExportSettings {
        quality: QualityMode::Bitrate { kbps: 8000 },
        ..ExportSettings::default()
    };
    let e = estimate_size(&s, secs(10), 1);
    assert_eq!(e.video_kbps, 8000);
}

#[test]
fn cbr_mode_estimate_uses_given_kbps() {
    let s = ExportSettings {
        quality: QualityMode::Cbr { kbps: 4500 },
        ..ExportSettings::default()
    };
    assert_eq!(estimate_size(&s, secs(10), 1).video_kbps, 4500);
}

#[test]
fn target_size_estimate_lands_near_requested_megabytes() {
    let s = ExportSettings {
        quality: QualityMode::TargetSize { mb: 50.0 },
        ..ExportSettings::default()
    };
    let e = estimate_size(&s, secs(60), 1);
    assert!(
        within(e.bytes as f64, 50_000_000.0, 0.01),
        "bytes {}",
        e.bytes
    );
    assert_eq!(e.video_kbps, solve_target_bitrate(50.0, secs(60), 192));
}

#[test]
fn audio_only_estimate_is_audio_bitrate_only() {
    let s = ExportSettings {
        kind: ExportKind::AudioOnly,
        ..ExportSettings::default()
    };
    let e = estimate_size(&s, secs(20), 1);
    assert_eq!(e.video_kbps, 0);
    assert_eq!(e.audio_kbps, 192);
    let expected = 192.0 * 125.0 * 20.0 * 1.02;
    assert!(within(e.bytes as f64, expected, 0.005), "bytes {}", e.bytes);
}

#[test]
fn still_estimate_is_width_times_height_times_one_and_a_half() {
    let s = ExportSettings {
        width: 1920,
        height: 1080,
        kind: ExportKind::Still {
            format: "png".to_string(),
            quality: 90,
            at: Tick::ZERO,
        },
        ..ExportSettings::default()
    };
    let e = estimate_size(&s, secs(10), 3);
    assert_eq!(e.bytes, 1920 * 1080 * 3 / 2);
    assert_eq!(e.video_kbps, 0);
    assert_eq!(e.audio_kbps, 0);
}

#[test]
fn mix_layout_counts_one_audio_track_regardless_of_track_count() {
    let s = ExportSettings::default();
    assert_eq!(s.audio.layout, AudioLayout::Mix);
    assert_eq!(estimate_size(&s, secs(10), 4).audio_kbps, 192);
}

#[test]
fn separate_layout_multiplies_audio_by_track_count() {
    let s = ExportSettings {
        audio: AudioExport {
            layout: AudioLayout::Separate,
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    };
    assert_eq!(estimate_size(&s, secs(10), 3).audio_kbps, 576);
}

#[test]
fn separate_layout_with_no_tracks_counts_one() {
    let s = ExportSettings {
        audio: AudioExport {
            layout: AudioLayout::Separate,
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    };
    assert_eq!(estimate_size(&s, secs(10), 0).audio_kbps, 192);
}

// ----- expand_name -----------------------------------------------------------------------------

#[test]
fn project_and_timestamp_tokens_expand() {
    let out = expand_name(
        "{project} {yyyy-MM-dd HH-mm-ss}",
        "Demo",
        stamp(),
        1920,
        1080,
        "p",
        0,
    );
    assert_eq!(out, "Demo 2026-03-07 09-05-04");
}

#[test]
fn date_time_res_and_preset_tokens_expand() {
    let out = expand_name(
        "{date}_{time}_{res}_{preset}",
        "x",
        stamp(),
        1280,
        720,
        "discord_10mb",
        0,
    );
    assert_eq!(out, "2026-03-07_09-05-04_1280x720_discord_10mb");
}

#[test]
fn n_token_is_empty_for_zero_and_numeric_for_positive() {
    assert_eq!(expand_name("clip{n}", "x", stamp(), 1, 2, "p", 0), "clip");
    assert_eq!(expand_name("clip{n}", "x", stamp(), 1, 2, "p", 3), "clip3");
}

#[test]
fn positive_collision_counter_without_n_token_appends_suffix() {
    assert_eq!(expand_name("Demo", "x", stamp(), 1, 2, "p", 2), "Demo (2)");
}

#[test]
fn zero_collision_counter_adds_no_suffix() {
    assert_eq!(expand_name("Demo", "x", stamp(), 1, 2, "p", 0), "Demo");
}

#[test]
fn unknown_token_expands_to_nothing() {
    assert_eq!(expand_name("a{zzz}b", "x", stamp(), 1, 2, "p", 0), "ab");
}

#[test]
fn illegal_characters_are_dropped_from_the_result() {
    let out = expand_name("a/b:c*d?e\"f<g>h|i", "x", stamp(), 1, 2, "p", 0);
    assert_eq!(out, "abcdefghi");
    assert!(illegal_free(&out));
}

#[test]
fn project_name_with_illegal_characters_is_sanitised() {
    let out = expand_name("{project}", "Trip: day 1/2?", stamp(), 1, 2, "p", 0);
    assert!(illegal_free(&out), "got {out}");
    assert_eq!(out, "Trip day 12");
}

#[test]
fn control_characters_are_dropped() {
    assert_eq!(expand_name("a\u{7}b", "x", stamp(), 1, 2, "p", 0), "ab");
}

#[test]
fn spaces_collapse_and_edges_trim_spaces_and_dots() {
    assert_eq!(
        expand_name("  a    b...  ", "x", stamp(), 1, 2, "p", 0),
        "a b"
    );
}

#[test]
fn reserved_device_names_get_a_prefix() {
    assert_eq!(expand_name("CON", "x", stamp(), 1, 2, "p", 0), "_CON");
    assert_eq!(expand_name("nul", "x", stamp(), 1, 2, "p", 0), "_nul");
    assert_eq!(expand_name("COM1", "x", stamp(), 1, 2, "p", 0), "_COM1");
    assert_eq!(expand_name("lpt9", "x", stamp(), 1, 2, "p", 0), "_lpt9");
}

#[test]
fn reserved_name_with_extension_is_still_prefixed() {
    assert_eq!(
        expand_name("nul.txt", "x", stamp(), 1, 2, "p", 0),
        "_nul.txt"
    );
}

#[test]
fn names_that_only_resemble_reserved_words_are_not_prefixed() {
    assert_eq!(
        expand_name("CONSOLE", "x", stamp(), 1, 2, "p", 0),
        "CONSOLE"
    );
    assert_eq!(expand_name("COM10", "x", stamp(), 1, 2, "p", 0), "COM10");
}

#[test]
fn stem_is_capped_at_120_characters() {
    let long = "a".repeat(200);
    let out = expand_name(&long, "x", stamp(), 1, 2, "p", 0);
    assert_eq!(out.chars().count(), 120);
}

#[test]
fn empty_or_blank_result_becomes_export() {
    assert_eq!(expand_name("", "", stamp(), 1, 2, "p", 0), "Export");
    assert_eq!(expand_name("   ...  ", "", stamp(), 1, 2, "p", 0), "Export");
    assert_eq!(expand_name("///", "", stamp(), 1, 2, "p", 0), "Export");
}

// ----- validate_settings -----------------------------------------------------------------------

fn has(problems: &[String], needle: &str) -> bool {
    problems.iter().any(|m| m.contains(needle))
}

#[test]
fn default_settings_validate_clean() {
    assert!(validate_settings(&ExportSettings::default()).is_empty());
}

#[test]
fn odd_width_is_reported() {
    let s = ExportSettings {
        width: 1281,
        ..ExportSettings::default()
    };
    assert!(has(&validate_settings(&s), "Width"));
}

#[test]
fn size_outside_128_to_8192_is_reported_for_both_axes() {
    let s = ExportSettings {
        width: 64,
        height: 8194,
        ..ExportSettings::default()
    };
    let problems = validate_settings(&s);
    assert!(has(&problems, "Width"));
    assert!(has(&problems, "Height"));
}

#[test]
fn fps_zero_and_above_120_are_reported_and_120_is_fine() {
    let zero = ExportSettings {
        fps: Some(Rate::whole(0)),
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&zero).is_empty());
    let high = ExportSettings {
        fps: Some(Rate::whole(121)),
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&high).is_empty());
    let ok = ExportSettings {
        fps: Some(Rate::whole(120)),
        ..ExportSettings::default()
    };
    assert!(validate_settings(&ok).is_empty());
}

#[test]
fn no_fps_override_is_valid() {
    let s = ExportSettings {
        fps: None,
        ..ExportSettings::default()
    };
    assert!(validate_settings(&s).is_empty());
}

#[test]
fn quality_slider_bounds_are_one_to_one_hundred() {
    let low = ExportSettings {
        quality: QualityMode::Quality { value: 0 },
        ..ExportSettings::default()
    };
    let high = ExportSettings {
        quality: QualityMode::Quality { value: 101 },
        ..ExportSettings::default()
    };
    assert!(has(&validate_settings(&low), "Quality"));
    assert!(has(&validate_settings(&high), "Quality"));
    for v in [1, 100] {
        let ok = ExportSettings {
            quality: QualityMode::Quality { value: v },
            ..ExportSettings::default()
        };
        assert!(validate_settings(&ok).is_empty(), "value {v}");
    }
}

#[test]
fn target_size_below_one_mb_is_reported_and_nan_too() {
    let small = ExportSettings {
        quality: QualityMode::TargetSize { mb: 0.5 },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&small).is_empty());
    let nan = ExportSettings {
        quality: QualityMode::TargetSize { mb: f64::NAN },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&nan).is_empty());
    let ok = ExportSettings {
        quality: QualityMode::TargetSize { mb: 1.0 },
        ..ExportSettings::default()
    };
    assert!(validate_settings(&ok).is_empty());
}

#[test]
fn bitrate_bounds_are_100_to_200000_kbps() {
    let low = ExportSettings {
        quality: QualityMode::Bitrate { kbps: 99 },
        ..ExportSettings::default()
    };
    let high = ExportSettings {
        quality: QualityMode::Cbr { kbps: 200_001 },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&low).is_empty());
    assert!(!validate_settings(&high).is_empty());
    let ok = ExportSettings {
        quality: QualityMode::Cbr { kbps: 100 },
        ..ExportSettings::default()
    };
    assert!(validate_settings(&ok).is_empty());
}

#[test]
fn audio_bitrate_must_be_an_aac_step() {
    let mut s = ExportSettings::default();
    s.audio.bitrate_kbps = 100;
    assert!(
        !validate_settings(&s).is_empty(),
        "100 kbps is not an AAC step"
    );
    for ok in [64u32, 96, 128, 160, 192, 256, 320] {
        s.audio.bitrate_kbps = ok;
        assert!(
            validate_settings(&s).is_empty(),
            "{ok} kbps should be valid"
        );
    }
}

#[test]
fn sample_rate_must_be_44100_or_48000() {
    let bad = ExportSettings {
        audio: AudioExport {
            sample_rate: 22_050,
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&bad).is_empty());
    let ok = ExportSettings {
        audio: AudioExport {
            sample_rate: 44_100,
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    };
    assert!(validate_settings(&ok).is_empty());
}

#[test]
fn lufs_target_must_be_between_minus_40_and_minus_5() {
    let bad = ExportSettings {
        audio: AudioExport {
            loudness: Loudness::Lufs { target: -50.0 },
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&bad).is_empty());
    let ok = ExportSettings {
        audio: AudioExport {
            loudness: Loudness::Lufs { target: -14.0 },
            ..AudioExport::default()
        },
        ..ExportSettings::default()
    };
    assert!(validate_settings(&ok).is_empty());
}

#[test]
fn still_export_needs_png_or_jpg_and_quality_one_to_hundred() {
    let gif = ExportSettings {
        kind: ExportKind::Still {
            format: "gif".to_string(),
            quality: 90,
            at: Tick::ZERO,
        },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&gif).is_empty());
    let bad_q = ExportSettings {
        kind: ExportKind::Still {
            format: "jpg".to_string(),
            quality: 101,
            at: Tick::ZERO,
        },
        ..ExportSettings::default()
    };
    assert!(!validate_settings(&bad_q).is_empty());
    let ok = ExportSettings {
        kind: ExportKind::Still {
            format: "jpg".to_string(),
            quality: 100,
            at: Tick::ZERO,
        },
        ..ExportSettings::default()
    };
    assert!(validate_settings(&ok).is_empty());
}

#[test]
fn several_problems_are_all_listed() {
    let s = ExportSettings {
        width: 1281,
        height: 721,
        quality: QualityMode::Quality { value: 0 },
        ..ExportSettings::default()
    };
    assert!(validate_settings(&s).len() >= 3);
}

#[test]
fn audio_only_export_kind_is_reported_as_not_available_yet() {
    let s = ExportSettings {
        kind: ExportKind::AudioOnly,
        ..ExportSettings::default()
    };
    assert!(has(
        &validate_settings(&s),
        "Audio-only export is not available yet"
    ));
}

#[test]
fn builtin_audio_only_preset_is_listed_but_flagged_by_validation() {
    // The preset stays in the list (its kind is still AudioOnly), but its settings are refused.
    let p = preset("audio_only");
    assert_eq!(p.settings.kind, ExportKind::AudioOnly);
    assert!(has(&validate_settings(&p.settings), "Audio-only"));
}

// ----- serde shapes ----------------------------------------------------------------------------

#[test]
fn target_size_quality_mode_serialises_with_mode_tag() {
    let v = serde_json::to_value(QualityMode::TargetSize { mb: 50.0 }).expect("ser");
    assert_eq!(v, serde_json::json!({"mode": "target_size", "mb": 50.0}));
    let back: QualityMode = serde_json::from_value(v).expect("de");
    assert_eq!(back, QualityMode::TargetSize { mb: 50.0 });
}

#[test]
fn quality_bitrate_and_cbr_modes_use_snake_case_tags() {
    let q = serde_json::to_value(QualityMode::Quality { value: 70 }).expect("ser");
    assert_eq!(q, serde_json::json!({"mode": "quality", "value": 70}));
    let b = serde_json::to_value(QualityMode::Bitrate { kbps: 6000 }).expect("ser");
    assert_eq!(b, serde_json::json!({"mode": "bitrate", "kbps": 6000}));
    let c = serde_json::to_value(QualityMode::Cbr { kbps: 6000 }).expect("ser");
    assert_eq!(c, serde_json::json!({"mode": "cbr", "kbps": 6000}));
}

#[test]
fn selected_clips_range_serialises_with_kind_tag_and_ids() {
    let r = ExportRange::SelectedClips {
        ids: vec!["a".to_string(), "b".to_string()],
    };
    let v = serde_json::to_value(&r).expect("ser");
    assert_eq!(
        v,
        serde_json::json!({"kind": "selected_clips", "ids": ["a", "b"]})
    );
    let back: ExportRange = serde_json::from_value(v).expect("de");
    assert_eq!(back, r);
}

#[test]
fn entire_and_work_area_ranges_are_tagged_unit_variants() {
    assert_eq!(
        serde_json::to_value(&ExportRange::Entire).expect("ser"),
        serde_json::json!({"kind": "entire"})
    );
    assert_eq!(
        serde_json::to_value(&ExportRange::WorkArea).expect("ser"),
        serde_json::json!({"kind": "work_area"})
    );
}

#[test]
fn loudness_serialises_with_mode_tag() {
    let lufs = serde_json::to_value(Loudness::Lufs { target: -14.0 }).expect("ser");
    assert_eq!(lufs, serde_json::json!({"mode": "lufs", "target": -14.0}));
    let off = serde_json::to_value(Loudness::Off).expect("ser");
    assert_eq!(off, serde_json::json!({"mode": "off"}));
}

#[test]
fn export_kind_video_and_still_serialise_with_kind_tag() {
    let video = serde_json::to_value(ExportKind::Video).expect("ser");
    assert_eq!(video, serde_json::json!({"kind": "video"}));
    let still = ExportKind::Still {
        format: "png".to_string(),
        quality: 90,
        at: secs(2),
    };
    let v = serde_json::to_value(&still).expect("ser");
    assert_eq!(v["kind"], "still");
    assert_eq!(v["format"], "png");
    assert_eq!(v["quality"], 90);
    let back: ExportKind = serde_json::from_value(v).expect("de");
    assert_eq!(back, still);
}

#[test]
fn codec_and_layout_enums_use_snake_case() {
    assert_eq!(serde_json::to_value(VideoCodec::H264).expect("ser"), "h264");
    assert_eq!(serde_json::to_value(VideoCodec::Hevc).expect("ser"), "hevc");
    assert_eq!(serde_json::to_value(VideoCodec::Av1).expect("ser"), "av1");
    assert_eq!(
        serde_json::to_value(AudioLayout::Separate).expect("ser"),
        "separate"
    );
}

#[test]
fn default_export_settings_round_trip_through_json() {
    let s = ExportSettings::default();
    let json = serde_json::to_string(&s).expect("ser");
    let back: ExportSettings = serde_json::from_str(&json).expect("de");
    assert_eq!(back, s);
}

#[test]
fn customised_export_settings_round_trip_through_json() {
    let s = ExportSettings {
        preset: "custom".to_string(),
        dir: Some("E:/exports".to_string()),
        range: ExportRange::SelectedClips {
            ids: vec!["x".to_string()],
        },
        fps: None,
        codec: VideoCodec::Hevc,
        quality: QualityMode::TargetSize { mb: 25.5 },
        audio: AudioExport {
            layout: AudioLayout::Separate,
            bitrate_kbps: 320,
            sample_rate: 44_100,
            loudness: Loudness::Lufs { target: -16.0 },
            limiter: false,
        },
        kind: ExportKind::AudioOnly,
        lossless: true,
        ..ExportSettings::default()
    };
    let v = serde_json::to_value(&s).expect("ser");
    let back: ExportSettings = serde_json::from_value(v).expect("de");
    assert_eq!(back, s);
}

#[test]
fn empty_json_object_gives_default_export_settings() {
    let s: ExportSettings = serde_json::from_str("{}").expect("de");
    assert_eq!(s, ExportSettings::default());
}

#[test]
fn default_values_match_documented_defaults() {
    let s = ExportSettings::default();
    assert_eq!(s.preset, "balanced_1080p30");
    assert_eq!(s.name_template, "{project} {yyyy-MM-dd HH-mm-ss}");
    assert_eq!(s.width, 1920);
    assert_eq!(s.height, 1080);
    assert_eq!(s.quality, QualityMode::Quality { value: 70 });
    assert_eq!(s.kind, ExportKind::Video);
    assert!(!s.lossless);
    let a = AudioExport::default();
    assert_eq!(a.bitrate_kbps, 192);
    assert_eq!(a.sample_rate, 48_000);
    assert!(a.limiter);
    assert_eq!(a.loudness, Loudness::Off);
}

#[test]
fn builtin_preset_round_trips_through_json() {
    let p = preset("discord_50mb");
    let v = serde_json::to_value(&p).expect("ser");
    let back: Preset = serde_json::from_value(v).expect("de");
    assert_eq!(back, p);
}
