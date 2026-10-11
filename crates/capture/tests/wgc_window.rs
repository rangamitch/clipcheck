#![allow(clippy::chunks_exact_to_as_chunks, clippy::approx_constant)]
//! End-to-end WGC tests against a real top-level window.
//!
//! Win32 is called through local `extern "system"` declarations so the test
//! does not depend on the crate's `windows` dependency being re-exported.

#![cfg(windows)]

use std::ffi::c_void;
use std::ptr::null;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_capture::{CaptureConfig, CaptureError, Frame, WindowCapturer};

// COLORREF is 0x00BBGGRR.
const RED: u32 = 0x0000FF;
const BLUE: u32 = 0xFF0000;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const SW_MINIMIZE: i32 = 6;
const SW_RESTORE: i32 = 9;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const WM_APP: u32 = 0x8000;
const WM_REPAINT: u32 = WM_APP + 1;

const CLASS_NAME: &str = "clipcheck_capture_test_class";
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
        let title = wide("clipcheck capture test");
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
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Receives frames until `dur` has elapsed, appending them to `out`.
fn pump(rx: &Receiver<Frame>, dur: Duration, out: &mut Vec<Frame>) {
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

/// BGRA bytes of the center pixel.
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

/// Starts capture for `test`. Returns `None` only when this session cannot do graphics capture
/// at all (`CaptureError::is_environment`), after printing a visible `SKIPPED:` line. Any other
/// start failure panics. When `CLIPCHECK_REQUIRE_CAPTURE` is set,
/// an environment skip panics too.
fn start_or_skip(test: &str, cfg: CaptureConfig) -> Option<(WindowCapturer, Receiver<Frame>)> {
    match WindowCapturer::start(cfg) {
        Ok(v) => Some(v),
        Err(e) if e.is_environment() => {
            let reason = format!("{test}: graphics capture unavailable on this session: {e}");
            if std::env::var_os("CLIPCHECK_REQUIRE_CAPTURE").is_some() {
                panic!("{reason} (CLIPCHECK_REQUIRE_CAPTURE is set, so a skip is a failure)");
            }
            eprintln!("SKIPPED: {reason}");
            None
        }
        Err(e) => panic!("{test}: WindowCapturer::start failed unexpectedly: {e:?}"),
    }
}

#[test]
fn wgc_window_capture_flow() {
    let win = TestWindow::create();
    thread::sleep(Duration::from_millis(300));

    let cfg = CaptureConfig {
        hwnd: win.hwnd,
        fps: 30,
    };
    let Some((capturer, rx)) = start_or_skip("wgc_window_capture_flow", cfg) else {
        return;
    };

    // Only pts values are kept for the monotonic check; frame buffers are dropped per phase.
    let mut timeline_pts: Vec<i64> = Vec::new();

    // 1. Red phase: 1 s of frames.
    let mut red = Vec::new();
    pump(&rx, Duration::from_secs(1), &mut red);
    assert!(
        red.len() >= 10,
        "red phase: expected >= 10 frames in 1 s at 30 fps, got {}",
        red.len()
    );
    for (i, f) in red.iter().enumerate() {
        assert_frame_shape(f, &format!("red frame {i}"));
        assert!(!f.is_black, "red frame {i} unexpectedly is_black=true");
    }
    let red_last = red.last().unwrap();
    let px = center_bgra(red_last);
    assert!(
        near(px, [0, 0, 255]),
        "red phase: center BGRA {:?} not ~[0,0,255] (frames={}, size {}x{})",
        px,
        red.len(),
        red_last.width,
        red_last.height
    );
    timeline_pts.extend(red.iter().map(|f| f.pts_100ns));

    // 2. Wait ~500 ms (keep draining so the channel does not stall), then go blue.
    let mut pre = Vec::new();
    pump(&rx, Duration::from_millis(500), &mut pre);
    timeline_pts.extend(pre.iter().map(|f| f.pts_100ns));
    win.set_color_and_repaint(BLUE);

    let mut blue = Vec::new();
    pump(&rx, Duration::from_secs(1), &mut blue);
    assert!(
        !blue.is_empty(),
        "blue phase: no frames received after color switch"
    );
    for (i, f) in blue.iter().enumerate() {
        assert_frame_shape(f, &format!("blue frame {i}"));
        assert!(!f.is_black, "blue frame {i} unexpectedly is_black=true");
    }
    let first_blue = blue.iter().position(|f| near(center_bgra(f), [255, 0, 0]));
    assert!(
        first_blue.is_some(),
        "blue phase: no frame with center ~[255,0,0] within 1 s (frames={}, first center {:?}, last center {:?})",
        blue.len(),
        center_bgra(&blue[0]),
        center_bgra(blue.last().unwrap())
    );
    let blue_last = blue.last().unwrap();
    let px = center_bgra(blue_last);
    assert!(
        near(px, [255, 0, 0]),
        "blue phase: final frame center BGRA {:?} not ~[255,0,0]",
        px
    );
    let (pre_w, pre_h) = (blue_last.width, blue_last.height);
    timeline_pts.extend(blue.iter().map(|f| f.pts_100ns));

    // 3. Minimize: black frames must appear within ~1.2 s.
    win.show(SW_MINIMIZE);
    let mut minimized = Vec::new();
    pump(&rx, Duration::from_millis(1200), &mut minimized);
    let black_count = minimized.iter().filter(|f| f.is_black).count();
    let live_count = minimized.len() - black_count;
    assert!(
        black_count > 0,
        "minimize: no black frames within 1.2 s (total {}, non-black {})",
        minimized.len(),
        live_count
    );
    for (i, f) in minimized.iter().filter(|f| f.is_black).enumerate() {
        assert_frame_shape(f, &format!("black frame {i}"));
        assert_eq!(
            (f.width, f.height),
            (pre_w, pre_h),
            "black frame {i} size {}x{} != pre-minimize {}x{}",
            f.width,
            f.height,
            pre_w,
            pre_h
        );
        let nonzero = f
            .bgra
            .chunks_exact(4)
            .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
            .count();
        assert_eq!(
            nonzero, 0,
            "black frame {i} has {nonzero} pixels with nonzero B/G/R"
        );
    }
    timeline_pts.extend(minimized.iter().map(|f| f.pts_100ns));

    // 4. Restore, go red again, and expect live frames within 2 s.
    win.show(SW_RESTORE);
    win.set_color_and_repaint(RED);
    let mut restored = Vec::new();
    pump(&rx, Duration::from_secs(3), &mut restored);
    let live: Vec<&Frame> = restored.iter().filter(|f| !f.is_black).collect();
    assert!(
        !live.is_empty(),
        "restore: no non-black frame within 2 s (total {}, black {})",
        restored.len(),
        restored.iter().filter(|f| f.is_black).count()
    );
    for (i, f) in live.iter().enumerate() {
        assert_frame_shape(f, &format!("restored live frame {i}"));
    }
    // The repaint after a restore can lag under load, so require that red shows up at some point
    // (as the blue phase does) instead of demanding it in the very last frame.
    assert!(
        live.iter().any(|f| near(center_bgra(f), [0, 0, 255])),
        "restore: no live frame with center ~[0,0,255] within 3 s (live frames {}, last center {:?})",
        live.len(),
        center_bgra(live.last().unwrap())
    );
    eprintln!(
        "wgc_window_capture_flow: red={} blue={} minimized(black={}, live={}) restored(live={})",
        red.len(),
        blue.len(),
        black_count,
        live_count,
        live.len()
    );
    timeline_pts.extend(restored.iter().map(|f| f.pts_100ns));

    // Global: pts_100ns is monotonic non-decreasing across all phases.
    for w in timeline_pts.windows(2) {
        assert!(w[1] >= w[0], "pts went backwards: {} then {}", w[0], w[1]);
    }

    // 5. Stop capture, then close the window (TestWindow::drop).
    capturer.stop();
    drop(rx);
    drop(win);
}

/// `IsWindow` runs before any WGC call, so an invalid HWND is rejected as `InvalidWindow`
/// with the handle echoed back, on every machine (including ones without WGC).
#[test]
fn invalid_hwnd_is_rejected_without_panic() {
    for hwnd in [0isize, 0x12345] {
        match WindowCapturer::start(CaptureConfig { hwnd, fps: 30 }) {
            Err(CaptureError::InvalidWindow(got)) => {
                assert_eq!(got, hwnd, "InvalidWindow must carry the rejected handle")
            }
            Err(other) => {
                panic!("hwnd {hwnd:#x}: expected InvalidWindow({hwnd:#x}), got {other:?}")
            }
            Ok((capturer, _rx)) => {
                capturer.stop();
                panic!("hwnd {hwnd:#x}: start unexpectedly succeeded");
            }
        }
    }
}

/// `start` validates fps before it touches the window handle. The handle here is a real test
/// window, so the only reason for the error is the fps value (the ordering is intentional).
#[test]
fn zero_fps_is_rejected() {
    let win = TestWindow::create();
    let r = WindowCapturer::start(CaptureConfig {
        hwnd: win.hwnd,
        fps: 0,
    });
    assert_eq!(
        r.err(),
        Some(CaptureError::InvalidFps(0)),
        "fps 0 must return InvalidFps(0)"
    );
}

/// Regression: attach/detach churn must never crash (long-lived daemon starts and stops
/// capture on every game launch).
#[test]
fn rapid_start_stop_stress() {
    let win = TestWindow::create();
    thread::sleep(Duration::from_millis(300));
    for i in 0..25u64 {
        let cfg = CaptureConfig {
            hwnd: win.hwnd,
            fps: 30,
        };
        let Some((capturer, rx)) = start_or_skip("rapid_start_stop_stress", cfg) else {
            return;
        };
        thread::sleep(Duration::from_millis((i * 37) % 350));
        let _ = rx.try_recv();
        capturer.stop();
    }
}

/// Regression: receiver dropped immediately after start, then stop (the encoder-startup
/// failure path in `Pipeline::start_capture`).
#[test]
fn drop_receiver_then_stop_stress() {
    let win = TestWindow::create();
    thread::sleep(Duration::from_millis(300));
    for _ in 0..40u64 {
        let cfg = CaptureConfig {
            hwnd: win.hwnd,
            fps: 30,
        };
        let Some((capturer, rx)) = start_or_skip("drop_receiver_then_stop_stress", cfg) else {
            return;
        };
        drop(rx);
        capturer.stop();
    }
}

/// Regression: fresh window, capture started immediately, stopped immediately, window
/// destroyed immediately (matches a game that exits right after launch).
#[test]
fn fresh_window_churn_stress() {
    for _ in 0..25u64 {
        let win = TestWindow::create();
        let cfg = CaptureConfig {
            hwnd: win.hwnd,
            fps: 30,
        };
        let Some((capturer, rx)) = start_or_skip("fresh_window_churn_stress", cfg) else {
            return;
        };
        drop(rx);
        capturer.stop();
        drop(win);
    }
}
