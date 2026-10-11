//! Pipeline result types and the running native capture handle.
//!
//! The ffmpeg capture pipeline was removed (D58): the recorder always uses the
//! native encoder in [`crate::native_pipeline`].

#[cfg(windows)]
use std::io;

use clipcheck_encode::pipe::EncodeReport;

#[cfg(windows)]
use crate::native_pipeline::NativeCapture;

/// Summary of a finished pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineReport {
    /// Frames received from the input channel.
    pub frames_in: u64,
    /// Frames not encoded: backlog overflow drops plus malformed frames
    /// (pixel buffer length does not match its declared size).
    pub dropped: u64,
    /// Report from the encoder after it has been finished.
    pub encode: EncodeReport,
    /// Audio blocks dropped, summed over all tracks, because a track's backlog
    /// exceeded two seconds.
    pub audio_dropped: u64,
}

/// A running window capture backed by the native in-process encoder.
/// Created by the recorder.
#[cfg(windows)]
pub struct RunningCapture {
    native: NativeCapture,
}

#[cfg(windows)]
impl RunningCapture {
    /// Stops the capture first (so no more frames are produced), then finishes
    /// the encoder.
    pub fn stop(self) -> io::Result<PipelineReport> {
        self.native.stop()
    }

    /// Wraps a started native capture.
    pub(crate) fn from_native(native: NativeCapture) -> RunningCapture {
        RunningCapture { native }
    }
}
