//! Player backend: probes a recording and extracts one audio sidecar per track.
//! Contract: docs/impl/player-mvp.md. Since D58 probing and extraction run in-process
//! (clipcheck-mp4 for MP4 family files, clipcheck-mfdec for everything else).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use clipcheck_library::Library;
use clipcheck_mp4::remux::{concat_remux, RemuxOptions, TrackSelect};
use serde::Serialize;

use crate::media;

/// One audio track of a recording.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrackDto {
    pub index: u32,
    pub title: String,
    pub codec: String,
    pub channels: u32,
    pub sample_rate: u32,
    pub url: Option<String>,
}

/// What the player needs to open a recording.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlayerOpenDto {
    pub id: String,
    pub video_url: String,
    pub duration_ms: i64,
    pub has_video: bool,
    pub playable: bool,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub tracks: Vec<AudioTrackDto>,
    pub mixed: bool,
    pub audio_error: Option<String>,
    /// True when two or more audio tracks still need `prepare_audio` (sidecars not checked yet).
    pub audio_pending: bool,
}

/// Result of `prepare_audio` (D60): the sidecar-backed track list.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AudioPrepDto {
    pub tracks: Vec<AudioTrackDto>,
    pub mixed: bool,
    pub audio_error: Option<String>,
}

/// Probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeInfo {
    pub duration_ms: i64,
    pub has_video: bool,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub audio: Vec<ProbeAudio>,
}

/// One probed audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAudio {
    pub index: u32,
    pub codec: String,
    pub channels: u32,
    pub sample_rate: u32,
    pub title: String,
}

/// Player failures.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("{0}")]
    Media(#[from] crate::media::MediaError),
    #[error("probe: {0}")]
    Probe(String),
    #[error("io: {0}")]
    Io(String),
}

/// Cache directory: env override, else `%LOCALAPPDATA%\ClipCheck\cache`, else the temp dir.
pub fn cache_dir_from(env_value: Option<&str>, local_app_data: Option<&str>) -> PathBuf {
    let nonblank = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(dir) = nonblank(env_value) {
        return PathBuf::from(dir);
    }
    if let Some(local) = nonblank(local_app_data) {
        return PathBuf::from(local).join("ClipCheck").join("cache");
    }
    std::env::temp_dir().join("ClipCheck").join("cache")
}

/// Duration from the library row, 0 if unavailable.
fn db_duration_ms(db: &Path, id: &str) -> i64 {
    Library::open_readonly(db)
        .ok()
        .and_then(|lib| lib.get_recording(id).ok().flatten())
        .map_or(0, |row| row.duration_ms)
}

/// Milliseconds from seconds; 0 for non-finite or negative values.
fn ms_from_secs(secs: f64) -> i64 {
    if secs.is_finite() && secs > 0.0 {
        (secs * 1000.0).round() as i64
    } else {
        0
    }
}

/// Probe result plus, per audio ordinal, the track's index in the file (MP4 only;
/// empty when the file was probed through Media Foundation).
#[derive(Clone)]
struct Probed {
    info: ProbeInfo,
    audio_track_index: Vec<usize>,
}

/// Probes an MP4 family file from its parsed movie.
fn probe_from_movie(movie: &clipcheck_mp4::reader::Movie) -> Probed {
    use clipcheck_mp4::reader::TrackKind;

    let mut video: Option<(String, u32, u32)> = None;
    let mut audio = Vec::new();
    let mut audio_track_index = Vec::new();
    for (i, track) in movie.tracks.iter().enumerate() {
        match &track.kind {
            TrackKind::Video {
                codec,
                width,
                height,
            } if video.is_none() => {
                video = Some((codec.clone(), *width, *height));
            }
            TrackKind::Audio {
                codec,
                channels,
                sample_rate,
            } => {
                let index = audio.len() as u32;
                let title = track
                    .title
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .to_string();
                audio.push(ProbeAudio {
                    index,
                    codec: codec.clone(),
                    channels: u32::from(*channels),
                    sample_rate: *sample_rate,
                    title,
                });
                audio_track_index.push(i);
            }
            _ => {}
        }
    }
    let has_video = video.is_some();
    let (video_codec, width, height) = video.unwrap_or_default();
    Probed {
        info: ProbeInfo {
            duration_ms: ms_from_secs(movie.duration_secs),
            has_video,
            width,
            height,
            video_codec,
            audio,
        },
        audio_track_index,
    }
}

/// Probes any file with Media Foundation (no per-track titles).
#[cfg(windows)]
fn probe_with_mfdec(path: &Path) -> Result<Probed, PlayerError> {
    let p = clipcheck_mfdec::probe(path).map_err(|e| PlayerError::Probe(e.to_string()))?;
    let audio = p
        .audio
        .iter()
        .enumerate()
        .map(|(i, a)| ProbeAudio {
            index: i as u32,
            codec: a.codec.clone(),
            channels: a.channels,
            sample_rate: a.sample_rate,
            title: String::new(),
        })
        .collect();
    Ok(Probed {
        info: ProbeInfo {
            duration_ms: p.duration_ms.max(0),
            has_video: p.width > 0 || !p.video_codec.is_empty(),
            width: p.width,
            height: p.height,
            video_codec: p.video_codec,
            audio,
        },
        audio_track_index: Vec::new(),
    })
}

#[cfg(not(windows))]
fn probe_with_mfdec(_path: &Path) -> Result<Probed, PlayerError> {
    Err(PlayerError::Probe("media probing needs Windows".into()))
}

/// Probes a recording: MP4 family files through clipcheck-mp4, falling back to
/// clipcheck-mfdec for other containers or when the MP4 reader fails.
fn probe_file(path: &Path) -> Result<Probed, PlayerError> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "mp4" | "m4v" | "mov") {
        return match clipcheck_mp4::reader::read_movie(path) {
            Ok(movie) => Ok(probe_from_movie(&movie)),
            Err(reader_err) => probe_with_mfdec(path)
                .map_err(|e| PlayerError::Probe(format!("mp4 reader: {reader_err}; {e}"))),
        };
    }
    probe_with_mfdec(path)
}

/// Makes sure the sidecar for audio track `n` exists (reusing a non-empty cached file).
/// `file_track` is the track's index in the file's full track list.
fn ensure_sidecar(
    src: &Path,
    sidecar: &Path,
    file_track: usize,
    n: u32,
    codec: &str,
) -> Result<(), String> {
    if fs::metadata(sidecar).map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(());
    }
    if codec != "aac" {
        return Err(format!(
            "track {n}: codec {codec} cannot be played without conversion"
        ));
    }
    if let Some(parent) = sidecar.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {e}"))?;
    }
    let tmp = PathBuf::from(format!("{}.part", sidecar.display()));
    let _ = fs::remove_file(&tmp);

    let opts = RemuxOptions {
        start_secs: 0.0,
        end_secs: None,
        tracks: TrackSelect::Only(vec![file_track]),
        faststart: true,
    };
    if let Err(e) = concat_remux(&[src.to_path_buf()], &tmp, &opts) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("extract track {n}: {e}"));
    }
    let ok = fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false);
    if !ok {
        let _ = fs::remove_file(&tmp);
        return Err(format!("extract track {n}: no output written"));
    }
    if let Err(e) = fs::rename(&tmp, sidecar) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("finalize track {n}: {e}"));
    }
    Ok(())
}

/// File size and modification time in ms: the sidecar cache key and the probe cache check.
fn file_stamp(path: &Path) -> Result<(u64, i64), PlayerError> {
    let meta = fs::metadata(path).map_err(|e| PlayerError::Io(e.to_string()))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    Ok((meta.len(), mtime_ms))
}

/// Most probe results kept; the cache is cleared when a new id would grow it past this.
const PROBE_CACHE_MAX: usize = 64;

/// Probe results keyed by recording id, with the file size and mtime they were taken at.
type ProbeCache = HashMap<String, (u64, i64, Probed)>;

/// The process-wide probe cache.
static PROBE_CACHE: OnceLock<Mutex<ProbeCache>> = OnceLock::new();

/// Per-id locks so two `prepare_audio` calls for one recording never run two remuxes.
static PREP_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

/// Locks a std mutex, recovering the data if an earlier holder panicked.
fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Probes the recording, reusing the cached result while size and mtime are unchanged.
fn probe_cached(path: &Path, id: &str, size: u64, mtime_ms: i64) -> Result<Probed, PlayerError> {
    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let map = lock_or_recover(cache);
        if let Some((s, m, probed)) = map.get(id) {
            if *s == size && *m == mtime_ms {
                return Ok(probed.clone());
            }
        }
    }
    let probed = probe_file(path)?;
    let mut map = lock_or_recover(cache);
    if map.len() >= PROBE_CACHE_MAX && !map.contains_key(id) {
        map.clear();
    }
    map.insert(id.to_string(), (size, mtime_ms, probed.clone()));
    Ok(probed)
}

/// Audio tracks as probed: no URLs, not mixed.
fn base_tracks(info: &ProbeInfo) -> Vec<AudioTrackDto> {
    info.audio
        .iter()
        .map(|a| AudioTrackDto {
            index: a.index,
            title: a.title.clone(),
            codec: a.codec.clone(),
            channels: a.channels,
            sample_rate: a.sample_rate,
            url: None,
        })
        .collect()
}

/// The per-id lock used by `prepare_audio_at`.
fn prep_lock(id: &str) -> Arc<Mutex<()>> {
    let locks = PREP_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    lock_or_recover(locks)
        .entry(id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Opens a recording for playback by probing only; no audio is extracted. Tracks have no
/// URLs and `mixed` is false. `audio_pending` is true when the recording has two or more
/// audio tracks that `prepare_audio_at` still has to turn into sidecars.
pub fn open_fast_at(db: &Path, id: &str) -> Result<PlayerOpenDto, PlayerError> {
    let path = media::resolve_recording_path(db, id)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let playable = ext != "mkv";
    let video_url = format!("http://clip.localhost/rec/{id}");

    let (size, mtime_ms) = file_stamp(&path)?;
    let probed = probe_cached(&path, id, size, mtime_ms)?;
    let info = &probed.info;

    let duration_ms = if info.duration_ms > 0 {
        info.duration_ms
    } else {
        db_duration_ms(db, id)
    };
    let audio_pending = info.audio.len() >= 2 && probed.audio_track_index.len() == info.audio.len();

    Ok(PlayerOpenDto {
        id: id.to_string(),
        video_url,
        duration_ms,
        has_video: info.has_video,
        playable,
        width: info.width,
        height: info.height,
        video_codec: info.video_codec.clone(),
        tracks: base_tracks(info),
        mixed: false,
        audio_error: None,
        audio_pending,
    })
}

/// Extracts (or reuses) one audio sidecar per track of a recording with two or more audio
/// tracks. Concurrent calls for the same id are serialized, so the later call finds the
/// sidecars already cached. Fewer tracks: the probed tracks, `mixed` false, no error.
pub fn prepare_audio_at(
    db: &Path,
    cache_dir: &Path,
    id: &str,
) -> Result<AudioPrepDto, PlayerError> {
    let path = media::resolve_recording_path(db, id)?;
    let (size, mtime_ms) = file_stamp(&path)?;
    let probed = probe_cached(&path, id, size, mtime_ms)?;
    let info = &probed.info;
    let mut tracks = base_tracks(info);
    if info.audio.len() < 2 {
        return Ok(AudioPrepDto {
            tracks,
            mixed: false,
            audio_error: None,
        });
    }

    let lock = prep_lock(id);
    let _guard = lock_or_recover(&lock);
    let mut failure = None;
    if probed.audio_track_index.len() != info.audio.len() {
        failure = Some("multiple audio tracks need an mp4 container".to_string());
    } else {
        for (a, &file_track) in info.audio.iter().zip(&probed.audio_track_index) {
            let sidecar = media::sidecar_path(cache_dir, id, size, mtime_ms, a.index);
            if let Err(msg) = ensure_sidecar(&path, &sidecar, file_track, a.index, &a.codec) {
                failure = Some(msg);
                break;
            }
        }
    }
    match failure {
        None => {
            for t in &mut tracks {
                t.url = Some(format!("http://clip.localhost/aud/{id}/{}", t.index));
            }
            Ok(AudioPrepDto {
                tracks,
                mixed: true,
                audio_error: None,
            })
        }
        Some(msg) => Ok(AudioPrepDto {
            tracks,
            mixed: false,
            audio_error: Some(msg),
        }),
    }
}

/// Opens a recording with its audio sidecars: `open_fast_at` merged with `prepare_audio_at`.
pub fn open_at(db: &Path, cache_dir: &Path, id: &str) -> Result<PlayerOpenDto, PlayerError> {
    let mut dto = open_fast_at(db, id)?;
    let prep = prepare_audio_at(db, cache_dir, id)?;
    dto.tracks = prep.tracks;
    dto.mixed = prep.mixed;
    dto.audio_error = prep.audio_error;
    dto.audio_pending = false;
    Ok(dto)
}

/// Tauri command: probes a recording for playback. Audio sidecars come from `prepare_audio`.
#[tauri::command]
pub async fn open_player(id: String) -> Result<PlayerOpenDto, String> {
    let db = crate::commands::try_default_db_path().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || open_fast_at(&db, &id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// Tauri command: extracts (or reuses) the audio sidecars of a multi-track recording.
#[tauri::command]
pub async fn prepare_audio(id: String) -> Result<AudioPrepDto, String> {
    let db = crate::commands::try_default_db_path().map_err(|e| e.to_string())?;
    let cache = cache_dir_from(
        std::env::var("CLIPCHECK_CACHE_DIR").ok().as_deref(),
        std::env::var("LOCALAPPDATA").ok().as_deref(),
    );
    tauri::async_runtime::spawn_blocking(move || {
        prepare_audio_at(&db, &cache, &id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_precedence() {
        assert_eq!(
            cache_dir_from(Some("D:\\c"), Some("C:\\L")),
            PathBuf::from("D:\\c")
        );
        assert_eq!(
            cache_dir_from(Some("  "), Some("C:\\L")),
            PathBuf::from("C:\\L").join("ClipCheck").join("cache")
        );
        assert_eq!(
            cache_dir_from(None, Some("")),
            std::env::temp_dir().join("ClipCheck").join("cache")
        );
    }
}
