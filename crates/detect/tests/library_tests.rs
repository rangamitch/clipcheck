//! Integration tests for `clipcheck_detect::library` and `known_games`.
//!
//! Parsers are exercised with in-memory fixtures. `scan_system` only reads
//! the local machine. Nothing here launches a game or external process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use clipcheck_detect::known_games;
use clipcheck_detect::library::*;
use clipcheck_detect::lists;

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn entry(store: Store, id: &str, name: &str, dir: &str) -> LibEntry {
    LibEntry {
        store,
        id: id.to_string(),
        name: name.to_string(),
        install_dir: PathBuf::from(dir),
    }
}

// ---------------------------------------------------------------------------
// 1. libraryfolders.vdf, current format
// ---------------------------------------------------------------------------

#[test]
fn vdf_current_format_three_libraries() {
    let vdf = concat!(
        "\u{feff}\"libraryfolders\"\r\n",
        "{\r\n",
        "\t// managed by Steam\r\n",
        "\t\"0\"\t\t{\r\n",
        "\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\r\n",
        "\t\t\"label\"\t\t\"\"\r\n",
        "\t\t\"contentid\"\t\t\"1234567890123456789\"\r\n",
        "\t\t\"totalsize\"\t\t\"0\"\r\n",
        "\t\t\"apps\"\r\n",
        "\t\t{\r\n",
        "\t\t\t\"228980\"\t\t\"123\"\r\n",
        "\t\t\t\"105600\"\t\t\"4567\"\r\n",
        "\t\t}\r\n",
        "\t}\r\n",
        "\t\"1\"\t\t{\r\n",
        "\t\t\"path\"\t\t\"F:\\\\SteamLibrary\"\r\n",
        "\t\t\"contentid\"\t\t\"987654321\"\r\n",
        "\t\t\"apps\"\r\n",
        "\t\t{\r\n",
        "\t\t\t\"730\"\t\t\"55555\"\r\n",
        "\t\t}\r\n",
        "\t}\r\n",
        "\t// a second comment\r\n",
        "\t\"2\"\t\t{\r\n",
        "\t\t\"path\"\t\t\"D:\\\\Games\\\\Steam Lib\"\r\n",
        "\t\t\"apps\"\r\n",
        "\t\t{\r\n",
        "\t\t}\r\n",
        "\t}\r\n",
        "}\r\n",
    );

    let paths = parse_library_folders_vdf(vdf);
    let got: Vec<String> = paths.iter().map(|p| path_str(p)).collect();
    assert_eq!(
        got,
        vec![
            r"C:\Program Files (x86)\Steam\steamapps".to_string(),
            r"F:\SteamLibrary\steamapps".to_string(),
            r"D:\Games\Steam Lib\steamapps".to_string(),
        ],
        "expected exactly 3 steamapps paths in file order with unescaped backslashes"
    );
    assert_eq!(paths.len(), 3);
    for p in &paths {
        assert!(
            path_str(p).ends_with("steamapps"),
            "path should end in steamapps: {}",
            path_str(p)
        );
    }
}

#[test]
fn vdf_current_format_unescapes_double_backslash_exactly() {
    let vdf = r#""libraryfolders" { "0" { "path" "F:\\SteamLibrary" } }"#;
    let paths = parse_library_folders_vdf(vdf);
    assert_eq!(paths.len(), 1);
    assert_eq!(path_str(&paths[0]), r"F:\SteamLibrary\steamapps");
}

// ---------------------------------------------------------------------------
// 2. libraryfolders.vdf, legacy format and de-duplication
// ---------------------------------------------------------------------------

#[test]
fn vdf_legacy_format_is_parsed() {
    let vdf = r#""LibraryFolders" { "TimeNextStatsReport" "1" "1" "E:\\SteamLib" }"#;
    let paths = parse_library_folders_vdf(vdf);
    assert_eq!(paths.len(), 1, "legacy value should yield one library");
    assert_eq!(path_str(&paths[0]), r"E:\SteamLib\steamapps");
}

#[test]
fn vdf_duplicate_library_differing_by_case_is_deduplicated() {
    let vdf = r#""LibraryFolders"
{
    "TimeNextStatsReport" "1"
    "1" "E:\\SteamLib"
    "2" "e:\\steamlib"
    "3" "E:\\SteamLib\\"
}"#;
    let paths = parse_library_folders_vdf(vdf);
    assert_eq!(
        paths.len(),
        1,
        "case-only and trailing-separator duplicates must collapse, got {:?}",
        paths
    );
    assert_eq!(path_str(&paths[0]), r"E:\SteamLib\steamapps");
}

#[test]
fn vdf_mixed_legacy_and_current_entries_dedupe_across_formats() {
    let vdf = r#""libraryfolders"
{
    "0" { "path" "C:\\Steam" }
    "1" "C:\\steam"
    "2" "G:\\Other"
}"#;
    let paths = parse_library_folders_vdf(vdf);
    let got: Vec<String> = paths.iter().map(|p| path_str(p)).collect();
    assert_eq!(
        got,
        vec![
            r"C:\Steam\steamapps".to_string(),
            r"G:\Other\steamapps".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// 3. libraryfolders.vdf, BOM and malformed input
// ---------------------------------------------------------------------------

#[test]
fn vdf_bom_prefix_is_tolerated() {
    let vdf = "\u{feff}\"libraryfolders\" { \"0\" { \"path\" \"F:\\\\SteamLibrary\" } }";
    let paths = parse_library_folders_vdf(vdf);
    assert_eq!(paths.len(), 1);
    assert_eq!(path_str(&paths[0]), r"F:\SteamLibrary\steamapps");
}

#[test]
fn vdf_empty_input_returns_empty() {
    assert!(parse_library_folders_vdf("").is_empty());
}

#[test]
fn vdf_garbage_input_returns_empty_without_panic() {
    assert!(parse_library_folders_vdf("garbage").is_empty());
    assert!(parse_library_folders_vdf("}}}{{{ \"unterminated").is_empty());
    assert!(parse_library_folders_vdf("\u{feff}").is_empty());
}

// ---------------------------------------------------------------------------
// 4. appmanifest_*.acf
// ---------------------------------------------------------------------------

#[test]
fn acf_real_world_shape_with_bom_tabs_and_odd_order() {
    // installdir appears before name, tabs as separators, BOM at the start.
    let acf = "\u{feff}\"AppState\"\n{\n\t\"appid\"\t\t\"105600\"\n\t\"installdir\"\t\t\"Terraria\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"name\"\t\t\"Terraria\"\n\t\"LastUpdated\"\t\t\"1700000000\"\n\t\"SizeOnDisk\"\t\t\"1234\"\n}\n";
    let got = parse_appmanifest_acf(acf);
    assert_eq!(
        got,
        Some((
            "105600".to_string(),
            "Terraria".to_string(),
            "Terraria".to_string()
        ))
    );
}

#[test]
fn acf_name_before_installdir_and_different_name() {
    let acf = "\"AppState\"\r\n{\r\n\t\"name\"\t\"Some Display Name\"\r\n\t\"installdir\"\t\"SomeDir\"\r\n\t\"appid\"\t\"42\"\r\n}\r\n";
    assert_eq!(
        parse_appmanifest_acf(acf),
        Some((
            "42".to_string(),
            "Some Display Name".to_string(),
            "SomeDir".to_string()
        ))
    );
}

#[test]
fn acf_missing_installdir_is_none() {
    let acf = "\"AppState\" { \"appid\" \"105600\" \"name\" \"Terraria\" }";
    assert_eq!(parse_appmanifest_acf(acf), None);
}

#[test]
fn acf_empty_appid_is_none() {
    let acf = "\"AppState\" { \"appid\" \"\" \"installdir\" \"Terraria\" \"name\" \"Terraria\" }";
    assert_eq!(parse_appmanifest_acf(acf), None);
}

#[test]
fn acf_missing_name_falls_back_to_installdir() {
    let acf = "\"AppState\" { \"appid\" \"105600\" \"installdir\" \"Terraria\" }";
    let got = parse_appmanifest_acf(acf).expect("should parse without name");
    assert_eq!(got.0, "105600");
    assert_eq!(got.1, "Terraria", "name must equal installdir when missing");
    assert_eq!(got.2, "Terraria");
}

#[test]
fn acf_empty_name_falls_back_to_installdir() {
    let acf = "\"AppState\" { \"appid\" \"7\" \"installdir\" \"Dir Name\" \"name\" \"\" }";
    let got = parse_appmanifest_acf(acf).expect("should parse");
    assert_eq!(got.1, "Dir Name");
}

#[test]
fn acf_garbage_is_none() {
    assert_eq!(parse_appmanifest_acf(""), None);
    assert_eq!(parse_appmanifest_acf("nonsense {{{"), None);
}

// ---------------------------------------------------------------------------
// 5. Epic .item JSON
// ---------------------------------------------------------------------------

#[test]
fn epic_valid_item_returns_tuple() {
    let json = r#"{
        "AppName": "Fortnite",
        "DisplayName": "Fortnite",
        "InstallLocation": "D:\\Epic Games\\Fortnite",
        "bIsIncompleteInstall": false
    }"#;
    assert_eq!(
        parse_epic_item_json(json),
        Some((
            "Fortnite".to_string(),
            "Fortnite".to_string(),
            r"D:\Epic Games\Fortnite".to_string()
        ))
    );
}

#[test]
fn epic_display_name_differs_from_app_name() {
    let json = r#"{"AppName":"Chameleon","DisplayName":"Chameleon Pretty","InstallLocation":"E:\\Games\\Cham"}"#;
    let got = parse_epic_item_json(json).expect("should parse");
    assert_eq!(got.0, "Chameleon");
    assert_eq!(got.1, "Chameleon Pretty");
    assert_eq!(got.2, r"E:\Games\Cham");
}

#[test]
fn epic_incomplete_install_is_none() {
    let json = r#"{"AppName":"X","DisplayName":"X","InstallLocation":"C:\\X","bIsIncompleteInstall":true}"#;
    assert_eq!(parse_epic_item_json(json), None);
}

#[test]
fn epic_invalid_json_is_none() {
    assert_eq!(parse_epic_item_json("{ not json"), None);
    assert_eq!(parse_epic_item_json(""), None);
}

#[test]
fn epic_bom_is_tolerated() {
    let json = "\u{feff}{\"AppName\":\"Rocket\",\"DisplayName\":\"Rocket League\",\"InstallLocation\":\"C:\\\\Games\\\\Rocket\"}";
    assert_eq!(
        parse_epic_item_json(json),
        Some((
            "Rocket".to_string(),
            "Rocket League".to_string(),
            r"C:\Games\Rocket".to_string()
        ))
    );
}

#[test]
fn epic_missing_display_name_uses_app_name() {
    let json = r#"{"AppName":"Lonely","InstallLocation":"C:\\Games\\Lonely"}"#;
    let got = parse_epic_item_json(json).expect("should parse");
    assert_eq!(got.1, "Lonely", "DisplayName fallback must be AppName");
}

#[test]
fn epic_missing_install_location_is_none() {
    let json = r#"{"AppName":"NoLoc","DisplayName":"NoLoc"}"#;
    assert_eq!(parse_epic_item_json(json), None);
}

// ---------------------------------------------------------------------------
// 6. GOG registry values
// ---------------------------------------------------------------------------

#[test]
fn gog_valid_values_build_entry() {
    let got = parse_gog_registry_values(
        Some("1207658"),
        Some("Some Game"),
        Some(r"F:\GOG Games\Some Game"),
    );
    assert_eq!(
        got,
        Some(LibEntry {
            store: Store::Gog,
            id: "1207658".to_string(),
            name: "Some Game".to_string(),
            install_dir: PathBuf::from(r"F:\GOG Games\Some Game"),
        })
    );
}

#[test]
fn gog_missing_path_is_none() {
    assert_eq!(
        parse_gog_registry_values(Some("1207658"), Some("Some Game"), None),
        None
    );
    assert_eq!(
        parse_gog_registry_values(Some("1207658"), Some("Some Game"), Some("   ")),
        None
    );
}

#[test]
fn gog_missing_id_is_none() {
    assert_eq!(
        parse_gog_registry_values(None, Some("Some Game"), Some(r"F:\GOG Games\Some Game")),
        None
    );
}

#[test]
fn gog_missing_name_falls_back_to_id() {
    let got = parse_gog_registry_values(Some("1207658"), None, Some(r"F:\GOG Games\X"))
        .expect("should parse without name");
    assert_eq!(got.store, Store::Gog);
    assert_eq!(got.name, "1207658", "name must equal id when missing");
    assert_eq!(got.id, "1207658");
}

// ---------------------------------------------------------------------------
// 7. Xbox MicrosoftGame.config
// ---------------------------------------------------------------------------

#[test]
fn xbox_bom_identity_and_display_name() {
    let xml = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Game configVersion=\"1\">\n  <Identity Name=\"Microsoft.Foo\" Publisher=\"CN=X\" Version=\"1.0.0.0\"/>\n  <ShellVisuals DefaultDisplayName=\"Foo Game\" StoreLogo=\"x.png\"/>\n</Game>";
    assert_eq!(
        parse_xbox_config(xml),
        Some(("Microsoft.Foo".to_string(), Some("Foo Game".to_string())))
    );
}

#[test]
fn xbox_without_shell_visuals_has_no_display() {
    let xml = r#"<Game><Identity Name="Microsoft.Foo" Publisher="CN=X"/></Game>"#;
    assert_eq!(
        parse_xbox_config(xml),
        Some(("Microsoft.Foo".to_string(), None))
    );
}

#[test]
fn xbox_without_identity_is_none() {
    let xml = r#"<Game><ShellVisuals DefaultDisplayName="Foo Game"/></Game>"#;
    assert_eq!(parse_xbox_config(xml), None);
    assert_eq!(parse_xbox_config(""), None);
}

#[test]
fn xbox_publisher_name_attribute_is_not_confused_with_name() {
    // PublisherName comes before Name; the parser must pick Name only.
    let xml =
        r#"<Game><Identity PublisherName="wrong" Name="Microsoft.Right" Publisher="CN=X"/></Game>"#;
    assert_eq!(
        parse_xbox_config(xml),
        Some(("Microsoft.Right".to_string(), None))
    );
}

#[test]
fn xbox_publisher_name_only_yields_none() {
    // Only PublisherName is present, so there is no Identity Name.
    let xml = r#"<Game><Identity PublisherName="x" Publisher="CN=X"/></Game>"#;
    assert_eq!(parse_xbox_config(xml), None);
}

// ---------------------------------------------------------------------------
// 8. LibraryScan::find_for_exe boundaries
// ---------------------------------------------------------------------------

fn foo_scan() -> LibraryScan {
    LibraryScan {
        entries: vec![entry(Store::Steam, "1", "Foo", r"C:\games\foo")],
    }
}

#[test]
fn find_matches_exe_inside_dir() {
    let scan = foo_scan();
    let hit = scan.find_for_exe(Path::new(r"C:\games\foo\x.exe"));
    assert_eq!(hit.map(|e| e.id.as_str()), Some("1"));
}

#[test]
fn find_rejects_sibling_with_shared_prefix() {
    let scan = foo_scan();
    assert!(
        scan.find_for_exe(Path::new(r"C:\games\foobar\x.exe"))
            .is_none(),
        "foobar must not match foo"
    );
}

#[test]
fn find_rejects_install_dir_itself() {
    let scan = foo_scan();
    assert!(scan.find_for_exe(Path::new(r"C:\games\foo")).is_none());
}

#[test]
fn find_is_case_insensitive() {
    let scan = foo_scan();
    assert!(scan
        .find_for_exe(Path::new(r"c:\GAMES\FOO\Bin\X.EXE"))
        .is_some());
}

#[test]
fn find_accepts_forward_slashes() {
    let scan = foo_scan();
    assert!(scan.find_for_exe(Path::new("C:/games/foo/x.exe")).is_some());
}

#[test]
fn find_strips_verbatim_prefix() {
    let scan = foo_scan();
    assert!(scan
        .find_for_exe(Path::new(r"\\?\C:\games\foo\x.exe"))
        .is_some());
}

#[test]
fn find_ignores_trailing_separator_on_install_dir() {
    let backslash = LibraryScan {
        entries: vec![entry(Store::Steam, "b", "B", r"C:\games\foo\")],
    };
    assert!(backslash
        .find_for_exe(Path::new(r"C:\games\foo\x.exe"))
        .is_some());

    let slash = LibraryScan {
        entries: vec![entry(Store::Steam, "s", "S", "C:/games/foo/")],
    };
    assert!(slash
        .find_for_exe(Path::new(r"C:\games\foo\x.exe"))
        .is_some());
}

#[test]
fn find_prefers_most_specific_nested_entry() {
    let scan = LibraryScan {
        entries: vec![
            entry(Store::Steam, "outer", "Games", r"C:\games"),
            entry(Store::Steam, "inner", "Foo", r"C:\games\foo"),
        ],
    };
    let hit = scan
        .find_for_exe(Path::new(r"C:\games\foo\bin\x.exe"))
        .expect("should match");
    assert_eq!(hit.id, "inner", "longest install_dir must win");

    // Under games but not under foo still resolves to the outer entry.
    let outer = scan
        .find_for_exe(Path::new(r"C:\games\other\y.exe"))
        .expect("should match outer");
    assert_eq!(outer.id, "outer");
}

#[test]
fn find_returns_none_when_no_entry_contains_exe() {
    let scan = foo_scan();
    assert!(scan.find_for_exe(Path::new(r"D:\other\x.exe")).is_none());
}

#[test]
fn find_on_empty_scan_is_none() {
    let scan = LibraryScan::default();
    assert!(scan
        .find_for_exe(Path::new(r"C:\games\foo\x.exe"))
        .is_none());
}

// ---------------------------------------------------------------------------
// 9. normalize_path
// ---------------------------------------------------------------------------

#[test]
fn normalize_strips_verbatim_prefix_and_trailing_sep() {
    assert_eq!(normalize_path(r"\\?\F:\A\B\"), r"f:\a\b");
}

#[test]
fn normalize_converts_forward_slashes() {
    assert_eq!(normalize_path("F:/A/B/"), r"f:\a\b");
}

#[test]
fn normalize_unc_verbatim_prefix() {
    assert_eq!(normalize_path(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
}

// ---------------------------------------------------------------------------
// 10. LibraryCache timing
// ---------------------------------------------------------------------------

fn counting_cache() -> (LibraryCache, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&counter);
    let cache = LibraryCache::with_scanner(move || {
        c.fetch_add(1, Ordering::SeqCst);
        LibraryScan {
            entries: vec![entry(Store::Gog, "7", "Seven", r"C:\games\seven")],
        }
    });
    (cache, counter)
}

#[test]
fn cache_rescans_on_schedule() {
    let (mut cache, counter) = counting_cache();
    let n = || counter.load(Ordering::SeqCst);

    cache.get(0);
    assert_eq!(n(), 1, "first get must scan");

    cache.get(59_999);
    assert_eq!(n(), 1, "59_999 ms is inside the interval");

    cache.get(60_000);
    assert_eq!(n(), 2, "60_000 ms reaches the interval");

    cache.get(60_001);
    assert_eq!(n(), 2, "60_001 ms is inside the new interval");

    cache.get(120_000);
    assert_eq!(n(), 3, "120_000 ms reaches the next interval");
}

#[test]
fn cache_rescans_when_clock_goes_backwards() {
    let (mut cache, counter) = counting_cache();
    cache.get(500_000);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    cache.get(10);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "a backwards clock must trigger a rescan"
    );
}

#[test]
fn cache_returns_scanner_entries() {
    let (mut cache, _counter) = counting_cache();
    let scan = cache.get(0);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(
        scan.entries[0],
        entry(Store::Gog, "7", "Seven", r"C:\games\seven")
    );
}

#[test]
fn rescan_interval_constant_is_60_seconds() {
    assert_eq!(RESCAN_INTERVAL_MS, 60_000);
}

// ---------------------------------------------------------------------------
// 11. scan_system (read-only, machine dependent)
// ---------------------------------------------------------------------------

#[test]
fn scan_system_does_not_panic_and_only_returns_existing_dirs() {
    let scan = scan_system();
    for e in &scan.entries {
        assert!(
            e.install_dir.is_dir(),
            "scan_system returned a missing dir: {} ({:?})",
            path_str(&e.install_dir),
            e.store
        );
    }
}

#[test]
fn scan_system_finds_steam_when_installed() {
    let steam_default = Path::new(r"C:\Program Files (x86)\Steam\steamapps").is_dir();
    let steam_on_any_drive =
        ('C'..='Z').any(|d| Path::new(&format!(r"{d}:\SteamLibrary\steamapps")).is_dir());
    let steam_present = steam_default || steam_on_any_drive;

    let scan = scan_system();
    let steam_entries: Vec<&LibEntry> = scan
        .entries
        .iter()
        .filter(|e| e.store == Store::Steam)
        .collect();
    println!(
        "scan_system: {} total entries, {} Steam entries (steam dirs present: {})",
        scan.entries.len(),
        steam_entries.len(),
        steam_present
    );

    if !steam_present {
        println!("no Steam install detected on this machine; skipping Steam assertion");
        return;
    }

    assert!(
        steam_entries.iter().any(|e| e.install_dir.is_dir()),
        "a Steam install exists but scan_system returned no existing Steam entry"
    );
}

// ---------------------------------------------------------------------------
// 12. known_games
// ---------------------------------------------------------------------------

#[test]
fn known_games_entries_are_well_formed() {
    let all = known_games::entries();
    assert!(!all.is_empty(), "entries() must not be empty");
    for g in &all {
        assert!(!g.exe.is_empty(), "empty exe in entry {:?}", g);
        assert!(
            g.exe.to_ascii_lowercase().ends_with(".exe"),
            "exe does not end with .exe: {:?}",
            g.exe
        );
        assert!(!g.name.is_empty(), "empty name for exe {:?}", g.exe);
        assert_eq!(
            g.name.trim(),
            g.name,
            "name has surrounding whitespace for exe {:?}",
            g.exe
        );
    }
}

#[test]
fn known_games_has_no_duplicate_exes() {
    let all = known_games::entries();
    let mut seen: std::collections::HashMap<String, Vec<String>> = Default::default();
    for g in &all {
        seen.entry(g.exe.to_ascii_lowercase())
            .or_default()
            .push(g.name.clone());
    }
    let mut dups: Vec<(String, Vec<String>)> = seen
        .into_iter()
        .filter(|(_, names)| names.len() > 1)
        .collect();
    dups.sort();
    for (exe, names) in &dups {
        println!("DUPLICATE exe {exe}: names {names:?}");
    }
    assert!(
        dups.is_empty(),
        "{} duplicate exe names found: {:?}",
        dups.len(),
        dups
    );
}

#[test]
fn known_games_does_not_overlap_blocklist_shell_or_generic_runtimes() {
    let generic = [
        "javaw.exe",
        "java.exe",
        "python.exe",
        "pythonw.exe",
        "node.exe",
        "electron.exe",
        "chrome.exe",
        "msedge.exe",
        "firefox.exe",
        "discord.exe",
        "steam.exe",
        "steamwebhelper.exe",
        "spotify.exe",
        "obs64.exe",
        "game.exe",
        "launcher.exe",
        "explorer.exe",
    ];
    let blocked: Vec<String> = lists::BUILTIN_BLOCKLIST
        .iter()
        .chain(lists::BUILTIN_SHELL.iter())
        .map(|s| s.to_ascii_lowercase())
        .collect();

    let mut violations = Vec::new();
    for g in known_games::entries() {
        let exe = g.exe.to_ascii_lowercase();
        if blocked.contains(&exe) {
            violations.push(format!("{} is in blocklist/shell", g.exe));
        }
        if generic.contains(&exe.as_str()) {
            violations.push(format!("{} is a generic runtime/app", g.exe));
        }
    }
    assert!(violations.is_empty(), "violations: {violations:?}");
}

#[test]
fn known_games_count_is_at_least_300() {
    let n = known_games::count();
    println!("known_games::count() = {n}");
    assert!(n >= 300, "only {n} distinct games");
}

#[test]
fn known_games_lookups() {
    assert_eq!(known_games::is_known("Terraria.exe"), Some("Terraria"));
    assert_eq!(known_games::is_known("TERRARIA.EXE"), Some("Terraria"));
    assert_eq!(known_games::is_known("  Terraria.exe "), Some("Terraria"));
    assert_eq!(
        known_games::is_known("MySingingMonsters.exe"),
        Some("My Singing Monsters")
    );
    assert_eq!(
        known_games::is_known("Stardew Valley.exe"),
        Some("Stardew Valley")
    );
    assert!(known_games::is_known("cs2.exe").is_some());
    assert!(known_games::is_known("VALORANT-Win64-Shipping.exe").is_some());
    assert_eq!(known_games::is_known("notagame_xyz.exe"), None);
    assert_eq!(known_games::is_known(""), None);
}
