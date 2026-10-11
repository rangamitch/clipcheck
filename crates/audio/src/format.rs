//! Pure sample-format, channel and rate conversion. No Windows calls.
//!
//! Every captured stream is normalised to interleaved stereo `f32` at
//! [`OUT_RATE`] (48 kHz): decode -> channel mix -> linear resample.

use crate::AudioError;

/// Output sample rate of every chunk produced by this crate.
pub const OUT_RATE: u32 = 48_000;
/// Output channel count (interleaved stereo).
pub const OUT_CHANNELS: usize = 2;

/// WAVE_FORMAT_PCM. Single definition shared with `loopback.rs`.
const WAVE_FORMAT_PCM: u16 = 1;
/// WAVE_FORMAT_IEEE_FLOAT. Single definition shared with `loopback.rs`.
pub(crate) const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// WAVE_FORMAT_EXTENSIBLE. Single definition shared with `loopback.rs`.
pub(crate) const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Layout of a captured stream, as reported by WASAPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputFormat {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub channels: u16,
    /// Container size per sample (8, 16, 24 or 32 for PCM; 32 for float).
    pub bits_per_sample: u16,
    /// True for IEEE float samples, false for integer PCM.
    pub is_float: bool,
}

impl InputFormat {
    /// Builds a format from WAVEFORMATEX fields.
    ///
    /// For `WAVE_FORMAT_EXTENSIBLE` the caller passes the low 16 bits of the
    /// SubFormat GUID's Data1 as `sub_format_tag` (1 = PCM, 3 = float); for any
    /// other format tag `sub_format_tag` is ignored.
    pub fn from_wave_fields(
        format_tag: u16,
        sub_format_tag: u16,
        channels: u16,
        sample_rate: u32,
        bits_per_sample: u16,
    ) -> Result<Self, AudioError> {
        let tag = if format_tag == WAVE_FORMAT_EXTENSIBLE {
            sub_format_tag
        } else {
            format_tag
        };
        let is_float = match tag {
            WAVE_FORMAT_PCM => false,
            WAVE_FORMAT_IEEE_FLOAT => true,
            _ => return Err(AudioError::Format("wave format tag is not PCM or float")),
        };
        let fmt = Self {
            sample_rate,
            channels,
            bits_per_sample,
            is_float,
        };
        fmt.validate()?;
        Ok(fmt)
    }

    /// Checks that the layout is one this crate can decode.
    pub fn validate(&self) -> Result<(), AudioError> {
        if self.channels == 0 || self.sample_rate == 0 {
            return Err(AudioError::Format("zero channels or sample rate"));
        }
        let sample_size_ok = if self.is_float {
            self.bits_per_sample == 32
        } else {
            matches!(self.bits_per_sample, 8 | 16 | 24 | 32)
        };
        if sample_size_ok {
            Ok(())
        } else {
            Err(AudioError::Format("unsupported sample size"))
        }
    }

    /// Bytes per interleaved frame (all channels, one sample each).
    pub fn frame_bytes(&self) -> usize {
        (self.bits_per_sample as usize / 8) * self.channels as usize
    }

    /// Raw bytes that decode to silence (0.0) for `frames` frames of this format.
    ///
    /// Unsigned 8-bit PCM is centred on `0x80`, so its silence byte is `0x80`;
    /// every other supported layout (signed PCM and float) is silent at all-zero bytes.
    pub fn silence_bytes(&self, frames: usize) -> Vec<u8> {
        let fill = if !self.is_float && self.bits_per_sample == 8 {
            0x80
        } else {
            0x00
        };
        vec![fill; frames * self.frame_bytes()]
    }
}

/// Decodes interleaved raw bytes in `fmt` into `out` as `f32` in [-1.0, 1.0].
///
/// `out` is cleared first and then filled, so its allocation is reused across
/// calls. Trailing bytes that do not form a whole sample are ignored.
///
/// If `fmt` has not passed [`InputFormat::validate`] and its sample size is not
/// one this function decodes, `out` is left empty. It is never filled with a
/// wrong-length buffer. Callers should validate first (as [`Normalizer::new`] does).
pub fn decode_into(raw: &[u8], fmt: &InputFormat, out: &mut Vec<f32>) {
    out.clear();
    match (fmt.is_float, fmt.bits_per_sample) {
        (true, _) => out.extend(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        ),
        (false, 8) => out.extend(raw.iter().map(|&b| (b as f32 - 128.0) / 128.0)),
        (false, 16) => out.extend(
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0),
        ),
        (false, 24) => out.extend(raw.as_chunks::<3>().0.iter().map(|b| {
            let v = (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16);
            // Sign-extend from 24 bits.
            let v = (v << 8) >> 8;
            v as f32 / 8_388_608.0
        })),
        (false, 32) => out.extend(
            raw.as_chunks::<4>()
                .0
                .iter()
                .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0),
        ),
        // Unvalidated sample size: produce no samples (out is already cleared).
        // Never resize to raw.len(), which would be one f32 per byte.
        _ => {}
    }
}

/// Decodes interleaved raw bytes in `fmt` to a new `Vec<f32>`.
///
/// Validates `fmt` first and returns `AudioError::Format` if it is not a layout
/// this crate can decode, so a bad format is never turned into a wrong-length buffer.
/// Convenience wrapper over [`decode_into`]; hot paths should reuse a buffer.
pub fn decode_to_f32(raw: &[u8], fmt: &InputFormat) -> Result<Vec<f32>, AudioError> {
    fmt.validate()?;
    let mut out = Vec::new();
    decode_into(raw, fmt, &mut out);
    Ok(out)
}

/// Mixes interleaved `channels`-wide `f32` frames down (or up) to stereo,
/// writing into `out` (cleared first, allocation reused).
///
/// Mono is duplicated to both sides. Channels 0 and 1 are the front pair; a
/// centre channel (index 2) is added to both sides at -3 dB, and for layouts
/// with at least six channels indices 4 and 5 (surround) are added at -3 dB.
/// The LFE channel (index 3) is dropped. Output is clamped to [-1, 1].
pub fn to_stereo_into(interleaved: &[f32], channels: usize, out: &mut Vec<[f32; 2]>) {
    const K: f32 = std::f32::consts::FRAC_1_SQRT_2;
    out.clear();
    match channels {
        0 => {}
        1 => out.extend(interleaved.iter().map(|&s| [s, s])),
        2 => out.extend(interleaved.as_chunks::<2>().0.iter().map(|c| [c[0], c[1]])),
        n => out.extend(interleaved.chunks_exact(n).map(|f| {
            let center = K * f[2];
            let mut l = f[0] + center;
            let mut r = f[1] + center;
            if n >= 6 {
                l += K * f[4];
                r += K * f[5];
            }
            [l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0)]
        })),
    }
}

/// Mixes interleaved `channels`-wide `f32` frames down (or up) to a new stereo
/// `Vec`. Convenience wrapper over [`to_stereo_into`].
pub fn to_stereo(interleaved: &[f32], channels: usize) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    to_stereo_into(interleaved, channels, &mut out);
    out
}

/// Streaming linear-interpolation resampler for stereo frames.
///
/// State carries across calls, so chunk boundaries are seamless. Output lags
/// input by at most one frame. Equal rates pass through unchanged.
#[derive(Debug)]
pub struct StereoResampler {
    step: f64,
    pos: f64,
    buf: Vec<[f32; 2]>,
    passthrough: bool,
}

impl StereoResampler {
    /// Creates a resampler from `in_rate` to `out_rate` Hz (both non-zero).
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            buf: Vec::new(),
            passthrough: in_rate == out_rate,
        }
    }

    /// Feeds input frames and returns the output frames that are ready.
    pub fn process(&mut self, input: &[[f32; 2]]) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        self.process_with(input, |f| out.push(f));
        out
    }

    /// Feeds input frames and passes each ready output frame to `emit`, in
    /// order. Allocation-free apart from the internal input history, which is
    /// reused across calls.
    fn process_with(&mut self, input: &[[f32; 2]], mut emit: impl FnMut([f32; 2])) {
        if self.passthrough {
            for &f in input {
                emit(f);
            }
            return;
        }
        self.buf.extend_from_slice(input);
        while (self.pos as usize) + 1 < self.buf.len() {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            let a = self.buf[i];
            let b = self.buf[i + 1];
            emit([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            self.pos += self.step;
        }
        // Drop frames no later output can reference.
        let consumed = self.pos as usize;
        if consumed > 0 {
            self.buf.drain(..consumed);
            self.pos -= consumed as f64;
        }
    }
}

/// Full per-source conversion: raw bytes -> stereo f32 at a fixed output rate.
///
/// Holds scratch buffers that are reused across calls, so per packet the only
/// allocation is the returned `Vec<f32>`, which the caller takes ownership of.
#[derive(Debug)]
pub struct Normalizer {
    fmt: InputFormat,
    resampler: StereoResampler,
    /// Decoded interleaved samples for the current packet.
    decoded: Vec<f32>,
    /// Stereo frames for the current packet; only used when channels != 2.
    stereo: Vec<[f32; 2]>,
}

impl Normalizer {
    /// Creates a normaliser for `fmt`, producing `out_rate` Hz stereo.
    pub fn new(fmt: InputFormat, out_rate: u32) -> Result<Self, AudioError> {
        fmt.validate()?;
        Ok(Self {
            fmt,
            resampler: StereoResampler::new(fmt.sample_rate, out_rate),
            decoded: Vec::new(),
            stereo: Vec::new(),
        })
    }

    /// Converts raw interleaved bytes in the input format into interleaved
    /// stereo `f32` samples at the output rate.
    pub fn push(&mut self, raw: &[u8]) -> Vec<f32> {
        let frame = self.fmt.frame_bytes();
        let whole = raw.len() / frame * frame;
        decode_into(&raw[..whole], &self.fmt, &mut self.decoded);
        self.convert_decoded()
    }

    /// Converts `frames` frames of silence in the input format. Uses the
    /// format's own silence level, so unsigned 8-bit PCM decodes to 0.0.
    ///
    /// Every supported format decodes its silence bytes to +0.0, so this fills
    /// the decode buffer with zeros directly instead of building silence bytes
    /// first. The output is identical to `push(&fmt.silence_bytes(frames))`.
    pub fn push_silence(&mut self, frames: usize) -> Vec<f32> {
        self.decoded.clear();
        self.decoded
            .resize(frames * self.fmt.channels as usize, 0.0);
        self.convert_decoded()
    }

    /// Runs mix and resample on `self.decoded` and returns the new output.
    fn convert_decoded(&mut self) -> Vec<f32> {
        let channels = self.fmt.channels as usize;
        let frames = self.decoded.len() / channels.max(1);
        // Output frames are about frames * out/in, plus one frame of carry-over.
        let cap = ((frames as f64) / self.resampler.step).ceil() as usize * 2 + 4;
        let mut out = Vec::with_capacity(cap);
        let mut push = |f: [f32; 2]| {
            out.push(f[0]);
            out.push(f[1]);
        };
        if channels == 2 {
            // Interleaved L,R pairs already have the stereo frame layout.
            let pairs: &[[f32; 2]] = self.decoded.as_chunks::<2>().0;
            self.resampler.process_with(pairs, &mut push);
        } else {
            to_stereo_into(&self.decoded, channels, &mut self.stereo);
            self.resampler.process_with(&self.stereo, &mut push);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(bits: u16, is_float: bool) -> InputFormat {
        InputFormat {
            sample_rate: OUT_RATE,
            channels: 2,
            bits_per_sample: bits,
            is_float,
        }
    }

    #[test]
    fn unsigned_8bit_silence_decodes_to_zero() {
        let f = fmt(8, false);
        assert_eq!(f.silence_bytes(1), vec![0x80, 0x80]);
        assert_eq!(
            decode_to_f32(&[0x80], &InputFormat { channels: 1, ..f }).expect("valid"),
            vec![0.0]
        );
        let mut n = Normalizer::new(f, OUT_RATE).expect("valid");
        let out = n.push_silence(4);
        assert_eq!(out.len(), 8);
        assert!(
            out.iter().all(|&s| s == 0.0),
            "silence must be 0.0, got {out:?}"
        );
    }

    #[test]
    fn unsigned_8bit_extremes_decode_to_full_range() {
        let f = InputFormat {
            channels: 1,
            ..fmt(8, false)
        };
        let v = decode_to_f32(&[0x00, 0xFF], &f).expect("valid");
        assert_eq!(v[0], -1.0);
        assert!((v[1] - 127.0 / 128.0).abs() < 1e-6);
    }

    #[test]
    fn signed_and_float_silence_is_all_zero_bytes() {
        assert_eq!(fmt(16, false).silence_bytes(1), vec![0u8; 4]);
        assert_eq!(fmt(24, false).silence_bytes(1), vec![0u8; 6]);
        assert_eq!(fmt(32, true).silence_bytes(1), vec![0u8; 8]);
        let mut n = Normalizer::new(fmt(32, true), OUT_RATE).expect("valid");
        assert!(n.push_silence(3).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn decodes_i16_and_mono_duplicates() {
        let fmt = InputFormat {
            sample_rate: OUT_RATE,
            channels: 1,
            bits_per_sample: 16,
            is_float: false,
        };
        let raw = 16_384i16.to_le_bytes();
        let mut n = Normalizer::new(fmt, OUT_RATE).expect("valid");
        assert_eq!(n.push(&raw), vec![0.5, 0.5]);
    }

    /// Straightforward reference: the pre-optimisation algorithm, written with
    /// fresh allocations at every step. The optimised `Normalizer` must match it
    /// bit for bit.
    struct Reference {
        fmt: InputFormat,
        step: f64,
        pos: f64,
        buf: Vec<[f32; 2]>,
        passthrough: bool,
    }

    impl Reference {
        fn new(fmt: InputFormat, out_rate: u32) -> Self {
            Self {
                fmt,
                step: fmt.sample_rate as f64 / out_rate as f64,
                pos: 0.0,
                buf: Vec::new(),
                passthrough: fmt.sample_rate == out_rate,
            }
        }

        fn process(&mut self, input: &[[f32; 2]]) -> Vec<[f32; 2]> {
            if self.passthrough {
                return input.to_vec();
            }
            self.buf.extend_from_slice(input);
            let mut out = Vec::new();
            while (self.pos as usize) + 1 < self.buf.len() {
                let i = self.pos as usize;
                let t = (self.pos - i as f64) as f32;
                let a = self.buf[i];
                let b = self.buf[i + 1];
                out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
                self.pos += self.step;
            }
            let consumed = self.pos as usize;
            if consumed > 0 {
                self.buf.drain(..consumed);
                self.pos -= consumed as f64;
            }
            out
        }

        fn push(&mut self, raw: &[u8]) -> Vec<f32> {
            let frame = self.fmt.frame_bytes();
            let whole = raw.len() / frame * frame;
            let decoded = decode_to_f32(&raw[..whole], &self.fmt).expect("valid");
            let stereo = reference_to_stereo(&decoded, self.fmt.channels as usize);
            self.process(&stereo).into_iter().flatten().collect()
        }

        fn push_silence(&mut self, frames: usize) -> Vec<f32> {
            let raw = self.fmt.silence_bytes(frames);
            self.push(&raw)
        }
    }

    /// Verbatim copy of the original channel mix, with fresh allocations.
    fn reference_to_stereo(interleaved: &[f32], channels: usize) -> Vec<[f32; 2]> {
        const K: f32 = std::f32::consts::FRAC_1_SQRT_2;
        match channels {
            0 => Vec::new(),
            1 => interleaved.iter().map(|&s| [s, s]).collect(),
            2 => interleaved
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| [c[0], c[1]])
                .collect(),
            n => interleaved
                .chunks_exact(n)
                .map(|f| {
                    let center = K * f[2];
                    let mut l = f[0] + center;
                    let mut r = f[1] + center;
                    if n >= 6 {
                        l += K * f[4];
                        r += K * f[5];
                    }
                    [l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0)]
                })
                .collect(),
        }
    }

    /// Deterministic byte source (LCG) so failures are reproducible.
    struct Lcg(u64);

    impl Lcg {
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n)
                .map(|_| {
                    self.0 = self
                        .0
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    (self.0 >> 33) as u8
                })
                .collect()
        }
    }

    fn assert_bits_eq(got: &[f32], want: &[f32], what: &str) {
        assert_eq!(got.len(), want.len(), "{what}: length");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(g.to_bits(), w.to_bits(), "{what}: sample {i}: {g} vs {w}");
        }
    }

    /// Feeds the same byte stream to the optimised and reference normalisers,
    /// with chunk sizes (in frames) that split frames across calls and
    /// interleaved silence, and compares every output bit for bit.
    fn check_equivalence(fmt: InputFormat, out_rate: u32, seed: u64) {
        let frame = fmt.frame_bytes();
        let mut fast = Normalizer::new(fmt, out_rate).expect("valid");
        let mut slow = Reference::new(fmt, out_rate);
        let mut rng = Lcg(seed);
        // Chunk sizes in frames, cycled; 1 and odd sizes exercise partial
        // resampler state and frame boundaries.
        let sizes = [1usize, 7, 480, 3, 1024, 2, 960, 13];
        for round in 0..24 {
            let frames = sizes[round % sizes.len()];
            let mut raw = rng.bytes(frames * frame);
            if round % 5 == 4 {
                // A trailing partial frame must be ignored by both.
                raw.push(0xAB);
            }
            let want = slow.push(&raw);
            let got = fast.push(&raw);
            assert_bits_eq(
                &got,
                &want,
                &format!("{fmt:?} out {out_rate} round {round}"),
            );
            if round % 4 == 3 {
                let n = sizes[(round + 2) % sizes.len()];
                let want = slow.push_silence(n);
                let got = fast.push_silence(n);
                assert_bits_eq(&got, &want, &format!("{fmt:?} silence round {round}"));
            }
        }
    }

    #[test]
    fn normalizer_matches_reference_bit_for_bit() {
        let formats = [
            (8u16, false),
            (16, false),
            (24, false),
            (32, false),
            (32, true),
        ];
        let rates = [OUT_RATE, 44_100, 96_000, 16_000];
        let channel_counts = [1u16, 2, 3, 6, 8];
        let mut seed = 1u64;
        for &(bits, is_float) in &formats {
            for &channels in &channel_counts {
                for &rate in &rates {
                    seed = seed.wrapping_add(7919);
                    let fmt = InputFormat {
                        sample_rate: rate,
                        channels,
                        bits_per_sample: bits,
                        is_float,
                    };
                    check_equivalence(fmt, OUT_RATE, seed);
                }
            }
        }
    }

    #[test]
    fn stereo_passthrough_rate_matches_reference() {
        // Float stereo at the output rate: the decode-straight-to-output path.
        for seed in 0..4u64 {
            let fmt = InputFormat {
                sample_rate: OUT_RATE,
                channels: 2,
                bits_per_sample: 32,
                is_float: true,
            };
            check_equivalence(fmt, OUT_RATE, 100 + seed);
        }
    }

    #[test]
    fn push_silence_matches_reference_silence_bytes() {
        for &(bits, is_float) in &[
            (8u16, false),
            (16, false),
            (24, false),
            (32, false),
            (32, true),
        ] {
            for &channels in &[1u16, 2, 6] {
                let fmt = InputFormat {
                    sample_rate: 44_100,
                    channels,
                    bits_per_sample: bits,
                    is_float,
                };
                let mut fast = Normalizer::new(fmt, OUT_RATE).expect("valid");
                let mut slow = Reference::new(fmt, OUT_RATE);
                for &n in &[0usize, 1, 441, 882, 3] {
                    let want = slow.push_silence(n);
                    let got = fast.push_silence(n);
                    assert_bits_eq(&got, &want, &format!("{fmt:?} silence {n}"));
                    // Silence must be +0.0, not -0.0.
                    assert!(got.iter().all(|s| s.to_bits() == 0), "{fmt:?}");
                }
            }
        }
    }

    #[test]
    fn decode_into_matches_decode_to_f32_and_reuses_buffer() {
        let fmt = InputFormat {
            sample_rate: OUT_RATE,
            channels: 2,
            bits_per_sample: 24,
            is_float: false,
        };
        let mut rng = Lcg(42);
        let mut out = Vec::with_capacity(64);
        for n in [3usize, 30, 6] {
            let raw = rng.bytes(n * 3);
            decode_into(&raw, &fmt, &mut out);
            assert_bits_eq(
                &out,
                &decode_to_f32(&raw, &fmt).expect("valid"),
                "decode_into",
            );
        }
        assert!(
            out.capacity() >= 64,
            "buffer should be reused, not replaced"
        );
    }

    #[test]
    fn to_stereo_into_matches_to_stereo() {
        let mut out = Vec::new();
        let input = [0.25f32, -0.5, 0.75, 0.1, -0.9, 0.3];
        for ch in [0usize, 1, 2, 3, 6] {
            to_stereo_into(&input, ch, &mut out);
            assert_eq!(out, to_stereo(&input, ch), "channels {ch}");
        }
    }
}
