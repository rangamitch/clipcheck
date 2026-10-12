//! Range and clipboard edits: paste, lift/extract, gap closing, freeze frame.
//! Owner: worker `tl-range`.
//!
//! Same contract as `ops_basic` (atomic on error, locked tracks refuse, no overlaps, never panic).
//! Every operation works on a clone of the project and replaces the project only when the result
//! passes [`check`], so a refused edit leaves the project unchanged.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{EditError, EditResult};
use crate::model::{new_id, Clip, ClipKind, EnvPoint, Group, Project, TrackKind};
use crate::ops_basic::PlaceMode;
use crate::time::{Tick, MAX_PROJECT_TICKS, MAX_TICK};

/// Pastes clip copies (the clipboard) so the earliest starts at `at`. Clips go onto their original
/// track when it still exists, else `track_id`, else the first track of the right kind. Mode
/// semantics as in [`place_media`] (Overwrite or Insert). Returns the new ids.
///
/// `Append` anchors on the pasted clips' own target tracks, as [`place_media`] does: the end of the
/// video target tracks when the paste holds any non-audio clip (audio lanes are not consulted),
/// otherwise the end of the first audio target track. See [`append_anchor`].
pub fn paste_clips(
    p: &mut Project,
    clips: &[Clip],
    at: Tick,
    track_id: Option<&str>,
    mode: PlaceMode,
) -> EditResult<Vec<String>> {
    if clips.is_empty() {
        return Ok(Vec::new());
    }
    check_time(at, "paste position")?;
    for c in clips {
        if c.start.0 < 0 || c.dur.0 <= 0 {
            return Err(EditError::Invalid(format!(
                "clip {} has a bad time range",
                c.id
            )));
        }
    }
    let earliest = clips.iter().map(|c| c.start).min().unwrap_or(Tick::ZERO);

    let mut targets: Vec<String> = Vec::with_capacity(clips.len());
    for c in clips {
        targets.push(pick_track(p, c, track_id, clip_track_kind(c.kind))?);
    }
    let mut uniq = targets.clone();
    uniq.sort();
    uniq.dedup();
    for t in &uniq {
        ensure_unlocked(p, t)?;
    }

    // Append lands after the last clip of the affected tracks; Insert and Overwrite use `at`.
    let insert = mode != PlaceMode::Overwrite;
    let anchor = if mode == PlaceMode::Append {
        append_anchor(p, clips, &targets)
    } else {
        at
    };

    // Links and groups are remapped inside the pasted set only. A link or group with one pasted
    // member is dropped.
    let mut link_counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut group_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for c in clips {
        if let Some(l) = c.link_id.as_deref() {
            *link_counts.entry(l).or_insert(0) += 1;
        }
        if let Some(g) = c.group_id.as_deref() {
            *group_counts.entry(g).or_insert(0) += 1;
        }
    }
    let mut paste_map = Remap::new(&p.groups);
    let mut ids: Vec<String> = Vec::with_capacity(clips.len());
    let mut new_clips: Vec<Clip> = Vec::with_capacity(clips.len());
    for (c, tid) in clips.iter().zip(targets) {
        let id = new_id();
        let start = anchor
            .checked_add(c.start.saturating_sub(earliest))
            .ok_or_else(limit)?;
        let mut n = c.clone();
        n.id = id.clone();
        n.track_id = tid;
        n.start = start;
        n.link_id = match c.link_id.as_deref() {
            Some(l) if link_counts.get(l).copied().unwrap_or(0) >= 2 => Some(paste_map.link(l)),
            _ => None,
        };
        n.group_id = match c.group_id.as_deref() {
            Some(g) if group_counts.get(g).copied().unwrap_or(0) >= 2 => Some(paste_map.group(g)),
            _ => None,
        };
        ids.push(id);
        new_clips.push(n);
    }

    let mut q = p.clone();
    let mut cut_map = Remap::new(&p.groups);
    if insert {
        // Split whatever spans the insert point, then push everything at or after it right by
        // the length of the pasted block.
        let span_end = new_clips.iter().map(Clip::end).max().unwrap_or(anchor);
        let shift = span_end.saturating_sub(anchor);
        for t in &uniq {
            cut_range(&mut q.clips, t, anchor, anchor, &mut cut_map);
        }
        for x in q.clips.iter_mut() {
            if x.start >= anchor && uniq.contains(&x.track_id) {
                x.start = x.start.saturating_add(shift);
            }
        }
    } else {
        for n in &new_clips {
            cut_range(&mut q.clips, &n.track_id, n.start, n.end(), &mut cut_map);
        }
    }
    q.clips.extend(new_clips);
    q.groups.extend(paste_map.new_groups);
    q.groups.extend(cut_map.new_groups);
    tidy(&mut q);
    check(&q)?;
    *p = q;
    Ok(ids)
}

/// Removes the time range `start..end` from the given tracks (all unlocked tracks when
/// `track_ids` is empty), splitting clips at the edges. `ripple` = Extract (later clips move left
/// by `end - start`); false = Lift (leaves a gap).
pub fn remove_range(
    p: &mut Project,
    start: Tick,
    end: Tick,
    track_ids: &[String],
    ripple: bool,
) -> EditResult<()> {
    check_time(start, "range start")?;
    check_time(end, "range end")?;
    if start >= end {
        return Err(EditError::Invalid(
            "range start must be before its end".to_string(),
        ));
    }
    let targets: Vec<String> = if track_ids.is_empty() {
        p.tracks
            .iter()
            .filter(|t| !t.locked)
            .map(|t| t.id.clone())
            .collect()
    } else {
        let mut v: Vec<String> = Vec::with_capacity(track_ids.len());
        for id in track_ids {
            ensure_unlocked(p, id)?;
            if !v.contains(id) {
                v.push(id.clone());
            }
        }
        v
    };

    let len = end - start;
    let mut q = p.clone();
    let mut map = Remap::new(&p.groups);
    for t in &targets {
        cut_range(&mut q.clips, t, start, end, &mut map);
    }
    if ripple {
        for x in q.clips.iter_mut() {
            if x.start >= end && targets.contains(&x.track_id) {
                x.start = x.start.saturating_sub(len);
            }
        }
    }
    q.groups.extend(map.new_groups);
    tidy(&mut q);
    check(&q)?;
    *p = q;
    Ok(())
}

/// Closes the gap that contains `at` on a track (shifts later clips left). No-op `Ok` when `at`
/// is not in a gap.
pub fn close_gap(p: &mut Project, track_id: &str, at: Tick) -> EditResult<()> {
    ensure_unlocked(p, track_id)?;
    check_time(at, "gap position")?;
    let spans: Vec<(Tick, Tick)> = p
        .clips
        .iter()
        .filter(|c| c.track_id == track_id)
        .map(|c| (c.start, c.end()))
        .collect();
    // `at` sits on a clip: no gap.
    if spans.iter().any(|&(s, e)| (s..e).contains(&at)) {
        return Ok(());
    }
    // No clip after `at`: nothing to close.
    let Some(gap_end) = spans
        .iter()
        .filter(|&&(s, _)| s > at)
        .map(|&(s, _)| s)
        .min()
    else {
        return Ok(());
    };
    let gap_start = spans
        .iter()
        .filter(|&&(_, e)| e <= at)
        .map(|&(_, e)| e)
        .max()
        .unwrap_or(Tick::ZERO);
    let len = gap_end.saturating_sub(gap_start);
    if len.0 <= 0 {
        return Ok(());
    }
    let mut q = p.clone();
    for x in q.clips.iter_mut() {
        if x.track_id == track_id && x.start >= gap_end {
            x.start = x.start.saturating_sub(len);
        }
    }
    check(&q)?;
    *p = q;
    Ok(())
}

/// Closes every gap on a track (clips become contiguous from the first clip's start).
pub fn close_all_gaps(p: &mut Project, track_id: &str) -> EditResult<()> {
    ensure_unlocked(p, track_id)?;
    let mut order: Vec<(Tick, usize)> = p
        .clips
        .iter()
        .enumerate()
        .filter(|(_, c)| c.track_id == track_id)
        .map(|(i, c)| (c.start, i))
        .collect();
    order.sort();
    let Some(&(first, _)) = order.first() else {
        return Ok(());
    };
    let mut q = p.clone();
    let mut cursor = first;
    for (_, i) in order {
        if let Some(c) = q.clips.get_mut(i) {
            c.start = cursor;
            cursor = cursor.saturating_add(c.dur);
        }
    }
    check(&q)?;
    *p = q;
    Ok(())
}

/// Freeze frame: splits the Video clip at `at` (like [`split_at`] on that clip only, no partners)
/// and inserts between the halves a new Video clip of length `dur` that shows the single source
/// frame at `at`. A freeze clip has `speed == 0.0` (so `src_len()` is zero) and `src_in` = the
/// source time at `at`; name "Freeze". Later clips on the track shift right by `dur`. Returns the
/// freeze clip id.
pub fn freeze_frame(p: &mut Project, clip_id: &str, at: Tick, dur: Tick) -> EditResult<String> {
    let idx = p
        .clips
        .iter()
        .position(|c| c.id == clip_id)
        .ok_or_else(|| EditError::NotFound(clip_id.to_string()))?;
    let c = p
        .clips
        .get(idx)
        .cloned()
        .ok_or_else(|| EditError::NotFound(clip_id.to_string()))?;
    if c.kind != ClipKind::Video {
        return Err(EditError::Invalid(
            "freeze frame needs a video clip".to_string(),
        ));
    }
    ensure_unlocked(p, &c.track_id)?;
    check_time(at, "freeze position")?;
    if !(c.start < at && at < c.end()) {
        return Err(EditError::Invalid(
            "freeze position must be inside the clip".to_string(),
        ));
    }
    let frame = p.canvas.fps.frame_duration();
    if dur.0 <= 0 || dur < frame {
        return Err(EditError::Invalid(
            "freeze duration is shorter than one frame".to_string(),
        ));
    }

    // Source time shown at `at`: forward clips advance from src_in, reversed ones run from the
    // end of their source range.
    let src_time = if c.reverse {
        c.src_in.saturating_add(src_offset(c.end() - at, c.speed))
    } else {
        c.src_in.saturating_add(src_offset(at - c.start, c.speed))
    };

    let end = c.end();
    let (left, mut right) = split_pieces(&c, at);
    right.link_id = None;
    right.group_id = c.group_id.clone();

    let mut q = p.clone();
    let slot = q
        .clips
        .get_mut(idx)
        .ok_or_else(|| EditError::NotFound(clip_id.to_string()))?;
    *slot = left;
    for x in q.clips.iter_mut() {
        if x.track_id == c.track_id && x.start >= end {
            x.start = x.start.checked_add(dur).ok_or_else(limit)?;
        }
    }
    right.start = at.checked_add(dur).ok_or_else(limit)?;
    let id = new_id();
    q.clips.push(right);
    q.clips.push(Clip {
        id: id.clone(),
        track_id: c.track_id.clone(),
        kind: ClipKind::Video,
        media_id: c.media_id.clone(),
        name: "Freeze".to_string(),
        start: at,
        dur,
        src_in: src_time,
        speed: 0.0,
        reverse: false,
        enabled: c.enabled,
        color: c.color.clone(),
        transform: c.transform,
        opacity: c.opacity,
        ..Clip::default()
    });
    tidy(&mut q);
    check(&q)?;
    *p = q;
    Ok(id)
}

// ---- private helpers ----

/// Error for a tick that would leave the 0..=MAX_TICK range.
fn limit() -> EditError {
    EditError::Limit("tick range".to_string())
}

/// Rejects a time outside 0..=MAX_TICK.
fn check_time(t: Tick, what: &str) -> EditResult<()> {
    if !(0..=MAX_TICK).contains(&t.0) {
        return Err(EditError::Invalid(format!("{what} is out of range")));
    }
    Ok(())
}

/// Track kind a clip belongs on.
fn clip_track_kind(kind: ClipKind) -> TrackKind {
    if kind == ClipKind::Audio {
        TrackKind::Audio
    } else {
        TrackKind::Video
    }
}

/// Refuses an unknown or locked track.
fn ensure_unlocked(p: &Project, track_id: &str) -> EditResult<()> {
    match p.track(track_id) {
        None => Err(EditError::NotFound(track_id.to_string())),
        Some(t) if t.locked => Err(EditError::Locked(track_id.to_string())),
        Some(_) => Ok(()),
    }
}

/// End of the last clip on a track (zero when the track is empty).
fn track_end(p: &Project, track_id: &str) -> Tick {
    p.clips
        .iter()
        .filter(|c| c.track_id == track_id)
        .map(Clip::end)
        .max()
        .unwrap_or(Tick::ZERO)
}

/// Start for an `Append` paste. `targets[i]` is the track chosen for `clips[i]`. When any pasted
/// clip goes on a video track, the anchor is the end of the latest video target track (audio
/// lanes are not consulted). An audio-only paste anchors on the end of the first audio target.
/// Like `place_media`, an occupied lane is refused by the overlap check, never shifted around.
fn append_anchor(p: &Project, clips: &[Clip], targets: &[String]) -> Tick {
    let video_targets: Vec<&str> = clips
        .iter()
        .zip(targets.iter())
        .filter(|(c, _)| clip_track_kind(c.kind) == TrackKind::Video)
        .map(|(_, t)| t.as_str())
        .collect();
    if !video_targets.is_empty() {
        return video_targets
            .iter()
            .map(|t| track_end(p, t))
            .max()
            .unwrap_or(Tick::ZERO);
    }
    targets
        .first()
        .map_or(Tick::ZERO, |t| track_end(p, t.as_str()))
}

/// Chooses the track for a pasted clip: its own track, then `wanted`, then the first unlocked
/// track of the right kind. The first two must have the right kind; they are not checked for
/// lock here (the caller refuses locked targets).
fn pick_track(
    p: &Project,
    clip: &Clip,
    wanted: Option<&str>,
    kind: TrackKind,
) -> EditResult<String> {
    let found = p
        .track(&clip.track_id)
        .filter(|t| t.kind == kind)
        .or_else(|| wanted.and_then(|id| p.track(id)).filter(|t| t.kind == kind));
    if let Some(t) = found {
        return Ok(t.id.clone());
    }
    p.tracks
        .iter()
        .find(|t| t.kind == kind && !t.locked)
        .map(|t| t.id.clone())
        .ok_or_else(|| EditError::WrongTrackKind(format!("no unlocked {kind:?} track")))
}

/// Whole-project invariants after an edit: known tracks, right clip kinds, positive durations
/// inside the tick range, project length within the limit, no overlaps on a track.
fn check(p: &Project) -> EditResult<()> {
    for c in &p.clips {
        let track = p
            .track(&c.track_id)
            .ok_or_else(|| EditError::NotFound(c.track_id.clone()))?;
        if c.start.0 < 0 || c.dur.0 <= 0 {
            return Err(EditError::Invalid(format!(
                "clip {} has a bad time range",
                c.id
            )));
        }
        if c.end().0 > MAX_TICK {
            return Err(limit());
        }
        if track.kind != clip_track_kind(c.kind) {
            return Err(EditError::WrongTrackKind(c.track_id.clone()));
        }
    }
    if p.timeline_end().0 > MAX_PROJECT_TICKS {
        return Err(EditError::Limit("project length".to_string()));
    }
    for t in &p.tracks {
        let mut spans: Vec<(Tick, Tick)> = p
            .clips
            .iter()
            .filter(|c| c.track_id == t.id)
            .map(|c| (c.start, c.end()))
            .collect();
        spans.sort();
        for pair in spans.windows(2) {
            if let [a, b] = pair {
                if b.0 < a.1 {
                    return Err(EditError::Overlap(t.id.clone()));
                }
            }
        }
    }
    Ok(())
}

/// Drops link ids held by fewer than two clips and groups with no members.
fn tidy(q: &mut Project) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for c in &q.clips {
        if let Some(l) = &c.link_id {
            *counts.entry(l.clone()).or_insert(0) += 1;
        }
    }
    for c in q.clips.iter_mut() {
        let orphan = match &c.link_id {
            Some(l) => counts.get(l).copied().unwrap_or(0) < 2,
            None => false,
        };
        if orphan {
            c.link_id = None;
        }
    }
    let used: BTreeSet<String> = q.clips.iter().filter_map(|c| c.group_id.clone()).collect();
    q.groups.retain(|g| used.contains(&g.id));
}

/// Old-id to new-id maps for links and groups created during one edit, plus the new group
/// entries to add to the project.
struct Remap {
    links: BTreeMap<String, String>,
    groups: BTreeMap<String, String>,
    src_groups: Vec<Group>,
    new_groups: Vec<Group>,
}

impl Remap {
    fn new(src_groups: &[Group]) -> Remap {
        Remap {
            links: BTreeMap::new(),
            groups: BTreeMap::new(),
            src_groups: src_groups.to_vec(),
            new_groups: Vec::new(),
        }
    }

    /// New link id for an old one (the same old id always gives the same new id).
    fn link(&mut self, old: &str) -> String {
        if let Some(n) = self.links.get(old) {
            return n.clone();
        }
        let n = new_id();
        self.links.insert(old.to_string(), n.clone());
        n
    }

    /// New group id for an old one. The new group copies name and colour from the old group.
    fn group(&mut self, old: &str) -> String {
        if let Some(n) = self.groups.get(old) {
            return n.clone();
        }
        let mut g = match self.src_groups.iter().find(|g| g.id == old) {
            Some(src) => Group {
                id: String::new(),
                name: src.name.clone(),
                color: src.color.clone(),
            },
            None => Group::default(),
        };
        g.id = new_id();
        let n = g.id.clone();
        self.new_groups.push(g);
        self.groups.insert(old.to_string(), n.clone());
        n
    }
}

/// Cuts the span `a..b` out of every clip on `track_id` that overlaps it. A clip spanning the
/// whole span is split in two: the right piece gets a new id and new link and group ids (through
/// `map`). Pieces that are only trimmed keep their id.
fn cut_range(clips: &mut Vec<Clip>, track_id: &str, a: Tick, b: Tick, map: &mut Remap) {
    let old = std::mem::take(clips);
    let mut out: Vec<Clip> = Vec::with_capacity(old.len() + 1);
    for c in old {
        if c.track_id != track_id || c.end() <= a || c.start >= b {
            out.push(c);
            continue;
        }
        let has_left = c.start < a;
        let has_right = b < c.end();
        if has_left && has_right {
            let mut l = c.clone();
            let keep = a - c.start;
            trim_tail(&mut l, keep);
            let mut r = c;
            let cut = b - r.start;
            trim_head(&mut r, cut);
            l.audio.fade_out = Tick::ZERO;
            r.audio.fade_in = Tick::ZERO;
            r.id = new_id();
            r.transition_in = None;
            r.link_id = r.link_id.take().map(|x| map.link(&x));
            r.group_id = r.group_id.take().map(|g| map.group(&g));
            out.push(l);
            out.push(r);
        } else if has_left {
            let mut l = c;
            let keep = a - l.start;
            trim_tail(&mut l, keep);
            out.push(l);
        } else if has_right {
            let mut r = c;
            let cut = b - r.start;
            trim_head(&mut r, cut);
            out.push(r);
        }
        // Otherwise the clip lies fully inside the span and is dropped.
    }
    *clips = out;
}

/// Splits a clip at `at` (strictly inside). The left piece keeps the id; the right piece gets a
/// new id, no transition and no link. Fades: fade-in stays left, fade-out moves right. Group
/// membership is copied as is; callers remap it when needed.
fn split_pieces(c: &Clip, at: Tick) -> (Clip, Clip) {
    let off = at - c.start;
    let mut left = c.clone();
    trim_tail(&mut left, off);
    let mut right = c.clone();
    trim_head(&mut right, off);
    right.id = new_id();
    right.transition_in = None;
    right.link_id = None;
    left.audio.fade_out = Tick::ZERO;
    right.audio.fade_in = Tick::ZERO;
    (left, right)
}

/// Removes the first `cut` of a clip's timeline length. Source advances for forward clips;
/// reversed clips keep their source start. `cut` must be inside `0..dur`.
fn trim_head(c: &mut Clip, cut: Tick) {
    let new_dur = c.dur.saturating_sub(cut);
    let env = slice_env(&c.audio.envelope, cut, c.dur);
    if !c.reverse {
        c.src_in = c.src_in.saturating_add(src_offset(cut, c.speed));
    }
    c.audio.envelope = env;
    c.audio.fade_in = c.audio.fade_in.saturating_sub(cut).min(new_dur);
    c.audio.fade_out = c.audio.fade_out.min(new_dur);
    c.start = c.start.saturating_add(cut);
    c.dur = new_dur;
}

/// Keeps only the first `keep` of a clip's timeline length. Source advances for reversed clips;
/// forward clips keep their source start. `keep` must be inside `0..dur`.
fn trim_tail(c: &mut Clip, keep: Tick) {
    let cut = c.dur.saturating_sub(keep);
    if c.reverse {
        c.src_in = c.src_in.saturating_add(src_offset(cut, c.speed));
    }
    c.audio.envelope = slice_env(&c.audio.envelope, Tick::ZERO, keep);
    c.audio.fade_in = c.audio.fade_in.min(keep);
    c.audio.fade_out = c.audio.fade_out.min(keep);
    c.dur = keep;
}

/// Source length consumed by a timeline offset at a speed (same rounding as `Clip::src_len`).
fn src_offset(off: Tick, speed: f64) -> Tick {
    let v = (off.0 as f64 * speed).round();
    Tick(v.clamp(0.0, MAX_TICK as f64) as i64)
}

/// Volume at time `t` (linear between points, held at the ends). Points must be sorted.
fn env_db_at(points: &[EnvPoint], t: Tick) -> f64 {
    let mut prev: Option<EnvPoint> = None;
    for pt in points {
        if pt.t > t {
            return match prev {
                None => pt.db,
                Some(a) => {
                    let span = (pt.t.0 - a.t.0) as f64;
                    if span <= 0.0 {
                        a.db
                    } else {
                        let f = (t.0 - a.t.0) as f64 / span;
                        a.db + (pt.db - a.db) * f
                    }
                }
            };
        }
        prev = Some(*pt);
    }
    prev.map_or(0.0, |a| a.db)
}

/// Envelope points for the window `from..to` of a clip, re-based so the window starts at zero.
/// Boundary points are interpolated. An empty envelope stays empty.
fn slice_env(points: &[EnvPoint], from: Tick, to: Tick) -> Vec<EnvPoint> {
    if points.is_empty() || to <= from {
        return Vec::new();
    }
    let mut out = vec![EnvPoint {
        t: Tick::ZERO,
        db: env_db_at(points, from),
    }];
    for pt in points {
        if pt.t > from && pt.t < to {
            out.push(EnvPoint {
                t: pt.t - from,
                db: pt.db,
            });
        }
    }
    out.push(EnvPoint {
        t: to - from,
        db: env_db_at(points, to),
    });
    out
}
