//! Exact integer time (docs/design/13-video-editor.md section 14.2, M1).
//!
//! One [`Tick`] is 1 / 254,016,000,000 s. Every common frame rate (23.976, 24, 25, 29.97, 30, 50,
//! 59.94, 60, 120) and audio rate (44.1, 48, 90 kHz) divides that exactly. A tick count must stay
//! below 2^53 so it survives a round trip through a JavaScript number: that is about 9.8 hours.

use serde::{Deserialize, Serialize};

/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 254_016_000_000;

/// Largest tick value the model accepts (2^53 - 1, the largest integer a JS number holds exactly).
pub const MAX_TICK: i64 = (1_i64 << 53) - 1;

/// Longest project timeline: 4 hours.
pub const MAX_PROJECT_TICKS: i64 = 4 * 3600 * TICKS_PER_SECOND;

/// An exact time or duration on the timeline or inside a source file.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    /// Zero.
    pub const ZERO: Tick = Tick(0);

    /// Whole seconds to ticks (saturating).
    pub fn from_secs(secs: i64) -> Tick {
        Tick(secs.saturating_mul(TICKS_PER_SECOND))
    }

    /// Seconds (float) to the nearest tick. NaN and infinities give zero; out-of-range values clamp.
    pub fn from_secs_f64(secs: f64) -> Tick {
        if !secs.is_finite() {
            return Tick::ZERO;
        }
        let v = (secs * TICKS_PER_SECOND as f64).round();
        if v >= MAX_TICK as f64 {
            Tick(MAX_TICK)
        } else if v <= -(MAX_TICK as f64) {
            Tick(-MAX_TICK)
        } else {
            Tick(v as i64)
        }
    }

    /// Milliseconds to ticks (saturating).
    pub fn from_ms(ms: i64) -> Tick {
        Tick(ms.saturating_mul(TICKS_PER_SECOND / 1000))
    }

    /// Seconds as a float (for display and decoder seeks only).
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }

    /// Whole milliseconds, rounded to nearest.
    pub fn as_ms(self) -> i64 {
        let unit = TICKS_PER_SECOND / 1000;
        if self.0 >= 0 {
            (self.0 + unit / 2) / unit
        } else {
            -((-self.0 + unit / 2) / unit)
        }
    }

    /// Checked addition.
    pub fn checked_add(self, other: Tick) -> Option<Tick> {
        self.0.checked_add(other.0).map(Tick)
    }

    /// Checked subtraction.
    pub fn checked_sub(self, other: Tick) -> Option<Tick> {
        self.0.checked_sub(other.0).map(Tick)
    }

    /// Saturating addition.
    pub fn saturating_add(self, other: Tick) -> Tick {
        Tick(self.0.saturating_add(other.0))
    }

    /// Saturating subtraction.
    pub fn saturating_sub(self, other: Tick) -> Tick {
        Tick(self.0.saturating_sub(other.0))
    }

    /// The smaller of two ticks.
    pub fn min(self, other: Tick) -> Tick {
        if self <= other {
            self
        } else {
            other
        }
    }

    /// The larger of two ticks.
    pub fn max(self, other: Tick) -> Tick {
        if self >= other {
            self
        } else {
            other
        }
    }

    /// Clamps into `lo..=hi` (returns `lo` when `hi < lo`).
    pub fn clamp(self, lo: Tick, hi: Tick) -> Tick {
        if hi < lo {
            return lo;
        }
        self.max(lo).min(hi)
    }

    /// Index of the frame that contains this time, flooring (negative times give 0).
    pub fn to_frame(self, rate: Rate) -> i64 {
        if self.0 <= 0 || rate.num == 0 || rate.den == 0 {
            return 0;
        }
        // frame = t * num / (den * TPS); do it in i128 to avoid overflow.
        let n = i128::from(self.0) * i128::from(rate.num);
        let d = i128::from(rate.den) * i128::from(TICKS_PER_SECOND);
        (n / d) as i64
    }

    /// Start time of frame `n` at `rate` (rounds to the nearest tick; exact for common rates).
    pub fn from_frame(n: i64, rate: Rate) -> Tick {
        if rate.num == 0 || rate.den == 0 {
            return Tick::ZERO;
        }
        let n128 = i128::from(n) * i128::from(rate.den) * i128::from(TICKS_PER_SECOND);
        let q = (n128 + i128::from(rate.num) / 2) / i128::from(rate.num);
        Tick(q.clamp(-i128::from(MAX_TICK), i128::from(MAX_TICK)) as i64)
    }

    /// Snaps to the nearest frame boundary at `rate`.
    pub fn snap_to_frame(self, rate: Rate) -> Tick {
        if rate.num == 0 || rate.den == 0 {
            return self;
        }
        let n = i128::from(self.0) * i128::from(rate.num);
        let d = i128::from(rate.den) * i128::from(TICKS_PER_SECOND);
        let frame = (n + d / 2).div_euclid(d);
        Tick::from_frame(frame as i64, rate)
    }

    /// Index of the audio sample at `sample_rate` Hz that this time falls on (floor, >= 0).
    pub fn to_sample(self, sample_rate: u32) -> i64 {
        if self.0 <= 0 || sample_rate == 0 {
            return 0;
        }
        (i128::from(self.0) * i128::from(sample_rate) / i128::from(TICKS_PER_SECOND)) as i64
    }

    /// Time of sample `n` at `sample_rate` Hz.
    pub fn from_sample(n: i64, sample_rate: u32) -> Tick {
        if sample_rate == 0 {
            return Tick::ZERO;
        }
        let q = (i128::from(n) * i128::from(TICKS_PER_SECOND) + i128::from(sample_rate) / 2)
            / i128::from(sample_rate);
        Tick(q.clamp(-i128::from(MAX_TICK), i128::from(MAX_TICK)) as i64)
    }
}

impl std::ops::Add for Tick {
    type Output = Tick;
    fn add(self, rhs: Tick) -> Tick {
        self.saturating_add(rhs)
    }
}

impl std::ops::Sub for Tick {
    type Output = Tick;
    fn sub(self, rhs: Tick) -> Tick {
        self.saturating_sub(rhs)
    }
}

/// A rational frame rate, `num / den` frames per second (for example 30000/1001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rate {
    /// Numerator.
    pub num: u32,
    /// Denominator (non-zero).
    pub den: u32,
}

impl Rate {
    /// A whole-number rate.
    pub const fn whole(fps: u32) -> Rate {
        Rate { num: fps, den: 1 }
    }

    /// Frames per second as a float.
    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            f64::from(self.num) / f64::from(self.den)
        }
    }

    /// Duration of one frame (rounded to the nearest tick).
    pub fn frame_duration(self) -> Tick {
        Tick::from_frame(1, self)
    }

    /// Nearest standard rate (23.976, 24, 25, 29.97, 30, 48, 50, 59.94, 60, 120) to `fps`,
    /// or a whole-number rate when none is within 0.01.
    pub fn from_f64(fps: f64) -> Rate {
        const STD: [(u32, u32); 10] = [
            (24000, 1001),
            (24, 1),
            (25, 1),
            (30000, 1001),
            (30, 1),
            (48, 1),
            (50, 1),
            (60000, 1001),
            (60, 1),
            (120, 1),
        ];
        for (n, d) in STD {
            if (f64::from(n) / f64::from(d) - fps).abs() < 0.01 {
                return Rate { num: n, den: d };
            }
        }
        let whole = fps.round().clamp(1.0, 240.0) as u32;
        Rate::whole(whole)
    }
}

impl Default for Rate {
    fn default() -> Self {
        Rate::whole(30)
    }
}
