//! Media Foundation source reader: probe and video decode (worker R3).

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows::core::{Interface, GUID, PCWSTR, PROPVARIANT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoIncrementMTAUsage, CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED,
};

use crate::{DecError, Frame, MediaProbe, OutFormat, ProbeAudio};

/// Maps a windows error to `DecError::Windows` under the given API step name.
pub(crate) fn win_err(context: &'static str, e: windows::core::Error) -> DecError {
    DecError::Windows {
        context,
        message: e.to_string(),
    }
}

/// Once-per-process Media Foundation startup (D58).
fn startup() -> Result<(), DecError> {
    static INIT: OnceLock<Result<(), DecError>> = OnceLock::new();
    INIT.get_or_init(|| unsafe {
        // SAFETY: CoIncrementMTAUsage has no preconditions; the cookie is intentionally dropped
        // (it has no Drop), so the MTA stays alive for the process lifetime.
        let _ = CoIncrementMTAUsage();
        // SAFETY: MFStartup takes a version constant and flags; it runs once per process.
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| win_err("MFStartup", e))
    })
    .clone()
}

/// Per-thread COM initialisation (MTA). Uninitialises on drop only if this call succeeded.
pub(crate) struct ComThread(bool);

impl ComThread {
    pub(crate) fn enter() -> Self {
        // SAFETY: CoInitializeEx with no reserved pointer is always sound to call; the result is
        // paired with CoUninitialize in Drop only when it succeeded (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self(hr.is_ok())
    }
}

impl Drop for ComThread {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Wide, NUL-terminated copy of a path for PCWSTR parameters.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn ensure_file(path: &Path) -> Result<(), DecError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(DecError::BadInput(format!(
            "no such file: {}",
            path.display()
        )))
    }
}

fn open_reader(path: &Path, attrs: Option<&IMFAttributes>) -> Result<IMFSourceReader, DecError> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and alive for the call; attrs is either None or a live store.
    unsafe { MFCreateSourceReaderFromURL(PCWSTR(w.as_ptr()), attrs) }
        .map_err(|e| win_err("MFCreateSourceReaderFromURL", e))
}

/// Native media type of stream `index`; `None` once the index is past the last stream.
fn native_type(reader: &IMFSourceReader, index: u32) -> Result<Option<IMFMediaType>, DecError> {
    // SAFETY: plain query on a live source reader.
    match unsafe { reader.GetNativeMediaType(index, 0) } {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => Ok(None),
        Err(e) => Err(win_err("GetNativeMediaType", e)),
    }
}

/// Index and subtype of the first video stream, if any.
fn first_video(reader: &IMFSourceReader) -> Result<Option<(u32, GUID)>, DecError> {
    let mut i = 0u32;
    while let Some(t) = native_type(reader, i)? {
        if guid_attr(&t, &MF_MT_MAJOR_TYPE) == Some(MFMediaType_Video) {
            let sub = guid_attr(&t, &MF_MT_SUBTYPE).unwrap_or_default();
            return Ok(Some((i, sub)));
        }
        i += 1;
    }
    Ok(None)
}

fn guid_attr(t: &IMFAttributes, key: &GUID) -> Option<GUID> {
    // SAFETY: key is a valid GUID reference for the call.
    unsafe { t.GetGUID(key) }.ok()
}

fn u64_attr(t: &IMFAttributes, key: &GUID) -> Option<u64> {
    // SAFETY: key is a valid GUID reference for the call.
    unsafe { t.GetUINT64(key) }.ok()
}

fn u32_attr(t: &IMFAttributes, key: &GUID) -> Option<u32> {
    // SAFETY: key is a valid GUID reference for the call.
    unsafe { t.GetUINT32(key) }.ok()
}

/// Splits a packed MF 64-bit (high, low) pair.
fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

fn video_codec_name(sub: &GUID) -> &'static str {
    if *sub == MFVideoFormat_H264 {
        "h264"
    } else if *sub == MFVideoFormat_HEVC {
        "hevc"
    } else if *sub == MFVideoFormat_AV1 {
        "av1"
    } else if *sub == MFVideoFormat_VP90 {
        "vp9"
    } else {
        "unknown"
    }
}

fn audio_codec_name(sub: &GUID) -> &'static str {
    if *sub == MFAudioFormat_AAC {
        "aac"
    } else if *sub == MFAudioFormat_Opus {
        "opus"
    } else if *sub == MFAudioFormat_MP3 {
        "mp3"
    } else if *sub == MFAudioFormat_PCM {
        "pcm"
    } else if *sub == MFAudioFormat_Float {
        "pcm_f32"
    } else {
        "unknown"
    }
}

/// Duration of the presentation in ms; 0 if unknown.
fn duration_ms(reader: &IMFSourceReader) -> i64 {
    // SAFETY: MEDIASOURCE selects the whole presentation; the GUID reference is valid.
    unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }
    .ok()
    .and_then(|pv| u64::try_from(&pv).ok())
    .map_or(0, |ticks| (ticks / 10_000) as i64)
}

/// Reads header facts without decoding.
pub fn probe(path: &Path) -> Result<MediaProbe, DecError> {
    ensure_file(path)?;
    startup()?;
    let _com = ComThread::enter();
    let reader = open_reader(path, None)?;
    let mut out = MediaProbe {
        duration_ms: duration_ms(&reader),
        width: 0,
        height: 0,
        fps: 0.0,
        video_codec: String::new(),
        audio: Vec::new(),
    };
    let mut have_video = false;
    let mut i = 0u32;
    while let Some(t) = native_type(&reader, i)? {
        i += 1;
        let major = guid_attr(&t, &MF_MT_MAJOR_TYPE);
        let sub = guid_attr(&t, &MF_MT_SUBTYPE).unwrap_or_default();
        if major == Some(MFMediaType_Video) && !have_video {
            have_video = true;
            if let Some((w, h)) = u64_attr(&t, &MF_MT_FRAME_SIZE).map(unpack) {
                out.width = w;
                out.height = h;
            }
            if let Some((num, den)) = u64_attr(&t, &MF_MT_FRAME_RATE).map(unpack) {
                if den != 0 {
                    out.fps = num as f32 / den as f32;
                }
            }
            out.video_codec = video_codec_name(&sub).to_string();
        } else if major == Some(MFMediaType_Audio) {
            out.audio.push(ProbeAudio {
                codec: audio_codec_name(&sub).to_string(),
                channels: u32_attr(&t, &MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0),
                sample_rate: u32_attr(&t, &MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0),
            });
        }
    }
    Ok(out)
}

/// Sequential decoder for the first video stream.
pub struct VideoDecoder {
    reader: IMFSourceReader,
    format: OutFormat,
    geo: Geo,
    // Declared last so COM is uninitialised after every interface above is released.
    _com: ComThread,
}

/// Layout of the current video type: the locked buffer and the visible rectangle inside it.
#[derive(Clone, Copy, Debug)]
struct Geo {
    buf_h: u32,
    /// Default stride of the buffer (may be negative for bottom-up images).
    stride: i32,
    /// Visible origin and size inside the buffer.
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// Visible rectangle (x, y, w, h) from MF_MT_MINIMUM_DISPLAY_APERTURE (an MFVideoArea blob).
/// Falls back to the whole buffer when the attribute is missing or out of bounds.
fn visible_rect(t: &IMFAttributes, buf_w: u32, buf_h: u32) -> (u32, u32, u32, u32) {
    let full = (0, 0, buf_w, buf_h);
    // SAFETY: the key is a valid GUID reference for the calls.
    let Ok(size) = (unsafe { t.GetBlobSize(&MF_MT_MINIMUM_DISPLAY_APERTURE) }) else {
        return full;
    };
    if size < 16 {
        return full;
    }
    let mut blob = vec![0u8; size as usize];
    // SAFETY: blob is exactly the size the store reported.
    if unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, None) }.is_err() {
        return full;
    }
    // MFVideoArea = OffsetX (MFOffset: u16 fract, i16 value), OffsetY (same), Area SIZE (i32 cx, cy).
    let i16_at = |o: usize| i64::from(i16::from_le_bytes([blob[o], blob[o + 1]]));
    let i32_at = |o: usize| {
        i64::from(i32::from_le_bytes([
            blob[o],
            blob[o + 1],
            blob[o + 2],
            blob[o + 3],
        ]))
    };
    let (x, y) = (i16_at(2).max(0), i16_at(6).max(0));
    let (cx, cy) = (i32_at(8), i32_at(12));
    if cx <= 0 || cy <= 0 || x + cx > i64::from(buf_w) || y + cy > i64::from(buf_h) {
        return full;
    }
    (x as u32, y as u32, cx as u32, cy as u32)
}

/// Geometry of the current video type.
fn geometry(t: &IMFAttributes, format: OutFormat) -> Result<Geo, DecError> {
    let (buf_w, buf_h) = u64_attr(t, &MF_MT_FRAME_SIZE)
        .map(unpack)
        .ok_or(DecError::Windows {
            context: "MF_MT_FRAME_SIZE",
            message: "attribute missing on the current video type".to_string(),
        })?;
    let default = match format {
        OutFormat::Bgra => (buf_w as i32).wrapping_mul(4),
        OutFormat::Nv12 => buf_w as i32,
    };
    let stride = u32_attr(t, &MF_MT_DEFAULT_STRIDE).map_or(default, |s| s as i32);
    let (x, y, w, h) = visible_rect(t, buf_w, buf_h);
    // Keep chroma-aligned origins for NV12.
    let (x, y) = match format {
        OutFormat::Bgra => (x, y),
        OutFormat::Nv12 => (x & !1, y & !1),
    };
    Ok(Geo {
        buf_h,
        stride,
        x,
        y,
        w,
        h,
    })
}

/// Maps a decoder-missing failure to `Unsupported`, anything else to `Windows`.
fn decoder_err(e: windows::core::Error, codec: &str) -> DecError {
    if e.code() == MF_E_TOPO_CODEC_NOT_FOUND || e.code() == MF_E_INVALIDMEDIATYPE {
        DecError::Unsupported(format!(
            "no decoder for {codec} video; HEVC needs the \"HEVC Video Extensions\" and AV1 needs the \"AV1 Video Extension\" from the Microsoft Store"
        ))
    } else {
        win_err("SetCurrentMediaType", e)
    }
}

impl VideoDecoder {
    /// Opens `path`, decoding to `format`.
    pub fn open(path: &Path, format: OutFormat) -> Result<Self, DecError> {
        ensure_file(path)?;
        startup()?;
        let com = ComThread::enter();

        let mut attrs: Option<IMFAttributes> = None;
        // SAFETY: attrs is a valid out-pointer for MFCreateAttributes.
        unsafe { MFCreateAttributes(&mut attrs, 1) }
            .map_err(|e| win_err("MFCreateAttributes", e))?;
        let attrs = attrs.ok_or(DecError::Windows {
            context: "MFCreateAttributes",
            message: "no attribute store returned".to_string(),
        })?;
        // SAFETY: plain attribute write on a store we own.
        unsafe { attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1) }
            .map_err(|e| win_err("SetUINT32", e))?;

        let reader = open_reader(path, Some(&attrs))?;
        let (video_index, sub) = first_video(&reader)?
            .ok_or_else(|| DecError::Unsupported("the file has no video stream".to_string()))?;
        let codec = video_codec_name(&sub);

        // SAFETY: stream indices and bool selections are plain values.
        unsafe { reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false) }
            .map_err(|e| win_err("SetStreamSelection", e))?;
        unsafe { reader.SetStreamSelection(video_index, true) }
            .map_err(|e| win_err("SetStreamSelection", e))?;

        let media = unsafe { MFCreateMediaType() }.map_err(|e| win_err("MFCreateMediaType", e))?;
        let subtype = match format {
            OutFormat::Bgra => MFVideoFormat_RGB32,
            OutFormat::Nv12 => MFVideoFormat_NV12,
        };
        // SAFETY: GUID references are valid for the calls.
        unsafe { media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video) }
            .map_err(|e| win_err("SetGUID", e))?;
        unsafe { media.SetGUID(&MF_MT_SUBTYPE, &subtype) }.map_err(|e| win_err("SetGUID", e))?;
        // SAFETY: the partial type is a live object; no reserved pointer.
        unsafe {
            reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &media)
        }
        .map_err(|e| decoder_err(e, codec))?;

        // SAFETY: stream index is a plain value.
        let current =
            unsafe { reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) }
                .map_err(|e| win_err("GetCurrentMediaType", e))?;
        let geo = geometry(&current, format)?;

        Ok(Self {
            reader,
            format,
            geo,
            _com: com,
        })
    }

    /// Seeks to the keyframe at or before `secs`.
    pub fn seek(&mut self, secs: f64) -> Result<(), DecError> {
        let ticks = (secs.max(0.0) * 1e7) as i64;
        let pos = PROPVARIANT::from(ticks);
        // SAFETY: GUID_NULL (zeroed) selects 100 ns units; pos is a live VT_I8 variant.
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &pos) }
            .map_err(|e| win_err("SetCurrentPosition", e))
    }

    /// Next decoded frame, or `None` at the end of the stream.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, DecError> {
        loop {
            let mut actual = 0u32;
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: all out-pointers reference live locals of the right types.
            unsafe {
                self.reader.ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    Some(&mut actual),
                    Some(&mut flags),
                    Some(&mut ts),
                    Some(&mut sample),
                )
            }
            .map_err(|e| win_err("ReadSample", e))?;

            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                return Ok(None);
            }
            if flags & (MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32) != 0 {
                // SAFETY: stream index is a plain value.
                let current = unsafe {
                    self.reader
                        .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                }
                .map_err(|e| win_err("GetCurrentMediaType", e))?;
                self.geo = geometry(&current, self.format)?;
            }
            let Some(sample) = sample else {
                // Stream tick or gap: no picture this time.
                continue;
            };
            let data = self.copy_frame(&sample)?;
            return Ok(Some(Frame {
                data,
                width: self.geo.w,
                height: self.geo.h,
                pts: ts,
            }));
        }
    }

    /// Copies one sample into a tight buffer (BGRA or NV12).
    fn copy_frame(&self, sample: &IMFSample) -> Result<Vec<u8>, DecError> {
        // SAFETY: plain call on a live sample.
        let buf = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|e| win_err("ConvertToContiguousBuffer", e))?;
        let (w, h) = (self.geo.w as usize, self.geo.h as usize);
        let out_len = match self.format {
            OutFormat::Bgra => w * h * 4,
            OutFormat::Nv12 => w * h + w * h.div_ceil(2),
        };
        let mut data = vec![0u8; out_len];

        if let Ok(b2d) = buf.cast::<IMF2DBuffer>() {
            let mut scan0: *mut u8 = null_mut();
            let mut pitch = 0i32;
            // SAFETY: out-pointers are valid locals; the buffer stays locked until Unlock2D below.
            unsafe { b2d.Lock2D(&mut scan0, &mut pitch) }.map_err(|e| win_err("Lock2D", e))?;
            let r = self.copy_pixels(scan0, pitch as isize, &mut data);
            // SAFETY: balances the successful Lock2D above.
            let _ = unsafe { b2d.Unlock2D() };
            r?;
        } else {
            let mut ptr: *mut u8 = null_mut();
            // SAFETY: out-pointer is a valid local; the buffer stays locked until Unlock below.
            unsafe { buf.Lock(&mut ptr, None, None) }.map_err(|e| win_err("Lock", e))?;
            let r = self.copy_pixels(ptr, self.geo.stride as isize, &mut data);
            // SAFETY: balances the successful Lock above.
            let _ = unsafe { buf.Unlock() };
            r?;
        }

        if self.format == OutFormat::Bgra {
            // RGB32 alpha is undefined; force opaque.
            for alpha in (3..data.len()).step_by(4) {
                data[alpha] = 255;
            }
        }
        Ok(data)
    }

    /// Copies rows from a locked buffer into `out`. `pitch` may be negative (bottom-up BGRA).
    fn copy_pixels(&self, ptr: *const u8, pitch: isize, out: &mut [u8]) -> Result<(), DecError> {
        if ptr.is_null() {
            return Err(DecError::Windows {
                context: "Lock",
                message: "null scanline pointer".to_string(),
            });
        }
        let g = self.geo;
        let (x, y, w, h) = (g.x as usize, g.y as usize, g.w as usize, g.h as usize);
        match self.format {
            OutFormat::Bgra => {
                let row = w * 4;
                for r in 0..h {
                    // SAFETY: visible rows y..y+h and columns x..x+w lie inside the locked buffer
                    // (visible_rect checks bounds); dst is a row-sized slice of `out`.
                    unsafe {
                        let src = ptr.offset((r + y) as isize * pitch + (x * 4) as isize);
                        std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(r * row), row);
                    }
                }
            }
            OutFormat::Nv12 => {
                if pitch < 0 {
                    return Err(DecError::Windows {
                        context: "Lock2D",
                        message: "bottom-up NV12 buffers are not supported".to_string(),
                    });
                }
                for r in 0..h {
                    // SAFETY: visible luma rows lie inside the locked buffer; dst is in bounds.
                    unsafe {
                        let src = ptr.offset((r + y) as isize * pitch + x as isize);
                        std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(r * w), w);
                    }
                }
                // SAFETY: the chroma plane starts right after the buffer's luma rows (pitch * buf_h bytes).
                let uv = unsafe { ptr.offset(pitch * g.buf_h as isize) };
                let base = w * h;
                for r in 0..h.div_ceil(2) {
                    // SAFETY: visible chroma rows lie inside the locked buffer; dst is in bounds.
                    unsafe {
                        let src = uv.offset((r + y / 2) as isize * pitch + x as isize);
                        std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(base + r * w), w);
                    }
                }
            }
        }
        Ok(())
    }

    /// Frame width.
    pub fn width(&self) -> u32 {
        self.geo.w
    }

    /// Frame height.
    pub fn height(&self) -> u32 {
        self.geo.h
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    use clipcheck_testkit::MediaSpec;

    /// Generates a 3 s 640x360 30 fps H.264 + AAC clip with the native testkit. `None` when the
    /// native encoders are unavailable.
    pub(crate) fn make_clip(dir: &Path) -> Option<PathBuf> {
        if !clipcheck_testkit::available() {
            return None;
        }
        let out = dir.join("clip.mp4");
        clipcheck_testkit::write_mp4(&out, &MediaSpec::h264_aac(3.0))
            .expect("generate test clip with clipcheck-testkit");
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_missing_file_is_bad_input() {
        let r = probe(Path::new("C:/definitely/not/here.mp4"));
        assert!(matches!(r, Err(DecError::BadInput(_))), "{r:?}");
    }

    #[test]
    fn probe_and_decode_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let Some(clip) = test_support::make_clip(dir.path()) else {
            eprintln!("SKIPPED: native test media unavailable");
            return;
        };

        let p = probe(&clip).unwrap();
        println!("probe: {p:?}");
        assert_eq!((p.width, p.height), (640, 360));
        assert!((p.fps - 30.0).abs() < 0.01, "fps {}", p.fps);
        assert_eq!(p.video_codec, "h264");
        assert!(
            (p.duration_ms - 3000).abs() <= 150,
            "duration {}",
            p.duration_ms
        );
        assert_eq!(p.audio.len(), 1);
        assert_eq!(p.audio[0].codec, "aac");
        assert_eq!(p.audio[0].sample_rate, 48000);

        let mut dec = VideoDecoder::open(&clip, OutFormat::Bgra).unwrap();
        assert_eq!((dec.width(), dec.height()), (640, 360));
        let mut last_pts = -1i64;
        for _ in 0..10 {
            let f = dec.next_frame().unwrap().expect("frame before EOF");
            assert_eq!((f.width, f.height), (640, 360));
            assert_eq!(f.data.len(), 640 * 360 * 4);
            assert!(f.pts > last_pts, "pts {} after {}", f.pts, last_pts);
            assert!((3..f.data.len()).step_by(4).all(|i| f.data[i] == 255));
            last_pts = f.pts;
        }

        let mut nv = VideoDecoder::open(&clip, OutFormat::Nv12).unwrap();
        let f = nv.next_frame().unwrap().expect("nv12 frame");
        assert_eq!(f.data.len(), 640 * 360 * 3 / 2);
    }
}
