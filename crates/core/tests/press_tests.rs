//! Integration tests for the `press` clip-hotkey option: `tap` (default) and `long`.
//! Config text is written the way users write it, and loaded with `Config::from_toml_str`.

use std::str::FromStr;

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

fn parse(text: &str) -> Config {
    Config::from_toml_str(text).expect("config should parse")
}

fn has_code(d: &[Diagnostic], code: &str) -> bool {
    d.iter().any(|x| x.code == code)
}

#[test]
fn config_round_trip_keeps_press() {
    let text = r#"
[[hotkeys.clips]]
name = "Long one"
keys = "Alt+F9"
seconds = 30
press = "long"

[[hotkeys.clips]]
name = "Tap one"
keys = "Alt+F10"
seconds = 60
press = "tap"
"#;
    let cfg = parse(text);
    assert_eq!(cfg.hotkeys.clips.len(), 2);
    assert_eq!(cfg.hotkeys.clips[0].press, Press::Long);
    assert_eq!(cfg.hotkeys.clips[1].press, Press::Tap);

    let out = cfg.to_toml_string().expect("serialize");
    let back = Config::from_toml_str(&out).expect("reparse serialized config");
    assert_eq!(back.hotkeys.clips.len(), 2);
    assert_eq!(
        back.hotkeys.clips[0].press,
        Press::Long,
        "serialized: {out}"
    );
    assert_eq!(back.hotkeys.clips[1].press, Press::Tap, "serialized: {out}");
    assert_eq!(back, cfg);
}

#[test]
fn default_clip_press_is_tap() {
    let defaults = Config::default();
    assert!(!defaults.hotkeys.clips.is_empty());
    assert!(
        defaults.hotkeys.clips.iter().all(|c| c.press == Press::Tap),
        "default clips: {:#?}",
        defaults.hotkeys.clips
    );

    let text = r#"
[[hotkeys.clips]]
name = "No press key"
keys = "Alt+F9"
seconds = 30
"#;
    let cfg = parse(text);
    assert_eq!(cfg.hotkeys.clips.len(), 1);
    assert_eq!(cfg.hotkeys.clips[0].press, Press::Tap);
}

#[test]
fn unknown_press_string_fails_to_load() {
    let text = r#"
[[hotkeys.clips]]
name = "Hold"
keys = "Alt+F9"
seconds = 30
press = "hold"
"#;
    assert!(
        Config::from_toml_str(text).is_err(),
        "press = \"hold\" must not load"
    );
    assert!(Press::from_str("hold").is_err());
}

#[test]
fn press_display_and_from_str() {
    assert_eq!(Press::Tap.to_string(), "tap");
    assert_eq!(Press::Long.to_string(), "long");
    assert_eq!(Press::from_str("long"), Ok(Press::Long));
    assert_eq!("long".parse::<Press>(), Ok(Press::Long));
    assert_eq!("tap".parse::<Press>(), Ok(Press::Tap));
}

#[test]
fn long_press_ms_is_400() {
    assert_eq!(LONG_PRESS_MS, 400);
}

#[test]
fn same_chord_tap_and_long_is_valid() {
    let text = r#"
[[hotkeys.clips]]
name = "Quick"
keys = "Alt+F9"
seconds = 30
press = "tap"

[[hotkeys.clips]]
name = "Held"
keys = "Alt+F9"
seconds = 60
press = "long"
"#;
    let cfg = parse(text);
    let d = validate(&cfg);
    assert!(
        !has_code(&d, "CFG-H002"),
        "tap+long should not conflict: {d:#?}"
    );
    let hotkey_errors: Vec<_> = d
        .iter()
        .filter(|x| x.path.starts_with("hotkeys") && x.severity == Severity::Error)
        .collect();
    assert!(
        hotkey_errors.is_empty(),
        "unexpected hotkey errors: {hotkey_errors:#?}"
    );
}

#[test]
fn same_chord_same_press_is_invalid() {
    let long_pair = r#"
[[hotkeys.clips]]
name = "Held A"
keys = "Alt+F9"
seconds = 30
press = "long"

[[hotkeys.clips]]
name = "Held B"
keys = "Alt+F9"
seconds = 60
press = "long"
"#;
    let d = validate(&parse(long_pair));
    assert!(
        has_code(&d, "CFG-H002"),
        "long+long should conflict: {d:#?}"
    );

    let tap_pair = r#"
[[hotkeys.clips]]
name = "Quick A"
keys = "Alt+F9"
seconds = 30

[[hotkeys.clips]]
name = "Quick B"
keys = "Alt+F9"
seconds = 60
"#;
    let d = validate(&parse(tap_pair));
    assert!(has_code(&d, "CFG-H002"), "tap+tap should conflict: {d:#?}");
}

#[test]
fn clip_and_session_on_same_chord_is_invalid() {
    let long_vs_session = r#"
[hotkeys.session]
split = "Alt+F11"

[[hotkeys.clips]]
name = "Held"
keys = "Alt+F11"
seconds = 30
press = "long"
"#;
    let d = validate(&parse(long_vs_session));
    assert!(
        has_code(&d, "CFG-H002"),
        "long clip vs session should conflict: {d:#?}"
    );

    let tap_vs_session = r#"
[hotkeys.session]
split = "Alt+F11"

[[hotkeys.clips]]
name = "Quick"
keys = "Alt+F11"
seconds = 30
press = "tap"
"#;
    let d = validate(&parse(tap_vs_session));
    assert!(
        has_code(&d, "CFG-H002"),
        "tap clip vs session should conflict: {d:#?}"
    );
}

#[test]
fn same_press_message_names_the_press() {
    let text = r#"
[[hotkeys.clips]]
name = "Held A"
keys = "Alt+F9"
seconds = 30
press = "long"

[[hotkeys.clips]]
name = "Held B"
keys = "Alt+F9"
seconds = 60
press = "long"
"#;
    let d = validate(&parse(text));
    let x = d
        .iter()
        .find(|x| x.code == "CFG-H002")
        .unwrap_or_else(|| panic!("expected CFG-H002 in {d:#?}"));
    assert!(x.message.contains("long press"), "message: {}", x.message);
}
