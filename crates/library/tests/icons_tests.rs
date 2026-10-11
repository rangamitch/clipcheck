//! Tester-authored tests for the game-icon disk cache (`clipcheck_library::icons`).
//! Black-box: only the public API and the documented on-disk layout are used.

use clipcheck_library::icons::{
    has_icon, icon_file_name, icon_path, icons_dir, read_icon, read_icons, write_icon,
    MAX_ICON_BYTES,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const SIG: [u8; 4] = [0x89, b'P', b'N', b'G'];

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "cc_icons_test_{}_{}_{}",
            tag,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Bytes that start with the PNG signature and are exactly `len` bytes long.
fn png_bytes(len: usize, seed: u8) -> Vec<u8> {
    assert!(len >= 8);
    let mut v = SIG.to_vec();
    v.extend_from_slice(&[0x0D, 0x0A, 0x1A, 0x0A]);
    let mut i: usize = 0;
    while v.len() < len {
        v.push(seed.wrapping_add(i as u8));
        i += 1;
    }
    v
}

fn list_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_single_component_inside(data_dir: &Path, id: &str) {
    let name = icon_file_name(id);
    assert!(!name.is_empty(), "file name empty for {id:?}");
    assert!(
        !name.contains('/') && !name.contains('\\'),
        "separator in {name:?}"
    );
    assert_ne!(name, ".");
    assert_ne!(name, "..");
    let p = icon_path(data_dir, id);
    assert_eq!(
        p.parent().unwrap(),
        icons_dir(data_dir),
        "icon path escaped icons dir for {id:?}: {p:?}"
    );
    assert_eq!(p.file_name().unwrap().to_string_lossy(), name);
}

#[test]
fn icons_dir_is_data_dir_slash_icons() {
    let d = Path::new("some/data");
    assert_eq!(icons_dir(d), d.join("icons"));
}

#[test]
fn file_name_lowercases_and_replaces_unsafe_chars() {
    assert_eq!(icon_file_name("Elden Ring"), "elden_ring.png");
    assert_eq!(icon_file_name("abc-123_x.y"), "abc-123_x.y.png");
}

#[test]
fn file_name_blank_is_underscore_png() {
    assert_eq!(icon_file_name(""), "_.png");
}

#[test]
fn path_separators_stay_inside_icons_dir() {
    let dir = TempDir::new("sep");
    for id in [
        "a/b",
        "a\\b",
        "/etc/passwd",
        "C:\\Windows\\x",
        "dir/sub/game",
    ] {
        assert_single_component_inside(dir.path(), id);
        let bytes = png_bytes(64, 7);
        write_icon(dir.path(), id, &bytes).unwrap_or_else(|e| panic!("write {id:?}: {e}"));
        assert_eq!(
            read_icon(dir.path(), id).as_deref(),
            Some(&bytes[..]),
            "id {id:?}"
        );
    }
    // Nothing may have been written outside the icons dir.
    assert_eq!(list_names(dir.path()), vec!["icons".to_string()]);
}

#[test]
fn dot_dot_ids_stay_inside_icons_dir() {
    let dir = TempDir::new("dotdot");
    for id in ["..", "../..", "../../escape", "."] {
        assert_single_component_inside(dir.path(), id);
        let bytes = png_bytes(32, 3);
        write_icon(dir.path(), id, &bytes).unwrap_or_else(|e| panic!("write {id:?}: {e}"));
        assert_eq!(read_icon(dir.path(), id).as_deref(), Some(&bytes[..]));
    }
    assert_eq!(list_names(dir.path()), vec!["icons".to_string()]);
}

#[test]
fn unicode_ids_map_to_ascii_file_names() {
    let dir = TempDir::new("unicode");
    for id in ["ゲーム", "Ünïcödé Game", "🎮 Arcade", "日本語.exe"] {
        let name = icon_file_name(id);
        assert!(name.is_ascii(), "non-ascii file name {name:?} for {id:?}");
        assert!(name.ends_with(".png"));
        assert_single_component_inside(dir.path(), id);
        let bytes = png_bytes(40, 9);
        write_icon(dir.path(), id, &bytes).unwrap_or_else(|e| panic!("write {id:?}: {e}"));
        assert_eq!(read_icon(dir.path(), id).as_deref(), Some(&bytes[..]));
    }
}

#[test]
fn file_name_is_deterministic() {
    assert_eq!(
        icon_file_name("Some Game: X"),
        icon_file_name("Some Game: X")
    );
}

#[test]
fn very_long_id_stays_single_component() {
    let dir = TempDir::new("longid");
    let id = "a/".repeat(5000) + &"Z".repeat(10_000);
    let name = icon_file_name(&id);
    assert!(!name.contains('/') && !name.contains('\\'));
    assert_single_component_inside(dir.path(), &id);
    // Writing may fail on a 255-byte-per-component filesystem, but if it succeeds the
    // round trip must be exact, and nothing may land outside the icons dir.
    let bytes = png_bytes(100, 1);
    match write_icon(dir.path(), &id, &bytes) {
        Ok(()) => assert_eq!(read_icon(dir.path(), &id).as_deref(), Some(&bytes[..])),
        Err(e) => eprintln!("NOTE: very long id write returned error (acceptable): {e}"),
    }
    assert_eq!(list_names(dir.path()), vec!["icons".to_string()]);
}

#[test]
fn has_icon_reflects_presence() {
    let dir = TempDir::new("has");
    assert!(!has_icon(dir.path(), "g1"));
    write_icon(dir.path(), "g1", &png_bytes(16, 2)).unwrap();
    assert!(has_icon(dir.path(), "g1"));
}

#[test]
fn write_then_read_roundtrip() {
    let dir = TempDir::new("roundtrip");
    let bytes = png_bytes(1234, 5);
    write_icon(dir.path(), "Game A", &bytes).unwrap();
    assert_eq!(read_icon(dir.path(), "Game A").as_deref(), Some(&bytes[..]));
    assert!(icon_path(dir.path(), "Game A").is_file());
}

#[test]
fn overwrite_replaces_content() {
    let dir = TempDir::new("overwrite");
    let first = png_bytes(500, 1);
    let second = png_bytes(20, 200);
    write_icon(dir.path(), "g", &first).unwrap();
    write_icon(dir.path(), "g", &second).unwrap();
    assert_eq!(read_icon(dir.path(), "g").as_deref(), Some(&second[..]));
    assert_eq!(fs::read(icon_path(dir.path(), "g")).unwrap(), second);
}

#[test]
fn no_leftover_tmp_file_after_write() {
    let dir = TempDir::new("tmpleft");
    write_icon(dir.path(), "g", &png_bytes(64, 4)).unwrap();
    write_icon(dir.path(), "g", &png_bytes(65, 4)).unwrap();
    let names = list_names(&icons_dir(dir.path()));
    assert_eq!(
        names,
        vec![icon_file_name("g")],
        "leftover files: {names:?}"
    );
}

#[test]
fn write_rejects_non_png_and_creates_nothing() {
    let dir = TempDir::new("badsig");
    assert!(
        write_icon(dir.path(), "bad1", b"").is_err(),
        "empty bytes accepted"
    );
    assert!(
        write_icon(dir.path(), "bad2", b"GIF89a....").is_err(),
        "GIF accepted"
    );
    assert!(
        write_icon(dir.path(), "bad3", &[0x89, b'P', b'N']).is_err(),
        "3-byte prefix accepted"
    );
    assert!(
        write_icon(dir.path(), "bad4", &[0x89, b'P', b'N', b'X', 1, 2, 3]).is_err(),
        "wrong 4th byte accepted"
    );
    for id in ["bad1", "bad2", "bad3", "bad4"] {
        assert!(
            !icon_path(dir.path(), id).exists(),
            "file created for rejected {id}"
        );
        assert_eq!(read_icon(dir.path(), id), None);
    }
}

#[test]
fn zero_byte_file_reads_none() {
    let dir = TempDir::new("zero");
    fs::create_dir_all(icons_dir(dir.path())).unwrap();
    fs::write(icon_path(dir.path(), "empty"), b"").unwrap();
    assert_eq!(read_icon(dir.path(), "empty"), None);
}

#[test]
fn absent_icon_reads_none() {
    let dir = TempDir::new("absent");
    assert_eq!(read_icon(dir.path(), "nope"), None);
    let fresh = dir.path().join("does_not_exist_yet");
    assert_eq!(read_icon(&fresh, "nope"), None);
}

#[test]
fn exact_limit_reads_some_and_one_over_reads_none() {
    assert_eq!(MAX_ICON_BYTES, 256 * 1024);
    let dir = TempDir::new("limit");
    fs::create_dir_all(icons_dir(dir.path())).unwrap();

    let exact = png_bytes(256 * 1024, 11);
    fs::write(icon_path(dir.path(), "exact"), &exact).unwrap();
    let got = read_icon(dir.path(), "exact").expect("exact-limit icon should read Some");
    assert_eq!(got.len(), 256 * 1024);
    assert_eq!(got, exact);

    let over = png_bytes(256 * 1024 + 1, 12);
    fs::write(icon_path(dir.path(), "over"), &over).unwrap();
    assert_eq!(
        read_icon(dir.path(), "over"),
        None,
        "limit+1 must read None"
    );
}

#[test]
fn read_icons_batch_omits_missing_and_oversize() {
    let dir = TempDir::new("batch");
    write_icon(dir.path(), "a", &png_bytes(30, 1)).unwrap();
    write_icon(dir.path(), "c", &png_bytes(40, 2)).unwrap();
    fs::create_dir_all(icons_dir(dir.path())).unwrap();
    fs::write(icon_path(dir.path(), "big"), png_bytes(256 * 1024 + 1, 3)).unwrap();
    fs::write(icon_path(dir.path(), "empty"), b"").unwrap();

    let ids: Vec<String> = ["a", "b_missing", "c", "big", "empty"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let map: HashMap<String, Vec<u8>> = read_icons(dir.path(), &ids);
    assert_eq!(map.len(), 2, "keys: {:?}", map.keys().collect::<Vec<_>>());
    assert_eq!(map.get("a"), Some(&png_bytes(30, 1)));
    assert_eq!(map.get("c"), Some(&png_bytes(40, 2)));
    assert!(!map.contains_key("b_missing"));
    assert!(!map.contains_key("big"));
    assert!(!map.contains_key("empty"));
}

#[test]
fn read_icons_empty_slice_is_empty_map() {
    let dir = TempDir::new("batchempty");
    assert!(read_icons(dir.path(), &[]).is_empty());
}

#[test]
fn concurrent_writes_same_id_never_corrupt() {
    let dir = TempDir::new("concurrent");
    let data = dir.path().to_path_buf();
    // Eight distinct payloads of different sizes, all with a valid PNG signature.
    let payloads: Vec<Vec<u8>> = (0..8u8)
        .map(|i| png_bytes(2000 + (i as usize) * 4096, i.wrapping_mul(31)))
        .collect();
    let payloads = Arc::new(payloads);
    let stop = Arc::new(AtomicBool::new(false));

    // Reader: every observed value must be exactly one of the payloads (never partial).
    let reader = {
        let data = data.clone();
        let payloads = payloads.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            let mut seen_some = 0usize;
            while !stop.load(Ordering::SeqCst) {
                if let Some(got) = read_icon(&data, "contested") {
                    seen_some += 1;
                    assert!(
                        payloads.contains(&got),
                        "reader saw a corrupt/partial icon of len {}",
                        got.len()
                    );
                }
            }
            seen_some
        })
    };

    let writers: Vec<_> = (0..8usize)
        .map(|t| {
            let data = data.clone();
            let payloads = payloads.clone();
            thread::spawn(move || {
                for _ in 0..40 {
                    write_icon(&data, "contested", &payloads[t]).expect("concurrent write failed");
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    reader.join().expect("reader saw corrupt data");

    let final_bytes = read_icon(&data, "contested").expect("icon missing after writes");
    assert!(
        payloads.contains(&final_bytes),
        "final icon is not a written payload"
    );
    assert_eq!(
        list_names(&icons_dir(&data)),
        vec![icon_file_name("contested")]
    );
}
