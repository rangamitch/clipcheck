//! Render errors.

use thiserror::Error;

/// Why a render or export failed.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum RenderError {
    /// The cancel flag was set. Partial output has been removed.
    #[error("cancelled")]
    Cancelled,
    /// Bad settings or an empty range (message says which).
    #[error("invalid export: {0}")]
    Invalid(String),
    /// Reading a source file failed in a way that stops the export.
    #[error("source error: {0}")]
    Source(String),
    /// The encoder could not be opened or failed.
    #[error("encoder error: {0}")]
    Encode(String),
    /// Writing the output failed.
    #[error("output error: {0}")]
    Output(String),
    /// Not implemented yet.
    #[error("not implemented: {0}")]
    Unsupported(&'static str),
}
