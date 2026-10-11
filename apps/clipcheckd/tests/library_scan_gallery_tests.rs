//! Regression: video files that already sit in the configured storage roots must be found by
//! the library service and listed, and the UI must be told (`library_changed`) when they are.
//!
//! The test runs the library service in-process on a temp data dir and temp roots with small
//! real files made by `clipcheck-testkit` (native H.264/AAC). It never starts a
//! daemon, a game or the user's app. If native test media is unavailable the test prints a note
//! and returns early.

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::{LibraryDir, Storage};
use clipcheck_ipc::proto::{Event, Topic};
use clipcheck_library::{Library, ListQuery};
use clipcheck_testkit::MediaSpec;
use clipcheckd::events::EventBus;
use clipcheckd::library_service::{LibraryOptions, LibraryService};

/// Writes a real, playable MP4 above the scanner's 64 KiB minimum. The testkit writes flat MP4
/// bytes; the scanner keys on the file extension, so `.mkv` names get the same bytes.
fn make_media(out: &Path) {
    fs::create_dir_all(out.parent().unwrap()).unwrap();
    let spec = MediaSpec::h264_aac(3.0).with_video(640, 360, 30);
    clipcheck_testkit::write_mp4(out, &spec).expect("write test media");
    let len = fs::metadata(out).expect("stat test media").len();
    assert!(
        len >= 64 * 1024,
        "test media {} is {len} bytes, below the scanner minimum",
        out.display()
    );
}

fn titles(lib: &Library) -> Vec<String> {
    let mut v: Vec<String> = lib
        .list(&ListQuery::default())
        .expect("list")
        .items
        .into_iter()
        .map(|r| r.rel_path.replace('\\', "/"))
        .collect();
    v.sort();
    v
}

#[test]
fn existing_files_in_the_roots_are_registered_listed_and_announced() {
    let tmp = tempfile::tempdir().expect("tempdir");
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let data = tmp.path().join("data");
    let primary = tmp.path().join("ClipCheck");
    let medal = tmp.path().join("Medal");
    fs::create_dir_all(&data).unwrap();
    // ClipCheck-shaped primary root and a Medal-shaped extra library.
    make_media(&primary.join("Clips").join("Apex Legends").join("a.mp4"));
    make_media(&primary.join("Sessions").join("Apex Legends").join("s1.mp4"));
    make_media(&medal.join("Clips").join("Valorant").join("MedalTV Ace.mp4"));
    make_media(&medal.join("Clips").join("Valorant").join("clutch.mkv"));

    let storage = Storage {
        root: primary.clone(),
        libraries: vec![LibraryDir {
            path: medal.display().to_string(),
            label: "Medal".to_owned(),
            read_only: true,
            enabled: true,
        }],
        ..Storage::default()
    };
    let bus = Arc::new(EventBus::new());
    let (sub, rx) = bus.subscribe();
    sub.set_topics(&[Topic::Library]);
    let service = LibraryService::start(LibraryOptions {
        data_dir: data.clone(),
        storage,
        bus: Arc::clone(&bus),
        recording: Arc::new(AtomicBool::new(false)),
    })
    .expect("start");
    service.set_ready();

    // Wait for both roots to report "done", collecting what the UI would be told.
    let deadline = Instant::now() + Duration::from_secs(90);
    let (mut done_roots, mut changed) = (std::collections::HashSet::new(), 0u32);
    while done_roots.len() < 2 && Instant::now() < deadline {
        if let Ok(d) = rx.recv_timeout(Duration::from_millis(200)) {
            match &*d.event {
                Event::LibraryScan { root_id, phase, .. } if phase == "done" => {
                    done_roots.insert(*root_id);
                }
                Event::LibraryChanged {} => changed += 1,
                _ => {}
            }
        }
    }
    service.stop();
    assert_eq!(done_roots.len(), 2, "both roots must finish scanning");

    let lib = Library::open(data.join("library.db")).expect("open");
    let listed = titles(&lib);
    assert_eq!(
        listed,
        [
            "Clips/Apex Legends/a.mp4",
            "Clips/Valorant/MedalTV Ace.mp4",
            "Clips/Valorant/clutch.mkv",
            "Sessions/Apex Legends/s1.mp4",
        ]
        .map(str::to_owned)
        .to_vec(),
        "every existing file must be listed for the gallery"
    );
    assert!(
        changed >= 1,
        "the UI is told through library_changed that rows were added (got {changed})"
    );
}
