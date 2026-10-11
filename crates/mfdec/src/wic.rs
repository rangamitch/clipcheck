//! JPEG thumbnails: decode one frame, scale and encode with WIC (worker R3).

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{Interface, PCWSTR};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER};

use crate::mf::{win_err, ComThread, VideoDecoder};
use crate::{DecError, Frame, OutFormat};

/// GENERIC_WRITE access right (winnt.h).
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Writes a JPEG of the frame at or after `at_secs`, `width` pixels wide, to `out`.
pub fn thumbnail_jpeg(path: &Path, at_secs: f64, width: u32, out: &Path) -> Result<(), DecError> {
    if width == 0 {
        return Err(DecError::BadInput(
            "thumbnail width must be > 0".to_string(),
        ));
    }
    let _com = ComThread::enter();
    let at = if at_secs.is_finite() {
        at_secs.max(0.0)
    } else {
        0.0
    };

    let frame = decode_at(path, at)?;
    let h2 = scaled_height(frame.width, frame.height, width);

    let part = part_path(out);
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| DecError::Windows {
            context: "create_dir_all",
            message: e.to_string(),
        })?;
    }
    let result = encode_jpeg(&frame, width, h2, &part);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    if out.exists() {
        std::fs::remove_file(out).map_err(|e| DecError::Windows {
            context: "remove_file",
            message: e.to_string(),
        })?;
    }
    std::fs::rename(&part, out).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        DecError::Windows {
            context: "rename",
            message: e.to_string(),
        }
    })
}

/// Decodes forward from `at` and returns the first frame with pts >= `at`, or the last one before EOF.
fn decode_at(path: &Path, at: f64) -> Result<Frame, DecError> {
    let mut dec = VideoDecoder::open(path, OutFormat::Bgra)?;
    // Media Foundation rejects a seek past the end (MF_E_OUT_OF_RANGE), but a target past the
    // end must still give the last frame: seek to just inside the duration instead.
    let duration = crate::probe(path).map_or(0.0, |p| p.duration_ms as f64 / 1000.0);
    let seek_to = if duration > 0.0 {
        at.min((duration - 0.5).max(0.0))
    } else {
        at
    };
    if seek_to > 0.0 {
        dec.seek(seek_to)?;
    }
    let target = (at * 1e7) as i64;
    let mut last: Option<Frame> = None;
    while let Some(f) = dec.next_frame()? {
        let reached = f.pts >= target;
        last = Some(f);
        if reached {
            break;
        }
    }
    last.ok_or_else(|| DecError::BadInput("no frame decoded".to_string()))
}

/// Output height for `width`, aspect kept, rounded to the nearest even value (minimum 2).
fn scaled_height(w: u32, h: u32, width: u32) -> u32 {
    if w == 0 {
        return 2;
    }
    let raw = (f64::from(width) * f64::from(h) / f64::from(w)).round() as u32;
    (raw.div_ceil(2) * 2).max(2)
}

/// `<out>.part`, the temporary file written before the rename.
fn part_path(out: &Path) -> PathBuf {
    let mut s: OsString = out.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Scales the BGRA frame with WIC and writes it as JPEG to `part`.
fn encode_jpeg(frame: &Frame, width: u32, h2: u32, part: &Path) -> Result<(), DecError> {
    // SAFETY: CLSID constant and no outer unknown; the factory is released when dropped.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| win_err("CoCreateInstance", e))?;

    // SAFETY: frame.data holds width*height*4 BGRA bytes and stays alive for the calls below.
    let bitmap = unsafe {
        factory.CreateBitmapFromMemory(
            frame.width,
            frame.height,
            &GUID_WICPixelFormat32bppBGRA,
            frame.width * 4,
            &frame.data,
        )
    }
    .map_err(|e| win_err("CreateBitmapFromMemory", e))?;

    // SAFETY: plain factory call.
    let scaler =
        unsafe { factory.CreateBitmapScaler() }.map_err(|e| win_err("CreateBitmapScaler", e))?;
    // SAFETY: source bitmap is live; target size is non-zero.
    unsafe { scaler.Initialize(&bitmap, width, h2, WICBitmapInterpolationModeFant) }
        .map_err(|e| win_err("IWICBitmapScaler::Initialize", e))?;

    // JPEG has no alpha: convert BGRA to BGR.
    // SAFETY: plain factory call.
    let converter = unsafe { factory.CreateFormatConverter() }
        .map_err(|e| win_err("CreateFormatConverter", e))?;
    // SAFETY: source and pixel format GUIDs are valid; no palette.
    unsafe {
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat24bppBGR,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }
    .map_err(|e| win_err("IWICFormatConverter::Initialize", e))?;

    let wide: Vec<u16> = part
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and alive for the call.
    let stream: IWICStream =
        unsafe { factory.CreateStream() }.map_err(|e| win_err("CreateStream", e))?;
    // SAFETY: wide is a valid NUL-terminated path; GENERIC_WRITE is a plain access mask.
    unsafe { stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE) }
        .map_err(|e| win_err("InitializeFromFilename", e))?;

    // SAFETY: JPEG container GUID; no vendor GUID.
    let encoder = unsafe { factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()) }
        .map_err(|e| win_err("CreateEncoder", e))?;
    let stream_iface: IStream = stream.cast().map_err(|e| win_err("IWICStream cast", e))?;
    // SAFETY: stream is live and writable; NoCache keeps the encoder from buffering the file.
    unsafe { encoder.Initialize(&stream_iface, WICBitmapEncoderNoCache) }
        .map_err(|e| win_err("IWICBitmapEncoder::Initialize", e))?;

    let mut frame_enc: Option<IWICBitmapFrameEncode> = None;
    // SAFETY: out-pointer for the frame; encoder options are optional and passed as null.
    unsafe { encoder.CreateNewFrame(&mut frame_enc, std::ptr::null_mut()) }
        .map_err(|e| win_err("CreateNewFrame", e))?;
    let frame_enc = frame_enc.ok_or(DecError::Windows {
        context: "CreateNewFrame",
        message: "no frame returned".to_string(),
    })?;
    // SAFETY: no encoder options (default JPEG quality) are passed.
    unsafe {
        frame_enc.Initialize(None::<&windows::Win32::System::Com::StructuredStorage::IPropertyBag2>)
    }
    .map_err(|e| win_err("IWICBitmapFrameEncode::Initialize", e))?;
    // SAFETY: plain size on a live frame.
    unsafe { frame_enc.SetSize(width, h2) }.map_err(|e| win_err("SetSize", e))?;
    let mut fmt = GUID_WICPixelFormat24bppBGR;
    // SAFETY: fmt is a valid in/out GUID.
    unsafe { frame_enc.SetPixelFormat(&mut fmt) }.map_err(|e| win_err("SetPixelFormat", e))?;
    // SAFETY: converter is a live bitmap source; null rect means the whole image.
    unsafe { frame_enc.WriteSource(&converter, std::ptr::null()) }
        .map_err(|e| win_err("WriteSource", e))?;
    // SAFETY: commits the frame and then the encoder, in that order.
    unsafe { frame_enc.Commit() }.map_err(|e| win_err("Commit", e))?;
    // SAFETY: as above.
    unsafe { encoder.Commit() }.map_err(|e| win_err("Commit", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mf::test_support::make_clip;

    #[test]
    fn thumbnail_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let Some(clip) = make_clip(dir.path()) else {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        };
        let out = dir.path().join("sub").join("thumb.jpg");
        thumbnail_jpeg(&clip, 1.5, 320, &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG SOI");
        assert!(bytes.len() > 1024, "size {}", bytes.len());
        assert!(!part_path(&out).exists(), ".part removed after rename");
        println!("thumbnail bytes: {}", bytes.len());

        assert!(matches!(
            thumbnail_jpeg(&clip, 0.0, 0, &out),
            Err(DecError::BadInput(_))
        ));
    }
}
