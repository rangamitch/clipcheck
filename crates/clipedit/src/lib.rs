//! `clipcheck-clipedit`: clip editing logic.
//!
//! [`plan`] is the pure trim planner (no I/O). [`run`] executes a plan in
//! process (decision D26; native since D58: MP4 remux for lossless trims and
//! Media Foundation decode/encode for precise trims; no external tools).
//! See `docs/design/08-player-and-clip-editor.md`.

#![forbid(unsafe_code)]

pub mod plan;
pub mod run;

pub use plan::{
    format_hms, plan_trim, suggest_output_name, TrimError, TrimInput, TrimMode, TrimPlan,
    CLIP_TRIM_KIND, MIN_SELECTION_MS,
};
pub use run::{run_trim, TrimProgress, TrimRequest, TrimRunError};
