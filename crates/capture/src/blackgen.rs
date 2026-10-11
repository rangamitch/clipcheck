//! Black-frame generation and window-state probes.

use std::ffi::c_void;
use std::sync::Arc;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow, IsWindowVisible};

/// Builds a fully black BGRA frame (RGB = 0, alpha = 255), tightly packed.
pub(crate) fn black_bgra(width: u32, height: u32) -> Vec<u8> {
    let mut v = vec![0u8; width as usize * height as usize * 4];
    for px in v.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    v
}

/// Keeps one black frame for the most recent canvas size, so hidden ticks copy it
/// instead of rebuilding it.
#[derive(Debug, Default)]
pub(crate) struct BlackCache {
    size: (u32, u32),
    bytes: Arc<Vec<u8>>,
}

impl BlackCache {
    /// Black BGRA of `width` x `height`, shared. Rebuilt only when the size changes; otherwise
    /// this clones the `Arc` and no pixels are copied.
    pub(crate) fn frame(&mut self, width: u32, height: u32) -> Arc<Vec<u8>> {
        if self.size != (width, height) {
            self.bytes = Arc::new(black_bgra(width, height));
            self.size = (width, height);
        }
        Arc::clone(&self.bytes)
    }
}

/// True when the window exists, is visible, is not minimized, and is not cloaked.
///
/// The probes run in order and stop at the first one that already decides the answer, so a
/// hidden or destroyed window costs one or two calls instead of four. The result is the same
/// as checking all four.
pub(crate) fn is_window_showing(hwnd: isize) -> bool {
    let h = HWND(hwnd as *mut c_void);
    // SAFETY: IsWindow, IsWindowVisible and IsIconic accept any HWND value and
    // return FALSE for invalid handles; they do not dereference the handle.
    if !unsafe { IsWindow(h) }.as_bool() {
        return false;
    }
    if !unsafe { IsWindowVisible(h) }.as_bool() {
        return false;
    }
    if unsafe { IsIconic(h) }.as_bool() {
        return false;
    }
    !is_cloaked(h)
}

fn is_cloaked(h: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: pvattribute points to a live u32 and cbattribute is exactly its
    // size, which is what DWMWA_CLOAKED requires. Failure is treated as "not cloaked".
    let r = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    r.is_ok() && cloaked != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_frame_is_zero_rgb_opaque() {
        let v = black_bgra(3, 2);
        assert_eq!(v.len(), 3 * 2 * 4);
        for i in 0..v.len() / 4 {
            assert_eq!(&v[i * 4..i * 4 + 3], &[0, 0, 0]);
            assert_eq!(v[i * 4 + 3], 255);
        }
    }

    #[test]
    fn cache_reuses_frame_for_same_size() {
        let mut cache = BlackCache::default();
        let p1 = cache.frame(3, 2).as_ptr();
        let p2 = cache.frame(3, 2).as_ptr();
        assert_eq!(p1, p2, "same size must not rebuild");
    }

    #[test]
    fn cache_rebuilds_on_size_change() {
        let mut cache = BlackCache::default();
        assert_eq!(cache.frame(3, 2).len(), 24);
        let f = cache.frame(4, 4);
        assert_eq!(f.len(), 64);
        for i in 0..f.len() / 4 {
            assert_eq!(&f[i * 4..i * 4 + 4], &[0, 0, 0, 255]);
        }
    }
}
