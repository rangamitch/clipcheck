//! Spec-driven tests for the `[updates]` config table (`clipcheck_core::config`).
//!
//! Written against the public API spec only: `Config.updates`, `Updates::effective_endpoint`,
//! `Updates::checks_enabled`, `validate` (CFG-X006 / CFG-X007) and `apply_class`.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn parse(text: &str) -> Config {
    Config::from_toml_str(text).expect("config should parse")
}

fn with_interval(n: u32) -> Config {
    let mut c = Config::default();
    c.updates.interval_hours = n;
    c
}

fn with_endpoint(e: &str) -> Config {
    let mut c = Config::default();
    c.updates.endpoint = e.to_owned();
    c
}

fn diags_with(c: &Config, code: &str) -> Vec<Diagnostic> {
    validate(c).into_iter().filter(|d| d.code == code).collect()
}

fn assert_updates_error(c: &Config, code: &str, path: &str) {
    let hits = diags_with(c, code);
    assert_eq!(
        hits.len(),
        1,
        "{code} should fire exactly once for {:?}: {hits:#?}",
        c.updates
    );
    assert_eq!(
        hits[0].severity,
        Severity::Error,
        "{code} should be an error: {:#?}",
        hits[0]
    );
    assert_eq!(hits[0].path, path, "{code} path: {:#?}", hits[0]);
}

fn assert_no_updates_error(c: &Config, code: &str) {
    let hits = diags_with(c, code);
    assert!(
        hits.is_empty(),
        "{code} should not fire for {:?}: {hits:#?}",
        c.updates
    );
}

fn class_of(mutate: impl FnOnce(&mut Config)) -> ApplyClass {
    let old = Config::default();
    let mut new = old.clone();
    mutate(&mut new);
    apply_class(&old, &new)
}

// ---------------------------------------------------------------------------
// defaults
// ---------------------------------------------------------------------------

#[test]
fn updates_defaults() {
    let u = &Config::default().updates;
    assert_eq!(u.check, UpdateCheck::Notify);
    assert_eq!(u.channel, UpdateChannel::Stable);
    assert_eq!(u.interval_hours, 12);
    assert_eq!(u.endpoint, "");
}

#[test]
fn empty_config_has_default_updates() {
    assert_eq!(parse("").updates, Config::default().updates);
}

// ---------------------------------------------------------------------------
// default TOML template and round trips
// ---------------------------------------------------------------------------

#[test]
fn default_toml_contains_updates_table() {
    let text = Config::default_toml();
    assert!(
        text.lines().any(|l| l.trim() == "[updates]"),
        "default_toml should contain an [updates] table"
    );
}

#[test]
fn default_toml_updates_parse_to_defaults() {
    assert_eq!(
        parse(Config::default_toml()).updates,
        Config::default().updates
    );
}

#[test]
fn default_updates_round_trip_through_toml() {
    let text = Config::default()
        .to_toml_string()
        .expect("default config should serialize");
    let back = parse(&text);
    assert_eq!(back.updates, Config::default().updates);
}

#[test]
fn every_field_value_round_trips_through_toml() {
    let cases: Vec<(&str, Updates)> = vec![
        (
            "never + stable + 1h + empty endpoint",
            Updates {
                check: UpdateCheck::Never,
                channel: UpdateChannel::Stable,
                interval_hours: 1,
                endpoint: String::new(),
            },
        ),
        (
            "notify + 168h + custom https endpoint",
            Updates {
                check: UpdateCheck::Notify,
                channel: UpdateChannel::Stable,
                interval_hours: 168,
                endpoint: "https://github.com/o/r/releases/latest/download/latest.json".into(),
            },
        ),
        (
            "never + local http endpoint with port",
            Updates {
                check: UpdateCheck::Never,
                channel: UpdateChannel::Stable,
                interval_hours: 6,
                endpoint: "http://127.0.0.1:8080/latest.json".into(),
            },
        ),
    ];
    for (label, updates) in cases {
        let cfg = Config {
            updates: updates.clone(),
            ..Config::default()
        };
        let text = cfg
            .to_toml_string()
            .unwrap_or_else(|e| panic!("{label}: serialize failed: {e}"));
        let back = parse(&text);
        assert_eq!(
            back.updates, updates,
            "{label}: round trip mismatch\n{text}"
        );
    }
}

#[test]
fn snake_case_enum_values_in_toml() {
    let cfg = parse("[updates]\ncheck = 'never'\nchannel = 'stable'\n");
    assert_eq!(cfg.updates.check, UpdateCheck::Never);
    assert_eq!(cfg.updates.channel, UpdateChannel::Stable);
    let cfg = parse("[updates]\ncheck = 'notify'\n");
    assert_eq!(cfg.updates.check, UpdateCheck::Notify);
}

// ---------------------------------------------------------------------------
// partial tables and missing tables
// ---------------------------------------------------------------------------

#[test]
fn partial_updates_table_keeps_other_defaults() {
    let cfg = parse("[updates]\ncheck = 'never'\n");
    assert_eq!(cfg.updates.check, UpdateCheck::Never);
    assert_eq!(cfg.updates.channel, UpdateChannel::Stable);
    assert_eq!(cfg.updates.interval_hours, 12);
    assert_eq!(cfg.updates.endpoint, "");
}

#[test]
fn config_without_updates_table_loads_defaults() {
    let cfg = parse("[video]\nfps = 60\n");
    assert_eq!(cfg.updates, Config::default().updates);
    assert_eq!(cfg.updates.check, UpdateCheck::Notify);
    assert_eq!(cfg.updates.interval_hours, 12);
}

// ---------------------------------------------------------------------------
// unknown enum values fail to parse
// ---------------------------------------------------------------------------

#[test]
fn unknown_check_value_fails_to_parse() {
    assert!(Config::from_toml_str("[updates]\ncheck = 'sometimes'\n").is_err());
}

#[test]
fn unknown_channel_value_fails_to_parse() {
    assert!(Config::from_toml_str("[updates]\nchannel = 'beta'\n").is_err());
}

// ---------------------------------------------------------------------------
// interval_hours boundaries (valid 1..=168, CFG-X006 otherwise)
// ---------------------------------------------------------------------------

#[test]
fn interval_zero_is_invalid() {
    assert_updates_error(&with_interval(0), "CFG-X006", "updates.interval_hours");
}

#[test]
fn interval_one_is_valid() {
    assert_no_updates_error(&with_interval(1), "CFG-X006");
}

#[test]
fn interval_168_is_valid() {
    assert_no_updates_error(&with_interval(168), "CFG-X006");
}

#[test]
fn interval_169_is_invalid() {
    assert_updates_error(&with_interval(169), "CFG-X006", "updates.interval_hours");
}

#[test]
fn interval_u32_max_is_invalid() {
    assert_updates_error(
        &with_interval(u32::MAX),
        "CFG-X006",
        "updates.interval_hours",
    );
}

#[test]
fn default_interval_is_valid() {
    assert_no_updates_error(&Config::default(), "CFG-X006");
}

// ---------------------------------------------------------------------------
// endpoint validation (CFG-X007)
// ---------------------------------------------------------------------------

#[test]
fn endpoint_empty_is_valid() {
    assert_no_updates_error(&with_endpoint(""), "CFG-X007");
}

#[test]
fn endpoint_github_https_is_valid() {
    assert_no_updates_error(
        &with_endpoint("https://github.com/o/r/releases/latest/download/latest.json"),
        "CFG-X007",
    );
}

#[test]
fn endpoint_plain_http_non_local_is_invalid() {
    assert_updates_error(
        &with_endpoint("http://example.com/x"),
        "CFG-X007",
        "updates.endpoint",
    );
}

#[test]
fn endpoint_http_127_with_port_is_valid() {
    assert_no_updates_error(
        &with_endpoint("http://127.0.0.1:8080/latest.json"),
        "CFG-X007",
    );
}

#[test]
fn endpoint_http_localhost_is_valid() {
    assert_no_updates_error(&with_endpoint("http://localhost"), "CFG-X007");
}

#[test]
fn endpoint_http_localhost_lookalike_is_invalid() {
    assert_updates_error(
        &with_endpoint("http://localhostevil.com"),
        "CFG-X007",
        "updates.endpoint",
    );
}

#[test]
fn endpoint_empty_host_is_invalid() {
    assert_updates_error(&with_endpoint("https://"), "CFG-X007", "updates.endpoint");
}

#[test]
fn endpoint_ftp_scheme_is_invalid() {
    assert_updates_error(&with_endpoint("ftp://x"), "CFG-X007", "updates.endpoint");
}

#[test]
fn endpoint_with_space_is_invalid() {
    assert_updates_error(
        &with_endpoint("https://example.com/a b.json"),
        "CFG-X007",
        "updates.endpoint",
    );
}

#[test]
fn endpoint_300_chars_is_valid() {
    let e = format!("https://h/{}", "a".repeat(290));
    assert_eq!(e.chars().count(), 300);
    assert_no_updates_error(&with_endpoint(&e), "CFG-X007");
}

#[test]
fn endpoint_301_chars_is_invalid() {
    let e = format!("https://h/{}", "a".repeat(291));
    assert_eq!(e.chars().count(), 301);
    assert_updates_error(&with_endpoint(&e), "CFG-X007", "updates.endpoint");
}

/// Not asserted: the spec does not say whether an uppercase scheme is valid.
/// Run with `--nocapture` to see the observed result.
#[test]
fn endpoint_uppercase_scheme_is_reported_not_asserted() {
    let c = with_endpoint("HTTPS://x");
    let hits = diags_with(&c, "CFG-X007");
    if hits.is_empty() {
        eprintln!("OBSERVED: HTTPS://x -> accepted (no CFG-X007)");
    } else {
        eprintln!("OBSERVED: HTTPS://x -> rejected: {:?}", hits[0]);
    }
}

// ---------------------------------------------------------------------------
// helper methods
// ---------------------------------------------------------------------------

#[test]
fn effective_endpoint_uses_default_when_empty() {
    let u = Config::default().updates;
    assert_eq!(
        u.effective_endpoint("https://default.example/latest.json"),
        "https://default.example/latest.json"
    );
}

#[test]
fn effective_endpoint_uses_endpoint_when_set() {
    let mut c = Config::default();
    c.updates.endpoint = "http://127.0.0.1:9000/latest.json".into();
    assert_eq!(
        c.updates
            .effective_endpoint("https://default.example/latest.json"),
        "http://127.0.0.1:9000/latest.json"
    );
}

#[test]
fn checks_enabled_true_only_for_notify() {
    let mut c = Config::default();
    c.updates.check = UpdateCheck::Notify;
    assert!(c.updates.checks_enabled());
    c.updates.check = UpdateCheck::Never;
    assert!(!c.updates.checks_enabled());
}

// ---------------------------------------------------------------------------
// apply_class
// ---------------------------------------------------------------------------

#[test]
fn apply_class_live_when_check_changes() {
    assert_eq!(
        class_of(|c| c.updates.check = UpdateCheck::Never),
        ApplyClass::Live
    );
}

#[test]
fn apply_class_live_when_interval_changes() {
    assert_eq!(class_of(|c| c.updates.interval_hours = 6), ApplyClass::Live);
}

#[test]
fn apply_class_live_when_endpoint_changes() {
    assert_eq!(
        class_of(|c| c.updates.endpoint = "https://x.example/latest.json".into()),
        ApplyClass::Live
    );
}

#[test]
fn apply_class_live_for_identical_configs() {
    assert_eq!(class_of(|_| {}), ApplyClass::Live);
}

/// `channel` has only one variant (`stable`), so it cannot differ between two valid configs.
/// Checked instead: an explicit `channel = 'stable'` in TOML is Live relative to defaults.
#[test]
fn apply_class_live_for_explicit_stable_channel() {
    let old = Config::default();
    let new = parse("[updates]\nchannel = 'stable'\n");
    assert_eq!(apply_class(&old, &new), ApplyClass::Live);
}

#[test]
fn apply_class_not_lowered_by_stronger_change() {
    // An updates.* change combined with a non-Live change must keep the stronger class.
    let old = Config::default();
    let mut new = old.clone();
    new.updates.check = UpdateCheck::Never;
    new.updates.interval_hours = 6;
    new.audio.separate_tracks = false;
    let class = apply_class(&old, &new);
    assert_ne!(
        class,
        ApplyClass::Live,
        "stronger change must not be lowered"
    );
    assert_eq!(class, ApplyClass::AudioReinit);
}

// ---------------------------------------------------------------------------
// serde JSON shape
// ---------------------------------------------------------------------------

#[test]
fn updates_json_has_exactly_four_keys() {
    let v = serde_json::to_value(&Config::default().updates).expect("serialize to JSON");
    let obj = v
        .as_object()
        .expect("updates should serialize to an object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["channel", "check", "endpoint", "interval_hours"]);
}

#[test]
fn updates_json_default_values() {
    let v = serde_json::to_value(&Config::default().updates).expect("serialize to JSON");
    assert_eq!(v["check"], "notify");
    assert_eq!(v["channel"], "stable");
    assert_eq!(v["interval_hours"], 12);
    assert_eq!(v["endpoint"], "");
}
