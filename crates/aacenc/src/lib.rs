//! clipcheck-aacenc: AAC-LC through the Windows AAC encoder MFT, in process (D57).
//!
//! Contracts: docs/impl/native-encoder.md, section "crates/aacenc".

#[cfg(windows)]
mod mf;

#[cfg(windows)]
pub use mf::AacEncoder;

/// One encoded AAC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AacPacket {
    /// Raw AAC frame, no ADTS header.
    pub data: Vec<u8>,
    /// Presentation time of the frame's first sample, 100 ns.
    pub pts: i64,
    /// PCM samples per channel in the frame (1024).
    pub samples: u32,
}

/// Errors from the AAC encoder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AacError {
    /// Arguments are invalid (rate, channels, odd sample count).
    #[error("bad input: {0}")]
    BadInput(String),
    /// A Media Foundation call failed.
    #[error("{context} failed: {message}")]
    Windows {
        /// The API step that failed.
        context: &'static str,
        /// Error text from Windows.
        message: String,
    },
}
