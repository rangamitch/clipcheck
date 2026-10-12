//! Pure audio gain maths shared by preview parameters and export.

use crate::model::{Clip, ClipAudio, EnvPoint, FadeCurve};
use crate::time::Tick;

/// dB to linear amplitude (`10^(db/20)`). -inf-ish values (<= -120) give 0.0.
pub fn db_to_lin(db: f64) -> f64 {
    if db.is_nan() || db <= -120.0 {
        return 0.0;
    }
    10f64.powf(db / 20.0)
}

/// Linear amplitude to dB; values <= 0 give -120.0.
pub fn lin_to_db(lin: f64) -> f64 {
    if lin.is_nan() || lin <= 0.0 {
        return -120.0;
    }
    20.0 * lin.log10()
}

/// Fade gain factor 0..=1 for position `pos` inside a fade of length `len` (pos 0 = silent end of
/// a fade-in). Linear: `pos/len`. EqualPower: `sin(pi/2 * pos/len)`. `len <= 0` gives 1.0.
/// `pos` is clamped into `0..=len`.
pub fn fade_factor(curve: FadeCurve, pos: Tick, len: Tick) -> f64 {
    if len <= Tick::ZERO {
        return 1.0;
    }
    let r = (pos.0 as f64 / len.0 as f64).clamp(0.0, 1.0);
    match curve {
        FadeCurve::Linear => r,
        FadeCurve::EqualPower => (std::f64::consts::FRAC_PI_2 * r).sin(),
    }
}

/// Envelope gain in dB at `t` (time from clip start): linear interpolation in dB between points,
/// flat before the first and after the last, 0.0 when there are no points.
pub fn envelope_db_at(env: &[EnvPoint], t: Tick) -> f64 {
    let (first, last) = match (env.first(), env.last()) {
        (Some(f), Some(l)) => (f, l),
        _ => return 0.0,
    };
    if t <= first.t {
        return first.db;
    }
    if t >= last.t {
        return last.db;
    }
    for w in env.windows(2) {
        if let [a, b] = w {
            if t >= a.t && t <= b.t {
                let span = b.t.0 as f64 - a.t.0 as f64;
                if span <= 0.0 {
                    return b.db;
                }
                let frac = (t.0 as f64 - a.t.0 as f64) / span;
                return a.db + (b.db - a.db) * frac;
            }
        }
    }
    last.db
}

/// Total linear gain of a clip at `t` (time from clip start, 0..clip.dur): clip gain x envelope x
/// fade in x fade out, ignoring track fader and mute. Muted clips give 0.0.
pub fn clip_gain_at(clip_audio: &ClipAudio, clip_dur: Tick, t: Tick) -> f64 {
    if clip_audio.muted {
        return 0.0;
    }
    let t = t.clamp(Tick::ZERO, clip_dur);
    let gain_db = clip_audio.gain_db + envelope_db_at(&clip_audio.envelope, t);
    let mut gain = db_to_lin(gain_db);
    if t < clip_audio.fade_in {
        gain *= fade_factor(clip_audio.fade_curve, t, clip_audio.fade_in);
    }
    let remaining = clip_dur - t;
    if remaining < clip_audio.fade_out {
        gain *= fade_factor(clip_audio.fade_curve, remaining, clip_audio.fade_out);
    }
    gain
}

/// Left/right gains for a pan value -1..=1 using the constant-power law
/// (`L = cos((pan+1) * pi/4)`, `R = sin((pan+1) * pi/4)`, scaled by sqrt(2) so centre is 1.0).
/// The pan is clamped into -1..=1; NaN counts as centre.
pub fn pan_gains(pan: f64) -> (f64, f64) {
    let pan = if pan.is_nan() {
        0.0
    } else {
        pan.clamp(-1.0, 1.0)
    };
    let angle = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
    let scale = std::f64::consts::SQRT_2;
    (angle.cos() * scale, angle.sin() * scale)
}

/// Convenience: gain of an audio [`Clip`] at timeline time `t` (absolute), 0.0 outside the clip.
/// The clip covers `start..end` (end exclusive).
pub fn clip_gain_at_timeline(clip: &Clip, t: Tick) -> f64 {
    if t < clip.start || t >= clip.end() {
        return 0.0;
    }
    clip_gain_at(&clip.audio, clip.dur, t - clip.start)
}
