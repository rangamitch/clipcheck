#![cfg(windows)]

//! External tests for the public API of clipcheck-testkit. Uses only the public API and the
//! clipcheck-mp4 reader; all output goes to tempfile directories.

use clipcheck_mp4::reader::{read_movie, Movie, Track, TrackKind};
use clipcheck_testkit::{available, write_mp4, write_segments, AudioSpec, MediaSpec, TestkitError};

macro_rules! need {
    () => {
        if !available() {
            eprintln!("SKIPPED: no Media Foundation encoder");
            return;
        }
    };
}

fn video_track(movie: &Movie) -> &Track {
    movie
        .tracks
        .iter()
        .find(|t| t.is_video())
        .expect("no video track")
}

fn audio_tracks(movie: &Movie) -> Vec<&Track> {
    movie.tracks.iter().filter(|t| t.is_audio()).collect()
}

fn sample_duration_sum(t: &Track) -> u64 {
    t.samples.iter().map(|s| u64::from(s.duration)).sum()
}

fn is_bad_spec(r: &Result<(), TestkitError>) -> bool {
    matches!(r, Err(TestkitError::BadSpec(_)))
}

// 1. Determinism: same spec -> same sample counts and durations.
#[test]
fn same_spec_gives_same_sample_counts_and_durations() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let spec = MediaSpec::h264_aac(2.0).with_audio(vec![AudioSpec::sine(440.0)]);
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    write_mp4(&a, &spec).unwrap();
    write_mp4(&b, &spec).unwrap();
    let ma = read_movie(&a).unwrap();
    let mb = read_movie(&b).unwrap();
    assert_eq!(ma.tracks.len(), mb.tracks.len());
    for (ta, tb) in ma.tracks.iter().zip(mb.tracks.iter()) {
        assert_eq!(ta.samples.len(), tb.samples.len(), "sample count differs");
        assert_eq!(
            sample_duration_sum(ta),
            sample_duration_sum(tb),
            "sample duration sum differs"
        );
        assert_eq!(ta.timescale, tb.timescale);
    }
}

// 2. No audio: video only -> 0 audio tracks.
#[test]
fn empty_audio_list_writes_no_audio_track() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("video_only.mp4");
    let spec = MediaSpec::h264_aac(1.0).with_audio(vec![]);
    write_mp4(&path, &spec).unwrap();
    let movie = read_movie(&path).unwrap();
    assert_eq!(audio_tracks(&movie).len(), 0);
    assert!(movie.tracks.iter().any(|t| t.is_video()));
}

// 3. Four titled audio tracks -> 4 tracks, titles in order.
#[test]
fn four_titled_audio_tracks_keep_order_and_titles() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("four.mp4");
    let spec = MediaSpec::h264_aac(1.0).with_audio(vec![
        AudioSpec::titled(220.0, "T220"),
        AudioSpec::titled(440.0, "T440"),
        AudioSpec::titled(660.0, "T660"),
        AudioSpec::titled(880.0, "T880"),
    ]);
    write_mp4(&path, &spec).unwrap();
    let movie = read_movie(&path).unwrap();
    let audio = audio_tracks(&movie);
    assert_eq!(audio.len(), 4, "audio track count");
    let titles: Vec<Option<String>> = audio.iter().map(|t| t.title.clone()).collect();
    assert_eq!(
        titles,
        vec![
            Some("T220".to_string()),
            Some("T440".to_string()),
            Some("T660".to_string()),
            Some("T880".to_string()),
        ]
    );
}

// 4. 44100 Hz mono audio works and reads back.
#[test]
fn mono_44100_audio_round_trips() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mono44k.mp4");
    let mut track = AudioSpec::sine(440.0);
    track.sample_rate = 44_100;
    track.channels = 1;
    let spec = MediaSpec::h264_aac(1.0).with_audio(vec![track]);
    write_mp4(&path, &spec).unwrap();
    let movie = read_movie(&path).unwrap();
    let audio = audio_tracks(&movie);
    assert_eq!(audio.len(), 1);
    match &audio[0].kind {
        TrackKind::Audio {
            sample_rate,
            channels,
            ..
        } => {
            assert_eq!(*sample_rate, 44_100);
            assert_eq!(*channels, 1);
        }
        other => panic!("unexpected kind {other:?}"),
    }
}

// 5. Segment set: 6 s in 2 s segments.
#[test]
fn segment_set_is_complete_keyframed_and_sums_to_six_seconds() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("segs");
    std::fs::create_dir_all(&out).unwrap();
    let spec = MediaSpec::h264_aac(6.0);
    let segs = write_segments(&out, &spec, 2.0).unwrap();
    assert!(out.join("segments.csv").is_file(), "segments.csv missing");
    assert!(!segs.is_empty());
    let mut total = 0.0f64;
    for seg in &segs {
        assert!(seg.is_file(), "listed segment missing: {}", seg.display());
        let m = read_movie(seg).unwrap_or_else(|e| panic!("{}: {e}", seg.display()));
        let v = video_track(&m);
        assert!(
            v.samples.first().is_some_and(|s| s.keyframe),
            "segment does not start with keyframe: {}",
            seg.display()
        );
        total += v.duration_secs();
    }
    let frame = 1.0 / 30.0;
    assert!(
        (total - 6.0).abs() <= 2.0 * frame + 1e-6,
        "summed segment duration {total}"
    );
}

// 6. Bad specs -> BadSpec, no panic.
#[test]
fn bad_specs_return_bad_spec_without_panic() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let cases: Vec<(&str, MediaSpec)> = vec![
        (
            "zero width",
            MediaSpec::h264_aac(1.0).with_video(0, 360, 30),
        ),
        (
            "zero height",
            MediaSpec::h264_aac(1.0).with_video(640, 0, 30),
        ),
        (
            "odd width",
            MediaSpec::h264_aac(1.0).with_video(641, 360, 30),
        ),
        (
            "odd height",
            MediaSpec::h264_aac(1.0).with_video(640, 361, 30),
        ),
        ("fps zero", MediaSpec::h264_aac(1.0).with_video(640, 360, 0)),
        ("secs zero", MediaSpec::h264_aac(0.0)),
        ("secs negative", MediaSpec::h264_aac(-1.0)),
    ];
    for (name, spec) in cases {
        let path = dir.path().join(format!("{}.mp4", name.replace(' ', "_")));
        let r = std::panic::catch_unwind(|| write_mp4(&path, &spec));
        let r = r.unwrap_or_else(|_| panic!("panicked on {name}"));
        assert!(is_bad_spec(&r), "{name}: expected BadSpec, got {r:?}");
    }
}

// 7. Missing parent directory: report behaviour, must not panic.
#[test]
fn missing_parent_directory_does_not_panic() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no").join("such").join("dir").join("a.mp4");
    let spec = MediaSpec::h264_aac(1.0);
    let r = std::panic::catch_unwind(|| write_mp4(&path, &spec));
    let r = r.expect("write_mp4 panicked on missing parent dir");
    match &r {
        Ok(()) => eprintln!(
            "case 7: created missing parent dirs; file exists = {}",
            path.exists()
        ),
        Err(TestkitError::Io(e)) => eprintln!("case 7: returned Err(Io): {e}"),
        Err(other) => eprintln!("case 7: returned other error: {other:?}"),
    }
    assert!(
        r.is_ok() || matches!(r, Err(TestkitError::Io(_))),
        "unexpected result for missing parent: {r:?}"
    );
    if r.is_ok() {
        assert!(path.is_file());
    }
}

// 8. 1280x720 30 fps 2 s.
#[test]
fn hd_720p_two_seconds_works() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hd.mp4");
    let spec = MediaSpec::h264_aac(2.0).with_video(1280, 720, 30);
    write_mp4(&path, &spec).unwrap();
    let movie = read_movie(&path).unwrap();
    let v = video_track(&movie);
    match &v.kind {
        TrackKind::Video { width, height, .. } => {
            assert_eq!((*width, *height), (1280, 720));
        }
        other => panic!("unexpected kind {other:?}"),
    }
    assert_eq!(v.samples.len(), 60, "frame count");
    assert!(
        (movie.duration_secs - 2.0).abs() < 0.1,
        "{}",
        movie.duration_secs
    );
}
