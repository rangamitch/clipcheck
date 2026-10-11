//! Desktop Duplication source (D64): records a window without the WGC capture border by
//! duplicating the window's monitor and cropping to the window's client area. The duplication
//! thread owns the DXGI objects; each accepted frame is copied into a texture from a
//! [`TextureRing`] on the caller's device and published into [`Shared`] like the WGC callback
//! does. Contract: docs/impl/no-border-capture.md.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::core::Interface;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTPUT_DESC,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetDesktopWindow, GetShellWindow, IsWindow,
};

use crate::dda_geom::{covers_monitor, crop_box, rotation_supported, CropBox, Rect};
use crate::gpu::{SharedTexture, TextureRing};
use crate::{wgc, CaptureError, Shared};

/// Times [`spawn`] has been entered in this process.
static NO_BORDER_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// Pause between attempts to rebuild a duplication that failed.
const RETRY_MS: u64 = 250;

/// Longest single sleep, so a stop request is seen promptly.
const SLICE_MS: u64 = 50;

/// See [`crate::no_border_attempts`].
pub(crate) fn attempts() -> u64 {
    NO_BORDER_ATTEMPTS.load(Ordering::Relaxed)
}

/// Owns the duplication thread. Dropping the sender ends its loop.
pub(crate) struct DdaThread {
    stop_tx: Sender<()>,
    join: JoinHandle<()>,
}

impl DdaThread {
    /// Signals the thread to stop and waits for it.
    pub(crate) fn shutdown(self) {
        let DdaThread { stop_tx, join } = self;
        drop(stop_tx);
        let _ = join.join();
    }
}

/// Inputs for the duplication thread, moved into it by [`spawn`].
struct Setup {
    hwnd: isize,
    fps: u32,
    device: ID3D11Device,
    require_cover: bool,
}

/// Starts the duplication thread for `hwnd`, delivering at most `fps` frames per second into
/// `shared`. Returns once the duplication is running, or the reason it cannot run.
pub(crate) fn spawn(
    hwnd: isize,
    fps: u32,
    shared: Shared,
    errors: Arc<AtomicU64>,
    device: ID3D11Device,
    require_cover: bool,
) -> Result<DdaThread, CaptureError> {
    NO_BORDER_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), CaptureError>>(1);
    let setup = Setup {
        hwnd,
        fps,
        device,
        require_cover,
    };
    let join = thread::Builder::new()
        .name("clipcheck-dda".into())
        .spawn(move || thread_main(setup, shared, errors, init_tx, stop_rx))
        .map_err(|e| CaptureError::Windows {
            context: "spawn DDA thread",
            message: e.to_string(),
            hresult: None,
        })?;
    match init_rx.recv() {
        Ok(Ok(())) => Ok(DdaThread { stop_tx, join }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err(CaptureError::ThreadStart)
        }
    }
}

/// One live duplication of an output, with the output's desktop rectangle.
struct Duplication {
    dup: IDXGIOutputDuplication,
    /// The output's DesktopCoordinates (virtual-screen pixels).
    output: Rect,
    /// The monitor this duplication was built for.
    hmon: HMONITOR,
}

/// Everything the duplication thread owns. Fields drop in declaration order, so the duplication
/// is released before the ring and the device.
struct Capture {
    dup: Option<Duplication>,
    ring: TextureRing,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    hwnd: HWND,
}

/// Thread body: sets the DPI mode, opens the capture, reports the result, then runs the loop.
fn thread_main(
    setup: Setup,
    shared: Shared,
    errors: Arc<AtomicU64>,
    init_tx: SyncSender<Result<(), CaptureError>>,
    stop_rx: Receiver<()>,
) {
    // SAFETY: this thread is private to the capture and the setting is thread-local, so no
    // other code observes it. The previous context is not needed.
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    match open(&setup) {
        Ok(mut cap) => {
            if let Some((w, h)) = initial_size(&cap) {
                let mut lat = wgc::lock_counted(&shared, &errors);
                lat.width = w;
                lat.height = h;
            }
            let _ = init_tx.send(Ok(()));
            frame_loop(&mut cap, &shared, &errors, setup.fps, &stop_rx);
        }
        Err(e) => {
            let _ = init_tx.send(Err(e));
        }
    }
}

/// Validates the window, checks coverage when required, and opens the duplication.
fn open(setup: &Setup) -> Result<Capture, CaptureError> {
    let h = HWND(setup.hwnd as *mut c_void);
    // SAFETY: IsWindow, GetDesktopWindow and GetShellWindow take no pointers that we
    // dereference; they only compare handle values.
    let (valid, desktop, shell) =
        unsafe { (IsWindow(h).as_bool(), GetDesktopWindow(), GetShellWindow()) };
    if !valid || h == desktop || h == shell {
        return Err(CaptureError::InvalidWindow(setup.hwnd));
    }
    let hmon = monitor_of(h);
    if setup.require_cover {
        let client = client_rect_screen(h)?;
        let monitor = monitor_rect(hmon)?;
        if !covers_monitor(client, monitor) {
            return Err(unavailable(
                "the game window does not cover its monitor (use borderless fullscreen)",
            ));
        }
    }
    let dup = open_duplication(&setup.device, hmon)?;
    // SAFETY: the device is live and the immediate context is owned by the device; this
    // thread is its only user of the context here.
    let context =
        unsafe { setup.device.GetImmediateContext() }.map_err(derr("GetImmediateContext"))?;
    Ok(Capture {
        dup: Some(dup),
        ring: TextureRing::new(setup.device.clone()),
        context,
        device: setup.device.clone(),
        hwnd: h,
    })
}

/// Size of the crop the window currently needs, or None when it has no visible area.
fn initial_size(cap: &Capture) -> Option<(u32, u32)> {
    let client = client_rect_screen(cap.hwnd).ok()?;
    let output = cap.dup.as_ref()?.output;
    let crop = crop_box(client, output)?;
    Some((crop.width, crop.height))
}

/// Runs the acquire/copy/publish loop until the stop sender is dropped.
fn frame_loop(
    cap: &mut Capture,
    shared: &Shared,
    errors: &AtomicU64,
    fps: u32,
    stop_rx: &Receiver<()>,
) {
    let timeout = timeout_ms(fps);
    let period = period_ticks(wgc::qpc_frequency(), fps);
    let mut last: Option<i64> = None;
    loop {
        if stop_requested(stop_rx) || !ensure_duplication(cap, errors, stop_rx) {
            return;
        }
        let acquired = match cap.dup.as_ref() {
            Some(d) => acquire_frame(&d.dup, timeout),
            None => continue,
        };
        let (info, resource) = match acquired {
            Ok(v) => v,
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                // Mode change, secure desktop or fullscreen switch: rebuild on the next pass.
                wgc::note_error(errors, derr("AcquireNextFrame")(e));
                cap.dup = None;
                continue;
            }
            Err(e) => {
                wgc::note_error(errors, derr("AcquireNextFrame")(e));
                if !wait_or_stop(stop_rx, SLICE_MS) {
                    return;
                }
                continue;
            }
        };
        // A frame is held from here on; it is released on every path below.
        if let Some(resource) = resource.as_ref() {
            // LastPresentTime == 0 means a cursor-only update, which carries no new pixels.
            if info.LastPresentTime != 0 {
                publish_frame(cap, shared, errors, resource, &mut last, period);
            }
        }
        release_frame(cap, errors);
    }
}

/// Copies the window's crop from an acquired desktop image into a ring texture and publishes it.
/// Returns without publishing when the frame is rate-gated or the window has no visible area.
fn publish_frame(
    cap: &mut Capture,
    shared: &Shared,
    errors: &AtomicU64,
    resource: &IDXGIResource,
    last: &mut Option<i64>,
    period: i64,
) {
    let now = wgc::qpc_now();
    if !wgc::accept_frame(*last, now, period) {
        return;
    }
    let client = match client_rect_screen(cap.hwnd) {
        Ok(r) => r,
        Err(e) => {
            wgc::note_error(errors, e);
            return;
        }
    };
    let Some(output) = cap.dup.as_ref().map(|d| d.output) else {
        return;
    };
    let Some(crop) = crop_box(client, output) else {
        return;
    };
    let src = match resource.cast::<ID3D11Texture2D>() {
        Ok(t) => t,
        Err(e) => {
            wgc::note_error(errors, derr("cast to ID3D11Texture2D")(e));
            return;
        }
    };
    let dst = match cap.ring.acquire(crop.width, crop.height) {
        Ok(t) => t,
        Err(e) => {
            wgc::note_error(errors, derr("TextureRing::acquire")(e));
            return;
        }
    };
    copy_crop(&cap.context, &dst, &src, crop);
    *last = Some(now);
    let mut lat = wgc::lock_counted(shared, errors);
    lat.seq = lat.seq.wrapping_add(1);
    lat.width = crop.width;
    lat.height = crop.height;
    lat.texture = Some(SharedTexture(dst));
}

/// Copies the `crop` box of `src` (output-local pixels) to the top-left of `dst`.
fn copy_crop(
    context: &ID3D11DeviceContext,
    dst: &ID3D11Texture2D,
    src: &ID3D11Texture2D,
    crop: CropBox,
) {
    let region = D3D11_BOX {
        left: crop.left,
        top: crop.top,
        front: 0,
        right: crop.left.saturating_add(crop.width),
        bottom: crop.top.saturating_add(crop.height),
        back: 1,
    };
    // SAFETY: dst and src are textures on the same device with the same format. crop_box clamps
    // the region to the output, which is the size of src, and dst is crop.width x crop.height.
    unsafe {
        context.CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, Some(&region as *const D3D11_BOX))
    };
}

/// Calls `AcquireNextFrame` and returns the frame info and the desktop image.
fn acquire_frame(
    dup: &IDXGIOutputDuplication,
    timeout: u32,
) -> windows::core::Result<(DXGI_OUTDUPL_FRAME_INFO, Option<IDXGIResource>)> {
    // SAFETY: every field of DXGI_OUTDUPL_FRAME_INFO is an integer, a BOOL or a POINT, so the
    // all-zero bit pattern is a valid value. It is overwritten by AcquireNextFrame on success.
    let mut info: DXGI_OUTDUPL_FRAME_INFO = unsafe { std::mem::zeroed() };
    let mut resource: Option<IDXGIResource> = None;
    // SAFETY: dup is a live duplication; info and resource are valid out-pointers.
    unsafe { dup.AcquireNextFrame(timeout, &mut info, &mut resource) }?;
    Ok((info, resource))
}

/// Releases the frame acquired on the current duplication, if any.
fn release_frame(cap: &Capture, errors: &AtomicU64) {
    if let Some(d) = cap.dup.as_ref() {
        // SAFETY: called exactly once after each successful AcquireNextFrame on this duplication.
        if let Err(e) = unsafe { d.dup.ReleaseFrame() } {
            wgc::note_error(errors, derr("ReleaseFrame")(e));
        }
    }
}

/// Makes sure a duplication exists for the window's current monitor. Failed attempts are counted
/// and retried every [`RETRY_MS`]. Returns false when a stop was requested first.
fn ensure_duplication(cap: &mut Capture, errors: &AtomicU64, stop_rx: &Receiver<()>) -> bool {
    loop {
        if stop_requested(stop_rx) {
            return false;
        }
        let hmon = monitor_of(cap.hwnd);
        if matches!(cap.dup.as_ref(), Some(d) if d.hmon == hmon) {
            return true;
        }
        // Drop the old duplication before creating the new one.
        cap.dup = None;
        match open_duplication(&cap.device, hmon) {
            Ok(dup) => {
                cap.dup = Some(dup);
                return true;
            }
            Err(e) => {
                wgc::note_error(errors, &e);
                if !wait_or_stop(stop_rx, RETRY_MS) {
                    return false;
                }
            }
        }
    }
}

/// Builds a duplication of the output for `hmon` on `device`, checking rotation and format.
fn open_duplication(device: &ID3D11Device, hmon: HMONITOR) -> Result<Duplication, CaptureError> {
    let (output, desc) = find_output(device, hmon)?;
    if !rotation_supported(desc.Rotation.0) {
        return Err(unavailable("rotated displays are not supported"));
    }
    let output1: IDXGIOutput1 = output.cast().map_err(derr("cast to IDXGIOutput1"))?;
    // SAFETY: output1 and device are live. DuplicateOutput returns a new duplication object.
    let dup = unsafe { output1.DuplicateOutput(device) }
        .map_err(|e| unavailable(format!("Desktop Duplication refused: {e}")))?;
    // SAFETY: dup is a live duplication; GetDesc returns a plain struct by value.
    let mode = unsafe { dup.GetDesc() };
    if mode.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(unavailable("unsupported display format (HDR?)"));
    }
    Ok(Duplication {
        dup,
        output: to_geom(desc.DesktopCoordinates),
        hmon,
    })
}

/// Finds the output of `device`'s adapter that is attached to `hmon`.
fn find_output(
    device: &ID3D11Device,
    hmon: HMONITOR,
) -> Result<(IDXGIOutput, DXGI_OUTPUT_DESC), CaptureError> {
    let dxgi: IDXGIDevice = device.cast().map_err(derr("cast to IDXGIDevice"))?;
    // SAFETY: dxgi is a live interface; GetAdapter returns a new reference to the adapter.
    let adapter = unsafe { dxgi.GetAdapter() }.map_err(derr("GetAdapter"))?;
    for index in 0u32.. {
        // SAFETY: adapter is live. EnumOutputs fails with DXGI_ERROR_NOT_FOUND past the last output.
        let output = match unsafe { adapter.EnumOutputs(index) } {
            Ok(o) => o,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(derr("EnumOutputs")(e)),
        };
        // SAFETY: output is live; GetDesc fills a DXGI_OUTPUT_DESC by value.
        let desc = unsafe { output.GetDesc() }.map_err(derr("GetDesc"))?;
        if desc.Monitor == hmon {
            return Ok((output, desc));
        }
    }
    Err(unavailable(
        "the monitor is on a different graphics adapter than the encoder",
    ))
}

/// The monitor the window is on (nearest monitor when it is off all monitors).
fn monitor_of(h: HWND) -> HMONITOR {
    // SAFETY: MonitorFromWindow only reads the handle value. DEFAULTTONEAREST returns a monitor
    // whenever one exists.
    unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) }
}

/// Virtual-screen rectangle of a monitor, in physical pixels.
fn monitor_rect(hmon: HMONITOR) -> Result<Rect, CaptureError> {
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        rcMonitor: ZERO_RECT,
        rcWork: ZERO_RECT,
        dwFlags: 0,
    };
    // SAFETY: mi is a valid MONITORINFO with cbSize set, as GetMonitorInfoW requires.
    let ok = unsafe { GetMonitorInfoW(hmon, &mut mi) };
    if !ok.as_bool() {
        return Err(win_fail("GetMonitorInfoW", "the monitor is not available"));
    }
    Ok(to_geom(mi.rcMonitor))
}

/// Client area of the window in virtual-screen pixels.
fn client_rect_screen(h: HWND) -> Result<Rect, CaptureError> {
    let mut rc = ZERO_RECT;
    // SAFETY: rc is a valid out-pointer for GetClientRect.
    unsafe { GetClientRect(h, &mut rc) }.map_err(derr("GetClientRect"))?;
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: origin is a valid in/out pointer for ClientToScreen.
    let ok = unsafe { ClientToScreen(h, &mut origin) };
    if !ok.as_bool() {
        return Err(win_fail("ClientToScreen", "the window is not valid"));
    }
    Ok(Rect::new(
        origin.x,
        origin.y,
        origin.x.saturating_add(rc.right),
        origin.y.saturating_add(rc.bottom),
    ))
}

/// Converts a Win32 RECT to the pure geometry type.
fn to_geom(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// True once the owner has dropped the stop sender.
fn stop_requested(stop_rx: &Receiver<()>) -> bool {
    matches!(stop_rx.try_recv(), Err(TryRecvError::Disconnected))
}

/// Sleeps `ms` in slices of at most [`SLICE_MS`]. Returns false when a stop was requested.
fn wait_or_stop(stop_rx: &Receiver<()>, ms: u64) -> bool {
    let mut left = ms;
    while left > 0 {
        if stop_requested(stop_rx) {
            return false;
        }
        let step = left.min(SLICE_MS);
        thread::sleep(Duration::from_millis(step));
        left -= step;
    }
    !stop_requested(stop_rx)
}

/// AcquireNextFrame wait for one frame period at `fps`, clamped to 4..=100 ms.
fn timeout_ms(fps: u32) -> u32 {
    (1000 / fps.max(1)).clamp(4, 100)
}

/// One output period at `fps`, in QPC ticks of `freq` per second.
fn period_ticks(freq: i64, fps: u32) -> i64 {
    freq / i64::from(fps.max(1))
}

/// Maps a windows-crate error to a [`CaptureError::Windows`] for `context`.
fn derr(context: &'static str) -> impl Fn(windows::core::Error) -> CaptureError {
    move |e| CaptureError::Windows {
        context,
        message: e.to_string(),
        hresult: Some(e.code().0),
    }
}

/// A [`CaptureError::Windows`] for a call that reports failure without an error code.
fn win_fail(context: &'static str, message: &str) -> CaptureError {
    CaptureError::Windows {
        context,
        message: message.to_string(),
        hresult: None,
    }
}

/// A [`CaptureError::NoBorderUnavailable`] with a user-readable reason.
fn unavailable(reason: impl Into<String>) -> CaptureError {
    CaptureError::NoBorderUnavailable {
        reason: reason.into(),
    }
}

/// An all-zero RECT, used as the starting value for Win32 out-parameters.
const ZERO_RECT: RECT = RECT {
    left: 0,
    top: 0,
    right: 0,
    bottom: 0,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_follows_fps_and_is_clamped() {
        assert_eq!(timeout_ms(60), 16);
        assert_eq!(timeout_ms(30), 33);
        assert_eq!(timeout_ms(240), 4);
        assert_eq!(timeout_ms(1), 100);
        assert_eq!(timeout_ms(0), 100);
    }

    #[test]
    fn period_is_one_frame_in_ticks() {
        assert_eq!(period_ticks(10_000_000, 60), 166_666);
        assert_eq!(period_ticks(10_000_000, 0), 10_000_000);
    }

    #[test]
    fn rect_conversion_keeps_edges() {
        let r = to_geom(RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        });
        assert_eq!(r, Rect::new(-1920, 0, 0, 1080));
    }

    #[test]
    fn attempts_counter_starts_readable() {
        let _ = attempts();
    }
}
