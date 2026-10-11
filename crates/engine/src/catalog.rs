//! Media probing, poster thumbnails and library registration for finished recordings
//! (07 §5.10 and §5.11). Probing and thumbnails run in-process: the MP4 reader for MP4/M4V/MOV
//! files, and the Media Foundation decoder on Windows for everything else. No child processes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use clipcheck_library::db::RecordingSource;
use clipcheck_library::schema::now_ms;
use clipcheck_library::{
    AudioTrackRow, LibError, Library, NewGame, NewRecording, RecStatus, RecordingKind,
};
use clipcheck_mp4::reader::{Movie, TrackKind};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Errors returned by [`register_recording`].
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The library rejected a request.
    #[error("library: {0}")]
    Lib(#[from] LibError),
    /// A helper process could not run, failed, or produced unusable output.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// The file is not inside the storage root it was registered under.
    #[error("{} is not under storage root {}", .file.display(), .root.display())]
    NotUnderRoot {
        /// The file that was offered.
        file: PathBuf,
        /// The storage root it must live under.
        root: PathBuf,
    },
    /// The file extension is not one the library accepts (`mp4`, `m4v`, `mov` or `mkv`).
    #[error("unsupported container extension '{0}'")]
    UnsupportedExt(String),
}

/// One audio stream of a probed file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrackInfo {
    /// 0-based ordinal among the file's audio streams (not the container stream index).
    pub index: u32,
    /// Codec name as reported by the MP4 reader or Media Foundation (`aac`, `opus`, ...); empty if unknown.
    pub codec: String,
    /// Channel count; 0 if unknown.
    pub channels: u32,
    /// Sample rate in Hz; 0 if unknown.
    pub sample_rate: u32,
    /// Stream title tag, if present.
    pub title: Option<String>,
}

/// Header-level facts about a media file, read in process (MP4 reader or Media Foundation).
///
/// Missing fields fall back to zero or empty values instead of failing the probe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    /// Container duration in milliseconds; 0 if unknown.
    pub duration_ms: i64,
    /// File size in bytes, from the file system.
    pub size_bytes: u64,
    /// First video stream width in pixels; 0 if there is no video.
    pub width: u32,
    /// First video stream height in pixels; 0 if there is no video.
    pub height: u32,
    /// Average (or, failing that, real) frame rate of the first video stream; 0.0 if unknown.
    pub fps: f32,
    /// Codec name of the first video stream; empty if there is no video.
    pub video_codec: String,
    /// Audio streams in file order.
    pub audio_tracks: Vec<AudioTrackInfo>,
}

/// Metadata supplied by the caller when registering a recording.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisterMeta<'a> {
    /// Clip or session part.
    pub kind: RecordingKind,
    /// Canonical game id (for example `exe:eldenring.exe`); empty for the unknown-game bucket.
    pub game: &'a str,
    /// Session ULID for session parts.
    pub session_id: Option<String>,
    /// 1-based part index for session parts.
    pub part_index: Option<u32>,
    /// Recording start time in UTC milliseconds.
    pub created_ms: i64,
}

/// Probes `file` and returns its header-level metadata, read in-process.
///
/// MP4, M4V and MOV files are read with the built-in MP4 reader. Other containers, and MP4
/// files the reader rejects, fall back to the Media Foundation probe on Windows. The
/// A missing file surfaces as `io::ErrorKind::NotFound`.
pub fn probe_media(file: &Path) -> io::Result<MediaInfo> {
    let size_bytes = fs::metadata(file)?.len();
    let mp4_like = file
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov"));
    let reader_err = if mp4_like {
        match clipcheck_mp4::reader::read_movie(file) {
            Ok(movie) => return Ok(info_from_movie(&movie, size_bytes)),
            Err(e) => Some(e),
        }
    } else {
        None
    };
    probe_fallback(file, size_bytes, reader_err)
}

/// Builds [`MediaInfo`] from a parsed MP4 movie. Missing values fall back to zero or empty.
fn info_from_movie(movie: &Movie, size_bytes: u64) -> MediaInfo {
    let secs = movie.duration_secs;
    let duration_ms = if secs.is_finite() && secs > 0.0 {
        (secs * 1000.0).round() as i64
    } else {
        0
    };
    let mut video = None;
    let mut audio_tracks = Vec::new();
    for track in &movie.tracks {
        match &track.kind {
            TrackKind::Video {
                codec,
                width,
                height,
            } if video.is_none() => video = Some((track, codec.clone(), *width, *height)),
            TrackKind::Audio {
                codec,
                channels,
                sample_rate,
            } => audio_tracks.push(AudioTrackInfo {
                index: audio_tracks.len() as u32,
                codec: codec.clone(),
                channels: u32::from(*channels),
                sample_rate: *sample_rate,
                title: track.title.clone().filter(|t| !t.is_empty()),
            }),
            _ => {}
        }
    }
    let (width, height, fps, video_codec) = match video {
        Some((track, codec, width, height)) => {
            let video_secs = track.duration_secs();
            let fps = if video_secs.is_finite() && video_secs > 0.0 {
                let raw = track.samples.len() as f64 / video_secs;
                ((raw * 1000.0).round() / 1000.0) as f32
            } else {
                0.0
            };
            (width, height, fps, codec)
        }
        None => (0, 0, 0.0, String::new()),
    };
    MediaInfo {
        duration_ms,
        size_bytes,
        width,
        height,
        fps,
        video_codec,
        audio_tracks,
    }
}

/// Probes with Media Foundation (Windows). The MP4 reader error, if any, is added to the
/// message so both failures are visible.
#[cfg(windows)]
fn probe_fallback(
    file: &Path,
    size_bytes: u64,
    reader_err: Option<clipcheck_mp4::Mp4Error>,
) -> io::Result<MediaInfo> {
    let probe = clipcheck_mfdec::probe(file).map_err(|e| {
        let reader = reader_err.map_or_else(String::new, |r| format!("; mp4 reader: {r}"));
        invalid(format!(
            "media probe failed for {}: {e}{reader}",
            file.display()
        ))
    })?;
    let titles = matroska_audio_titles(file);
    Ok(MediaInfo {
        duration_ms: probe.duration_ms,
        size_bytes,
        width: probe.width,
        height: probe.height,
        fps: probe.fps,
        video_codec: probe.video_codec,
        audio_tracks: probe
            .audio
            .into_iter()
            .enumerate()
            .map(|(i, a)| AudioTrackInfo {
                index: i as u32,
                codec: a.codec,
                channels: a.channels,
                sample_rate: a.sample_rate,
                title: titles.get(i).cloned().flatten(),
            })
            .collect(),
    })
}

/// Audio track titles from a Matroska or WebM header, in audio-track order.
///
/// Media Foundation does not report stream titles, so they are read here from the
/// `Tracks/TrackEntry/Name` elements (`TrackType` 2 is audio). Scans only the first 4 MiB,
/// which covers the track list of files written by the encoder. Returns an empty list for
/// anything that is not Matroska or has no readable track list.
#[cfg(windows)]
fn matroska_audio_titles(file: &Path) -> Vec<Option<String>> {
    use std::io::Read;
    let mut head = Vec::new();
    let read = fs::File::open(file).and_then(|f| f.take(4 * 1024 * 1024).read_to_end(&mut head));
    if read.is_err() {
        return Vec::new();
    }
    let mut audio = Vec::new();
    ebml_walk(&head, &mut audio);
    audio
}

/// Reads an EBML variable-length integer at `pos`. Returns `(value, length, is_unknown_size)`.
/// IDs keep their length marker (`keep_marker`); sizes drop it, and all-ones means unknown.
#[cfg(windows)]
fn ebml_vint(buf: &[u8], pos: usize, keep_marker: bool) -> Option<(u64, usize, bool)> {
    let first = *buf.get(pos)?;
    if first == 0 {
        return None;
    }
    let len = first.leading_zeros() as usize + 1;
    let bytes = buf.get(pos..pos + len)?;
    let mut value = if keep_marker {
        u64::from(first)
    } else {
        u64::from(first) & ((1u64 << (8 - len)) - 1)
    };
    for b in &bytes[1..] {
        value = (value << 8) | u64::from(*b);
    }
    let unknown = !keep_marker && value == (1u64 << (7 * len)) - 1;
    Some((value, len, unknown))
}

/// Walks EBML elements in `buf`, collecting the name of each audio `TrackEntry`. Descends
/// into Segment and Tracks, skips other elements, and stops at the first Cluster.
#[cfg(windows)]
fn ebml_walk(buf: &[u8], audio: &mut Vec<Option<String>>) -> Option<()> {
    const SEGMENT: u64 = 0x1853_8067;
    const TRACKS: u64 = 0x1654_AE6B;
    const TRACK_ENTRY: u64 = 0xAE;
    const CLUSTER: u64 = 0x1F43_B675;
    let mut pos = 0;
    while pos < buf.len() {
        let (id, id_len, _) = ebml_vint(buf, pos, true)?;
        let (size, size_len, unknown) = ebml_vint(buf, pos + id_len, false)?;
        let body_start = pos + id_len + size_len;
        let body_end = if unknown {
            buf.len()
        } else {
            body_start
                .checked_add(usize::try_from(size).ok()?)?
                .min(buf.len())
        };
        let body = buf.get(body_start..body_end)?;
        match id {
            SEGMENT | TRACKS => {
                ebml_walk(body, audio);
            }
            CLUSTER => return Some(()),
            TRACK_ENTRY => {
                if let Some((kind, name)) = ebml_track_entry(body) {
                    if kind == 2 {
                        audio.push(name);
                    }
                }
            }
            _ => {}
        }
        pos = body_end;
    }
    Some(())
}

/// Returns `(TrackType, Name)` from the children of one `TrackEntry`.
#[cfg(windows)]
fn ebml_track_entry(buf: &[u8]) -> Option<(u64, Option<String>)> {
    const TRACK_TYPE: u64 = 0x83;
    const NAME: u64 = 0x536E;
    let mut kind = 0;
    let mut name = None;
    let mut pos = 0;
    while pos < buf.len() {
        let (id, id_len, _) = ebml_vint(buf, pos, true)?;
        let (size, size_len, unknown) = ebml_vint(buf, pos + id_len, false)?;
        if unknown {
            return None;
        }
        let body_start = pos + id_len + size_len;
        let body_end = body_start.checked_add(usize::try_from(size).ok()?)?;
        let body = buf.get(body_start..body_end)?;
        match id {
            TRACK_TYPE => kind = body.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)),
            NAME => {
                let text = String::from_utf8_lossy(body)
                    .trim_end_matches('\0')
                    .to_owned();
                name = Some(text).filter(|t| !t.is_empty());
            }
            _ => {}
        }
        pos = body_end;
    }
    Some((kind, name))
}

/// No fallback off Windows: the MP4 reader error is returned, or an unsupported error for
/// containers the reader does not handle.
#[cfg(not(windows))]
fn probe_fallback(
    file: &Path,
    _size_bytes: u64,
    reader_err: Option<clipcheck_mp4::Mp4Error>,
) -> io::Result<MediaInfo> {
    match reader_err {
        Some(clipcheck_mp4::Mp4Error::Io(e)) => Err(e),
        Some(e) => Err(invalid(format!("cannot read {}: {e}", file.display()))),
        None => Err(io::Error::from(io::ErrorKind::Unsupported)),
    }
}

/// Extracts one JPEG frame from `file` at `at_secs` and writes it to `out_jpg`, scaled to
/// `width` pixels wide, using the in-process Media Foundation decoder (Windows).
///
/// Creates the parent directory if needed. On failure any partial output is removed. Off
/// Windows this returns `io::ErrorKind::Unsupported`.
pub fn make_thumbnail(file: &Path, out_jpg: &Path, at_secs: f64, width: u32) -> io::Result<()> {
    if let Some(dir) = out_jpg.parent() {
        fs::create_dir_all(dir)?;
    }
    let t = if at_secs.is_finite() && at_secs > 0.0 {
        at_secs
    } else {
        0.0
    };
    thumbnail_impl(file, out_jpg, t, width)
}

#[cfg(windows)]
fn thumbnail_impl(file: &Path, out_jpg: &Path, at_secs: f64, width: u32) -> io::Result<()> {
    match clipcheck_mfdec::thumbnail_jpeg(file, at_secs, width, out_jpg) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(out_jpg);
            Err(io::Error::other(e.to_string()))
        }
    }
}

#[cfg(not(windows))]
fn thumbnail_impl(_file: &Path, _out_jpg: &Path, _at_secs: f64, _width: u32) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Probes, thumbnails and registers a finished recording in the library, with source
/// `recorded`.
///
/// Steps: ensure the storage root row; return the existing id if `file` (relative to `root`)
/// is already registered; probe (errors fail registration); upsert the game when `meta.game`
/// is non-empty; write a poster to `thumbs_dir/<id>/poster.jpg` (failure is logged and the
/// recording is registered without one); insert the recording with status `ready`.
///
/// `file` must lie under `root`. See [`container_for`] for the accepted extensions.
///
/// The probe and poster run while `lib` is borrowed, so a caller that holds a library lock
/// blocks other users for the whole call. Such callers should use [`find_registered`],
/// [`prepare_recording`] and [`commit_prepared`] instead, with the lock taken only for the
/// first and last.
///
/// Audio tracks are stored in the `audio_tracks` table right after the recording row is inserted
/// (see [`MediaInfo::audio_tracks`] and [`Library::set_audio_tracks`]). Tracks with an unknown
/// channel count or sample rate are skipped, and a failed write is logged, not returned: the
/// recording stays registered without its audio rows. This path does not set
/// `recordings.audio_track_count`, so that column stays 0 for recordings registered here.
pub fn register_recording(
    lib: &Library,
    thumbs_dir: &Path,
    root: &Path,
    file: &Path,
    meta: RegisterMeta<'_>,
) -> Result<String, CatalogError> {
    register_with_source(lib, thumbs_dir, root, file, meta, RecordingSource::Recorded)
}

/// Same as [`register_recording`], but the row gets source `import`. Used for files copied
/// into the library by the import tool, so the UI can badge them.
pub fn register_imported(
    lib: &Library,
    thumbs_dir: &Path,
    root: &Path,
    file: &Path,
    meta: RegisterMeta<'_>,
) -> Result<String, CatalogError> {
    register_with_source(lib, thumbs_dir, root, file, meta, RecordingSource::Import)
}

#[allow(clippy::too_many_arguments)]
fn register_with_source(
    lib: &Library,
    thumbs_dir: &Path,
    root: &Path,
    file: &Path,
    meta: RegisterMeta<'_>,
    source: RecordingSource,
) -> Result<String, CatalogError> {
    // Any status counts (recording, pending, ready, missing, error): the row already exists and
    // a second insert would violate UNIQUE(root_id, rel_path).
    if let Some(id) = find_registered(lib, root, file)? {
        return Ok(id);
    }
    let prepared = prepare_recording(thumbs_dir, root, file)?;
    commit_prepared(lib, prepared, meta, source)
}

/// Returns the id of the recording already registered for `file` under `root`, or `None`.
/// Any status counts. Only this lookup needs the library, so it is the one step to run under a
/// library lock before [`prepare_recording`].
pub fn find_registered(
    lib: &Library,
    root: &Path,
    file: &Path,
) -> Result<Option<String>, CatalogError> {
    let rel_path = relative_to_root(root, file)?.to_string_lossy().into_owned();
    let root_id = lib.ensure_storage_root(&root.to_string_lossy(), now_ms())?;
    Ok(lib.find_by_rel_path(root_id, &rel_path)?)
}

/// A recording that has been probed and had its poster written, but is not yet in the library.
/// Built by [`prepare_recording`] without the library, and inserted by [`commit_prepared`].
#[derive(Debug)]
pub struct PreparedRecording {
    id: String,
    root: PathBuf,
    rel_path: String,
    file_name: String,
    db_ext: &'static str,
    playable: bool,
    info: MediaInfo,
    file_mtime_ms: i64,
    /// Poster written for `id`. Removed again if the commit finds the file already registered.
    poster: Option<PathBuf>,
}

/// Probes `file` and writes its poster, without touching the library. Pair with
/// [`find_registered`] before and [`commit_prepared`] after. Errors when the extension is not
/// accepted or the probe fails. A poster failure is logged and the recording gets no poster.
pub fn prepare_recording(
    thumbs_dir: &Path,
    root: &Path,
    file: &Path,
) -> Result<PreparedRecording, CatalogError> {
    let rel_path = relative_to_root(root, file)?.to_string_lossy().into_owned();
    let file_name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let Some((db_ext, playable)) = container_for(&ext) else {
        return Err(CatalogError::UnsupportedExt(ext));
    };

    let info = probe_media(file)?;
    let file_mtime_ms = mtime_ms(file)?;
    let id = Ulid::new().to_string();

    // Poster: 25% into the file, at most 3 s. A failure is logged; the recording is still
    // registered, just without a poster, since a poster is not needed to play it.
    let at = (info.duration_ms as f64 / 1000.0 * 0.25).min(3.0);
    let poster_path = thumbs_dir.join(&id).join("poster.jpg");
    let poster = match make_thumbnail(file, &poster_path, at, 320) {
        Ok(()) => Some(poster_path),
        Err(e) => {
            eprintln!("clipcheck: no poster for {}: {e}", file.display());
            None
        }
    };

    Ok(PreparedRecording {
        id,
        root: root.to_path_buf(),
        rel_path,
        file_name,
        db_ext,
        playable,
        info,
        file_mtime_ms,
        poster,
    })
}

/// Inserts a prepared recording with status `ready`, with the given source. If the file was
/// registered after [`find_registered`] ran, returns that existing id and discards the poster
/// written for this attempt. Library work only, so it is the step to run under the lock.
pub fn commit_prepared(
    lib: &Library,
    prepared: PreparedRecording,
    meta: RegisterMeta<'_>,
    source: RecordingSource,
) -> Result<String, CatalogError> {
    let now = now_ms();
    let root_id = lib.ensure_storage_root(&prepared.root.to_string_lossy(), now)?;
    if let Some(existing) = lib.find_by_rel_path(root_id, &prepared.rel_path)? {
        if let Some(poster) = &prepared.poster {
            discard_poster(poster);
        }
        return Ok(existing);
    }

    let PreparedRecording {
        id,
        rel_path,
        file_name,
        db_ext,
        playable,
        info,
        file_mtime_ms,
        ..
    } = prepared;

    let game_id = meta.game.trim();
    let game_id = if game_id.is_empty() {
        None
    } else {
        let display_full = game_id.strip_prefix("exe:").unwrap_or(game_id);
        let display: String = display_full.chars().take(200).collect();
        let display = if display.trim().is_empty() {
            "Unknown".to_owned()
        } else {
            display
        };
        // One shared sanitizer so the library folder name always equals the on-disk game folder.
        let folder: String = clipcheck_core::config::schema::sanitize_game_folder(&display);
        lib.upsert_game(
            &NewGame {
                id: game_id.to_owned(),
                display_name: display,
                folder_name: folder,
                exe_name: game_id.strip_prefix("exe:").map(str::to_owned),
                store_id: None,
            },
            now,
        )?;
        Some(game_id.to_owned())
    };

    lib.insert_recording_sourced(
        &NewRecording {
            id: id.clone(),
            kind: meta.kind,
            session_id: meta.session_id,
            part_index: meta.part_index.map(i64::from),
            game_id,
            root_id,
            rel_path,
            file_name,
            ext: db_ext.to_owned(),
            size_bytes: info.size_bytes as i64,
            file_mtime_ms,
            created_at_ms: meta.created_ms,
            added_at_ms: now,
            duration_ms: info.duration_ms,
            status: RecStatus::Ready,
            error_detail: None,
            end_reason: None,
            recovered: false,
            degraded: false,
            dropped_packets: 0,
            gap_ms: 0,
            needs_faststart: false,
        },
        source,
        playable,
    )?;
    let tracks = audio_rows(&info.audio_tracks, &id);
    if let Err(e) = lib.set_audio_tracks(&id, &tracks) {
        eprintln!("clipcheck: audio tracks not stored for {id}: {e}");
    }
    Ok(id)
}

/// Maps probed audio streams to `audio_tracks` rows. The table requires a channel count and a
/// sample rate above 0, so a track where either is unknown (0) is skipped with a log line
/// rather than failing the whole write. Titles are cut to the table's 120-character limit.
fn audio_rows(tracks: &[AudioTrackInfo], rec_id: &str) -> Vec<AudioTrackRow> {
    tracks
        .iter()
        .filter_map(|t| {
            if t.channels == 0 || t.sample_rate == 0 {
                eprintln!(
                    "clipcheck: audio track {} of {rec_id} has unknown channels or sample rate; not stored",
                    t.index
                );
                return None;
            }
            Some(AudioTrackRow {
                track_index: t.index,
                codec: t.codec.clone(),
                channels: t.channels,
                sample_rate: t.sample_rate,
                bitrate_kbps: None,
                language: None,
                title: t.title.as_ref().map(|s| s.chars().take(120).collect()),
            })
        })
        .collect()
}

/// Maps a lower-case file extension to the library's `ext` column and its playability.
///
/// Same mapping as the scanner (`scan.rs`): `m4v` and `mov` are stored as `mp4` (the real name
/// stays in `file_name`), `mkv` is stored as `mkv` and is not playable in the webview.
/// Anything else is `None`.
pub(crate) fn container_for(ext: &str) -> Option<(&'static str, bool)> {
    match ext {
        "mp4" | "m4v" | "mov" => Some(("mp4", true)),
        "mkv" => Some(("mkv", false)),
        _ => None,
    }
}

/// Path of `file` relative to `root`, comparing canonical paths.
fn relative_to_root(root: &Path, file: &Path) -> Result<PathBuf, CatalogError> {
    let not_under = || CatalogError::NotUnderRoot {
        file: file.to_path_buf(),
        root: root.to_path_buf(),
    };
    let root_c = root.canonicalize()?;
    let file_c = file.canonicalize()?;
    match file_c.strip_prefix(&root_c) {
        Ok(rel) if !rel.as_os_str().is_empty() => Ok(rel.to_path_buf()),
        _ => Err(not_under()),
    }
}

/// Modified time of `path` in UTC milliseconds. An unreadable or pre-1970 time is an error,
/// not 0: a stored 0 would read as 1970.
fn mtime_ms(path: &Path) -> io::Result<i64> {
    let modified = fs::metadata(path)?.modified()?;
    let since = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|e| invalid(format!("{} is dated before 1970: {e}", path.display())))?;
    i64::try_from(since.as_millis()).map_err(|_| {
        invalid(format!(
            "{} has an out-of-range modification time",
            path.display()
        ))
    })
}

/// Removes a poster written for a recording that turned out to be registered already. The
/// per-id folder goes too when it is empty. Failure is ignored: a stray poster is harmless.
fn discard_poster(poster: &Path) {
    let _ = fs::remove_file(poster);
    if let Some(dir) = poster.parent() {
        let _ = fs::remove_dir(dir);
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inserts a row for `rel` under `root` with the given status, returns its id.
    fn insert_row(lib: &Library, root: &Path, rel: &str, status: RecStatus) -> String {
        let root_id = lib
            .ensure_storage_root(&root.to_string_lossy(), now_ms())
            .unwrap();
        let id = Ulid::new().to_string();
        lib.insert_recording_sourced(
            &NewRecording {
                id: id.clone(),
                kind: RecordingKind::Clip,
                session_id: None,
                part_index: None,
                game_id: None,
                root_id,
                rel_path: rel.to_owned(),
                file_name: rel.to_owned(),
                ext: "mp4".to_owned(),
                size_bytes: 0,
                file_mtime_ms: 0,
                created_at_ms: 0,
                added_at_ms: 0,
                duration_ms: 0,
                status,
                error_detail: None,
                end_reason: None,
                recovered: false,
                degraded: false,
                dropped_packets: 0,
                gap_ms: 0,
                needs_faststart: false,
            },
            RecordingSource::Recorded,
            true,
        )
        .unwrap();
        id
    }

    fn meta() -> RegisterMeta<'static> {
        RegisterMeta {
            kind: RecordingKind::Clip,
            game: "",
            session_id: None,
            part_index: None,
            created_ms: 0,
        }
    }

    /// Re-registering a file whose row is in any status must return the existing id and must
    /// not reach the probe (a file that is not media would fail it) or the insert (UNIQUE).
    #[test]
    fn register_returns_existing_row_for_every_status() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open_in_memory().unwrap();

        for status in [
            RecStatus::Recording,
            RecStatus::Pending,
            RecStatus::Ready,
            RecStatus::Missing,
            RecStatus::Error,
        ] {
            let rel = format!("{status:?}.mp4");
            let file = dir.path().join(&rel);
            fs::write(&file, b"x").unwrap();
            let id = insert_row(&lib, dir.path(), &rel, status);
            let got = register_recording(&lib, dir.path(), dir.path(), &file, meta())
                .unwrap_or_else(|e| panic!("status {status:?}: {e}"));
            assert_eq!(got, id, "status {status:?}");
        }
    }

    /// The issue's scenario: register, mark missing, register again, same id, one row.
    #[test]
    fn register_after_mark_missing_keeps_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clip.mp4");
        fs::write(&file, b"x").unwrap();
        let lib = Library::open_in_memory().unwrap();

        let id = insert_row(&lib, dir.path(), "clip.mp4", RecStatus::Ready);
        let root_id = lib
            .ensure_storage_root(&dir.path().to_string_lossy(), now_ms())
            .unwrap();
        assert_eq!(
            lib.mark_missing(root_id, std::slice::from_ref(&id))
                .unwrap(),
            1
        );

        let again = register_recording(&lib, dir.path(), dir.path(), &file, meta()).unwrap();
        assert_eq!(again, id);
        assert_eq!(lib.find_by_rel_path(root_id, "clip.mp4").unwrap(), Some(id));
    }
}
