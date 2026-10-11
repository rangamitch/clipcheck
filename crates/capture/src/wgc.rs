//! Windows Graphics Capture source for a single window.
//!
//! A dedicated thread owns the WinRT/D3D objects. The FrameArrived callback either reads each
//! frame back to BGRA on the CPU and publishes it into [`Shared`] (CPU mode), or copies it into
//! a texture ring on the caller's device and publishes the texture (GPU mode).
//! Frames arriving sooner than half an output period after the last accepted one are dropped
//! before any copy (see [`accept_frame`]).

use std::ffi::c_void;
use std::fmt::Display;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use windows::core::{factory, Interface, HSTRING};
use windows::Foundation::Metadata::ApiInformation;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetShellWindow, IsWindow};

use crate::buffers::BufferRing;
use crate::gpu::{copy_top_left, SharedTexture, TextureRing};
use crate::{lock, CaptureError, Shared};

/// Pixel buffers in rotation on the readback side. Three covers the newest published frame,
/// the pacer's last live frame, and one spare, so a free slot is usually available.
const READBACK_BUFFERS: usize = 3;

/// Owns the WGC thread. Dropping the sender ends the thread's wait loop.
pub(crate) struct WgcThread {
    stop_tx: Sender<()>,
    join: JoinHandle<()>,
}

impl WgcThread {
    /// Signals the WGC thread to tear down its session and waits for it to exit.
    pub(crate) fn shutdown(self) {
        let WgcThread { stop_tx, join } = self;
        drop(stop_tx);
        let _ = join.join();
    }
}

/// Where captured frames go.
pub(crate) enum Source {
    /// CPU readback into BGRA buffers (`Latest::bgra`).
    Cpu,
    /// GPU texture copies on this device (`Latest::texture`). WGC runs on the same device.
    Gpu(ID3D11Device),
}

/// Starts the WGC thread for `hwnd`, delivering at most `fps` frames per second. Returns once the
/// session is running. `errors` counts failures on the frame callback path (see [`note_error`]).
pub(crate) fn spawn(
    hwnd: isize,
    fps: u32,
    shared: Shared,
    errors: Arc<AtomicU64>,
    source: Source,
) -> Result<WgcThread, CaptureError> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), CaptureError>>(1);
    let join = thread::Builder::new()
        .name("clipcheck-wgc".into())
        .spawn(move || thread_main(hwnd, fps, shared, errors, source, init_tx, stop_rx))
        .map_err(|e| CaptureError::Windows {
            context: "spawn WGC thread",
            message: e.to_string(),
            hresult: None,
        })?;
    match init_rx.recv() {
        Ok(Ok(())) => Ok(WgcThread { stop_tx, join }),
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

/// Keeps a multithreaded apartment alive for the whole process.
///
/// The `windows` crate caches WinRT activation factories process-wide after first use. If the
/// only thread holding the MTA exits, the apartment is torn down and those cached factories
/// dangle, so the *next* capture crashes inside `GraphicsCaptureSession::IsSupported()`.
fn ensure_process_mta() {
    static MTA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    MTA.get_or_init(|| {
        // SAFETY: no preconditions; the returned cookie is intentionally never released so the
        // MTA stays alive until process exit.
        let _ = unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() };
    });
}

fn thread_main(
    hwnd: isize,
    fps: u32,
    shared: Shared,
    errors: Arc<AtomicU64>,
    source: Source,
    init_tx: SyncSender<Result<(), CaptureError>>,
    stop_rx: mpsc::Receiver<()>,
) {
    // The process-wide MTA is taken first, so this thread's own apartment can end
    // without tearing down the MTA that cached WinRT factories depend on.
    ensure_process_mta();
    // SAFETY: RoInitialize is balanced by the ComApartment guard below, which is
    // created only when this call succeeds (S_OK or S_FALSE).
    let ro = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let _apartment = match ro {
        Ok(()) => ComApartment,
        Err(e) => {
            let _ = init_tx.send(Err(werr("RoInitialize")(e)));
            return;
        }
    };

    match setup(hwnd, fps, &shared, &errors, source) {
        Ok(running) => {
            let _ = init_tx.send(Ok(()));
            // Blocks until stop() drops the sender.
            let _ = stop_rx.recv();
            teardown(running, &errors);
        }
        Err(e) => {
            let _ = init_tx.send(Err(e));
        }
    }
    // `_apartment` drops here, after teardown has released every WinRT and D3D object.
}

/// Balances a successful `RoInitialize` on this thread with `RoUninitialize` on drop.
///
/// Only the thread-local reference is released. The process-wide MTA taken by
/// [`ensure_process_mta`] stays alive, which is what the cached factories need.
struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: this thread's RoInitialize succeeded, so this call balances it.
        // The guard is dropped only after teardown, so no capture objects remain.
        unsafe { RoUninitialize() };
    }
}

/// Removes the yellow capture border where Windows allows it (Windows 11). The property is
/// absent on Windows 10 and the OS may refuse the call, in which case the border stays.
/// Both outcomes are logged: the old code discarded the result, so the border showed
/// with no trace in the logs.
fn disable_border(session: &GraphicsCaptureSession) {
    let present = ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from("IsBorderRequired"),
    )
    .unwrap_or(false);
    if !present {
        eprintln!("clipcheck capture: IsBorderRequired not available on this Windows build; capture border stays");
        return;
    }
    if let Err(e) = session.SetIsBorderRequired(false) {
        eprintln!(
            "clipcheck capture: SetIsBorderRequired(false) refused ({e}); capture border stays"
        );
    }
}

/// Counts one failure on the WGC callback or teardown path. The first failure is logged
/// to stderr; later ones are counted silently.
pub(crate) fn note_error(errors: &AtomicU64, what: impl Display) {
    if errors.fetch_add(1, Ordering::Relaxed) == 0 {
        eprintln!(
            "clipcheck capture: capture callback error ({what}); further errors are counted silently"
        );
    }
}

/// Locks `m`, recovering a poisoned mutex and counting the poisoning as an error.
pub(crate) fn lock_counted<'a, T>(m: &'a Mutex<T>, errors: &AtomicU64) -> MutexGuard<'a, T> {
    m.lock().unwrap_or_else(|e| {
        note_error(errors, "poisoned mutex");
        e.into_inner()
    })
}

/// Live WGC objects that must be closed on the WGC thread.
struct Running {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: windows::Foundation::EventRegistrationToken,
    /// Callback state; taken (and dropped) in teardown after the handler is removed.
    readback: std::sync::Arc<std::sync::Mutex<Option<Readback>>>,
}

fn teardown(r: Running, errors: &AtomicU64) {
    let _ = r.pool.RemoveFrameArrived(r.token);
    // Taking the mutex waits for any callback still running on a thread-pool
    // thread; dropping the state releases the D3D objects before the pool closes.
    // A poisoned mutex is recovered so the D3D state is released in every case.
    lock_counted(&*r.readback, errors).take();
    let _ = r.session.Close();
    let _ = r.pool.Close();
}

fn werr(context: &'static str) -> impl Fn(windows::core::Error) -> CaptureError {
    move |e| CaptureError::Windows {
        context,
        message: e.to_string(),
        hresult: Some(e.code().0),
    }
}

fn setup(
    hwnd: isize,
    fps: u32,
    shared: &Shared,
    errors: &Arc<AtomicU64>,
    source: Source,
) -> Result<Running, CaptureError> {
    let h = HWND(hwnd as *mut c_void);

    // SAFETY: IsWindow, GetDesktopWindow and GetShellWindow take no pointers
    // that we dereference; they only compare handle values.
    let (valid, desktop, shell) =
        unsafe { (IsWindow(h).as_bool(), GetDesktopWindow(), GetShellWindow()) };
    if !valid || h == desktop || h == shell {
        return Err(CaptureError::InvalidWindow(hwnd));
    }

    let supported = GraphicsCaptureSession::IsSupported().map_err(werr("IsSupported"))?;
    if !supported {
        return Err(CaptureError::Unsupported);
    }

    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(werr("GraphicsCaptureItem interop factory"))?;
    // SAFETY: h is a valid, non-desktop, non-shell top-level window handle (checked above).
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForWindow(h) }.map_err(werr("CreateForWindow"))?;
    let size: SizeInt32 = item.Size().map_err(werr("GraphicsCaptureItem::Size"))?;

    let (device, context, gpu) = match source {
        Source::Cpu => {
            let (device, context) = create_device()?;
            (device, context, None)
        }
        Source::Gpu(device) => {
            // SAFETY: device is a live D3D11 device owned by this call; GetImmediateContext only
            // returns a new reference to its immediate context.
            let context =
                unsafe { device.GetImmediateContext() }.map_err(werr("GetImmediateContext"))?;
            let ring = TextureRing::new(device.clone());
            (device, context, Some(ring))
        }
    };
    let dxgi: IDXGIDevice = device.cast().map_err(werr("cast to IDXGIDevice"))?;
    // SAFETY: dxgi is a valid IDXGIDevice owned for the duration of the call.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(werr("CreateDirect3D11DeviceFromDXGIDevice"))?;
    let d3d: IDirect3DDevice = inspectable
        .cast()
        .map_err(werr("cast to IDirect3DDevice"))?;

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &d3d,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )
    .map_err(werr("CreateFreeThreaded"))?;
    let session = pool
        .CreateCaptureSession(&item)
        .map_err(werr("CreateCaptureSession"))?;
    // Cursor is optional polish; older Windows builds reject it.
    let _ = session.SetIsCursorCaptureEnabled(false);
    disable_border(&session);
    // Caps WGC delivery at the output fps, so a game rendering at 240 fps is not
    // copied or read back 240 times a second. Older Windows builds reject it; that is fine.
    let _ = session.SetMinUpdateInterval(TimeSpan {
        Duration: 10_000_000 / fps.max(1) as i64,
    });

    {
        let mut lat = lock(shared);
        lat.width = size.Width.max(0) as u32;
        lat.height = size.Height.max(0) as u32;
    }

    let readback = Readback {
        device,
        context,
        d3d: AgileDevice(d3d),
        shared: shared.clone(),
        pool_size: size,
        staging: None,
        ring: BufferRing::new(READBACK_BUFFERS),
        gpu,
        period_ticks: qpc_frequency() / fps.max(1) as i64,
        last_accepted: None,
        errors: errors.clone(),
    };
    let readback = Arc::new(Mutex::new(Some(readback)));
    let cb_readback = readback.clone();
    let cb_errors = errors.clone();
    let token = pool
        .FrameArrived(&TypedEventHandler::new(
            move |sender: &Option<Direct3D11CaptureFramePool>,
                  _args: &Option<windows::core::IInspectable>| {
                if let Some(pool) = sender {
                    let mut slot = lock_counted(&*cb_readback, &cb_errors);
                    if let Some(rb) = slot.as_mut() {
                        rb.on_frame(pool);
                    }
                }
                Ok(())
            },
        ))
        .map_err(werr("FrameArrived"))?;

    session.StartCapture().map_err(werr("StartCapture"))?;
    Ok(Running {
        pool,
        session,
        token,
        readback,
    })
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: out-pointers reference live Option slots; the API writes valid
    // COM references into them on success.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device as *mut _),
            None,
            Some(&mut context as *mut _),
        )
    }
    .map_err(werr("D3D11CreateDevice"))?;
    match (device, context) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(CaptureError::Windows {
            context: "D3D11CreateDevice",
            message: "no device returned".into(),
            hresult: None,
        }),
    }
}

/// WinRT wrapper for the capture device, so the FrameArrived closure is `Send`.
struct AgileDevice(IDirect3DDevice);

// SAFETY: IDirect3DDevice is a WinRT agile object. It is created for a
// free-threaded frame pool and is only used to Recreate that pool, which
// the callback does from the threadpool thread that delivers frames.
unsafe impl Send for AgileDevice {}

/// Per-callback state. Lives inside the FrameArrived handler on a threadpool thread.
struct Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    d3d: AgileDevice,
    shared: Shared,
    pool_size: SizeInt32,
    /// Staging texture and its (width, height), reused while the size is unchanged.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// Reusable CPU pixel buffers; a frame is written into a free one instead of a new allocation.
    ring: BufferRing,
    /// GPU texture ring. `Some` in GPU mode, where frames are copied to these textures instead of
    /// being read back to the CPU.
    gpu: Option<TextureRing>,
    /// QPC ticks in one output period; the rate gate uses half of it (see [`accept_frame`]).
    period_ticks: i64,
    /// QPC time of the last frame accepted for processing.
    last_accepted: Option<i64>,
    /// Shared with `WindowCapturer`; counts every failure on this callback path.
    errors: Arc<AtomicU64>,
}

/// Rate gate for WGC frames. A frame arriving less than half an output period after the last
/// accepted one is dropped before any copy or readback. `period_ticks` is one output period in
/// QPC ticks. The first frame is always accepted.
pub(crate) fn accept_frame(last: Option<i64>, now: i64, period_ticks: i64) -> bool {
    match last {
        None => true,
        Some(t) => now.saturating_sub(t) >= period_ticks / 2,
    }
}

impl Readback {
    fn on_frame(&mut self, pool: &Direct3D11CaptureFramePool) {
        let frame = match pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(e) => {
                note_error(&self.errors, e);
                return;
            }
        };
        let now = qpc_now();
        if !accept_frame(self.last_accepted, now, self.period_ticks) {
            // Too soon after the last accepted frame: drop it without any copy.
            let _ = frame.Close();
            return;
        }
        self.last_accepted = Some(now);
        // A failed frame is not published; the pacer repeats the last live frame.
        if let Err(e) = self.process(pool, &frame) {
            note_error(&self.errors, e);
        }
        let _ = frame.Close();
    }

    fn process(
        &mut self,
        pool: &Direct3D11CaptureFramePool,
        frame: &windows::Graphics::Capture::Direct3D11CaptureFrame,
    ) -> Result<(), windows::core::Error> {
        let content = frame.ContentSize()?;
        if content.Width <= 0 || content.Height <= 0 {
            return Ok(());
        }
        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            // Content size changed (window resized): rebuild the pool at the new size.
            pool.Recreate(
                &self.d3d.0,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                content,
            )?;
            self.pool_size = content;
            return Ok(());
        }

        let surface = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        // SAFETY: the surface came from this capture pool and is a D3D11 texture.
        let tex: ID3D11Texture2D = unsafe { access.GetInterface()? };

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc is a valid out-struct for GetDesc.
        unsafe { tex.GetDesc(&mut desc) };
        let w = (content.Width as u32).min(desc.Width);
        let h = (content.Height as u32).min(desc.Height);
        if w == 0 || h == 0 {
            return Ok(());
        }

        if let Some(ring) = self.gpu.as_mut() {
            // GPU mode: copy the content region into the next ring texture; no CPU access.
            let dst = ring.acquire(w, h)?;
            copy_top_left(&self.context, &dst, &tex, w, h);
            let mut lat = lock_counted(self.shared.as_ref(), &self.errors);
            lat.seq = lat.seq.wrapping_add(1);
            lat.width = w;
            lat.height = h;
            lat.texture = Some(SharedTexture(dst));
            return Ok(());
        }

        let staging = self.ensure_staging(&desc)?;
        let buf = self.ring.lease();
        read_rows(&self.context, &tex, &staging, w, h, buf)?;
        let bgra = self.ring.published();

        let mut lat = lock_counted(self.shared.as_ref(), &self.errors);
        lat.seq = lat.seq.wrapping_add(1);
        lat.width = w;
        lat.height = h;
        lat.bgra = bgra;
        Ok(())
    }

    fn ensure_staging(
        &mut self,
        desc: &D3D11_TEXTURE2D_DESC,
    ) -> Result<ID3D11Texture2D, windows::core::Error> {
        if let Some((s, sw, sh)) = &self.staging {
            if *sw == desc.Width && *sh == desc.Height {
                return Ok(s.clone());
            }
        }
        let mut sd = *desc;
        sd.Usage = D3D11_USAGE_STAGING;
        sd.BindFlags = 0;
        sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        sd.MiscFlags = 0;
        let mut out: Option<ID3D11Texture2D> = None;
        // SAFETY: sd is a valid descriptor, out receives a new texture on success.
        unsafe {
            self.device
                .CreateTexture2D(&sd, None, Some(&mut out as *mut _))?
        };
        let s =
            out.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
        self.staging = Some((s.clone(), desc.Width, desc.Height));
        Ok(s)
    }
}

/// Copies `tex` to `staging` and reads the top-left `w` x `h` region as tightly packed BGRA
/// into `out`, which is resized to fit and fully overwritten on success.
fn read_rows(
    context: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
    staging: &ID3D11Texture2D,
    w: u32,
    h: u32,
    out: &mut Vec<u8>,
) -> Result<(), windows::core::Error> {
    let mut sd = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: sd is a valid out-struct for GetDesc.
    unsafe { staging.GetDesc(&mut sd) };
    // SAFETY: staging and tex are same-size, same-format textures on this device.
    // Map is paired with Unmap on every path after a successful Map; the mapped
    // rows are read only within the RowPitch bounds of a texture at least h rows tall.
    unsafe {
        context.CopyResource(staging, tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        let result = match mapped_row_bytes(base.is_null(), pitch, w, h, sd.Height) {
            Some(row_bytes) => {
                let total = row_bytes * h as usize;
                out.resize(total, 0);
                if pitch == row_bytes {
                    // Rows are already packed: one contiguous copy. mapped_row_bytes has
                    // checked that the mapping covers h rows of this length.
                    out.copy_from_slice(std::slice::from_raw_parts(base, total));
                } else {
                    for y in 0..h as usize {
                        let src = std::slice::from_raw_parts(base.add(y * pitch), row_bytes);
                        out[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src);
                    }
                }
                Ok(())
            }
            // Invalid mapping: report a failure so the caller drops the frame
            // instead of publishing zeroed pixels as a live frame.
            None => Err(windows::core::Error::from(
                windows::Win32::Foundation::E_UNEXPECTED,
            )),
        };
        context.Unmap(staging, 0);
        result
    }
}

/// Validates a mapped staging surface before its rows are read and returns the
/// tightly packed row length in bytes. Returns `None` (the frame must be dropped)
/// when the base pointer is null, the pitch is shorter than one row, the surface
/// has fewer than `h` rows, a size is zero, or the arithmetic would overflow.
fn mapped_row_bytes(base_is_null: bool, pitch: usize, w: u32, h: u32, rows: u32) -> Option<usize> {
    if base_is_null || w == 0 || h == 0 || h > rows {
        return None;
    }
    let row_bytes = (w as usize).checked_mul(4)?;
    if pitch < row_bytes {
        return None;
    }
    // The last row must end inside the mapping: (h - 1) * pitch + row_bytes.
    pitch
        .checked_mul((h - 1) as usize)?
        .checked_add(row_bytes)?;
    Some(row_bytes)
}

/// Current QPC value in ticks.
pub(crate) fn qpc_now() -> i64 {
    let mut v = 0i64;
    // SAFETY: v is a valid out-pointer for QueryPerformanceCounter.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v
}

/// QPC frequency in ticks per second.
pub(crate) fn qpc_frequency() -> i64 {
    let mut v = 0i64;
    // SAFETY: v is a valid out-pointer for QueryPerformanceFrequency.
    let _ = unsafe { QueryPerformanceFrequency(&mut v) };
    v
}

#[cfg(test)]
mod gate_tests {
    use super::accept_frame;

    #[test]
    fn first_frame_is_always_accepted() {
        assert!(accept_frame(None, 5, 100));
    }

    #[test]
    fn frames_closer_than_half_a_period_are_dropped() {
        // Period 100 ticks: half is 50.
        assert!(!accept_frame(Some(1000), 1000, 100));
        assert!(!accept_frame(Some(1000), 1049, 100));
        assert!(accept_frame(Some(1000), 1050, 100));
        assert!(accept_frame(Some(1000), 1100, 100));
    }
}

#[cfg(test)]
mod pitch_tests {
    use super::mapped_row_bytes;

    #[test]
    fn valid_pitch_returns_row_bytes() {
        assert_eq!(mapped_row_bytes(false, 400, 100, 10, 10), Some(400));
    }

    #[test]
    fn padded_pitch_is_accepted() {
        assert_eq!(mapped_row_bytes(false, 512, 100, 10, 10), Some(400));
    }

    #[test]
    fn pitch_smaller_than_row_is_rejected() {
        assert_eq!(mapped_row_bytes(false, 399, 100, 10, 10), None);
    }

    #[test]
    fn null_base_is_rejected() {
        assert_eq!(mapped_row_bytes(true, 400, 100, 10, 10), None);
    }

    #[test]
    fn too_few_mapped_rows_is_rejected() {
        assert_eq!(mapped_row_bytes(false, 400, 100, 10, 9), None);
    }

    #[test]
    fn zero_sizes_are_rejected() {
        assert_eq!(mapped_row_bytes(false, 400, 0, 10, 10), None);
        assert_eq!(mapped_row_bytes(false, 400, 100, 0, 10), None);
    }

    #[test]
    fn overflowing_sizes_are_rejected() {
        assert_eq!(
            mapped_row_bytes(false, usize::MAX, u32::MAX, u32::MAX, u32::MAX),
            None
        );
        assert_eq!(
            mapped_row_bytes(false, usize::MAX, 1, u32::MAX, u32::MAX),
            None
        );
        assert_eq!(mapped_row_bytes(false, usize::MAX / 2, 1, 4, 4), None);
    }
}
