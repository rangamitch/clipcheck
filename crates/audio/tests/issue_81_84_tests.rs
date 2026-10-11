//! Tests for issues #81 (unsigned 8-bit PCM silence) and #84 (capture errors
//! must not end the stream silently). Pure conversion tests plus type-level
//! checks: no audio device is opened.

use clipcheck_audio::format::{decode_to_f32, Normalizer};
use clipcheck_audio::{AudioError, InputFormat, OUT_CHANNELS, OUT_RATE};

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

fn all_formats() -> Vec<InputFormat> {
    let mut v = Vec::new();
    for &rate in &[44_100u32, 48_000, 96_000] {
        for &ch in &[1u16, 2] {
            for &bits in &[8u16, 16, 24, 32] {
                v.push(pcm(bits, ch, rate));
            }
            v.push(float32(ch, rate));
        }
    }
    v
}

/// Expected output sample count (interleaved stereo f32) for `frames` input
/// frames resampled from `rate` to OUT_RATE, allowing +/- 2 frames.
fn assert_len_close(actual_samples: usize, in_frames: usize, rate: u32) {
    let expected_frames = in_frames as f64 * OUT_RATE as f64 / rate as f64;
    let actual_frames = (actual_samples / OUT_CHANNELS) as f64;
    assert!(
        (actual_frames - expected_frames).abs() <= 2.0,
        "rate {rate}: expected ~{expected_frames} frames, got {actual_frames}"
    );
    assert_eq!(
        actual_samples % OUT_CHANNELS,
        0,
        "output is not whole stereo frames"
    );
}

// ---- #81: silence bytes -------------------------------------------------

#[test]
fn silence_bytes_8bit_pcm_is_0x80() {
    let f = pcm(8, 1, 48_000);
    assert_eq!(f.silence_bytes(3), vec![0x80; 3]);
    let s = pcm(8, 2, 48_000);
    assert_eq!(s.silence_bytes(2), vec![0x80; 4]);
}

#[test]
fn silence_bytes_signed_and_float_are_zero() {
    for f in [
        pcm(16, 2, 48_000),
        pcm(24, 1, 48_000),
        pcm(32, 2, 48_000),
        float32(2, 48_000),
    ] {
        let b = f.silence_bytes(4);
        assert_eq!(b.len(), 4 * f.frame_bytes(), "length for {f:?}");
        assert!(b.iter().all(|&x| x == 0), "non-zero silence for {f:?}");
    }
}

#[test]
fn silence_bytes_zero_frames_is_empty() {
    for f in all_formats() {
        assert!(f.silence_bytes(0).is_empty(), "{f:?}");
    }
}

// ---- #81: 8-bit decode table --------------------------------------------

#[test]
fn decode_8bit_table() {
    let f = pcm(8, 1, 48_000);
    let out = decode_to_f32(&[0x80, 0x00, 0xFF, 0x40], &f).expect("valid");
    assert_eq!(out.len(), 4);
    assert_eq!(out[0], 0.0, "0x80 must decode to exactly 0.0");
    assert_eq!(out[1], -1.0, "0x00 must decode to -1.0");
    assert!((out[2] - 0.992).abs() < 0.001, "0xFF decoded to {}", out[2]);
    assert_eq!(out[3], -0.5, "0x40 must decode to -0.5");
}

#[test]
fn decode_8bit_via_normalizer_passthrough_rate() {
    // Equal in/out rate: the resampler passes frames through unchanged, so
    // mono 8-bit 0x80 must come out as stereo 0.0 pairs.
    let mut n = Normalizer::new(pcm(8, 1, OUT_RATE), OUT_RATE).unwrap();
    let out = n.push(&[0x80, 0x00]);
    assert_eq!(
        out.len(),
        4,
        "two mono frames -> four stereo samples: {out:?}"
    );
    assert_eq!(out[0], 0.0);
    assert_eq!(out[1], 0.0);
    assert_eq!(out[2], -1.0);
    assert_eq!(out[3], -1.0);
}

// ---- #81: Normalizer::push_silence --------------------------------------

#[test]
fn push_silence_is_exactly_zero_for_every_format() {
    for f in all_formats() {
        let mut n = Normalizer::new(f, OUT_RATE).unwrap();
        let out = n.push_silence(4_800);
        assert!(!out.is_empty(), "no output for {f:?}");
        assert!(
            out.iter().all(|&s| s == 0.0),
            "non-zero sample for {f:?}: first bad = {:?}",
            out.iter().find(|&&s| s != 0.0)
        );
        assert_len_close(out.len(), 4_800, f.sample_rate);
    }
}

#[test]
fn push_silence_zero_frames_is_empty() {
    for f in all_formats() {
        let mut n = Normalizer::new(f, OUT_RATE).unwrap();
        assert!(n.push_silence(0).is_empty(), "{f:?}");
    }
}

#[test]
fn push_silence_huge_count_is_zero_and_sized() {
    let f = pcm(8, 1, 96_000);
    let mut n = Normalizer::new(f, OUT_RATE).unwrap();
    let out = n.push_silence(1_000_000);
    assert!(
        out.iter().all(|&s| s == 0.0),
        "non-zero sample in huge silence"
    );
    assert_len_close(out.len(), 1_000_000, 96_000);
}

#[test]
fn push_silence_mono_upmix_is_zero_stereo() {
    let mut n = Normalizer::new(pcm(8, 1, OUT_RATE), OUT_RATE).unwrap();
    let out = n.push_silence(100);
    assert_eq!(out.len(), 200, "mono 100 frames -> 200 stereo samples");
    assert!(out.iter().all(|&s| s == 0.0));
}

#[test]
fn push_silence_is_consistent_across_calls() {
    // Silence pushed in pieces must match one large push in total length.
    let f = pcm(8, 2, 44_100);
    let mut a = Normalizer::new(f, OUT_RATE).unwrap();
    let mut total = 0usize;
    for _ in 0..10 {
        let out = a.push_silence(441);
        assert!(out.iter().all(|&s| s == 0.0));
        total += out.len();
    }
    assert_len_close(total, 4_410, 44_100);
}

// ---- #84: error variants -------------------------------------------------

#[test]
fn thread_error_variants_display_and_debug() {
    for e in [AudioError::ThreadSpawn, AudioError::ThreadExited] {
        assert!(!format!("{e}").is_empty(), "empty Display for {e:?}");
        assert!(!format!("{e:?}").is_empty(), "empty Debug");
    }
    assert!(format!("{}", AudioError::ThreadSpawn).contains("thread"));
    assert!(format!("{}", AudioError::ThreadExited).contains("thread"));
}

#[test]
fn thread_error_variants_are_distinct() {
    let spawn = AudioError::ThreadSpawn;
    let exited = AudioError::ThreadExited;
    assert_ne!(spawn, exited);
    assert_ne!(spawn, AudioError::Unsupported("x"));
    assert_ne!(exited, AudioError::Unsupported("x"));
    assert_ne!(spawn, AudioError::NoDevice);
    assert_ne!(exited, AudioError::NoDevice);
    assert_eq!(spawn.clone(), AudioError::ThreadSpawn);
}

#[cfg(windows)]
#[test]
fn last_error_methods_exist_with_expected_signatures() {
    // Compile-time check only: no device is opened.
    use clipcheck_audio::{MicMeter, Microphone, ProcessLoopback, SystemMix};
    let _: fn(&ProcessLoopback) -> Option<AudioError> = ProcessLoopback::last_error;
    let _: fn(&SystemMix) -> Option<AudioError> = SystemMix::last_error;
    let _: fn(&Microphone) -> Option<AudioError> = Microphone::last_error;
    let _: fn(&MicMeter) -> Option<AudioError> = MicMeter::last_error;
}
