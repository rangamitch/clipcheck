//! Regression tests for clipcheck-audio issue #88: `decode_into` must never
//! return a wrong-length buffer for an unvalidated sample size. Public API only,
//! pure conversion, no Windows audio needed.

use clipcheck_audio::format::{decode_into, decode_to_f32, InputFormat, Normalizer};
use clipcheck_audio::AudioError;

fn pcm(bits: u16, channels: u16) -> InputFormat {
    InputFormat {
        sample_rate: 48_000,
        channels,
        bits_per_sample: bits,
        is_float: false,
    }
}

/// Sample sizes that `validate` rejects and `decode_into` does not decode.
const UNSUPPORTED_PCM_BITS: [u16; 4] = [0, 12, 20, 64];

#[test]
fn issue_88_unsupported_pcm_sample_size_is_an_error() {
    // The old fallback returned one f32 per raw byte: 8 bytes gave 8 samples.
    // decode_to_f32 now validates the format and returns Err instead.
    let raw = [0u8; 8];
    for bits in UNSUPPORTED_PCM_BITS {
        let r = decode_to_f32(&raw, &pcm(bits, 1));
        assert!(
            matches!(r, Err(AudioError::Format(_))),
            "bits {bits}: expected AudioError::Format, got {r:?}"
        );
    }
}

#[test]
fn issue_88_decode_into_clears_previous_contents_for_unsupported_size() {
    let mut out = vec![1.0f32, 2.0, 3.0];
    decode_into(&[0u8; 8], &pcm(12, 1), &mut out);
    assert!(out.is_empty(), "stale samples left behind: {out:?}");
}

#[test]
fn issue_88_supported_16bit_path_still_has_one_sample_per_sample() {
    // 8 bytes of mono 16-bit PCM is 4 samples, not 8.
    let out = decode_to_f32(&[0u8; 8], &pcm(16, 1)).expect("valid");
    assert_eq!(out.len(), 4);
}

#[test]
fn issue_88_unsupported_sample_size_is_rejected_by_normalizer() {
    let r = Normalizer::new(pcm(12, 1), 48_000);
    assert!(
        matches!(r, Err(AudioError::Format(_))),
        "Normalizer::new must reject 12-bit PCM"
    );
}

// SKIPPED #91: the WAVE_FORMAT_* constants are now pub(crate) and read_mix_format
// is private and Windows-only, so there is no public-API behaviour to test.
