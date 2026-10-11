//! clipcheck-engine: glue between capture, encode, buffer and session.
pub mod audiofeed;
pub mod audiomix;
pub mod canvas;
pub mod catalog;
pub mod import;
#[cfg(windows)]
pub mod native_pipeline;
pub mod pipeline;
pub mod procs;
pub mod recorder;
pub mod scan;
pub mod sessionfile;
pub(crate) mod sweep;
pub mod tracker;
pub mod window_size;
