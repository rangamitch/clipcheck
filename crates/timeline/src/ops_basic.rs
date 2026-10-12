//! Basic timeline edits: media bin, insert/overwrite, move, delete, split, duplicate.
//! Owner: worker `tl-basic`.
//!
//! CONTRACT (all functions): on `Err` the project is unchanged. Every operation works on a copy of
//! the project and swaps it in only after [`check_project`] accepts the result. Locked tracks
//! refuse edits (`EditError::Locked`). No overlaps may result on any track. Never panic on bad
//! input.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::{EditError, EditResult};
use crate::model::{
    new_id, Clip, ClipAudio, ClipKind, EnvPoint, Group, MediaKind, MediaRef, Project, Track,
    TrackKind,
};
use crate::ops_group::expand_selection;
use crate::time::{Tick, MAX_PROJECT_TICKS, MAX_TICK};

/// Most video tracks a project may hold.
const MAX_VIDEO_TRACKS: usize = 6;
/// Most audio tracks a project may hold.
const MAX_AUDIO_TRACKS: usize = 8;
/// Length of a still image placed with a zero `src_len`, in seconds.
const IMAGE_DEFAULT_SECS: i64 = 5;

/// How a new clip is placed when the target time is already occupied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceMode {
    /// Replace whatever is under the new clip (split/trim existing clips as needed).
    Overwrite,
    /// Push everything at and after the point to the right on the affected tracks.
    Insert,
    /// Ignore `at`; place at the end of the target track.
    Append,
}

/// One requested clip move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMove {
    /// Clip to move.
    pub clip_id: String,
    /// New start on the timeline (>= 0).
    pub new_start: Tick,
    /// New track (same kind as the clip's current track), or None to keep the track.
    pub new_track: Option<String>,
}

/// Adds a media file to the bin and returns its id. If a media with the same `path` already
/// exists, returns that id instead of adding a duplicate. Sets `id` if empty.
pub fn add_media(p: &mut Project, mut media: MediaRef) -> EditResult<String> {
    if let Some(existing) = p.media.iter().find(|m| m.path == media.path) {
        return Ok(existing.id.clone());
    }
    check_time(media.duration, "duration")?;
    if media.id.is_empty() {
        media.id = new_id();
    } else if p.media.iter().any(|m| m.id == media.id) {
        return Err(EditError::Invalid(format!(
            "media id {} is already used",
            media.id
        )));
    }
    let id = media.id.clone();
    p.media.push(media);
    Ok(id)
}

/// Removes a media file from the bin. `EditError::InUse(n)` when n clips still use it.
pub fn remove_media(p: &mut Project, media_id: &str) -> EditResult<()> {
    if p.media_ref(media_id).is_none() {
        return Err(EditError::NotFound(format!("media {media_id}")));
    }
    let used = p
        .clips
        .iter()
        .filter(|c| c.media_id.as_deref() == Some(media_id))
        .count();
    if used > 0 {
        return Err(EditError::InUse(used));
    }
    p.media.retain(|m| m.id != media_id);
    Ok(())
}

/// Replaces a media file's path/probe data (relink). Clips keep their ranges. Clears `missing`.
/// The new data must have the same `kind` as the old one (`EditError::Invalid` otherwise).
pub fn relink_media(p: &mut Project, media_id: &str, mut new: MediaRef) -> EditResult<()> {
    let old_kind = p
        .media_ref(media_id)
        .ok_or_else(|| EditError::NotFound(format!("media {media_id}")))?
        .kind;
    if old_kind != new.kind {
        return Err(EditError::Invalid(
            "relink must keep the media kind".to_string(),
        ));
    }
    check_time(new.duration, "duration")?;
    new.id = media_id.to_string();
    new.missing = false;
    if let Some(slot) = p.media.iter_mut().find(|m| m.id == media_id) {
        *slot = new;
    }
    Ok(())
}

/// Places media on the timeline. For a video media: one Video clip on `video_track` (the first
/// unlocked video track when None) plus one linked Audio clip per placed audio stream on
/// consecutive audio tracks starting at the first unlocked audio track (new audio tracks are added
/// up to the limit of 8; streams beyond that are skipped). The placed streams are all streams
/// except the pre-mixed "Mix" stream (see [`placed_audio_streams`]); every placed clip is unmuted
/// and its `audio.source_track` is the stream's index in `media.audio`. Audio media: Audio clips
/// only (stream 0). Image media: one Image clip of `src_len` (default 5 s when `src_len` is zero).
/// `src_in`/`src_len` select the used range of the source (zero `src_len` means "to the end of the
/// media"). All created clips share a fresh `link_id`. Returns the new clip ids (video first).
/// `Append` places at the end of the video track (or the audio track for audio media). The project
/// canvas size/fps is NOT changed.
pub fn place_media(
    p: &mut Project,
    media_id: &str,
    at: Tick,
    src_in: Tick,
    src_len: Tick,
    video_track: Option<&str>,
    mode: PlaceMode,
) -> EditResult<Vec<String>> {
    let media: MediaRef = p
        .media_ref(media_id)
        .cloned()
        .ok_or_else(|| EditError::NotFound(format!("media {media_id}")))?;
    check_time(at, "at")?;
    check_time(src_in, "src_in")?;
    check_time(src_len, "src_len")?;

    let length = match media.kind {
        MediaKind::Image => {
            if src_len == Tick::ZERO {
                Tick::from_secs(IMAGE_DEFAULT_SECS)
            } else {
                src_len
            }
        }
        MediaKind::Video | MediaKind::Audio => {
            if media.duration <= Tick::ZERO {
                return Err(EditError::Invalid("media has no duration".to_string()));
            }
            let len = if src_len == Tick::ZERO {
                media.duration.saturating_sub(src_in)
            } else {
                src_len
            };
            if len <= Tick::ZERO {
                return Err(EditError::Invalid("source range is empty".to_string()));
            }
            if src_in.saturating_add(len) > media.duration {
                return Err(EditError::Invalid(
                    "source range runs past the end of the media".to_string(),
                ));
            }
            len
        }
    };
    if length < min_dur(p) {
        return Err(EditError::Invalid(
            "clip would be shorter than one frame".to_string(),
        ));
    }

    let mut w = p.clone();

    // Video target (not used by audio media).
    let video_id: Option<String> = if media.kind == MediaKind::Audio {
        None
    } else {
        let id = match video_track {
            Some(id) => {
                let t = w
                    .track(id)
                    .ok_or_else(|| EditError::NotFound(format!("track {id}")))?;
                if t.kind != TrackKind::Video {
                    return Err(EditError::WrongTrackKind(id.to_string()));
                }
                id.to_string()
            }
            None => match w
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Video && !t.locked)
            {
                Some(t) => t.id.clone(),
                None => {
                    return Err(match w.tracks.iter().find(|t| t.kind == TrackKind::Video) {
                        Some(first) => EditError::Locked(first.id.clone()),
                        None => EditError::NotFound("video track".to_string()),
                    });
                }
            },
        };
        ensure_unlocked(&w, &id)?;
        Some(id)
    };

    // Audio slots: one audio track per placed stream, consecutive from the first unlocked audio
    // track. `placed[i]` is the original stream index of slot `i`.
    let placed = placed_audio_streams(&media);
    let stream_count = placed.len();
    let mut slots: Vec<String> = Vec::new();
    if stream_count > 0 {
        let audio: Vec<(String, bool)> = w
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .map(|t| (t.id.clone(), t.locked))
            .collect();
        let start_pos = audio
            .iter()
            .position(|(_, locked)| !locked)
            .unwrap_or(audio.len());
        if !audio.is_empty() && start_pos == audio.len() {
            let first = audio.first().map(|a| a.0.clone()).unwrap_or_default();
            return Err(EditError::Locked(first));
        }
        let room = MAX_AUDIO_TRACKS.saturating_sub(audio.len());
        let available = audio.len().saturating_sub(start_pos).saturating_add(room);
        let used = stream_count.min(available);
        for i in 0..used {
            match audio.get(start_pos + i) {
                Some((id, locked)) => {
                    if *locked {
                        return Err(EditError::Locked(id.clone()));
                    }
                    slots.push(id.clone());
                }
                None => {
                    let n = w
                        .tracks
                        .iter()
                        .filter(|t| t.kind == TrackKind::Audio)
                        .count()
                        + 1;
                    let track = Track {
                        id: new_id(),
                        kind: TrackKind::Audio,
                        name: format!("A{n}"),
                        ..Track::default()
                    };
                    slots.push(track.id.clone());
                    w.tracks.push(track);
                }
            }
        }
    }

    let affected: Vec<String> = video_id
        .iter()
        .cloned()
        .chain(slots.iter().cloned())
        .collect();

    let start = match mode {
        PlaceMode::Append => {
            // Video/image media append after the chosen video track; audio-only media after the
            // first audio track. Audio lanes are not consulted: an occupied lane at that start is
            // overwritten by the new clips (see `overwrite_under`), never shifted around.
            let anchor = video_id.clone().or_else(|| slots.first().cloned());
            match anchor {
                Some(tid) => w
                    .clips
                    .iter()
                    .filter(|c| c.track_id == tid)
                    .map(end_of)
                    .max()
                    .unwrap_or(Tick::ZERO),
                None => Tick::ZERO,
            }
        }
        PlaceMode::Overwrite | PlaceMode::Insert => at,
    };

    match mode {
        PlaceMode::Overwrite => {
            for tid in &affected {
                w.clips = cut(
                    std::mem::take(&mut w.clips),
                    tid,
                    start,
                    start.saturating_add(length),
                );
            }
        }
        PlaceMode::Insert => {
            for tid in &affected {
                w.clips = cut(std::mem::take(&mut w.clips), tid, start, start);
            }
            for c in w.clips.iter_mut() {
                if c.start >= start && affected.contains(&c.track_id) {
                    c.start = c.start.saturating_add(length);
                }
            }
        }
        PlaceMode::Append => {}
    }

    // Name each audio lane after what it carries (the stream title, such as "Game" or "Mic").
    let labels: Vec<String> = placed
        .iter()
        .take(slots.len())
        .map(|&orig| stream_label(&media, orig, media.audio.len()))
        .collect();
    for (tid, label) in slots.iter().zip(&labels) {
        if label.is_empty() || w.clips.iter().any(|c| &c.track_id == tid) {
            continue;
        }
        if let Some(t) = w.tracks.iter_mut().find(|t| &t.id == tid) {
            if is_default_audio_name(&t.name) {
                t.name = label.clone();
            }
        }
    }

    let link = new_id();
    let mut created: Vec<Clip> = Vec::new();
    if let Some(vt) = &video_id {
        created.push(Clip {
            id: new_id(),
            track_id: vt.clone(),
            kind: if media.kind == MediaKind::Image {
                ClipKind::Image
            } else {
                ClipKind::Video
            },
            media_id: Some(media.id.clone()),
            name: media.name.clone(),
            start,
            dur: length,
            src_in,
            speed: 1.0,
            link_id: Some(link.clone()),
            ..Clip::default()
        });
    }
    for (i, tid) in slots.iter().enumerate() {
        let Some(&orig) = placed.get(i) else {
            continue;
        };
        created.push(Clip {
            id: new_id(),
            track_id: tid.clone(),
            kind: ClipKind::Audio,
            media_id: Some(media.id.clone()),
            name: match labels.get(i) {
                Some(l) if !l.is_empty() => format!("{} - {}", media.name, l),
                _ => media.name.clone(),
            },
            start,
            dur: length,
            src_in,
            speed: 1.0,
            link_id: Some(link.clone()),
            audio: ClipAudio {
                source_track: u32::try_from(orig).unwrap_or(0),
                muted: false,
                ..ClipAudio::default()
            },
            ..Clip::default()
        });
    }

    let ids: Vec<String> = created.iter().map(|c| c.id.clone()).collect();
    w.clips.extend(created);
    // The new clips overwrite whatever they land on (a no-op for Overwrite and Insert, which
    // already cut or shifted the tracks above).
    w.clips = overwrite_under(std::mem::take(&mut w.clips), &ids);
    check_project(&w)?;
    *p = w;
    Ok(ids)
}

/// Original indices (into `media.audio`) of the audio streams placed for `media`, in order.
/// Video media skip every stream whose trimmed title is "Mix" (case-insensitive): that stream is
/// the pre-mixed combination of the individual sources, which are placed instead. If skipping
/// would leave no stream (only a Mix stream, or one unnamed stream), all streams are placed.
/// Audio media place stream 0 only (one lane, as before). Image media place none.
fn placed_audio_streams(media: &MediaRef) -> Vec<usize> {
    match media.kind {
        MediaKind::Image => Vec::new(),
        MediaKind::Audio => vec![0],
        MediaKind::Video => {
            let all: Vec<usize> = (0..media.audio.len()).collect();
            let kept: Vec<usize> = all
                .iter()
                .copied()
                .filter(|&i| !is_mix_stream(media, i))
                .collect();
            if kept.is_empty() {
                all
            } else {
                kept
            }
        }
    }
}

/// True when audio stream `i` of `media` is titled "Mix" (ignoring case and surrounding spaces).
fn is_mix_stream(media: &MediaRef, i: usize) -> bool {
    media
        .audio
        .get(i)
        .is_some_and(|a| a.title.trim().eq_ignore_ascii_case("Mix"))
}

/// Label of audio stream `i` (an index into `media.audio`) of a media file: its title, else
/// "Audio N" (N = i + 1) when the file has several streams (`count` = `media.audio.len()`).
fn stream_label(media: &MediaRef, i: usize, count: usize) -> String {
    let title = media
        .audio
        .get(i)
        .map(|a| a.title.trim().to_string())
        .unwrap_or_default();
    if !title.is_empty() {
        title
    } else if count > 1 {
        format!("Audio {}", i + 1)
    } else {
        String::new()
    }
}

/// True for the names the editor gives new audio lanes ("A1", "A2", ...).
fn is_default_audio_name(name: &str) -> bool {
    let rest = name.strip_prefix('A').unwrap_or("");
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

/// Moves clips (and their group/link partners, which keep their relative offsets). In magnetic
/// mode (`p.modes.magnetic`) gaps left behind are NOT closed by this function (only deletes
/// ripple). A moved clip that lands on clips that are not being moved overwrites them: those are
/// trimmed, split or removed where the moved clips cover them (see [`overwrite_under`]).
pub fn move_clips(p: &mut Project, moves: &[ClipMove]) -> EditResult<()> {
    if moves.is_empty() {
        return Ok(());
    }
    // Explicit moves: clip id -> (old start, new start, new track).
    let mut explicit: HashMap<String, (Tick, Tick, Option<String>)> = HashMap::new();
    for m in moves {
        let clip = p
            .clip(&m.clip_id)
            .ok_or_else(|| EditError::NotFound(format!("clip {}", m.clip_id)))?;
        check_time(m.new_start, "new_start")?;
        if let Some(t) = &m.new_track {
            let track = p
                .track(t)
                .ok_or_else(|| EditError::NotFound(format!("track {t}")))?;
            if (track.kind == TrackKind::Audio) != (clip.kind == ClipKind::Audio) {
                return Err(EditError::WrongTrackKind(m.clip_id.clone()));
            }
        }
        let want = (clip.start, m.new_start, m.new_track.clone());
        if explicit.get(&m.clip_id).is_some_and(|prev| *prev != want) {
            return Err(EditError::Invalid(format!(
                "conflicting moves for clip {}",
                m.clip_id
            )));
        }
        explicit.insert(m.clip_id.clone(), want);
    }

    // Start offset per clip: explicit moves first, then partners inherit the delta of the listed
    // clip they belong to.
    let mut deltas: HashMap<String, i64> = HashMap::new();
    for (id, (old, new, _)) in &explicit {
        deltas.insert(id.clone(), new.0.saturating_sub(old.0));
    }
    let mut inherited: HashMap<String, i64> = HashMap::new();
    for (id, (old, new, _)) in &explicit {
        let d = new.0.saturating_sub(old.0);
        for partner in expand_selection(p, std::slice::from_ref(id)) {
            if explicit.contains_key(&partner) {
                continue;
            }
            if inherited.get(&partner).is_some_and(|prev| *prev != d) {
                return Err(EditError::Invalid(
                    "conflicting moves for a group or link".to_string(),
                ));
            }
            inherited.insert(partner, d);
        }
    }
    deltas.extend(inherited);

    let mut w = p.clone();
    for c in &w.clips {
        if deltas.contains_key(&c.id) {
            ensure_unlocked(&w, &c.track_id)?;
        }
    }
    for (_, _, new_track) in explicit.values() {
        if let Some(t) = new_track {
            ensure_unlocked(&w, t)?;
        }
    }
    for c in w.clips.iter_mut() {
        if let Some(d) = deltas.get(&c.id) {
            c.start = Tick(c.start.0.saturating_add(*d));
            if let Some((_, _, Some(t))) = explicit.get(&c.id) {
                c.track_id = t.clone();
            }
        }
    }
    // Clip order (not HashMap order) decides which of two moved clips wins an overlap.
    let moved: Vec<String> = w
        .clips
        .iter()
        .filter(|c| deltas.contains_key(&c.id))
        .map(|c| c.id.clone())
        .collect();
    w.clips = overwrite_under(std::mem::take(&mut w.clips), &moved);
    check_project(&w)?;
    *p = w;
    Ok(())
}

/// Deletes clips plus their group/link partners. With `ripple`, everything after each deleted
/// clip on its track shifts left by the deleted length (gap closed), per track independently.
/// Returns the ids actually deleted.
pub fn delete_clips(p: &mut Project, ids: &[String], ripple: bool) -> EditResult<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    for id in ids {
        if p.clip(id).is_none() {
            return Err(EditError::NotFound(format!("clip {id}")));
        }
    }
    let doomed = closure_of(p, ids);
    for c in &p.clips {
        if doomed.contains(c.id.as_str()) {
            ensure_unlocked(p, &c.track_id)?;
        }
    }

    let mut w = p.clone();
    let deleted: Vec<String> = w
        .clips
        .iter()
        .filter(|c| doomed.contains(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect();

    if ripple {
        let gaps: Vec<(String, Tick, Tick)> = w
            .clips
            .iter()
            .filter(|c| doomed.contains(c.id.as_str()))
            .map(|c| (c.track_id.clone(), c.start, c.dur))
            .collect();
        for c in w.clips.iter_mut() {
            if doomed.contains(c.id.as_str()) {
                continue;
            }
            let shift = gaps
                .iter()
                .filter(|(t, s, _)| *t == c.track_id && *s < c.start)
                .fold(Tick::ZERO, |acc, (_, _, d)| acc.saturating_add(*d));
            c.start = c.start.saturating_sub(shift);
        }
    }

    w.clips.retain(|c| !doomed.contains(c.id.as_str()));

    // A link with fewer than two members left no longer links anything.
    let mut links: HashMap<String, usize> = HashMap::new();
    for c in &w.clips {
        if let Some(l) = &c.link_id {
            *links.entry(l.clone()).or_insert(0) += 1;
        }
    }
    for c in w.clips.iter_mut() {
        if c.link_id
            .as_ref()
            .is_some_and(|l| links.get(l).copied().unwrap_or(0) < 2)
        {
            c.link_id = None;
        }
    }

    // Groups that no clip references any more are removed.
    let used: HashSet<String> = w.clips.iter().filter_map(|c| c.group_id.clone()).collect();
    w.groups.retain(|g| used.contains(&g.id));

    check_project(&w)?;
    *p = w;
    Ok(deleted)
}

/// Splits at `time`. `ids` empty = every clip on every unlocked, targeted track spanning `time`
/// strictly inside (start < time < end). Group/link partners of listed ids are split too. The
/// left half keeps the id; the right half gets a new id, copies all properties, `src_in` advanced
/// by the split offset times speed (handles `reverse`), audio fades/envelope split correctly
/// (fade_in stays on the left, fade_out moves to the right, envelope points are divided by time),
/// the right half has no `transition_in`. Right halves of linked clips share a NEW link_id; right
/// halves of grouped clips share a NEW group (same name/color). Returns the new right-half ids.
///
/// A listed split that would leave a piece shorter than one frame gives `Invalid`. With `ids`
/// empty such clips are skipped instead. Partners that do not span `time` are left alone.
pub fn split_at(p: &mut Project, time: Tick, ids: &[String]) -> EditResult<Vec<String>> {
    check_time(time, "time")?;
    let frame = min_dur(p);
    let spans = |c: &Clip| c.start < time && time < end_of(c);
    let fits =
        |c: &Clip| time.saturating_sub(c.start) >= frame && end_of(c).saturating_sub(time) >= frame;

    let mut split: HashSet<String> = HashSet::new();
    if ids.is_empty() {
        for c in &p.clips {
            if !spans(c) || !fits(c) {
                continue;
            }
            if let Some(t) = p.track(&c.track_id) {
                if !t.locked && t.targeted {
                    split.insert(c.id.clone());
                }
            }
        }
    } else {
        for id in ids {
            if p.clip(id).is_none() {
                return Err(EditError::NotFound(format!("clip {id}")));
            }
        }
        let set = closure_of(p, ids);
        for c in &p.clips {
            if !set.contains(c.id.as_str()) || !spans(c) {
                continue;
            }
            ensure_unlocked(p, &c.track_id)?;
            if !fits(c) {
                return Err(EditError::Invalid(format!(
                    "split leaves a piece of clip {} shorter than one frame",
                    c.id
                )));
            }
            split.insert(c.id.clone());
        }
    }
    if split.is_empty() {
        return Ok(Vec::new());
    }

    let mut w = p.clone();
    let mut remap = Remap::default();
    let mut new_ids: Vec<String> = Vec::new();
    let mut out: Vec<Clip> = Vec::with_capacity(w.clips.len() + split.len());
    for c in std::mem::take(&mut w.clips) {
        if !split.contains(c.id.as_str()) {
            out.push(c);
            continue;
        }
        let end = end_of(&c);
        let (Some(left), Some(mut right)) = (piece_of(&c, c.start, time), piece_of(&c, time, end))
        else {
            out.push(c);
            continue;
        };
        right.id = new_id();
        if let Some(l) = &c.link_id {
            right.link_id = Some(remap.link(l));
        }
        if let Some(g) = &c.group_id {
            right.group_id = Some(remap.group(&w.groups, g));
        }
        new_ids.push(right.id.clone());
        out.push(left);
        out.push(right);
    }
    w.clips = out;
    w.groups.extend(remap.new_groups);
    check_project(&w)?;
    *p = w;
    Ok(new_ids)
}

/// Copies clips (with partners) so the earliest starts at `earliest.start + offset`, on the same
/// tracks. New ids, new link/group ids. Where the copies land on other clips they overwrite them
/// (see [`overwrite_under`]); the copies themselves never cut each other.
pub fn duplicate_clips(p: &mut Project, ids: &[String], offset: Tick) -> EditResult<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    for id in ids {
        if p.clip(id).is_none() {
            return Err(EditError::NotFound(format!("clip {id}")));
        }
    }
    let set = closure_of(p, ids);
    for c in &p.clips {
        if set.contains(c.id.as_str()) {
            ensure_unlocked(p, &c.track_id)?;
        }
    }

    let mut w = p.clone();
    let mut remap = Remap::default();
    let mut copies: Vec<Clip> = Vec::new();
    for c in p.clips.iter().filter(|c| set.contains(c.id.as_str())) {
        let mut n = c.clone();
        n.id = new_id();
        n.start = c.start.saturating_add(offset);
        if let Some(l) = &c.link_id {
            n.link_id = Some(remap.link(l));
        }
        if let Some(g) = &c.group_id {
            n.group_id = Some(remap.group(&p.groups, g));
        }
        copies.push(n);
    }
    let new_ids: Vec<String> = copies.iter().map(|c| c.id.clone()).collect();
    w.clips.extend(copies);
    w.groups.extend(remap.new_groups);
    w.clips = overwrite_under(std::mem::take(&mut w.clips), &new_ids);
    check_project(&w)?;
    *p = w;
    Ok(new_ids)
}

// ----- helpers -----

/// Shortest clip the editor creates: one frame of the canvas rate.
fn min_dur(p: &Project) -> Tick {
    p.canvas.fps.frame_duration().max(Tick(1))
}

/// Rejects a tick outside 0..=MAX_TICK.
fn check_time(t: Tick, what: &str) -> EditResult<()> {
    if t < Tick::ZERO || t > Tick(MAX_TICK) {
        return Err(EditError::Invalid(format!("{what} out of range")));
    }
    Ok(())
}

/// End of a clip on the timeline (saturating).
fn end_of(c: &Clip) -> Tick {
    c.start.saturating_add(c.dur)
}

/// `dur * speed` in ticks, rounded; zero for non-positive or non-finite speeds.
fn scale(dur: Tick, speed: f64) -> Tick {
    if !speed.is_finite() || speed <= 0.0 {
        return Tick::ZERO;
    }
    let v = (dur.0 as f64 * speed).round();
    Tick(v.clamp(0.0, MAX_TICK as f64) as i64)
}

/// Fails with `Locked` when the track is locked, `NotFound` when it does not exist.
fn ensure_unlocked(p: &Project, track_id: &str) -> EditResult<()> {
    let t = p
        .track(track_id)
        .ok_or_else(|| EditError::NotFound(format!("track {track_id}")))?;
    if t.locked {
        return Err(EditError::Locked(track_id.to_string()));
    }
    Ok(())
}

/// Ids in the group/link closure of `ids` plus `ids` themselves.
fn closure_of(p: &Project, ids: &[String]) -> HashSet<String> {
    let mut set: HashSet<String> = expand_selection(p, ids).into_iter().collect();
    set.extend(ids.iter().cloned());
    set
}

/// The part of `orig` between timeline ticks `from` and `to` (both inside the clip), as a new
/// clip with the same id. Source offsets follow the playback direction. Fades stay only where
/// they touch the original edge, envelope points are rebased, and the transition is kept only when
/// the piece starts where the original starts. Returns None for an empty or outside range.
fn piece_of(orig: &Clip, from: Tick, to: Tick) -> Option<Clip> {
    let end = end_of(orig);
    if to <= from || from < orig.start || to > end {
        return None;
    }
    if from == orig.start && to == end {
        return Some(orig.clone());
    }
    let dur = to.saturating_sub(from);
    let off_left = from.saturating_sub(orig.start);
    let off_right = end.saturating_sub(to);
    let mut c = orig.clone();
    c.start = from;
    c.dur = dur;
    let consumed = if orig.reverse { off_right } else { off_left };
    c.src_in = orig.src_in.saturating_add(scale(consumed, orig.speed));
    c.audio.fade_in = if from == orig.start {
        orig.audio.fade_in.min(dur)
    } else {
        Tick::ZERO
    };
    c.audio.fade_out = if to == end {
        orig.audio.fade_out.min(dur)
    } else {
        Tick::ZERO
    };
    let rel_to = off_left.saturating_add(dur);
    c.audio.envelope = orig
        .audio
        .envelope
        .iter()
        .filter(|pt| pt.t >= off_left && pt.t < rel_to)
        .map(|pt| EnvPoint {
            t: pt.t.saturating_sub(off_left),
            db: pt.db,
        })
        .collect();
    if from != orig.start {
        c.transition_in = None;
    }
    Some(c)
}

/// Removes the timeline range `[from, to)` from one track's clips (`from == to` only splits the
/// clips that span `from`). Clips on other tracks are kept in place. The first piece of a clip
/// keeps its id, later pieces get new ids.
fn cut(clips: Vec<Clip>, track_id: &str, from: Tick, to: Tick) -> Vec<Clip> {
    cut_except(clips, track_id, from, to, &HashSet::new())
}

/// Lets the clips in `incoming` overwrite what is under them, the way an overwrite edit does.
/// Ids in `incoming` are the clips that were just placed (moved, duplicated, pasted, trimmed or
/// generated). For each incoming clip, in order, its time range is cut out of every clip on its
/// track that is not incoming, so a clip spanning the range is split in two, a clip overlapping
/// an edge is trimmed, and a clip fully covered is removed. Clips in `incoming` are never cut by
/// the others, so a selection moved together keeps its relative positions. If two incoming clips
/// overlap each other, the one later in `incoming` wins and cuts the earlier one.
/// Zero-length leftovers are dropped; touching clips are not affected. Unknown ids are ignored.
pub(crate) fn overwrite_under(clips: Vec<Clip>, incoming: &[String]) -> Vec<Clip> {
    // Ranges come from the clips as they are now; an incoming clip is never cut before its own
    // step, so these stay valid while the steps run.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut ranges: Vec<(String, Tick, Tick, String)> = Vec::with_capacity(incoming.len());
    for id in incoming {
        if !seen.insert(id.as_str()) {
            continue;
        }
        if let Some(c) = clips.iter().find(|c| &c.id == id) {
            ranges.push((c.track_id.clone(), c.start, end_of(c), id.clone()));
        }
    }
    // Step `i` protects the incoming clips `i..`; earlier incoming clips may be cut.
    let mut protected: HashSet<String> = ranges.iter().map(|r| r.3.clone()).collect();
    let mut out = clips;
    for (track_id, from, to, id) in &ranges {
        out = cut_except(out, track_id, *from, *to, &protected);
        protected.remove(id);
    }
    out
}

/// [`cut`] that leaves alone every clip whose id is in `keep`.
fn cut_except(
    clips: Vec<Clip>,
    track_id: &str,
    from: Tick,
    to: Tick,
    keep: &HashSet<String>,
) -> Vec<Clip> {
    let mut out = Vec::with_capacity(clips.len());
    for c in clips {
        if c.track_id != track_id || keep.contains(&c.id) {
            out.push(c);
            continue;
        }
        let end = end_of(&c);
        let left = if c.start < from {
            piece_of(&c, c.start, end.min(from))
        } else {
            None
        };
        let right = if end > to {
            piece_of(&c, c.start.max(to), end)
        } else {
            None
        };
        for (i, mut piece) in [left, right].into_iter().flatten().enumerate() {
            if i > 0 {
                piece.id = new_id();
            }
            out.push(piece);
        }
    }
    out
}

/// Maps old link and group ids to fresh ones, so copies and split halves share new ids.
#[derive(Default)]
struct Remap {
    links: HashMap<String, String>,
    groups: HashMap<String, String>,
    new_groups: Vec<Group>,
}

impl Remap {
    /// The new link id for an old one (the same new id for every member of the old link).
    fn link(&mut self, old: &str) -> String {
        self.links
            .entry(old.to_string())
            .or_insert_with(new_id)
            .clone()
    }

    /// The new group id for an old one, creating a copy of the group (same name and colour).
    fn group(&mut self, groups: &[Group], old: &str) -> String {
        if let Some(id) = self.groups.get(old) {
            return id.clone();
        }
        let (name, color) = match groups.iter().find(|g| g.id == old) {
            Some(g) => (g.name.clone(), g.color.clone()),
            None => ("Group".to_string(), None),
        };
        let id = new_id();
        self.new_groups.push(Group {
            id: id.clone(),
            name,
            color,
        });
        self.groups.insert(old.to_string(), id.clone());
        id
    }
}

/// Checks the invariants every edit must keep: unique clip ids, existing tracks of the right
/// kind, positive lengths inside the tick range and the 4 hour limit, no overlaps, and the track
/// count limits.
fn check_project(p: &Project) -> EditResult<()> {
    let videos = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .count();
    if videos > MAX_VIDEO_TRACKS {
        return Err(EditError::Limit(format!(
            "{videos} video tracks (max {MAX_VIDEO_TRACKS})"
        )));
    }
    let audios = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count();
    if audios > MAX_AUDIO_TRACKS {
        return Err(EditError::Limit(format!(
            "{audios} audio tracks (max {MAX_AUDIO_TRACKS})"
        )));
    }

    let mut seen: HashSet<&str> = HashSet::with_capacity(p.clips.len());
    for c in &p.clips {
        if !seen.insert(c.id.as_str()) {
            return Err(EditError::Invalid(format!("duplicate clip id {}", c.id)));
        }
        let track = p
            .track(&c.track_id)
            .ok_or_else(|| EditError::NotFound(format!("track {}", c.track_id)))?;
        if (c.kind == ClipKind::Audio) != (track.kind == TrackKind::Audio) {
            return Err(EditError::WrongTrackKind(c.id.clone()));
        }
        if c.start < Tick::ZERO || c.src_in < Tick::ZERO || c.dur <= Tick::ZERO {
            return Err(EditError::Invalid(format!(
                "clip {} needs a start and source offset >= 0 and a length > 0",
                c.id
            )));
        }
        if c.start > Tick(MAX_TICK) || c.dur > Tick(MAX_TICK) || c.src_in > Tick(MAX_TICK) {
            return Err(EditError::Invalid(format!(
                "clip {} is outside the tick range",
                c.id
            )));
        }
        if end_of(c) > Tick(MAX_PROJECT_TICKS) {
            return Err(EditError::Limit(format!(
                "clip {} ends after the 4 hour limit",
                c.id
            )));
        }
    }

    for t in &p.tracks {
        let mut spans: Vec<(Tick, Tick)> = p
            .clips
            .iter()
            .filter(|c| c.track_id == t.id)
            .map(|c| (c.start, end_of(c)))
            .collect();
        spans.sort();
        let mut reach = Tick::ZERO;
        for (s, e) in spans {
            if s < reach {
                return Err(EditError::Overlap(t.id.clone()));
            }
            reach = reach.max(e);
        }
    }
    Ok(())
}
