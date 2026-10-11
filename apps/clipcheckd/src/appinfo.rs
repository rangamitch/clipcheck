//! Executable metadata for the Settings pickers: 32x32 PNG icons (base64) and
//! friendly names from the version resource.
//!
//! The pure helpers (base64, downscaling, PNG encoding) carry no OS calls and
//! are unit-tested directly. The Win32 calls are confined to the `win` module
//! below; each `unsafe` block states the invariant it relies on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use clipcheck_util::sync::lock_unpoisoned as lock;

/// Edge length of the icons handed to the UI, in pixels.
pub const ICON_SIZE: u32 = 32;

static ICON_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();

/// Returns the exe's icon as a `ICON_SIZE` square PNG, base64-encoded without a
/// `data:` prefix. `None` when the icon cannot be read. Results (including
/// failures) are cached per path for the life of the process.
pub fn icon_png_b64(exe_path: &Path) -> Option<String> {
    let cache = ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = lock(cache).get(exe_path).cloned();
    if let Some(hit) = cached {
        return hit;
    }
    let value = win::extract_icon_rgba(exe_path)
        .and_then(|(w, h, px)| encode_icon_png(w, h, &px))
        .map(|png| base64_encode(&png));
    lock(cache).insert(exe_path.to_path_buf(), value.clone());
    value
}

/// Returns the exe's icon as raw PNG bytes (`ICON_SIZE` square), for writing to disk.
/// Unlike [`icon_png_b64`] this does not cache: the icon file on disk is the cache.
/// `None` when the icon cannot be read.
pub fn icon_png(exe_path: &Path) -> Option<Vec<u8>> {
    win::extract_icon_rgba(exe_path).and_then(|(w, h, px)| encode_icon_png(w, h, &px))
}

/// The file description from the exe's version resource, trimmed. `None` when
/// the file has no version info or the description is empty.
pub fn file_description(exe_path: &Path) -> Option<String> {
    win::file_description(exe_path)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// One display adapter as the DXGI enumeration reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// Adapter description.
    pub name: String,
    /// PCI vendor id.
    pub vendor_id: u32,
    /// DXGI enumeration index.
    pub index: u32,
    /// Dedicated video memory in MiB.
    pub dedicated_mb: u64,
}

/// Hardware display adapters. Software adapters (the Microsoft Basic Render
/// Driver) are skipped. Empty when DXGI is unavailable.
pub fn enumerate_gpus() -> Vec<GpuInfo> {
    win::enumerate_gpus()
}

/// Identifies the display hardware and its user-mode driver: one
/// `name#vendor_id#driver` entry per hardware adapter, joined by `|` in DXGI
/// enumeration order. The driver is `a.b.c.d`, or `?` when the query fails.
/// Software adapters are skipped. Empty when DXGI is unavailable.
pub fn gpu_fingerprint() -> String {
    win::gpu_fingerprint()
}

/// Formats a packed 64-bit user-mode driver version as four dotted 16-bit fields.
pub fn format_driver_version(version: i64) -> String {
    let v = version as u64;
    format!(
        "{}.{}.{}.{}",
        (v >> 48) & 0xFFFF,
        (v >> 32) & 0xFFFF,
        (v >> 16) & 0xFFFF,
        v & 0xFFFF
    )
}

/// Builds the fingerprint string from `(name, vendor_id, driver)` triples.
/// A `None` driver renders as `?`.
pub fn format_gpu_fingerprint(adapters: &[(String, u32, Option<i64>)]) -> String {
    adapters
        .iter()
        .map(|(name, vendor_id, driver)| {
            let driver = driver.map_or_else(|| "?".to_owned(), format_driver_version);
            format!("{name}#{vendor_id}#{driver}")
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Encodes `ICON_SIZE` x `ICON_SIZE` (or any size) RGBA pixels as a PNG, scaled
/// to `ICON_SIZE` first. Returns `None` if the buffer length does not match.
pub fn encode_icon_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    let scaled = resize_rgba(rgba, width, height, ICON_SIZE);
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, ICON_SIZE, ICON_SIZE);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&scaled).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}

/// Downscales (box filter, alpha-weighted) or upscales (nearest) RGBA pixels
/// to `dst` x `dst`. `src` must hold `w * h * 4` bytes.
pub fn resize_rgba(src: &[u8], w: u32, h: u32, dst: u32) -> Vec<u8> {
    let (w, h, dst) = (w as usize, h as usize, dst as usize);
    let mut out = vec![0u8; dst * dst * 4];
    for dy in 0..dst {
        let (y0, y1) = span(dy, dst, h);
        for dx in 0..dst {
            let (x0, x1) = span(dx, dst, w);
            let mut acc = [0u64; 4];
            let mut count = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * w + sx) * 4;
                    let a = u64::from(src[i + 3]);
                    acc[0] += u64::from(src[i]) * a;
                    acc[1] += u64::from(src[i + 1]) * a;
                    acc[2] += u64::from(src[i + 2]) * a;
                    acc[3] += a;
                    count += 1;
                }
            }
            let o = (dy * dst + dx) * 4;
            if count == 0 || acc[3] == 0 {
                continue;
            }
            for c in 0..3 {
                out[o + c] = (acc[c] / acc[3]).min(255) as u8;
            }
            out[o + 3] = (acc[3] / count).min(255) as u8;
        }
    }
    out
}

/// Source range `[start, end)` covered by destination cell `d` of `dst` cells over `src` pixels.
fn span(d: usize, dst: usize, src: usize) -> (usize, usize) {
    if src == 0 {
        return (0, 0);
    }
    let start = d * src / dst;
    let end = ((d + 1) * src / dst).max(start + 1).min(src);
    (start.min(src - 1), end)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648) with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    };
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, FILE_FLAGS_AND_ATTRIBUTES,
    };
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    use super::GpuInfo;

    /// Nul-terminated UTF-16 form of a path.
    fn to_wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Releases a GDI bitmap or device context when dropped.
    enum Gdi {
        Bitmap(HBITMAP),
        Dc(HDC),
    }

    impl Drop for Gdi {
        fn drop(&mut self) {
            // SAFETY: each handle was returned by a creating call and is released exactly once,
            // here. The handles are not used after this guard is dropped.
            unsafe {
                match self {
                    Gdi::Bitmap(h) => {
                        let _ = DeleteObject(HGDIOBJ(h.0));
                    }
                    Gdi::Dc(dc) => {
                        let _ = DeleteDC(*dc);
                    }
                }
            }
        }
    }

    /// Reads a 32bpp top-down BGRA copy of `bitmap` (`w` x `h`) into a new buffer.
    fn bitmap_bgra(hdc: HDC, bitmap: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; w as usize * h as usize * 4];
        // SAFETY: `px` holds exactly w*h*4 bytes, the size requested by the 32bpp header.
        // `hdc` and `bitmap` are live handles owned by the caller's guards.
        let lines = unsafe {
            GetDIBits(
                hdc,
                bitmap,
                0,
                h,
                Some(px.as_mut_ptr().cast::<c_void>()),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        (lines != 0).then_some(px)
    }

    /// Reads a 1bpp AND mask as per-pixel alpha (set bit = transparent).
    fn mask_alpha(hdc: HDC, bitmap: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
        let stride = (w as usize).div_ceil(32) * 4;
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 1,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = vec![0u8; stride * h as usize];
        // SAFETY: `bits` holds stride*h bytes, the 1bpp row layout described by `bmi`.
        // `hdc` and `bitmap` are live handles owned by the caller's guards.
        let lines = unsafe {
            GetDIBits(
                hdc,
                bitmap,
                0,
                h,
                Some(bits.as_mut_ptr().cast::<c_void>()),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            return None;
        }
        let mut alpha = vec![0u8; w as usize * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let byte = bits[y * stride + x / 8];
                let bit = (byte >> (7 - (x % 8))) & 1;
                alpha[y * w as usize + x] = if bit == 1 { 0 } else { 255 };
            }
        }
        Some(alpha)
    }

    /// Extracts the exe's large shell icon as RGBA pixels. Returns `(w, h, rgba)`.
    pub fn extract_icon_rgba(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
        let wide = to_wide(path);
        let mut info = SHFILEINFOW::default();
        // SAFETY: `wide` is nul-terminated and outlives the call. `info` is a valid,
        // writable SHFILEINFOW of the size passed.
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let icon = info.hIcon;
        let result = icon_pixels(icon);
        // SAFETY: `icon` came from SHGetFileInfoW and is destroyed exactly once.
        unsafe {
            let _ = DestroyIcon(icon);
        }
        result
    }

    fn icon_pixels(
        icon: windows::Win32::UI::WindowsAndMessaging::HICON,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let mut ii = ICONINFO::default();
        // SAFETY: `icon` is a valid icon handle; `ii` is a writable ICONINFO.
        unsafe { GetIconInfo(icon, &mut ii) }.ok()?;
        let _color_guard = (!ii.hbmColor.is_invalid()).then(|| Gdi::Bitmap(ii.hbmColor))?;
        let _mask_guard = (!ii.hbmMask.is_invalid()).then(|| Gdi::Bitmap(ii.hbmMask));

        let mut bm = BITMAP::default();
        // SAFETY: `bm` is a writable BITMAP of the size passed to GetObjectW.
        let got = unsafe {
            GetObjectW(
                HGDIOBJ(ii.hbmColor.0),
                size_of::<BITMAP>() as i32,
                Some((&mut bm as *mut BITMAP).cast::<c_void>()),
            )
        };
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);

        // SAFETY: a memory DC compatible with the screen (None) is created and released by `dc`.
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return None;
        }
        let _dc_guard = Gdi::Dc(dc);

        let mut rgba = bitmap_bgra(dc, ii.hbmColor, w, h)?;
        let has_alpha = rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        let mask = if has_alpha {
            None
        } else {
            mask_alpha(dc, ii.hbmMask, w, h)
        };
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            px.swap(0, 2); // BGRA -> RGBA
            if !has_alpha {
                px[3] = mask.as_ref().map_or(255, |m| m[i]);
            }
        }
        Some((w, h, rgba))
    }

    /// Hardware adapters through DXGI (`CreateDXGIFactory1` + `EnumAdapters1`).
    pub fn enumerate_gpus() -> Vec<GpuInfo> {
        let mut out = Vec::new();
        // SAFETY: CreateDXGIFactory1 takes no pointers; the returned factory is a COM
        // object owned by the `factory` binding and released when it drops.
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
            return out;
        };
        for index in 0u32.. {
            // SAFETY: `factory` is a live IDXGIFactory1 and `index` is a plain integer.
            // A failure (DXGI_ERROR_NOT_FOUND) ends the enumeration.
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            // SAFETY: `adapter` is a live IDXGIAdapter1; GetDesc1 writes only into the
            // returned struct by value.
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                continue;
            };
            let description = String::from_utf16_lossy(
                &desc.Description[..desc
                    .Description
                    .iter()
                    .position(|&u| u == 0)
                    .unwrap_or(desc.Description.len())],
            );
            let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
                || description.contains("Microsoft Basic Render Driver");
            if software {
                continue;
            }
            out.push(GpuInfo {
                name: description,
                vendor_id: desc.VendorId,
                index,
                dedicated_mb: (desc.DedicatedVideoMemory as u64) / (1024 * 1024),
            });
        }
        out
    }

    /// Fingerprint of the hardware adapters: name, vendor id and the UMD driver
    /// version (via `CheckInterfaceSupport(IDXGIDevice)`), built by the pure helpers.
    pub fn gpu_fingerprint() -> String {
        let mut adapters: Vec<(String, u32, Option<i64>)> = Vec::new();
        // SAFETY: CreateDXGIFactory1 takes no pointers; the factory is released on drop.
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
            return super::format_gpu_fingerprint(&adapters);
        };
        for index in 0u32.. {
            // SAFETY: `factory` is a live IDXGIFactory1; a failure ends the enumeration.
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            // SAFETY: `adapter` is a live IDXGIAdapter1; GetDesc1 writes only the returned struct.
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                continue;
            };
            let description = String::from_utf16_lossy(
                &desc.Description[..desc
                    .Description
                    .iter()
                    .position(|&u| u == 0)
                    .unwrap_or(desc.Description.len())],
            );
            let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
                || description.contains("Microsoft Basic Render Driver");
            if software {
                continue;
            }
            // SAFETY: `adapter` is live; the pointer is to the IID constant, which lives
            // for the whole program. The call only writes the i64 result it returns.
            let driver =
                unsafe { adapter.CheckInterfaceSupport(&<IDXGIDevice as Interface>::IID) }.ok();
            adapters.push((description, desc.VendorId, driver));
        }
        super::format_gpu_fingerprint(&adapters)
    }

    /// The file description string from the exe's version resource.
    pub fn file_description(path: &Path) -> Option<String> {
        let wide = to_wide(path);
        let mut handle = 0u32;
        // SAFETY: `wide` is nul-terminated; `handle` is a writable u32.
        let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), Some(&mut handle)) };
        if size == 0 {
            return None;
        }
        // u64 backing keeps the buffer 8-byte aligned for the version structures.
        let mut block = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: `block` provides `size` writable bytes, as the size query requested.
        unsafe {
            GetFileVersionInfoW(
                PCWSTR(wide.as_ptr()),
                0,
                size,
                block.as_mut_ptr().cast::<c_void>(),
            )
        }
        .ok()?;
        let base = block.as_ptr().cast::<c_void>();

        let translation = query(base, "\\VarFileInfo\\Translation")?;
        let (lang, codepage) = {
            // SAFETY: a Translation query returns at least one (lang, codepage) u16 pair
            // inside `block`; the length check guards the read.
            let (ptr, len) = translation;
            if len < 4 {
                return None;
            }
            let pair = ptr as *const u16;
            unsafe { (*pair, *pair.add(1)) }
        };
        let key = format!("\\StringFileInfo\\{lang:04x}{codepage:04x}\\FileDescription");
        let (ptr, len) = query(base, &key)?;
        // SAFETY: a string query returns `len` UTF-16 units (including the terminator)
        // inside `block`.
        let units = unsafe { std::slice::from_raw_parts(ptr as *const u16, len as usize) };
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }

    /// Runs VerQueryValueW and returns the pointer (into `base`) and length it reports.
    fn query(base: *const c_void, sub_block: &str) -> Option<(*const c_void, u32)> {
        let wide: Vec<u16> = sub_block.encode_utf16().chain(Some(0)).collect();
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `base` points at a block filled by GetFileVersionInfoW and still alive
        // (owned by the caller). `wide` is nul-terminated. VerQueryValueW writes a pointer
        // into that block and its length, which we only read from while the block lives.
        let ok = unsafe { VerQueryValueW(base, PCWSTR(wide.as_ptr()), &mut ptr, &mut len) };
        if !ok.as_bool() || ptr.is_null() || len == 0 {
            return None;
        }
        Some((ptr, len))
    }
}

#[cfg(not(windows))]
mod win {
    use std::path::Path;

    pub fn extract_icon_rgba(_path: &Path) -> Option<(u32, u32, Vec<u8>)> {
        None
    }

    pub fn file_description(_path: &Path) -> Option<String> {
        None
    }

    pub fn enumerate_gpus() -> Vec<super::GpuInfo> {
        Vec::new()
    }

    pub fn gpu_fingerprint() -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn resize_keeps_uniform_color() {
        let src = [10u8, 20, 30, 255].repeat(64 * 64);
        let out = resize_rgba(&src, 64, 64, 32);
        assert_eq!(out.len(), 32 * 32 * 4);
        assert!(out
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [10, 20, 30, 255]));
    }

    #[test]
    fn transparent_pixels_stay_transparent() {
        let src = [255u8, 0, 0, 0].repeat(4 * 4);
        let out = resize_rgba(&src, 4, 4, 32);
        assert!(out.as_chunks::<4>().0.iter().all(|p| p[3] == 0));
    }

    #[test]
    fn png_has_signature_and_32x32_ihdr() {
        let src = [0u8, 0, 255, 255].repeat(16 * 16);
        let png = encode_icon_png(16, 16, &src).expect("png");
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 32);
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 32);
    }

    #[test]
    fn mismatched_buffer_is_rejected() {
        assert!(encode_icon_png(4, 4, &[0u8; 10]).is_none());
    }

    #[test]
    fn driver_version_unpacks_four_16_bit_fields() {
        assert_eq!(format_driver_version(0x000F_0001_0002_0003), "15.1.2.3");
        assert_eq!(format_driver_version(0), "0.0.0.0");
        assert_eq!(
            format_driver_version(0x7FFF_FFFF_FFFF_FFFF),
            "32767.65535.65535.65535"
        );
    }

    #[test]
    fn missing_driver_renders_question_mark() {
        let adapters = [("Card".to_owned(), 4318, None)];
        assert_eq!(format_gpu_fingerprint(&adapters), "Card#4318#?");
    }

    #[test]
    fn adapters_join_with_pipe_in_order() {
        let adapters = [
            ("A".to_owned(), 1, Some(0x0001_0000_0000_0000)),
            ("B".to_owned(), 2, None),
        ];
        assert_eq!(format_gpu_fingerprint(&adapters), "A#1#1.0.0.0|B#2#?");
    }

    #[test]
    fn no_adapters_gives_empty_fingerprint() {
        assert_eq!(format_gpu_fingerprint(&[]), "");
    }
}
