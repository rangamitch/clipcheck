//! clipcheck-detect: pure game-detection logic.
//!
//! This crate performs no Windows API calls. The Windows probe layer feeds it
//! plain-data [`CandidateSnapshot`]s and monotonic-ish millisecond timestamps;
//! it answers with [`score::Verdict`]s and [`debounce::DetectEvent`]s.
//! See `docs/design/01-game-detection.md`.

pub mod debounce;
pub mod known_games;
pub mod library;
pub mod lists;
pub mod score;
pub mod timing;

use serde::{Deserialize, Serialize};

pub use debounce::{Debouncer, DetectEvent, Liveness, Pick, StartReason, StopReason};
pub use lists::Lists;
pub use score::{evaluate, evaluate_with, FullscreenKind, GateReason, Reason, Verdict};

/// Axis-aligned rectangle in physical pixels (right/bottom exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge.
    pub right: i32,
    /// Bottom edge.
    pub bottom: i32,
}

impl Rect {
    /// Creates a rectangle from its position and size.
    pub fn from_size(left: i32, top: i32, width: i32, height: i32) -> Self {
        Self {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    /// Width in pixels (never negative).
    pub fn width(&self) -> i64 {
        i64::from((self.right - self.left).max(0))
    }

    /// Height in pixels (never negative).
    pub fn height(&self) -> i64 {
        i64::from((self.bottom - self.top).max(0))
    }

    /// Area in square pixels.
    pub fn area(&self) -> i64 {
        self.width() * self.height()
    }

    /// Area of the intersection with `other`.
    pub fn intersection_area(&self, other: &Rect) -> i64 {
        let w = i64::from((self.right.min(other.right) - self.left.max(other.left)).max(0));
        let h = i64::from((self.bottom.min(other.bottom) - self.top.max(other.top)).max(0));
        w * h
    }
}

/// Identifies the window (and owning process) that capture would bind to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureTarget {
    /// Owning process id.
    pub pid: u32,
    /// Window handle, as an opaque integer.
    pub hwnd: u64,
}

/// Plain-data snapshot of one candidate window, produced by the probe layer.
///
/// All exe names are compared case-insensitively by this crate; callers may
/// pass them in any case.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateSnapshot {
    /// Executable file name, e.g. `"Game.exe"`.
    pub exe_name: String,
    /// Window title (may be empty).
    pub window_title: String,
    /// Window rectangle in screen pixels.
    pub window_rect: Rect,
    /// Rectangle of the monitor the window is mostly on.
    pub monitor_rect: Rect,
    /// Capture target identity for this window.
    pub target: CaptureTarget,
    /// Window is visible (`IsWindowVisible`).
    pub is_visible: bool,
    /// Window is minimized.
    pub is_minimized: bool,
    /// Window is DWM-cloaked.
    pub is_cloaked: bool,
    /// Window is a tool window (`WS_EX_TOOLWINDOW`).
    pub is_tool_window: bool,
    /// Window has a caption/title bar (used for NearFullscreen).
    pub has_caption: bool,
    /// Process is a known launcher (caller-side check; lists are also consulted).
    pub is_launcher: bool,
    /// This window's process is the primary of its process-tree group (5.5).
    pub is_primary_in_group: bool,
    /// Exe matches the known-games database.
    pub in_known_db: bool,
    /// Install directory of the exe is inside a library-scan entry.
    pub in_library_scan: bool,
    /// Rolling 5 s average of GPU 3D utilization for this pid, if measured.
    pub gpu_3d_load_pct: Option<f32>,
    /// Rolling 60 s average of GPU 3D utilization, if measured (penalty only).
    pub gpu_3d_avg60_pct: Option<f32>,
    /// Graphics API modules loaded in the process (e.g. `"d3d11.dll"`).
    pub loaded_graphics_modules: Vec<String>,
    /// Milliseconds since this window was last foreground, if ever.
    pub ms_since_foreground: Option<u64>,
    /// Candidate is the currently running (already started) game target.
    /// Running-only penalties apply only when this is true.
    pub is_running_game: bool,
    /// Milliseconds since the owning process started.
    pub process_age_ms: u64,
}

/// One allowlist entry (`detect.allow` / manual add).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowRule {
    /// Executable file name.
    pub exe: String,
    /// Optional display name shown in the UI.
    pub display_name: Option<String>,
}

/// Detection configuration (the subset of `detect.*` keys used by this crate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectConfig {
    /// Debounce before a game starts recording.
    pub start_confirm_ms: u32,
    /// Grace period after liveness failure before stopping (also relaunch window).
    pub stop_grace_ms: u32,
    /// Eligibility threshold after gates.
    pub min_score: u8,
    /// Geometry match tolerance in pixels.
    pub fullscreen_tolerance_px: u8,
    /// GPU 3D percentage that earns the full GPU points.
    pub gpu_threshold_pct: f32,
    /// Allowlisted (and manually added) games.
    pub allow: Vec<AllowRule>,
    /// Extra exe names added to the built-in blocklist.
    pub block_extra: Vec<String>,
    /// Allowlist-only mode: only pinned (allowlisted) apps are eligible.
    ///
    /// Auto mode (`false`) still records any accepted game. A pinned app wins
    /// the single slot over unpinned candidates whenever the slot is free or
    /// not yet bound; it does not displace a game that is already recording
    /// (other games never displace a live recording, see 5.6). A recording of
    /// an unpinned game is released on the next tick when this is `true`.
    #[serde(default)]
    pub allowlist_only: bool,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            start_confirm_ms: 3000,
            stop_grace_ms: 10_000,
            min_score: 60,
            fullscreen_tolerance_px: 2,
            gpu_threshold_pct: 15.0,
            allow: Vec::new(),
            block_extra: Vec::new(),
            allowlist_only: false,
        }
    }
}

#[cfg(windows)]
pub mod enrich;
#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod modules;
#[cfg(windows)]
pub mod runner;
#[cfg(windows)]
pub mod win;
