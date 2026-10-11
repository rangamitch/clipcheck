//! clipcheck-mp4: MP4 writer (D57), reader and remuxer (D58); ClipCheck's own MP4 code, no external tools.
//!
//! Pure Rust, no `unsafe`. Contracts: docs/impl/native-encoder.md, section "crates/mp4".
//! Every value read from a bitstream is untrusted: parsers return `None`/`Err`, never panic.

#![forbid(unsafe_code)]

pub mod aac;
pub mod annexb;
pub mod avc;
pub mod fmp4;
pub mod hevc;
pub mod reader;
pub mod remux;
pub mod segment;

pub use annexb::VideoCodec;

/// Errors from the muxer.
#[derive(Debug, thiserror::Error)]
pub enum Mp4Error {
    /// Writing a segment or the segment list failed.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The first keyframe carried no SPS/PPS (or VPS/SPS/PPS for HEVC).
    #[error("first keyframe has no parameter sets")]
    MissingParameterSets,
    /// An audio packet named a track that does not exist.
    #[error("audio track {0} does not exist")]
    BadTrack(usize),
    /// The muxer configuration is unusable.
    #[error("invalid muxer config: {0}")]
    InvalidConfig(String),
    /// A file being read is malformed or exceeds a limit.
    #[error("invalid mp4 data: {0}")]
    InvalidData(String),
}
