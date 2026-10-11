//! Native capture -> encode pipeline (D57): GPU frames from `WindowCapturer::start_gpu` go
//! straight into `NativeEncoder::push_texture` on a worker thread. No CPU readback, no child process.
//! Writes `seg-NNNNNN.mp4` + `segments.csv` and reports a [`crate::pipeline::PipelineReport`].
//!
//! The GPU capture hands out frames that reference a small texture ring, so the worker keeps only
//! the newest [`MAX_QUEUED_FRAMES`] frames and drops older ones when it falls behind.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use clipcheck_audio::{OUT_CHANNELS, OUT_RATE};
use clipcheck_capture::{CaptureConfig, GpuFrame, WindowCapturer};
use clipcheck_encode::native::{native_target, NativeEncoder, NativeParams};
use clipcheck_encode::pipe::AudioTrackSpec;
use clipcheck_encode::tuning::{EncoderTuning, RateMode, Speed};
use clipcheck_hwenc::RateControl;

use crate::pipeline::PipelineReport;

/// How long the worker blocks waiting for a frame before re-checking `stop`.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Frames kept queued in front of the encoder. The capture ring has four textures, so older
/// frames may already be overwritten and are dropped.
const MAX_QUEUED_FRAMES: usize = 2;
/// Audio backlog limit per track, in samples (2 s of interleaved 48 kHz audio).
const MAX_AUDIO_BACKLOG_SAMPLES: usize = 2 * OUT_RATE as usize * OUT_CHANNELS;
/// Audio tracks handed to the native pipeline: each track's spec and its sample channel.
pub type AudioInputs = Vec<(AudioTrackSpec, Receiver<Vec<f32>>)>;
/// Segment length in seconds.
const SEGMENT_SECS: f32 = 2.0;

/// Builds the native encoder settings for an encoder id (such as `h264_nvenc`). `None` when the name has no
/// native equivalent (see [`native_target`]).
///
/// The canvas is rounded down to even sizes. Rate control maps as follows (D57): the MF vendor
/// MFTs do not all honour constant quality, so the quality modes become a peak-constrained VBR
/// with average 70% of the cap.
pub fn native_params(
    encoder_name: &str,
    fps: u32,
    canvas: (u32, u32),
    bitrate_kbps: u32,
    tuning: &EncoderTuning,
    out_dir: PathBuf,
) -> Option<NativeParams> {
    let target = native_target(encoder_name)?;
    let rate = match tuning.rate {
        RateMode::Cbr => RateControl::Cbr { kbps: bitrate_kbps },
        RateMode::Vbr { max_kbps } => RateControl::Vbr {
            kbps: bitrate_kbps,
            peak_kbps: max_kbps.max(bitrate_kbps),
        },
        RateMode::Quality { .. } => vbr_from_cap(bitrate_kbps),
        RateMode::QualityPeak { max_kbps, .. } => vbr_from_cap(max_kbps),
    };
    let quality_vs_speed = match tuning.speed {
        Speed::Fastest => 20,
        Speed::Balanced => 50,
        Speed::Quality => 80,
    };
    Some(NativeParams {
        out_dir,
        width: canvas.0 & !1,
        height: canvas.1 & !1,
        fps,
        target,
        rate,
        quality_vs_speed,
        segment_secs: SEGMENT_SECS,
        wall_clock: true,
    })
}

/// Peak-constrained VBR with a peak of `cap` kbit/s and an average of 70% of it.
fn vbr_from_cap(cap: u32) -> RateControl {
    RateControl::Vbr {
        kbps: cap.saturating_mul(7) / 10,
        peak_kbps: cap,
    }
}

/// A running native capture: a GPU window capture feeding an in-process encoder.
pub struct NativeCapture {
    capturer: Option<WindowCapturer>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<io::Result<PipelineReport>>>,
}

/// Starts a GPU window capture at `fps` and a native encoder with `params`, with one AAC track
/// per `audio` entry.
///
/// On failure the audio lanes are handed back so the caller can use them for another encoder.
/// They come back empty only when the worker thread could not be spawned, because the lanes
/// were already moved into it.
pub fn start_native_capture(
    hwnd: isize,
    fps: u32,
    params: NativeParams,
    audio: AudioInputs,
) -> Result<NativeCapture, (io::Error, AudioInputs)> {
    let specs: Vec<AudioTrackSpec> = audio.iter().map(|(spec, _)| spec.clone()).collect();
    // Audio pts are counted from the encoder's start, like the wall-clock video. Blocks queued
    // before that predate the timeline; keeping them would shift audio ahead of video.
    for (_, rx) in &audio {
        while rx.try_recv().is_ok() {}
    }
    let encoder = match NativeEncoder::start(params, None, &specs) {
        Ok(encoder) => encoder,
        Err(e) => return Err((e, audio)),
    };
    let device = encoder.device().clone();
    let (capturer, frames) = match WindowCapturer::start_gpu(CaptureConfig { hwnd, fps }, &device) {
        Ok(started) => started,
        Err(e) => {
            let _ = encoder.finish();
            return Err((io::Error::other(e.to_string()), audio));
        }
    };

    let lanes: Vec<AudioLane> = audio
        .into_iter()
        .map(|(_, rx)| AudioLane::new(rx))
        .collect();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = stop.clone();
        thread::Builder::new()
            .name("clipcheck-native-pipeline".into())
            .spawn(move || run_worker(frames, encoder, stop, lanes))
    };
    match handle {
        Ok(handle) => Ok(NativeCapture {
            capturer: Some(capturer),
            stop,
            handle: Some(handle),
        }),
        Err(e) => {
            capturer.stop();
            Err((e, Vec::new()))
        }
    }
}

impl NativeCapture {
    /// Stops the capture first (so no more frames are produced), then stops the worker, which
    /// drains the queued frames and finishes the encoder.
    pub fn stop(mut self) -> io::Result<PipelineReport> {
        self.shutdown_capture();
        self.stop.store(true, Ordering::Release);
        // Only `stop` and `Drop` take the handle. `stop` consumes the value, so `Drop` has not run.
        let handle = self
            .handle
            .take()
            .expect("native pipeline handle is taken only by stop or drop");
        handle
            .join()
            .map_err(|_| io::Error::other("native pipeline thread panicked"))?
    }

    fn shutdown_capture(&mut self) {
        if let Some(capturer) = self.capturer.take() {
            capturer.stop();
        }
    }
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        self.shutdown_capture();
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// One audio track on the worker side: its input channel and pending blocks.
struct AudioLane {
    rx: Receiver<Vec<f32>>,
    pending: VecDeque<Vec<f32>>,
    pending_samples: usize,
    disconnected: bool,
    dropped: u64,
}

impl AudioLane {
    fn new(rx: Receiver<Vec<f32>>) -> Self {
        Self {
            rx,
            pending: VecDeque::new(),
            pending_samples: 0,
            disconnected: false,
            dropped: 0,
        }
    }
}

/// Moves every block waiting on each audio channel into the lane's pending queue, drops the
/// oldest blocks beyond the backlog limit, then writes the rest to the encoder. Never blocks.
fn pump_audio(lanes: &mut [AudioLane], encoder: &mut NativeEncoder) -> io::Result<()> {
    for (track, lane) in lanes.iter_mut().enumerate() {
        if !lane.disconnected {
            loop {
                match lane.rx.try_recv() {
                    Ok(block) => {
                        lane.pending_samples += block.len();
                        lane.pending.push_back(block);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        lane.disconnected = true;
                        break;
                    }
                }
            }
        }
        while lane.pending_samples > MAX_AUDIO_BACKLOG_SAMPLES {
            match lane.pending.pop_front() {
                Some(block) => {
                    lane.pending_samples -= block.len();
                    lane.dropped += 1;
                }
                None => break,
            }
        }
        while let Some(block) = lane.pending.pop_front() {
            lane.pending_samples -= block.len();
            encoder.push_audio(track, &block)?;
        }
    }
    Ok(())
}

/// Pushes one captured frame: its GPU texture, or black when the frame has none or is black.
fn encode_one(encoder: &mut NativeEncoder, frame: GpuFrame) -> io::Result<()> {
    let GpuFrame {
        texture, is_black, ..
    } = frame;
    match texture {
        Some(tex) if !is_black => encoder.push_texture(&tex.0),
        _ => encoder.push_black(),
    }
}

fn run_worker(
    frames: Receiver<GpuFrame>,
    mut encoder: NativeEncoder,
    stop: Arc<AtomicBool>,
    mut lanes: Vec<AudioLane>,
) -> io::Result<PipelineReport> {
    let mut queue: VecDeque<GpuFrame> = VecDeque::new();
    let mut frames_in: u64 = 0;
    let mut dropped: u64 = 0;
    let mut closed = false;
    let mut failure: Option<io::Error> = None;

    loop {
        let stopping = stop.load(Ordering::Acquire);

        // Nothing queued: wait briefly for the next frame, then re-check stop.
        if queue.is_empty() && !stopping && !closed {
            match frames.recv_timeout(POLL_INTERVAL) {
                Ok(frame) => {
                    queue.push_back(frame);
                    frames_in += 1;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => closed = true,
            }
        }

        // Pull everything already waiting into the local queue.
        if !closed {
            loop {
                match frames.try_recv() {
                    Ok(frame) => {
                        queue.push_back(frame);
                        frames_in += 1;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        closed = true;
                        break;
                    }
                }
            }
        }

        // Keep only the newest frames: older ones may already be overwritten in the ring.
        while queue.len() > MAX_QUEUED_FRAMES {
            queue.pop_front();
            dropped += 1;
        }

        if let Err(e) = pump_audio(&mut lanes, &mut encoder) {
            failure = Some(e);
            break;
        }

        match queue.pop_front() {
            Some(frame) => {
                if let Err(e) = encode_one(&mut encoder, frame) {
                    failure = Some(e);
                    break;
                }
            }
            None => {
                if closed || stopping {
                    break;
                }
            }
        }
    }

    // Write the audio already delivered, so the tracks end with the video.
    if failure.is_none() {
        if let Err(e) = pump_audio(&mut lanes, &mut encoder) {
            failure = Some(e);
        }
    }
    let audio_dropped = lanes.iter().map(|lane| lane.dropped).sum();
    let encoder_dropped = encoder.frames_dropped();

    // Always finish the encoder so its segments are closed even on error.
    let encode = encoder.finish();
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(PipelineReport {
        frames_in,
        dropped: dropped + encoder_dropped,
        encode: encode?,
        audio_dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_for(
        rate: RateMode,
        speed: Speed,
        canvas: (u32, u32),
        name: &str,
    ) -> Option<NativeParams> {
        let tuning = EncoderTuning {
            rate,
            speed,
            ..EncoderTuning::default()
        };
        native_params(name, 60, canvas, 6000, &tuning, PathBuf::from("out"))
    }

    #[test]
    fn unknown_encoder_has_no_native_params() {
        assert!(params_for(RateMode::Cbr, Speed::Balanced, (1920, 1080), "libx265").is_none());
        assert!(params_for(RateMode::Cbr, Speed::Balanced, (1920, 1080), "av1_nvenc").is_none());
    }

    #[test]
    fn canvas_is_rounded_down_to_even() {
        let p = params_for(RateMode::Cbr, Speed::Balanced, (1921, 1081), "libx264").unwrap();
        assert_eq!((p.width, p.height), (1920, 1080));
        assert_eq!(p.fps, 60);
        assert_eq!(p.segment_secs, 2.0);
        assert!(p.wall_clock);
    }

    #[test]
    fn cbr_and_vbr_map_directly() {
        let p = params_for(RateMode::Cbr, Speed::Balanced, (1920, 1080), "h264_nvenc").unwrap();
        assert_eq!(p.rate, RateControl::Cbr { kbps: 6000 });
        let p = params_for(
            RateMode::Vbr { max_kbps: 9000 },
            Speed::Balanced,
            (1920, 1080),
            "h264_nvenc",
        )
        .unwrap();
        assert_eq!(
            p.rate,
            RateControl::Vbr {
                kbps: 6000,
                peak_kbps: 9000
            }
        );
        // A peak below the average is raised to the average.
        let p = params_for(
            RateMode::Vbr { max_kbps: 1000 },
            Speed::Balanced,
            (1920, 1080),
            "h264_nvenc",
        )
        .unwrap();
        assert_eq!(
            p.rate,
            RateControl::Vbr {
                kbps: 6000,
                peak_kbps: 6000
            }
        );
    }

    #[test]
    fn quality_modes_become_peak_vbr() {
        let p = params_for(
            RateMode::Quality { cq: 23 },
            Speed::Balanced,
            (1920, 1080),
            "libx264",
        )
        .unwrap();
        assert_eq!(
            p.rate,
            RateControl::Vbr {
                kbps: 4200,
                peak_kbps: 6000
            }
        );
        let p = params_for(
            RateMode::QualityPeak {
                cq: 23,
                max_kbps: 10000,
            },
            Speed::Balanced,
            (1920, 1080),
            "libx264",
        )
        .unwrap();
        assert_eq!(
            p.rate,
            RateControl::Vbr {
                kbps: 7000,
                peak_kbps: 10000
            }
        );
    }

    #[test]
    fn speed_maps_to_quality_vs_speed() {
        let q = |s| {
            params_for(RateMode::Cbr, s, (1920, 1080), "libx264")
                .unwrap()
                .quality_vs_speed
        };
        assert_eq!(q(Speed::Fastest), 20);
        assert_eq!(q(Speed::Balanced), 50);
        assert_eq!(q(Speed::Quality), 80);
    }
}
