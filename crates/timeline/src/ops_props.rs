//! Property edits: clip patches, envelope, tracks, canvas, markers, generators.
//! Owner: worker `tl-props`.
//!
//! Same contract as `ops_basic`: every function validates first and mutates only when all checks
//! pass, so an `Err` leaves the project unchanged. Clip edits on a locked track give
//! `EditError::Locked`. Track settings (including `locked` itself) are not clip edits and are
//! never refused for being locked.

use serde::{Deserialize, Serialize};

use crate::error::{EditError, EditResult};
use crate::model::{
    new_id, Canvas, Clip, ClipAudio, ClipKind, EnvPoint, FadeCurve, Marker, Project, Rgba,
    TextSpec, Track, TrackKind, Transform, Transition,
};
use crate::ops_basic::overwrite_under;
use crate::time::{Tick, MAX_PROJECT_TICKS};

/// Most video tracks a project may hold.
const MAX_VIDEO_TRACKS: usize = 6;
/// Most audio tracks a project may hold.
const MAX_AUDIO_TRACKS: usize = 8;
/// Most envelope points one clip may be given.
const MAX_ENV_POINTS: usize = 64;
/// Longest marker label, in characters.
const MAX_MARKER_LABEL: usize = 80;
/// Longest title text, in characters.
const MAX_TITLE_TEXT: usize = 500;
/// Longest project name, in characters (after trimming).
const MAX_PROJECT_NAME: usize = 120;
/// Longest track name, in characters (after trimming).
const MAX_TRACK_NAME: usize = 40;
/// Longest canvas aspect preset id, in characters.
const MAX_ASPECT_PRESET: usize = 32;
/// Shortest and longest transition, in ticks.
const MIN_TRANSITION: Tick = Tick(25_401_600_000);
const MAX_TRANSITION: Tick = Tick(5 * 254_016_000_000);

/// A partial update of clip properties. `None` = leave alone. Double options (`Option<Option<T>>`)
/// distinguish "leave alone" (outer None) from "clear" (Some(None)).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipPatch {
    /// Display name.
    pub name: Option<String>,
    /// Label colour.
    #[serde(
        default,
        with = "crate::serde_util::double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub color: Option<Option<String>>,
    /// Enabled flag.
    pub enabled: Option<bool>,
    /// Whole transform replacement.
    pub transform: Option<Transform>,
    /// Opacity 0..=1.
    pub opacity: Option<f64>,
    /// Gain dB -60..=12.
    pub gain_db: Option<f64>,
    /// Mute.
    pub muted: Option<bool>,
    /// Pan -1..=1.
    pub pan: Option<f64>,
    /// Mono downmix.
    pub mono: Option<bool>,
    /// Source audio stream index.
    pub source_track: Option<u32>,
    /// Fade-in length (clamped so fades never exceed the clip length together).
    pub fade_in: Option<Tick>,
    /// Fade-out length.
    pub fade_out: Option<Tick>,
    /// Fade shape.
    pub fade_curve: Option<FadeCurve>,
    /// Transition into the clip (Some(None) clears). Length 0.1..=5 s and not longer than the clip.
    #[serde(
        default,
        with = "crate::serde_util::double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub transition_in: Option<Option<Transition>>,
    /// Title text (only for Title clips).
    pub text: Option<TextSpec>,
    /// Solid colour (only for Solid clips).
    pub solid: Option<Rgba>,
}

/// A partial update of track properties.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackPatch {
    /// Name.
    pub name: Option<String>,
    /// Muted.
    pub muted: Option<bool>,
    /// Solo.
    pub solo: Option<bool>,
    /// Locked.
    pub locked: Option<bool>,
    /// Hidden (video tracks).
    pub hidden: Option<bool>,
    /// Fader dB -60..=12.
    pub volume_db: Option<f64>,
    /// Targeted.
    pub targeted: Option<bool>,
    /// Row height 24..=200.
    pub height: Option<u32>,
}

/// Applies a patch to every listed clip (group/link partners are NOT added; the caller chooses).
/// Validates every field first (range errors give `Invalid` and nothing changes).
/// Audio fields apply to Audio clips only, text to Title clips only, solid to Solid clips only,
/// transform/opacity to Video/Image/Title/Solid clips only; a mismatch is `Invalid`.
pub fn patch_clips(p: &mut Project, ids: &[String], patch: &ClipPatch) -> EditResult<()> {
    validate_clip_patch(patch)?;
    let targets = target_indices(p, ids)?;
    for &i in &targets {
        let Some(clip) = p.clips.get(i) else {
            continue;
        };
        ensure_unlocked(p, &clip.track_id)?;
        check_clip_accepts(clip, patch)?;
    }
    for &i in &targets {
        if let Some(clip) = p.clips.get_mut(i) {
            apply_clip_patch(clip, patch);
        }
    }
    Ok(())
}

/// Replaces an audio clip's volume envelope (points sorted, de-duplicated by time, clamped to the
/// clip's length and -60..=12 dB, at most 64 points).
pub fn set_envelope(p: &mut Project, clip_id: &str, points: Vec<EnvPoint>) -> EditResult<()> {
    let i = clip_index(p, clip_id)?;
    let Some(clip) = p.clips.get(i) else {
        return Err(EditError::NotFound(format!("clip {clip_id}")));
    };
    if clip.kind != ClipKind::Audio {
        return Err(EditError::Invalid(format!(
            "clip {clip_id} is not an audio clip"
        )));
    }
    ensure_unlocked(p, &clip.track_id)?;
    if points.len() > MAX_ENV_POINTS {
        return Err(EditError::Limit(format!(
            "at most {MAX_ENV_POINTS} envelope points"
        )));
    }
    if points.iter().any(|pt| !pt.db.is_finite()) {
        return Err(EditError::Invalid(
            "envelope db must be a finite number".to_string(),
        ));
    }
    let dur = clip.dur;

    let mut sorted = points;
    sorted.sort_by_key(|pt| pt.t);
    let mut out: Vec<EnvPoint> = Vec::with_capacity(sorted.len());
    for pt in sorted {
        if pt.t < Tick::ZERO || pt.t > dur {
            continue;
        }
        let pt = EnvPoint {
            t: pt.t,
            db: pt.db.clamp(-60.0, 12.0),
        };
        // Stable sort keeps input order among equal times, so the last one wins.
        if out.last().is_some_and(|last| last.t == pt.t) {
            if let Some(last) = out.last_mut() {
                *last = pt;
            }
        } else {
            out.push(pt);
        }
    }
    if let Some(clip) = p.clips.get_mut(i) {
        clip.audio.envelope = out;
    }
    Ok(())
}

/// Applies `delta_db` to every listed clip's gain (used by "balance" sliders and group gain UI).
pub fn offset_gain(p: &mut Project, ids: &[String], delta_db: f64) -> EditResult<()> {
    if !delta_db.is_finite() {
        return Err(EditError::Invalid(
            "delta_db must be a finite number".to_string(),
        ));
    }
    let targets = target_indices(p, ids)?;
    for &i in &targets {
        let Some(clip) = p.clips.get(i) else {
            continue;
        };
        if clip.kind != ClipKind::Audio {
            return Err(EditError::Invalid(format!(
                "clip {} is not an audio clip",
                clip.id
            )));
        }
        ensure_unlocked(p, &clip.track_id)?;
    }
    for &i in &targets {
        if let Some(clip) = p.clips.get_mut(i) {
            clip.audio.gain_db = (clip.audio.gain_db + delta_db).clamp(-60.0, 12.0);
        }
    }
    Ok(())
}

/// Adds a track of `kind` at the top of its kind (video: end of the list = top layer; audio: end
/// of audio list). Name "V<n>" / "A<n>". Limits: 6 video + 8 audio. Returns the id.
pub fn add_track(p: &mut Project, kind: TrackKind) -> EditResult<String> {
    let (limit, prefix) = match kind {
        TrackKind::Video => (MAX_VIDEO_TRACKS, "V"),
        TrackKind::Audio => (MAX_AUDIO_TRACKS, "A"),
    };
    let count = p.tracks.iter().filter(|t| t.kind == kind).count();
    if count >= limit {
        return Err(EditError::Limit(format!("at most {limit} {prefix} tracks")));
    }
    let id = new_id();
    let track = Track {
        id: id.clone(),
        kind,
        name: format!("{prefix}{}", count + 1),
        ..Track::default()
    };
    // Right after the last track of the same kind (or at the end when there is none).
    let pos = p
        .tracks
        .iter()
        .rposition(|t| t.kind == kind)
        .map_or(p.tracks.len(), |i| i + 1);
    p.tracks.insert(pos.min(p.tracks.len()), track);
    Ok(id)
}

/// Removes a track and its clips (also removes emptied groups and fixes dangling link ids). The
/// last track of a kind may not be removed.
pub fn remove_track(p: &mut Project, track_id: &str) -> EditResult<()> {
    let track = p
        .track(track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))?;
    if track.locked {
        return Err(EditError::Locked(track_id.to_string()));
    }
    let kind = track.kind;
    let same_kind = p.tracks.iter().filter(|t| t.kind == kind).count();
    if same_kind <= 1 {
        return Err(EditError::Invalid(format!(
            "cannot remove the last {} track",
            if kind == TrackKind::Video {
                "video"
            } else {
                "audio"
            }
        )));
    }

    p.tracks.retain(|t| t.id != track_id);
    p.clips.retain(|c| c.track_id != track_id);

    // Link ids held by one clip only, and groups with fewer than two members, are dangling.
    let mut link_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut group_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for c in &p.clips {
        if let Some(l) = &c.link_id {
            *link_counts.entry(l.clone()).or_default() += 1;
        }
        if let Some(g) = &c.group_id {
            *group_counts.entry(g.clone()).or_default() += 1;
        }
    }
    for c in p.clips.iter_mut() {
        let clear_link = c
            .link_id
            .as_ref()
            .is_some_and(|l| link_counts.get(l).copied().unwrap_or(0) <= 1);
        if clear_link {
            c.link_id = None;
        }
        let clear_group = c
            .group_id
            .as_ref()
            .is_some_and(|g| group_counts.get(g).copied().unwrap_or(0) <= 1);
        if clear_group {
            c.group_id = None;
        }
    }
    p.groups
        .retain(|g| group_counts.get(&g.id).copied().unwrap_or(0) >= 2);
    Ok(())
}

/// Applies a patch to a track. Track settings are not clip edits, so a locked track still accepts
/// this (it is how a track is unlocked).
pub fn patch_track(p: &mut Project, track_id: &str, patch: &TrackPatch) -> EditResult<()> {
    let i = track_index(p, track_id)?;
    let name = match &patch.name {
        Some(n) => {
            let trimmed = n.trim();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_TRACK_NAME {
                return Err(EditError::Invalid(format!(
                    "track name must be 1..={MAX_TRACK_NAME} characters"
                )));
            }
            Some(trimmed.to_string())
        }
        None => None,
    };
    if let Some(v) = patch.volume_db {
        check_range("volume_db", v, -60.0, 12.0)?;
    }
    if let Some(h) = patch.height {
        if !(24..=200).contains(&h) {
            return Err(EditError::Invalid(
                "height must be between 24 and 200".to_string(),
            ));
        }
    }
    if let Some(track) = p.tracks.get_mut(i) {
        if let Some(n) = name {
            track.name = n;
        }
        if let Some(v) = patch.muted {
            track.muted = v;
        }
        if let Some(v) = patch.solo {
            track.solo = v;
        }
        if let Some(v) = patch.locked {
            track.locked = v;
        }
        if let Some(v) = patch.hidden {
            track.hidden = v;
        }
        if let Some(v) = patch.volume_db {
            track.volume_db = v;
        }
        if let Some(v) = patch.targeted {
            track.targeted = v;
        }
        if let Some(v) = patch.height {
            track.height = v;
        }
    }
    Ok(())
}

/// Moves a track within its kind by `delta` positions (positive = later in the list).
pub fn move_track(p: &mut Project, track_id: &str, delta: i32) -> EditResult<()> {
    let track = p
        .track(track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))?;
    let kind = track.kind;
    let mut same: Vec<Track> = p
        .tracks
        .iter()
        .filter(|t| t.kind == kind)
        .cloned()
        .collect();
    let pos = same
        .iter()
        .position(|t| t.id == track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))?;
    let last = same.len().saturating_sub(1) as i64;
    let new_pos = (pos as i64 + i64::from(delta)).clamp(0, last) as usize;
    if new_pos == pos {
        return Ok(());
    }
    let moved = same.remove(pos);
    same.insert(new_pos, moved);
    // Put the reordered tracks back into the slots that hold tracks of this kind.
    let mut reordered = same.into_iter();
    for t in p.tracks.iter_mut() {
        if t.kind == kind {
            if let Some(n) = reordered.next() {
                *t = n;
            }
        }
    }
    Ok(())
}

/// Replaces the canvas. Validates: even width/height 128..=8192, fps 1..=120, preset string.
/// Clips are not changed.
pub fn set_canvas(p: &mut Project, canvas: Canvas) -> EditResult<()> {
    check_canvas_dim("width", canvas.width)?;
    check_canvas_dim("height", canvas.height)?;
    if canvas.fps.num == 0 || canvas.fps.den == 0 {
        return Err(EditError::Invalid("fps must be non-zero".to_string()));
    }
    if canvas.fps.num < canvas.fps.den {
        return Err(EditError::Invalid("fps must be at least 1".to_string()));
    }
    if canvas.fps.as_f64() > 120.0 {
        return Err(EditError::Invalid("fps must be at most 120".to_string()));
    }
    if canvas.aspect_preset.chars().count() > MAX_ASPECT_PRESET {
        return Err(EditError::Invalid(format!(
            "aspect_preset must be at most {MAX_ASPECT_PRESET} characters"
        )));
    }
    p.canvas = canvas;
    Ok(())
}

/// Renames the project (1..=120 chars after trimming).
pub fn rename_project(p: &mut Project, name: &str) -> EditResult<()> {
    let trimmed = name.trim();
    let len = trimmed.chars().count();
    if !(1..=MAX_PROJECT_NAME).contains(&len) {
        return Err(EditError::Invalid(format!(
            "project name must be 1..={MAX_PROJECT_NAME} characters"
        )));
    }
    p.name = trimmed.to_string();
    Ok(())
}

/// Sets the editing modes (magnetic and snap).
pub fn set_modes(p: &mut Project, magnetic: bool, snap: bool) -> EditResult<()> {
    p.modes.magnetic = magnetic;
    p.modes.snap = snap;
    Ok(())
}

/// Adds a marker; returns its id.
pub fn add_marker(
    p: &mut Project,
    time: Tick,
    label: &str,
    color: Option<String>,
) -> EditResult<String> {
    check_marker_time(time)?;
    check_marker_label(label)?;
    let id = new_id();
    p.markers.push(Marker {
        id: id.clone(),
        time,
        label: label.to_string(),
        color,
    });
    p.markers.sort_by_key(|m| m.time);
    Ok(id)
}

/// Edits a marker (any of time, label, colour).
pub fn edit_marker(
    p: &mut Project,
    marker_id: &str,
    time: Option<Tick>,
    label: Option<String>,
    color: Option<Option<String>>,
) -> EditResult<()> {
    let i = p
        .markers
        .iter()
        .position(|m| m.id == marker_id)
        .ok_or_else(|| EditError::NotFound(format!("marker {marker_id}")))?;
    if let Some(t) = time {
        check_marker_time(t)?;
    }
    if let Some(l) = &label {
        check_marker_label(l)?;
    }
    if let Some(marker) = p.markers.get_mut(i) {
        if let Some(t) = time {
            marker.time = t;
        }
        if let Some(l) = label {
            marker.label = l;
        }
        if let Some(c) = color {
            marker.color = c;
        }
    }
    p.markers.sort_by_key(|m| m.time);
    Ok(())
}

/// Removes a marker.
pub fn remove_marker(p: &mut Project, marker_id: &str) -> EditResult<()> {
    let before = p.markers.len();
    p.markers.retain(|m| m.id != marker_id);
    if p.markers.len() == before {
        return Err(EditError::NotFound(format!("marker {marker_id}")));
    }
    Ok(())
}

/// Adds a Title clip on a video track at `at` for `dur` (default text spec).
pub fn add_title(
    p: &mut Project,
    track_id: &str,
    at: Tick,
    dur: Tick,
    text: TextSpec,
) -> EditResult<String> {
    validate_text(&text)?;
    let clip = Clip {
        kind: ClipKind::Title,
        name: title_name(&text.text),
        text: Some(text),
        ..Clip::default()
    };
    insert_generated(p, track_id, at, dur, clip)
}

/// Adds a Solid colour clip on a video track.
pub fn add_solid(
    p: &mut Project,
    track_id: &str,
    at: Tick,
    dur: Tick,
    color: Rgba,
) -> EditResult<String> {
    let clip = Clip {
        kind: ClipKind::Solid,
        name: format!("Solid #{:02X}{:02X}{:02X}", color.r, color.g, color.b),
        solid: Some(color),
        ..Clip::default()
    };
    insert_generated(p, track_id, at, dur, clip)
}

/// Detaches nothing; returns an audio clip's defaults for a media file (helper used by tests and
/// `place_media`): an Audio clip for stream `stream` of `media_id` covering the given ranges.
pub fn audio_clip_for(
    p: &Project,
    media_id: &str,
    stream: u32,
    track_id: &str,
    start: Tick,
    src_in: Tick,
    src_len: Tick,
) -> EditResult<crate::model::Clip> {
    let media = p
        .media_ref(media_id)
        .ok_or_else(|| EditError::NotFound(format!("media {media_id}")))?;
    let track = p
        .track(track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))?;
    if track.kind != TrackKind::Audio {
        return Err(EditError::WrongTrackKind(track_id.to_string()));
    }
    let stream_ok = match media.kind {
        crate::model::MediaKind::Video | crate::model::MediaKind::Audio => {
            (stream as usize) < media.audio.len()
        }
        crate::model::MediaKind::Image => false,
    };
    if !stream_ok {
        return Err(EditError::Invalid(format!(
            "audio stream {stream} does not exist in media {media_id}"
        )));
    }
    if start < Tick::ZERO || src_in < Tick::ZERO {
        return Err(EditError::Invalid(
            "start and src_in must not be negative".to_string(),
        ));
    }
    if src_len <= Tick::ZERO {
        return Err(EditError::Invalid("src_len must be positive".to_string()));
    }
    let title = media
        .audio
        .get(stream as usize)
        .map(|a| a.title.trim().to_string())
        .unwrap_or_default();
    let name = if title.is_empty() {
        media.name.clone()
    } else {
        format!("{} - {}", media.name, title)
    };
    Ok(Clip {
        id: new_id(),
        track_id: track_id.to_string(),
        kind: ClipKind::Audio,
        media_id: Some(media_id.to_string()),
        name,
        start,
        dur: src_len,
        src_in,
        speed: 1.0,
        audio: ClipAudio {
            source_track: stream,
            muted: stream != 0,
            ..ClipAudio::default()
        },
        ..Clip::default()
    })
}

/// Convenience used by tests: track by index among tracks of a kind.
pub fn track_of_kind(p: &Project, kind: TrackKind, index: usize) -> Option<&Track> {
    p.tracks.iter().filter(|t| t.kind == kind).nth(index)
}

// ---------------------------------------------------------------------------------------------
// Helpers (all validation is done before any mutation).

/// Index of a clip by id.
fn clip_index(p: &Project, clip_id: &str) -> EditResult<usize> {
    p.clips
        .iter()
        .position(|c| c.id == clip_id)
        .ok_or_else(|| EditError::NotFound(format!("clip {clip_id}")))
}

/// Index of a track by id.
fn track_index(p: &Project, track_id: &str) -> EditResult<usize> {
    p.tracks
        .iter()
        .position(|t| t.id == track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))
}

/// Indices of the listed clips, sorted and without duplicates. Unknown ids give `NotFound`.
fn target_indices(p: &Project, ids: &[String]) -> EditResult<Vec<usize>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(clip_index(p, id)?);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// A clip on a locked track may not be edited.
fn ensure_unlocked(p: &Project, track_id: &str) -> EditResult<()> {
    match p.track(track_id) {
        Some(t) if t.locked => Err(EditError::Locked(track_id.to_string())),
        _ => Ok(()),
    }
}

/// Range check for a finite number (NaN and infinities are rejected).
fn check_range(field: &str, v: f64, lo: f64, hi: f64) -> EditResult<()> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(EditError::Invalid(format!(
            "{field} must be between {lo} and {hi}"
        )))
    }
}

/// Transition length: 0.1 s to 5 s.
fn check_transition(t: &Transition) -> EditResult<()> {
    if (MIN_TRANSITION..=MAX_TRANSITION).contains(&t.duration) {
        Ok(())
    } else {
        Err(EditError::Invalid(
            "transition_in duration must be between 0.1 s and 5 s".to_string(),
        ))
    }
}

/// Canvas width or height: even, 128..=8192.
fn check_canvas_dim(field: &str, v: u32) -> EditResult<()> {
    if (128..=8192).contains(&v) && v.is_multiple_of(2) {
        Ok(())
    } else {
        Err(EditError::Invalid(format!(
            "canvas {field} must be an even number from 128 to 8192"
        )))
    }
}

/// Marker time: not negative.
fn check_marker_time(t: Tick) -> EditResult<()> {
    if t < Tick::ZERO {
        Err(EditError::Invalid(
            "marker time must not be negative".to_string(),
        ))
    } else {
        Ok(())
    }
}

/// Marker label: at most 80 characters.
fn check_marker_label(label: &str) -> EditResult<()> {
    if label.chars().count() > MAX_MARKER_LABEL {
        Err(EditError::Invalid(format!(
            "marker label must be at most {MAX_MARKER_LABEL} characters"
        )))
    } else {
        Ok(())
    }
}

/// Checks a transform's ranges.
fn validate_transform(t: &Transform) -> EditResult<()> {
    check_range("transform.scale", t.scale, 0.05, 8.0)?;
    check_range("transform.pos_x", t.pos_x, -2.0, 2.0)?;
    check_range("transform.pos_y", t.pos_y, -2.0, 2.0)?;
    if t.rotate_90 > 3 {
        return Err(EditError::Invalid(
            "transform.rotate_90 must be between 0 and 3".to_string(),
        ));
    }
    check_range("transform.crop.left", t.crop.left, 0.0, 0.9)?;
    check_range("transform.crop.right", t.crop.right, 0.0, 0.9)?;
    check_range("transform.crop.top", t.crop.top, 0.0, 0.9)?;
    check_range("transform.crop.bottom", t.crop.bottom, 0.0, 0.9)?;
    if t.crop.left + t.crop.right >= 0.95 {
        return Err(EditError::Invalid(
            "transform.crop left + right must be below 0.95".to_string(),
        ));
    }
    if t.crop.top + t.crop.bottom >= 0.95 {
        return Err(EditError::Invalid(
            "transform.crop top + bottom must be below 0.95".to_string(),
        ));
    }
    Ok(())
}

/// Checks a text spec's ranges.
fn validate_text(t: &TextSpec) -> EditResult<()> {
    if t.text.chars().count() > MAX_TITLE_TEXT {
        return Err(EditError::Invalid(format!(
            "title text must be at most {MAX_TITLE_TEXT} characters"
        )));
    }
    check_range("text.size", t.size, 8.0, 1000.0)
}

/// Name for a title clip: the first line, trimmed, at most 30 characters.
fn title_name(text: &str) -> String {
    let first: String = text
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(30)
        .collect();
    if first.is_empty() {
        "Title".to_string()
    } else {
        first
    }
}

/// Checks every field of a clip patch that does not depend on the clip it is applied to.
fn validate_clip_patch(patch: &ClipPatch) -> EditResult<()> {
    if let Some(t) = &patch.transform {
        validate_transform(t)?;
    }
    if let Some(v) = patch.opacity {
        check_range("opacity", v, 0.0, 1.0)?;
    }
    if let Some(v) = patch.gain_db {
        check_range("gain_db", v, -60.0, 12.0)?;
    }
    if let Some(v) = patch.pan {
        check_range("pan", v, -1.0, 1.0)?;
    }
    if let Some(v) = patch.fade_in {
        if v < Tick::ZERO {
            return Err(EditError::Invalid(
                "fade_in must not be negative".to_string(),
            ));
        }
    }
    if let Some(v) = patch.fade_out {
        if v < Tick::ZERO {
            return Err(EditError::Invalid(
                "fade_out must not be negative".to_string(),
            ));
        }
    }
    if let Some(Some(t)) = &patch.transition_in {
        check_transition(t)?;
    }
    if let Some(t) = &patch.text {
        validate_text(t)?;
    }
    Ok(())
}

/// Checks that one clip accepts every field the patch supplies.
fn check_clip_accepts(clip: &Clip, patch: &ClipPatch) -> EditResult<()> {
    let is_audio = clip.kind == ClipKind::Audio;
    let audio_field = patch.gain_db.is_some()
        || patch.muted.is_some()
        || patch.pan.is_some()
        || patch.mono.is_some()
        || patch.source_track.is_some()
        || patch.fade_in.is_some()
        || patch.fade_out.is_some()
        || patch.fade_curve.is_some();
    let visual_field =
        patch.transform.is_some() || patch.opacity.is_some() || patch.transition_in.is_some();
    let mismatch = |field: &str| {
        Err(EditError::Invalid(format!(
            "{field} does not apply to clip {} ({:?})",
            clip.id, clip.kind
        )))
    };
    if audio_field && !is_audio {
        return mismatch("audio fields");
    }
    if visual_field && is_audio {
        return mismatch("transform, opacity or transition_in");
    }
    if patch.text.is_some() && clip.kind != ClipKind::Title {
        return mismatch("text");
    }
    if patch.solid.is_some() && clip.kind != ClipKind::Solid {
        return mismatch("solid");
    }
    if patch.fade_in.is_some() || patch.fade_out.is_some() {
        let fi = patch.fade_in.unwrap_or(clip.audio.fade_in);
        let fo = patch.fade_out.unwrap_or(clip.audio.fade_out);
        if fi.saturating_add(fo) > clip.dur {
            return Err(EditError::Invalid(format!(
                "fade_in + fade_out exceed the length of clip {}",
                clip.id
            )));
        }
    }
    if let Some(Some(t)) = &patch.transition_in {
        if t.duration > clip.dur {
            return Err(EditError::Invalid(format!(
                "transition_in is longer than clip {}",
                clip.id
            )));
        }
    }
    Ok(())
}

/// Writes a validated patch into a clip. Only call after `check_clip_accepts` passed.
fn apply_clip_patch(clip: &mut Clip, patch: &ClipPatch) {
    if let Some(n) = &patch.name {
        clip.name = n.clone();
    }
    if let Some(c) = &patch.color {
        clip.color = c.clone();
    }
    if let Some(e) = patch.enabled {
        clip.enabled = e;
    }
    if let Some(t) = patch.transform {
        clip.transform = t;
    }
    if let Some(o) = patch.opacity {
        clip.opacity = o;
    }
    if let Some(g) = patch.gain_db {
        clip.audio.gain_db = g;
    }
    if let Some(m) = patch.muted {
        clip.audio.muted = m;
    }
    if let Some(pan) = patch.pan {
        clip.audio.pan = pan;
    }
    if let Some(mono) = patch.mono {
        clip.audio.mono = mono;
    }
    if let Some(s) = patch.source_track {
        clip.audio.source_track = s;
    }
    if let Some(f) = patch.fade_in {
        clip.audio.fade_in = f;
    }
    if let Some(f) = patch.fade_out {
        clip.audio.fade_out = f;
    }
    if let Some(c) = patch.fade_curve {
        clip.audio.fade_curve = c;
    }
    if let Some(tr) = patch.transition_in {
        clip.transition_in = tr;
    }
    if let Some(t) = &patch.text {
        clip.text = Some(t.clone());
    }
    if let Some(s) = patch.solid {
        clip.solid = Some(s);
    }
}

/// Inserts a generated clip (title or solid) on a video track after checking the span.
fn insert_generated(
    p: &mut Project,
    track_id: &str,
    at: Tick,
    dur: Tick,
    mut clip: Clip,
) -> EditResult<String> {
    let track = p
        .track(track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))?;
    if track.kind != TrackKind::Video {
        return Err(EditError::WrongTrackKind(track_id.to_string()));
    }
    if track.locked {
        return Err(EditError::Locked(track_id.to_string()));
    }
    if at < Tick::ZERO {
        return Err(EditError::Invalid(
            "start time must not be negative".to_string(),
        ));
    }
    if dur < p.canvas.fps.frame_duration() {
        return Err(EditError::Invalid(
            "duration must be at least one frame".to_string(),
        ));
    }
    let end = at
        .checked_add(dur)
        .ok_or_else(|| EditError::Limit("tick range".to_string()))?;
    if end > Tick(MAX_PROJECT_TICKS) {
        return Err(EditError::Limit(
            "project length is limited to 4 hours".to_string(),
        ));
    }
    let id = new_id();
    clip.id = id.clone();
    clip.track_id = track_id.to_string();
    clip.start = at;
    clip.dur = dur;
    // The generated clip overwrites whatever is under it (see `overwrite_under`).
    p.clips.push(clip);
    p.clips = overwrite_under(std::mem::take(&mut p.clips), std::slice::from_ref(&id));
    Ok(id)
}
