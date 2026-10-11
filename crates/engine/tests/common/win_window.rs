//! A solid-colour Win32 window for capture tests. Adapted from
//! `crates/capture/tests/wgc_window.rs`. The window owns its thread and pumps
//! messages there; dropping the handle closes the window and joins the thread.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::null;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const SW_SHOWNOACTIVATE: i32 = 4;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
const CLASS_NAME: &str = "clipcheck_engine_test_class";

/// Settle time after showing the window, so it has painted before capture.
const SETTLE: Duration = Duration::from_millis(300);

thread_local! {
    /// Fill colour of the window owned by the current thread. Only the window
    /// thread runs the window procedure, so this is race-free.
    static FILL: Cell<u32> = const { Cell::new(0) };
}

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
                let brush = CreateSolidBrush(FILL.with(Cell::get));
                FillRect(hdc, &rc, brush);
                DeleteObject(brush);
                EndPaint(hwnd, &ps);
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

/// Runs on the window's own thread; owns the HWND and pumps messages.
unsafe fn window_thread(colour: u32, tx: mpsc::Sender<isize>) {
    FILL.with(|c| c.set(colour));
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
        RegisterClassW(&wc); // failure: class already registered in this process
        let title = wide("clipcheck engine test");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            800,
            600,
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

/// A test window filled with one solid colour.
pub struct TestWindow {
    pub hwnd: isize,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestWindow {
    /// Creates a window filled with `colour`, given as COLORREF (0x00BBGGRR).
    /// Waits a fixed settle time so the window has painted before any capture
    /// starts. A failure here panics: the window is the test's only input.
    pub fn create(colour: u32) -> TestWindow {
        let (tx, rx) = mpsc::channel::<isize>();
        let thread = thread::spawn(move || unsafe { window_thread(colour, tx) });
        let hwnd = rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("test window thread did not report: {e}"));
        if hwnd == 0 {
            let _ = thread.join();
            panic!("CreateWindowExW failed for the test window");
        }
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        thread::sleep(SETTLE);
        TestWindow {
            hwnd,
            thread: Some(thread),
        }
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
