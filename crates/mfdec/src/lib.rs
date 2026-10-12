//! clipcheck-mfdec: decode and probe through Media Foundation, JPEG thumbnails through WIC (D58).
//! In-process probing and decoding. Contract: docs/impl/ffmpeg-removal.md, R3.

#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
mod mf;
#[cfg(windows)]
mod wic;

#[cfg(windows)]
pub use mf::{probe, VideoDecoder};
#[cfg(windows)]
pub use wic::thumbnail_jpeg;

/// Header facts about a media file.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    /// Duration in ms; 0 if unknown.
    pub duration_ms: i64,
    /// First video stream width; 0 without video.
    pub width: u32,
    /// First video stream height; 0 without video.
    pub height: u32,
    /// Frame rate of the first video stream; 0.0 if unknown.
    pub fps: f32,
    /// "h264", "hevc", "av1", "vp9", ... ; empty without video.
    pub video_codec: String,
    /// Audio streams in order.
    pub audio: Vec<ProbeAudio>,
}

/// One audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAudio {
    /// "aac", "opus", "mp3", ... ; empty if unknown.
    pub codec: String,
    /// Channel count; 0 if unknown.
    pub channels: u32,
    /// Sample rate in Hz; 0 if unknown.
    pub sample_rate: u32,
}

/// One decoded frame, tightly packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// BGRA (w*h*4) or NV12 (w*h*3/2) bytes.
    pub data: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Presentation time, 100 ns.
    pub pts: i64,
}

/// Pixel format of decoded frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutFormat {
    /// 32-bit BGRA.
    Bgra,
    /// NV12.
    Nv12,
}

/// Errors from decoding and probing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecError {
    /// Bad arguments or unreadable file.
    #[error("bad input: {0}")]
    BadInput(String),
    /// No decoder installed (e.g. HEVC/AV1 Video Extensions from the Microsoft Store).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A Windows call failed.
    #[error("{context} failed: {message}")]
    Windows {
        /// The API step that failed.
        context: &'static str,
        /// Error text from Windows.
        message: String,
    },
}
