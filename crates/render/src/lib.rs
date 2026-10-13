//! `clipcheck-render`: the editor's export engine (docs/design/13-video-editor.md section 8).
//!
//! Layout:
//! - pure, testable anywhere: [`frame`], [`dsp`], [`reference`] (CPU compositor), [`pipeline`]
//!   (drives providers and a sink from a render plan), [`job`] (control flags, progress).
//! - Windows only: `engine` (Media Foundation decode, hardware/software encode, MP4 mux),
//!   `sources`, `titles`, `smart`, `still`.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]

pub mod dsp;
pub mod error;
pub mod frame;
pub mod job;
pub mod nv12;
pub mod pipeline;
pub mod reference;

#[cfg(windows)]
pub mod engine;
#[cfg(windows)]
pub mod gpuenc;
#[cfg(windows)]
pub mod smart;
#[cfg(windows)]
pub mod sources;
#[cfg(windows)]
pub mod still;
#[cfg(windows)]
pub mod titles;

pub use error::RenderError;
pub use frame::Frame;
#[cfg(windows)]
pub use gpuenc::{adapter_info, AdapterInfo};
pub use job::{JobControl, Progress};
