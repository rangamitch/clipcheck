//! Media bin helpers: probing, library lookup, waveform peaks, thumbnails, preview URLs.
//! Owner: worker `ui-media`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::UNIX_EPOCH;

use clipcheck_library::Library;
use clipcheck_mfdec::audio::AudioDecoder;
use clipcheck_render::dsp::{peak_db, LoudnessMeter};
use clipcheck_timeline::cmd::Session;
use clipcheck_timeline::model::{
    new_id, Clip, ClipKind, Fingerprint, MediaAudio, MediaKind, MediaRef,
};
use clipcheck_timeline::time::Tick;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use super::projects::media_is_missing;
use super::{paths, settings, EditorState};
use crate::commands;
use crate::media::resolve_recording_path;
use crate::mediaserver;

/// Media longer than this (ms) computes its waveform in the background and emits `peaks-ready`.
const LONG_MEDIA_MS: i64 = 10 * 60 * 1000;
/// Longest source range measured for loudness, in seconds.
const MEASURE_MAX_SECS: f64 = 600.0;
/// Frames per decoder read while measuring loudness.
const MEASURE_READ_FRAMES: usize = 8192;
/// Smallest and largest waveform bucket counts.
const MIN_BUCKETS: u32 = 16;
const MAX_BUCKETS: u32 = 8192;
/// Thumbnail height limits in pixels.
const MIN_THUMB_HEIGHT: u32 = 16;
const MAX_THUMB_HEIGHT: u32 = 240;
/// Most thumbnail times accepted in one call.
const MAX_THUMB_TIMES: usize = 1024;
/// Largest image file used as a thumbnail (it is sent as a data URI for every time).
const MAX_IMAGE_THUMB_BYTES: u64 = 1024 * 1024;
/// Largest image file accepted by the probe.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Most thumbnails kept in memory.
const THUMB_CACHE_MAX: usize = 256;

/// Probes a file with `clipcheck_mp4::reader` (MP4/MOV/M4A: duration, size, fps, codec, audio
/// stream titles from track names) falling back to `clipcheck_mfdec::probe` (other containers,
/// MP3/WAV/FLAC, images through WIC size). Returns a `MediaRef` with a NEW id, `name` = file name,
/// fingerprint from file metadata, `missing = false`. Errors with a readable reason for
/// unsupported files ("Unsupported file type .xyz", "No decoder for HEVC").
#[tauri::command]
pub async fn editor_probe_media(path: String) -> Result<MediaRef, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(&path);
        if !path.is_absolute() {
            return Err(format!("path is not absolute: {}", path.display()));
        }
        build_media_ref(&path)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Resolves a library recording id to its file (library DB, read-only handle as in `player.rs`)
/// and probes it; sets `library_recording_id` and uses the recording's title as `name`.
#[tauri::command]
pub async fn editor_media_from_recording(recording_id: String) -> Result<MediaRef, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = commands::try_default_db_path().map_err(|e| e.to_string())?;
        let path = resolve_recording_path(&db, &recording_id).map_err(|e| e.to_string())?;
        let mut media = build_media_ref(&path)?;
        if media.duration == Tick::ZERO {
            // The probe found no duration; the library row may still know it.
            let row = Library::open_readonly(&db)
                .ok()
                .and_then(|lib| lib.get_recording(&recording_id).ok().flatten());
            if let Some(row) = row {
                media.duration = tick_from_ms(row.duration_ms);
            }
        }
        media.library_recording_id = Some(recording_id);
        Ok(media)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Missing/changed flags for every media of the open project.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatus {
    /// Media id.
    pub media_id: String,
    /// File gone or different from the fingerprint.
    pub missing: bool,
}

/// Re-checks every media of the open project on disk (does not modify the project).
#[tauri::command(async)]
pub fn editor_media_status(state: State<'_, EditorState>) -> Result<Vec<MediaStatus>, String> {
    // The lock is held only while copying the media list; the file checks run without it.
    let media: Vec<MediaRef> = {
        let guard = lock_session(&state);
        guard
            .as_ref()
            .map(|s| s.project.media.clone())
            .unwrap_or_default()
    };
    Ok(media
        .iter()
        .map(|m| MediaStatus {
            media_id: m.id.clone(),
            missing: media_is_missing(m),
        })
        .collect())
}

/// Peak waveform (`buckets` values 0..=1) for one audio stream of a media file of the open project.
/// Served from the peaks cache when present; otherwise computed in the background with
/// `mfdec::audio::decode_peaks`, stored in the cache, and returned. Emits the Tauri event
/// `peaks-ready` `{ mediaId, stream }` instead of blocking when the file is longer than 10 minutes
/// (the command then returns an empty vec immediately).
#[tauri::command]
pub async fn editor_waveform(
    state: State<'_, EditorState>,
    app: AppHandle,
    media_id: String,
    stream: u32,
    buckets: u32,
) -> Result<Vec<f32>, String> {
    let buckets = buckets.clamp(MIN_BUCKETS, MAX_BUCKETS) as usize;
    let media = find_media(&state, &media_id)?;
    if media.kind == MediaKind::Image {
        return Err("images have no audio".to_string());
    }
    let Ok(index) = usize::try_from(stream) else {
        return Err(format!("no audio stream {stream} in this media"));
    };
    if index >= media.audio.len() {
        return Err(format!("no audio stream {stream} in this media"));
    }
    let path = PathBuf::from(&media.path);
    let meta = fs::metadata(&path).map_err(|_| format!("file is missing: {}", media.path))?;
    let cache_file = peaks_cache_file(&cache_root(), &path, &meta, stream, buckets);
    if let Some(peaks) = peaks_mem_get(&cache_file) {
        return Ok(peaks);
    }
    if let Some(peaks) = read_peaks(&cache_file, buckets) {
        return Ok(peaks);
    }

    if media.duration.as_ms() > LONG_MEDIA_MS {
        // A computation already running for this cache file: the UI gets `peaks-ready` later.
        if !claim_pending(&cache_file) {
            return Ok(Vec::new());
        }
        let pending = PendingGuard(cache_file.clone());
        let id = media_id.clone();
        drop(tauri::async_runtime::spawn(async move {
            // Released when the task ends, on success or failure.
            let _pending = pending;
            let done = tauri::async_runtime::spawn_blocking(move || {
                compute_peaks(&path, &cache_file, stream, buckets)
            })
            .await;
            if matches!(done, Ok(Ok((_, true)))) {
                let _ = app.emit(
                    "peaks-ready",
                    serde_json::json!({ "mediaId": id, "stream": stream }),
                );
            }
        }));
        return Ok(Vec::new());
    }

    tauri::async_runtime::spawn_blocking(move || {
        compute_peaks(&path, &cache_file, stream, buckets).map(|(peaks, _)| peaks)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// JPEG thumbnails as `data:image/jpeg;base64,...` strings for the given source times (ms) of a
/// media file of the open project, each `height` px tall (16..=240). Uses `mfdec` to decode and
/// `mfdec::thumbnail_jpeg`; an on-disk cache avoids repeat work. An image returns a single entry
/// (its one frame, not one copy per time). Audio media returns an error. Stops with an error when
/// the project it was requested for is closed meanwhile.
#[tauri::command]
pub async fn editor_thumbnails(
    state: State<'_, EditorState>,
    app: AppHandle,
    media_id: String,
    times_ms: Vec<i64>,
    height: u32,
) -> Result<Vec<String>, String> {
    let media = find_media(&state, &media_id)?;
    if media.kind == MediaKind::Audio {
        return Err("audio media has no video frames".to_string());
    }
    if times_ms.len() > MAX_THUMB_TIMES {
        return Err(format!("at most {MAX_THUMB_TIMES} thumbnails per call"));
    }
    let height = height.clamp(MIN_THUMB_HEIGHT, MAX_THUMB_HEIGHT);
    let Some(project_id) = open_project_id(&state) else {
        // Empty, not an error: the UI marks a failed request as failed for good.
        return Ok(Vec::new());
    };
    tauri::async_runtime::spawn_blocking(move || {
        let still_open = || {
            app.try_state::<EditorState>()
                .and_then(|st| open_project_id(&st))
                .is_some_and(|id| id == project_id)
        };
        thumbnails_blocking(&media, &times_ms, height, &still_open)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A playable URL for a media file of the open project: for library recordings the existing
/// loopback server URL (see `mediaserver` and `player.rs`), for imports a URL registered in a
/// per-session allowlist served by the same mechanism (only exact paths of media in the open
/// project are ever served). Errors if the codec cannot be played by WebView2 and no proxy exists
/// (message names the codec).
#[tauri::command]
pub async fn editor_media_url(
    state: State<'_, EditorState>,
    media_id: String,
) -> Result<String, String> {
    let media = find_media(&state, &media_id)?;
    if !Path::new(&media.path).is_file() {
        return Err(format!("file is missing: {}", media.path));
    }
    media_url_for(&media)
}

/// Result of measuring a clip's loudness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoudnessReport {
    /// Integrated LUFS (None if silent).
    pub lufs: Option<f64>,
    /// Peak in dBFS.
    pub peak_db: f64,
}

/// Measures the loudness of an audio clip's used source range (applies speed, not gain) with
/// `render::dsp::LoudnessMeter`, for the Normalise button. The UI then sends `OffsetGain`.
#[tauri::command]
pub async fn editor_measure_clip_loudness(
    state: State<'_, EditorState>,
    clip_id: String,
) -> Result<LoudnessReport, String> {
    let (clip, media) = find_clip_and_media(&state, &clip_id)?;
    let stream = clip.audio.source_track;
    let index = usize::try_from(stream).unwrap_or(usize::MAX);
    if index >= media.audio.len() {
        return Err(format!("the media has no audio stream {stream}"));
    }
    let path = PathBuf::from(&media.path);
    let src_in = clip.src_in;
    let src_len = clip.src_len();
    tauri::async_runtime::spawn_blocking(move || measure_loudness(&path, stream, src_in, src_len))
        .await
        .map_err(|e| e.to_string())?
}

/// Playable URL for one audio stream (0-based, in audio-track order) of a media file of the open
/// project. The webview's `<video>`/`<audio>` elements play only a file's default audio stream, so
/// the stream is copied losslessly into its own single-track MP4 in the cache
/// (`audio\<size>-<mtime_ms>-<stream>.m4a`, reused when present) and served through the import
/// allowlist (`audio/mp4`). Only MP4/MOV/M4V files; images have no audio.
#[tauri::command]
pub async fn editor_audio_stream_url(
    state: State<'_, EditorState>,
    media_id: String,
    stream: u32,
) -> Result<String, String> {
    let media = find_media(&state, &media_id)?;
    if media.kind == MediaKind::Image {
        return Err("images have no audio".to_string());
    }
    let index = usize::try_from(stream).unwrap_or(usize::MAX);
    if index >= media.audio.len() {
        return Err(format!("no audio stream {stream} in this media"));
    }
    let path = PathBuf::from(&media.path);
    let out = tauri::async_runtime::spawn_blocking(move || extract_audio_stream(&path, stream))
        .await
        .map_err(|e| e.to_string())??;
    mediaserver::register_extra_path(&out).ok_or_else(|| {
        format!(
            "{} cannot be served: it is not a media file or the media server is not running",
            out.display()
        )
    })
}

// ---------------------------------------------------------------------------------------------
// Session access. Locks are held only while cloning what is needed.
// ---------------------------------------------------------------------------------------------

/// Locks the session, recovering from a poisoned mutex.
fn lock_session(state: &EditorState) -> MutexGuard<'_, Option<Session>> {
    state.session.lock().unwrap_or_else(|e| e.into_inner())
}

/// Id of the open project, if any.
fn open_project_id(state: &EditorState) -> Option<String> {
    lock_session(state).as_ref().map(|s| s.project.id.clone())
}

/// A copy of one media entry of the open project.
fn find_media(state: &EditorState, media_id: &str) -> Result<MediaRef, String> {
    let guard = lock_session(state);
    let session = guard
        .as_ref()
        .ok_or_else(|| "no project is open".to_string())?;
    session
        .project
        .media_ref(media_id)
        .cloned()
        .ok_or_else(|| format!("media {media_id} is not in the open project"))
}

/// Copies of an audio clip and the media it plays from.
fn find_clip_and_media(state: &EditorState, clip_id: &str) -> Result<(Clip, MediaRef), String> {
    let guard = lock_session(state);
    let session = guard
        .as_ref()
        .ok_or_else(|| "no project is open".to_string())?;
    let clip = session
        .project
        .clip(clip_id)
        .cloned()
        .ok_or_else(|| format!("clip {clip_id} not found"))?;
    if clip.kind != ClipKind::Audio {
        return Err("the clip is not an audio clip".to_string());
    }
    let media_id = clip
        .media_id
        .clone()
        .ok_or_else(|| "the clip has no media".to_string())?;
    let media = session
        .project
        .media_ref(&media_id)
        .cloned()
        .ok_or_else(|| "the clip's media is not in the project".to_string())?;
    Ok((clip, media))
}

// ---------------------------------------------------------------------------------------------
// Probing.
// ---------------------------------------------------------------------------------------------

/// Builds a `MediaRef` (with a new id) for a file, choosing the probe by extension.
fn build_media_ref(path: &Path) -> Result<MediaRef, String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("not a file: {}", path.display()));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut media = MediaRef {
        id: new_id(),
        name,
        path: path.to_string_lossy().into_owned(),
        fingerprint: Fingerprint {
            size: meta.len(),
            mtime_ms: mtime_ms(&meta),
        },
        ..MediaRef::default()
    };

    match ext_lower(path).as_str() {
        "mp4" | "m4v" | "mov" | "m4a" => match clipcheck_mp4::reader::read_movie(path) {
            Ok(movie) => fill_from_movie(&mut media, &movie),
            Err(reader_err) => fill_from_mfdec(&mut media, path)
                .map_err(|e| format!("cannot read {}: {reader_err}; {e}", path.display()))?,
        },
        "wav" | "mp3" | "flac" | "aac" | "ogg" | "wma" => fill_from_mfdec(&mut media, path)?,
        "png" | "jpg" | "jpeg" | "bmp" => fill_image(&mut media, path)?,
        "webp" => return Err("WebP images are not supported yet; use PNG, JPEG or BMP".to_string()),
        "" => return Err("Unsupported file type (no extension)".to_string()),
        other => return Err(format!("Unsupported file type .{other}")),
    }

    if media.kind == MediaKind::Audio && media.audio.is_empty() {
        return Err("no audio stream in this file".to_string());
    }
    Ok(media)
}

/// Fills a media entry from the MP4 reader's parsed movie.
fn fill_from_movie(media: &mut MediaRef, movie: &clipcheck_mp4::reader::Movie) {
    use clipcheck_mp4::reader::TrackKind;

    media.duration = tick_from_secs(movie.duration_secs);
    let mut has_video = false;
    for track in &movie.tracks {
        match &track.kind {
            TrackKind::Video {
                codec,
                width,
                height,
            } if !has_video => {
                has_video = true;
                media.video_codec = codec.clone();
                media.width = *width;
                media.height = *height;
                // Average frame rate: samples per second of the track.
                let scale = f64::from(track.timescale.max(1));
                let total: f64 = track
                    .samples
                    .iter()
                    .map(|s| f64::from(s.duration) / scale)
                    .sum();
                let count = track.samples.len() as f64;
                media.fps = if total > 0.0 && count > 0.0 {
                    (count / total) as f32
                } else {
                    0.0
                };
            }
            TrackKind::Audio {
                codec,
                channels,
                sample_rate,
            } => media.audio.push(MediaAudio {
                title: track
                    .title
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string(),
                codec: codec.clone(),
                channels: u32::from(*channels),
                sample_rate: *sample_rate,
            }),
            _ => {}
        }
    }
    media.kind = if has_video {
        MediaKind::Video
    } else {
        MediaKind::Audio
    };
}

/// Fills a media entry from `clipcheck_mfdec::probe` (any container Media Foundation reads).
fn fill_from_mfdec(media: &mut MediaRef, path: &Path) -> Result<(), String> {
    let info = clipcheck_mfdec::probe(path).map_err(|e| e.to_string())?;
    media.duration = tick_from_secs(info.duration_ms.max(0) as f64 / 1000.0);
    media.width = info.width;
    media.height = info.height;
    media.fps = if info.fps.is_finite() { info.fps } else { 0.0 };
    media.video_codec = info.video_codec.clone();
    media.audio = info
        .audio
        .iter()
        .map(|a| MediaAudio {
            title: String::new(),
            codec: a.codec.clone(),
            channels: a.channels,
            sample_rate: a.sample_rate,
        })
        .collect();
    let has_video = info.width > 0 || !info.video_codec.is_empty();
    media.kind = if has_video {
        MediaKind::Video
    } else {
        MediaKind::Audio
    };
    Ok(())
}

/// Fills a media entry for a still image, reading its size from the file header.
fn fill_image(media: &mut MediaRef, path: &Path) -> Result<(), String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_IMAGE_BYTES {
        return Err("image file is larger than 64 MB".to_string());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let (codec, _mime, width, height) = image_header(&bytes)
        .ok_or_else(|| "cannot read the image size from the file header".to_string())?;
    media.kind = MediaKind::Image;
    media.width = width;
    media.height = height;
    media.video_codec = codec.to_string();
    media.duration = Tick::ZERO;
    media.fps = 0.0;
    media.audio.clear();
    Ok(())
}

/// Codec name, MIME type, width and height from a PNG, JPEG or BMP header.
fn image_header(b: &[u8]) -> Option<(&'static str, &'static str, u32, u32)> {
    let found = if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        if b.get(12..16)? != &b"IHDR"[..] {
            return None;
        }
        let w = u32::from_be_bytes(arr::<4>(b, 16)?);
        let h = u32::from_be_bytes(arr::<4>(b, 20)?);
        ("png", "image/png", w, h)
    } else if b.starts_with(&[0xFF, 0xD8]) {
        let (w, h) = jpeg_size(b)?;
        ("jpeg", "image/jpeg", w, h)
    } else if b.starts_with(b"BM") {
        let w = i32::from_le_bytes(arr::<4>(b, 18)?).unsigned_abs();
        let h = i32::from_le_bytes(arr::<4>(b, 22)?).unsigned_abs();
        ("bmp", "image/bmp", w, h)
    } else {
        return None;
    };
    if found.2 == 0 || found.3 == 0 {
        return None;
    }
    Some(found)
}

/// Width and height from the first start-of-frame segment of a JPEG.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i: usize = 2;
    loop {
        if *b.get(i)? != 0xFF {
            return None;
        }
        let marker = *b.get(i.checked_add(1)?)?;
        match marker {
            // Fill byte: skip one byte and look again.
            0xFF => {
                i += 1;
                continue;
            }
            // Standalone markers without a length.
            0x01 | 0xD0..=0xD8 => {
                i += 2;
                continue;
            }
            // End of image, or start of scan before any frame header.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes(arr::<2>(b, i + 2)?));
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let h = u16::from_be_bytes(arr::<2>(b, i + 5)?);
            let w = u16::from_be_bytes(arr::<2>(b, i + 7)?);
            return Some((u32::from(w), u32::from(h)));
        }
        if len < 2 {
            return None;
        }
        i = i.checked_add(2 + len)?;
    }
}

/// `N` bytes at `at`, or None when out of range.
fn arr<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    let end = at.checked_add(N)?;
    <[u8; N]>::try_from(b.get(at..end)?).ok()
}

// ---------------------------------------------------------------------------------------------
// Waveforms.
// ---------------------------------------------------------------------------------------------

/// Serialises the disk-heavy background jobs (waveform decodes read a whole recording, thumbnails
/// seek through one) so several of them never hammer the disk together, and drops the thread to
/// background I/O and CPU priority while one runs so the rest of the system stays responsive.
static HEAVY_IO: Mutex<()> = Mutex::new(());

/// Holds the heavy-job lock and the background priority mode until dropped.
struct HeavyIo {
    _lock: MutexGuard<'static, ()>,
}

impl HeavyIo {
    fn enter() -> HeavyIo {
        let lock = HEAVY_IO.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        // SAFETY: plain Win32 calls on the current thread; failure just keeps normal priority.
        unsafe {
            use windows::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            };
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
        HeavyIo { _lock: lock }
    }
}

impl Drop for HeavyIo {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: as in `enter`; the pooled thread returns to normal priority for its next job.
        unsafe {
            use windows::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_END,
            };
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END);
        }
    }
}

/// Most peak results kept in memory (cleared when a new key would exceed this).
const PEAKS_MEM_MAX: usize = 256;
/// Most decodes of one cache file per process; further attempts fail instead of decoding again.
const MAX_COMPUTES_PER_KEY: u32 = 2;

/// Peak results of this process keyed by cache file path. Holds results even when the disk write
/// failed, so a cache miss on disk never triggers a second decode of the same file.
static PEAKS_MEM: Mutex<Option<HashMap<PathBuf, Vec<f32>>>> = Mutex::new(None);

/// Number of decodes started per cache file path in this process.
static COMPUTES: Mutex<Option<HashMap<PathBuf, u32>>> = Mutex::new(None);

/// Peaks held in memory for a cache file, if any.
fn peaks_mem_get(file: &Path) -> Option<Vec<f32>> {
    let guard = PEAKS_MEM.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref()?.get(file).cloned()
}

/// Keeps peaks in memory for a cache file, clearing the map first when it is full.
fn peaks_mem_put(file: &Path, peaks: Vec<f32>) {
    let mut guard = PEAKS_MEM.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= PEAKS_MEM_MAX && !map.contains_key(file) {
        map.clear();
    }
    map.insert(file.to_path_buf(), peaks);
}

/// Counts one decode of a cache file. Errors once the file has been decoded more than
/// `MAX_COMPUTES_PER_KEY` times in this process (a runaway guard).
fn count_compute(file: &Path) -> Result<(), String> {
    let mut guard = COMPUTES.lock().unwrap_or_else(|e| e.into_inner());
    let count = guard
        .get_or_insert_with(HashMap::new)
        .entry(file.to_path_buf())
        .or_insert(0);
    *count = count.saturating_add(1);
    if *count > MAX_COMPUTES_PER_KEY {
        return Err("waveform unavailable".to_string());
    }
    Ok(())
}

/// Returns the peaks of one audio stream, decoding the file only when no result is known yet.
/// Order: memory, then the cache file, then a decode (stored in memory and written to disk; a
/// failed write is logged and does not fail the call). The flag is false only when the result
/// was already in memory, so the caller emits `peaks-ready` only when the UI has not seen it yet.
fn compute_peaks(
    path: &Path,
    cache_file: &Path,
    stream: u32,
    buckets: usize,
) -> Result<(Vec<f32>, bool), String> {
    let _heavy = HeavyIo::enter();
    // Another job may have produced the result while this one waited for its turn.
    if let Some(done) = peaks_mem_get(cache_file) {
        return Ok((done, false));
    }
    if let Some(done) = read_peaks(cache_file, buckets) {
        peaks_mem_put(cache_file, done.clone());
        return Ok((done, true));
    }
    count_compute(cache_file)?;
    let (src, src_stream) = decode_source_locked(path, stream);
    let (peaks, _secs) = clipcheck_mfdec::audio::decode_peaks(&src, src_stream, buckets)
        .map_err(|e| e.to_string())?;
    peaks_mem_put(cache_file, peaks.clone());
    if let Err(e) = write_peaks(cache_file, &peaks) {
        eprintln!(
            "editor waveform: cannot write peaks cache {}: {e}",
            cache_file.display()
        );
    }
    Ok((peaks, true))
}

/// FNV-1a 64-bit hash. Unlike `DefaultHasher` its value is fixed across Rust versions, so
/// on-disk cache names stay valid after a toolchain upgrade.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes
        .iter()
        .fold(OFFSET_BASIS, |h, b| (h ^ u64::from(*b)).wrapping_mul(PRIME))
}

/// Short hex tag of a source file's canonical path (case-folded), so two files with the same size
/// and modification time never share a cache entry.
fn source_tag(path: &Path) -> String {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canon.to_string_lossy().to_ascii_lowercase();
    format!("{:016x}", fnv1a64(text.as_bytes()))[..12].to_string()
}

/// Peaks cache file of one stream of `path`: the `paths::peaks_file` name with the source tag
/// appended to the stem, so the key includes the file's path as well as its size and mtime.
fn peaks_cache_file(
    cache: &Path,
    path: &Path,
    meta: &fs::Metadata,
    stream: u32,
    buckets: usize,
) -> PathBuf {
    let base = paths::peaks_file(cache, meta.len(), mtime_ms(meta), stream, buckets);
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    base.with_file_name(format!("{stem}-{}.pk", source_tag(path)))
}

/// Cache files whose background waveform computation is running (keyed by cache file path).
static PENDING: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Marks a cache file as being computed. False when it already is.
fn claim_pending(file: &Path) -> bool {
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashSet::new)
        .insert(file.to_path_buf())
}

/// Clears a cache file's pending mark when dropped.
struct PendingGuard(PathBuf);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = guard.as_mut() {
            set.remove(&self.0);
        }
    }
}

/// Reads a peaks cache file; None when it is missing, has another length, or holds bad values.
fn read_peaks(file: &Path, buckets: usize) -> Option<Vec<f32>> {
    let bytes = fs::read(file).ok()?;
    if bytes.len() != buckets.checked_mul(4)? {
        return None;
    }
    let (quads, _rest) = bytes.as_chunks::<4>();
    let values: Vec<f32> = quads.iter().map(|c| f32::from_le_bytes(*c)).collect();
    if values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(values)
}

/// Writes peaks as little-endian f32 values, atomically (`.part` then rename).
fn write_peaks(file: &Path, peaks: &[f32]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut bytes = Vec::with_capacity(peaks.len() * 4);
    for p in peaks {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    let part = PathBuf::from(format!("{}.part", file.display()));
    fs::write(&part, &bytes).map_err(|e| e.to_string())?;
    fs::rename(&part, file).map_err(|e| {
        let _ = fs::remove_file(&part);
        e.to_string()
    })
}

// ---------------------------------------------------------------------------------------------
// Thumbnails.
// ---------------------------------------------------------------------------------------------

/// Key of the in-memory thumbnail cache: media path, time in ms, pixel height, file size and
/// modification time (ms). The last two make an overwritten file miss instead of serving old frames.
type ThumbKey = (String, i64, u32, u64, i64);

/// Bounded in-memory cache of data-URI thumbnails, least recently used first.
#[derive(Default)]
struct ThumbCache {
    map: HashMap<ThumbKey, String>,
    order: VecDeque<ThumbKey>,
}

/// The process-wide thumbnail cache.
static THUMBS: Mutex<Option<ThumbCache>> = Mutex::new(None);

/// Looks a thumbnail up and marks it most recently used.
fn thumb_get(key: &ThumbKey) -> Option<String> {
    let mut guard = THUMBS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(ThumbCache::default);
    let value = cache.map.get(key).cloned()?;
    cache.order.retain(|k| k != key);
    cache.order.push_back(key.clone());
    Some(value)
}

/// Stores a thumbnail, evicting the least recently used entries past the limit.
fn thumb_put(key: ThumbKey, value: String) {
    let mut guard = THUMBS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(ThumbCache::default);
    cache.order.retain(|k| k != &key);
    cache.order.push_back(key.clone());
    cache.map.insert(key, value);
    while cache.map.len() > THUMB_CACHE_MAX {
        let Some(old) = cache.order.pop_front() else {
            break;
        };
        cache.map.remove(&old);
    }
}

/// Computes the thumbnails of one media entry (blocking). `still_open` reports whether the
/// project the request was made for is still open. When it is not, the result is an empty list:
/// the UI treats missing entries as placeholders and does not mark the media as failed.
fn thumbnails_blocking(
    media: &MediaRef,
    times_ms: &[i64],
    height: u32,
    still_open: &dyn Fn() -> bool,
) -> Result<Vec<String>, String> {
    let path = Path::new(&media.path);
    let meta = fs::metadata(path).map_err(|_| format!("file is missing: {}", media.path))?;

    if media.kind == MediaKind::Image {
        // A still image has one frame: one entry, not a copy of its data URI per time.
        return Ok(vec![image_data_uri(path)?]);
    }
    if media.width == 0 || media.height == 0 {
        return Err("the media has no video frames".to_string());
    }
    let width = (f64::from(height) * f64::from(media.width) / f64::from(media.height))
        .round()
        .max(2.0) as u32;

    let dir = cache_root().join("thumbs");
    let mtime = mtime_ms(&meta);
    let size = meta.len();
    // One heavy-job lock for the whole call, not one per frame.
    let _heavy = HeavyIo::enter();
    let mut out = Vec::with_capacity(times_ms.len());
    let mut decodes = 0usize;
    let mut decoded_ok = 0usize;
    for &ms in times_ms {
        if !still_open() {
            return Ok(Vec::new());
        }
        let ms = ms.max(0);
        let key: ThumbKey = (media.path.clone(), ms, height, size, mtime);
        if let Some(hit) = thumb_get(&key) {
            out.push(hit);
            continue;
        }
        // One failing time (for example one past the end) leaves an empty slot; the rest still come.
        let disk = dir.join(disk_name(&media.path, meta.len(), mtime, ms, height));
        let cached = fs::metadata(&disk).map(|m| m.len() > 0).unwrap_or(false);
        let mut failed = false;
        if !cached {
            decodes += 1;
            if clipcheck_mfdec::thumbnail_jpeg(path, ms as f64 / 1000.0, width, &disk).is_ok() {
                decoded_ok += 1;
            } else {
                failed = true;
            }
        }
        if failed {
            let _ = fs::remove_file(&disk);
            // The decoder cannot handle this source: stop instead of retrying every time.
            if thumb_decoder_gave_up(decodes, decoded_ok) {
                return Err("thumbnails unavailable for this media".to_string());
            }
            out.push(String::new());
            continue;
        }
        let Ok(bytes) = fs::read(&disk) else {
            out.push(String::new());
            continue;
        };
        let uri = format!("data:image/jpeg;base64,{}", base64_encode(&bytes));
        thumb_put(key, uri.clone());
        out.push(uri);
    }
    Ok(out)
}

/// Data URI of a small image file (its own bytes, with the image's MIME type).
fn image_data_uri(path: &Path) -> Result<String, String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_IMAGE_THUMB_BYTES {
        return Err("image is too large for a thumbnail (over 1 MB)".to_string());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let (_codec, mime, _w, _h) =
        image_header(&bytes).ok_or_else(|| "cannot read the image header".to_string())?;
    Ok(format!("data:{mime};base64,{}", base64_encode(&bytes)))
}

/// Cache file name for one thumbnail: a hash of the file identity, time and height. Uses FNV-1a
/// over fixed-width fields, so names stay the same across Rust versions.
fn disk_name(path: &str, size: u64, mtime: i64, ms: i64, height: u32) -> String {
    let mut buf = path.to_ascii_lowercase().into_bytes();
    buf.push(0);
    buf.extend_from_slice(&size.to_le_bytes());
    buf.extend_from_slice(&mtime.to_le_bytes());
    buf.extend_from_slice(&ms.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    format!("{:016x}.jpg", fnv1a64(&buf))
}

/// Frame decodes in one thumbnail call that must all fail (with none succeeding) before the
/// source is treated as undecodable.
const THUMB_GIVE_UP_AFTER: usize = 3;

/// True when the decoder should be given up on: `decodes` frame decodes were attempted in this
/// call, `successes` of them worked, and the first `THUMB_GIVE_UP_AFTER` all failed.
fn thumb_decoder_gave_up(decodes: usize, successes: usize) -> bool {
    decodes >= THUMB_GIVE_UP_AFTER && successes == 0
}

/// Standard base64 with padding.
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(base64_symbol(n >> 18));
        out.push(base64_symbol(n >> 12));
        out.push(if chunk.len() > 1 {
            base64_symbol(n >> 6)
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            base64_symbol(n)
        } else {
            '='
        });
    }
    out
}

/// One base64 alphabet symbol for the low 6 bits of `v`.
fn base64_symbol(v: u32) -> char {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    ALPHABET
        .get((v & 63) as usize)
        .map_or('=', |b| char::from(*b))
}

// ---------------------------------------------------------------------------------------------
// Preview URLs and loudness.
// ---------------------------------------------------------------------------------------------

/// URL the webview plays for a media entry: the recording route for library media, a registered
/// import URL otherwise.
fn media_url_for(media: &MediaRef) -> Result<String, String> {
    if let Some(id) = &media.library_recording_id {
        if ext_lower(Path::new(&media.path)) == "mkv" {
            return Err(
                "MKV (Matroska) cannot be previewed: WebView2 cannot play it and no proxy exists"
                    .to_string(),
            );
        }
        return Ok(match mediaserver::start() {
            Some(server) => format!("{}/rec/{id}", server.base_url()),
            None => format!("http://clip.localhost/rec/{id}"),
        });
    }
    mediaserver::register_extra_path(Path::new(&media.path)).ok_or_else(|| {
        format!(
            "{} cannot be served: it is not a media file or the media server is not running",
            media.path
        )
    })
}

/// Copies audio stream `stream` (index among the audio tracks) of an MP4/MOV/M4V file into the
/// audio cache with a lossless remux, and returns the cached file. A non-empty cached file is
/// reused. Runs under `HeavyIo`, which serialises heavy jobs, so concurrent requests for the same
/// stream wait for the first one and then find its output in the cache.
fn extract_audio_stream(path: &Path, stream: u32) -> Result<PathBuf, String> {
    let _heavy = HeavyIo::enter();
    extract_audio_stream_locked(path, stream)
}

/// Where audio stream `stream` of `path` can be decoded unambiguously by Media Foundation: for an
/// MP4/MOV/M4V with several audio tracks, its single-track extraction (stream 0 of that file),
/// so the decoded data is exactly the track the editor plays and never depends on how MF orders or
/// exposes multi-track MP4s; otherwise the file itself. Caller must hold `HeavyIo`.
fn decode_source_locked(path: &Path, stream: u32) -> (PathBuf, u32) {
    if matches!(ext_lower(path).as_str(), "mp4" | "m4v" | "mov") {
        let multi = clipcheck_mp4::reader::read_movie(path)
            .map(|m| m.tracks.iter().filter(|t| t.is_audio()).count() > 1)
            .unwrap_or(false);
        if multi {
            if let Ok(single) = extract_audio_stream_locked(path, stream) {
                return (single, 0);
            }
        }
    }
    (path.to_path_buf(), stream)
}

/// `extract_audio_stream` for callers already holding the `HeavyIo` lock (it is not reentrant).
fn extract_audio_stream_locked(path: &Path, stream: u32) -> Result<PathBuf, String> {
    if !matches!(ext_lower(path).as_str(), "mp4" | "m4v" | "mov") {
        return Err("audio track extraction needs an MP4 file".to_string());
    }
    let meta = fs::metadata(path).map_err(|_| format!("file is missing: {}", path.display()))?;
    let out = cache_root().join("audio").join(format!(
        "{}-{}-{stream}-{}.m4a",
        meta.len(),
        mtime_ms(&meta),
        source_tag(path)
    ));
    let cached = || fs::metadata(&out).map(|m| m.len() > 0).unwrap_or(false);
    if cached() {
        return Ok(out);
    }
    let movie = clipcheck_mp4::reader::read_movie(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // Track index in the movie's full track list (the remux selects by that index).
    let track = movie
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio())
        .nth(stream as usize)
        .map(|(i, _)| i)
        .ok_or_else(|| format!("no audio stream {stream} in this media"))?;
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let opts = clipcheck_mp4::remux::RemuxOptions {
        start_secs: 0.0,
        end_secs: None,
        tracks: clipcheck_mp4::remux::TrackSelect::Only(vec![track]),
        faststart: true,
    };
    clipcheck_mp4::remux::concat_remux(&[path.to_path_buf()], &out, &opts)
        .map_err(|e| format!("cannot extract audio stream {stream}: {e}"))?;
    Ok(out)
}

/// Integrated loudness and peak of a clip's source range (stereo mix of the stream, at most ten
/// minutes). Speed is not applied: it changes timing, not the measured level.
fn measure_loudness(
    path: &Path,
    stream: u32,
    src_in: Tick,
    src_len: Tick,
) -> Result<LoudnessReport, String> {
    // Held for the whole measurement (open, seek and every read), so the decode is serialised
    // with the other heavy jobs and runs at background priority.
    let _heavy = HeavyIo::enter();
    let (src, src_stream) = decode_source_locked(path, stream);
    let mut dec = AudioDecoder::open(&src, src_stream).map_err(|e| e.to_string())?;
    let rate = dec.sample_rate().max(1);
    let channels = dec.channels().max(1) as usize;
    let start_secs = src_in.as_secs_f64().max(0.0);
    let len_secs = src_len.as_secs_f64().clamp(0.0, MEASURE_MAX_SECS);
    let end_frame = (len_secs * f64::from(rate)).round() as i64;
    if start_secs > 0.0 {
        dec.seek(start_secs).map_err(|e| e.to_string())?;
    }

    let mut meter = LoudnessMeter::new(rate, 2);
    let mut raw: Vec<f32> = Vec::new();
    let mut stereo: Vec<f32> = Vec::new();
    let mut peak = f64::NEG_INFINITY;
    let mut frames_used: u64 = 0;
    loop {
        raw.clear();
        let Some((_, ts)) = dec
            .read(MEASURE_READ_FRAMES, &mut raw)
            .map_err(|e| e.to_string())?
        else {
            break;
        };
        // Frame offset of this read from the start of the range (the seek may land a little early).
        let first = (((ts as f64) / 1e7 - start_secs) * f64::from(rate)).round() as i64;
        stereo.clear();
        let mut done = false;
        for (i, frame) in raw.chunks_exact(channels).enumerate() {
            let rel = first.saturating_add(i as i64);
            if rel < 0 {
                continue;
            }
            if rel >= end_frame {
                done = true;
                break;
            }
            let left = frame.first().copied().unwrap_or(0.0);
            let right = if channels > 1 {
                frame.get(1).copied().unwrap_or(left)
            } else {
                left
            };
            stereo.push(left);
            stereo.push(right);
        }
        if !stereo.is_empty() {
            meter.push(&stereo);
            peak = peak.max(peak_db(&stereo));
            frames_used += (stereo.len() / 2) as u64;
        }
        if done {
            break;
        }
    }
    if frames_used == 0 {
        return Err("the clip's source range has no audio".to_string());
    }
    Ok(LoudnessReport {
        lufs: meter.integrated_lufs(),
        peak_db: peak,
    })
}

// ---------------------------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------------------------

/// Editor cache folder from the settings (`cache_dir`, or `<editor_root>\cache`).
fn cache_root() -> PathBuf {
    let root = paths::editor_root(std::env::var("LOCALAPPDATA").ok().as_deref());
    let cfg = settings::load(&settings::settings_path());
    paths::cache_dir(&root, &cfg.cache_dir)
}

/// Lower-cased extension without the dot ("" when none).
fn ext_lower(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Modified time in ms since the Unix epoch (0 when unknown).
fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Seconds to ticks; zero for NaN, infinities and negative values.
fn tick_from_secs(secs: f64) -> Tick {
    if secs.is_finite() && secs > 0.0 {
        Tick::from_secs_f64(secs)
    } else {
        Tick::ZERO
    }
}

/// Milliseconds to ticks; negative values give zero.
fn tick_from_ms(ms: i64) -> Tick {
    Tick::from_ms(ms.max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn png_header_size() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_header(&png), Some(("png", "image/png", 640, 480)));
    }

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn thumb_give_up_after_three_failed_decodes() {
        assert!(!thumb_decoder_gave_up(0, 0));
        assert!(!thumb_decoder_gave_up(1, 0));
        assert!(!thumb_decoder_gave_up(2, 0));
        assert!(thumb_decoder_gave_up(3, 0));
        assert!(thumb_decoder_gave_up(4, 0));
        // A single working frame keeps the source alive.
        assert!(!thumb_decoder_gave_up(3, 1));
        assert!(!thumb_decoder_gave_up(5, 2));
    }

    #[test]
    fn disk_name_is_stable_and_case_insensitive() {
        let a = disk_name(r"C:\Clips\A.mp4", 10, 20, 30, 90);
        assert_eq!(a, disk_name(r"c:\clips\a.mp4", 10, 20, 30, 90));
        assert_ne!(a, disk_name(r"C:\Clips\A.mp4", 11, 20, 30, 90));
        assert_ne!(a, disk_name(r"C:\Clips\A.mp4", 10, 20, 31, 90));
        assert_ne!(a, disk_name(r"C:\Clips\A.mp4", 10, 20, 30, 91));
        assert!(a.ends_with(".jpg"));
        assert_eq!(a.len(), 16 + 4);
    }

    #[test]
    fn source_tag_differs_per_path() {
        let a = source_tag(Path::new(r"C:\cache-test\a\clip.mp4"));
        let b = source_tag(Path::new(r"C:\cache-test\b\clip.mp4"));
        assert_eq!(a.len(), 12);
        assert_ne!(a, b);
        assert_eq!(a, source_tag(Path::new(r"C:\cache-test\a\clip.mp4")));
    }
}
