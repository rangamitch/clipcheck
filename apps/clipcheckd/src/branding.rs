//! Application branding: the product name and access to the icon embedded in
//! this program (PNG bytes compiled in, so it works from any host exe, including
//! the Tauri app that embeds the daemon).

/// Icon sizes compiled in, ascending. The nearest one at or above the request is used.
#[cfg(windows)]
static ICONS: [(u32, &[u8]); 7] = [
    (16, include_bytes!("../../../assets/icons/png/icon-16.png")),
    (24, include_bytes!("../../../assets/icons/png/icon-24.png")),
    (32, include_bytes!("../../../assets/icons/png/icon-32.png")),
    (48, include_bytes!("../../../assets/icons/png/icon-48.png")),
    (64, include_bytes!("../../../assets/icons/png/icon-64.png")),
    (
        128,
        include_bytes!("../../../assets/icons/png/icon-128.png"),
    ),
    (
        256,
        include_bytes!("../../../assets/icons/png/icon-256.png"),
    ),
];

/// Builds the application icon at the requested square size from the embedded PNGs.
///
/// Returns the raw `HICON` handle as `isize`, or `None` if the load fails. The caller
/// owns the handle and must release it with [`destroy_icon`].
#[cfg(windows)]
pub fn load_tray_icon(size_px: u32) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};

    let size = i32::try_from(size_px).ok()?;
    let (_, bytes) = ICONS
        .iter()
        .find(|(s, _)| *s >= size_px)
        .unwrap_or(&ICONS[ICONS.len() - 1]);
    // SAFETY: `bytes` is a valid PNG image for the whole call; version 0x00030000 is the
    // documented value for icon resources, and Windows scales to the requested size.
    let handle =
        unsafe { CreateIconFromResourceEx(bytes, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR) }
            .ok()?;
    if handle.0.is_null() {
        return None;
    }
    Some(handle.0 as isize)
}

/// Non-Windows builds have no embedded icon.
#[cfg(not(windows))]
pub fn load_tray_icon(_size_px: u32) -> Option<isize> {
    None
}

/// Releases an icon handle previously returned by [`load_tray_icon`].
/// A zero handle is ignored.
#[cfg(windows)]
pub fn destroy_icon(h: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

    if h == 0 {
        return;
    }
    // SAFETY: the caller passes a handle from `load_tray_icon`, which is an
    // icon handle owned by the caller and not yet destroyed.
    let _ = unsafe { DestroyIcon(HICON(h as *mut core::ffi::c_void)) };
}

/// Non-Windows builds hold no icon handles, so there is nothing to release.
#[cfg(not(windows))]
pub fn destroy_icon(_h: isize) {}

#[cfg(all(test, windows))]
mod tests {
    use super::{destroy_icon, load_tray_icon};

    #[test]
    fn loads_embedded_icon_at_common_tray_sizes() {
        for size in [16u32, 24, 32, 256] {
            let handle = load_tray_icon(size).expect("embedded icon resource 1 should load");
            assert_ne!(handle, 0, "size {size} returned a null handle");
            destroy_icon(handle);
        }
    }

    #[test]
    fn destroy_icon_ignores_zero_handle() {
        destroy_icon(0);
    }
}
