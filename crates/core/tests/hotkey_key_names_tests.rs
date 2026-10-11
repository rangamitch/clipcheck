//! Config-level rules for hotkey key names, from docs/design/hotkey-key-names.md.
//!
//! Escape is refused everywhere, Alt+Space is refused as reserved, Space and Ctrl+Space are
//! accepted. Public API only; nothing here touches the user's config or recordings.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

fn h001_at<'a>(d: &'a [Diagnostic], path: &str) -> Option<&'a Diagnostic> {
    d.iter().find(|d| d.code == "CFG-H001" && d.path == path)
}

fn session_split_toml(chord: &str) -> String {
    format!("[hotkeys.session]\nsplit = \"{chord}\"\n")
}

#[test]
fn ctrl_space_is_accepted_in_config_and_validates_clean() {
    let cfg = Config::from_toml_str(&session_split_toml("Ctrl+Space"))
        .expect("Ctrl+Space is a valid chord");
    assert_eq!(
        cfg.hotkeys.session.split,
        Some("Ctrl+Space".parse::<KeyChord>().expect("parses"))
    );
    assert!(h001_at(&validate(&cfg), "hotkeys.session.split").is_none());
}

#[test]
fn bare_space_is_accepted_for_a_clip_hotkey() {
    let text = "[[hotkeys.clips]]\nname = \"s\"\nkeys = \"Space\"\nseconds = 30\n";
    let cfg = Config::from_toml_str(text).expect("bare Space is a valid chord");
    assert!(cfg.hotkeys.clips.iter().any(|c| c.name == "s"));
    assert!(validate(&cfg)
        .iter()
        .all(|d| d.code != "CFG-H001" || !d.path.contains("clips")));
}

#[test]
fn alt_space_is_refused_as_reserved_at_parse_and_validation() {
    assert!(Config::from_toml_str(&session_split_toml("Alt+Space")).is_err());
    let mut cfg = Config::default();
    cfg.hotkeys.session.split = Some(KeyChord::alt(0x20));
    let diags = validate(&cfg);
    let d = h001_at(&diags, "hotkeys.session.split").expect("CFG-H001 at session split");
    assert!(d.message.contains("Alt+Space is reserved"), "{}", d.message);
}

#[test]
fn escape_is_refused_in_every_form_at_parse() {
    for chord in ["Escape", "escape", "Esc", "Ctrl+Escape", "Shift+Esc"] {
        assert!(
            Config::from_toml_str(&session_split_toml(chord)).is_err(),
            "{chord} must be refused"
        );
        assert!(chord.parse::<KeyChord>().is_err(), "{chord} must not parse");
    }
}

#[test]
fn escape_is_refused_in_clip_hotkeys_at_parse() {
    let text = "[[hotkeys.clips]]\nname = \"x\"\nkeys = \"Escape\"\nseconds = 30\n";
    assert!(Config::from_toml_str(text).is_err());
}

#[test]
fn escape_chord_fails_validation_as_cfg_h001_for_session_and_clip() {
    let mut cfg = Config::default();
    cfg.hotkeys.session.split = Some(KeyChord {
        mods: Modifiers::default(),
        vk: 0x1B,
    });
    cfg.hotkeys.clips[1].keys = KeyChord::alt(0x1B);
    let diags = validate(&cfg);
    let session = h001_at(&diags, "hotkeys.session.split").expect("session CFG-H001");
    assert!(
        session.message.contains("Escape is reserved"),
        "{}",
        session.message
    );
    let clip = h001_at(&diags, "hotkeys.clips[1].keys").expect("clip CFG-H001");
    assert!(
        clip.message.contains("Escape is reserved"),
        "{}",
        clip.message
    );
}

#[test]
fn menu_is_an_alias_for_apps_key() {
    let menu: KeyChord = "Alt+Menu".parse().expect("Menu parses");
    let apps: KeyChord = "Alt+AppsKey".parse().expect("AppsKey parses");
    assert_eq!(menu, apps);
    assert_eq!(menu.vk, 0x5D);
}
