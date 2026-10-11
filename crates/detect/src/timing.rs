//! Timing constants for game detection, in one place (docs/design/01-game-detection.md 5.1, 5.12).
//!
//! Worst-case stop latency is `LIVENESS_POLL_MS` (the loop notices the loss)
//! plus, for a destroyed window only, `WINDOW_GONE_GRACE_MS`. A process exit has
//! no grace at all.

/// Interval between cheap liveness checks of the bound target while a game is
/// recording or ending. The full window scan keeps its own, slower cadence
/// (`detect_poll_ms`).
pub const LIVENESS_POLL_MS: u64 = 250;

/// Grace after the bound window is destroyed while its process still runs
/// (loading screens, fullscreen toggles, launcher hand-off). A window of the same
/// game that appears within this time keeps the session (`GameRelaunched`).
/// `detect.stop_grace_ms` caps it when that value is smaller.
pub const WINDOW_GONE_GRACE_MS: u32 = 1_000;
