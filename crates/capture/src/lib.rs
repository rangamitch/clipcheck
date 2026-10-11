//! clipcheck-capture: Windows Graphics Capture of ONE window, emitted at constant fps.
//!
//! Frames are produced by a CFR pacer thread (see docs/design/02-video-capture.md):
//! - a new WGC frame since the last tick is emitted as a live frame;
//! - otherwise the last live frame is duplicated while the window is showing;
//! - once the window is minimized, hidden, cloaked or gone for longer than
//!   [`HIDDEN_GRACE`], black frames of the last good size are emitted at fps.
//!
//! Two output modes share that pacer: [`WindowCapturer::start`] emits CPU BGRA [`Frame`]s,
//! [`WindowCapturer::start_gpu`] emits [`GpuFrame`]s that are D3D11 texture copies (no readback).
//!
//! Only window captures are made; monitors and the desktop are rejected.

mod blackgen;
mod buffers;
mod dda;
pub mod dda_geom;
mod gpu;
mod wgc;

pub use gpu::{GpuFrame, SharedTexture};

/// Which source feeds the pacer. See docs/impl/no-border-capture.md (D64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureBackend {
    /// Windows Graphics Capture of the window (the default; draws the capture border on Windows 10).
    #[default]
    Wgc,
    /// Desktop Duplication of the window's monitor, cropped to the window: no capture border.
    NoBorder {
        /// Fail unless the window covers at least 95 % of its monitor (production passes true).
        require_cover: bool,
    },
}

/// How many times the no-border source has been entered in this process. Stays 0 unless a caller
/// asks for [`CaptureBackend::NoBorder`]; tests use it to prove the opt-in gate.
pub fn no_border_attempts() -> u64 {
    dda::attempts()
}

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long the window may be hidden before output switches to black.
pub const HIDDEN_GRACE: Duration = Duration::from_millis(250);

/// Minimum time between window-state polls. The pacer reuses the last answer until this has
/// passed; the first tick always polls.
const WINDOW_STATE_INTERVAL: Duration = Duration::from_millis(100);

/// Capacity of the frame channel. When full, the pacer drops the new frame instead of blocking.
const FRAME_QUEUE: usize = 4;

/// One captured frame, tightly packed BGRA (stride = width * 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel data, B G R A per pixel, row-major, no padding. Shared and immutable: duplicate and
    /// black ticks clone the `Arc`, not the pixels. Read it as a slice with `&frame.bgra[..]`
    /// or through deref (`frame.bgra.len()`, `frame.bgra[i]`).
    pub bgra: Arc<Vec<u8>>,
    /// Presentation time in 100 ns units since capture start (from QueryPerformanceCounter).
    pub pts_100ns: i64,
    /// True for black frames emitted while the window is hidden.
    pub is_black: bool,
}

/// Capture target and output rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Window handle (HWND) as an integer.
    pub hwnd: isize,
    /// Output frames per second, 1..=240.
    pub fps: u32,
}

/// Errors from starting a capture.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// The handle is not a valid window, or is the desktop or shell window.
    #[error("invalid capture target window {0:#x}")]
    InvalidWindow(isize),
    /// Windows Graphics Capture is not available on this system or session.
    #[error("Windows Graphics Capture is not supported on this system")]
    Unsupported,
    /// Requested fps outside 1..=240.
    #[error("fps must be between 1 and 240, got {0}")]
    InvalidFps(u32),
    /// A Windows API call failed.
    #[error("{context} failed: {message}")]
    Windows {
        /// The API step that failed.
        context: &'static str,
        /// Error text from Windows.
        message: String,
        /// The HRESULT Windows returned, when the failing call reported one.
        hresult: Option<i32>,
    },
    /// The capture thread exited before reporting its state.
    #[error("capture thread failed to start")]
    ThreadStart,
    /// The opt-in no-border source cannot run here; the caller should use window capture.
    #[error("no-border capture unavailable: {reason}")]
    NoBorderUnavailable {
        /// Why, in a sentence a user can read.
        reason: String,
    },
}

/// DXGI_ERROR_UNSUPPORTED: D3D11 has no hardware device on this machine.
const DXGI_ERROR_UNSUPPORTED: i32 = 0x887A_0004_u32 as i32;

impl CaptureError {
    /// True only when the failure means this machine or session cannot run graphics capture
    /// at all (no WGC support, or no hardware D3D11 device). Every other failure, including
    /// invalid windows and invalid fps, is a real error and must not be treated as a skip.
    pub fn is_environment(&self) -> bool {
        match self {
            CaptureError::Unsupported => true,
            CaptureError::Windows {
                context: "D3D11CreateDevice",
                hresult: Some(code),
                ..
            } => *code == DXGI_ERROR_UNSUPPORTED,
            _ => false,
        }
    }
}

/// Latest WGC frame and canvas size, written by the WGC callback and read by the pacer.
#[derive(Debug, Default)]
pub(crate) struct Latest {
    /// Incremented for every new frame. Zero means none yet.
    pub(crate) seq: u64,
    /// Width of the last good frame, or of the window at start.
    pub(crate) width: u32,
    /// Height of the last good frame, or of the window at start.
    pub(crate) height: u32,
    /// BGRA of the most recent frame (CPU mode). Empty when none has arrived or in GPU mode.
    /// Shared, so the pacer can take it under the lock by reference count alone and copy the
    /// pixels after unlocking.
    pub(crate) bgra: Arc<Vec<u8>>,
    /// Texture holding the most recent frame (GPU mode). `None` in CPU mode.
    pub(crate) texture: Option<SharedTexture>,
}

pub(crate) type Shared = Arc<Mutex<Latest>>;

pub(crate) fn lock(shared: &Shared) -> MutexGuard<'_, Latest> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Running capture. Dropping it (or calling [`WindowCapturer::stop`]) stops both threads.
pub struct WindowCapturer {
    stop: Arc<AtomicBool>,
    pacer: Option<JoinHandle<()>>,
    wgc: Option<wgc::WgcThread>,
    dda: Option<dda::DdaThread>,
    errors: Arc<AtomicU64>,
}

impl WindowCapturer {
    /// Like [`WindowCapturer::start`], but frames stay on the GPU: WGC runs on `device` and each
    /// frame is a BGRA texture copy on that device (no CPU readback). See docs/impl/native-encoder.md.
    pub fn start_gpu(
        cfg: CaptureConfig,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    ) -> Result<(WindowCapturer, Receiver<GpuFrame>), CaptureError> {
        Self::start_gpu_with(cfg, device, CaptureBackend::Wgc)
    }

    /// Like [`WindowCapturer::start_gpu`] with an explicit [`CaptureBackend`];
    /// `CaptureBackend::Wgc` is exactly `start_gpu`.
    pub fn start_gpu_with(
        cfg: CaptureConfig,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        backend: CaptureBackend,
    ) -> Result<(WindowCapturer, Receiver<GpuFrame>), CaptureError> {
        start_with(
            cfg,
            wgc::Source::Gpu(device.clone()),
            backend,
            gpu_pixels,
            gpu_emit(),
        )
    }

    /// Starts capturing the window in `cfg`. Frames arrive on the returned receiver at `cfg.fps`.
    pub fn start(cfg: CaptureConfig) -> Result<(WindowCapturer, Receiver<Frame>), CaptureError> {
        start_with(
            cfg,
            wgc::Source::Cpu,
            CaptureBackend::Wgc,
            cpu_pixels,
            cpu_emit(),
        )
    }

    /// Stops the pacer and the WGC session and waits for both threads to exit.
    pub fn stop(self) {
        drop(self);
    }

    /// Number of failures on the WGC frame path so far: failed frame fetches, failed
    /// readbacks, and poisoned locks. The first failure is also logged to stderr. A
    /// count that keeps rising means capture is failing, while the pacer repeats the last frame.
    pub fn callback_errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

/// Shared start path for both output modes: validates fps, starts WGC on `source`, and starts
/// the pacer, which hands `emit`'s items to the returned channel.
fn start_with<P: Clone + 'static, I: Send + 'static>(
    cfg: CaptureConfig,
    source: wgc::Source,
    backend: CaptureBackend,
    pixels: fn(&Latest) -> Option<P>,
    emit: impl FnMut(Tick<P>, i64) -> I + Send + 'static,
) -> Result<(WindowCapturer, Receiver<I>), CaptureError> {
    if cfg.fps == 0 || cfg.fps > 240 {
        return Err(CaptureError::InvalidFps(cfg.fps));
    }
    let shared: Shared = Arc::new(Mutex::new(Latest::default()));
    let errors = Arc::new(AtomicU64::new(0));
    // The no-border source is entered only for CaptureBackend::NoBorder; Wgc never touches `dda`.
    let (wgc, dda) = match (backend, source) {
        (CaptureBackend::Wgc, source) => (
            Some(wgc::spawn(
                cfg.hwnd,
                cfg.fps,
                shared.clone(),
                errors.clone(),
                source,
            )?),
            None,
        ),
        (CaptureBackend::NoBorder { require_cover }, wgc::Source::Gpu(device)) => (
            None,
            Some(dda::spawn(
                cfg.hwnd,
                cfg.fps,
                shared.clone(),
                errors.clone(),
                device,
                require_cover,
            )?),
        ),
        (CaptureBackend::NoBorder { .. }, wgc::Source::Cpu) => {
            return Err(CaptureError::NoBorderUnavailable {
                reason: "CPU capture mode does not support it".into(),
            })
        }
    };

    let (tx, rx) = mpsc::sync_channel::<I>(FRAME_QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let pacer = {
        let stop = stop.clone();
        let errs = errors.clone();
        thread::Builder::new()
            .name("clipcheck-pacer".into())
            .spawn(move || run_pacer(cfg, shared, stop, tx, errs, pixels, emit))
            .map_err(|e| CaptureError::Windows {
                context: "spawn pacer thread",
                message: e.to_string(),
                hresult: None,
            })?
    };
    Ok((
        WindowCapturer {
            stop,
            pacer: Some(pacer),
            wgc,
            dda,
            errors,
        },
        rx,
    ))
}

impl Drop for WindowCapturer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(p) = self.pacer.take() {
            let _ = p.join();
        }
        if let Some(w) = self.wgc.take() {
            w.shutdown();
        }
        if let Some(d) = self.dda.take() {
            d.shutdown();
        }
    }
}

/// What the pacer asks the output mode to emit on one tick. `P` is the mode's pixel payload
/// (CPU: shared BGRA bytes; GPU: a texture).
pub(crate) enum Tick<P> {
    /// A new frame: the newest published pixels.
    Live { width: u32, height: u32, pixels: P },
    /// The last live frame again.
    Duplicate { width: u32, height: u32, pixels: P },
    /// A black frame of the canvas size.
    Black { width: u32, height: u32 },
}

/// What the pacer does on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickAction {
    /// Emit the newest WGC frame.
    Live,
    /// Re-emit the last live frame.
    Duplicate,
    /// Emit a black frame of the canvas size.
    Black,
    /// Nothing to emit yet (no frame has ever arrived).
    Idle,
}

/// Pure tick decision. `hidden_for` is zero while the window is showing.
pub(crate) fn decide(hidden_for: Duration, has_new: bool, has_last: bool) -> TickAction {
    if hidden_for >= HIDDEN_GRACE {
        return TickAction::Black;
    }
    if has_new {
        TickAction::Live
    } else if has_last {
        TickAction::Duplicate
    } else {
        TickAction::Idle
    }
}

/// Offset of tick `k` from the start, at `fps` ticks per second (exact, no drift).
pub(crate) fn tick_offset(k: u64, fps: u32) -> Duration {
    let nanos = k as u128 * 1_000_000_000u128 / fps as u128;
    Duration::from_nanos(nanos as u64)
}

/// Index of the last tick at or before `elapsed`.
pub(crate) fn tick_index_at(elapsed: Duration, fps: u32) -> u64 {
    (elapsed.as_nanos() * fps as u128 / 1_000_000_000u128) as u64
}

/// Last window-state poll, reused until [`WINDOW_STATE_INTERVAL`] has passed.
#[derive(Debug, Default)]
struct WindowStateCache {
    polled_at: Option<Instant>,
    showing: bool,
}

impl WindowStateCache {
    /// Returns the cached showing flag. `poll` runs only when nothing has been polled yet or
    /// the last poll is at least [`WINDOW_STATE_INTERVAL`] old.
    fn showing(&mut self, now: Instant, poll: impl FnOnce() -> bool) -> bool {
        let stale = match self.polled_at {
            None => true,
            Some(t) => now.saturating_duration_since(t) >= WINDOW_STATE_INTERVAL,
        };
        if stale {
            self.showing = poll();
            self.polled_at = Some(now);
        }
        self.showing
    }
}

/// The last live frame's pixels, kept so duplicate ticks can repeat it.
struct LastLive<P> {
    width: u32,
    height: u32,
    pixels: P,
}

/// CPU payload: the newest BGRA bytes, if any frame has been published.
fn cpu_pixels(lat: &Latest) -> Option<Arc<Vec<u8>>> {
    if lat.bgra.is_empty() {
        None
    } else {
        Some(lat.bgra.clone())
    }
}

/// GPU payload: the newest texture, if any frame has been published.
fn gpu_pixels(lat: &Latest) -> Option<SharedTexture> {
    lat.texture.clone()
}

/// CPU output: ticks become [`Frame`]s; black frames come from the cached black generator.
fn cpu_emit() -> impl FnMut(Tick<Arc<Vec<u8>>>, i64) -> Frame + Send + 'static {
    let mut black = blackgen::BlackCache::default();
    move |tick, pts_100ns| match tick {
        Tick::Live {
            width,
            height,
            pixels: bgra,
        }
        | Tick::Duplicate {
            width,
            height,
            pixels: bgra,
        } => Frame {
            width,
            height,
            bgra,
            pts_100ns,
            is_black: false,
        },
        Tick::Black { width, height } => Frame {
            width,
            height,
            bgra: black.frame(width, height),
            pts_100ns,
            is_black: true,
        },
    }
}

/// GPU output: ticks become [`GpuFrame`]s carrying the texture; black frames carry none.
fn gpu_emit() -> impl FnMut(Tick<SharedTexture>, i64) -> GpuFrame + Send + 'static {
    move |tick, pts_100ns| match tick {
        Tick::Live {
            width,
            height,
            pixels,
        }
        | Tick::Duplicate {
            width,
            height,
            pixels,
        } => GpuFrame {
            texture: Some(pixels),
            width,
            height,
            pts_100ns,
            is_black: false,
        },
        Tick::Black { width, height } => GpuFrame {
            texture: None,
            width,
            height,
            pts_100ns,
            is_black: true,
        },
    }
}

/// The CFR pacer, shared by both output modes. `pixels` extracts the mode's payload from the
/// published state; `emit` turns each tick into an output item. Output runs at `cfg.fps`.
fn run_pacer<P: Clone, I>(
    cfg: CaptureConfig,
    shared: Shared,
    stop: Arc<AtomicBool>,
    tx: SyncSender<I>,
    errors: Arc<AtomicU64>,
    pixels: fn(&Latest) -> Option<P>,
    mut emit: impl FnMut(Tick<P>, i64) -> I,
) {
    let fps = cfg.fps;
    let period = tick_offset(1, fps);
    let t0 = Instant::now();
    let qpc0 = wgc::qpc_now();
    let qpc_freq = wgc::qpc_frequency().max(1) as i128;

    let mut k: u64 = 0;
    let mut last_seq: u64 = 0;
    let mut last_live: Option<LastLive<P>> = None;
    let mut hidden_since: Option<Instant> = None;
    let mut window_state = WindowStateCache::default();

    while !stop.load(Ordering::Acquire) {
        let due = t0 + tick_offset(k, fps);
        let mut now = Instant::now();
        if due > now {
            thread::sleep(due - now);
            now = Instant::now();
        }
        // Fell more than one frame behind: skip ahead to the current tick.
        if now.duration_since(due) > period {
            k = tick_index_at(now.duration_since(t0), fps);
        }

        let visible = window_state.showing(now, || blackgen::is_window_showing(cfg.hwnd));
        let hidden_for = if visible {
            hidden_since = None;
            Duration::ZERO
        } else {
            now.duration_since(*hidden_since.get_or_insert(now))
        };

        // The lock covers only scalar reads and a reference-count clone. Pixels are copied after
        // it is released, so the WGC callback never waits on a frame-sized copy.
        let (seq, width, height, payload) = {
            let lat = wgc::lock_counted(&shared, &errors);
            (lat.seq, lat.width, lat.height, pixels(&lat))
        };
        let has_new = seq != last_seq && payload.is_some();
        let tick = match decide(hidden_for, has_new, last_live.is_some()) {
            TickAction::Live => match payload {
                Some(p) => {
                    last_seq = seq;
                    last_live = Some(LastLive {
                        width,
                        height,
                        pixels: p.clone(),
                    });
                    Some(Tick::Live {
                        width,
                        height,
                        pixels: p,
                    })
                }
                None => None,
            },
            TickAction::Duplicate => last_live.as_ref().map(|l| Tick::Duplicate {
                width: l.width,
                height: l.height,
                pixels: l.pixels.clone(),
            }),
            TickAction::Black => (width > 0 && height > 0).then_some(Tick::Black { width, height }),
            TickAction::Idle => None,
        };

        if let Some(t) = tick {
            let elapsed = wgc::qpc_now() - qpc0;
            let pts_100ns = (elapsed as i128 * 10_000_000 / qpc_freq) as i64;
            match tx.try_send(emit(t, pts_100ns)) {
                Ok(()) => {}
                // Consumer is behind: drop this tick rather than block the pacer.
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_live_dup_idle() {
        assert_eq!(decide(Duration::ZERO, true, false), TickAction::Live);
        assert_eq!(decide(Duration::ZERO, false, true), TickAction::Duplicate);
        assert_eq!(decide(Duration::ZERO, false, false), TickAction::Idle);
    }

    #[test]
    fn decide_grace_then_black() {
        let just_under = HIDDEN_GRACE - Duration::from_millis(1);
        assert_eq!(decide(just_under, false, true), TickAction::Duplicate);
        assert_eq!(decide(HIDDEN_GRACE, false, true), TickAction::Black);
        assert_eq!(decide(HIDDEN_GRACE, true, true), TickAction::Black);
    }

    #[test]
    fn tick_offsets_are_exact() {
        assert_eq!(tick_offset(30, 30), Duration::from_secs(1));
        assert_eq!(tick_offset(1, 3), Duration::from_nanos(333_333_333));
        assert_eq!(tick_index_at(Duration::from_secs(1), 30), 30);
        assert_eq!(tick_index_at(Duration::from_millis(999), 30), 29);
    }

    #[test]
    fn only_missing_capture_support_is_an_environment_error() {
        let win = |context, hresult| CaptureError::Windows {
            context,
            message: String::new(),
            hresult,
        };
        assert!(CaptureError::Unsupported.is_environment());
        assert!(win("D3D11CreateDevice", Some(DXGI_ERROR_UNSUPPORTED)).is_environment());
        // Same HRESULT from a different step is a real failure, not an environment skip.
        assert!(!win("CreateForWindow", Some(DXGI_ERROR_UNSUPPORTED)).is_environment());
        assert!(!win("D3D11CreateDevice", Some(0x8007_0005_u32 as i32)).is_environment());
        assert!(!win("D3D11CreateDevice", None).is_environment());
        assert!(!CaptureError::InvalidWindow(1).is_environment());
        assert!(!CaptureError::InvalidFps(0).is_environment());
    }

    #[test]
    fn rejects_bad_fps() {
        let cfg = CaptureConfig { hwnd: 1, fps: 0 };
        assert!(matches!(
            WindowCapturer::start(cfg),
            Err(CaptureError::InvalidFps(0))
        ));
        let cfg = CaptureConfig { hwnd: 1, fps: 241 };
        assert!(matches!(
            WindowCapturer::start(cfg),
            Err(CaptureError::InvalidFps(241))
        ));
    }
}
