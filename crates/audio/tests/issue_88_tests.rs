//! Regression tests for clipcheck-audio issue #88: `decode_to_f32` is fallible,
//! validates the format, and never returns a wrong-length buffer. Public API
//! only, pure conversion, no Windows audio needed.

use clipcheck_audio::format::{decode_to_f32, InputFormat};
use clipcheck_audio::AudioError;

fn fmt(bits: u16, is_float: bool) -> InputFormat {
    InputFormat {
        sample_rate: 48_000,
        channels: 1,
        bits_per_sample: bits,
        is_float,
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-6
}

#[test]
fn issue_88_12_bit_pcm_returns_format_error() {
    let raw = [0u8; 8];
    let r = decode_to_f32(&raw, &fmt(12, false));
    assert!(
        matches!(r, Err(AudioError::Format(_))),
        "12-bit PCM must be rejected, got {r:?}"
    );
}

#[test]
fn issue_88_zero_channels_returns_format_error() {
    let f = InputFormat {
        channels: 0,
        ..fmt(16, false)
    };
    let r = decode_to_f32(&[0u8; 8], &f);
    assert!(matches!(r, Err(AudioError::Format(_))), "got {r:?}");
}

#[test]
fn issue_88_float_with_non_32_bits_returns_format_error() {
    let r = decode_to_f32(&[0u8; 8], &fmt(16, true));
    assert!(matches!(r, Err(AudioError::Format(_))), "got {r:?}");
}

#[test]
fn issue_88_16_bit_pcm_length_and_values() {
    // Four 16-bit samples: 16384 (0.5), -32768 (-1.0), 0 (0.0), 32767 (~1.0).
    let raw: Vec<u8> = [16384i16, -32768, 0, 32767]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let out = decode_to_f32(&raw, &fmt(16, false)).expect("valid");
    assert_eq!(out.len(), raw.len() / 2);
    assert!(close(out[0], 0.5), "got {out:?}");
    assert!(close(out[1], -1.0), "got {out:?}");
    assert!(close(out[2], 0.0), "got {out:?}");
    assert!((out[3] - 32767.0 / 32768.0).abs() <= 1e-6, "got {out:?}");
}

#[test]
fn issue_88_24_bit_pcm_length_and_values() {
    // Three 24-bit samples: 0x400000 (0.5), 0x800000 (-1.0), 0x000000 (0.0).
    let raw = [0x00u8, 0x00, 0x40, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00];
    let out = decode_to_f32(&raw, &fmt(24, false)).expect("valid");
    assert_eq!(out.len(), raw.len() / 3);
    assert!(close(out[0], 0.5), "got {out:?}");
    assert!(close(out[1], -1.0), "got {out:?}");
    assert!(close(out[2], 0.0), "got {out:?}");
}

#[test]
fn issue_88_32_bit_pcm_length_and_values() {
    let raw: Vec<u8> = [0x4000_0000i32, i32::MIN, 0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let out = decode_to_f32(&raw, &fmt(32, false)).expect("valid");
    assert_eq!(out.len(), raw.len() / 4);
    assert!(close(out[0], 0.5), "got {out:?}");
    assert!(close(out[1], -1.0), "got {out:?}");
    assert!(close(out[2], 0.0), "got {out:?}");
}

#[test]
fn issue_88_32_bit_float_length_and_values() {
    let raw: Vec<u8> = [0.25f32, -0.75, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let out = decode_to_f32(&raw, &fmt(32, true)).expect("valid");
    assert_eq!(out.len(), raw.len() / 4);
    assert!(close(out[0], 0.25), "got {out:?}");
    assert!(close(out[1], -0.75), "got {out:?}");
    assert!(close(out[2], 0.0), "got {out:?}");
}
