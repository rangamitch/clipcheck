//! Tests for `clipcheck_ui_lib::player` (spec: docs/impl/player-mvp.md, "player.rs").
//! Black-box: public API and the spec. Fixtures are H.264 + AAC MP4 files generated with
//! `clipcheck-testkit` into TempDirs.

mod common_player;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use clipcheck_testkit::{AudioSpec, MediaSpec};
use clipcheck_ui_lib::media::{serve, sidecar_path};
use clipcheck_ui_lib::player::{cache_dir_from, open_at, PlayerOpenDto};
use common_player::{hdr, make_db, must, pattern, Row};
use tempfile::TempDir;

const SKIP_MSG: &str = "SKIPPED: native test media unavailable";

/// Media fixtures, generated once per test binary with clipcheck-testkit.
struct Fx {
    _dir: TempDir,
    dir: PathBuf,
}

static FX: OnceLock<Option<Fx>> = OnceLock::new();

fn fx() -> Option<&'static Fx> {
    FX.get_or_init(build_fx).as_ref()
}

fn build_fx() -> Option<Fx> {
    if !clipcheck_testkit::available() {
        return None;
    }
    let dir = TempDir::new().expect("tempdir");
    let d = dir.path().to_path_buf();
    let base = |audio: Vec<AudioSpec>| {
        MediaSpec::h264_aac(3.0)
            .with_video(320, 180, 15)
            .with_audio(audio)
    };
    // 2 audio tracks (titles Mix / Mic), 3 s, 320x180.
    clipcheck_testkit::write_mp4(
        &d.join("two.mp4"),
        &base(vec![
            AudioSpec::titled(440.0, "Mix"),
            AudioSpec::titled(880.0, "Mic"),
        ]),
    )
    .expect("write two.mp4");
    // 1 audio track.
    clipcheck_testkit::write_mp4(&d.join("one.mp4"), &base(vec![AudioSpec::sine(440.0)]))
        .expect("write one.mp4");
    // Video only.
    clipcheck_testkit::write_mp4(&d.join("video.mp4"), &base(vec![])).expect("write video.mp4");

    Some(Fx { _dir: dir, dir: d })
}

/// Returns the fixtures, or prints the loud skip line and returns None.
macro_rules! fixtures_or_skip {
    () => {
        match fx() {
            Some(f) => f,
            None => {
                eprintln!("{SKIP_MSG}");
                return;
            }
        }
    };
}

/// A fresh DB + cache inside its own TempDir, rooted at the shared fixture dir.
struct Ctx {
    _t: TempDir,
    db: PathBuf,
    cache: PathBuf,
}

fn ctx(root: &Path, rows: &[Row<'_>]) -> Ctx {
    let t = TempDir::new().expect("tempdir");
    let db = make_db(&t.path().join("library.db"), root, rows);
    let cache = t.path().join("cache");
    Ctx { _t: t, db, cache }
}

/// Track kinds of an MP4 read with the in-process reader: `"audio:<codec>"`, `"video"`, `"other"`.
fn mp4_track_kinds(file: &Path) -> Vec<String> {
    use clipcheck_mp4::reader::{read_movie, TrackKind};
    let movie = must(read_movie(file), "read_movie");
    movie
        .tracks
        .iter()
        .map(|t| match &t.kind {
            TrackKind::Audio { codec, .. } => format!("audio:{codec}"),
            TrackKind::Video { .. } => "video".to_owned(),
            TrackKind::Other { .. } => "other".to_owned(),
        })
        .collect()
}

// ---------------------------------------------------------------- cache_dir_from

#[test]
fn cache_dir_env_wins() {
    let got = cache_dir_from(Some(r"D:\mycache"), Some(r"C:\Users\x\AppData\Local"));
    assert_eq!(got, PathBuf::from(r"D:\mycache"));
}

#[test]
fn cache_dir_blank_env_ignored() {
    let la = r"C:\Users\x\AppData\Local";
    let want = Path::new(la).join("ClipCheck").join("cache");
    assert_eq!(cache_dir_from(Some(""), Some(la)), want);
    assert_eq!(cache_dir_from(Some("   "), Some(la)), want);
}

#[test]
fn cache_dir_localappdata_join() {
    let la = r"C:\Users\x\AppData\Local";
    assert_eq!(
        cache_dir_from(None, Some(la)),
        Path::new(la).join("ClipCheck").join("cache")
    );
}

#[test]
fn cache_dir_fallback_temp() {
    assert_eq!(
        cache_dir_from(None, None),
        std::env::temp_dir().join("ClipCheck").join("cache")
    );
}

// ---------------------------------------------------------------- open_at (generated fixtures)

#[test]
fn open_at_two_tracks_extracts_sidecars() {
    let f = fixtures_or_skip!();
    let c = ctx(
        &f.dir,
        &[Row {
            id: "01PLTWO0000000000000000000",
            rel_path: "two.mp4",
            ext: "mp4",
            duration_ms: 3000,
        }],
    );
    let dto: PlayerOpenDto = must(
        open_at(&c.db, &c.cache, "01PLTWO0000000000000000000"),
        "open_at",
    );

    assert_eq!(dto.id, "01PLTWO0000000000000000000");
    assert_eq!(
        dto.video_url,
        "http://clip.localhost/rec/01PLTWO0000000000000000000"
    );
    assert!(dto.playable);
    assert!(dto.has_video);
    assert_eq!((dto.width, dto.height), (320, 180));
    assert_eq!(dto.video_codec, "h264");
    assert!(
        (2900..=3150).contains(&dto.duration_ms),
        "duration {}",
        dto.duration_ms
    );
    assert!(dto.mixed, "2 tracks must be mixed");
    assert_eq!(dto.audio_error, None);
    assert_eq!(dto.tracks.len(), 2);
    for (i, t) in dto.tracks.iter().enumerate() {
        assert_eq!(t.index, u32::try_from(i).unwrap());
        assert_eq!(t.codec, "aac");
        assert!(t.channels > 0);
        assert!(t.sample_rate > 0);
        assert_eq!(
            t.url.as_deref(),
            Some(format!("http://clip.localhost/aud/01PLTWO0000000000000000000/{i}").as_str())
        );
    }

    let src = f.dir.join("two.mp4");
    let size = std::fs::metadata(&src).expect("meta").len();
    let mtime = common_player::mtime_ms(&src);
    let sp: Vec<PathBuf> = (0..2)
        .map(|n| sidecar_path(&c.cache, "01PLTWO0000000000000000000", size, mtime, n))
        .collect();
    for p in &sp {
        let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        assert!(len > 0, "sidecar missing or empty: {}", p.display());
    }

    // No leftover .part files.
    for p in &sp {
        for entry in std::fs::read_dir(p.parent().expect("parent")).expect("read_dir") {
            let name = entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            assert!(!name.ends_with(".part"), "leftover {name}");
        }
    }

    // Each sidecar is an MP4 with exactly one track, an AAC audio track.
    for p in &sp {
        let kinds = mp4_track_kinds(p);
        assert_eq!(kinds, vec!["audio:aac".to_owned()], "{}", p.display());
    }

    // Cache reuse: a second open_at must not rewrite the sidecars.
    let before: Vec<_> = sp
        .iter()
        .map(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .expect("mtime")
        })
        .collect();
    let again = must(
        open_at(&c.db, &c.cache, "01PLTWO0000000000000000000"),
        "open_at again",
    );
    assert!(again.mixed);
    let after: Vec<_> = sp
        .iter()
        .map(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .expect("mtime")
        })
        .collect();
    assert_eq!(before, after, "cache must be reused, not re-extracted");

    // Served through the sidecar URL path, bytes equal the file.
    let r = serve(&c.db, &c.cache, "/aud/01PLTWO0000000000000000000/0", None);
    assert!(r.status == 200 || r.status == 206, "status {}", r.status);
    assert_eq!(r.body, std::fs::read(&sp[0]).expect("read sidecar"));
    assert_eq!(
        hdr(&r.headers, "Content-Type").as_deref(),
        Some("audio/mp4")
    );
    let r1 = serve(
        &c.db,
        &c.cache,
        "/aud/01PLTWO0000000000000000000/1",
        Some("bytes=0-99"),
    );
    assert_eq!(r1.status, 206);
    assert_eq!(r1.body, std::fs::read(&sp[1]).expect("read sidecar")[..100]);
}

/// Spec: a track's title comes from the MP4 `udta` name, falling back to `handler_name` when it is
/// not a generic handler. The fixture's track titles (`AudioSpec::titled`) are written by
/// clipcheck-testkit.
#[test]
fn open_at_two_tracks_titles_mix_and_mic() {
    let f = fixtures_or_skip!();
    let c = ctx(
        &f.dir,
        &[Row {
            id: "01PLTTL0000000000000000000",
            rel_path: "two.mp4",
            ext: "mp4",
            duration_ms: 3000,
        }],
    );
    let dto = must(
        open_at(&c.db, &c.cache, "01PLTTL0000000000000000000"),
        "open_at",
    );
    let titles: Vec<&str> = dto.tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["Mix", "Mic"]);
}

#[test]
fn open_at_one_track_no_sidecars() {
    let f = fixtures_or_skip!();
    let c = ctx(
        &f.dir,
        &[Row {
            id: "01PLONE0000000000000000000",
            rel_path: "one.mp4",
            ext: "mp4",
            duration_ms: 3000,
        }],
    );
    let dto = must(
        open_at(&c.db, &c.cache, "01PLONE0000000000000000000"),
        "open_at",
    );
    assert!(!dto.mixed);
    assert_eq!(dto.tracks.len(), 1);
    assert_eq!(dto.tracks[0].url, None);
    assert_eq!(dto.audio_error, None);
    assert!(dto.has_video);
    assert!(!c.cache.exists(), "no sidecar dir for a 1-track file");
}

#[test]
fn open_at_video_only_has_no_tracks() {
    let f = fixtures_or_skip!();
    let c = ctx(
        &f.dir,
        &[Row {
            id: "01PLVID0000000000000000000",
            rel_path: "video.mp4",
            ext: "mp4",
            duration_ms: 3000,
        }],
    );
    let dto = must(
        open_at(&c.db, &c.cache, "01PLVID0000000000000000000"),
        "open_at",
    );
    assert!(dto.tracks.is_empty());
    assert!(!dto.mixed);
    assert!(dto.has_video);
    assert_eq!(dto.audio_error, None);
}

#[test]
fn open_at_corrupt_file_reports_outcome() {
    let t = TempDir::new().expect("tempdir");
    let root = t.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("junk.mp4"), pattern(4096)).expect("write");
    let c = ctx(
        &root,
        &[Row {
            id: "01PLBAD0000000000000000000",
            rel_path: "junk.mp4",
            ext: "mp4",
            duration_ms: 1000,
        }],
    );
    match open_at(&c.db, &c.cache, "01PLBAD0000000000000000000") {
        Err(e) => eprintln!("OBSERVATION: corrupt file => Err({e})"),
        Ok(dto) => {
            eprintln!(
                "OBSERVATION: corrupt file => Ok(tracks={}, mixed={}, audio_error={:?})",
                dto.tracks.len(),
                dto.mixed,
                dto.audio_error
            );
            assert!(!dto.mixed, "corrupt file must never be mixed");
            assert!(
                dto.audio_error.is_some() || dto.tracks.is_empty(),
                "corrupt file must not report usable tracks silently"
            );
        }
    }
}

// ---------------------------------------------------------------- open_at (no fixtures needed)

/// Audio is extracted in process: with no external tool the 2-track MP4 still gets its sidecars
/// and no tool message appears anywhere.
#[test]
fn open_at_extracts_audio_without_external_tool() {
    let f = fixtures_or_skip!();
    let c = ctx(
        &f.dir,
        &[Row {
            id: "01PLNOFF000000000000000000",
            rel_path: "two.mp4",
            ext: "mp4",
            duration_ms: 7777,
        }],
    );
    let dto = must(
        open_at(&c.db, &c.cache, "01PLNOFF000000000000000000"),
        "open_at",
    );
    assert_eq!(dto.audio_error, None, "no tool message expected");
    assert!(dto.mixed, "2 tracks must be mixed without external tools");
    assert_eq!(dto.tracks.len(), 2);
    assert!(dto.playable);
    assert!(dto.duration_ms > 0, "duration {}", dto.duration_ms);
    assert_eq!(
        dto.video_url,
        "http://clip.localhost/rec/01PLNOFF000000000000000000"
    );
}

/// Fake bytes that no reader accepts make `open_at` fail; they never produce a playable row.
#[test]
fn open_at_fake_bytes_is_err() {
    let t = TempDir::new().expect("tempdir");
    let root = t.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("fake.mp4"), pattern(2048)).expect("write");
    let c = ctx(
        &root,
        &[Row {
            id: "01PLFAKE000000000000000000",
            rel_path: "fake.mp4",
            ext: "mp4",
            duration_ms: 7777,
        }],
    );
    let err = open_at(&c.db, &c.cache, "01PLFAKE000000000000000000")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(!err.is_empty(), "fake bytes must not open");
    assert!(!err.to_lowercase().contains("not found"), "{err}");
}

#[test]
fn open_at_unknown_id_is_err() {
    let t = TempDir::new().expect("tempdir");
    let c = ctx(t.path(), &[]);
    assert!(open_at(&c.db, &c.cache, "NOPE").is_err());
}

#[test]
fn open_at_deleted_source_is_err() {
    let t = TempDir::new().expect("tempdir");
    let root = t.path().join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("gone.mp4"), pattern(64)).expect("write");
    let c = ctx(
        &root,
        &[Row {
            id: "01PLGONE000000000000000000",
            rel_path: "gone.mp4",
            ext: "mp4",
            duration_ms: 1000,
        }],
    );
    std::fs::remove_file(root.join("gone.mp4")).expect("remove");
    assert!(open_at(&c.db, &c.cache, "01PLGONE000000000000000000").is_err());
}
