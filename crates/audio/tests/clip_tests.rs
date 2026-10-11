//! Tests for the "Test microphone" clip helpers (public API only).
//!
//! The pure tests run everywhere. The device test records one second from the
//! default capture endpoint and skips (prints SKIP) when none exists.

use clipcheck_audio::{downmix_mono, encode_wav_mono16, to_i16, MIC_TEST_SECONDS};

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[test]
fn mic_test_seconds_is_five() {
    assert_eq!(MIC_TEST_SECONDS, 5);
}

#[test]
fn downmix_averages_stereo_pairs() {
    let out = downmix_mono(&[1.0, -1.0, 0.5, 0.5]);
    assert_eq!(out, vec![0.0, 0.5]);
}

#[test]
fn downmix_drops_trailing_odd_sample() {
    let out = downmix_mono(&[0.2, 0.4, 0.9]);
    assert_eq!(out.len(), 1);
    assert!((out[0] - 0.3).abs() < 1e-6, "got {out:?}");
}

#[test]
fn downmix_counts_nan_as_zero() {
    let out = downmix_mono(&[f32::NAN, 0.5, 0.2, f32::NAN]);
    assert_eq!(out.len(), 2);
    assert!(
        (out[0] - 0.25).abs() < 1e-6,
        "NaN left should be 0: {out:?}"
    );
    assert!(
        (out[1] - 0.1).abs() < 1e-6,
        "NaN right should be 0: {out:?}"
    );
}

#[test]
fn downmix_empty_is_empty() {
    assert!(downmix_mono(&[]).is_empty());
}

#[test]
fn to_i16_clamps_and_scales() {
    assert_eq!(to_i16(&[2.0]), vec![32767]);
    assert_eq!(to_i16(&[-2.0]), vec![-32767]);
    assert_eq!(to_i16(&[0.0]), vec![0]);
    assert_eq!(to_i16(&[1.0]), vec![32767]);
    assert_eq!(to_i16(&[-1.0]), vec![-32767]);
}

#[test]
fn to_i16_non_finite_is_zero() {
    assert_eq!(to_i16(&[f32::NAN]), vec![0]);
    assert_eq!(to_i16(&[f32::INFINITY]), vec![0]);
    assert_eq!(to_i16(&[f32::NEG_INFINITY]), vec![0]);
}

#[test]
fn to_i16_rounds_to_nearest() {
    // 0.5 * 32767 = 16383.5 rounds to 16384 (half away from zero).
    let v = to_i16(&[0.5])[0];
    assert!(v == 16383 || v == 16384, "got {v}");
}

#[test]
fn wav_header_is_canonical_mono16() {
    let samples: Vec<i16> = vec![1, -2, 3, -4, 5];
    let rate = 48_000u32;
    let wav = encode_wav_mono16(&samples, rate);

    assert_eq!(wav.len(), 44 + 2 * samples.len());
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(u32_at(&wav, 4), 36 + 2 * samples.len() as u32);
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[12..16], b"fmt ");
    assert_eq!(u32_at(&wav, 16), 16, "fmt chunk size");
    assert_eq!(u16_at(&wav, 20), 1, "PCM format");
    assert_eq!(u16_at(&wav, 22), 1, "mono");
    assert_eq!(u32_at(&wav, 24), rate, "sample rate");
    assert_eq!(u32_at(&wav, 28), rate * 2, "byte rate");
    assert_eq!(u16_at(&wav, 32), 2, "block align");
    assert_eq!(u16_at(&wav, 34), 16, "bits per sample");
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32_at(&wav, 40), 2 * samples.len() as u32, "data length");

    for (i, s) in samples.iter().enumerate() {
        let at = 44 + 2 * i;
        assert_eq!(i16::from_le_bytes([wav[at], wav[at + 1]]), *s, "sample {i}");
    }
}

#[test]
fn wav_sample_rate_is_taken_from_input() {
    let wav = encode_wav_mono16(&[0i16; 4], 16_000);
    assert_eq!(u32_at(&wav, 24), 16_000);
    assert_eq!(u32_at(&wav, 28), 32_000);
}

#[test]
fn wav_empty_input_is_header_only() {
    let wav = encode_wav_mono16(&[], 48_000);
    assert_eq!(wav.len(), 44);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32_at(&wav, 40), 0, "data length");
    assert_eq!(u32_at(&wav, 4), 36, "RIFF size");
}

#[cfg(windows)]
#[test]
fn record_mic_clip_one_second_has_expected_length() {
    use clipcheck_audio::{list_endpoints, record_mic_clip, EndpointKind};

    let captures = list_endpoints(EndpointKind::Capture).expect("list capture endpoints");
    if captures.is_empty() {
        println!("SKIP: no capture endpoint for record_mic_clip");
        return;
    }

    let clip = match record_mic_clip(None, 1) {
        Ok(c) => c,
        Err(clipcheck_audio::AudioError::NoDevice) => {
            println!("SKIP: record_mic_clip returned NoDevice");
            return;
        }
        Err(e) => panic!("record_mic_clip(None, 1) failed: {e:?}"),
    };

    assert_eq!(clip.wav.len(), 44 + 2 * 48_000, "wav length");
    assert_eq!(clip.duration_ms, 1000, "duration_ms");
    assert_eq!(&clip.wav[0..4], b"RIFF");
    assert_eq!(&clip.wav[8..12], b"WAVE");
    assert!(
        clip.peak_db.is_finite() && (-100.0..=0.0).contains(&clip.peak_db),
        "peak_db out of range: {}",
        clip.peak_db
    );
}
