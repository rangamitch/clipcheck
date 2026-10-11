//! clipcheck-testkit: dev-only generator of small, deterministic test media, natively
//! (Media Foundation H.264 software encoder + AAC encoder + `clipcheck-mp4`), replacing the
//! `ffmpeg -f lavfi ...` fixtures (D58, D62).
//!
//! Output matches the real recorder: fragmented `seg-NNNNNN.mp4` + `segments.csv`
//! ([`write_segments`]) or one flat faststart MP4 ([`write_mp4`]).
//!
//! Not covered: HEVC, MKV/other containers, non-AAC audio, B-frames, audio-only files,
//! sample rates other than 44100/48000, more than 2 channels, corrupt files (truncate the
//! output yourself).

use std::path::{Path, PathBuf};

/// Errors from media generation.
#[derive(Debug, thiserror::Error)]
pub enum TestkitError {
    /// The spec is unusable.
    #[error("bad spec: {0}")]
    BadSpec(String),
    /// An encoder or muxer step failed.
    #[error("{0}")]
    Backend(String),
    /// File-system error.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Not supported on this platform.
    #[error("testkit media generation needs Windows (Media Foundation)")]
    Unsupported,
}

/// H.264 video track (software encoder, NV12 input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSpec {
    /// Width in pixels (even).
    pub width: u32,
    /// Height in pixels (even).
    pub height: u32,
    /// Frames per second (> 0).
    pub fps: u32,
    /// Keyframe interval in frames (> 0).
    pub gop_frames: u32,
}

/// One AAC-LC audio track carrying a sine wave.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSpec {
    /// 44100 or 48000.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u16,
    /// Sine frequency in Hz.
    pub freq_hz: f32,
    /// Track title (`udta/name`), read back as `Track::title`.
    pub title: Option<String>,
}

impl AudioSpec {
    /// 48 kHz stereo sine at `freq_hz`, no title.
    pub fn sine(freq_hz: f32) -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            freq_hz,
            title: None,
        }
    }

    /// Same with a track title.
    pub fn titled(freq_hz: f32, title: &str) -> Self {
        Self {
            title: Some(title.to_string()),
            ..Self::sine(freq_hz)
        }
    }
}

/// What to generate.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSpec {
    /// Duration in seconds (video frame count = round(secs * fps)).
    pub secs: f64,
    /// Video track; required (the segment muxer has no audio-only mode).
    pub video: Option<VideoSpec>,
    /// Audio tracks (may be empty).
    pub audio: Vec<AudioSpec>,
}

impl MediaSpec {
    /// 640x360, 30 fps, GOP 30, one 48 kHz stereo 440 Hz track.
    pub fn h264_aac(secs: f64) -> Self {
        Self {
            secs,
            video: Some(VideoSpec {
                width: 640,
                height: 360,
                fps: 30,
                gop_frames: 30,
            }),
            audio: vec![AudioSpec::sine(440.0)],
        }
    }

    /// Replaces the video size and fps (GOP = fps, i.e. a keyframe per second).
    pub fn with_video(mut self, width: u32, height: u32, fps: u32) -> Self {
        self.video = Some(VideoSpec {
            width,
            height,
            fps,
            gop_frames: fps,
        });
        self
    }

    /// Replaces the audio tracks.
    pub fn with_audio(mut self, audio: Vec<AudioSpec>) -> Self {
        self.audio = audio;
        self
    }
}

#[cfg(windows)]
mod gen;
pub mod tone;

/// True when the software H.264 encoder and the AAC encoder can be opened; tests should print a
/// reason and return early otherwise.
pub fn available() -> bool {
    #[cfg(windows)]
    {
        gen::available()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Writes a fragmented segment set exactly like the native recorder (`seg-000000.mp4`, ... and
/// `segments.csv`) into `dir` and returns the segment paths.
pub fn write_segments(
    dir: &Path,
    spec: &MediaSpec,
    segment_secs: f64,
) -> Result<Vec<PathBuf>, TestkitError> {
    #[cfg(windows)]
    {
        gen::write_segments(dir, spec, segment_secs)
    }
    #[cfg(not(windows))]
    {
        let _ = (dir, spec, segment_secs);
        Err(TestkitError::Unsupported)
    }
}

/// Writes one flat, faststart MP4 at `path` (segments in a temp dir, then
/// `clipcheck_mp4::remux::concat_remux`). Missing parent directories are created.
pub fn write_mp4(path: &Path, spec: &MediaSpec) -> Result<(), TestkitError> {
    #[cfg(windows)]
    {
        use clipcheck_mp4::remux::{concat_remux, RemuxOptions};
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = tempfile::tempdir()?;
        let segs = gen::write_segments(tmp.path(), spec, 2.0)?;
        concat_remux(&segs, path, &RemuxOptions::default())
            .map_err(|e| TestkitError::Backend(format!("remux: {e}")))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (path, spec);
        Err(TestkitError::Unsupported)
    }
}
