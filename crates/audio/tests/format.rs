#![allow(clippy::chunks_exact_to_as_chunks, clippy::approx_constant)]
//! Pure conversion tests: no Windows audio required.

use clipcheck_audio::format::{decode_to_f32, to_stereo, InputFormat, Normalizer, StereoResampler};
use clipcheck_audio::{AudioError, OUT_CHANNELS, OUT_RATE};

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn pcm(bits: u16, channels: u16, rate: u32) -> InputFormat {
    InputFormat {
        sample_rate: rate,
        channels,
        bits_per_sample: bits,
        is_float: false,
    }
}

fn float32(channels: u16, rate: u32) -> InputFormat {
    InputFormat {
        sample_rate: rate,
        channels,
        bits_per_sample: 32,
        is_float: true,
    }
}

#[test]
fn consts() {
    assert_eq!(OUT_RATE, 48_000);
    assert_eq!(OUT_CHANNELS, 2);
}

#[test]
fn decode_i16() {
    let raw: Vec<u8> = [16384i16, -32768]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let out = decode_to_f32(&raw, &pcm(16, 1, 48_000)).expect("valid");
    assert_eq!(out.len(), 2);
    assert!(close(out[0], 0.5, 1e-6), "got {:?}", out);
    assert!(close(out[1], -1.0, 1e-6), "got {:?}", out);
}

#[test]
fn decode_i24() {
    let out = decode_to_f32(&[0x00, 0x00, 0x40], &pcm(24, 1, 48_000)).expect("valid");
    assert_eq!(out.len(), 1);
    assert!(close(out[0], 0.5, 1e-6), "got {:?}", out);
    // Negative full scale: 0x800000 little-endian is [00 00 80] -> -1.0.
    let neg = decode_to_f32(&[0x00, 0x00, 0x80], &pcm(24, 1, 48_000)).expect("valid");
    assert!(close(neg[0], -1.0, 1e-6), "got {:?}", neg);
}

#[test]
fn decode_i32() {
    let raw = 0x4000_0000i32.to_le_bytes();
    let out = decode_to_f32(&raw, &pcm(32, 1, 48_000)).expect("valid");
    assert!(close(out[0], 0.5, 1e-6), "got {:?}", out);
}

#[test]
fn decode_u8() {
    let out = decode_to_f32(&[128u8, 0u8], &pcm(8, 1, 48_000)).expect("valid");
    assert!(close(out[0], 0.0, 1e-6), "got {:?}", out);
    assert!(close(out[1], -1.0, 1e-6), "got {:?}", out);
}

#[test]
fn decode_f32() {
    let raw = 0.25f32.to_le_bytes();
    let out = decode_to_f32(&raw, &float32(1, 48_000)).expect("valid");
    assert!(close(out[0], 0.25, 1e-7), "got {:?}", out);
}

#[test]
fn decode_ignores_trailing_partial_sample() {
    let raw = [0x00u8, 0x40, 0xFF];
    let out = decode_to_f32(&raw, &pcm(16, 1, 48_000)).expect("valid");
    assert_eq!(out.len(), 1);
}

#[test]
fn to_stereo_mono_duplicates() {
    let out = to_stereo(&[0.1, -0.2], 1);
    assert_eq!(out, vec![[0.1, 0.1], [-0.2, -0.2]]);
}

#[test]
fn to_stereo_stereo_passthrough() {
    let out = to_stereo(&[0.1, 0.2, -0.3, 0.4], 2);
    assert_eq!(out, vec![[0.1, 0.2], [-0.3, 0.4]]);
}

#[test]
fn to_stereo_zero_channels_is_empty() {
    assert!(to_stereo(&[0.1, 0.2], 0).is_empty());
}

#[test]
fn to_stereo_6ch_front_only() {
    let out = to_stereo(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], 6);
    assert_eq!(out.len(), 1);
    assert!(
        close(out[0][0], 1.0, 1e-6) && close(out[0][1], 0.0, 1e-6),
        "got {:?}",
        out
    );
}

#[test]
fn to_stereo_6ch_center_mix() {
    let out = to_stereo(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0], 6);
    assert!(
        close(out[0][0], 0.7071, 1e-3) && close(out[0][1], 0.7071, 1e-3),
        "got {:?}",
        out
    );
}

#[test]
fn to_stereo_6ch_lfe_dropped() {
    let out = to_stereo(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 6);
    assert!(
        close(out[0][0], 0.0, 1e-6) && close(out[0][1], 0.0, 1e-6),
        "got {:?}",
        out
    );
}

#[test]
fn to_stereo_3ch_clamps() {
    let out = to_stereo(&[1.0, 1.0, 1.0], 3);
    assert_eq!(out, vec![[1.0, 1.0]]);
}

#[test]
fn resampler_identity_is_exact() {
    let input: Vec<[f32; 2]> = (0..17)
        .map(|i| [i as f32 * 0.01, -(i as f32) * 0.02])
        .collect();
    let mut r = StereoResampler::new(48_000, 48_000);
    assert_eq!(r.process(&input), input);
}

#[test]
fn resampler_constant_downsample() {
    let input = vec![[0.5f32, 0.5f32]; 1000];
    let mut r = StereoResampler::new(48_000, 24_000);
    let out = r.process(&input);
    assert!(out.len() > 100, "got {} frames", out.len());
    // Skip startup frames; everything after must be exactly 0.5.
    for f in out.iter().skip(2) {
        assert!(
            close(f[0], 0.5, 1e-6) && close(f[1], 0.5, 1e-6),
            "got {:?}",
            f
        );
    }
}

#[test]
fn resampler_ramp_upsample_midpoints() {
    let input: Vec<[f32; 2]> = (0..4).map(|i| [i as f32, i as f32]).collect();
    let mut r = StereoResampler::new(48_000, 96_000);
    let out = r.process(&input);
    let expected = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5];
    assert_eq!(out.len(), expected.len(), "got {:?}", out);
    for (f, e) in out.iter().zip(expected.iter()) {
        assert!(close(f[0], *e, 1e-6), "expected {e}, got {:?}", out);
    }
}

#[test]
fn resampler_chunked_equals_oneshot() {
    let ramp: Vec<[f32; 2]> = (0..1000)
        .map(|i| [i as f32 / 1000.0, -(i as f32) / 1000.0])
        .collect();

    let mut one = StereoResampler::new(44_100, 48_000);
    let whole = one.process(&ramp);

    let mut chunked = StereoResampler::new(44_100, 48_000);
    let mut parts = Vec::new();
    for c in ramp.chunks(7) {
        parts.extend(chunked.process(c));
    }

    assert!(
        (whole.len() as i64 - parts.len() as i64).abs() <= 1,
        "lengths differ: oneshot {} chunked {}",
        whole.len(),
        parts.len()
    );
    let n = whole.len().min(parts.len());
    for i in 0..n {
        assert!(
            close(whole[i][0], parts[i][0], 1e-6) && close(whole[i][1], parts[i][1], 1e-6),
            "frame {i} differs: {:?} vs {:?}",
            whole[i],
            parts[i]
        );
    }
}

#[test]
fn resampler_length_44100_to_48000() {
    let input = vec![[0.0f32, 0.0f32]; 44_100];
    let mut r = StereoResampler::new(44_100, 48_000);
    let out = r.process(&input);
    let diff = (out.len() as i64 - 48_000).abs();
    assert!(diff <= 2, "expected ~48000 frames, got {}", out.len());
}

#[test]
fn from_wave_fields_cases() {
    let ok = InputFormat::from_wave_fields(3, 0, 2, 48_000, 32).expect("float32 plain tag");
    assert_eq!(ok.frame_bytes(), 8);
    assert!(ok.is_float);

    let pcm16 = InputFormat::from_wave_fields(0xFFFE, 1, 2, 48_000, 16).expect("extensible PCM");
    assert!(!pcm16.is_float);
    assert_eq!(pcm16.frame_bytes(), 4);

    let flt = InputFormat::from_wave_fields(0xFFFE, 3, 2, 48_000, 32).expect("extensible float");
    assert!(flt.is_float);

    assert!(matches!(
        InputFormat::from_wave_fields(0xFFFE, 3, 2, 48_000, 16),
        Err(AudioError::Format(_))
    ));
    assert!(matches!(
        InputFormat::from_wave_fields(2, 0, 2, 48_000, 16),
        Err(AudioError::Format(_))
    ));
    assert!(InputFormat::from_wave_fields(1, 0, 0, 48_000, 16).is_err());
}

#[test]
fn normalizer_passthrough_stereo_float() {
    let samples = [0.1f32, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8];
    let raw: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut n = Normalizer::new(float32(2, 48_000), OUT_RATE).expect("valid fmt");
    let out = n.push(&raw);
    assert_eq!(out.len(), 8);
    assert_eq!(out, samples.to_vec());
}

#[test]
fn normalizer_mono_16bit_24k_upsample() {
    let frames = 2_400usize;
    let mut raw = Vec::new();
    for i in 0..frames {
        let v = ((i as f32 / frames as f32) * 20_000.0 - 10_000.0) as i16;
        raw.extend_from_slice(&v.to_le_bytes());
    }
    let mut n = Normalizer::new(pcm(16, 1, 24_000), OUT_RATE).expect("valid fmt");
    let out = n.push(&raw);
    assert_eq!(out.len() % 2, 0);
    let stereo_frames = out.len() / 2;
    let expected = 2 * frames as i64;
    assert!(
        (stereo_frames as i64 - expected).abs() <= 2,
        "expected ~{expected} stereo frames, got {stereo_frames}"
    );
    for f in out.chunks_exact(2) {
        assert_eq!(f[0], f[1], "L != R");
    }
}

#[test]
fn normalizer_ignores_partial_trailing_frame() {
    // 2ch 16-bit at 48 kHz: 4 bytes per frame. Feed 2 whole frames + 2 stray bytes.
    let mut raw = Vec::new();
    for v in [1000i16, -1000, 2000, -2000] {
        raw.extend_from_slice(&v.to_le_bytes());
    }
    raw.extend_from_slice(&[0x7F, 0x7F]);
    let mut n = Normalizer::new(pcm(16, 2, OUT_RATE), OUT_RATE).expect("valid fmt");
    let out = n.push(&raw);
    assert_eq!(out.len(), 4, "got {:?}", out);
}

#[test]
fn normalizer_rejects_invalid_format() {
    let bad = InputFormat {
        sample_rate: 48_000,
        channels: 2,
        bits_per_sample: 12,
        is_float: false,
    };
    assert!(Normalizer::new(bad, OUT_RATE).is_err());
}
