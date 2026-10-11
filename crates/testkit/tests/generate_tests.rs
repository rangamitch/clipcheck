#![cfg(windows)]

use clipcheck_mfdec::{OutFormat, VideoDecoder};
use clipcheck_mp4::reader::{read_movie, TrackKind};
use clipcheck_testkit::{available, write_mp4, write_segments, AudioSpec, MediaSpec};

macro_rules! need {
    () => {
        if !available() {
            eprintln!("skip: Media Foundation H.264/AAC encoders unavailable");
            return;
        }
    };
}

#[test]
fn flat_mp4_has_expected_structure() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.mp4");
    let spec = MediaSpec::h264_aac(3.0).with_audio(vec![
        AudioSpec::titled(440.0, "Game"),
        AudioSpec::titled(880.0, "Mic"),
    ]);
    write_mp4(&path, &spec).unwrap();
    let movie = read_movie(&path).unwrap();
    let video = movie.tracks.iter().find(|t| t.is_video()).unwrap();
    assert_eq!(video.samples.len(), 90);
    assert!(
        (movie.duration_secs - 3.0).abs() < 1.0 / 30.0 + 0.03,
        "{}",
        movie.duration_secs
    );
    let keys: Vec<usize> = video
        .samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.keyframe)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(keys, vec![0, 30, 60]);
    let audio: Vec<_> = movie.tracks.iter().filter(|t| t.is_audio()).collect();
    assert_eq!(audio.len(), 2);
    assert_eq!(audio[0].title.as_deref(), Some("Game"));
    assert_eq!(audio[1].title.as_deref(), Some("Mic"));
    assert!(matches!(
        audio[0].kind,
        TrackKind::Audio {
            sample_rate: 48_000,
            ..
        }
    ));
    assert!((audio[0].duration_secs() - 3.0).abs() < 0.1);
}

#[test]
fn small_size_other_fps() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.mp4");
    let spec = MediaSpec::h264_aac(2.0).with_video(160, 120, 15);
    write_mp4(&path, &spec).unwrap();
    let movie = read_movie(&path).unwrap();
    let video = movie.tracks.iter().find(|t| t.is_video()).unwrap();
    assert_eq!(video.samples.len(), 30);
}

#[test]
fn segments_and_csv() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("seg");
    let segs = write_segments(&out, &MediaSpec::h264_aac(5.0), 2.0).unwrap();
    assert!(segs.len() >= 2, "{segs:?}");
    assert!(segs[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("seg-000000"));
    assert!(out.join("segments.csv").exists());
    for s in &segs {
        assert!(s.exists());
    }
}

#[test]
fn first_frame_decodes() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.mp4");
    write_mp4(&path, &MediaSpec::h264_aac(1.0)).unwrap();
    let probe = clipcheck_mfdec::probe(&path).unwrap();
    assert_eq!((probe.width, probe.height), (640, 360));
    assert_eq!(probe.video_codec, "h264");
    assert_eq!(probe.audio.len(), 1);
    assert_eq!(probe.audio[0].sample_rate, 48_000);
    let mut dec = VideoDecoder::open(&path, OutFormat::Bgra).unwrap();
    let f = dec.next_frame().unwrap().expect("a frame");
    assert_eq!((f.width, f.height), (640, 360));
    assert!(f.data.iter().any(|&b| b != 0));
}

#[test]
fn rejects_bad_specs() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = MediaSpec::h264_aac(1.0);
    spec.video = None;
    assert!(write_mp4(&dir.path().join("x.mp4"), &spec).is_err());
}

#[test]
fn six_second_mp4_keeps_all_audio_packets() {
    need!();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.mp4");
    write_mp4(&path, &MediaSpec::h264_aac(6.0)).unwrap();
    let movie = read_movie(&path).unwrap();
    let audio = movie.tracks.iter().find(|t| t.is_audio()).unwrap();
    let expected = (6 * 48_000 / 1024) as i64;
    let got = audio.samples.len() as i64;
    assert!(
        (got - expected).abs() <= 1,
        "{got} AAC packets, expected {expected}"
    );
}
