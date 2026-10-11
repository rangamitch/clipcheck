//! Microphone level metering: RMS and peak in dBFS over 50 ms windows.

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::thread::JoinHandle;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use crate::{AudioChunk, AudioError, Microphone, OUT_CHANNELS, OUT_RATE};

/// Lower bound of the reported range, also used for silence.
pub const SILENCE_DB: f32 = -100.0;

/// Length of one metering window in milliseconds.
pub const METER_WINDOW_MS: u64 = 50;

/// One metering window's levels, in dBFS (-100 = silence, 0 = full scale).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterReading {
    /// Root-mean-square level, -100..0 dBFS.
    pub rms_db: f32,
    /// Peak absolute sample level, -100..0 dBFS.
    pub peak_db: f32,
}

/// Converts a linear amplitude to dBFS, clamped to -100..0. Non-finite and
/// non-positive amplitudes map to -100.
fn amplitude_to_db(linear: f32) -> f32 {
    if !linear.is_finite() || linear <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * linear.log10()).clamp(SILENCE_DB, 0.0)
}

/// RMS level of `samples` in dBFS. Empty input is silence (-100).
pub fn rms_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return SILENCE_DB;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let mean = sum / samples.len() as f64;
    amplitude_to_db(mean.sqrt() as f32)
}

/// Peak absolute level of `samples` in dBFS. Empty input is silence (-100).
pub fn peak_db(samples: &[f32]) -> f32 {
    let peak = samples
        .iter()
        .filter(|s| s.is_finite())
        .fold(0.0f32, |m, &s| m.max(s.abs()));
    amplitude_to_db(peak)
}

/// Live microphone meter: captures from a capture endpoint and emits one
/// [`MeterReading`] per 50 ms of audio.
#[cfg(windows)]
pub struct MicMeter {
    mic: Option<Microphone>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl MicMeter {
    /// Starts metering the endpoint with the given ID, or the default capture
    /// endpoint when `device` is `None`. Returns `AudioError::NoDevice` when no
    /// matching capture endpoint exists.
    pub fn start(device: Option<String>) -> Result<(Self, Receiver<MeterReading>), AudioError> {
        let (mic, chunks) = Microphone::start(device)?;
        let (tx, readings) = mpsc::channel::<MeterReading>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("clipcheck-meter".to_owned())
            .spawn(move || meter_loop(chunks, tx, &stop_thread))
            .map_err(|_| AudioError::ThreadSpawn)?;
        Ok((
            Self {
                mic: Some(mic),
                stop,
                thread: Some(thread),
            },
            readings,
        ))
    }

    /// The capture error that ended metering, if the microphone stopped because of
    /// a device or conversion failure. `None` while running or after a normal stop.
    pub fn last_error(&self) -> Option<AudioError> {
        self.mic.as_ref().and_then(Microphone::last_error)
    }

    /// Stops capture and joins the meter thread. Safe to call more than once.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut mic) = self.mic.take() {
            mic.stop();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(windows)]
impl Drop for MicMeter {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Accumulates capture chunks into 50 ms windows and sends a reading per window.
#[cfg(windows)]
fn meter_loop(chunks: Receiver<AudioChunk>, out: Sender<MeterReading>, stop: &AtomicBool) {
    let window = OUT_RATE as usize * OUT_CHANNELS * METER_WINDOW_MS as usize / 1000;
    let mut acc: Vec<f32> = Vec::with_capacity(window * 2);
    let timeout = Duration::from_millis(METER_WINDOW_MS);
    while !stop.load(Ordering::Relaxed) {
        match chunks.recv_timeout(timeout) {
            Ok(chunk) => {
                acc.extend_from_slice(&chunk.samples);
                while acc.len() >= window {
                    let reading = MeterReading {
                        rms_db: rms_db(&acc[..window]),
                        peak_db: peak_db(&acc[..window]),
                    };
                    acc.drain(..window);
                    if out.send(reading).is_err() {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_minus_100() {
        assert_eq!(rms_db(&[0.0; 64]), SILENCE_DB);
        assert_eq!(peak_db(&[0.0; 64]), SILENCE_DB);
        assert_eq!(rms_db(&[]), SILENCE_DB);
    }

    #[test]
    fn full_scale_is_zero_db() {
        assert!((peak_db(&[1.0, -1.0]) - 0.0).abs() < 1e-4);
    }
}
