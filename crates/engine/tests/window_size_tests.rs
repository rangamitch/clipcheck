//! Behaviour tests for `window_size::window_size` (public API only).
//!
//! `choose_canvas` is pub(crate) and is tested from inside the crate, not here.

mod common;

use clipcheck_engine::window_size::window_size;

#[test]
fn zero_handle_is_none() {
    assert_eq!(window_size(0), None);
}

#[test]
fn negative_or_garbage_handle_is_none() {
    // A handle of -1 is never a valid HWND. 12345 is a small handle value that is
    // not a live window in this process; GetWindowRect must fail for it.
    assert_eq!(window_size(-1), None);
    assert_eq!(window_size(12345), None);
}

#[cfg(windows)]
#[test]
fn real_window_size_matches_request() {
    use common::win_window::TestWindow;

    // The helper always creates an 800x600 WS_OVERLAPPEDWINDOW. The outer frame
    // (borders and title bar) is included in the size returned by GetWindowRect,
    // so the outer size can be larger than the requested size. Only lower bounds
    // are asserted, not equality.
    const REQUESTED_W: u32 = 800;
    const REQUESTED_H: u32 = 600;

    // TestWindow closes the window on Drop, so the window is closed even when an
    // assertion below panics.
    let win = TestWindow::create(0x0000_FF00);
    let size = window_size(win.hwnd);
    let (w, h) = size.expect("window_size returned None for a live 800x600 window");
    assert!(
        w >= REQUESTED_W,
        "width {w} is smaller than requested {REQUESTED_W}"
    );
    assert!(
        h >= REQUESTED_H,
        "height {h} is smaller than requested {REQUESTED_H}"
    );
}

// tiny_window_is_none: skipped. The helper (common/win_window.rs) only creates
// a fixed 800x600 window and exposes no size parameter, so a window below the
// 64 px minimum cannot be made without writing a new window-creation function.
