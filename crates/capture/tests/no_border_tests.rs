//! Tests for the opt-in no-border (DDA) capture source and its pure geometry helpers.
//!
//! Only windows created by this file are ever captured. Win32 is called through local
//! `extern "system"` declarations. Environment problems print `SKIPPED:` lines unless
//! `CLIPCHECK_REQUIRE_CAPTURE=1`, in which case they fail.

#![cfg(windows)]

use std::ffi::c_void;
use std::ptr::null;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Once;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_capture::dda_geom::{covers_monitor, crop_box, rotation_supported, Rect, MIN_CROP};
use clipcheck_capture::{
    no_border_attempts, CaptureBackend, CaptureConfig, CaptureError, GpuFrame, WindowCapturer,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

// COLORREF is 0x00BBGGRR.
const RED: u32 = 0x0000_00FF;
const COLOR_TOLERANCE: i32 = 8;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const HWND_TOPMOST: isize = -1;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_SHOWWINDOW: u32 = 0x0040;

const CLASS_NAME: &str = "clipcheck_no_border_test_class";

/// Fill colour used by the window procedure; set by `spawn_test_window` before the window exists.
static COLOR: AtomicU32 = AtomicU32::new(RED);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRect {
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
    paint: WinRect,
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
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn BeginPaint(hwnd: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(hwnd: isize, ps: *const PaintStruct) -> i32;
    fn GetClientRect(hwnd: isize, rect: *mut WinRect) -> i32;
    fn FillRect(hdc: isize, rect: *const WinRect, brush: isize) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32)
        -> i32;
    fn SetProcessDpiAwarenessContext(value: isize) -> i32;
}

static DPI_AWARE: Once = Once::new();

/// Makes this test process per-monitor DPI aware so GetClientRect and captured frames both use
/// physical pixels (otherwise client sizes are virtualised and differ from the texture size).
fn ensure_dpi_aware() {
    DPI_AWARE.call_once(|| {
        // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4
        unsafe { SetProcessDpiAwarenessContext(-4) };
    });
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
                let mut rc = WinRect::default();
                GetClientRect(hwnd, &mut rc);
                let brush = CreateSolidBrush(COLOR.load(Ordering::SeqCst));
                FillRect(hdc, &rc, brush);
                DeleteObject(brush);
                EndPaint(hwnd, &ps);
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

static REGISTER: Once = Once::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn register_class() {
    REGISTER.call_once(|| {
        let name = wide(CLASS_NAME);
        let wc = WndClassW {
            style: 0,
            lpfn_wnd_proc: wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: unsafe { GetModuleHandleW(null()) },
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            lpsz_menu_name: null(),
            lpsz_class_name: name.as_ptr(),
        };
        unsafe { RegisterClassW(&wc) };
    });
}

/// A top-level window owned by this test file. Dropping it closes the window and joins its thread.
struct TestWindow {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
            let _ = t.join();
        }
    }
}

/// Creates a visible window at (100, 100) with a solid `colour` client area, on its own thread
/// that pumps messages. `outer_w`/`outer_h` are the outer (frame-inclusive) sizes.
fn spawn_test_window(outer_w: i32, outer_h: i32, colour: u32, topmost: bool) -> TestWindow {
    ensure_dpi_aware();
    COLOR.store(colour, Ordering::SeqCst);
    register_class();
    let (tx, rx) = mpsc::channel::<isize>();
    let thread = thread::spawn(move || unsafe {
        let class = wide(CLASS_NAME);
        let title = wide("clipcheck no-border test");
        let hinst = GetModuleHandleW(null());
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            outer_w,
            outer_h,
            0,
            0,
            hinst,
            null(),
        );
        if hwnd != 0 && topmost {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                100,
                100,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_SHOWWINDOW,
            );
        }
        let _ = tx.send(hwnd);
        if hwnd == 0 {
            return;
        }
        let mut msg = Msg::default();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
    let hwnd = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("test window thread did not report a handle");
    assert!(hwnd != 0, "CreateWindowExW failed for the test window");
    TestWindow {
        hwnd,
        thread: Some(thread),
    }
}

fn client_size(hwnd: isize) -> (i32, i32) {
    let mut rc = WinRect::default();
    unsafe { GetClientRect(hwnd, &mut rc) };
    (rc.right - rc.left, rc.bottom - rc.top)
}

/// Reports a skip (or fails when `CLIPCHECK_REQUIRE_CAPTURE=1`).
fn skip(test: &str, reason: &str) {
    if std::env::var("CLIPCHECK_REQUIRE_CAPTURE").is_ok_and(|v| v == "1") {
        panic!("{test}: {reason} (CLIPCHECK_REQUIRE_CAPTURE=1, so a skip is a failure)");
    }
    eprintln!("SKIPPED: {test}: {reason}");
}

/// Creates the shared D3D11 device; `None` (after a SKIPPED line) when unavailable.
fn gpu_device(test: &str) -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    match clipcheck_gpuconv::d3d::create_device(None) {
        Ok(v) => Some(v),
        Err(e) => {
            skip(test, &format!("D3D11 device unavailable: {e:?}"));
            None
        }
    }
}

/// Starts capture with `backend`; `None` (after a SKIPPED line) on environment errors.
fn start_or_skip(
    test: &str,
    cfg: CaptureConfig,
    device: &ID3D11Device,
    backend: CaptureBackend,
) -> Option<(WindowCapturer, Receiver<GpuFrame>)> {
    match WindowCapturer::start_gpu_with(cfg, device, backend) {
        Ok(v) => Some(v),
        Err(CaptureError::NoBorderUnavailable { reason }) => {
            skip(test, &format!("no-border unavailable: {reason}"));
            None
        }
        Err(e) if e.is_environment() => {
            skip(test, &format!("{e:?}"));
            None
        }
        Err(e) => panic!("{test}: start_gpu_with({backend:?}) failed unexpectedly: {e:?}"),
    }
}

/// Copies the texture to a staging texture and returns (width, height, BGRA centre pixel).
fn centre_pixel(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
) -> (u32, u32, [u8; 4]) {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    assert_eq!(
        desc.Format, DXGI_FORMAT_B8G8R8A8_UNORM,
        "frame texture is not B8G8R8A8_UNORM"
    );
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
    unsafe { ctx.CopyResource(&staging, tex) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .expect("Map(staging) failed")
    };
    let (w, h) = (desc.Width, desc.Height);
    let (x, y) = ((w / 2) as usize, (h / 2) as usize);
    let pixel = unsafe {
        let base = mapped.pData as *const u8;
        let p = base.add(y * mapped.RowPitch as usize + x * 4);
        [*p, *p.add(1), *p.add(2), *p.add(3)]
    };
    unsafe { ctx.Unmap(&staging, 0) };
    (w, h, pixel)
}

fn channel_close(actual: [u8; 4], colourref: u32, tol: i32) -> bool {
    // COLORREF is 0x00BBGGRR: want = [R, G, B].
    let want = [
        (colourref & 0xFF) as u8,
        ((colourref >> 8) & 0xFF) as u8,
        ((colourref >> 16) & 0xFF) as u8,
    ];
    // actual is BGRA bytes [B, G, R, A]: got = [R, G, B].
    let got = [actual[2], actual[1], actual[0]];
    (0..3).all(|i| (got[i] as i32 - want[i] as i32).abs() <= tol)
}

// ---------------------------------------------------------------------------
// GPU / window tests
// ---------------------------------------------------------------------------

#[test]
fn default_backend_is_wgc() {
    assert_eq!(CaptureBackend::default(), CaptureBackend::Wgc);
}

#[test]
fn opt_in_gate_wgc_never_enters_no_border() {
    const T: &str = "opt_in_gate_wgc_never_enters_no_border";
    let Some((dev, _ctx)) = gpu_device(T) else {
        return;
    };
    let win = spawn_test_window(400, 300, RED, false);
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };

    let before = no_border_attempts();
    let Some((cap, _rx)) = start_or_skip(T, cfg, &dev, CaptureBackend::Wgc) else {
        return;
    };
    cap.stop();
    match WindowCapturer::start_gpu(cfg, &dev) {
        Ok((cap2, _rx2)) => cap2.stop(),
        Err(e) if e.is_environment() => skip(T, &format!("start_gpu(Wgc): {e:?}")),
        Err(e) => panic!("{T}: start_gpu failed unexpectedly: {e:?}"),
    }
    assert_eq!(
        no_border_attempts(),
        before,
        "Wgc starts must not enter the no-border source"
    );
}

#[test]
fn require_cover_refuses_small_window() {
    const T: &str = "require_cover_refuses_small_window";
    let Some((dev, _ctx)) = gpu_device(T) else {
        return;
    };
    let win = spawn_test_window(400, 300, RED, true);
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };

    let before = no_border_attempts();
    match WindowCapturer::start_gpu_with(
        cfg,
        &dev,
        CaptureBackend::NoBorder {
            require_cover: true,
        },
    ) {
        Ok(_) => panic!("a 400x300 window must not pass the require_cover gate"),
        Err(CaptureError::NoBorderUnavailable { reason }) => {
            assert!(
                reason.to_ascii_lowercase().contains("cover"),
                "refusal reason should mention cover, got: {reason:?}"
            );
        }
        Err(e) if e.is_environment() => {
            skip(T, &format!("{e:?}"));
            return;
        }
        Err(e) => panic!("unexpected error: {e:?}"),
    }
    assert_eq!(
        no_border_attempts(),
        before + 1,
        "exactly one no-border attempt should be counted"
    );

    // No thread should be left behind: a normal Wgc start must still work.
    if let Some((cap, _rx)) = start_or_skip(T, cfg, &dev, CaptureBackend::Wgc) {
        cap.stop();
    }
}

#[test]
fn no_border_frames_match_client_size_and_colour() {
    const T: &str = "no_border_frames_match_client_size_and_colour";
    let Some((dev, ctx)) = gpu_device(T) else {
        return;
    };
    let win = spawn_test_window(640, 480, RED, true);
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let (cw, ch) = client_size(win.hwnd);
    let Some((cap, rx)) = start_or_skip(
        T,
        cfg,
        &dev,
        CaptureBackend::NoBorder {
            require_cover: false,
        },
    ) else {
        return;
    };

    // Wait up to 10 s for a frame whose size and centre pixel are correct.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last: Option<(u32, u32, u32, u32, [u8; 4])> = None;
    let mut ok = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                let Some(tex) = frame.texture.as_ref() else {
                    continue;
                };
                let (tw, th, px) = centre_pixel(&dev, &ctx, &tex.0);
                last = Some((frame.width, frame.height, tw, th, px));
                let size_ok =
                    (frame.width as i32 - cw).abs() <= 3 && (frame.height as i32 - ch).abs() <= 3;
                if size_ok && channel_close(px, RED, COLOR_TOLERANCE) {
                    ok = true;
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    cap.stop();
    let (fw, fh, tw, th, px) =
        last.unwrap_or_else(|| panic!("{T}: no frame with a texture arrived"));
    assert!(ok, "{T}: frame {fw}x{fh} (texture {tw}x{th}) vs client {cw}x{ch}; centre BGRA {px:?}, expected red");
}

#[test]
fn invalid_hwnd_is_rejected() {
    const T: &str = "invalid_hwnd_is_rejected";
    let Some((dev, _ctx)) = gpu_device(T) else {
        return;
    };
    for hwnd in [0isize, 0x1234] {
        let cfg = CaptureConfig { hwnd, fps: 30 };
        let r = WindowCapturer::start_gpu_with(
            cfg,
            &dev,
            CaptureBackend::NoBorder {
                require_cover: false,
            },
        );
        assert!(
            matches!(r, Err(CaptureError::InvalidWindow(_))),
            "hwnd {hwnd:#x}: expected InvalidWindow"
        );
    }
}

#[test]
fn invalid_fps_is_rejected() {
    const T: &str = "invalid_fps_is_rejected";
    let Some((dev, _ctx)) = gpu_device(T) else {
        return;
    };
    let win = spawn_test_window(400, 300, RED, false);
    for fps in [0u32, 241] {
        let cfg = CaptureConfig {
            hwnd: win.hwnd,
            fps,
        };
        let r = WindowCapturer::start_gpu_with(
            cfg,
            &dev,
            CaptureBackend::NoBorder {
                require_cover: false,
            },
        );
        match r {
            Err(CaptureError::InvalidFps(v)) => assert_eq!(v, fps),
            other => panic!(
                "fps {fps}: expected InvalidFps, got {:?}",
                other.map(|_| "Ok")
            ),
        }
    }
}

#[test]
fn no_border_start_stop_loop_is_prompt() {
    const T: &str = "no_border_start_stop_loop_is_prompt";
    let Some((dev, _ctx)) = gpu_device(T) else {
        return;
    };
    let win = spawn_test_window(640, 480, RED, true);
    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let began = Instant::now();
    let mut started = 0;
    for _ in 0..5 {
        let Some((cap, rx)) = start_or_skip(
            T,
            cfg,
            &dev,
            CaptureBackend::NoBorder {
                require_cover: false,
            },
        ) else {
            break;
        };
        started += 1;
        let _ = rx.recv_timeout(Duration::from_secs(2));
        cap.stop();
    }
    let elapsed = began.elapsed();
    if started == 0 {
        return;
    }
    assert_eq!(started, 5, "all five starts should succeed once one has");
    assert!(
        elapsed < Duration::from_secs(10),
        "5 start/stop cycles took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Pure geometry tests (clipcheck_capture::dda_geom)
// ---------------------------------------------------------------------------

#[test]
fn rect_basics() {
    let r = Rect::new(10, 20, 110, 70);
    assert_eq!(r.width(), 100);
    assert_eq!(r.height(), 50);
    assert_eq!(r.area(), 5000);
    assert!(!r.is_empty());
    assert!(
        Rect::new(5, 5, 5, 10).is_empty(),
        "zero-width rect is empty"
    );
}

#[test]
fn rect_intersect_overlap_and_disjoint() {
    let a = Rect::new(0, 0, 100, 100);
    let i = a.intersect(&Rect::new(50, 50, 150, 150));
    assert_eq!((i.left, i.top, i.right, i.bottom), (50, 50, 100, 100));

    let d = a.intersect(&Rect::new(200, 200, 300, 300));
    assert_eq!((d.left, d.top, d.right, d.bottom), (0, 0, 0, 0));
    assert!(d.is_empty());
}

#[test]
fn rect_translate_moves_all_edges() {
    let t = Rect::new(0, 0, 10, 10).translate(-5, 3);
    assert_eq!((t.left, t.top, t.right, t.bottom), (-5, 3, 5, 13));
}

#[test]
fn covers_exact_95_percent_boundary() {
    let mon = Rect::new(0, 0, 100, 100); // area 10000
    assert!(
        covers_monitor(Rect::new(0, 0, 95, 100), mon),
        "95% must count as covering"
    );
    assert!(
        !covers_monitor(Rect::new(0, 0, 94, 100), mon),
        "94% must not count as covering"
    );
    assert!(
        covers_monitor(Rect::new(-10, -10, 200, 200), mon),
        "a window larger than the monitor covers it"
    );
}

#[test]
fn covers_negative_origin_monitor() {
    let mon = Rect::new(-1920, 0, 0, 1080);
    assert!(covers_monitor(Rect::new(-1920, 0, 0, 1080), mon));
    assert!(!covers_monitor(Rect::new(-1920, 0, -960, 1080), mon));
}

#[test]
fn covers_empty_monitor_is_false() {
    assert!(!covers_monitor(
        Rect::new(0, 0, 100, 100),
        Rect::new(0, 0, 0, 0)
    ));
}

#[test]
fn window_spanning_two_outputs_does_not_cover_either() {
    let a = Rect::new(0, 0, 1920, 1080);
    let win = Rect::new(1000, 0, 2000, 500);
    assert!(!covers_monitor(win, a), "about 22% of output A is covered");
}

#[test]
fn window_spanning_two_outputs_crops_to_second_output() {
    let out_b = Rect::new(1920, 0, 3840, 1080);
    let c = crop_box(Rect::new(1000, 0, 2000, 500), out_b).expect("visible part is 80x500");
    assert_eq!((c.left, c.top, c.width, c.height), (0, 0, 80, 500));
}

#[test]
fn crop_box_negative_origin_output() {
    let out = Rect::new(-1920, 0, 0, 1080);
    let c = crop_box(Rect::new(-1000, 100, -500, 600), out).expect("visible");
    assert_eq!((c.left, c.top, c.width, c.height), (920, 100, 500, 500));
}

#[test]
fn crop_box_fully_outside_is_none() {
    let out = Rect::new(0, 0, 1920, 1080);
    assert!(crop_box(Rect::new(3000, 3000, 3500, 3500), out).is_none());
}

#[test]
fn min_crop_boundary_15_vs_16() {
    assert_eq!(MIN_CROP, 16, "MIN_CROP is specified as 16");
    let out = Rect::new(0, 0, 1920, 1080);
    assert!(
        crop_box(Rect::new(0, 0, 15, 500), out).is_none(),
        "width 15 is below MIN_CROP"
    );
    assert!(
        crop_box(Rect::new(0, 0, 500, 15), out).is_none(),
        "height 15 is below MIN_CROP"
    );
    let c = crop_box(Rect::new(100, 100, 116, 600), out).expect("width 16 is allowed");
    assert_eq!((c.left, c.top, c.width, c.height), (100, 100, 16, 500));
}

#[test]
fn geometry_extremes_do_not_panic() {
    let full = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    let small = Rect::new(0, 0, 10, 10);
    let _ = full.width();
    let _ = full.height();
    let _ = full.area();
    let _ = full.is_empty();
    let _ = full.intersect(&small);
    let _ = full.intersect(&full);
    let _ = full.translate(i32::MAX, i32::MIN);
    let _ = covers_monitor(full, small);
    let _ = covers_monitor(small, full);
    let _ = crop_box(full, full);
    let _ = crop_box(full, small);
    let _ = crop_box(small, full);
}

#[test]
fn rotation_supported_only_0_and_1() {
    assert!(rotation_supported(0));
    assert!(rotation_supported(1));
    for r in [2, 3, -1, 90, 180, 270, i32::MIN, i32::MAX] {
        assert!(!rotation_supported(r), "rotation {r} must not be supported");
    }
}
