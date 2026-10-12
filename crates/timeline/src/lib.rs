//! `clipcheck-timeline`: the pure video-editor model.
//!
//! Exact integer time ([`time::Tick`]), the project data model, edit operations, a command/undo
//! session, the render plan and export maths. No I/O, no OS or UI dependency (layering rule M3).
//! See `docs/design/13-video-editor.md` and `docs/impl/editor.md`.

#![forbid(unsafe_code)]
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

pub mod audiomath;
pub mod cmd;
pub mod error;
pub mod export;
pub mod json;
pub mod model;
pub mod ops_basic;
pub mod ops_group;
pub mod ops_props;
pub mod ops_range;
pub mod ops_trim;
pub mod plan;
pub mod serde_util;
pub mod snap;
pub mod time;

pub use cmd::{Applied, Command, Session};
pub use error::{EditError, EditResult};
pub use model::*;
pub use time::{Rate, Tick, MAX_PROJECT_TICKS, MAX_TICK, TICKS_PER_SECOND};
