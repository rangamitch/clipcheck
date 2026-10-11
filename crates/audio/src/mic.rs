//! Microphone (capture endpoint) capture.

use std::sync::mpsc::Receiver;

use windows::Win32::Media::Audio::eCapture;

use crate::engine::{spawn_capture, CaptureHandle};
use crate::loopback::open_mmdevice;
use crate::{AudioChunk, AudioError};

/// Capture from a microphone (capture endpoint).
pub struct Microphone {
    handle: CaptureHandle,
}

impl Microphone {
    /// Starts capture from the endpoint with the given ID, or the default
    /// capture endpoint when `device` is `None`.
    ///
    /// Returns `AudioError::NoDevice` when no matching endpoint exists.
    pub fn start(device: Option<String>) -> Result<(Self, Receiver<AudioChunk>), AudioError> {
        let (handle, rx) = spawn_capture("clipcheck-mic", move || {
            open_mmdevice(eCapture, device.as_deref(), false)
        })?;
        Ok((Self { handle }, rx))
    }

    /// Stops capture and joins the capture thread.
    pub fn stop(&mut self) {
        self.handle.stop();
    }

    /// The error that ended capture, if the capture thread stopped because of a
    /// device or conversion failure. `None` while running or after a normal stop.
    pub fn last_error(&self) -> Option<AudioError> {
        self.handle.error()
    }
}
