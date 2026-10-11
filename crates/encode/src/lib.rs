//! clipcheck-encode (see docs/design)

#[cfg(windows)]
pub mod native;
pub mod pipe;
pub use pipe::SEGMENT_LIST_FILE;
pub mod probe;
pub mod select;
pub mod tuning;
