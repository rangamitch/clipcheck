//! Audio decoding through the Media Foundation source reader. Owner: worker `mfdec-audio`.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows::core::{GUID, PCWSTR, PROPVARIANT};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoIncrementMTAUsage;

use crate::mf::{win_err, ComThread};
use crate::DecError;

/// Upper bound on stream indices probed, so a hostile file cannot make the scan run forever.
const MAX_STREAMS: u32 = 256;

/// Frames per `read` call used by `decode_peaks`.
const PEAK_CHUNK_FRAMES: usize = 8192;

/// One open audio stream decoded to 32-bit float PCM.
pub struct AudioDecoder {
    reader: IMFSourceReader,
    /// Media Foundation stream index of the selected audio stream.
    index: u32,
    sample_rate: u32,
    channels: u32,
    duration_100ns: i64,
    // Declared last so COM is uninitialised after the reader is released.
    _com: ComThread,
}

impl AudioDecoder {
    /// Opens audio stream `stream` (0-based among the file's audio streams) of any file Media
    /// Foundation can read (MP4/M4A/AAC/MP3/WAV/FLAC...). Output is interleaved f32 at the stream's
    /// native rate and channel count (MF converts to float PCM).
    pub fn open(path: &Path, stream: u32) -> Result<AudioDecoder, DecError> {
        ensure_file(path)?;
        startup()?;
        let com = ComThread::enter();

        let wpath = wide(path);
        // SAFETY: wpath is NUL-terminated and alive for the call; no attribute store is passed.
        let reader = unsafe { MFCreateSourceReaderFromURL(PCWSTR(wpath.as_ptr()), None) }
            .map_err(|e| win_err("MFCreateSourceReaderFromURL", e))?;

        let (index, native) = audio_stream(&reader, stream)?.ok_or_else(|| {
            DecError::BadInput(format!("no audio stream {stream} in {}", path.display()))
        })?;

        // SAFETY: stream indices and bool selections are plain values.
        unsafe { reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false) }
            .map_err(|e| win_err("SetStreamSelection", e))?;
        unsafe { reader.SetStreamSelection(index, true) }
            .map_err(|e| win_err("SetStreamSelection", e))?;

        let rate = u32_attr(&native, &MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0);
        let channels = u32_attr(&native, &MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0);

        // SAFETY: MFCreateMediaType has no preconditions; the result is owned by this function.
        let media = unsafe { MFCreateMediaType() }.map_err(|e| win_err("MFCreateMediaType", e))?;
        // SAFETY: GUID references are valid for the calls; writes go into our own media type.
        unsafe { media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio) }
            .map_err(|e| win_err("SetGUID", e))?;
        unsafe { media.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float) }
            .map_err(|e| win_err("SetGUID", e))?;
        unsafe { media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32) }
            .map_err(|e| win_err("SetUINT32", e))?;
        if channels > 0 && rate > 0 {
            // Packed float frames: 4 bytes per channel per frame.
            let block = channels.saturating_mul(4);
            // SAFETY: plain attribute writes on our own media type.
            unsafe { media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels) }
                .map_err(|e| win_err("SetUINT32", e))?;
            unsafe { media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate) }
                .map_err(|e| win_err("SetUINT32", e))?;
            unsafe { media.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block) }
                .map_err(|e| win_err("SetUINT32", e))?;
            unsafe {
                media.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    rate.saturating_mul(block),
                )
            }
            .map_err(|e| win_err("SetUINT32", e))?;
        }

        // SAFETY: the partial type is a live object; no reserved pointer is passed.
        unsafe { reader.SetCurrentMediaType(index, None, &media) }
            .map_err(|e| decoder_err(e, stream))?;

        // The reader's current type is what `read` will actually deliver.
        let (rate, channels) = current_format(&reader, index)?;
        if rate == 0 || channels == 0 {
            return Err(DecError::Unsupported(
                "audio stream reports no sample rate or channel count".to_string(),
            ));
        }
        let duration_100ns = presentation_duration(&reader);

        Ok(AudioDecoder {
            reader,
            index,
            sample_rate: rate,
            channels,
            duration_100ns,
            _com: com,
        })
    }

    /// Native sample rate of the decoded output.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count of the decoded output.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Duration in 100 ns units (0 if unknown).
    pub fn duration_100ns(&self) -> i64 {
        self.duration_100ns
    }

    /// Seeks to `secs` (the next read starts at or just before it; callers trim).
    pub fn seek(&mut self, secs: f64) -> Result<(), DecError> {
        let ticks = (secs.max(0.0) * 1e7) as i64;
        let pos = PROPVARIANT::from(ticks);
        // SAFETY: GUID_NULL (zeroed) selects 100 ns units; pos is a live VT_I8 variant.
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &pos) }
            .map_err(|e| win_err("SetCurrentPosition", e))
    }

    /// Reads at least `max_frames` PCM frames (at least one) into `out` (appended, interleaved),
    /// or fewer at end of stream. Whole decoded samples are appended, so the result may hold more
    /// than `max_frames` frames when one decoded sample is larger than the request. Returns the
    /// number of frames appended and the presentation time (100 ns) of the first one; `Ok(None)`
    /// once the stream has no data left.
    pub fn read(
        &mut self,
        max_frames: usize,
        out: &mut Vec<f32>,
    ) -> Result<Option<(usize, i64)>, DecError> {
        let want = max_frames.max(1);
        let mut frames = 0usize;
        let mut first_ts: Option<i64> = None;
        let mut eos = false;
        while frames < want && !eos {
            let (flags, ts, sample) = self.read_sample()?;
            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                eos = true;
            }
            if flags & (MF_SOURCE_READERF_ERROR.0 as u32) != 0 {
                return Err(DecError::Windows {
                    context: "ReadSample",
                    message: "stream error flag set".to_string(),
                });
            }
            if flags & (MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32) != 0 {
                self.refresh_format()?;
            }
            // A missing sample is a stream tick or gap: no audio this time.
            let Some(sample) = sample else {
                continue;
            };
            let channels = self.channels as usize;
            let before = out.len();
            append_sample(&sample, channels, out)?;
            let added = out.len().saturating_sub(before) / channels.max(1);
            if added > 0 && first_ts.is_none() {
                first_ts = Some(ts);
            }
            frames += added;
        }
        Ok(match first_ts {
            Some(ts) if frames > 0 => Some((frames, ts)),
            _ => None,
        })
    }

    /// One `ReadSample` call on the selected stream: (flags, timestamp, optional sample).
    fn read_sample(&mut self) -> Result<(u32, i64, Option<IMFSample>), DecError> {
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: out-pointers reference live locals of the right types; the actual-index
        // pointer is not requested.
        unsafe {
            self.reader.ReadSample(
                self.index,
                0,
                None,
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )
        }
        .map_err(|e| win_err("ReadSample", e))?;
        Ok((flags, ts, sample))
    }

    /// Re-reads the current type after the reader reports a type change.
    fn refresh_format(&mut self) -> Result<(), DecError> {
        let (rate, channels) = current_format(&self.reader, self.index)?;
        if rate > 0 && channels > 0 {
            self.sample_rate = rate;
            self.channels = channels;
        }
        Ok(())
    }
}

/// Streams a whole audio stream and returns `buckets` peak values (0..=1, max absolute sample of
/// all channels per bucket) plus the stream duration in seconds. Uses constant memory.
///
/// When the file reports a duration, buckets are laid over that duration. When it does not, the
/// stream is first reduced to one peak per 0.1 s (a growable list, one entry per 0.1 s), which is
/// then folded into `buckets`; the returned duration is then the decoded length.
pub fn decode_peaks(path: &Path, stream: u32, buckets: usize) -> Result<(Vec<f32>, f64), DecError> {
    let mut dec = AudioDecoder::open(path, stream)?;
    let rate = dec.sample_rate().max(1) as usize;
    let ch = dec.channels().max(1) as usize;
    let dur_100ns = dec.duration_100ns();
    let file_secs = dur_100ns.max(0) as f64 / 1e7;
    if buckets == 0 {
        return Ok((Vec::new(), file_secs));
    }

    let mut buf: Vec<f32> = Vec::new();
    if dur_100ns > 0 {
        let total_frames = dur_100ns as f64 * rate as f64 / 1e7;
        let scale = buckets as f64 / total_frames.max(1.0);
        let mut peaks = vec![0f32; buckets];
        let mut pos: u64 = 0;
        while dec.read(PEAK_CHUNK_FRAMES, &mut buf)?.is_some() {
            for frame in buf.chunks_exact(ch) {
                let b = ((pos as f64 * scale) as usize).min(buckets - 1);
                if let Some(slot) = peaks.get_mut(b) {
                    *slot = slot.max(frame_peak(frame));
                }
                pos += 1;
            }
            buf.clear();
        }
        clamp_unit(&mut peaks);
        return Ok((peaks, file_secs));
    }

    // Unknown duration: one peak per 0.1 s first, then fold into the buckets.
    let window = (rate / 10).max(1);
    let mut per_tenth: Vec<f32> = Vec::new();
    let mut acc = 0f32;
    let mut in_window = 0usize;
    let mut frames_total: u64 = 0;
    while dec.read(PEAK_CHUNK_FRAMES, &mut buf)?.is_some() {
        for frame in buf.chunks_exact(ch) {
            acc = acc.max(frame_peak(frame));
            in_window += 1;
            frames_total += 1;
            if in_window == window {
                per_tenth.push(acc);
                acc = 0.0;
                in_window = 0;
            }
        }
        buf.clear();
    }
    if in_window > 0 {
        per_tenth.push(acc);
    }

    let mut peaks = vec![0f32; buckets];
    let n = per_tenth.len();
    if n > 0 {
        let scale = buckets as f64 / n as f64;
        for (i, &p) in per_tenth.iter().enumerate() {
            let b = ((i as f64 * scale) as usize).min(buckets - 1);
            if let Some(slot) = peaks.get_mut(b) {
                *slot = slot.max(p);
            }
        }
    }
    clamp_unit(&mut peaks);
    Ok((peaks, frames_total as f64 / rate as f64))
}

/// Largest absolute sample across the channels of one frame.
fn frame_peak(frame: &[f32]) -> f32 {
    frame.iter().fold(0f32, |m, s| m.max(s.abs()))
}

/// Clamps peaks into 0..=1 (NaN-safe: NaN is not produced by `max`).
fn clamp_unit(peaks: &mut [f32]) {
    for p in peaks.iter_mut() {
        *p = p.clamp(0.0, 1.0);
    }
}

/// Once-per-process Media Foundation startup, same scheme as `mf.rs`.
fn startup() -> Result<(), DecError> {
    static INIT: OnceLock<Result<(), DecError>> = OnceLock::new();
    INIT.get_or_init(|| {
        // SAFETY: CoIncrementMTAUsage has no preconditions; the cookie has no Drop and is
        // intentionally dropped, so the MTA stays alive for the process lifetime.
        let _ = unsafe { CoIncrementMTAUsage() };
        // SAFETY: MFStartup takes a version constant and flags; it runs once in this closure.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| win_err("MFStartup", e))
    })
    .clone()
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

/// Native media type of stream `index`; `None` once the index is past the last stream.
fn native_type(reader: &IMFSourceReader, index: u32) -> Result<Option<IMFMediaType>, DecError> {
    // SAFETY: plain query on a live source reader.
    match unsafe { reader.GetNativeMediaType(index, 0) } {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => Ok(None),
        Err(e) => Err(win_err("GetNativeMediaType", e)),
    }
}

/// Media Foundation stream index and native type of the `nth` audio stream (0-based).
fn audio_stream(
    reader: &IMFSourceReader,
    nth: u32,
) -> Result<Option<(u32, IMFMediaType)>, DecError> {
    let mut seen = 0u32;
    for index in 0..MAX_STREAMS {
        let Some(t) = native_type(reader, index)? else {
            return Ok(None);
        };
        if guid_attr(&t, &MF_MT_MAJOR_TYPE) == Some(MFMediaType_Audio) {
            if seen == nth {
                return Ok(Some((index, t)));
            }
            seen += 1;
        }
    }
    Ok(None)
}

fn guid_attr(t: &IMFAttributes, key: &GUID) -> Option<GUID> {
    // SAFETY: key is a valid GUID reference for the call.
    unsafe { t.GetGUID(key) }.ok()
}

fn u32_attr(t: &IMFAttributes, key: &GUID) -> Option<u32> {
    // SAFETY: key is a valid GUID reference for the call.
    unsafe { t.GetUINT32(key) }.ok()
}

/// Sample rate and channel count of the stream's current (output) type; 0 where unknown.
fn current_format(reader: &IMFSourceReader, index: u32) -> Result<(u32, u32), DecError> {
    // SAFETY: stream index is a plain value on a live reader.
    let current = unsafe { reader.GetCurrentMediaType(index) }
        .map_err(|e| win_err("GetCurrentMediaType", e))?;
    let rate = u32_attr(&current, &MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0);
    let channels = u32_attr(&current, &MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0);
    Ok((rate, channels))
}

/// Duration of the whole presentation in 100 ns units; 0 if unknown.
fn presentation_duration(reader: &IMFSourceReader) -> i64 {
    // SAFETY: MEDIASOURCE selects the whole presentation; the GUID reference is valid.
    unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }
    .ok()
    .and_then(|pv| u64::try_from(&pv).ok())
    .and_then(|ticks| i64::try_from(ticks).ok())
    .unwrap_or(0)
}

/// Maps a decoder-missing failure to `Unsupported`, anything else to `Windows`.
fn decoder_err(e: windows::core::Error, stream: u32) -> DecError {
    if e.code() == MF_E_TOPO_CODEC_NOT_FOUND || e.code() == MF_E_INVALIDMEDIATYPE {
        DecError::Unsupported(format!("no decoder for audio stream {stream}"))
    } else {
        win_err("SetCurrentMediaType", e)
    }
}

/// Appends the whole frames of one sample, converted from little-endian f32 bytes.
fn append_sample(sample: &IMFSample, channels: usize, out: &mut Vec<f32>) -> Result<(), DecError> {
    // SAFETY: plain call on a live sample.
    let buf = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(|e| win_err("ConvertToContiguousBuffer", e))?;
    let mut ptr: *mut u8 = null_mut();
    let mut len = 0u32;
    // SAFETY: out-pointers are valid locals; the buffer stays locked until Unlock below.
    unsafe { buf.Lock(&mut ptr, None, Some(&mut len)) }.map_err(|e| win_err("Lock", e))?;
    let result = if ptr.is_null() {
        Err(DecError::Windows {
            context: "Lock",
            message: "null sample pointer".to_string(),
        })
    } else {
        // SAFETY: Lock reported `len` readable bytes at `ptr`; they stay valid until Unlock and
        // are only read here.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        push_f32(bytes, channels, out);
        Ok(())
    };
    // SAFETY: balances the successful Lock above.
    let _ = unsafe { buf.Unlock() };
    result
}

/// Decodes little-endian f32 bytes into `out`, dropping any trailing partial frame.
fn push_f32(bytes: &[u8], channels: usize, out: &mut Vec<f32>) {
    let frame_bytes = 4 * channels.max(1);
    let whole = bytes.len() - bytes.len() % frame_bytes;
    let whole_bytes = bytes.get(..whole).unwrap_or_default();
    out.reserve(whole / 4);
    let (quads, _rest) = whole_bytes.as_chunks::<4>();
    out.extend(quads.iter().map(|c| f32::from_le_bytes(*c)));
}
