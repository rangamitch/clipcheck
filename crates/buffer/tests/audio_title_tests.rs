//! Integration tests for the audio track title preservation through
//! `clipcheck_buffer::extract_clip` (the remuxer keeps the titles). Fixtures are
//! generated with `clipcheck-testkit` once per process into a shared fixture
//! directory. Each test copies the segments it needs into its own tempdir.

mod common;

use clipcheck_buffer::extract_clip;
use clipcheck_mp4::reader::{read_movie, TrackKind};
use clipcheck_testkit::{write_mp4, AudioSpec, MediaSpec};
use common::{plan_of, shared_fixture_dir};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Writes a 2 s mp4 with a video track and three audio tracks (440, 880 and
/// 220 Hz sines). When `titled` is true the tracks get the titles Mix, Game
/// and Mic.
fn make_audio_segment(out: &Path, titled: bool) {
    let audio = if titled {
        vec![
            AudioSpec::titled(440.0, "Mix"),
            AudioSpec::titled(880.0, "Game"),
            AudioSpec::titled(220.0, "Mic"),
        ]
    } else {
        vec![
            AudioSpec::sine(440.0),
            AudioSpec::sine(880.0),
            AudioSpec::sine(220.0),
        ]
    };
    let spec = MediaSpec::h264_aac(2.0)
        .with_video(160, 120, 30)
        .with_audio(audio);
    write_mp4(out, &spec).unwrap_or_else(|e| panic!("generate audio segment: {e}"));
    assert!(out.is_file(), "segment not written: {}", out.display());
}

/// Returns the shared 2 s audio segment (titled or untitled), generating it on
/// first use in this process. Callers must treat it as read-only and copy it
/// before writing anything.
fn shared_audio_segment(titled: bool) -> &'static Path {
    static TITLED: OnceLock<PathBuf> = OnceLock::new();
    static UNTITLED: OnceLock<PathBuf> = OnceLock::new();
    let cell = if titled { &TITLED } else { &UNTITLED };
    cell.get_or_init(|| {
        let dir = shared_fixture_dir("audio-src");
        let out = dir.join(if titled { "titled.mp4" } else { "untitled.mp4" });
        make_audio_segment(&out, titled);
        out
    })
    .as_path()
}

/// Titles of every audio track in `file`, in track order. Yields an empty
/// string for a track that has no title.
fn audio_titles(file: &Path) -> Vec<String> {
    let movie = read_movie(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
    movie
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Audio { .. }))
        .map(|t| t.title.clone().unwrap_or_default())
        .collect()
}

/// Two 2 s copies of the shared segment (titled or untitled) in a fresh
/// tempdir, returned with the dir so it outlives the test body.
fn two_segments(titled: bool) -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = shared_audio_segment(titled);
    let segs: Vec<PathBuf> = (0..2)
        .map(|i| {
            let dest = dir.path().join(format!("seg-{i}.mp4"));
            fs::copy(src, &dest).expect("copy shared segment");
            dest
        })
        .collect();
    (dir, segs)
}

#[test]
fn extract_clip_preserves_audio_titles() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let (dir, segs) = two_segments(true);

    let out = dir.path().join("clip.mp4");
    extract_clip(&plan_of(&segs, 4.0), &out).expect("extract_clip");

    let titles = audio_titles(&out);
    assert_eq!(
        titles.len(),
        3,
        "expected 3 audio streams, got {}: {titles:?}",
        titles.len()
    );
    assert_eq!(titles, vec!["Mix", "Game", "Mic"]);
}

#[test]
fn extract_clip_without_titles_succeeds_with_empty_titles() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let (dir, segs) = two_segments(false);

    let out = dir.path().join("clip.mp4");
    extract_clip(&plan_of(&segs, 4.0), &out).expect("extract_clip");

    let titles = audio_titles(&out);
    assert_eq!(
        titles.len(),
        3,
        "expected 3 audio streams, got {}: {titles:?}",
        titles.len()
    );
    assert!(
        titles.iter().all(|t| t.is_empty()),
        "expected empty titles, got {titles:?}"
    );
}
