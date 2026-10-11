//! OS-backed tests for `clipcheckd::appinfo`: icon extraction, file descriptions, and GPU
//! enumeration. The pure helpers are covered by unit tests inside `appinfo.rs`.
//!
//! Windows only.

#![cfg(windows)]

mod common;

use std::path::Path;

use clipcheckd::appinfo::{enumerate_gpus, file_description, icon_png_b64};

use common::*;

#[test]
fn notepad_icon_is_32px_png() {
    let icon = icon_png_b64(Path::new(r"C:\Windows\System32\notepad.exe"))
        .expect("notepad.exe should have an icon");
    assert_png_32(&b64_decode(&icon));
}

#[test]
fn test_exe_icon_is_none_or_32px_png() {
    let exe = std::env::current_exe().expect("current_exe");
    match icon_png_b64(&exe) {
        Some(icon) => assert_png_32(&b64_decode(&icon)),
        None => println!("note: test exe {} has no extractable icon", exe.display()),
    }
}

#[test]
fn icon_lookup_is_cached_and_stable() {
    let p = Path::new(r"C:\Windows\System32\notepad.exe");
    let a = icon_png_b64(p);
    let b = icon_png_b64(p);
    assert!(a.is_some(), "notepad icon missing");
    assert_eq!(a, b, "second lookup differs from first");
}

#[test]
fn icon_of_missing_path_is_none() {
    let p = Path::new(r"C:\definitely\missing\clipcheck-nope.exe");
    assert_eq!(icon_png_b64(p), None);
}

#[test]
fn notepad_file_description_is_non_empty() {
    let desc = file_description(Path::new(r"C:\Windows\System32\notepad.exe"))
        .expect("notepad.exe should have a file description");
    assert!(!desc.trim().is_empty(), "file description is blank");
    println!("notepad description: {desc:?}");
}

#[test]
fn gpu_enumeration_has_names_and_no_basic_render_driver() {
    let gpus = enumerate_gpus();
    for g in &gpus {
        println!(
            "gpu index={} vendor_id={:#06x} dedicated_mb={} name={:?}",
            g.index, g.vendor_id, g.dedicated_mb, g.name
        );
        assert!(!g.name.trim().is_empty(), "GPU with empty name: {g:?}");
        assert!(
            !g.name.contains("Microsoft Basic Render Driver"),
            "software adapter must be filtered out: {}",
            g.name
        );
    }
    println!("gpu count: {}", gpus.len());
}
