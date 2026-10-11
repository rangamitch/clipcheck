//! Fixed-length microphone clip capture for the "Test microphone" feature, plus
//! the pure helpers that turn captured float audio into a 16-bit mono WAV.

#[cfg(windows)]
use std::sync::mpsc::RecvTimeoutError;
#[cfg(windows)]
use std::time::{Duration, Instant};

use crate::{AudioError, OUT_CHANNELS};
#[cfg(windows)]
use crate::{Microphone, OUT_RATE};

/// Length of a microphone test clip in seconds.
pub const MIC_TEST_SECONDS: u32 = 5;

/// Size of the canonical WAV header written by [`encode_wav_mono16`].
const WAV_HEADER_BYTES: u32 = 44;

/// Largest sample count whose data chunk and RIFF size still fit in `u32`.
const MAX_WAV_SAMPLES: usize = ((u32::MAX - WAV_HEADER_BYTES) / 2) as usize;

/// A recorded microphone clip, ready to play back.
#[derive(Clone, Debug, PartialEq)]
pub struct MicClip {
    /// Complete WAV file: 16-bit PCM, mono, [`OUT_RATE`] Hz.
    pub wav: Vec<u8>,
    /// Clip length in milliseconds.
    pub duration_ms: u32,
    /// Peak absolute level over the whole clip, in dBFS (-100 = silence).
    pub peak_db: f32,
}

/// Averages each interleaved stereo frame into one mono sample. Non-finite
/// input samples count as 0.0. A trailing odd sample is dropped.
pub fn downmix_mono(interleaved_stereo: &[f32]) -> Vec<f32> {
    interleaved_stereo
        .as_chunks::<OUT_CHANNELS>()
        .0
        .iter()
        .map(|frame| {
            let sum: f32 = frame
                .iter()
                .map(|&s| if s.is_finite() { s } else { 0.0 })
                .sum();
            sum / OUT_CHANNELS as f32
        })
        .collect()
}

/// Converts float samples to 16-bit PCM: clamped to -1.0..=1.0, scaled by
/// 32767 and rounded to nearest. Non-finite samples become 0.
pub fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            if s.is_finite() {
                (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
            } else {
                0
            }
        })
        .collect()
}

/// Encodes mono 16-bit samples as a canonical 44-byte-header PCM WAV file.
/// Sample counts too large for a WAV `u32` size field are truncated.
pub fn encode_wav_mono16(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let samples = &samples[..samples.len().min(MAX_WAV_SAMPLES)];
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(WAV_HEADER_BYTES as usize + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&data_len.saturating_add(WAV_HEADER_BYTES - 8).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&sample_rate.saturating_mul(2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Records `seconds` of audio from a capture endpoint (`None` = default) and
/// returns it as a mono 16-bit WAV. Gives up after `seconds + 5` s of wall time.
///
/// Returns the capture error that stopped the microphone when there is one,
/// `AudioError::NoAudio` when the deadline passes or the stream ends early
/// without a recorded error, and `AudioError::NoDevice` when no endpoint matches.
#[cfg(windows)]
pub fn record_mic_clip(device: Option<String>, seconds: u32) -> Result<MicClip, AudioError> {
    let (mut mic, chunks) = Microphone::start(device)?;
    let target_samples = seconds as usize * OUT_RATE as usize * OUT_CHANNELS;
    let deadline = Instant::now() + Duration::from_secs(u64::from(seconds) + 5);
    let mut samples: Vec<f32> = Vec::with_capacity(target_samples.min(1 << 22));

    let mut failed = false;
    while samples.len() < target_samples {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match chunks.recv_timeout(remaining) {
            Ok(chunk) => samples.extend_from_slice(&chunk.samples),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                failed = true;
                break;
            }
        }
    }

    // Stop joins the capture thread, so any error it recorded is visible afterwards.
    mic.stop();
    if failed {
        return Err(mic.last_error().unwrap_or(AudioError::NoAudio));
    }

    samples.truncate(target_samples);
    let frames = samples.len() / OUT_CHANNELS;
    let mono = downmix_mono(&samples);
    let peak_db = crate::meter::peak_db(&mono);
    let wav = encode_wav_mono16(&to_i16(&mono), OUT_RATE);
    let duration_ms = (frames as u64 * 1000 / u64::from(OUT_RATE)) as u32;
    Ok(MicClip {
        wav,
        duration_ms,
        peak_db,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OUT_RATE;

    #[test]
    fn downmix_averages_pairs_and_drops_odd_tail() {
        assert_eq!(downmix_mono(&[1.0, 0.0, 0.5, 0.5, 0.3]), vec![0.5, 0.5]);
        assert!(downmix_mono(&[f32::NAN, 1.0]).iter().all(|s| s.is_finite()));
    }

    #[test]
    fn to_i16_clamps_scales_and_zeroes_non_finite() {
        assert_eq!(
            to_i16(&[2.0, -2.0, 0.0, f32::NAN]),
            vec![32767, -32767, 0, 0]
        );
    }

    #[test]
    fn wav_header_is_canonical() {
        let wav = encode_wav_mono16(&[1, -1, 0], OUT_RATE);
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]), 36 + 6);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 6);
    }
}
