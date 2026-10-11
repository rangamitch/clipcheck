//! Types shared by the native encoder and the engine: the segment list file
//! name, the audio track description and the session report.

use std::path::PathBuf;

/// Name of the segment list (CSV, one row per closed segment) written next to the segments.
pub const SEGMENT_LIST_FILE: &str = "segments.csv";

/// One audio track muxed as an AAC stream next to the video.
///
/// Audio PCM is pushed to the encoder session
/// (48 kHz interleaved stereo f32, not resampled).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrackSpec {
    /// Stream title, stored as the `title` tag, e.g. `"Game"`, `"Mic"`, `"Mix"`.
    /// Empty means no title tag.
    pub title: String,
    /// AAC bitrate in kbit/s. Must be non-zero. Default 160.
    pub bitrate_kbps: u32,
}

impl Default for AudioTrackSpec {
    fn default() -> Self {
        Self {
            title: String::new(),
            bitrate_kbps: 160,
        }
    }
}

/// Result of a finished encoding session.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodeReport {
    /// Number of frames accepted by `push_frame`.
    pub frames_in: u64,
    /// Segment files in `out_dir`, sorted by name.
    pub segments: Vec<PathBuf>,
    /// True if the encoder finished cleanly before the finish timeout.
    pub exit_ok: bool,
    /// Last few KiB of encoder diagnostics.
    pub stderr_tail: String,
}
