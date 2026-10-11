//! Win32 test window and SendInput helpers for the daemon integration tests.
//!
//! Raw externs, as in `crates/capture/tests/wgc_window.rs`. Windows only.

use std::ffi::c_void;
use std::iter::once;
use std::mem::size_of;
use std::ptr::null;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const WINDOW_CLASS: &str = "clipcheck_test_window_class";
pub const WINDOW_TITLE: &str = "clipcheck test window";

// COLORREF is 0x00BBGGRR. This is the fill colour of the test window.
pub const FILL: u32 = 0x0040_80C0;

pub const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const SW_SHOWNORMAL: i32 = 1;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_PAINT: u32 = 0x000F;

pub const INPUT_KEYBOARD: u32 = 1;
pub const KEYEVENTF_KEYUP: u32 = 0x0002;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12; // Alt
pub const VK_F20: u16 = 0x83;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PaintStruct {
    hdc: isize,
    erase: i32,
    paint: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
pub struct WndClassW {
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

/// Matches the Win32 `KEYBDINPUT` layout.
#[repr(C)]
pub struct KeybdInput {
    w_vk: u16,
    w_scan: u16,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

/// Matches the Win32 `INPUT` layout on 64-bit targets (40 bytes). The padding covers the
/// larger `MOUSEINPUT` member of the union.
#[repr(C)]
pub struct KeyInput {
    ty: u32,
    ki: KeybdInput,
    _pad: [u8; 8],
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<KeyInput>() == 40);

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
    fn SendInput(count: u32, inputs: *const KeyInput, size: i32) -> u32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateSolidBrush(color: u32) -> isize;
    fn DeleteObject(obj: isize) -> i32;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

/// Paints the client area a solid colour. Posts quit on destroy so the thread can exit.
unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PaintStruct::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = Rect::default();
                GetClientRect(hwnd, &mut rc);
                let brush = CreateSolidBrush(FILL);
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

/// Runs on the window's own thread. Creates the window and pumps its messages.
///
/// # Safety
/// Calls Win32 functions. It must run on a thread that may own windows.
pub unsafe fn window_thread(tx: mpsc::Sender<isize>) {
    unsafe {
        let inst = GetModuleHandleW(null());
        let class = wide(WINDOW_CLASS);
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
        // Failure here only means the class already exists in this process.
        RegisterClassW(&wc);
        let title = wide(WINDOW_TITLE);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            640,
            480,
            0,
            0,
            inst,
            null(),
        );
        if hwnd != 0 {
            ShowWindow(hwnd, SW_SHOWNORMAL);
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
    }
}

/// A visible, un-minimised top-level window owned by a helper thread. Dropping it closes the
/// window and joins the thread.
pub struct TestWindow {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl TestWindow {
    /// Creates the window. Returns `Err` when no window can be made (for example no desktop).
    pub fn create() -> Result<TestWindow, String> {
        let (tx, rx) = mpsc::channel::<isize>();
        let thread = thread::spawn(move || unsafe { window_thread(tx) });
        let hwnd = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("window thread did not report an HWND: {e}"))?;
        if hwnd == 0 {
            let _ = thread.join();
            return Err("CreateWindowExW returned NULL (no interactive desktop?)".into());
        }
        Ok(TestWindow {
            hwnd,
            thread: Some(thread),
        })
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

fn key_event(vk: u16, up: bool) -> KeyInput {
    KeyInput {
        ty: INPUT_KEYBOARD,
        ki: KeybdInput {
            w_vk: vk,
            w_scan: 0,
            dw_flags: if up { KEYEVENTF_KEYUP } else { 0 },
            time: 0,
            dw_extra_info: 0,
        },
        _pad: [0; 8],
    }
}

/// Presses the keys in `vks` down in order, then releases them in reverse, with SendInput.
/// RegisterHotKey is global, so the focused window does not matter.
pub fn press_chord(vks: &[u16]) {
    let mut seq: Vec<KeyInput> = vks.iter().map(|&vk| key_event(vk, false)).collect();
    seq.extend(vks.iter().rev().map(|&vk| key_event(vk, true)));
    let sent = unsafe { SendInput(seq.len() as u32, seq.as_ptr(), size_of::<KeyInput>() as i32) };
    assert_eq!(
        sent as usize,
        seq.len(),
        "SendInput injected {sent} of {} key events",
        seq.len()
    );
}
