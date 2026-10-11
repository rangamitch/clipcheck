//! Tests for `clipcheck_mp4::reader`. Inputs are generated natively with
//! `clipcheck-testkit`; the reader's output is judged against the generating spec
//! (track layout, frame counts, keyframe cadence, duration) and against the
//! Media Foundation probe (`clipcheck_mfdec::probe`) for stream metadata.
//!
//! Skips (prints SKIPPED and returns) when native test media is unavailable.

use clipcheck_mp4::reader::{read_movie, Movie, Track, TrackKind};
use clipcheck_testkit::{write_mp4, write_segments, AudioSpec, MediaSpec, VideoSpec};
use std::path::Path;
use std::time::Instant;

/// What a generated file must look like, derived from the spec that produced it.
struct Want {
    width: u32,
    height: u32,
    gop: u32,
    /// Expected video frame count (whole files only).
    frames: Option<usize>,
    /// Expected duration in seconds (whole files only).
    secs: Option<f64>,
    /// (sample rate, channels) per audio track, in file order.
    audio: Vec<(u32, u16)>,
}

fn want_from(spec: &MediaSpec) -> Want {
    let v = spec.video.as_ref().expect("video in spec");
    Want {
        width: v.width,
        height: v.height,
        gop: v.gop_frames,
        frames: Some((spec.secs * f64::from(v.fps)).round() as usize),
        secs: Some(spec.secs),
        audio: spec
            .audio
            .iter()
            .map(|a| (a.sample_rate, a.channels))
            .collect(),
    }
}

/// Judges a file against `want`: track layout, sample offsets inside the file,
/// keyframe cadence, frame count and duration, plus the MF probe's metadata.
fn verify_file(file: &Path, want: &Want) -> Movie {
    let movie = read_movie(file).unwrap_or_else(|e| panic!("read_movie failed: {e:?}"));
    let file_len = std::fs::metadata(file).expect("metadata").len();
    assert_eq!(movie.tracks.len(), 1 + want.audio.len(), "track count");
    assert!(movie.tracks[0].is_video(), "track 0 must be video");

    let v = video_track(&movie);
    match &v.kind {
        TrackKind::Video { width, height, .. } => {
            assert_eq!((*width, *height), (want.width, want.height), "video size");
        }
        _ => unreachable!(),
    }
    if let Some(frames) = want.frames {
        assert_eq!(v.samples.len(), frames, "video sample count");
    }
    for (i, s) in v.samples.iter().enumerate() {
        assert_eq!(
            s.keyframe,
            (i as u32).is_multiple_of(want.gop),
            "video sample {i}: keyframe flag (gop {})",
            want.gop
        );
    }

    for (k, track) in movie.tracks.iter().skip(1).enumerate() {
        let (rate, ch) = want.audio[k];
        match &track.kind {
            TrackKind::Audio {
                channels,
                sample_rate,
                ..
            } => {
                assert_eq!((*sample_rate, *channels), (rate, ch), "audio track {k}");
            }
            other => panic!("track {} should be audio, got {other:?}", k + 1),
        }
        assert!(!track.samples.is_empty(), "audio track {k} has no samples");
    }

    for track in &movie.tracks {
        for s in &track.samples {
            assert!(
                s.offset + u64::from(s.size) <= file_len,
                "track {} sample at {} size {} exceeds file length {}",
                track.id,
                s.offset,
                s.size,
                file_len
            );
        }
    }

    if let Some(secs) = want.secs {
        assert!(
            (movie.duration_secs - secs).abs() <= 0.1,
            "duration {} vs spec {}",
            movie.duration_secs,
            secs
        );
    }

    let p = clipcheck_mfdec::probe(file).expect("mfdec probe");
    assert_eq!((p.width, p.height), (want.width, want.height), "probe size");
    assert_eq!(p.audio.len(), want.audio.len(), "probe audio track count");
    // The MF probe does not promise the file's audio stream order, so compare as multisets.
    let mut got: Vec<(u32, u32)> = p
        .audio
        .iter()
        .map(|a| (a.sample_rate, a.channels))
        .collect();
    let mut exp: Vec<(u32, u32)> = want
        .audio
        .iter()
        .map(|(r, c)| (*r, u32::from(*c)))
        .collect();
    got.sort_unstable();
    exp.sort_unstable();
    assert_eq!(got, exp, "probe audio formats");
    movie
}

fn video_track(movie: &Movie) -> &Track {
    movie
        .tracks
        .iter()
        .find(|t| t.is_video())
        .expect("video track")
}

macro_rules! require_media {
    () => {
        if !clipcheck_testkit::available() {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        }
    };
}

#[test]
fn t1_flat_h264_with_aac() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("flat.mp4");
    let spec = MediaSpec::h264_aac(3.0);
    write_mp4(&f, &spec).unwrap();

    let want = want_from(&spec);
    let movie = verify_file(&f, &want);
    assert_eq!(movie.tracks.len(), 2);
    let v = video_track(&movie);
    assert_eq!(v.samples.len(), 90);
    let keys = v.samples.iter().filter(|s| s.keyframe).count();
    assert_eq!(keys, 3, "GOP 30 over 90 frames gives 3 keyframes");
    // Not verified here: B-frame cts_offset and the B-frame edit list. The
    // testkit encoder is not asked for B-frames, so there is no spec to check.
}

#[test]
fn t2_fragmented_segments() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();
    let spec = MediaSpec {
        secs: 4.0,
        video: Some(VideoSpec {
            width: 640,
            height: 360,
            fps: 30,
            gop_frames: 60,
        }),
        audio: vec![AudioSpec::sine(440.0)],
    };
    let segs = write_segments(dir.path(), &spec, 2.0).unwrap();
    assert!(!segs.is_empty(), "segments written");

    let want = Want {
        frames: None,
        secs: None,
        ..want_from(&spec)
    };
    let mut total = 0usize;
    for seg in &segs {
        let movie = verify_file(seg, &want);
        assert_eq!(movie.tracks.len(), 2);
        total += video_track(&movie).samples.len();
    }
    println!("  T2 segments={} video samples={total}", segs.len());
    assert_eq!(total, 120, "4 s at 30 fps across all segments");
}

#[test]
fn t3_fragmented_truncated_70pct() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();
    let spec = MediaSpec {
        secs: 4.0,
        video: Some(VideoSpec {
            width: 640,
            height: 360,
            fps: 30,
            gop_frames: 60,
        }),
        audio: vec![AudioSpec::sine(440.0)],
    };
    let segs = write_segments(dir.path(), &spec, 4.0).unwrap();
    let f = segs[0].clone();
    let bytes = std::fs::read(&f).unwrap();
    let cut = bytes.len() * 70 / 100;
    let t = dir.path().join("trunc.mp4");
    std::fs::write(&t, &bytes[..cut]).unwrap();

    let full = read_movie(&f).expect("full read");
    let part = read_movie(&t).expect("truncated read must succeed");
    let full_n: usize = full.tracks.iter().map(|t| t.samples.len()).sum();
    let part_n: usize = part.tracks.iter().map(|t| t.samples.len()).sum();
    println!(
        "  T3 file={}B cut={}B full_samples={} truncated_samples={}",
        bytes.len(),
        cut,
        full_n,
        part_n
    );
    assert!(part_n > 0, "truncated file should yield some samples");
    assert!(part_n < full_n, "truncated file should yield fewer samples");
    for tr in &part.tracks {
        for smp in &tr.samples {
            assert!(
                smp.offset + smp.size as u64 <= cut as u64,
                "track {} sample at {} size {} exceeds truncated length {}",
                tr.id,
                smp.offset,
                smp.size,
                cut
            );
        }
    }
}

#[test]
fn t4_track_titles() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();
    // Dropped in wave F3: the HEVC (libx265) sub-case and the .mov container
    // sub-case. The native generator writes H.264 + AAC MP4 only.
    let tt = dir.path().join("titled.mp4");
    let spec = MediaSpec::h264_aac(1.0).with_audio(vec![AudioSpec::titled(440.0, "Game")]);
    write_mp4(&tt, &spec).unwrap();
    let rd = read_movie(&tt).expect("titled read");
    assert_eq!(
        rd.tracks.get(1).and_then(|t| t.title.as_deref()),
        Some("Game"),
        "reader must return the audio track title"
    );
}

#[test]
fn t5_two_audio_tracks() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("two_audio.mp4");
    let spec = MediaSpec::h264_aac(1.0).with_audio(vec![
        AudioSpec::sine(440.0),
        AudioSpec {
            sample_rate: 44100,
            channels: 2,
            freq_hz: 880.0,
            title: None,
        },
    ]);
    write_mp4(&f, &spec).unwrap();
    let movie = verify_file(&f, &want_from(&spec));
    assert_eq!(movie.tracks.len(), 3, "expect 3 tracks");
    assert!(movie.tracks[0].is_video(), "track 0 must be video");
    assert!(movie.tracks[1].is_audio(), "track 1 must be audio");
    assert!(movie.tracks[2].is_audio(), "track 2 must be audio");
    match (&movie.tracks[1].kind, &movie.tracks[2].kind) {
        (
            TrackKind::Audio {
                sample_rate: r1, ..
            },
            TrackKind::Audio {
                sample_rate: r2, ..
            },
        ) => {
            assert_eq!(*r1, 48000);
            assert_eq!(*r2, 44100);
        }
        _ => panic!("tracks 1 and 2 must be audio"),
    }
}

#[test]
fn t6_robustness() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();

    // Text renamed .mp4 -> Err.
    let txt = dir.path().join("text.mp4");
    std::fs::write(
        &txt,
        "this is plain text, not a movie file at all\n".repeat(20),
    )
    .unwrap();
    assert!(read_movie(&txt).is_err(), "text file must be an error");

    // Empty file -> Err.
    let empty = dir.path().join("empty.mp4");
    std::fs::write(&empty, b"").unwrap();
    assert!(read_movie(&empty).is_err(), "empty file must be an error");

    // Base fixture.
    let base = dir.path().join("base.mp4");
    write_mp4(&base, &MediaSpec::h264_aac(2.0)).unwrap();
    let bytes = std::fs::read(&base).unwrap();
    let len = bytes.len();
    let scratch = dir.path().join("case.mp4");

    let (mut ok, mut err) = (0usize, 0usize);
    for n in (0..len).step_by(997) {
        std::fs::write(&scratch, &bytes[..n]).unwrap();
        match read_movie(&scratch) {
            Ok(_) => ok += 1,
            Err(_) => err += 1,
        }
    }
    println!("  T6 truncations every 997B of {len}B: ok={ok} err={err} (no panic)");

    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let (mut ok, mut err) = (0usize, 0usize);
    for _ in 0..200 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let pos = ((state >> 33) as usize) % len;
        let mask = (((state >> 17) & 0xFF) as u8) | 1;
        let mut b = bytes.clone();
        b[pos] ^= mask;
        std::fs::write(&scratch, &b).unwrap();
        match read_movie(&scratch) {
            Ok(_) => ok += 1,
            Err(_) => err += 1,
        }
    }
    println!("  T6 200 single-byte flips: ok={ok} err={err} (no panic)");
}

#[test]
fn t7_large_1080p60_read_time() {
    require_media!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("big.mp4");
    let spec = MediaSpec::h264_aac(20.0).with_video(1920, 1080, 60);
    write_mp4(&f, &spec).unwrap();
    println!(
        "  T7 file size = {} bytes",
        std::fs::metadata(&f).unwrap().len()
    );
    let start = Instant::now();
    let movie = read_movie(&f).expect("read big");
    let el = start.elapsed();
    println!("  T7 read_movie took {:?}", el);
    let v = video_track(&movie);
    assert_eq!(
        v.samples.len(),
        1200,
        "1080p60 x 20s should have 1200 video samples"
    );
    assert!(el.as_secs_f64() < 1.0, "read took {:?}, expected < 1 s", el);
    verify_file(&f, &want_from(&spec));
}
