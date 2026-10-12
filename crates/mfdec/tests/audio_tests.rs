//! Black-box tests for `clipcheck_mfdec::audio` (Media Foundation audio decode and peak
//! extraction). Fixtures: a 16-bit PCM WAV written by hand in a temp dir (48 kHz stereo, 2 s,
//! 440 Hz sine at amplitude 0.5), plus an H.264+AAC MP4 from clipcheck-testkit for the AAC path.
//! When the native test encoders are unavailable the MP4 tests print SKIPPED and return.
#![cfg(windows)]

use clipcheck_mfdec::audio::{decode_peaks, AudioDecoder};
use clipcheck_mfdec::DecError;
use clipcheck_testkit::{AudioSpec, MediaSpec};
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

/// Sample rate of the synthetic WAV.
const RATE: u32 = 48_000;
/// Channel count of the synthetic WAV.
const CHANNELS: u16 = 2;
/// Sine frequency of the synthetic WAV, Hz.
const FREQ: f64 = 440.0;
/// Sine peak amplitude of the synthetic WAV (linear, 0..1).
const AMP: f64 = 0.5;
/// Length of the synthetic WAV, seconds.
const SECS: f64 = 2.0;
/// Frames in the synthetic WAV.
const WAV_FRAMES: usize = 96_000;

/// Writes a 16-bit PCM stereo WAV of `SECS` seconds, a 440 Hz sine at `AMP`, to `path`.
fn write_wav(path: &Path) {
    let frames = (SECS * RATE as f64).round() as usize;
    assert_eq!(frames, WAV_FRAMES);
    let block_align = CHANNELS as u32 * 2;
    let data_len = frames as u32 * block_align;
    let mut out: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * block_align).to_le_bytes());
    out.extend_from_slice(&(block_align as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..frames {
        let t = n as f64 / RATE as f64;
        let v = (AMP * (2.0 * PI * FREQ * t).sin() * 32767.0).round() as i16;
        for _ in 0..CHANNELS {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    std::fs::write(path, out).expect("write wav fixture");
}

/// A WAV fixture in a fresh temp directory. The directory lives as long as the returned value.
fn wav_fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sine.wav");
    write_wav(&path);
    (dir, path)
}

/// Opens stream `stream` and returns the error (AudioDecoder has no Debug impl).
fn open_err(path: &Path, stream: u32) -> DecError {
    match AudioDecoder::open(path, stream) {
        Ok(_) => panic!("expected open to fail for stream {stream}"),
        Err(e) => e,
    }
}

/// Reads the whole stream; returns interleaved samples.
fn read_all(dec: &mut AudioDecoder) -> Vec<f32> {
    let mut out = Vec::new();
    while dec
        .read(4096, &mut out)
        .expect("read must not fail on a valid file")
        .is_some()
    {}
    out
}

/// Root mean square of a sample slice (0.0 for an empty slice).
fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt()
}

#[test]
fn wav_open_reports_format_and_duration() {
    let (_dir, path) = wav_fixture();
    let dec = AudioDecoder::open(&path, 0).expect("open wav");
    assert_eq!(dec.sample_rate(), RATE);
    assert_eq!(dec.channels(), 2);
    let dur = dec.duration_100ns();
    assert!(
        (19_000_000..=21_000_000).contains(&dur),
        "duration {dur} not about 20_000_000 (2 s)"
    );
}

#[test]
fn wav_read_rms_matches_sine_amplitude() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    let mut buf = Vec::new();
    let got = dec.read(8192, &mut buf).expect("read");
    assert!(got.is_some(), "first read must return data");
    assert!(!buf.is_empty());
    // A sine of peak AMP has RMS AMP / sqrt(2).
    let expected = AMP / 2f64.sqrt();
    let measured = rms(&buf);
    assert!(
        (measured - expected).abs() / expected < 0.01,
        "rms {measured} differs from {expected} by more than 1 %"
    );
}

#[test]
fn wav_total_decoded_frames_about_two_seconds() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    let samples = read_all(&mut dec);
    let frames = samples.len() / CHANNELS as usize;
    let lo = (WAV_FRAMES as f64 * 0.95) as usize;
    let hi = (WAV_FRAMES as f64 * 1.05) as usize;
    assert!(
        (lo..=hi).contains(&frames),
        "decoded {frames} frames, expected about {WAV_FRAMES}"
    );
    assert_eq!(samples.len() % CHANNELS as usize, 0, "whole frames only");
}

#[test]
fn wav_read_returns_none_at_end_of_stream() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    let mut buf = Vec::new();
    let _ = read_all(&mut dec);
    buf.clear();
    let first = dec.read(4096, &mut buf).expect("read after end");
    assert!(first.is_none(), "read past end must be Ok(None)");
    assert!(buf.is_empty(), "nothing may be appended past end");
    let second = dec.read(4096, &mut buf).expect("second read after end");
    assert!(second.is_none(), "end of stream must stay Ok(None)");
}

#[test]
fn wav_seek_one_second_starts_near_one_second() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    dec.seek(1.0).expect("seek to 1 s");
    let mut buf = Vec::new();
    let (frames, ts) = dec
        .read(4096, &mut buf)
        .expect("read after seek")
        .expect("data after seek");
    assert!(frames > 0);
    let target = 10_000_000i64;
    assert!(
        (ts - target).abs() <= 2_000_000,
        "first timestamp {ts} is not within 0.2 s of {target}"
    );
}

#[test]
fn wav_seek_negative_clamps_to_start() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    dec.seek(-5.0).expect("negative seek must not fail");
    let mut buf = Vec::new();
    let (_, ts) = dec
        .read(4096, &mut buf)
        .expect("read after negative seek")
        .expect("data after negative seek");
    assert!(
        (0..=2_000_000).contains(&ts),
        "timestamp {ts} is not near the start"
    );
}

#[test]
fn wav_read_appends_to_existing_buffer() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    let mut buf = vec![7.0f32, 8.0, 9.0];
    let (frames, _) = dec
        .read(1024, &mut buf)
        .expect("read")
        .expect("data available");
    assert_eq!(&buf[..3], &[7.0, 8.0, 9.0], "existing samples must be kept");
    assert_eq!(buf.len(), 3 + frames * CHANNELS as usize);
}

#[test]
fn wav_read_zero_max_frames_still_returns_a_sample() {
    let (_dir, path) = wav_fixture();
    let mut dec = AudioDecoder::open(&path, 0).expect("open wav");
    let mut buf = Vec::new();
    let got = dec.read(0, &mut buf).expect("read with max_frames 0");
    let (frames, _) = got.expect("at least one decoded sample is returned");
    assert!(frames >= 1);
}

#[test]
fn wav_stream_index_out_of_range_is_bad_input() {
    let (_dir, path) = wav_fixture();
    match open_err(&path, 1) {
        DecError::BadInput(msg) => assert!(msg.contains("audio stream 1"), "message: {msg}"),
        other => panic!("expected BadInput, got {other:?}"),
    }
}

#[test]
fn missing_file_is_bad_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("does-not-exist.wav");
    match open_err(&path, 0) {
        DecError::BadInput(_) => {}
        other => panic!("expected BadInput, got {other:?}"),
    }
}

#[test]
fn garbage_file_is_error_not_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("garbage.wav");
    std::fs::write(&path, b"this is definitely not a wave file at all").expect("write garbage");
    // Any error variant is acceptable; the contract is that it fails cleanly.
    let _ = open_err(&path, 0);
}

#[test]
fn decode_peaks_bucket_count_and_range() {
    let (_dir, path) = wav_fixture();
    let (peaks, _) = decode_peaks(&path, 0, 64).expect("peaks");
    assert_eq!(peaks.len(), 64);
    for (i, p) in peaks.iter().enumerate() {
        assert!(
            (0.0..=1.0).contains(p),
            "bucket {i} value {p} outside 0..=1"
        );
    }
}

#[test]
fn decode_peaks_constant_for_constant_sine() {
    let (_dir, path) = wav_fixture();
    let (peaks, _) = decode_peaks(&path, 0, 32).expect("peaks");
    for (i, p) in peaks.iter().enumerate() {
        assert!(
            (0.45..=0.55).contains(p),
            "bucket {i} peak {p} is not near the sine amplitude {AMP}"
        );
    }
}

#[test]
fn decode_peaks_zero_buckets_is_empty_with_duration() {
    let (_dir, path) = wav_fixture();
    let (peaks, secs) = decode_peaks(&path, 0, 0).expect("zero buckets");
    assert!(peaks.is_empty());
    assert!(
        (secs - SECS).abs() < 0.1,
        "duration {secs} s is not about 2 s"
    );
}

#[test]
fn decode_peaks_duration_about_two_seconds() {
    let (_dir, path) = wav_fixture();
    let (_, secs) = decode_peaks(&path, 0, 10).expect("peaks");
    assert!(
        (secs - SECS).abs() < 0.1,
        "duration {secs} s is not about 2 s"
    );
}

#[test]
fn decode_peaks_missing_file_is_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("missing.wav");
    match decode_peaks(&path, 0, 8) {
        Err(DecError::BadInput(_)) => {}
        Err(other) => panic!("expected BadInput, got {other:?}"),
        Ok(_) => panic!("expected an error for a missing file"),
    }
}

#[test]
fn wav_decodes_identically_on_reopen() {
    let (_dir, path) = wav_fixture();
    let mut a = AudioDecoder::open(&path, 0).expect("open a");
    let mut b = AudioDecoder::open(&path, 0).expect("open b");
    let mut sa = Vec::new();
    let mut sb = Vec::new();
    a.read(1000, &mut sa).expect("read a");
    b.read(1000, &mut sb).expect("read b");
    assert!(!sa.is_empty());
    assert_eq!(sa, sb, "two decoders over the same file must agree");
}

/// Writes an H.264 + AAC MP4 with the given audio tracks, `secs` long, into a temp dir.
fn mp4_fixture(secs: f64, audio: Vec<AudioSpec>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("av.mp4");
    let spec = MediaSpec::h264_aac(secs).with_audio(audio);
    clipcheck_testkit::write_mp4(&path, &spec).expect("write mp4 fixture");
    (dir, path)
}

#[test]
fn mp4_aac_stream_zero_format() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let (_dir, path) = mp4_fixture(1.0, vec![AudioSpec::sine(440.0)]);
    let dec = AudioDecoder::open(&path, 0).expect("open aac stream 0");
    assert_eq!(dec.sample_rate(), 48_000);
    assert_eq!(dec.channels(), 2);
}

#[test]
fn mp4_aac_decodes_about_the_clip_length() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let (_dir, path) = mp4_fixture(1.0, vec![AudioSpec::sine(440.0)]);
    let mut dec = AudioDecoder::open(&path, 0).expect("open aac");
    let samples = read_all(&mut dec);
    let frames = samples.len() / 2;
    // AAC priming and padding move the count a little; allow 10 %.
    assert!(
        (43_200..=52_800).contains(&frames),
        "decoded {frames} frames from a 1 s AAC track"
    );
    assert!(rms(&samples) > 0.01, "decoded AAC must not be silent");
}

#[test]
fn mp4_second_audio_track_is_selectable_and_third_is_not() {
    if !clipcheck_testkit::available() {
        eprintln!("SKIPPED: native test media unavailable");
        return;
    }
    let (_dir, path) = mp4_fixture(
        1.0,
        vec![AudioSpec::sine(440.0), AudioSpec::titled(880.0, "second")],
    );
    let dec = AudioDecoder::open(&path, 1).expect("open second audio track");
    assert_eq!(dec.sample_rate(), 48_000);
    match open_err(&path, 2) {
        DecError::BadInput(_) => {}
        other => panic!("expected BadInput for stream 2, got {other:?}"),
    }
}
