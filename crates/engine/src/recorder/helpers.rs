//! Helpers for the recorder: tracker sink, session construction, error
//! mapping, locking and the platform capture shims.

use std::io;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use std::sync::mpsc::Receiver;

use clipcheck_buffer::ClipPlanError;
#[cfg(windows)]
use clipcheck_capture::CaptureBackend;
use clipcheck_encode::pipe::AudioTrackSpec;

use super::{ActiveSession, CaptureHandle, RecorderError, RecorderEvent};
use crate::procs::lock;
use crate::sessionfile::SessionAssembler;
use crate::tracker::CompletedSegment;

use super::RecorderConfig;
#[cfg(windows)]
use crate::native_pipeline::{native_params, start_native_capture_with};
#[cfg(windows)]
use crate::pipeline::RunningCapture;

/// Builds the tracker callback. Each completed segment is staged into the
/// active session, if there is one.
pub(super) fn segment_sink(
    session: Arc<Mutex<Option<ActiveSession>>>,
    events: Sender<RecorderEvent>,
) -> Box<dyn Fn(&CompletedSegment) + Send> {
    Box::new(move |seg| {
        let mut slot = lock(&session);
        if let Some(active) = slot.as_mut() {
            if let Err(e) = active.asm.add_segment(&seg.path) {
                let _ = events.send(RecorderEvent::Error(format!(
                    "could not stage segment {}: {e}",
                    seg.seq
                )));
            }
        }
    })
}

/// Creates the assembler for part `part` of session `seq`, in its own stage dir
/// under the run directory. A separate directory per part keeps `SessionAssembler`
/// from adopting leftovers from an earlier part.
pub(super) fn new_session(
    run_dir: &Path,
    seq: u32,
    part: u32,
) -> Result<ActiveSession, RecorderError> {
    let stage = run_dir.join(format!("stage-s{seq}-p{part}"));
    let asm = SessionAssembler::new(stage).map_err(io_err)?;
    Ok(ActiveSession {
        asm,
        part_index: part,
    })
}

/// Maps a plan failure to a recorder error.
pub(super) fn plan_err(e: ClipPlanError) -> RecorderError {
    match e {
        ClipPlanError::Empty => RecorderError::BufferEmpty,
        ClipPlanError::InvalidDuration => {
            RecorderError::InvalidRequest("invalid clip duration".to_string())
        }
    }
}

pub(super) fn io_err(e: io::Error) -> RecorderError {
    RecorderError::Io(e.to_string())
}

/// Audio tracks handed to the pipeline: each track's spec and its sample channel.
pub(super) type AudioLanes = Vec<(AudioTrackSpec, Receiver<Vec<f32>>)>;

#[cfg(windows)]
pub(super) fn start_capture(
    cfg: &RecorderConfig,
    hwnd: isize,
    run_dir: &Path,
    audio: AudioLanes,
    canvas: (u32, u32),
) -> Result<CaptureHandle, RecorderError> {
    // No fallback encoder (D62): a failed native start is the recording's error.
    let params = native_params(
        &cfg.encoder_name,
        cfg.fps,
        // The canvas chosen from the window's aspect (choose_canvas), not the 16:9 fallback.
        canvas,
        cfg.bitrate_kbps,
        &cfg.tuning,
        run_dir.to_path_buf(),
    )
    .ok_or_else(|| {
        RecorderError::Io(format!(
            "encoder {} is not supported by the native encoder",
            cfg.encoder_name
        ))
    })?;
    let backend = if cfg.no_border_capture {
        CaptureBackend::NoBorder {
            require_cover: true,
        }
    } else {
        CaptureBackend::Wgc
    };
    match start_native_capture_with(hwnd, cfg.fps, params, audio, backend) {
        Ok(native) => Ok(RunningCapture::from_native(native)),
        Err((e, _lanes)) => Err(io_err(e)),
    }
}

#[cfg(not(windows))]
pub(super) fn start_capture(
    _cfg: &RecorderConfig,
    _hwnd: isize,
    _run_dir: &Path,
    _audio: AudioLanes,
    _canvas: (u32, u32),
) -> Result<CaptureHandle, RecorderError> {
    Err(RecorderError::Unsupported)
}

/// Process id owning the window `hwnd`, or 0 when it cannot be resolved.
#[cfg(windows)]
pub(super) fn window_pid(hwnd: isize) -> u32 {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid: u32 = 0;
    // SAFETY: GetWindowThreadProcessId accepts any HWND value. An invalid handle
    // returns 0 and leaves `pid` at 0. `pid` outlives the call.
    unsafe {
        GetWindowThreadProcessId(HWND(hwnd as *mut std::ffi::c_void), Some(&mut pid));
    }
    pid
}

/// Process id owning the window `hwnd`. Always 0 off Windows.
#[cfg(not(windows))]
pub(super) fn window_pid(_hwnd: isize) -> u32 {
    0
}

#[cfg(windows)]
pub(super) fn stop_capture(capture: CaptureHandle) -> io::Result<u64> {
    capture.stop().map(|report| report.dropped)
}

#[cfg(not(windows))]
pub(super) fn stop_capture(_capture: CaptureHandle) -> io::Result<u64> {
    Ok(0)
}
