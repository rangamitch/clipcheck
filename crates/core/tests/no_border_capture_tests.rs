//! Black-box tests for the `video.no_border_capture` setting (config layer).
//! Written by the tester agent. Uses only the public config API.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

fn class_of(mutate: impl FnOnce(&mut Config)) -> ApplyClass {
    let old = Config::default();
    let mut new = old.clone();
    mutate(&mut new);
    apply_class(&old, &new)
}

/// Returns the text of the `[video]` table in the default template (up to the next table header).
fn video_block(toml: &str) -> String {
    let start = toml
        .find("[video]")
        .expect("default template has a [video] table");
    let rest = &toml[start + "[video]".len()..];
    let end = rest.find("\n[").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn default_is_false() {
    assert!(!Video::default().no_border_capture);
    assert!(!Config::default().video.no_border_capture);
}

#[test]
fn template_contains_key_in_video_table_set_to_false() {
    let block = video_block(Config::default_toml());
    assert!(
        block
            .lines()
            .any(|l| l.trim_start().starts_with("no_border_capture") && l.contains("= false")),
        "video block lacks `no_border_capture = false`:\n{block}"
    );
}

#[test]
fn template_parses_to_default() {
    let cfg = Config::from_toml_str(Config::default_toml()).expect("template parses");
    assert!(!cfg.video.no_border_capture);
}

#[test]
fn true_in_video_table_loads_as_true() {
    let text =
        Config::default_toml().replacen("no_border_capture = false", "no_border_capture = true", 1);
    assert!(
        text.contains("no_border_capture = true"),
        "replacement did not apply"
    );
    let cfg = Config::from_toml_str(&text).expect("parses");
    assert!(cfg.video.no_border_capture);
}

#[test]
fn missing_key_loads_as_false() {
    let text: String = Config::default_toml()
        .lines()
        .filter(|l| !l.trim_start().starts_with("no_border_capture"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(!text.contains("no_border_capture"), "key was not removed");
    let cfg = Config::from_toml_str(&text).expect("parses without key");
    assert!(!cfg.video.no_border_capture);
}

#[test]
fn round_trip_keeps_true() {
    let mut cfg = Config::default();
    cfg.video.no_border_capture = true;
    let text = cfg.to_toml_string().expect("serializes");
    let back = Config::from_toml_str(&text).expect("re-parses");
    assert!(back.video.no_border_capture);
}

#[test]
fn round_trip_keeps_false() {
    let cfg = Config::default();
    let text = cfg.to_toml_string().expect("serializes");
    let back = Config::from_toml_str(&text).expect("re-parses");
    assert!(!back.video.no_border_capture);
}

#[test]
fn toggling_only_this_key_is_pipeline_reinit() {
    assert_eq!(
        class_of(|c| c.video.no_border_capture = true),
        ApplyClass::PipelineReinit
    );
}

#[test]
fn validation_accepts_both_values() {
    for value in [false, true] {
        let mut cfg = Config::default();
        cfg.video.no_border_capture = value;
        let errors: Vec<_> = validate(&cfg)
            .into_iter()
            .filter(|d| d.path.contains("no_border_capture") || d.severity == Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "validate({value}) produced {:?}",
            errors
                .iter()
                .map(|d| (&d.code, &d.path, &d.message))
                .collect::<Vec<_>>()
        );
    }
}
