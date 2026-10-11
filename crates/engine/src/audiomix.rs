//! Pure audio mixing for multi-source tracks (task A3).
//!
//! Every distinct capture source pushes interleaved stereo `f32` samples into
//! its own [`SourceRing`]. [`Mixer::tick`] runs once per 20 ms block and
//! produces one block per track. A track is a sum of [`SourceEdge`]s. An edge
//! names a source, a linear gain and an optional [`NoiseGate`], so one source
//! may appear in several tracks with a different gain in each.
//!
//! Nothing here touches a clock or a device. The feeder thread supplies the
//! pacing and the samples, which keeps the arithmetic testable.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};

use clipcheck_audio::{OUT_CHANNELS, OUT_RATE};

/// Length of one mixing block in milliseconds.
pub const BLOCK_MS: u64 = 20;
/// Interleaved samples in one mixing block (20 ms of stereo at 48 kHz).
pub const BLOCK_SAMPLES: usize = OUT_RATE as usize * BLOCK_MS as usize / 1000 * OUT_CHANNELS;
/// Largest backlog a [`SourceRing`] keeps, in milliseconds.
pub const BACKLOG_MS: u64 = 250;
/// Largest backlog a [`SourceRing`] keeps, in interleaved samples.
pub const BACKLOG_SAMPLES: usize = OUT_RATE as usize * BACKLOG_MS as usize / 1000 * OUT_CHANNELS;
/// Time a noise gate stays open after its input last exceeded the threshold.
/// Rounded up to whole blocks, so the real hold is at least this long.
pub const GATE_HOLD_MS: u64 = 150;
/// Time a noise gate takes to fall from open to closed, in whole blocks.
pub const GATE_RELEASE_MS: u64 = 100;

/// Converts a gain in decibels to a linear factor (`10^(db / 20)`).
///
/// `-inf` maps to `0.0`. NaN maps to `1.0` (no change).
pub fn db_to_gain(db: f32) -> f32 {
    if db.is_nan() {
        return 1.0;
    }
    10f32.powf(db / 20.0)
}

/// Per-source queue of interleaved stereo samples.
///
/// The queue holds at most [`BACKLOG_SAMPLES`]. A push that overflows drops
/// the oldest samples and counts them. A pop that runs short pads with zeros
/// and counts the padding.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SourceRing {
    samples: VecDeque<f32>,
    dropped: u64,
    underflow: u64,
}

impl SourceRing {
    /// Creates an empty ring.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `chunk`. Drops the oldest samples beyond the backlog limit.
    pub fn push(&mut self, chunk: &[f32]) {
        self.samples.extend(chunk.iter().copied());
        if self.samples.len() > BACKLOG_SAMPLES {
            let excess = self.samples.len() - BACKLOG_SAMPLES;
            self.samples.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    /// Removes and returns exactly `n` samples. Missing samples are zero and
    /// are added to the underflow count.
    pub fn pop_block(&mut self, n: usize) -> Vec<f32> {
        let mut out = vec![0.0; n];
        self.pop_into(&mut out);
        out
    }

    /// Fills `out` with the next `out.len()` samples. Missing samples are zero
    /// and are added to the underflow count. Like [`SourceRing::pop_block`], but
    /// writes into a caller-owned buffer.
    pub fn pop_into(&mut self, out: &mut [f32]) {
        let take = out.len().min(self.samples.len());
        for (o, s) in out.iter_mut().zip(self.samples.drain(..take)) {
            *o = s;
        }
        out[take..].fill(0.0);
        self.underflow += (out.len() - take) as u64;
    }

    /// Samples currently queued.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True when no samples are queued.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Total samples dropped because the backlog overflowed.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Total zero samples added because a pop ran short.
    pub fn underflow(&self) -> u64 {
        self.underflow
    }
}

/// Root-mean-square level of `block` in decibels relative to full scale.
///
/// An empty or all-zero block gives `-inf`.
pub fn block_level_db(block: &[f32]) -> f32 {
    if block.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mean_sq = block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32;
    if mean_sq <= 0.0 {
        return f32::NEG_INFINITY;
    }
    10.0 * mean_sq.log10()
}

/// Level floor for the meters, in dBFS. Anything at or below it reports this.
pub const SILENCE_DB: f32 = -120.0;

/// Peak and RMS level of one track in dBFS, both floored at [`SILENCE_DB`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelSnapshot {
    /// Largest absolute sample since the previous take.
    pub peak_db: f32,
    /// RMS of the latest block.
    pub rms_db: f32,
}

/// Peak and RMS of `block` in dBFS, both floored at [`SILENCE_DB`].
///
/// An empty or all-zero block gives `(SILENCE_DB, SILENCE_DB)`. The result is
/// never NaN or -inf.
pub fn block_peak_rms_db(block: &[f32]) -> (f32, f32) {
    let (peak, rms) = block_peak_rms_linear(block);
    (linear_to_db(peak), linear_to_db(rms))
}

/// Linear peak and RMS of `block`. Both are non-negative and never NaN.
/// NaN samples count as zero.
fn block_peak_rms_linear(block: &[f32]) -> (f32, f32) {
    if block.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f32;
    for &s in block {
        let s = if s.is_nan() { 0.0 } else { s };
        peak = peak.max(s.abs());
        sum_sq += s * s;
    }
    (peak, (sum_sq / block.len() as f32).sqrt())
}

/// Converts a non-negative linear amplitude to dBFS, floored at [`SILENCE_DB`].
fn linear_to_db(x: f32) -> f32 {
    if x.is_nan() || x <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * x.log10()).max(SILENCE_DB)
}

/// Lock-free per-track level meters shared between the mixer thread and readers.
///
/// The mixer calls [`LevelTap::record`] for every track block. A reader calls
/// [`LevelTap::take`]. Each slot is one `f32` stored as its bit pattern in an
/// atomic. Peaks are kept with `fetch_max`, which is valid because non-negative
/// `f32` bit patterns order the same way as their values.
#[derive(Debug)]
pub struct LevelTap {
    /// Largest linear peak since the last take, per track.
    peak: Vec<AtomicU32>,
    /// Linear RMS of the latest block, per track.
    rms: Vec<AtomicU32>,
}

impl LevelTap {
    /// Creates a tap with `tracks` slots, all at silence.
    pub fn new(tracks: usize) -> Self {
        Self {
            peak: (0..tracks).map(|_| AtomicU32::new(0)).collect(),
            rms: (0..tracks).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    /// Number of track slots.
    pub fn tracks(&self) -> usize {
        self.peak.len()
    }

    /// Records one block for `track`. Out-of-range tracks are ignored. NaN
    /// samples count as zero. Does not allocate or block.
    pub fn record(&self, track: usize, block: &[f32]) {
        let (Some(peak), Some(rms)) = (self.peak.get(track), self.rms.get(track)) else {
            return;
        };
        let (p, r) = block_peak_rms_linear(block);
        peak.fetch_max(p.to_bits(), Ordering::Relaxed);
        rms.store(r.to_bits(), Ordering::Relaxed);
    }

    /// Returns one snapshot per track in dBFS and resets every peak to silence.
    /// The RMS values are left as they are.
    pub fn take(&self) -> Vec<LevelSnapshot> {
        self.peak
            .iter()
            .zip(self.rms.iter())
            .map(|(p, r)| {
                let peak = f32::from_bits(p.swap(0, Ordering::Relaxed));
                let rms = f32::from_bits(r.load(Ordering::Relaxed));
                LevelSnapshot {
                    peak_db: linear_to_db(peak),
                    rms_db: linear_to_db(rms),
                }
            })
            .collect()
    }
}

/// Noise gate that works on whole 20 ms blocks.
///
/// A block whose level is above the threshold opens the gate at full gain.
/// The gate then stays open for [`GATE_HOLD_MS`] (rounded up to blocks). After
/// that it closes linearly over [`GATE_RELEASE_MS`]. Each block gets one
/// constant gain value, so transitions are stepwise at block edges. The gate
/// starts closed.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseGate {
    threshold_db: f32,
    hold_blocks_left: u32,
    gain: f32,
}

/// Blocks of hold after the last loud block (150 ms rounded up to 8 blocks).
const GATE_HOLD_BLOCKS: u32 = GATE_HOLD_MS.div_ceil(BLOCK_MS) as u32;
/// Blocks in the release ramp (100 ms, 5 blocks).
const GATE_RELEASE_BLOCKS: u32 = (GATE_RELEASE_MS / BLOCK_MS) as u32;

impl NoiseGate {
    /// Creates a closed gate that opens above `threshold_db`.
    pub fn new(threshold_db: f32) -> Self {
        Self {
            threshold_db,
            hold_blocks_left: 0,
            gain: 0.0,
        }
    }

    /// The opening threshold in decibels.
    pub fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    /// Current gate gain in `[0.0, 1.0]`, as of the last processed block.
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Advances the gate by one block and scales `block` in place.
    pub fn process(&mut self, block: &mut [f32]) {
        if block_level_db(block) > self.threshold_db {
            self.gain = 1.0;
            self.hold_blocks_left = GATE_HOLD_BLOCKS;
        } else if self.hold_blocks_left > 0 {
            self.hold_blocks_left -= 1;
        } else {
            let step = 1.0 / GATE_RELEASE_BLOCKS as f32;
            self.gain = (self.gain - step).max(0.0);
        }
        if self.gain != 1.0 {
            for s in block.iter_mut() {
                *s *= self.gain;
            }
        }
    }
}

/// Sums `blocks`, each scaled by its gain, and clamps the result to `[-1, 1]`.
///
/// Returns the mixed block and the number of samples that had to be clamped.
/// The output is as long as the longest input, and shorter inputs count as
/// zero past their end. Only the first `min(blocks.len(), gains.len())` pairs
/// are used. NaN results become `0.0` and count as clipped.
pub fn mix_blocks_counted(blocks: &[&[f32]], gains: &[f32]) -> (Vec<f32>, usize) {
    let pairs = blocks.len().min(gains.len());
    let len = blocks[..pairs].iter().map(|b| b.len()).max().unwrap_or(0);
    let mut out = vec![0.0f32; len];
    for (block, &gain) in blocks[..pairs].iter().zip(&gains[..pairs]) {
        for (o, &s) in out.iter_mut().zip(block.iter()) {
            *o += s * gain;
        }
    }
    let mut clipped = 0usize;
    for s in out.iter_mut() {
        if s.is_nan() {
            *s = 0.0;
            clipped += 1;
        } else if *s > 1.0 {
            *s = 1.0;
            clipped += 1;
        } else if *s < -1.0 {
            *s = -1.0;
            clipped += 1;
        }
    }
    (out, clipped)
}

/// Adds `block` to `mixed`, scaled by `gain`. Unity gain adds without a multiply,
/// which gives the same result. Unclamped, so the caller clamps the sum once.
fn add_scaled(mixed: &mut [f32], block: &[f32], gain: f32) {
    if gain == 1.0 {
        for (o, &s) in mixed.iter_mut().zip(block) {
            *o += s;
        }
    } else {
        for (o, &s) in mixed.iter_mut().zip(block) {
            *o += s * gain;
        }
    }
}

/// Sums `blocks` scaled by `gains` and clamps to `[-1, 1]`. See
/// [`mix_blocks_counted`].
pub fn mix_blocks(blocks: &[&[f32]], gains: &[f32]) -> Vec<f32> {
    mix_blocks_counted(blocks, gains).0
}

/// One source feeding one track.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceEdge {
    /// Index into the mixer's sources.
    pub source: usize,
    /// Linear gain of this source in this track.
    pub gain: f32,
    /// Optional noise gate, applied before the gain.
    pub gate: Option<NoiseGate>,
}

/// Mixes several source rings into one block per track on every tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Mixer {
    rings: Vec<SourceRing>,
    tracks: Vec<Vec<SourceEdge>>,
    clipped: u64,
    /// One block per ring, refilled on every tick. Never sent out.
    scratch: Vec<Vec<f32>>,
    /// Working copy for an edge that has a gate. A gate changes samples, so it
    /// cannot run on the shared `scratch` block that other edges read.
    gate_buf: Vec<f32>,
}

impl Mixer {
    /// Creates a mixer with `source_count` empty rings and the given tracks.
    ///
    /// An edge whose source index is out of range contributes silence.
    pub fn new(source_count: usize, tracks: Vec<Vec<SourceEdge>>) -> Self {
        Self {
            rings: vec![SourceRing::new(); source_count],
            tracks,
            clipped: 0,
            scratch: vec![vec![0.0; BLOCK_SAMPLES]; source_count],
            gate_buf: vec![0.0; BLOCK_SAMPLES],
        }
    }

    /// Queues `samples` for `source`. Unknown sources are ignored.
    pub fn push(&mut self, source: usize, samples: &[f32]) {
        if let Some(ring) = self.rings.get_mut(source) {
            ring.push(samples);
        }
    }

    /// Produces one [`BLOCK_SAMPLES`] block per track.
    ///
    /// Every ring pops one block whether or not a track uses it, so the
    /// backlogs stay bounded. Each edge then gets its gate and gain, and the
    /// edges of a track are mixed and clamped.
    ///
    /// The only allocation is the returned block of each track, because the
    /// caller takes ownership of it. Source blocks, gate copies and the sum are
    /// reused between ticks. An edge with unity gain and no gate adds its source
    /// directly, with no multiply.
    pub fn tick(&mut self) -> Vec<Vec<f32>> {
        let Mixer {
            rings,
            tracks,
            clipped,
            scratch,
            gate_buf,
        } = self;
        for (ring, block) in rings.iter_mut().zip(scratch.iter_mut()) {
            ring.pop_into(block);
        }
        let mut out = Vec::with_capacity(tracks.len());
        for edges in tracks.iter_mut() {
            let mut mixed = vec![0.0f32; BLOCK_SAMPLES];
            for edge in edges.iter_mut() {
                match edge.gate.as_mut() {
                    Some(gate) => {
                        match scratch.get(edge.source) {
                            Some(block) => gate_buf.copy_from_slice(block),
                            None => gate_buf.fill(0.0),
                        }
                        gate.process(gate_buf);
                        add_scaled(&mut mixed, gate_buf, edge.gain);
                    }
                    // A missing source adds nothing. Gains come from `db_to_gain`,
                    // so this matches adding a zero block.
                    None => {
                        if let Some(block) = scratch.get(edge.source) {
                            add_scaled(&mut mixed, block, edge.gain);
                        }
                    }
                }
            }
            let mut n_clipped = 0u64;
            for s in mixed.iter_mut() {
                if s.is_nan() {
                    *s = 0.0;
                    n_clipped += 1;
                } else if *s > 1.0 {
                    *s = 1.0;
                    n_clipped += 1;
                } else if *s < -1.0 {
                    *s = -1.0;
                    n_clipped += 1;
                }
            }
            *clipped += n_clipped;
            out.push(mixed);
        }
        out
    }

    /// Total samples clamped by the mixer since it was created.
    pub fn clipped(&self) -> u64 {
        self.clipped
    }

    /// The ring of `source`, if it exists.
    pub fn ring(&self, source: usize) -> Option<&SourceRing> {
        self.rings.get(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_silence_is_floored() {
        assert_eq!(block_peak_rms_db(&[]), (SILENCE_DB, SILENCE_DB));
        assert_eq!(block_peak_rms_db(&[0.0; 16]), (SILENCE_DB, SILENCE_DB));
        assert_eq!(block_peak_rms_db(&[f32::NAN; 4]), (SILENCE_DB, SILENCE_DB));
    }

    #[test]
    fn level_full_scale_sine_peaks_at_zero_db() {
        let sine: Vec<f32> = (0..480)
            .map(|k| (2.0 * std::f32::consts::PI * k as f32 / 48.0).sin())
            .collect();
        let (peak, rms) = block_peak_rms_db(&sine);
        assert!(peak.abs() < 0.01, "peak {peak}");
        // A full-scale sine has RMS 1/sqrt(2), about -3.01 dBFS.
        assert!((rms + 3.0103).abs() < 0.05, "rms {rms}");
    }

    #[test]
    fn level_tap_peak_is_max_and_take_resets_it() {
        let tap = LevelTap::new(2);
        assert_eq!(tap.tracks(), 2);
        tap.record(0, &[0.2, -0.1]);
        tap.record(0, &[0.5, 0.3]);
        tap.record(0, &[0.1]);
        let first = tap.take();
        assert_eq!(first.len(), 2);
        assert!((first[0].peak_db - 20.0 * 0.5f32.log10()).abs() < 1e-3);
        assert_eq!(first[1].peak_db, SILENCE_DB);
        // The RMS is that of the latest block only.
        assert!((first[0].rms_db - 20.0 * 0.1f32.log10()).abs() < 1e-3);

        let second = tap.take();
        assert_eq!(second[0].peak_db, SILENCE_DB);
        assert!((second[0].rms_db - first[0].rms_db).abs() < 1e-6);
    }

    #[test]
    fn level_tap_ignores_out_of_range_and_nan() {
        let tap = LevelTap::new(1);
        tap.record(5, &[1.0]);
        tap.record(0, &[f32::NAN, f32::NAN]);
        let snaps = tap.take();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].peak_db, SILENCE_DB);
        assert_eq!(snaps[0].rms_db, SILENCE_DB);
    }

    #[test]
    fn gain_table() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-20.0) - 0.1).abs() < 1e-6);
        assert!((db_to_gain(6.0) - 1.9953).abs() < 1e-3);
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(db_to_gain(f32::NAN), 1.0);
    }

    #[test]
    fn block_size_is_20ms_stereo() {
        assert_eq!(BLOCK_SAMPLES, 1920);
        assert_eq!(BACKLOG_SAMPLES, 24000);
    }
}
