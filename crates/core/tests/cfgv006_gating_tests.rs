//! Gating tests for CFG-V006 (`video.max_bitrate_kbps` must be at least `video.bitrate_kbps`).
//!
//! The check fires only when preset = Custom, rate_control = Vbr and max < bitrate.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn has_code(d: &[Diagnostic], code: &str) -> bool {
    d.iter().any(|x| x.code == code)
}

fn assert_v006_present(c: &Config) {
    let d = validate(c);
    let found = d.iter().find(|x| x.code == "CFG-V006");
    let x = found.unwrap_or_else(|| panic!("expected CFG-V006 in {d:#?}"));
    assert_eq!(
        x.severity,
        Severity::Error,
        "CFG-V006 should be an error: {x:?}"
    );
    assert_eq!(x.path, "video.max_bitrate_kbps", "CFG-V006 path: {x:?}");
}

fn assert_v006_absent(c: &Config) {
    let d = validate(c);
    assert!(
        !has_code(&d, "CFG-V006"),
        "CFG-V006 should not fire: {d:#?}"
    );
}

/// Custom preset, VBR, max 10000 below bitrate 14000 (the firing case).
fn custom_vbr_below() -> Config {
    let mut c = Config::default();
    c.video.preset = Preset::Custom;
    c.video.rate_control = RateControl::Vbr;
    c.video.bitrate_kbps = 14_000;
    c.video.max_bitrate_kbps = 10_000;
    c
}

// ---------------------------------------------------------------------------
// gating
// ---------------------------------------------------------------------------

#[test]
fn custom_vbr_max_below_bitrate_fires_v006() {
    assert_v006_present(&custom_vbr_below());
}

#[test]
fn custom_quality_with_same_numbers_does_not_fire_v006() {
    let mut c = custom_vbr_below();
    c.video.rate_control = RateControl::Quality;
    assert_v006_absent(&c);
}

#[test]
fn custom_cbr_with_same_numbers_does_not_fire_v006() {
    let mut c = custom_vbr_below();
    c.video.rate_control = RateControl::Cbr;
    assert_v006_absent(&c);
}

#[test]
fn ultra_preset_with_default_max_has_no_error_on_max_bitrate() {
    let mut c = Config::default();
    c.video.preset = Preset::Ultra;
    assert_eq!(
        c.video.max_bitrate_kbps, 15_000,
        "default max should be 15000"
    );
    let d = validate(&c);
    assert!(
        !has_code(&d, "CFG-V006"),
        "CFG-V006 should not fire for ultra: {d:#?}"
    );
    assert!(
        !d.iter().any(|x| x.path == "video.max_bitrate_kbps"),
        "no diagnostic at all expected on video.max_bitrate_kbps: {d:#?}"
    );
}

#[test]
fn high_and_medium_presets_do_not_fire_v006() {
    // Make the case discriminating: VBR with max below bitrate would fire under Custom.
    for preset in [Preset::High, Preset::Medium] {
        let mut c = custom_vbr_below();
        c.video.preset = preset;
        assert_v006_absent(&c);
    }
}

#[test]
fn custom_vbr_max_at_or_above_bitrate_does_not_fire_v006() {
    let mut c = Config::default();
    c.video.preset = Preset::Custom;
    c.video.rate_control = RateControl::Vbr;
    c.video.bitrate_kbps = 14_000;
    c.video.max_bitrate_kbps = 20_000;
    assert_v006_absent(&c);
}
