//! Media Foundation backed providers for the pipeline, plus the WIC still-image decoder.
//! Windows only. Owner: worker `render-engine`.

use std::collections::{HashMap, HashSet};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use clipcheck_mfdec::audio::AudioDecoder;
use clipcheck_mfdec::{DecError, Frame as DecFrame, OutFormat, VideoDecoder};
use clipcheck_timeline::model::{MediaKind, Project};
use clipcheck_timeline::time::{Tick, TICKS_PER_SECOND};
use windows::core::PCWSTR;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, IWICPalette,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use crate::error::RenderError;
use crate::frame::Frame;
use crate::pipeline::{AudioProvider, VideoProvider};

/// Open video decoders kept per provider (least recently used is closed first). Slots are keyed by
/// media id only, so the same file at two source offsets in one frame shares a decoder and seeks
/// between them; the cap is raised so that a few such layers do not evict each other.
const MAX_OPEN: usize = 8;
/// Slack, in 100 ns units (0.2 us), when matching a frame's presentation time to a request.
/// Media Foundation rounds frame timestamps of non-integer rates (23.976, 29.97, 59.94) to 100 ns,
/// so a frame requested at the floored tick time can carry a pts 1 unit later.
const TS_SLACK_100NS: i64 = 2;
/// A request this far (100 ns units, 2 s) past the last decoded frame seeks instead of decoding on.
const SEEK_AHEAD_100NS: i64 = 20_000_000;
/// Open audio decoders kept per provider. A project can play many streams of one file at once
/// (one decoder per stream); fewer slots than concurrent streams would reopen and reseek a
/// decoder on every block.
const MAX_OPEN_AUDIO: usize = 16;
/// Frames requested from an audio decoder per read (about 0.34 s at 48 kHz).
const AUDIO_CHUNK_FRAMES: usize = 16384;
/// Largest still image accepted, in pixels (about 160 MB of BGRA).
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

/// Initialises COM (multithreaded) for the current thread and uninitialises it on drop, but only
/// when that initialisation succeeded (S_OK or S_FALSE).
pub(crate) struct ComGuard(bool);

impl ComGuard {
    /// Enters COM for the current thread.
    pub(crate) fn enter() -> ComGuard {
        // SAFETY: CoInitializeEx with no reserved pointer is always sound to call; the matching
        // CoUninitialize runs in Drop only when this call succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

/// A source error from a WIC or COM call.
pub(crate) fn src_err(context: &str, e: windows::core::Error) -> RenderError {
    RenderError::Source(format!("{context}: {e}"))
}

/// An output error from a WIC or COM call.
pub(crate) fn out_err(context: &str, e: windows::core::Error) -> RenderError {
    RenderError::Output(format!("{context}: {e}"))
}

/// Where a media id lives and what it is.
#[derive(Debug, Clone)]
struct MediaSource {
    path: PathBuf,
    kind: MediaKind,
    /// Frame rate; sets the frame duration used to detect backward requests (30 when unknown).
    fps: f64,
}

/// Media bin of the project keyed by id.
fn sources_of(project: &Project) -> HashMap<String, MediaSource> {
    project
        .media
        .iter()
        .map(|m| {
            let fps = if m.fps.is_finite() && m.fps > 0.0 {
                f64::from(m.fps)
            } else {
                30.0
            };
            (
                m.id.clone(),
                MediaSource {
                    path: PathBuf::from(&m.path),
                    kind: m.kind,
                    fps,
                },
            )
        })
        .collect()
}

/// Timeline or source time as 100 ns units.
fn to_100ns(t: Tick) -> i64 {
    let v = i128::from(t.0) * 10_000_000 / i128::from(TICKS_PER_SECOND);
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Frames at `rate` Hz for a presentation time in 100 ns units (never negative).
fn ts_to_frames(ts: i64, rate: u32) -> i64 {
    let v = i128::from(ts.max(0)) * i128::from(rate) / 10_000_000;
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Converts a decoded BGRA frame to the pipeline's frame, checking the buffer size.
fn to_frame(f: DecFrame) -> Result<(i64, Frame), DecError> {
    let expected = (f.width as usize)
        .saturating_mul(f.height as usize)
        .saturating_mul(4);
    if f.data.len() != expected {
        return Err(DecError::BadInput(format!(
            "frame has {} bytes, expected {expected}",
            f.data.len()
        )));
    }
    let pts = f.pts;
    Ok((
        pts,
        Frame {
            width: f.width,
            height: f.height,
            bgra: f.data,
        },
    ))
}

/// Decodes the first frame of a still image file (PNG, JPEG, BMP, GIF, TIFF, ...) with WIC to
/// 32-bit BGRA.
pub(crate) fn decode_image(path: &Path) -> Result<Frame, RenderError> {
    let _com = ComGuard::enter();
    // SAFETY: CLSID constant, no outer unknown; the factory is released when dropped.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| src_err("CoCreateInstance", e))?;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and alive for the call; no vendor GUID.
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR(wide.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(|e| src_err("CreateDecoderFromFilename", e))?;
    // SAFETY: frame index 0 exists for any decodable image.
    let frame = unsafe { decoder.GetFrame(0) }.map_err(|e| src_err("GetFrame", e))?;
    let (mut w, mut h) = (0u32, 0u32);
    // SAFETY: out-pointers reference live locals.
    unsafe { frame.GetSize(&mut w, &mut h) }.map_err(|e| src_err("GetSize", e))?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS {
        return Err(RenderError::Source(format!(
            "image size {w}x{h} is not supported"
        )));
    }
    // SAFETY: plain factory call.
    let converter = unsafe { factory.CreateFormatConverter() }
        .map_err(|e| src_err("CreateFormatConverter", e))?;
    // SAFETY: source is a live bitmap frame; no palette; the destination GUID is a constant.
    unsafe {
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
    }
    .map_err(|e| src_err("IWICFormatConverter::Initialize", e))?;
    let stride = w * 4;
    let mut bgra = vec![0u8; stride as usize * h as usize];
    // SAFETY: the buffer holds stride * h bytes; a null rectangle copies the whole image.
    unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut bgra) }
        .map_err(|e| src_err("CopyPixels", e))?;
    Ok(Frame {
        width: w,
        height: h,
        bgra,
    })
}

/// One open video decoder with its look-ahead state.
struct VideoSlot {
    media_id: String,
    dec: VideoDecoder,
    /// Last frame at or before the most recent target, with its pts (100 ns).
    cur: Option<(i64, Frame)>,
    /// Decoded frame that is not due yet.
    pending: Option<DecFrame>,
    eof: bool,
    /// Set after the first request; the decoder is fresh until then.
    started: bool,
    /// Last use, for least-recently-used eviction.
    used: u64,
}

impl VideoSlot {
    /// The frame at `target` (100 ns): the last frame whose pts is at or before it, or the first
    /// frame when the target precedes the stream.
    fn frame_at(
        &mut self,
        target: i64,
        target_secs: f64,
        frame_100ns: i64,
    ) -> Result<Option<Frame>, DecError> {
        let cur_pts = self.cur.as_ref().map(|c| c.0);
        let last_pts = self.pending.as_ref().map(|p| p.pts).or(cur_pts);
        // Behind the last frame by more than one frame, or far ahead: decoders only move forward.
        let behind = cur_pts.is_some_and(|c| target < c - frame_100ns);
        let far_ahead = last_pts.is_some_and(|l| target - l > SEEK_AHEAD_100NS);
        let first_request_late = !self.started && target > 0;
        self.started = true;
        if behind || far_ahead || first_request_late {
            match self.dec.seek(target_secs) {
                Ok(()) => {
                    self.cur = None;
                    self.pending = None;
                    self.eof = false;
                }
                // Keep the last frame when a seek fails (for example past the end).
                Err(e) if self.cur.is_none() => return Err(e),
                Err(_) => {}
            }
        }

        loop {
            if self.pending.is_none() && !self.eof {
                match self.dec.next_frame()? {
                    Some(f) => self.pending = Some(f),
                    None => self.eof = true,
                }
            }
            match self.pending.take() {
                Some(p) if p.pts <= target + TS_SLACK_100NS => self.cur = Some(to_frame(p)?),
                Some(p) => {
                    self.pending = Some(p);
                    break;
                }
                None => break,
            }
        }
        if self.cur.is_none() {
            if let Some(p) = self.pending.take() {
                self.cur = Some(to_frame(p)?);
            }
        }
        Ok(self.cur.as_ref().map(|c| c.1.clone()))
    }
}

/// Decodes video frames with `clipcheck_mfdec::VideoDecoder`. Keeps at most `MAX_OPEN` decoders open (LRU),
/// seeks only when the requested time is not the next frame of an open decoder, and reuses the
/// previous frame when asked for the same or an earlier frame time within one frame duration.
/// Still images are decoded once with WIC and cached. A media id that cannot be opened is
/// remembered and answered with `Ok(None)` from then on.
pub struct MfVideoProvider {
    sources: HashMap<String, MediaSource>,
    slots: Vec<VideoSlot>,
    images: HashMap<String, Frame>,
    bad: HashSet<String>,
    clock: u64,
}

impl MfVideoProvider {
    /// Builds a provider that resolves media ids through the project's media bin.
    pub fn new(project: &Project) -> MfVideoProvider {
        MfVideoProvider {
            sources: sources_of(project),
            slots: Vec::new(),
            images: HashMap::new(),
            bad: HashSet::new(),
            clock: 0,
        }
    }

    /// Index of the open decoder for `media_id`, opening one (and evicting the least recently
    /// used decoder when full) if needed.
    fn slot_index(&mut self, media_id: &str) -> Option<usize> {
        self.clock = self.clock.saturating_add(1);
        let now = self.clock;
        if let Some(i) = self.slots.iter().position(|s| s.media_id == media_id) {
            if let Some(s) = self.slots.get_mut(i) {
                s.used = now;
            }
            return Some(i);
        }
        let path = self.sources.get(media_id).map(|s| s.path.clone())?;
        let dec = match VideoDecoder::open(&path, OutFormat::Bgra) {
            Ok(d) => d,
            Err(_) => {
                self.bad.insert(media_id.to_string());
                return None;
            }
        };
        if self.slots.len() >= MAX_OPEN {
            let oldest = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.used)
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                self.slots.remove(i);
            }
        }
        self.slots.push(VideoSlot {
            media_id: media_id.to_string(),
            dec,
            cur: None,
            pending: None,
            eof: false,
            started: false,
            used: now,
        });
        self.slots.len().checked_sub(1)
    }

    /// The decoded image for an image media item, loaded once.
    fn image(&mut self, media_id: &str, src: &MediaSource) -> Option<Frame> {
        if let Some(f) = self.images.get(media_id) {
            return Some(f.clone());
        }
        let loaded = if src.path.is_file() {
            decode_image(&src.path).ok()
        } else {
            None
        };
        match loaded {
            Some(f) => {
                self.images.insert(media_id.to_string(), f.clone());
                Some(f)
            }
            None => {
                self.bad.insert(media_id.to_string());
                None
            }
        }
    }
}

impl VideoProvider for MfVideoProvider {
    fn frame_at(&mut self, media_id: &str, src_time: Tick) -> Result<Option<Frame>, RenderError> {
        if self.bad.contains(media_id) {
            return Ok(None);
        }
        // Only the cheap fields are copied per call; the path is cloned when a decoder is opened.
        let Some((kind, fps)) = self.sources.get(media_id).map(|s| (s.kind, s.fps)) else {
            return Ok(None);
        };
        if kind == MediaKind::Image {
            let Some(src) = self.sources.get(media_id).cloned() else {
                return Ok(None);
            };
            return Ok(self.image(media_id, &src));
        }
        let Some(idx) = self.slot_index(media_id) else {
            return Ok(None);
        };
        let target = to_100ns(src_time);
        let frame_100ns = (10_000_000.0 / fps).round() as i64;
        let secs = src_time.as_secs_f64().max(0.0);
        let Some(slot) = self.slots.get_mut(idx) else {
            return Ok(None);
        };
        match slot.frame_at(target, secs, frame_100ns) {
            Ok(f) => Ok(f),
            Err(_) => {
                // Drop the decoder; the next request reopens it.
                self.slots.remove(idx);
                Ok(None)
            }
        }
    }
}

/// Stereo (left, right) for one source frame `fr`, whose length is the channel count.
///
/// Mono is duplicated. 6 channels (L R C LFE Ls Rs) and 8 channels (L R C LFE Lb Rb Ls Rs) are
/// folded to stereo: the centre (dialogue) and the surrounds are mixed in at -3 dB (0.707), the
/// LFE is dropped, and each side is divided by its coefficient sum so full-scale input stays within
/// [-1, 1]. Any other count keeps the first two channels.
fn downmix_frame(fr: &[f32]) -> (f32, f32) {
    const K: f32 = 0.707;
    let at = |i: usize| fr.get(i).copied().unwrap_or(0.0);
    match fr.len() {
        0 => (0.0, 0.0),
        1 => (at(0), at(0)),
        6 => {
            let norm = 1.0 / (1.0 + 2.0 * K);
            let l = at(0) + K * at(2) + K * at(4);
            let r = at(1) + K * at(2) + K * at(5);
            (l * norm, r * norm)
        }
        8 => {
            let norm = 1.0 / (1.0 + 3.0 * K);
            let l = at(0) + K * at(2) + K * at(4) + K * at(6);
            let r = at(1) + K * at(2) + K * at(5) + K * at(7);
            (l * norm, r * norm)
        }
        _ => (at(0), at(1)),
    }
}

/// One open audio decoder with its resampling buffer.
struct AudioSlot {
    media_id: String,
    stream: u32,
    dec: AudioDecoder,
    /// Native sample rate of the decoder, Hz.
    rate: u32,
    /// Native channel count (at least 1).
    channels: usize,
    /// Stereo samples at the native rate; `buf[0..2]` is source frame `base`.
    buf: Vec<f32>,
    /// Source frame index of the first buffered frame.
    base: i64,
    /// Source frame index the decoder returns next.
    read_pos: i64,
    /// True after a seek until the first chunk arrives, which sets `base` and `read_pos`.
    fresh: bool,
    eof: bool,
    /// Decoder output before channel mapping.
    scratch: Vec<f32>,
    /// Last use, for least-recently-used eviction.
    used: u64,
}

impl AudioSlot {
    /// Appends one decoded chunk as stereo. Returns `false` at the end of the stream.
    fn read_chunk(&mut self) -> Result<bool, DecError> {
        self.scratch.clear();
        let Some((_, ts)) = self.dec.read(AUDIO_CHUNK_FRAMES, &mut self.scratch)? else {
            return Ok(false);
        };
        let c = self.channels.max(1);
        let appended = self.scratch.len() / c;
        if appended == 0 {
            return Ok(false);
        }
        if self.fresh {
            let at = ts_to_frames(ts, self.rate);
            self.base = at;
            self.read_pos = at;
            self.fresh = false;
        }
        for fr in self.scratch.chunks_exact(c) {
            let (l, r) = downmix_frame(fr);
            self.buf.push(l);
            self.buf.push(r);
        }
        self.read_pos = self.read_pos.saturating_add(appended as i64);
        Ok(true)
    }

    /// Stereo sample at source frame `i`, silence outside the buffer.
    fn sample(&self, i: i64) -> (f32, f32) {
        let Ok(k) = usize::try_from(i - self.base) else {
            return (0.0, 0.0);
        };
        match (self.buf.get(k * 2), self.buf.get(k * 2 + 1)) {
            (Some(l), Some(r)) => (*l, *r),
            _ => (0.0, 0.0),
        }
    }

    /// Writes `frames` stereo output frames (linear resampling). Output frame `j` reads source
    /// position `pos0 + ratio * j`, in source frames.
    fn render(
        &mut self,
        pos0: f64,
        ratio: f64,
        frames: usize,
        out: &mut Vec<f32>,
    ) -> Result<(), DecError> {
        let first = pos0.floor() as i64;
        let last = (pos0 + ratio * frames as f64).floor() as i64 + 1;
        let behind = !self.fresh && first < self.base;
        let far_ahead = first > self.read_pos.saturating_add(i64::from(self.rate));
        if behind || far_ahead {
            self.dec.seek(first as f64 / f64::from(self.rate))?;
            self.buf.clear();
            self.base = first;
            self.read_pos = first;
            self.fresh = true;
            self.eof = false;
        }

        while !self.eof && self.read_pos <= last {
            if !self.read_chunk()? {
                self.eof = true;
            }
        }

        // Drop what is before the first output sample.
        let avail = (self.buf.len() / 2) as i64;
        let drop = (first - self.base).clamp(0, avail);
        if drop > 0 {
            let n = usize::try_from(drop).unwrap_or(0).saturating_mul(2);
            self.buf.drain(..n);
            self.base += drop;
        }
        if self.buf.is_empty() && first > self.base {
            self.base = first;
        }

        out.reserve(frames * 2);
        for j in 0..frames {
            let pos = pos0 + ratio * j as f64;
            let i0 = pos.floor() as i64;
            let t = (pos - i0 as f64) as f32;
            let (l0, r0) = self.sample(i0);
            let (l1, r1) = self.sample(i0 + 1);
            out.push(l0 + (l1 - l0) * t);
            out.push(r0 + (r1 - r0) * t);
        }
        Ok(())
    }
}

/// Version marker in the remuxed audio-stream file names. Bump it when the remux output changes so
/// files written by an older build are never reused and are pruned as stale.
const AUDIO_STREAM_CACHE_VERSION: &str = "v2";

/// True for a file name in the audio-stream cache that an older build wrote: any `.m4a` that is
/// neither a current file (`-v2.m4a`) nor a current in-progress file (`-v2.part.m4a`).
fn is_stale_audio_cache_name(name: &str) -> bool {
    let current = format!("-{AUDIO_STREAM_CACHE_VERSION}.m4a");
    let current_part = format!("-{AUDIO_STREAM_CACHE_VERSION}.part.m4a");
    name.ends_with(".m4a") && !name.ends_with(&current) && !name.ends_with(&current_part)
}

/// Deletes files in the remuxed audio-stream directory that this build does not use: old-format
/// `.m4a` files (see `is_stale_audio_cache_name`), `.part` files last written more than an hour
/// ago, and current files last written more than seven days ago. Runs once per process, before
/// the directory is first used. Best effort: every error is ignored.
fn prune_audio_stream_dir(dir: &Path) {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    const MAX_PART_AGE: std::time::Duration = std::time::Duration::from_secs(3600);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let current_suffix = format!("-{AUDIO_STREAM_CACHE_VERSION}.m4a");
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let age = meta
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok());
        let doomed = if is_stale_audio_cache_name(&name) {
            true
        } else if name.ends_with(".part") || name.ends_with(".part.m4a") {
            age.is_some_and(|a| a > MAX_PART_AGE)
        } else if name.ends_with(&current_suffix) {
            age.is_some_and(|a| a > MAX_AGE)
        } else {
            false
        };
        if doomed {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Stream `stream` (0-based among the file's audio tracks in trak order) as a (file, stream) pair
/// Media Foundation decodes unambiguously. For an MP4/MOV/M4V with several audio tracks the track
/// is losslessly remuxed into its own single-track file in the temp dir (reused when present) and
/// stream 0 of that file is returned, so the result never depends on how MF orders or exposes
/// multi-track MP4s. Anything else (or any failure) falls back to the original file and stream.
fn isolate_audio_stream(path: &Path, stream: u32) -> (PathBuf, u32) {
    use std::hash::{Hash, Hasher};
    let fallback = || (path.to_path_buf(), stream);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "mp4" | "m4v" | "mov") {
        return fallback();
    }
    let Ok(movie) = clipcheck_mp4::reader::read_movie(path) else {
        return fallback();
    };
    let audio: Vec<usize> = movie
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio())
        .map(|(i, _)| i)
        .collect();
    let Some(&track) = audio.get(stream as usize) else {
        return fallback();
    };
    if audio.len() < 2 {
        return fallback();
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return fallback();
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let dir = std::env::temp_dir().join("clipcheck-audio-streams");
    static PRUNED: std::sync::Once = std::sync::Once::new();
    PRUNED.call_once(|| prune_audio_stream_dir(&dir));
    let out = dir.join(format!(
        "{:016x}-{}-{mtime}-{stream}-{AUDIO_STREAM_CACHE_VERSION}.m4a",
        h.finish(),
        meta.len()
    ));
    if std::fs::metadata(&out)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
    {
        return (out, 0);
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return fallback();
    }
    let tmp = out.with_extension("part.m4a");
    let opts = clipcheck_mp4::remux::RemuxOptions {
        start_secs: 0.0,
        end_secs: None,
        tracks: clipcheck_mp4::remux::TrackSelect::Only(vec![track]),
        faststart: true,
    };
    if clipcheck_mp4::remux::concat_remux(&[path.to_path_buf()], &tmp, &opts).is_ok()
        && std::fs::rename(&tmp, &out).is_ok()
    {
        return (out, 0);
    }
    let _ = std::fs::remove_file(&tmp);
    fallback()
}

/// Decodes audio with `clipcheck_mfdec::audio::AudioDecoder`, resamples to the requested rate and
/// converts to stereo f32 (linear resampling; see `downmix_frame` for the channel mapping). Keeps
/// at most `MAX_OPEN_AUDIO` decoders open. Forward reads are contiguous; a read that goes back or
/// more than one second ahead seeks.
pub struct MfAudioProvider {
    sources: HashMap<String, MediaSource>,
    slots: Vec<AudioSlot>,
    bad: HashSet<(String, u32)>,
    clock: u64,
}

impl MfAudioProvider {
    /// Builds a provider that resolves media ids through the project's media bin.
    pub fn new(project: &Project) -> MfAudioProvider {
        MfAudioProvider {
            sources: sources_of(project),
            slots: Vec::new(),
            bad: HashSet::new(),
            clock: 0,
        }
    }

    /// Index of the open decoder for (`media_id`, `stream`), opening one if needed.
    fn slot_index(&mut self, media_id: &str, stream: u32, path: &Path) -> Option<usize> {
        self.clock = self.clock.saturating_add(1);
        let now = self.clock;
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| s.media_id == media_id && s.stream == stream)
        {
            if let Some(s) = self.slots.get_mut(i) {
                s.used = now;
            }
            return Some(i);
        }
        if self.bad.contains(&(media_id.to_string(), stream)) {
            return None;
        }
        let (src, src_stream) = isolate_audio_stream(path, stream);
        let dec = match AudioDecoder::open(&src, src_stream) {
            Ok(d) => d,
            Err(_) => {
                self.bad.insert((media_id.to_string(), stream));
                return None;
            }
        };
        if self.slots.len() >= MAX_OPEN_AUDIO {
            let oldest = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.used)
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                self.slots.remove(i);
            }
        }
        let rate = dec.sample_rate();
        let channels = usize::try_from(dec.channels()).unwrap_or(1).max(1);
        self.slots.push(AudioSlot {
            media_id: media_id.to_string(),
            stream,
            dec,
            rate,
            channels,
            buf: Vec::new(),
            base: 0,
            read_pos: 0,
            fresh: true,
            eof: false,
            scratch: Vec::new(),
            used: now,
        });
        self.slots.len().checked_sub(1)
    }
}

impl AudioProvider for MfAudioProvider {
    fn read(
        &mut self,
        media_id: &str,
        stream: u32,
        src_time: Tick,
        frames: usize,
        sample_rate: u32,
        out: &mut Vec<f32>,
    ) -> Result<bool, RenderError> {
        out.clear();
        if frames == 0 || sample_rate == 0 {
            return Ok(false);
        }
        let Some(path) = self.sources.get(media_id).map(|s| s.path.clone()) else {
            return Ok(false);
        };
        let Some(idx) = self.slot_index(media_id, stream, &path) else {
            return Ok(false);
        };
        let Some(slot) = self.slots.get_mut(idx) else {
            return Ok(false);
        };
        if slot.rate == 0 {
            return Ok(false);
        }
        let rate = f64::from(slot.rate);
        let pos0 = src_time.as_secs_f64().max(0.0) * rate;
        let ratio = rate / f64::from(sample_rate);
        match slot.render(pos0, ratio, frames, out) {
            Ok(()) => Ok(true),
            Err(_) => {
                out.clear();
                self.slots.remove(idx);
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod audio_cache_tests {
    use super::{is_stale_audio_cache_name, prune_audio_stream_dir};

    #[test]
    fn old_format_names_are_stale() {
        assert!(is_stale_audio_cache_name("x-1-2-3.m4a"));
        assert!(is_stale_audio_cache_name("abc-1-2-3-5.part.m4a"));
    }

    #[test]
    fn current_and_non_cache_names_are_not_stale() {
        assert!(!is_stale_audio_cache_name("abc-1-2-3-v2.m4a"));
        assert!(!is_stale_audio_cache_name("abc-1-2-3-v2.part.m4a"));
        assert!(!is_stale_audio_cache_name("notes.txt"));
        assert!(!is_stale_audio_cache_name("a.m4a.keep"));
        assert!(!is_stale_audio_cache_name("a.part"));
    }

    #[test]
    fn prune_removes_old_format_files_and_keeps_fresh_current_ones() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("clipcheck-prune-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["x-1-2-3.m4a", "y-v2.m4a", "z-v2.part.m4a", "notes.txt"] {
            std::fs::write(dir.join(name), b"data").unwrap();
        }
        prune_audio_stream_dir(&dir);
        assert!(!dir.join("x-1-2-3.m4a").exists());
        assert!(dir.join("y-v2.m4a").exists());
        assert!(dir.join("z-v2.part.m4a").exists());
        assert!(dir.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod downmix_tests {
    use super::downmix_frame;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn stereo_passes_through() {
        assert_eq!(downmix_frame(&[0.25, -0.5]), (0.25, -0.5));
    }

    #[test]
    fn mono_is_duplicated() {
        assert_eq!(downmix_frame(&[0.3]), (0.3, 0.3));
    }

    #[test]
    fn five_one_centre_is_audible_in_both_channels() {
        // Dialogue only on the centre channel must not be lost.
        let (l, r) = downmix_frame(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
        let expected = 0.707 / (1.0 + 2.0 * 0.707);
        assert!(close(l, expected), "left {l}");
        assert!(close(r, expected), "right {r}");
    }

    #[test]
    fn five_one_lfe_is_dropped() {
        assert_eq!(downmix_frame(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]), (0.0, 0.0));
    }

    #[test]
    fn five_one_full_scale_stays_within_unit_range() {
        let (l, r) = downmix_frame(&[1.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
        assert!(close(l, 1.0), "left {l}");
        assert!(close(r, 1.0), "right {r}");
    }

    #[test]
    fn five_one_left_surround_only_hits_left() {
        let (l, r) = downmix_frame(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert!(l > 0.0);
        assert!(close(r, 0.0));
    }

    #[test]
    fn seven_one_centre_and_surrounds_map_to_both_sides() {
        let (l, r) = downmix_frame(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let expected = 0.707 / (1.0 + 3.0 * 0.707);
        assert!(close(l, expected) && close(r, expected));
        let (l, r) = downmix_frame(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
        assert!(close(l, 1.0) && close(r, 1.0));
    }

    #[test]
    fn other_layouts_keep_first_two_channels() {
        assert_eq!(downmix_frame(&[0.1, 0.2, 0.9]), (0.1, 0.2));
    }
}
