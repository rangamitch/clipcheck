//! Pixel size of a captured window, read before the encoder starts (D54).
//!
//! Used by `Recorder::attach` to choose the canvas aspect. Returns `None` when the size
//! cannot be read or is implausible; the caller then falls back to the 16:9 canvas.

/// Smallest width or height (in pixels) accepted as a plausible window size.
const MIN_DIMENSION: u32 = 64;

/// Size in pixels of the window `hwnd` as `(width, height)`, or `None` when unavailable.
///
/// Returns `None` when the handle is 0, `GetWindowRect` fails, or either dimension is
/// below 64 pixels. On non-Windows targets this always returns `None`.
#[cfg(windows)]
pub fn window_size(hwnd: isize) -> Option<(u32, u32)> {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    if hwnd == 0 {
        return None;
    }
    let handle = HWND(hwnd as usize as *mut c_void);
    let mut rect = RECT::default();
    // SAFETY: `handle` is only passed to GetWindowRect, which validates it and reports
    // failure through the Result; the out-parameter is a valid, initialized `RECT`.
    if unsafe { GetWindowRect(handle, &mut rect) }.is_err() {
        return None;
    }
    let width = (i64::from(rect.right) - i64::from(rect.left)).clamp(0, i64::from(u32::MAX));
    let height = (i64::from(rect.bottom) - i64::from(rect.top)).clamp(0, i64::from(u32::MAX));
    let (width, height) = (width as u32, height as u32);
    if width < MIN_DIMENSION || height < MIN_DIMENSION {
        return None;
    }
    Some((width, height))
}

/// Non-Windows stub: window geometry is not available, so always `None`.
#[cfg(not(windows))]
pub fn window_size(hwnd: isize) -> Option<(u32, u32)> {
    let _ = hwnd;
    None
}

#[cfg(test)]
mod tests {
    use super::window_size;

    #[test]
    fn null_handle_returns_none() {
        assert_eq!(window_size(0), None);
    }
}
