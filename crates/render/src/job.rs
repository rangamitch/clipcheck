//! Job control and progress (pure). Final contract.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Shared flags a running export checks every frame and audio block.
#[derive(Debug, Clone, Default)]
pub struct JobControl {
    cancel: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    /// Frames rendered so far, for speed readouts.
    frames: Arc<AtomicU64>,
}

impl JobControl {
    /// New control with nothing set.
    pub fn new() -> JobControl {
        JobControl::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Sets or clears the pause flag.
    pub fn set_paused(&self, paused: bool) {
        self.pause.store(paused, Ordering::SeqCst);
    }

    /// Whether paused.
    pub fn is_paused(&self) -> bool {
        self.pause.load(Ordering::SeqCst)
    }

    /// Adds to the rendered-frame counter.
    pub fn add_frames(&self, n: u64) {
        self.frames.fetch_add(n, Ordering::Relaxed);
    }

    /// Frames rendered so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

/// One progress report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// Output time reached, seconds.
    pub out_secs: f64,
    /// Total output length, seconds.
    pub total_secs: f64,
    /// 0..=100.
    pub percent: f32,
    /// Output frames written.
    pub frames: u64,
    /// True while paused by [`JobControl::set_paused`].
    pub paused: bool,
}
