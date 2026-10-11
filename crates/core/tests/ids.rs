use clipcheck_core::ids::{
    exe_file_name_lower, normalize_exe, unix_ms_now, ClipId, GameId, SessionId,
};
use std::str::FromStr;
use ulid::Ulid;

#[test]
fn new_ids_are_distinct() {
    let a = SessionId::new();
    let b = SessionId::new();
    assert_ne!(a, b);

    let c = ClipId::new();
    let d = ClipId::new();
    assert_ne!(c, d);
}

#[test]
fn default_produces_fresh_id() {
    assert_ne!(SessionId::default(), SessionId::default());
    assert_ne!(ClipId::default(), ClipId::default());
}

#[test]
fn from_ulid_and_ulid_round_trip() {
    let u = Ulid::new();
    let s = SessionId::from_ulid(u);
    assert_eq!(s.ulid(), u);
    let c = ClipId::from_ulid(u);
    assert_eq!(c.ulid(), u);
}

#[test]
fn ordering_matches_ulid_ordering() {
    let lo = Ulid::from_parts(1_000, 1);
    let hi = Ulid::from_parts(2_000, 1);
    assert!(SessionId::from_ulid(lo) < SessionId::from_ulid(hi));
    assert!(ClipId::from_ulid(lo) < ClipId::from_ulid(hi));
    assert_eq!(
        SessionId::from_ulid(lo).cmp(&SessionId::from_ulid(hi)),
        lo.cmp(&hi)
    );
}

#[test]
fn ids_are_copy_and_hashable() {
    use std::collections::HashSet;
    let s = SessionId::new();
    let copy = s;
    assert_eq!(s, copy);
    let mut set = HashSet::new();
    set.insert(s);
    set.insert(copy);
    assert_eq!(set.len(), 1);
}

#[test]
fn display_is_26_char_ulid_and_round_trips() {
    let s = SessionId::new();
    let text = s.to_string();
    assert_eq!(text.len(), 26);
    assert_eq!(text, s.ulid().to_string());
    assert_eq!(SessionId::from_str(&text).unwrap(), s);

    let c = ClipId::new();
    let text = c.to_string();
    assert_eq!(text.len(), 26);
    assert_eq!(ClipId::from_str(&text).unwrap(), c);
}

#[test]
fn from_str_rejects_invalid_input() {
    assert!(SessionId::from_str("not-a-ulid").is_err());
    assert!(SessionId::from_str("").is_err());
    assert!(ClipId::from_str("not-a-ulid").is_err());
    assert!(ClipId::from_str("").is_err());
}

#[test]
fn serde_session_id_is_plain_string() {
    let s = SessionId::new();
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, format!("\"{}\"", s));
    let back: SessionId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);
}

#[test]
fn serde_clip_id_is_plain_string() {
    let c = ClipId::new();
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(json, format!("\"{}\"", c));
    let back: ClipId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c);
}

#[test]
fn serde_rejects_invalid_id_string() {
    assert!(serde_json::from_str::<SessionId>("\"not-a-ulid\"").is_err());
    assert!(serde_json::from_str::<ClipId>("\"\"").is_err());
    assert!(serde_json::from_str::<ClipId>("12345").is_err());
}

#[test]
fn from_exe_normalizes_names() {
    assert_eq!(GameId::from_exe("Hades.exe").as_str(), "hades");
    assert_eq!(GameId::from_exe("HADES.EXE").as_str(), "hades");
    assert_eq!(GameId::from_exe("C:\\Games\\Hades.exe").as_str(), "hades");
    assert_eq!(GameId::from_exe("/usr/games/Hades.exe").as_str(), "hades");
    assert_eq!(GameId::from_exe("hades").as_str(), "hades");
    assert_eq!(GameId::from_exe("Game.v2.exe").as_str(), "game.v2");
    assert_eq!(GameId::from_exe("  Hades.exe ").as_str(), "hades");
}

#[test]
fn from_exe_has_no_store() {
    let g = GameId::from_exe("hades.exe");
    assert_eq!(g.store(), None);
    assert_eq!(g.exe(), "hades");
    assert_eq!(g.to_string(), "hades");
}

#[test]
fn with_store_builds_full_id() {
    let g = GameId::with_store("Hades.exe", "Steam", "1145360");
    assert_eq!(g.as_str(), "hades@steam:1145360");
    assert_eq!(g.to_string(), "hades@steam:1145360");
    assert_eq!(g.exe(), "hades");
    assert_eq!(g.store(), Some(("steam", "1145360")));
}

#[test]
fn with_store_equals_from_exe_plus_store_parts() {
    let a = GameId::with_store("C:\\Games\\HADES.EXE", "STEAM", " 1145360 ");
    assert_eq!(a.as_str(), "hades@steam:1145360");
}

#[test]
fn game_id_serde_is_transparent() {
    let g = GameId::with_store("Hades.exe", "Steam", "1145360");
    let json = serde_json::to_string(&g).unwrap();
    assert_eq!(json, "\"hades@steam:1145360\"");
    let back: GameId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, g);

    let plain = GameId::from_exe("hades.exe");
    let json = serde_json::to_string(&plain).unwrap();
    assert_eq!(json, "\"hades\"");
    let back: GameId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, plain);
}

#[test]
fn game_id_ordering_and_hash() {
    use std::collections::HashSet;
    let a = GameId::from_exe("a.exe");
    let b = GameId::from_exe("b.exe");
    assert!(a < b);
    let mut set = HashSet::new();
    set.insert(a.clone());
    set.insert(GameId::from_exe("A.EXE"));
    assert_eq!(set.len(), 1);
}

#[test]
fn unix_ms_now_is_plausible() {
    let now = unix_ms_now();
    assert!(now > 1_700_000_000_000, "got {now}");

    let expected = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(
        (expected - now).abs() < 5_000,
        "expected ~{expected}, got {now}"
    );
}

#[test]
fn exe_file_name_lower_strips_dirs_with_both_separators() {
    assert_eq!(exe_file_name_lower("C:\\Games\\Foo.EXE"), "foo.exe");
    assert_eq!(exe_file_name_lower("/usr/games/Foo.EXE"), "foo.exe");
    assert_eq!(exe_file_name_lower("C:/Games\\Sub/Foo.exe"), "foo.exe");
}

#[test]
fn exe_file_name_lower_trims_and_lowercases() {
    assert_eq!(exe_file_name_lower("  Hades.exe  "), "hades.exe");
    assert_eq!(exe_file_name_lower("C:\\Games\\ Hades.exe "), "hades.exe");
    assert_eq!(exe_file_name_lower("HADES"), "hades");
}

#[test]
fn exe_file_name_lower_keeps_extension_and_handles_no_extension() {
    assert_eq!(exe_file_name_lower("Game"), "game");
    assert_eq!(exe_file_name_lower("Game.v2"), "game.v2");
    assert_eq!(exe_file_name_lower(".exe"), ".exe");
}

#[test]
fn exe_file_name_lower_empty_and_trailing_dir() {
    assert_eq!(exe_file_name_lower(""), "");
    assert_eq!(exe_file_name_lower("C:\\Games\\"), "");
    assert_eq!(exe_file_name_lower("/usr/games/"), "");
}

#[test]
fn exe_file_name_lower_non_ascii_lowercase() {
    assert_eq!(exe_file_name_lower("É.EXE"), "é.exe");
}

#[test]
fn normalize_exe_strips_one_trailing_exe() {
    assert_eq!(normalize_exe("C:\\Games\\Foo.EXE"), "foo");
    assert_eq!(normalize_exe("Foo.exe"), "foo");
    assert_eq!(normalize_exe("Foo"), "foo");
    assert_eq!(normalize_exe("Foo.exe.exe"), "foo.exe");
    assert_eq!(normalize_exe(".exe"), "");
    assert_eq!(normalize_exe("exe"), "exe");
    assert_eq!(normalize_exe(""), "");
    assert_eq!(normalize_exe("C:\\Games\\"), "");
    assert_eq!(normalize_exe("É.EXE"), "é");
}

#[test]
fn normalize_exe_is_exe_file_name_lower_minus_exe_suffix() {
    let samples = [
        "C:\\Games\\Foo.EXE",
        "/usr/games/Hades.exe",
        "  Hades.exe ",
        "Hades",
        "Game.v2.exe",
        "Game.v2",
        "Foo.exe.exe",
        ".exe",
        "exe",
        "",
        "C:\\Games\\",
        "É.EXE",
        "C:/a\\B/c.Exe",
    ];
    for s in samples {
        let lower = exe_file_name_lower(s);
        let expected = match lower.strip_suffix(".exe") {
            Some(stem) => stem.to_owned(),
            None => lower.clone(),
        };
        assert_eq!(normalize_exe(s), expected, "sample {s:?}");
    }
}
