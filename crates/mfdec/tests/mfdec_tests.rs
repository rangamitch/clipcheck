//! Black-box tests for clipcheck-mfdec (Media Foundation probe/decode + WIC JPEG thumbnails).
//! Fixtures are generated with clipcheck-testkit (native H.264 + AAC MP4). If the native
//! encoders are unavailable the test prints SKIPPED and returns.
#![cfg(windows)]

use clipcheck_mfdec::{probe, thumbnail_jpeg, DecError, Frame, OutFormat, VideoDecoder};
use clipcheck_testkit::{AudioSpec, MediaSpec};
use std::path::{Path, PathBuf};

/// One frame at 60 fps in 100 ns units (166_666.67).
const FRAME_100NS: i64 = 166_667;

macro_rules! native_or_skip {
    () => {
        if !clipcheck_testkit::available() {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        }
    };
}

/// 1280x720 60 fps H.264 + stereo AAC 48 kHz, 4 s, keyframe every 60 frames.
fn make_h264(out: &Path) {
    let spec = MediaSpec::h264_aac(4.0).with_video(1280, 720, 60);
    clipcheck_testkit::write_mp4(out, &spec).expect("write 1280x720 h264 fixture");
}

fn decode_all(path: &Path, fmt: OutFormat) -> Vec<Frame> {
    let mut d = VideoDecoder::open(path, fmt).expect("open");
    let mut v = Vec::new();
    while let Some(f) = d.next_frame().expect("next_frame") {
        v.push(f);
    }
    v
}

/// Returns (width, height) from the first SOF0/SOF2 marker of a JPEG.
fn jpeg_dims(b: &[u8]) -> Option<(u16, u16)> {
    let mut i = 2usize;
    while i + 9 <= b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = b[i + 1];
        if m == 0xFF || m == 0x00 {
            i += 1;
            continue;
        }
        if m == 0xC0 || m == 0xC2 {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return Some((w, h));
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        i += 2 + len;
    }
    None
}

// 1. Probe of H.264 + AAC mp4.
#[test]
fn t01_probe_h264_aac_mp4() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("h264.mp4");
    make_h264(&f);

    let p = probe(&f).expect("probe");
    assert_eq!(p.width, 1280);
    assert_eq!(p.height, 720);
    assert!((p.fps - 60.0).abs() <= 0.1, "fps {}", p.fps);
    assert_eq!(p.video_codec.as_str(), "h264");
    assert!(
        (p.duration_ms - 4000).abs() <= 60,
        "duration_ms {}",
        p.duration_ms
    );
    assert_eq!(p.audio.len(), 1);
    assert_eq!(p.audio[0].codec.as_str(), "aac");
    assert_eq!(p.audio[0].channels, 2);
    assert_eq!(p.audio[0].sample_rate, 48000);
}

// 2. Two audio tracks.
#[test]
fn t02_two_audio_tracks() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("two_audio.mp4");
    let spec = MediaSpec::h264_aac(2.0)
        .with_video(1280, 720, 60)
        .with_audio(vec![AudioSpec::sine(440.0), AudioSpec::sine(880.0)]);
    clipcheck_testkit::write_mp4(&f, &spec).expect("write two-audio fixture");
    let p = probe(&f).expect("probe");
    assert_eq!(p.audio.len(), 2, "audio tracks");
}

// 3. (DROPPED) Same streams in MKV: testkit writes MP4/fragmented only.

// 4. (DROPPED) HEVC open/probe: testkit produces H.264 only.

// 5. Full decode: frame count, pts spacing, buffer layout, alpha.
#[test]
fn t05_decode_counts_pts_and_layout() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("h264.mp4");
    make_h264(&f);

    let bgra = decode_all(&f, OutFormat::Bgra);
    assert!(
        (238..=242).contains(&bgra.len()),
        "bgra frame count {}",
        bgra.len()
    );
    let pts: Vec<i64> = bgra.iter().map(|fr| fr.pts).collect();
    for w in pts.windows(2) {
        let d = w[1] - w[0];
        assert!(d > 0, "pts not strictly increasing: {} then {}", w[0], w[1]);
        assert!((d - FRAME_100NS).abs() <= 1000, "pts spacing {d}");
    }
    for (i, fr) in bgra.iter().enumerate() {
        let (w, h) = (fr.width as usize, fr.height as usize);
        assert_eq!(fr.data.len(), w * h * 4, "bgra len, frame {i}");
        assert!(
            fr.data.iter().skip(3).step_by(4).all(|&b| b == 255),
            "bgra alpha not 255 in frame {i}"
        );
    }

    let nv = decode_all(&f, OutFormat::Nv12);
    assert!(
        (238..=242).contains(&nv.len()),
        "nv12 frame count {}",
        nv.len()
    );
    for (i, fr) in nv.iter().enumerate() {
        let (w, h) = (fr.width as usize, fr.height as usize);
        assert_eq!(fr.data.len(), w * h * 3 / 2, "nv12 len, frame {i}");
    }
}

// 6. Odd/cropped coded size: 1920x1080 visible.
#[test]
fn t06_visible_size_1080p() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("1080.mp4");
    let spec = MediaSpec::h264_aac(1.0)
        .with_video(1920, 1080, 30)
        .with_audio(vec![]);
    clipcheck_testkit::write_mp4(&f, &spec).expect("write 1080p fixture");

    let p = probe(&f).expect("probe");
    assert_eq!((p.width, p.height), (1920, 1080));

    let bgra = decode_all(&f, OutFormat::Bgra);
    assert!(!bgra.is_empty());
    assert_eq!((bgra[0].width, bgra[0].height), (1920, 1080));
    assert_eq!(bgra[0].data.len(), 1920 * 1080 * 4);

    let nv = decode_all(&f, OutFormat::Nv12);
    assert!(!nv.is_empty());
    assert_eq!((nv[0].width, nv[0].height), (1920, 1080));
    assert_eq!(nv[0].data.len(), 1920 * 1080 * 3 / 2);
}

// 7. (DROPPED) Pixel values for a solid red source: testkit has no solid-colour source, so the
// exact centre-pixel colour cannot be predicted from the spec.

// 8. Seek lands on keyframe at/before 2.0 s and decoding reaches 2.0 s.
#[test]
fn t08_seek_to_2s() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("h264.mp4");
    make_h264(&f);

    let mut d = VideoDecoder::open(&f, OutFormat::Bgra).expect("open");
    d.seek(2.0).expect("seek");
    let mut pts = Vec::new();
    while let Some(fr) = d.next_frame().expect("next_frame") {
        pts.push(fr.pts);
    }
    assert!(!pts.is_empty(), "no frames after seek");
    let first = pts[0];
    eprintln!(
        "seek(2.0): first pts = {first} ({} frames after)",
        pts.len()
    );
    assert!(
        (10_000_000..=20_500_000).contains(&first),
        "first pts after seek {first} not in [1.0 s, 2.05 s]"
    );
    assert!(
        pts.iter().any(|&p| (19_833_000..=20_170_000).contains(&p)),
        "no frame near 2.0 s after seek"
    );
    for w in pts.windows(2) {
        assert!(w[1] > w[0], "pts not increasing after seek");
    }
}

// 9. Thumbnail JPEG.
#[test]
fn t09_thumbnail_jpeg() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("h264.mp4");
    make_h264(&src);

    // Normal thumbnail at 1.5 s, 320 wide.
    let out = dir.path().join("thumb.jpg");
    thumbnail_jpeg(&src, 1.5, 320, &out).expect("thumbnail 1.5s");
    let bytes = std::fs::read(&out).expect("read jpeg");
    assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "JPEG SOI");
    assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9], "JPEG EOI");
    assert!(bytes.len() > 1024, "jpeg size {}", bytes.len());
    let dims = jpeg_dims(&bytes);
    eprintln!(
        "thumbnail 1.5s dims = {:?}, size = {} bytes",
        dims,
        bytes.len()
    );
    assert_eq!(dims, Some((320, 180)));
    assert!(
        !PathBuf::from(format!("{}.part", out.display())).exists(),
        "leftover .part"
    );

    // Width 0 -> BadInput.
    let out0 = dir.path().join("thumb0.jpg");
    match thumbnail_jpeg(&src, 1.5, 0, &out0) {
        Err(DecError::BadInput(_)) => {}
        other => panic!("width 0 expected BadInput, got {other:?}"),
    }

    // Non-existent nested subdir is created.
    let out_sub = dir.path().join("a").join("b").join("c").join("t.jpg");
    thumbnail_jpeg(&src, 1.5, 320, &out_sub).expect("thumbnail into new subdir");
    assert!(out_sub.exists(), "subdir output missing");

    // No .part files left in the output directories.
    for o in [&out, &out0, &out_sub] {
        let parent = o.parent().unwrap();
        for e in std::fs::read_dir(parent).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".part"), "leftover part file {name}");
        }
    }
}

// 9b. Thumbnail requested past the end of the file must still produce a JPEG (last frame).
#[test]
fn t09b_thumbnail_past_end() {
    native_or_skip!();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("h264.mp4");
    make_h264(&src);

    let out_end = dir.path().join("thumb_end.jpg");
    thumbnail_jpeg(&src, 99.0, 320, &out_end).expect("thumbnail past end");
    let b2 = std::fs::read(&out_end).expect("read end jpeg");
    assert_eq!(&b2[..3], &[0xFF, 0xD8, 0xFF]);
    assert_eq!(jpeg_dims(&b2), Some((320, 180)));
}

// 10. Robustness: garbage .mp4 and missing file must return Err, not panic.
#[test]
fn t10_robustness_bad_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake.mp4");
    std::fs::write(
        &fake,
        b"this is plain text, definitely not a video container",
    )
    .unwrap();
    assert!(probe(&fake).is_err(), "probe of text file should be Err");
    assert!(
        VideoDecoder::open(&fake, OutFormat::Bgra).is_err(),
        "open of text file should be Err"
    );
    assert!(
        VideoDecoder::open(&fake, OutFormat::Nv12).is_err(),
        "open(nv12) of text file should be Err"
    );

    let missing = dir.path().join("does_not_exist.mp4");
    assert!(
        matches!(probe(&missing), Err(DecError::BadInput(_))),
        "probe of missing file should be BadInput"
    );
    assert!(
        VideoDecoder::open(&missing, OutFormat::Bgra).is_err(),
        "open of missing file should be Err"
    );
}
