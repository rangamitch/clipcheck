//! clipcheck-gpuconv: BGRA -> NV12 scale + letterbox for the native encoder path (D57).
//!
//! `d3d` does it on the GPU with the D3D11 video processor (zero copy); `cpu` is the
//! software fallback. Contracts: docs/impl/native-encoder.md, section "crates/gpuconv".

pub mod cpu;
#[cfg(windows)]
pub mod d3d;

pub use cpu::{bgra_to_nv12, letterbox_rect};

/// Errors from the converters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConvError {
    /// Sizes or buffer lengths are invalid.
    #[error("bad input: {0}")]
    BadInput(String),
    /// A Windows API call failed.
    #[error("{context} failed: {message}")]
    Windows {
        /// The API step that failed.
        context: &'static str,
        /// Error text from Windows.
        message: String,
    },
}
