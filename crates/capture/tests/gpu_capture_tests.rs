#![allow(dead_code, clippy::chunks_exact_to_as_chunks, clippy::approx_constant)]
//! GPU-mode WGC tests and rate-cap checks against real top-level windows created by the test.
//!
//! Win32 is called through local `extern "system"` declarations. Only windows created by this
//! file are ever captured.

#![cfg(windows)]

use std::ffi::c_void;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_capture::{CaptureConfig, CaptureError, Frame, GpuFrame, WindowCapturer};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

// COLORREF is 0x00BBGGRR.
const RED: u32 = 0x0000FF;
const GREEN: u32 = 0x00FF00;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const SW_MINIMIZE: i32 = 6;
const SW_RESTORE: i32 = 9;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const WM_APP: u32 = 0x8000;
const WM_REPAINT: u32 = WM_APP + 1;

const CLASS_NAME: &str = "clipcheck_capture_gpu_test_class";
const COLOR_TOLERANCE: i32 = 8;

/// Fill color used by the window procedure; changed from the test thread.
static COLOR: AtomicU32 = AtomicU32::new(RED);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PaintStruct {
    hdc: isize,
    erase: i32,
    paint: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassW {
    style: u32,
    lpfn_wnd_proc: WndProc,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: isize,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(wc: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *const c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn InvalidateRect(hwnd: isize, rect: *const Rect, erase: i32) -> i32;
    fn BeginPaint(hwnd: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(hwnd: isize, ps: *const PaintStruct) -> i32;
    fn GetClientRect(hwnd: isize, rect: *mut Rect) -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    fn FillRect(hdc: isize, rect: *const Rect, brush: isize) -> i32;
    fn DestroyWindow(hwnd: isize) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(color: u32) -> isize;
    fn DeleteObject(obj: isize) -> i32;
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PaintStruct::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = Rect::default();
                GetClientRect(hwnd, &mut rc);
                let brush = CreateSolidBrush(COLOR.load(Ordering::SeqCst));
                FillRect(hdc, &rc, brush);
                DeleteObject(brush);
                EndPaint(hwnd, &ps);
                0
            }
            WM_REPAINT => {
                InvalidateRect(hwnd, null(), 1);
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Runs on the window's own thread: owns the HWND and pumps its messages.
unsafe fn window_thread(tx: mpsc::Sender<isize>) {
    unsafe {
        let inst = GetModuleHandleW(null());
        let class = wide(CLASS_NAME);
        let wc = WndClassW {
            style: 0,
            lpfn_wnd_proc: wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: inst,
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            lpsz_menu_name: null(),
            lpsz_class_name: class.as_ptr(),
        };
        // Failure here just means the class already exists in this process.
        RegisterClassW(&wc);
        let title = wide("clipcheck gpu capture test");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            480,
            360,
            0,
            0,
            inst,
            null(),
        );
        let _ = tx.send(hwnd);
        if hwnd == 0 {
            return;
        }
        let mut msg = Msg::default();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Owns the window thread. Dropping it posts WM_CLOSE and joins the thread.
struct TestWindow {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl TestWindow {
    fn create() -> TestWindow {
        let (tx, rx) = mpsc::channel::<isize>();
        let thread = thread::spawn(move || unsafe { window_thread(tx) });
        let hwnd = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("window thread did not report an HWND within 5 s");
        assert!(hwnd != 0, "CreateWindowExW returned NULL");
        TestWindow {
            hwnd,
            thread: Some(thread),
        }
    }

    fn show(&self, cmd: i32) {
        unsafe { ShowWindow(self.hwnd, cmd) };
    }

    /// Changes the fill color and asks the window thread to repaint.
    fn set_color_and_repaint(&self, color: u32) {
        COLOR.store(color, Ordering::SeqCst);
        unsafe { PostMessageW(self.hwnd, WM_REPAINT, 0, 0) };
    }

    /// Outer (frame incl. title bar) size in pixels.
    fn outer_size(&self) -> (i32, i32) {
        let mut rc = Rect::default();
        unsafe { GetWindowRect(self.hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }

    /// Client-area size in pixels.
    fn client_size(&self) -> (i32, i32) {
        let mut rc = Rect::default();
        unsafe { GetClientRect(self.hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Keeps a window dirty by posting repaint requests every ~2 ms until dropped.
struct Repainter {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Repainter {
    fn start(hwnd: isize) -> Repainter {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let thread = thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                unsafe { PostMessageW(hwnd, WM_REPAINT, 0, 0) };
                thread::sleep(Duration::from_millis(2));
            }
        });
        Repainter {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Repainter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Receives items until `dur` has elapsed, appending them to `out`.
fn pump<T>(rx: &Receiver<T>, dur: Duration, out: &mut Vec<T>) {
    let deadline = Instant::now() + dur;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(f) => out.push(f),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// BGRA bytes of the center pixel of a CPU frame.
fn center_bgra(f: &Frame) -> [u8; 4] {
    assert_eq!(
        f.bgra.len(),
        f.width as usize * f.height as usize * 4,
        "bgra length {} != w*h*4 for {}x{}",
        f.bgra.len(),
        f.width,
        f.height
    );
    let x = (f.width / 2) as usize;
    let y = (f.height / 2) as usize;
    let idx = (y * f.width as usize + x) * 4;
    [
        f.bgra[idx],
        f.bgra[idx + 1],
        f.bgra[idx + 2],
        f.bgra[idx + 3],
    ]
}

/// True when B, G and R are each within tolerance of `want` (alpha ignored).
fn near(px: [u8; 4], want_bgr: [u8; 3]) -> bool {
    (0..3).all(|i| (px[i] as i32 - want_bgr[i] as i32).abs() <= COLOR_TOLERANCE)
}

/// BGR triple for a COLORREF value (0x00BBGGRR).
fn bgr_of(color: u32) -> [u8; 3] {
    [
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
    ]
}

fn assert_frame_shape(f: &Frame, ctx: &str) {
    assert!(
        f.width > 0 && f.height > 0,
        "{ctx}: zero size {}x{}",
        f.width,
        f.height
    );
    assert_eq!(
        f.bgra.len(),
        f.width as usize * f.height as usize * 4,
        "{ctx}: bgra.len()={} for {}x{}",
        f.bgra.len(),
        f.width,
        f.height
    );
}

fn skip_or_panic_env(test: &str, e: &CaptureError) -> bool {
    let reason = format!("{test}: graphics capture unavailable on this session: {e}");
    if std::env::var_os("CLIPCHECK_REQUIRE_CAPTURE").is_some() {
        panic!("{reason} (CLIPCHECK_REQUIRE_CAPTURE is set, so a skip is a failure)");
    }
    eprintln!("SKIPPED: {reason}");
    true
}

/// Starts CPU capture; `None` (after a SKIPPED line) when graphics capture is unavailable.
fn start_cpu_or_skip(test: &str, cfg: CaptureConfig) -> Option<(WindowCapturer, Receiver<Frame>)> {
    match WindowCapturer::start(cfg) {
        Ok(v) => Some(v),
        Err(e) if e.is_environment() => {
            skip_or_panic_env(test, &e);
            None
        }
        Err(e) => panic!("{test}: WindowCapturer::start failed unexpectedly: {e:?}"),
    }
}

/// Starts GPU capture; `None` (after a SKIPPED line) when graphics capture is unavailable.
fn start_gpu_or_skip(
    test: &str,
    cfg: CaptureConfig,
    device: &ID3D11Device,
) -> Option<(WindowCapturer, Receiver<GpuFrame>)> {
    match WindowCapturer::start_gpu(cfg, device) {
        Ok(v) => Some(v),
        Err(e) if e.is_environment() => {
            skip_or_panic_env(test, &e);
            None
        }
        Err(e) => panic!("{test}: WindowCapturer::start_gpu failed unexpectedly: {e:?}"),
    }
}

/// Creates the shared D3D11 device; `None` (after a SKIPPED line) when unavailable.
fn gpu_device(test: &str) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    match clipcheck_gpuconv::d3d::create_device(None) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("SKIPPED: {test}: D3D11 device unavailable: {e:?}");
            None
        }
    }
}

/// Copies `src` into a staging texture and returns (centre BGRA pixel, source desc).
fn read_center(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    src: &ID3D11Texture2D,
) -> ([u8; 4], D3D11_TEXTURE2D_DESC) {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { src.GetDesc(&mut desc) };
    let sd = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe {
        device
            .CreateTexture2D(&sd, None, Some(&mut staging))
            .expect("CreateTexture2D(staging) failed")
    };
    let staging = staging.expect("staging texture is None");
    unsafe { ctx.CopyResource(&staging, src) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .expect("Map(staging) failed")
    };
    let x = (desc.Width / 2) as usize;
    let y = (desc.Height / 2) as usize;
    let px = unsafe {
        let base = mapped.pData as *const u8;
        let off = y * mapped.RowPitch as usize + x * 4;
        [
            *base.add(off),
            *base.add(off + 1),
            *base.add(off + 2),
            *base.add(off + 3),
        ]
    };
    unsafe { ctx.Unmap(&staging, 0) };
    (px, desc)
}

/// Checks a live GPU frame's texture: BGRA, size matches the frame, centre pixel near `color`.
/// Returns the centre pixel.
fn check_live_gpu_frame(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    f: &GpuFrame,
    color: u32,
    ctx_label: &str,
) -> [u8; 4] {
    let tex = f
        .texture
        .as_ref()
        .unwrap_or_else(|| panic!("{ctx_label}: live frame has texture None"))
        .0
        .clone();
    let (px, desc) = read_center(device, ctx, &tex);
    assert_eq!(
        desc.Format, DXGI_FORMAT_B8G8R8A8_UNORM,
        "{ctx_label}: texture format {:?}",
        desc.Format
    );
    assert_eq!(
        desc.Width, f.width,
        "{ctx_label}: texture width != frame width"
    );
    assert_eq!(
        desc.Height, f.height,
        "{ctx_label}: texture height != frame height"
    );
    assert!(
        f.width > 0 && f.height > 0,
        "{ctx_label}: zero size {}x{}",
        f.width,
        f.height
    );
    assert!(
        near(px, bgr_of(color)),
        "{ctx_label}: centre px BGRA {:?} not near window colour {:06X}",
        px,
        color
    );
    px
}

#[test]
fn gpu_mode_30fps_frames_colour_and_pts() {
    let test = "gpu_mode_30fps_frames_colour_and_pts";
    let Some((device, ctx)) = gpu_device(test) else {
        return;
    };
    let win = TestWindow::create();
    win.set_color_and_repaint(RED);
    thread::sleep(Duration::from_millis(200));
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let Some((cap, rx)) = start_gpu_or_skip(test, cfg, &device) else {
        return;
    };

    let mut frames: Vec<GpuFrame> = Vec::new();
    let t0 = Instant::now();
    pump(&rx, Duration::from_millis(700), &mut frames);
    win.set_color_and_repaint(GREEN);
    let repaint_at = frames.len();
    pump(&rx, Duration::from_millis(800), &mut frames);
    let elapsed = t0.elapsed().as_secs_f64();
    let errors = cap.callback_errors();
    cap.stop();

    let n = frames.len();
    let (cw, ch) = win.client_size();
    println!(
        "[gpu_30fps] frames={n} in {elapsed:.3}s => {:.2} fps; repaint after frame index {repaint_at}; \
         client {cw}x{ch}; callback_errors={errors}",
        n as f64 / elapsed
    );
    assert!(
        (35..=50).contains(&n),
        "expected 35..=50 frames in ~1.5 s at 30 fps, got {n}"
    );
    assert_eq!(errors, 0, "callback_errors must be 0");

    let live: Vec<&GpuFrame> = frames.iter().filter(|f| !f.is_black).collect();
    assert!(!live.is_empty(), "no live frames");
    for f in &frames {
        if !f.is_black {
            assert!(f.texture.is_some(), "live frame without texture");
        } else {
            assert!(f.texture.is_none(), "black frame must have texture None");
        }
    }

    // Size semantics: frame size should roughly match the client area.
    let first = live[0];
    println!(
        "[gpu_30fps] first live frame {}x{} (client {cw}x{ch})",
        first.width, first.height
    );
    // WGC captures the visible window (title bar included), so the frame is between the client
    // area and the outer window rect (DWM invisible borders excluded).
    let (ow, oh) = win.outer_size();
    println!("[gpu_30fps] outer window {ow}x{oh}");
    assert!(
        first.width as i32 >= cw - 2
            && first.width as i32 <= ow + 2
            && first.height as i32 >= ch - 2
            && first.height as i32 <= oh + 2,
        "frame size {}x{} not between client {cw}x{ch} and outer {ow}x{oh}",
        first.width,
        first.height
    );

    // Colour: first live frame is RED; once GREEN appears it never reverts.
    // Each live frame must be RED or GREEN (never a third colour); once GREEN appears it must
    // never revert to RED.
    let mut seen_green_at: Option<usize> = None;
    for (i, f) in frames.iter().enumerate() {
        if f.is_black {
            continue;
        }
        let tex = f.texture.as_ref().expect("live frame texture").0.clone();
        let (px, _) = read_center(&device, &ctx, &tex);
        let is_green = near(px, bgr_of(GREEN));
        let is_red = near(px, bgr_of(RED));
        assert!(
            is_green || is_red,
            "frame {i}: centre px {px:?} is neither RED nor GREEN"
        );
        if is_green && seen_green_at.is_none() {
            seen_green_at = Some(i);
        }
        if seen_green_at.is_some() {
            assert!(
                is_green,
                "frame {i}: reverted to RED after GREEN at {seen_green_at:?}"
            );
        }
    }
    println!(
        "[gpu_30fps] first GREEN frame index {seen_green_at:?} (repaint after index {repaint_at}); \
         first live frame centre checked RED"
    );
    assert!(
        seen_green_at.is_some(),
        "no GREEN frame after repainting the window green"
    );
    check_live_gpu_frame(&device, &ctx, first, RED, "first live frame");

    // pts strictly increasing, mean spacing ~33.3 ms.
    for w in frames.windows(2) {
        assert!(
            w[1].pts_100ns > w[0].pts_100ns,
            "pts not strictly increasing: {} then {}",
            w[0].pts_100ns,
            w[1].pts_100ns
        );
    }
    if n >= 2 {
        let span_ms = (frames[n - 1].pts_100ns - frames[0].pts_100ns) as f64 / 10_000.0;
        let mean_ms = span_ms / (n as f64 - 1.0);
        println!("[gpu_30fps] mean pts spacing {mean_ms:.2} ms (target 33.3)");
        assert!(
            (28.0..=40.0).contains(&mean_ms),
            "mean pts spacing {mean_ms:.2} ms not ~33.3 ms"
        );
    }
}

#[test]
fn gpu_mode_hide_then_restore_gives_black_then_live() {
    let test = "gpu_mode_hide_then_restore_gives_black_then_live";
    let Some((device, ctx)) = gpu_device(test) else {
        return;
    };
    let win = TestWindow::create();
    win.set_color_and_repaint(RED);
    thread::sleep(Duration::from_millis(200));
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let Some((cap, rx)) = start_gpu_or_skip(test, cfg, &device) else {
        return;
    };

    let mut before: Vec<GpuFrame> = Vec::new();
    pump(&rx, Duration::from_millis(500), &mut before);

    win.show(SW_MINIMIZE);
    let mut hidden: Vec<GpuFrame> = Vec::new();
    pump(&rx, Duration::from_millis(1200), &mut hidden);

    win.show(SW_RESTORE);
    win.set_color_and_repaint(RED);
    let mut after: Vec<GpuFrame> = Vec::new();
    pump(&rx, Duration::from_millis(1000), &mut after);
    let errors = cap.callback_errors();
    cap.stop();

    let live_before = before.iter().filter(|f| !f.is_black).count();
    let black_hidden = hidden.iter().filter(|f| f.is_black).count();
    let live_hidden = hidden.iter().filter(|f| !f.is_black).count();
    let live_after = after.iter().filter(|f| !f.is_black).count();
    println!(
        "[gpu_hide] before: {} frames ({live_before} live); hidden: {} frames ({black_hidden} black, {live_hidden} live); \
         after restore: {} frames ({live_after} live); callback_errors={errors}",
        before.len(),
        hidden.len(),
        after.len()
    );
    assert_eq!(errors, 0, "callback_errors must be 0");
    assert!(live_before > 0, "no live frames before minimize");
    assert!(
        black_hidden > 0,
        "no black frames while minimized (expected is_black frames with texture None)"
    );
    for f in hidden.iter().filter(|f| f.is_black) {
        assert!(f.texture.is_none(), "black frame has a texture");
    }
    // Once the window is hidden past the grace period, the tail of the hidden phase should be black.
    let tail = &hidden[hidden.len().saturating_sub(5)..];
    assert!(
        tail.iter().all(|f| f.is_black),
        "last hidden frames should be black"
    );
    assert!(live_after > 0, "no live frames after restore");
    let last = after.iter().rev().find(|f| !f.is_black).expect("live");
    check_live_gpu_frame(&device, &ctx, last, RED, "after restore");
}

#[test]
fn gpu_and_cpu_rate_cap_10fps_under_continuous_repaint() {
    let test = "gpu_and_cpu_rate_cap_10fps_under_continuous_repaint";
    let Some((device, _ctx)) = gpu_device(test) else {
        return;
    };
    let win = TestWindow::create();
    win.set_color_and_repaint(RED);
    thread::sleep(Duration::from_millis(100));
    let _repaint = Repainter::start(win.hwnd);
    thread::sleep(Duration::from_millis(100));

    // GPU mode.
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 10,
    };
    if let Some((cap, rx)) = start_gpu_or_skip(test, cfg, &device) {
        let mut frames: Vec<GpuFrame> = Vec::new();
        let t0 = Instant::now();
        pump(&rx, Duration::from_millis(2000), &mut frames);
        let secs = t0.elapsed().as_secs_f64();
        let errors = cap.callback_errors();
        cap.stop();
        let fps = frames.len() as f64 / secs;
        println!(
            "[rate_cap_gpu] {} frames in {secs:.3}s => {fps:.2} fps (target 10); callback_errors={errors}",
            frames.len()
        );
        assert_eq!(errors, 0, "GPU callback_errors must be 0");
        assert!(
            (8.0..=12.0).contains(&fps),
            "GPU delivered {fps:.2} fps under repaint, expected 8..=12"
        );
    }

    // CPU mode.
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 10,
    };
    if let Some((cap, rx)) = start_cpu_or_skip(test, cfg) {
        let mut frames: Vec<Frame> = Vec::new();
        let t0 = Instant::now();
        pump(&rx, Duration::from_millis(2000), &mut frames);
        let secs = t0.elapsed().as_secs_f64();
        let errors = cap.callback_errors();
        cap.stop();
        let fps = frames.len() as f64 / secs;
        println!(
            "[rate_cap_cpu] {} frames in {secs:.3}s => {fps:.2} fps (target 10); callback_errors={errors}",
            frames.len()
        );
        assert_eq!(errors, 0, "CPU callback_errors must be 0");
        assert!(
            (8.0..=12.0).contains(&fps),
            "CPU delivered {fps:.2} fps under repaint, expected 8..=12"
        );
    }
}

#[test]
fn start_gpu_rejects_invalid_fps_and_hwnd() {
    let test = "start_gpu_rejects_invalid_fps_and_hwnd";
    let Some((device, _ctx)) = gpu_device(test) else {
        return;
    };
    let win = TestWindow::create();

    for fps in [0u32, 241u32] {
        let cfg = CaptureConfig {
            hwnd: win.hwnd,
            fps,
        };
        match WindowCapturer::start_gpu(cfg, &device) {
            Err(CaptureError::InvalidFps(v)) => {
                println!("[invalid_fps] fps={fps} -> InvalidFps({v})");
                assert_eq!(v, fps);
            }
            Err(e) if e.is_environment() => {
                skip_or_panic_env(test, &e);
                return;
            }
            Err(e) => panic!("fps={fps}: expected InvalidFps, got {e:?}"),
            Ok((cap, _rx)) => {
                cap.stop();
                panic!("fps={fps}: start_gpu unexpectedly succeeded");
            }
        }
    }

    for hwnd in [0isize, 0x1234_5678_isize] {
        let cfg = CaptureConfig { hwnd, fps: 30 };
        match WindowCapturer::start_gpu(cfg, &device) {
            Err(CaptureError::InvalidWindow(h)) => {
                println!("[invalid_hwnd] hwnd={hwnd:#x} -> InvalidWindow({h:#x})");
            }
            Err(e) if e.is_environment() => {
                skip_or_panic_env(test, &e);
                return;
            }
            Err(e) => panic!("hwnd={hwnd:#x}: expected InvalidWindow, got {e:?}"),
            Ok((cap, _rx)) => {
                cap.stop();
                panic!("hwnd={hwnd:#x}: start_gpu unexpectedly succeeded");
            }
        }
    }
}

#[test]
fn start_gpu_stop_stress_10_times() {
    let test = "start_gpu_stop_stress_10_times";
    let Some((device, _ctx)) = gpu_device(test) else {
        return;
    };
    let mut total_frames = 0usize;
    for i in 0..10 {
        let win = TestWindow::create();
        win.set_color_and_repaint(RED);
        thread::sleep(Duration::from_millis(50));
        let cfg = CaptureConfig {
            hwnd: win.hwnd,
            fps: 30,
        };
        let Some((cap, rx)) = start_gpu_or_skip(test, cfg, &device) else {
            return;
        };
        let mut frames: Vec<GpuFrame> = Vec::new();
        pump(&rx, Duration::from_millis(300), &mut frames);
        let errors = cap.callback_errors();
        cap.stop();
        println!(
            "[stress] iter {i}: {} frames, callback_errors={errors}",
            frames.len()
        );
        assert_eq!(errors, 0, "iter {i}: callback_errors must be 0");
        assert!(!frames.is_empty(), "iter {i}: no frames delivered");
        total_frames += frames.len();
        drop(win);
    }
    println!("[stress] total frames over 10 start/stop cycles: {total_frames}");
}

#[test]
fn cpu_and_gpu_concurrent_on_same_window() {
    let test = "cpu_and_gpu_concurrent_on_same_window";
    let Some((device, _ctx)) = gpu_device(test) else {
        return;
    };
    let win = TestWindow::create();
    win.set_color_and_repaint(RED);
    thread::sleep(Duration::from_millis(100));

    let cpu_cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let Some((cpu_cap, cpu_rx)) = start_cpu_or_skip(test, cpu_cfg) else {
        return;
    };
    let gpu_cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let Some((gpu_cap, gpu_rx)) = start_gpu_or_skip(test, gpu_cfg, &device) else {
        cpu_cap.stop();
        return;
    };

    let mut cpu_frames: Vec<Frame> = Vec::new();
    let mut gpu_frames: Vec<GpuFrame> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        let mut progressed = false;
        if let Ok(f) = cpu_rx.try_recv() {
            cpu_frames.push(f);
            progressed = true;
        }
        if let Ok(f) = gpu_rx.try_recv() {
            gpu_frames.push(f);
            progressed = true;
        }
        if !progressed {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let cpu_errors = cpu_cap.callback_errors();
    let gpu_errors = gpu_cap.callback_errors();
    cpu_cap.stop();
    gpu_cap.stop();

    println!(
        "[concurrent] cpu frames={} gpu frames={} (live gpu={}); callback_errors cpu={cpu_errors} gpu={gpu_errors}",
        cpu_frames.len(),
        gpu_frames.len(),
        gpu_frames.iter().filter(|f| !f.is_black).count()
    );
    assert!(!cpu_frames.is_empty(), "CPU mode delivered no frames");
    assert!(!gpu_frames.is_empty(), "GPU mode delivered no frames");
    assert_eq!(cpu_errors, 0, "CPU callback_errors must be 0");
    assert_eq!(gpu_errors, 0, "GPU callback_errors must be 0");
    for f in &cpu_frames {
        assert_frame_shape(f, "concurrent cpu frame");
    }
    let cpu_dims = (cpu_frames[0].width, cpu_frames[0].height);
    let gpu_dims = gpu_frames
        .iter()
        .find(|f| !f.is_black)
        .map(|f| (f.width, f.height))
        .expect("at least one live GPU frame");
    println!("[concurrent] cpu first frame {cpu_dims:?}, gpu first live frame {gpu_dims:?}");
    assert!(
        (cpu_dims.0 as i32 - gpu_dims.0 as i32).abs() <= 2
            && (cpu_dims.1 as i32 - gpu_dims.1 as i32).abs() <= 2,
        "CPU frame size {cpu_dims:?} differs from GPU frame size {gpu_dims:?}"
    );
}
