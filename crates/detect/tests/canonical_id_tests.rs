//! Stable ids and names for wildcard known games, and name matching for config lists.
//! Public API only.

use clipcheck_detect::known_games::{canonical_for, is_known, stable_name};
use clipcheck_detect::lists::{name_matches, Lists};

#[test]
fn every_fivem_build_gets_the_same_canonical_id() {
    for exe in [
        "FiveM_b2802_GTAProcess.exe",
        "FiveM_b3258_GTAProcess.exe",
        "fivem_b3258_gtaprocess.EXE",
        "C:\\Users\\me\\AppData\\Local\\FiveM\\FiveM.app\\FiveM_b3258_GameProcess.exe",
    ] {
        assert_eq!(canonical_for(exe).as_deref(), Some("exe:fivem"), "{exe}");
        assert_eq!(stable_name(exe), Some("FiveM"), "{exe}");
    }
}

#[test]
fn exact_known_games_have_no_canonical_id() {
    // Plain entries keep their file-name ids, so existing library rows do not move.
    assert_eq!(canonical_for("Terraria.exe"), None);
    assert_eq!(stable_name("Terraria.exe"), None);
    assert_eq!(is_known("Terraria.exe"), Some("Terraria"));
}

#[test]
fn unknown_games_have_no_canonical_id() {
    assert_eq!(canonical_for("not-a-game.exe"), None);
    assert_eq!(stable_name("not-a-game.exe"), None);
    assert_eq!(canonical_for(""), None);
    assert_eq!(canonical_for("   "), None);
}

#[test]
fn canonical_id_is_lowercase_without_spaces() {
    // The known-game database is embedded, so use the real data: every pattern entry
    // yields an id that is lowercase, has no spaces and starts with `exe:`.
    let mut checked = 0;
    for game in clipcheck_detect::known_games::entries() {
        if !game.exe.contains(['*', '?']) {
            continue;
        }
        // Build a concrete file name by replacing each wildcard with a fixed filler.
        let concrete: String = game
            .exe
            .chars()
            .map(|c| if c == '*' || c == '?' { 'x' } else { c })
            .collect();
        let Some(id) = canonical_for(&concrete) else {
            continue;
        };
        checked += 1;
        assert!(id.starts_with("exe:"), "{id}");
        assert!(!id.contains(' '), "{id}");
        assert_eq!(id, id.to_ascii_lowercase(), "{id}");
    }
    assert!(
        checked >= 2,
        "expected FiveM pattern entries, checked {checked}"
    );
}

#[test]
fn name_matches_plain_entries_exactly() {
    assert!(name_matches("Game.EXE", "game.exe"));
    assert!(name_matches("game.exe", "C:\\Games\\Game.exe"));
    assert!(!name_matches("game.exe", "game2.exe"));
    assert!(!name_matches("game", "game.exe"));
}

#[test]
fn name_matches_wildcard_entries() {
    assert!(name_matches(
        "FiveM*GTAProcess.exe",
        "FiveM_b2802_GTAProcess.exe"
    ));
    assert!(name_matches(
        "fivem*gtaprocess.exe",
        "FIVEM_B3258_GTAPROCESS.EXE"
    ));
    assert!(name_matches("game?.exe", "game1.exe"));
    assert!(!name_matches("game?.exe", "game12.exe"));
    assert!(!name_matches(
        "FiveM*GTAProcess.exe",
        "FiveM_b2802_GameProcess.exe"
    ));
}

#[test]
fn name_matches_ignores_empty_and_wildcard_only_entries() {
    assert!(!name_matches("", "game.exe"));
    assert!(!name_matches("*", "game.exe"));
    assert!(!name_matches("??", "game.exe"));
    assert!(!name_matches("*.exe", ""));
}

#[test]
fn lists_block_and_allow_use_wildcards() {
    let lists = Lists::new(&["MyLauncher*.exe".to_string()], &[]);
    assert!(lists.is_blocked("MyLauncher_2.exe"));
    assert!(!lists.is_blocked("Other.exe"));
}
