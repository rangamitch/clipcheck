//! Platform-neutral encoder types (orchestrator-owned).

/// Output codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
}

/// GPU or software vendor of an encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Vendor {
    /// NVIDIA (PCI vendor 0x10DE).
    Nvidia,
    /// AMD (0x1002).
    Amd,
    /// Intel (0x8086).
    Intel,
    /// Microsoft software encoder.
    Microsoft,
    /// Anything else.
    Other,
}

/// Rate control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// Constant bitrate.
    Cbr {
        /// Target in kbit/s.
        kbps: u32,
    },
    /// Peak-constrained variable bitrate.
    Vbr {
        /// Average in kbit/s.
        kbps: u32,
        /// Peak in kbit/s (>= kbps).
        peak_kbps: u32,
    },
    /// Constant quality.
    Quality {
        /// Quantizer-like quality, 0 (best) ..= 51.
        cq: u8,
    },
}

/// Encoder session settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoEncoderConfig {
    /// Output codec.
    pub codec: Codec,
    /// Width in pixels (even).
    pub width: u32,
    /// Height in pixels (even).
    pub height: u32,
    /// Frames per second (> 0).
    pub fps: u32,
    /// Rate control.
    pub rate: RateControl,
    /// Frames per GOP (IDR interval), > 0.
    pub gop_frames: u32,
    /// 0 = fastest .. 100 = best quality (`CODECAPI_AVEncCommonQualityVsSpeed`).
    pub quality_vs_speed: u32,
    /// Ask for low-latency mode (`MF_LOW_LATENCY`).
    pub low_latency: bool,
}

/// One encoded access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedPacket {
    /// Annex B access unit (start codes).
    pub data: Vec<u8>,
    /// Presentation time, 100 ns.
    pub pts: i64,
    /// Decode time, 100 ns (== pts without B-frames).
    pub dts: i64,
    /// IDR / clean point.
    pub keyframe: bool,
}

/// An encoder found by [`crate::enumerate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderDescriptor {
    /// Friendly name (`MFT_FRIENDLY_NAME_Attribute`).
    pub name: String,
    /// Vendor.
    pub vendor: Vendor,
    /// Codec it produces.
    pub codec: Codec,
    /// Hardware (async) MFT.
    pub hardware: bool,
    /// Position in the enumeration order for this codec; used by [`crate::open`].
    pub index: usize,
}

/// Errors from the encoders.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncError {
    /// Settings or input are invalid.
    #[error("bad input: {0}")]
    BadInput(String),
    /// No encoder matches.
    #[error("encoder not found: {0}")]
    NotFound(String),
    /// A Windows / Media Foundation call failed.
    #[error("{context} failed: {message} (hr {hresult:#010x})")]
    Windows {
        /// The API step that failed.
        context: &'static str,
        /// Error text from Windows.
        message: String,
        /// HRESULT.
        hresult: i32,
    },
}

#[cfg(windows)]
impl EncError {
    /// Maps a `windows` error for `context`.
    pub fn win(context: &'static str) -> impl Fn(windows::core::Error) -> EncError {
        move |e| EncError::Windows {
            context,
            message: e.message().to_string(),
            hresult: e.code().0,
        }
    }
}
