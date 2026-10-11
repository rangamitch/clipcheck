//! Rate control, speed/quality and GPU options for the video encoder.

/// Rate control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateMode {
    /// Constant bitrate at the target bitrate.
    #[default]
    Cbr,
    /// Variable bitrate: average the target bitrate, peak `max_kbps`.
    Vbr {
        /// Peak bitrate in kbit/s. Must be at least `bitrate_kbps`.
        max_kbps: u32,
    },
    /// Constant quality. `cq` is a quantizer-style value (0..=51, or 0..=63 for
    /// NVENC AV1). Bitrate is not the target.
    Quality {
        /// Constant-quality value. Lower is higher quality.
        cq: u8,
    },
    /// Constant quality with a peak cap. The quality value sets the bits spent, and
    /// `max_kbps` only bounds the peak (`-maxrate`, `-bufsize` = 2 x peak). Presets use this.
    /// AMF has no peak cap in its constant-QP mode, so the cap is not applied there.
    QualityPeak {
        /// Constant-quality value, same range as [`RateMode::Quality`].
        cq: u8,
        /// Peak bitrate in kbit/s. Must be non-zero.
        max_kbps: u32,
    },
}

/// Speed versus quality trade-off, mapped to each backend's preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Speed {
    /// Fastest encode, lowest latency.
    Fastest,
    /// Middle ground.
    #[default]
    Balanced,
    /// Best compression quality, slowest.
    Quality,
}

/// Encoder options for the native encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderTuning {
    /// Rate control mode.
    pub rate: RateMode,
    /// Speed/quality preset.
    pub speed: Speed,
    /// B-frames between references. 0..=4. Ignored by encoders without B-frames.
    pub b_frames: u8,
    /// GPU index for NVENC (`-gpu`). `None` lets the driver choose. Ignored elsewhere.
    pub gpu_index: Option<u32>,
}

impl Default for EncoderTuning {
    fn default() -> Self {
        Self {
            rate: RateMode::Cbr,
            speed: Speed::Balanced,
            b_frames: 2,
            gpu_index: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tuning_values() {
        let t = EncoderTuning::default();
        assert_eq!(t.rate, RateMode::Cbr);
        assert_eq!(t.speed, Speed::Balanced);
        assert_eq!(t.b_frames, 2);
        assert_eq!(t.gpu_index, None);
    }
}
