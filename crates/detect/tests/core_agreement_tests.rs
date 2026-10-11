//! Agreement between clipcheck-detect's exe normalization and clipcheck-core's id keys.
//!
//! detect keeps the `.exe` suffix (matching key); core's game id key drops one trailing `.exe`.

use clipcheck_core::ids::{exe_file_name_lower, normalize_exe as core_normalize_exe, GameId};
use clipcheck_detect::lists::{normalize_exe as detect_normalize_exe, Lists};

const SAMPLES: &[&str] = &[
    r"C:\Games\Foo.EXE",
    "/usr/bin/foo",
    " Hades.exe ",
    "hades",
    ".exe",
    "",
    "\u{c9}.EXE",
    r"a\b/c.exe",
    r"C:\Games\",
    "/usr/bin/",
    "MiXeD CaSe.ExE",
    r"C:\a\b\Hades.exe",
    "Hades.exe.exe",
    r"  C:\Games\ Hades.exe  ",
    "\u{c4}\u{d6}\u{dc}.exe",
    r"C:\Games\Foo.exe.bak",
];

/// Expected detect result for each sample, index-aligned with SAMPLES.
const EXPECTED_DETECT: &[&str] = &[
    "foo.exe",
    "foo",
    "hades.exe",
    "hades",
    ".exe",
    "",
    "\u{e9}.exe",
    "c.exe",
    "",
    "",
    "mixed case.exe",
    "hades.exe",
    "hades.exe.exe",
    "hades.exe",
    "\u{e4}\u{f6}\u{fc}.exe",
    "foo.exe.bak",
];

#[test]
fn samples_table_is_aligned() {
    assert_eq!(SAMPLES.len(), EXPECTED_DETECT.len());
    assert!(SAMPLES.len() >= 15);
}

#[test]
fn detect_normalize_matches_core_file_name_key() {
    for s in SAMPLES {
        assert_eq!(
            detect_normalize_exe(s),
            exe_file_name_lower(s),
            "detect vs core exe_file_name_lower for {s:?}"
        );
    }
}

#[test]
fn detect_normalize_matches_expected_values() {
    for (s, want) in SAMPLES.iter().zip(EXPECTED_DETECT) {
        assert_eq!(detect_normalize_exe(s), *want, "detect normalize of {s:?}");
    }
}

#[test]
fn core_id_key_is_detect_key_minus_one_exe_suffix() {
    for s in SAMPLES {
        let detect = detect_normalize_exe(s);
        let core = core_normalize_exe(s);
        let expected = detect.strip_suffix(".exe").unwrap_or(&detect).to_owned();
        assert_eq!(core, expected, "core normalize of {s:?}");
    }
}

#[test]
fn semantic_difference_keeps_exe_in_detect_only() {
    assert_eq!(detect_normalize_exe("Hades.exe"), "hades.exe");
    assert_eq!(core_normalize_exe("Hades.exe"), "hades");
    assert_ne!(
        detect_normalize_exe("Hades.exe"),
        core_normalize_exe("Hades.exe")
    );
    // Without an .exe suffix the two keys agree.
    assert_eq!(detect_normalize_exe("hades"), core_normalize_exe("hades"));
}

#[test]
fn game_id_from_exe_drops_exe_and_dir() {
    assert_eq!(GameId::new("Hades.exe").exe(), "hades");
    assert_eq!(GameId::from_exe(r"C:\Games\Foo.EXE").as_str(), "foo");
    assert_eq!(GameId::from_exe(r"C:\Games\Foo.EXE").exe(), "foo");
}

#[test]
fn game_id_with_store_keeps_exe_part_normalized() {
    let id = GameId::new("Hades.exe@Steam:1145360");
    assert_eq!(id.exe(), "hades");
    assert_eq!(id.store(), Some(("steam", "1145360")));
}

#[test]
fn unicode_exe_in_block_extra_matches_case_insensitively() {
    // Entry written with uppercase non-ASCII and .EXE; process names in other cases.
    let lists = Lists::new(&["\u{c9}.EXE".to_string()], &[]);
    assert!(lists.is_blocked("\u{e9}.exe"), "lowercase unicode name");
    assert!(lists.is_blocked("\u{c9}.exe"), "uppercase unicode name");
    assert!(
        lists.is_blocked("C:\\Apps\\\u{c9}.EXE"),
        "directory part stripped"
    );
    assert!(
        !lists.is_blocked("e.exe"),
        "ASCII base letter must not match"
    );
}

#[test]
fn ascii_block_extra_matches_with_directory_and_case() {
    let lists = Lists::new(&["MyGame.exe".to_string()], &[]);
    assert!(lists.is_blocked(r"D:\Games\MYGAME.EXE"));
    assert!(lists.is_blocked("  mygame.exe  "));
    assert!(
        !lists.is_blocked("mygame"),
        "entry keeps .exe, so bare stem does not match"
    );
}

#[test]
fn core_key_entry_without_exe_does_not_block_detect_name() {
    // Probe: a blocklist entry in core's key form (no .exe) against a process name.
    // detect's key keeps .exe on both sides, so this is expected NOT to match.
    let lists = Lists::new(&["hades".to_string()], &[]);
    assert!(!lists.is_blocked("Hades.exe"));
}
