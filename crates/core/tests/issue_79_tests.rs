//! Tests for issues #79 and #92: canonical `[games."<id>"]` keys.
//!
//! Canonical form is `name.exe` or `name.exe@store:id`, lowercase exe and store name,
//! surrounding whitespace trimmed. Covers `normalize_game_key`, `Config::game_override`
//! and the CFG-G001 validation rules.

use clipcheck_core::config::schema::*;
use clipcheck_core::config::validate::*;

fn over(name: &str) -> GameOverride {
    GameOverride {
        display_name: Some(name.to_owned()),
        ..Default::default()
    }
}

/// Config with the given raw map keys (inserted verbatim, so spelling is preserved).
fn cfg_with(keys: &[&str]) -> Config {
    let mut c = Config::default();
    for k in keys {
        c.games.insert((*k).to_owned(), over(k));
    }
    c
}

fn g001(c: &Config) -> Vec<Diagnostic> {
    validate(c)
        .into_iter()
        .filter(|d| d.code == "CFG-G001")
        .collect()
}

fn name_of(c: &Config, query: &str) -> String {
    c.game_override(query)
        .and_then(|o| o.display_name.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// normalize_game_key
// ---------------------------------------------------------------------------

#[test]
fn normalize_accepts_canonical_and_case_variants() {
    assert_eq!(normalize_game_key("cs2.exe").as_deref(), Some("cs2.exe"));
    assert_eq!(normalize_game_key("CS2.EXE").as_deref(), Some("cs2.exe"));
    assert_eq!(
        normalize_game_key("  Cs2.exe\t").as_deref(),
        Some("cs2.exe")
    );
    assert_eq!(
        normalize_game_key("cs2.exe@steam:730").as_deref(),
        Some("cs2.exe@steam:730")
    );
    assert_eq!(
        normalize_game_key("CS2.EXE@STEAM:730").as_deref(),
        Some("cs2.exe@steam:730")
    );
    assert_eq!(
        normalize_game_key(" cs2.exe@Steam:730 ").as_deref(),
        Some("cs2.exe@steam:730")
    );
}

#[test]
fn normalize_rejects_invalid_keys() {
    let bad = [
        "",
        "   ",
        ".exe",
        "cs2",
        "cs2.dll",
        "cs2.exe.bak",
        "exe:cs2.exe",
        "EXE:cs2.exe",
        "games/cs2.exe",
        "games\\cs2.exe",
        "C:\\Games\\cs2.exe",
        "cs2.exe@steam",
        "cs2.exe@:730",
        "cs2.exe@steam:",
        "cs2.exe@",
    ];
    for k in bad {
        assert_eq!(normalize_game_key(k), None, "normalize_game_key({k:?})");
    }
}

// ---------------------------------------------------------------------------
// CFG-G001 validation
// ---------------------------------------------------------------------------

#[test]
fn valid_keys_produce_no_g001() {
    for k in [
        "cs2.exe",
        "CS2.EXE",
        " cs2.exe ",
        "cs2.exe@steam:730",
        "CS2.EXE@STEAM:730",
        "hades.exe@epic:1",
    ] {
        let c = cfg_with(&[k]);
        assert!(
            g001(&c).is_empty(),
            "{k:?} should be valid: {:#?}",
            validate(&c)
        );
    }
}

#[test]
fn invalid_keys_produce_g001_with_path() {
    for k in [
        "cs2",
        ".exe",
        "cs2.dll",
        "cs2.exe@steam",
        "cs2.exe@:730",
        "cs2.exe@steam:",
        "games/cs2.exe",
        "C:\\Games\\cs2.exe",
        "",
    ] {
        let c = cfg_with(&[k]);
        let d = g001(&c);
        assert_eq!(d.len(), 1, "{k:?} should give one CFG-G001: {d:#?}");
        assert_eq!(d[0].severity, Severity::Error, "{k:?}");
        assert_eq!(d[0].path, format!("games.\"{k}\""), "{k:?}");
    }
}

#[test]
fn exe_prefix_form_is_rejected_with_specific_message() {
    for k in ["exe:cs2.exe", "EXE:cs2.exe", "  exe:cs2.exe"] {
        let c = cfg_with(&[k]);
        let d = g001(&c);
        assert_eq!(d.len(), 1, "{k:?}: {d:#?}");
        assert_eq!(d[0].severity, Severity::Error, "{k:?}");
        assert!(
            d[0].message.contains("'exe:' prefix"),
            "{k:?} should get the exe: prefix diagnostic, got: {}",
            d[0].message
        );
    }
}

#[test]
fn non_prefix_invalid_key_gets_generic_message() {
    let d = g001(&cfg_with(&["cs2"]));
    assert_eq!(d.len(), 1);
    assert!(
        !d[0].message.contains("'exe:' prefix"),
        "generic key should not get the prefix message: {}",
        d[0].message
    );
}

#[test]
fn duplicates_after_normalization_are_g001() {
    let pairs: &[(&str, &str)] = &[
        ("Cs2.EXE", "cs2.exe"),
        (" cs2.exe ", "cs2.exe"),
        ("cs2.exe@STEAM:730", "cs2.exe@steam:730"),
    ];
    for (a, b) in pairs {
        let c = cfg_with(&[a, b]);
        let d = g001(&c);
        assert_eq!(d.len(), 1, "{a:?} vs {b:?} should be one duplicate: {d:#?}");
        assert!(
            d[0].message.contains("duplicates"),
            "{a:?} vs {b:?}: {}",
            d[0].message
        );
    }
}

#[test]
fn distinct_stores_and_bare_key_are_not_duplicates() {
    let c = cfg_with(&["cs2.exe", "cs2.exe@steam:730", "cs2.exe@epic:1"]);
    assert!(g001(&c).is_empty(), "{:#?}", validate(&c));
}

// ---------------------------------------------------------------------------
// game_override lookup
// ---------------------------------------------------------------------------

#[test]
fn exact_normalized_key_wins_over_bare() {
    let c = cfg_with(&["cs2.exe", "cs2.exe@steam:730"]);
    assert_eq!(name_of(&c, "cs2.exe@steam:730"), "cs2.exe@steam:730");
    assert_eq!(name_of(&c, "CS2.EXE@STEAM:730"), "cs2.exe@steam:730");
    assert_eq!(name_of(&c, "cs2.exe"), "cs2.exe");
}

#[test]
fn bare_query_matches_bare_entry_any_case_and_whitespace() {
    let c = cfg_with(&["Cs2.EXE"]);
    assert_eq!(name_of(&c, "cs2.exe"), "Cs2.EXE");
    assert_eq!(name_of(&c, "CS2.exe"), "Cs2.EXE");
    assert_eq!(name_of(&c, "  cs2.exe  "), "Cs2.EXE");
}

#[test]
fn qualified_query_falls_back_to_bare_entry() {
    let c = cfg_with(&["cs2.exe"]);
    assert_eq!(name_of(&c, "cs2.exe@steam:730"), "cs2.exe");
}

#[test]
fn bare_query_uses_first_qualified_entry_when_no_bare() {
    let c = cfg_with(&["cs2.exe@steam:999", "cs2.exe@steam:730"]);
    // BTreeMap order: "cs2.exe@steam:730" sorts before "cs2.exe@steam:999".
    assert_eq!(name_of(&c, "cs2.exe"), "cs2.exe@steam:730");
    assert_eq!(name_of(&c, "CS2.EXE"), "cs2.exe@steam:730");
}

#[test]
fn bare_query_prefers_bare_over_qualified() {
    let c = cfg_with(&["cs2.exe@steam:730", "cs2.exe"]);
    assert_eq!(name_of(&c, "cs2.exe"), "cs2.exe");
}

#[test]
fn qualified_query_does_not_match_other_store() {
    let c = cfg_with(&["cs2.exe@steam:730"]);
    assert!(c.game_override("cs2.exe@epic:730").is_none());
    assert!(c.game_override("cs2.exe@steam:1").is_none());
}

#[test]
fn unknown_or_invalid_queries_return_none() {
    let c = cfg_with(&["cs2.exe", "cs2.exe@steam:730"]);
    assert!(c.game_override("missing.exe").is_none());
    assert!(c.game_override("cs2").is_none());
    assert!(c.game_override("").is_none());
    assert!(c.game_override("exe:cs2.exe").is_none());
    assert!(c.game_override("games/cs2.exe").is_none());
}

#[test]
fn invalid_map_keys_are_ignored_by_lookup() {
    let c = cfg_with(&["exe:cs2.exe", "cs2"]);
    assert!(c.game_override("cs2.exe").is_none());
    assert!(c.game_override("cs2").is_none());
}

// ---------------------------------------------------------------------------
// TOML round trip
// ---------------------------------------------------------------------------

const GAMES_TOML: &str = r#"
[games."cs2.exe"]
display_name = "CS2 bare"

[games."Hades.EXE@epic:1145360"]
display_name = "Hades epic"

[games."cs2.exe@steam:730"]
display_name = "CS2 steam"
"#;

#[test]
fn toml_with_several_games_validates_clean() {
    let c = Config::from_toml_str(GAMES_TOML).expect("parses");
    let d = validate(&c);
    assert!(d.is_empty(), "expected no diagnostics: {d:#?}");
    assert_eq!(c.games.len(), 3);
}

#[test]
fn toml_games_lookups_hit() {
    let c = Config::from_toml_str(GAMES_TOML).expect("parses");
    assert_eq!(name_of(&c, "CS2.EXE"), "CS2 bare");
    assert_eq!(name_of(&c, "cs2.exe@steam:730"), "CS2 steam");
    assert_eq!(name_of(&c, "hades.exe"), "Hades epic");
    assert_eq!(name_of(&c, "hades.exe@EPIC:1145360"), "Hades epic");
    assert_eq!(name_of(&c, "hades.exe@steam:1145360"), "");
}

#[test]
fn toml_round_trip_keeps_games_and_lookups() {
    let c = Config::from_toml_str(GAMES_TOML).expect("parses");
    let text = c.to_toml_string().expect("serializes");
    let back = Config::from_toml_str(&text).expect("re-parses");
    assert_eq!(back, c);
    assert!(validate(&back).is_empty(), "{:#?}", validate(&back));
    assert_eq!(name_of(&back, "cs2.exe@steam:730"), "CS2 steam");
    assert_eq!(name_of(&back, "CS2.EXE"), "CS2 bare");
}

#[test]
fn toml_exe_prefix_key_is_reported_not_silently_accepted() {
    let c = Config::from_toml_str(
        r#"
[games."exe:cs2.exe"]
display_name = "bad"
"#,
    )
    .expect("parses");
    let d = g001(&c);
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(d[0].message.contains("'exe:' prefix"));
    assert!(c.game_override("cs2.exe").is_none());
}
