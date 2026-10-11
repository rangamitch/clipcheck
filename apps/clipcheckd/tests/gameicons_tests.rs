//! Tester-authored tests for `clipcheckd::gameicons::ensure_async` and
//! `clipcheckd::appinfo::icon_png`. Uses unique temp dirs and cleans them up.

use clipcheck_library::icons::{icon_path, icons_dir, read_icon, write_icon};
use clipcheckd::appinfo::icon_png;
use clipcheckd::gameicons::ensure_async;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "cc_gameicons_test_{}_{}_{}",
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

/// Parse the IHDR width and height (big-endian, bytes 16..24) of a PNG.
fn ihdr_dims(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || png[..8] != SIG || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Some((w, h))
}

/// A minimal valid-looking PNG-signature payload (not a real image, used for pre-existing icons).
fn fake_png(seed: u8) -> Vec<u8> {
    let mut v = SIG.to_vec();
    v.extend((0..64u8).map(|i| i.wrapping_add(seed)));
    v
}

#[test]
fn pid_zero_creates_no_icons_dir() {
    let tmp = TempDir::new("pid0");
    ensure_async(tmp.path(), "game_zero", 0);
    // Give any (wrongly) spawned background work time to act.
    thread::sleep(Duration::from_millis(300));
    assert!(
        !icons_dir(tmp.path()).exists(),
        "pid 0 must not create the icons dir"
    );
    assert_eq!(read_icon(tmp.path(), "game_zero"), None);
}

#[test]
fn existing_icon_is_left_untouched() {
    let tmp = TempDir::new("existing");
    let original = fake_png(42);
    write_icon(tmp.path(), "game_existing", &original).unwrap();

    // Our own pid is a real, running exe; if the implementation wrongly re-extracted,
    // the bytes would be replaced with a real 32x32 icon.
    ensure_async(tmp.path(), "game_existing", std::process::id());
    thread::sleep(Duration::from_millis(500));

    assert_eq!(
        read_icon(tmp.path(), "game_existing").as_deref(),
        Some(&original[..]),
        "existing icon bytes were modified"
    );
    assert_eq!(
        fs::read(icon_path(tmp.path(), "game_existing")).unwrap(),
        original
    );
}

#[test]
fn bogus_pid_writes_nothing() {
    let tmp = TempDir::new("bogus");
    let id = "game_bogus_pid";
    ensure_async(tmp.path(), id, 0xFFFF_FFF0);
    thread::sleep(Duration::from_millis(1000));
    assert!(
        !icon_path(tmp.path(), id).exists(),
        "an icon file was written for a non-existent pid"
    );
    assert_eq!(read_icon(tmp.path(), id), None);
}

#[test]
fn ensure_async_returns_immediately() {
    // Extraction is on a background thread; the call itself must not block for long.
    let tmp = TempDir::new("nonblock");
    let start = Instant::now();
    ensure_async(tmp.path(), "game_fast", std::process::id());
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "ensure_async blocked for {:?}",
        start.elapsed()
    );
}

#[test]
fn self_exe_icon_extracted_to_disk_as_32x32_png() {
    let tmp = TempDir::new("selfpid");
    let id = format!("game_self_{}", std::process::id());
    let exe = std::env::current_exe().expect("current_exe");

    // Direct extraction helper first: must return a 32x32 PNG for a real exe.
    let direct = icon_png(&exe);
    match &direct {
        Some(png) => {
            assert!(
                png.starts_with(&SIG),
                "icon_png did not return PNG signature"
            );
            assert_eq!(ihdr_dims(png), Some((32, 32)), "icon_png not 32x32");
        }
        None => eprintln!(
            "NOTE: icon_png(current_exe) returned None on this machine; \
             the test exe may have no extractable shell icon. Background path is checked below."
        ),
    }

    ensure_async(tmp.path(), &id, std::process::id());

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut written: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        if let Some(bytes) = read_icon(tmp.path(), &id) {
            written = Some(bytes);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    match written {
        Some(bytes) => {
            assert!(bytes.starts_with(&SIG), "written icon lacks PNG signature");
            assert_eq!(
                ihdr_dims(&bytes),
                Some((32, 32)),
                "written icon is not 32x32"
            );
            assert!(icon_path(tmp.path(), &id).is_file());
            // Icons dir should contain only the one icon, no leftover temp file.
            let names: Vec<String> = fs::read_dir(icons_dir(tmp.path()))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names.len(), 1, "unexpected files in icons dir: {names:?}");
        }
        None => {
            // Documented exception: a machine where the test exe has no extractable icon.
            eprintln!(
                "NOTE: no icon appeared within 5 s for the test exe ({}); \
                 this is accepted when icon_png(current_exe) is also None.",
                exe.display()
            );
            assert!(
                direct.is_none(),
                "icon_png(current_exe) returned an icon but ensure_async wrote nothing within 5 s"
            );
        }
    }
}

#[test]
fn icon_png_unreadable_path_is_none() {
    let tmp = TempDir::new("unreadable");
    let missing = tmp.path().join("no_such_program.exe");
    assert_eq!(icon_png(&missing), None);
}
