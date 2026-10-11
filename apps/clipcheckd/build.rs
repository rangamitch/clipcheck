//! Build script: embeds the ClipCheck icon, version information and
//! application manifest into `clipcheckd.exe`. Windows targets only.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(err) = embed_windows_resources() {
        panic!("failed to embed Windows resources into clipcheckd: {err}");
    }
}

/// Fills the rc template with absolute paths and the crate version, then
/// compiles it. Absolute paths avoid rc.exe resolving names against an
/// unpredictable working directory.
fn embed_windows_resources() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    let icon = manifest_dir
        .join("..")
        .join("..")
        .join("assets")
        .join("icons")
        .join("clipcheck.ico");
    let template = manifest_dir.join("resources").join("clipcheckd.rc");
    let manifest = manifest_dir.join("resources").join("clipcheckd.manifest");

    for path in [&icon, &template, &manifest] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let version = env::var("CARGO_PKG_VERSION")?;
    let (comma_version, dotted_version) = version_strings(&version);

    let rc = fs::read_to_string(&template)?
        .replace("@ICON@", &to_rc_path(&icon))
        .replace("@MANIFEST@", &to_rc_path(&manifest))
        .replace("@VERSION_COMMA@", &comma_version)
        .replace("@VERSION_STR@", &dotted_version);

    let generated = out_dir.join("clipcheckd.rc");
    fs::write(&generated, rc)?;

    // embed-resource panics if the resource compiler fails, so errors are not silent.
    embed_resource::compile(&generated, embed_resource::NONE);
    Ok(())
}

/// Converts a path to the forward-slash form that rc.exe accepts in quoted strings.
fn to_rc_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Returns `("a,b,c,0", "a.b.c.0")` for a semantic version such as `0.1.0`.
/// Pre-release or build suffixes are ignored; missing parts default to 0.
fn version_strings(version: &str) -> (String, String) {
    let mut parts: Vec<u32> = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    parts.resize(3, 0);
    (
        format!("{},{},{},0", parts[0], parts[1], parts[2]),
        format!("{}.{}.{}.0", parts[0], parts[1], parts[2]),
    )
}
