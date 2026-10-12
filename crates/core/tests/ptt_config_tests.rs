//! Tests for push-to-talk configuration: `hotkeys.session.push_to_talk`,
//! `audio.mic.ptt_release_ms`, `audio.mic.ptt_separate_track`, and the track plan that
//! follows from them. Public API only.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn chord(s: &str) -> KeyChord {
    s.parse().expect("test chord should parse")
}

fn parse(text: &str) -> Config {
    Config::from_toml_str(text).expect("config should parse")
}

fn find<'a>(d: &'a [Diagnostic], code: &str) -> &'a Diagnostic {
    d.iter()
        .find(|x| x.code == code)
        .unwrap_or_else(|| panic!("expected {code} in {d:#?}"))
}

fn assert_error(c: &Config, code: &str, path: &str) {
    let d = validate(c);
    let x = find(&d, code);
    assert_eq!(
        x.severity,
        Severity::Error,
        "{code} should be an error: {x:?}"
    );
    assert_eq!(x.path, path, "{code} path: {x:?}");
}

fn assert_no(c: &Config, code: &str) {
    let d = validate(c);
    assert!(
        !d.iter().any(|x| x.code == code),
        "{code} should not fire: {d:#?}"
    );
}

fn titles(plan: &[TrackPlan]) -> Vec<&str> {
    plan.iter().map(|t| t.title.as_str()).collect()
}

/// A bare mouse name used as the only PTT key must not trigger chord diagnostics.
fn assert_no_chord_error_on_bare(name: &str) {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![chord(name)];
    let d = validate(&c);
    assert!(
        !d.iter()
            .any(|x| matches!(x.code, "CFG-H002" | "CFG-H003") && x.severity == Severity::Error),
        "{name} as PTT key: {d:#?}"
    );
}

/// A default config with the mic set up for a valid separate push-to-talk track.
fn ptt_separate_base() -> Config {
    let mut c = Config::default();
    c.audio.mic.enabled = true;
    c.audio.separate_tracks = true;
    c.audio.mic.ptt_separate_track = true;
    c.hotkeys.session.push_to_talk = vec![chord("Mouse4")];
    c
}

// ---------------------------------------------------------------------------
// 1. defaults and TOML
// ---------------------------------------------------------------------------

#[test]
fn push_to_talk_defaults_are_empty_and_off() {
    let c = Config::default();
    assert!(c.hotkeys.session.push_to_talk.is_empty(), "no PTT keys");
    assert_eq!(c.audio.mic.ptt_release_ms, 250, "default release tail");
    assert!(!c.audio.mic.ptt_separate_track, "separate track off");
}

#[test]
fn push_to_talk_parses_from_toml() {
    let c = parse(
        r#"
[hotkeys.session]
push_to_talk = ["Mouse4", "Ctrl+Space"]
"#,
    );
    let keys = &c.hotkeys.session.push_to_talk;
    assert_eq!(keys.len(), 2, "two PTT keys: {keys:?}");
    assert_eq!(keys[0], chord("Mouse4"), "bare mouse key");
    assert_eq!(keys[1], chord("Ctrl+Space"), "modifier chord");
    assert_no(&c, "CFG-H009");
}

#[test]
fn mouse_buttons_parse_as_bare_keys() {
    for name in ["Mouse3", "Mouse4", "Mouse5"] {
        let k = chord(name);
        assert_eq!(k, chord(name), "{name} parses deterministically");
        assert_no_chord_error_on_bare(name);
    }
    // Round trip through TOML keeps the mouse names.
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![chord("Mouse3"), chord("Mouse5")];
    let text = c.to_toml_string().expect("serialize");
    let back = parse(&text);
    assert_eq!(
        back.hotkeys.session.push_to_talk,
        vec![chord("Mouse3"), chord("Mouse5")],
        "mouse PTT keys survive save and load"
    );
}

#[test]
fn push_to_talk_round_trips_through_toml() {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![chord("Mouse4"), chord("Ctrl+Space")];
    c.audio.mic.ptt_release_ms = 900;
    c.audio.mic.ptt_separate_track = false;
    let text = c.to_toml_string().expect("serialize");
    let back = parse(&text);
    assert_eq!(back, c, "full config round trip");
}

#[test]
fn default_config_with_no_ptt_is_clean() {
    let d = validate(&Config::default());
    assert!(d.is_empty(), "default config: {d:#?}");
}

// ---------------------------------------------------------------------------
// 2. push_to_talk validation
// ---------------------------------------------------------------------------

#[test]
fn four_ptt_keys_are_allowed() {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![
        chord("Mouse3"),
        chord("Mouse4"),
        chord("Mouse5"),
        chord("Ctrl+Space"),
    ];
    assert_no(&c, "CFG-H009");
}

#[test]
fn more_than_four_ptt_keys_is_h009() {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![
        chord("Mouse3"),
        chord("Mouse4"),
        chord("Mouse5"),
        chord("Ctrl+Space"),
        chord("Ctrl+Shift+V"),
    ];
    assert_error(&c, "CFG-H009", "hotkeys.session.push_to_talk");
}

#[test]
fn ptt_key_equal_to_mute_mic_is_h002() {
    let mut c = Config::default();
    let mute = c.hotkeys.session.mute_mic.expect("default mute key");
    c.hotkeys.session.push_to_talk = vec![mute];
    let d = validate(&c);
    let x = find(&d, "CFG-H002");
    assert_eq!(x.severity, Severity::Error, "H002 is an error: {x:?}");
    assert!(
        x.path.starts_with("hotkeys.session.push_to_talk["),
        "path names the PTT entry: {x:?}"
    );
}

#[test]
fn ptt_key_equal_to_clip_hotkey_is_h002() {
    let mut c = Config::default();
    let clip = c.hotkeys.clips[0].keys;
    c.hotkeys.session.push_to_talk = vec![clip];
    let d = validate(&c);
    let x = find(&d, "CFG-H002");
    assert_eq!(x.severity, Severity::Error, "H002 is an error: {x:?}");
    assert!(
        x.path.starts_with("hotkeys.session.push_to_talk["),
        "path names the PTT entry: {x:?}"
    );
}

#[test]
fn reserved_chord_as_ptt_is_h003() {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![chord("Alt+F4")];
    assert_error(&c, "CFG-H003", "hotkeys.session.push_to_talk[0]");
}

#[test]
fn duplicate_ptt_keys_are_h002() {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![chord("Mouse4"), chord("Mouse4")];
    let d = validate(&c);
    let x = find(&d, "CFG-H002");
    assert_eq!(x.severity, Severity::Error, "H002 is an error: {x:?}");
    assert!(
        x.path.starts_with("hotkeys.session.push_to_talk["),
        "path names the PTT entry: {x:?}"
    );
}

#[test]
fn distinct_ptt_keys_are_not_h002() {
    let mut c = Config::default();
    c.hotkeys.session.push_to_talk = vec![chord("Mouse4"), chord("Mouse5")];
    assert_no(&c, "CFG-H002");
}

// ---------------------------------------------------------------------------
// 3. ptt_release_ms validation
// ---------------------------------------------------------------------------

#[test]
fn release_ms_bounds() {
    let mut c = Config::default();
    c.audio.mic.ptt_release_ms = 0;
    assert_no(&c, "CFG-A014");

    c.audio.mic.ptt_release_ms = 2000;
    assert_no(&c, "CFG-A014");
}

#[test]
fn release_ms_above_2000_is_a014() {
    let mut c = Config::default();
    c.audio.mic.ptt_release_ms = 2001;
    assert_error(&c, "CFG-A014", "audio.mic.ptt_release_ms");
}

#[test]
fn release_ms_parses_from_toml() {
    let c = parse(
        r#"
[audio.mic]
ptt_release_ms = 1500
"#,
    );
    assert_eq!(c.audio.mic.ptt_release_ms, 1500);
    assert_no(&c, "CFG-A014");
}

// ---------------------------------------------------------------------------
// 4. ptt_separate_track validation
// ---------------------------------------------------------------------------

#[test]
fn separate_track_with_valid_combination_is_clean() {
    let c = ptt_separate_base();
    assert_no(&c, "CFG-A015");
}

#[test]
fn separate_track_without_any_ptt_key_is_a015() {
    let mut c = ptt_separate_base();
    c.hotkeys.session.push_to_talk.clear();
    assert_error(&c, "CFG-A015", "audio.mic.ptt_separate_track");
}

#[test]
fn separate_track_with_mic_disabled_is_a015() {
    let mut c = ptt_separate_base();
    c.audio.mic.enabled = false;
    assert_error(&c, "CFG-A015", "audio.mic.ptt_separate_track");
}

#[test]
fn separate_track_without_separate_tracks_is_a015() {
    let mut c = ptt_separate_base();
    c.audio.separate_tracks = false;
    assert_error(&c, "CFG-A015", "audio.mic.ptt_separate_track");
}

#[test]
fn separate_track_off_never_fires_a015() {
    let mut c = Config::default();
    c.audio.mic.ptt_separate_track = false;
    c.audio.mic.enabled = false;
    c.audio.separate_tracks = false;
    assert_no(&c, "CFG-A015");
}

#[test]
fn separate_track_accepts_legacy_push_to_talk_mode() {
    let mut c = Config::default();
    c.audio.mic.enabled = true;
    c.audio.separate_tracks = true;
    c.audio.mic.ptt_separate_track = true;
    c.hotkeys.session.push_to_talk.clear();
    c.hotkeys.session.mute_mic_mode = MuteMode::PushToTalk;
    c.hotkeys.session.mute_mic = Some(chord("Alt+F7"));
    assert_no(&c, "CFG-A015");
}

// ---------------------------------------------------------------------------
// 5. track plan
// ---------------------------------------------------------------------------

#[test]
fn default_track_plan_has_no_push_to_talk_track() {
    let c = Config::default();
    let plan = c.audio.track_plan();
    assert!(
        !titles(&plan).contains(&"Push to talk"),
        "no PTT track by default: {:?}",
        titles(&plan)
    );
    assert!(
        plan.iter().all(|t| !t.sources.contains(&SourceRef::MicPtt)),
        "MicPtt never appears by default"
    );
}

#[test]
fn separate_ptt_track_plan_titles_and_order() {
    let c = ptt_separate_base();
    let plan = c.audio.track_plan();
    assert_eq!(
        titles(&plan),
        vec!["Mix", "Game", "Microphone", "Push to talk", "Discord"],
        "track order with separate PTT track"
    );
}

#[test]
fn separate_ptt_track_mix_has_mic_but_not_mic_ptt() {
    let c = ptt_separate_base();
    let plan = c.audio.track_plan();
    let mix = &plan[0];
    assert_eq!(mix.title, "Mix");
    assert!(
        mix.sources.contains(&SourceRef::Mic),
        "Mix carries Mic: {:?}",
        mix.sources
    );
    assert!(
        !mix.sources.contains(&SourceRef::MicPtt),
        "Mix must NOT carry MicPtt: {:?}",
        mix.sources
    );
}

#[test]
fn separate_ptt_track_has_only_mic_ptt_source() {
    let c = ptt_separate_base();
    let plan = c.audio.track_plan();
    let ptt = plan
        .iter()
        .find(|t| t.title == "Push to talk")
        .expect("Push to talk track");
    assert_eq!(
        ptt.sources,
        vec![SourceRef::MicPtt],
        "PTT track sources exactly [MicPtt]"
    );
}

#[test]
fn mic_disabled_means_no_push_to_talk_track() {
    let mut c = ptt_separate_base();
    c.audio.mic.enabled = false;
    let plan = c.audio.track_plan();
    assert!(
        !titles(&plan).contains(&"Push to talk"),
        "no PTT track with mic disabled: {:?}",
        titles(&plan)
    );
}

#[test]
fn gain_for_mic_ptt_equals_mic_gain() {
    let mut c = Config::default();
    c.audio.mic.gain_db = -7.5;
    assert_eq!(c.audio.gain_for(&SourceRef::MicPtt), -7.5);
    assert_eq!(
        c.audio.gain_for(&SourceRef::MicPtt),
        c.audio.gain_for(&SourceRef::Mic),
        "MicPtt gain matches Mic gain"
    );
}
