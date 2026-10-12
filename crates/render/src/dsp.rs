//! Audio DSP (pure, no OS). Owner: worker `render-dsp`.
//!
//! Conventions: samples are `f32` in -1.0..=1.0, interleaved. A trailing partial frame (input
//! length not a multiple of `channels`) is ignored. `channels == 0` gives empty output.
//!
//! Specs used:
//! - ITU-R BS.1770-4 (2015): K-weighting (pre-filter high shelf and RLB high-pass), 400 ms gating
//!   blocks with 75 % overlap (100 ms hop), absolute gate -70 LUFS, relative gate -10 LU,
//!   loudness = -0.691 + 10 log10(sum of channel mean squares). The K-weighting filters are the
//!   BS.1770-4 analog prototypes (f0, gain, Q) mapped to any sample rate with the bilinear
//!   transform; at 48 kHz they reproduce the standard's tabulated coefficients. Channel weights
//!   are 1.0 for every channel (no surround weighting).
//! - EBU R128: the LUFS scale and gating as used for loudness normalisation (target -23 LUFS by
//!   convention; the target itself is chosen by the caller).

use std::collections::VecDeque;
use std::f64::consts::{PI, SQRT_2};

/// Upper bound on output frames for any resampling call. Larger requests return empty output
/// instead of attempting a huge allocation (about 12 h at 48 kHz).
const MAX_OUT_FRAMES: u64 = 1 << 31;

/// Second-order IIR section (direct form II transposed), coefficients normalised so a0 = 1.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    /// Builds a section from `b` (numerator) and `a1`, `a2` (denominator, a0 = 1), with zero state.
    fn new(b: [f64; 3], a: [f64; 2]) -> Biquad {
        Biquad {
            b0: b[0],
            b1: b[1],
            b2: b[2],
            a1: a[0],
            a2: a[1],
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Filters one sample.
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Second-order Butterworth low-pass (RBJ cookbook form, Q = 1/sqrt(2)).
fn butterworth_lowpass(fs: f64, fc: f64) -> Biquad {
    let w0 = 2.0 * PI * fc / fs;
    let cosw = w0.cos();
    let alpha = w0.sin() / SQRT_2;
    let a0 = 1.0 + alpha;
    let b0 = (1.0 - cosw) / 2.0 / a0;
    Biquad::new(
        [b0, (1.0 - cosw) / a0, b0],
        [-2.0 * cosw / a0, (1.0 - alpha) / a0],
    )
}

/// Output frames for `frames * num / den`, rounded half up, or `None` when over the cap.
fn scaled_frames(frames: usize, num: u64, den: u64) -> Option<usize> {
    let n = (frames as u64).saturating_mul(num);
    let v = n.saturating_mul(2).saturating_add(den) / den.saturating_mul(2).max(1);
    if v > MAX_OUT_FRAMES {
        None
    } else {
        usize::try_from(v).ok()
    }
}

/// Appends one frame of `ch` samples read by linear interpolation at fractional frame `pos`.
/// Positions are clamped to the first and last frame. Requires `frames >= 1`.
fn push_interp(data: &[f32], ch: usize, frames: usize, pos: f64, out: &mut Vec<f32>) {
    let last = frames.saturating_sub(1);
    let p = pos.max(0.0);
    let floor = p.floor();
    let i0 = (floor as usize).min(last);
    let i1 = (i0 + 1).min(last);
    let frac = (p - floor).clamp(0.0, 1.0) as f32;
    for c in 0..ch {
        let a = data.get(i0 * ch + c).copied().unwrap_or(0.0);
        let b = data.get(i1 * ch + c).copied().unwrap_or(0.0);
        out.push(a + (b - a) * frac);
    }
}

/// Converts between sample rates by linear interpolation with a short windowed-sinc low-pass when
/// downsampling (quality is secondary; must not alias badly or click). `channels >= 1`. Output
/// length is `round(frames * to / from)` frames.
///
/// Downsampling first applies a 2nd-order Butterworth low-pass at 0.45 * `to` per channel.
/// Equal rates return the input (whole frames only).
pub fn resample(input: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
    if channels == 0 || from == 0 || to == 0 {
        return Vec::new();
    }
    let frames = input.len() / channels;
    let whole = frames * channels;
    if from == to {
        return input.iter().take(whole).copied().collect();
    }
    if frames == 0 {
        return Vec::new();
    }
    let Some(out_frames) = scaled_frames(frames, u64::from(to), u64::from(from)) else {
        return Vec::new();
    };

    // Only a downsample needs a filtered copy; an upsample interpolates straight from the input.
    let filtered: Vec<f32>;
    let work: &[f32] = if to < from {
        let mut buf: Vec<f32> = input.iter().take(whole).copied().collect();
        let lp = butterworth_lowpass(f64::from(from), 0.45 * f64::from(to));
        let mut filters = vec![lp; channels];
        for frame in buf.chunks_exact_mut(channels) {
            for (f, s) in filters.iter_mut().zip(frame.iter_mut()) {
                *s = f.process(f64::from(*s)) as f32;
            }
        }
        filtered = buf;
        &filtered
    } else {
        input.get(..whole).unwrap_or(input)
    };

    let ratio = f64::from(from) / f64::from(to);
    let mut out = Vec::with_capacity(out_frames * channels);
    for o in 0..out_frames {
        push_interp(work, channels, frames, o as f64 * ratio, &mut out);
    }
    out
}

/// Plays `input` at `speed` (> 0) by resampling (pitch follows speed). Output frames =
/// `round(frames / speed)`.
///
/// Speed `<= 0`, non-finite speed, or `channels == 0` return empty output. Speed 1.0 returns the
/// input (whole frames only). Positions are linear interpolation at `i * speed`.
pub fn change_speed(input: &[f32], channels: usize, speed: f64) -> Vec<f32> {
    let mut out = Vec::new();
    change_speed_into(input, channels, speed, &mut out);
    out
}

/// Same as [`change_speed`], writing into `out` (cleared first) so a caller can reuse its buffer.
pub fn change_speed_into(input: &[f32], channels: usize, speed: f64, out: &mut Vec<f32>) {
    out.clear();
    if channels == 0 || !speed.is_finite() || speed <= 0.0 {
        return;
    }
    let frames = input.len() / channels;
    if frames == 0 {
        return;
    }
    if (speed - 1.0).abs() < 1e-12 {
        out.extend(input.iter().take(frames * channels).copied());
        return;
    }
    let r = (frames as f64 / speed).round();
    if !r.is_finite() || r > MAX_OUT_FRAMES as f64 {
        return;
    }
    let out_frames = r as usize;
    out.reserve(out_frames * channels);
    for o in 0..out_frames {
        push_interp(input, channels, frames, o as f64 * speed, out);
    }
}

/// Reverses the frame order of interleaved audio.
pub fn reverse_frames(input: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let mut buf: Vec<f32> = input
        .iter()
        .take(input.len() / channels * channels)
        .copied()
        .collect();
    reverse_frames_in_place(&mut buf, channels);
    buf
}

/// Reverses the frame order of interleaved audio in place. A trailing partial frame (length not a
/// multiple of `channels`) is not moved. `channels == 0` does nothing.
pub fn reverse_frames_in_place(buf: &mut [f32], channels: usize) {
    if channels == 0 {
        return;
    }
    let whole = buf.len() / channels * channels;
    let Some(body) = buf.get_mut(..whole) else {
        return;
    };
    let mut lo = 0usize;
    let mut hi = whole / channels;
    while lo + 1 < hi {
        hi -= 1;
        let (head, tail) = body.split_at_mut(hi * channels);
        if let (Some(a), Some(b)) = (
            head.get_mut(lo * channels..(lo + 1) * channels),
            tail.get_mut(..channels),
        ) {
            a.swap_with_slice(b);
        }
        lo += 1;
    }
}

/// Adds `src * gain` into `dst` (up to the shorter length).
pub fn mix_into(dst: &mut [f32], src: &[f32], gain: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += s * gain;
    }
}

/// Applies per-channel gains to interleaved stereo (`left`, `right`).
pub fn apply_stereo_gain(buf: &mut [f32], left: f32, right: f32) {
    for frame in buf.as_chunks_mut::<2>().0 {
        let [l, r] = frame;
        *l *= left;
        *r *= right;
    }
}

/// Down-mixes interleaved stereo to mono in place (both channels = average).
pub fn stereo_to_mono_in_place(buf: &mut [f32]) {
    for frame in buf.as_chunks_mut::<2>().0 {
        let [l, r] = frame;
        let m = (*l + *r) * 0.5;
        *l = m;
        *r = m;
    }
}

/// Converts mono interleaved to stereo interleaved.
pub fn mono_to_stereo(input: &[f32]) -> Vec<f32> {
    input.iter().flat_map(|&s| [s, s]).collect()
}

/// Largest absolute sample in dBFS (-120.0 for silence/empty).
pub fn peak_db(buf: &[f32]) -> f64 {
    let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (20.0 * f64::from(peak).log10()).max(-120.0)
}

/// Waveform peaks: `buckets` values in 0.0..=1.0, each the max absolute sample over an equal share
/// of the frames (all channels). Empty input gives `buckets` zeros. `channels == 0` or
/// `buckets == 0` gives empty output.
pub fn waveform_peaks(input: &[f32], channels: usize, buckets: usize) -> Vec<f32> {
    if channels == 0 || buckets == 0 {
        return Vec::new();
    }
    let mut out = vec![0.0f32; buckets];
    let frames = input.len() / channels;
    if frames == 0 {
        return out;
    }
    for (f, frame) in input.chunks_exact(channels).enumerate() {
        let b = ((f as u128 * buckets as u128) / frames as u128) as usize;
        let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if let Some(slot) = out.get_mut(b) {
            *slot = slot.max(peak);
        }
    }
    for v in &mut out {
        *v = v.clamp(0.0, 1.0);
    }
    out
}

/// Look-ahead peak limiter (ceiling in dBFS, e.g. -1.0), 5 ms look-ahead, 50 ms release, stereo
/// interleaved. Output is delayed by the look-ahead; call [`Limiter::flush`] at the end.
///
/// Algorithm: each input frame's target gain is `min(1, ceiling / peak)`. For output frame `t`
/// the gain bound is `min over j in [t, t + L] of (target_j + (j - t) / L)` (a linear attack of
/// slope 1 / L that reaches each target when its peak arrives; computed with a monotonic deque).
/// The applied gain follows the bound downwards at once and recovers exponentially (50 ms) toward
/// the bound. A final hard clamp to +-ceiling guards against rounding.
#[derive(Debug, Clone)]
pub struct Limiter {
    ceiling: f32,
    channels: usize,
    look: usize,
    release: f64,
    /// Delay line: the last `look` input frames, interleaved.
    ring: Vec<f32>,
    /// Frame slot in `ring` for the next input frame.
    ring_pos: usize,
    /// Index of the next input frame.
    frame_index: usize,
    /// Monotonic deque of `(frame index, target_j + j / L)` for the look-ahead window.
    window: VecDeque<(usize, f64)>,
    /// Applied gain after the most recent output frame.
    gain: f64,
}

impl Limiter {
    /// New limiter. A non-finite `ceiling_db` is treated as -1.0 dBFS; sample rate 0 is treated as
    /// 48 000 Hz; `channels` 0 is treated as 1.
    pub fn new(ceiling_db: f64, sample_rate: u32, channels: usize) -> Limiter {
        let sr = if sample_rate == 0 {
            48_000.0
        } else {
            f64::from(sample_rate)
        };
        let ceiling_db = if ceiling_db.is_finite() {
            ceiling_db
        } else {
            -1.0
        };
        let channels = channels.max(1);
        let look = (sr * 0.005).round().max(1.0) as usize;
        let release = 1.0 - (-1.0 / (0.050 * sr)).exp();
        let mut limiter = Limiter {
            ceiling: 10f64.powf(ceiling_db / 20.0) as f32,
            channels,
            look,
            release,
            ring: Vec::new(),
            ring_pos: 0,
            frame_index: 0,
            window: VecDeque::new(),
            gain: 1.0,
        };
        limiter.reset();
        limiter
    }

    /// Clears the delay line and gain state.
    fn reset(&mut self) {
        self.ring = vec![0.0; self.look * self.channels];
        self.ring_pos = 0;
        self.frame_index = 0;
        self.window.clear();
        self.gain = 1.0;
    }

    /// Processes one frame (`channels` samples) in place.
    fn process_frame(&mut self, frame: &mut [f32]) {
        let ch = self.channels;
        let n = self.frame_index;
        let look = self.look as f64;

        let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let target = (f64::from(self.ceiling) / f64::from(peak)).min(1.0);
        let v = target + n as f64 / look;
        while self.window.back().is_some_and(|&(_, bv)| bv >= v) {
            self.window.pop_back();
        }
        self.window.push_back((n, v));
        self.frame_index = n + 1;

        let mut gain = 1.0;
        if n >= self.look {
            let t = n - self.look;
            while self.window.front().is_some_and(|&(idx, _)| idx < t) {
                self.window.pop_front();
            }
            let min_v = self.window.front().map_or(1.0, |&(_, mv)| mv);
            let bound = (min_v - t as f64 / look).clamp(0.0, 1.0);
            let recovered = self.gain + (1.0 - self.gain) * self.release;
            self.gain = bound.min(recovered);
            gain = self.gain;
        }

        let ceil = f64::from(self.ceiling);
        let base = self.ring_pos * ch;
        if let Some(slot) = self.ring.get_mut(base..base + ch) {
            for (s, d) in frame.iter_mut().zip(slot.iter_mut()) {
                let old = *d;
                *d = *s;
                *s = (f64::from(old) * gain).clamp(-ceil, ceil) as f32;
            }
        }
        self.ring_pos += 1;
        if self.ring_pos == self.look {
            self.ring_pos = 0;
        }
    }

    /// Processes a block in place (length multiple of channels). Trailing partial frames are left
    /// unchanged. The first look-ahead frames of output are silence.
    pub fn process(&mut self, buf: &mut [f32]) {
        let ch = self.channels;
        for frame in buf.chunks_exact_mut(ch) {
            self.process_frame(frame);
        }
    }

    /// Returns the delayed tail (look-ahead samples, interleaved) after the last block. The
    /// limiter is reset afterwards, so it can start a new stream.
    pub fn flush(&mut self) -> Vec<f32> {
        let mut silence = vec![0.0f32; self.channels];
        let mut out = Vec::with_capacity(self.look * self.channels);
        for _ in 0..self.look {
            silence.iter_mut().for_each(|s| *s = 0.0);
            self.process_frame(&mut silence);
            out.extend_from_slice(&silence);
        }
        self.reset();
        out
    }
}

/// ITU-R BS.1770-4 loudness meter (K-weighting, 400 ms blocks with 75 % overlap, absolute gate
/// -70 LUFS, relative gate -10 LU).
#[derive(Debug, Clone)]
pub struct LoudnessMeter {
    channels: usize,
    /// Block hop in frames (100 ms).
    hop: usize,
    /// Block length in frames (400 ms).
    block: usize,
    pre: Vec<Biquad>,
    rlb: Vec<Biquad>,
    /// Ring of per-frame energy (sum over channels of K-weighted squares), `block` long.
    energy: Vec<f64>,
    pos: usize,
    window_sum: f64,
    frames: u64,
    /// Mean square (summed over channels) of every completed block, in order.
    blocks: Vec<f64>,
}

impl LoudnessMeter {
    /// New meter for interleaved audio. `channels` is clamped to at least 1 (all channels weighted
    /// 1.0). Sample rate 0 is treated as 48 000 Hz.
    pub fn new(sample_rate: u32, channels: usize) -> LoudnessMeter {
        let sr = if sample_rate == 0 {
            48_000
        } else {
            sample_rate
        };
        let fs = f64::from(sr);
        let channels = channels.max(1);
        let hop = ((fs / 10.0).round() as usize).max(1);
        let block = hop * 4;

        // BS.1770-4 Annex 1 analog prototypes, bilinear transform (K = tan(pi f0 / fs)).
        let k = (PI * 1_681.974_450_955_533 / fs).tan();
        let vh = 10f64.powf(3.999_843_853_973_347 / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let q = 0.707_175_236_955_419_6;
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad::new(
            [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );

        let k2 = (PI * 38.135_470_876_024_44 / fs).tan();
        let q2 = 0.500_327_037_323_877_3;
        let a02 = 1.0 + k2 / q2 + k2 * k2;
        let hp = Biquad::new(
            [1.0, -2.0, 1.0],
            [2.0 * (k2 * k2 - 1.0) / a02, (1.0 - k2 / q2 + k2 * k2) / a02],
        );

        LoudnessMeter {
            channels,
            hop,
            block,
            pre: vec![shelf; channels],
            rlb: vec![hp; channels],
            energy: vec![0.0; block],
            pos: 0,
            window_sum: 0.0,
            frames: 0,
            blocks: Vec::new(),
        }
    }

    /// Feeds samples. Any block size is accepted, but samples that do not complete a frame are
    /// ignored, so callers should pass whole frames.
    pub fn push(&mut self, interleaved: &[f32]) {
        let ch = self.channels;
        for frame in interleaved.chunks_exact(ch) {
            let mut e = 0.0f64;
            for ((pre, rlb), &s) in self.pre.iter_mut().zip(self.rlb.iter_mut()).zip(frame) {
                let z = rlb.process(pre.process(f64::from(s)));
                e += z * z;
            }
            let old = self.energy.get(self.pos).copied().unwrap_or(0.0);
            if let Some(slot) = self.energy.get_mut(self.pos) {
                *slot = e;
            }
            self.window_sum += e - old;
            self.pos += 1;
            if self.pos == self.block {
                self.pos = 0;
            }
            self.frames += 1;

            let block = self.block as u64;
            if self.frames >= block && (self.frames - block).is_multiple_of(self.hop as u64) {
                self.blocks
                    .push(self.window_sum.max(0.0) / self.block as f64);
            }
        }
    }

    /// Integrated loudness in LUFS, None when no block passes the gates.
    pub fn integrated_lufs(&self) -> Option<f64> {
        let to_lufs = |ms: f64| -0.691 + 10.0 * ms.log10();
        let above_abs = |ms: &f64| to_lufs(*ms) > -70.0;

        let (sum, count) = self
            .blocks
            .iter()
            .filter(|ms| above_abs(ms))
            .fold((0.0f64, 0usize), |(s, c), &ms| (s + ms, c + 1));
        if count == 0 {
            return None;
        }
        let relative_gate = to_lufs(sum / count as f64) - 10.0;

        let (sum2, count2) = self
            .blocks
            .iter()
            .filter(|ms| above_abs(ms) && to_lufs(**ms) > relative_gate)
            .fold((0.0f64, 0usize), |(s, c), &ms| (s + ms, c + 1));
        if count2 == 0 {
            return None;
        }
        Some(to_lufs(sum2 / count2 as f64))
    }
}

/// Gain in dB to move `measured_lufs` to `target_lufs`, capped to -30..=+30 dB. A non-finite
/// measurement (for example silence) gives 0 dB.
pub fn normalize_gain_db(measured_lufs: f64, target_lufs: f64) -> f64 {
    if !measured_lufs.is_finite() {
        return 0.0;
    }
    (target_lufs - measured_lufs).clamp(-30.0, 30.0)
}
