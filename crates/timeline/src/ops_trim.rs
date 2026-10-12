//! Trim-type edits: edge trim (plain/ripple), roll, slip, slide, speed, reverse, join,
//! work area. Owner: worker `tl-trim`.
//!
//! Same contract as `ops_basic` (atomic on error, locked tracks refuse, no overlaps, never panic).
//! Every operation works on a clone of the project and assigns it back only on success.

use serde::{Deserialize, Serialize};

use crate::error::{EditError, EditResult};
use crate::model::{Clip, ClipKind, EnvPoint, Project, WorkArea};
use crate::ops_group::expand_selection;
use crate::time::{Rate, Tick, MAX_TICK, TICKS_PER_SECOND};

/// Which edge of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    /// Left edge.
    Start,
    /// Right edge.
    End,
}

/// Moves one edge of a clip to `new_time` on the timeline (and the edge of its group/link
/// partners by the same delta). Rules: the clip keeps at least one frame (`canvas.fps` frame
/// duration) of length; the source range may not go below 0 or beyond the media duration (images,
/// titles and solids are unbounded); the edge may not cross a neighbour on the same track unless
/// `ripple` is true.
/// - Start edge, not ripple: `start` moves, `dur` shrinks/grows, `src_in` advances by
///   `delta * speed` (reverse: `src_in` unchanged, only dur changes).
/// - End edge, not ripple: `dur` changes only.
/// - Ripple: as above, then every later clip on the same track shifts by the change in length so
///   the track stays contiguous (for a Start-edge ripple the clip's `start` stays put and later
///   clips shift by `-delta`).
///
/// Fades are clamped to the new length; envelope points beyond the end are dropped.
pub fn trim_edge(
    p: &mut Project,
    clip_id: &str,
    edge: Edge,
    new_time: Tick,
    ripple: bool,
) -> EditResult<()> {
    let main = get_clip(p, clip_id)?;
    let old_edge = edge_time(&main, edge);
    let delta = new_time.saturating_sub(old_edge);
    let mut np = p.clone();
    for id in partner_ids(p, clip_id)? {
        let c = get_clip(p, &id)?;
        let target = edge_time(&c, edge).saturating_add(delta);
        trim_one(&mut np, &id, edge, target, ripple)?;
    }
    check_overlaps(&np)?;
    *p = np;
    Ok(())
}

/// Roll edit: moves the cut between two adjacent clips (left ends where right starts) to
/// `new_cut`, lengthening one and shortening the other. Both must be on the same track and
/// touching. Limited by source handles and the one-frame minimum. Partners follow.
pub fn roll_edit(p: &mut Project, left_id: &str, right_id: &str, new_cut: Tick) -> EditResult<()> {
    if left_id == right_id {
        return Err(EditError::Invalid(
            "left and right are the same clip".to_string(),
        ));
    }
    let left = get_clip(p, left_id)?;
    let right = get_clip(p, right_id)?;
    if left.track_id != right.track_id {
        return Err(EditError::Invalid(
            "roll clips must be on the same track".to_string(),
        ));
    }
    let old_cut = end_of(&left);
    if right.start != old_cut {
        return Err(EditError::Invalid("roll clips must touch".to_string()));
    }
    check_start_trim(&right)?;
    let fd = p.canvas.fps.frame_duration();
    let lo = left.start.saturating_add(fd);
    let hi = end_of(&right).saturating_sub(fd);
    if new_cut < lo || new_cut > hi {
        return Err(EditError::Invalid(
            "roll cut is outside the allowed range".to_string(),
        ));
    }
    let delta = new_cut.saturating_sub(old_cut);

    let mut left_partners = partner_ids(p, left_id)?;
    left_partners.retain(|i| i != left_id && i != right_id);
    let mut right_partners = partner_ids(p, right_id)?;
    right_partners.retain(|i| i != left_id && i != right_id);
    let mut left_targets = Vec::new();
    for pid in &left_partners {
        let c = get_clip(p, pid)?;
        left_targets.push((pid.clone(), end_of(&c).saturating_add(delta)));
    }
    let mut right_targets = Vec::new();
    for pid in &right_partners {
        let c = get_clip(p, pid)?;
        right_targets.push((pid.clone(), c.start.saturating_add(delta)));
    }

    let mut np = p.clone();
    let mut nl = left.clone();
    nl.dur = left.dur.saturating_add(delta);
    commit(&mut np, nl)?;

    let mut nr = right.clone();
    nr.start = new_cut;
    nr.dur = right.dur.saturating_sub(delta);
    if !right.reverse {
        nr.src_in = right.src_in.saturating_add(scale_tick(delta, right.speed));
    }
    commit(&mut np, nr)?;

    for (pid, target) in left_targets {
        trim_one(&mut np, &pid, Edge::End, target, false)?;
    }
    for (pid, target) in right_targets {
        trim_one(&mut np, &pid, Edge::Start, target, false)?;
    }
    check_overlaps(&np)?;
    *p = np;
    Ok(())
}

/// Slip: moves the source range of a clip by `delta` (timeline units, converted with speed)
/// without changing its place or length. Clamped to the media's bounds (error if impossible).
/// Group and link partners slip by the same delta so that linked audio stays in sync.
pub fn slip(p: &mut Project, clip_id: &str, delta: Tick) -> EditResult<()> {
    let ids = partner_ids(p, clip_id)?;
    let mut np = p.clone();
    for id in ids {
        let c = get_clip(&np, &id)?;
        let mut nc = c.clone();
        nc.src_in = c.src_in.saturating_add(scale_tick(delta, c.speed));
        commit(&mut np, nc)?;
    }
    *p = np;
    Ok(())
}

/// Slide: moves a clip by `delta` while its neighbours on the same track absorb the change (left
/// neighbour's end and right neighbour's start move by `delta`). Fails if a neighbour would drop
/// below one frame or run out of source handle. Partners move with it.
pub fn slide(p: &mut Project, clip_id: &str, delta: Tick) -> EditResult<()> {
    let c = get_clip(p, clip_id)?;
    if delta == Tick::ZERO {
        return Ok(());
    }
    let left_nb = p
        .clips
        .iter()
        .find(|x| x.track_id == c.track_id && x.id != c.id && end_of(x) == c.start)
        .map(|x| x.id.clone());
    let right_nb = p
        .clips
        .iter()
        .find(|x| x.track_id == c.track_id && x.id != c.id && x.start == end_of(&c))
        .map(|x| x.id.clone());
    let partners = partner_ids(p, clip_id)?;

    let mut np = p.clone();
    if let Some(lid) = left_nb.as_deref() {
        let mut l = get_clip(&np, lid)?;
        l.dur = l.dur.saturating_add(delta);
        commit(&mut np, l)?;
    }
    for id in &partners {
        let is_neighbour =
            Some(id.as_str()) == left_nb.as_deref() || Some(id.as_str()) == right_nb.as_deref();
        if is_neighbour {
            continue;
        }
        let mut x = get_clip(&np, id)?;
        x.start = x.start.saturating_add(delta);
        commit(&mut np, x)?;
    }
    if let Some(rid) = right_nb.as_deref() {
        let mut r = get_clip(&np, rid)?;
        check_start_trim(&r)?;
        r.start = r.start.saturating_add(delta);
        r.dur = r.dur.saturating_sub(delta);
        if !r.reverse {
            r.src_in = r.src_in.saturating_add(scale_tick(delta, r.speed));
        }
        commit(&mut np, r)?;
    }
    check_overlaps(&np)?;
    *p = np;
    Ok(())
}

/// Sets constant speed 0.1..=8.0 on the clip and its group/link partners, keeping the same
/// source range (so `dur` becomes `src_len / speed`). With `ripple`, later clips on each affected
/// track shift by the length change; without, the call fails with Overlap if the clip would run
/// into its right neighbour. Envelope point times and fade lengths are rescaled/clamped.
///
/// Fades scale with the timeline length. After scaling, `fade_in + fade_out` is clamped to the new
/// `dur` by shrinking both fades by the same factor (see [`clamp_fade_pair`]), not by cutting only
/// the fade-out as [`clamp_fades`] does.
pub fn set_speed(p: &mut Project, clip_id: &str, speed: f64, ripple: bool) -> EditResult<()> {
    if !speed.is_finite() || !(0.1..=8.0).contains(&speed) {
        return Err(EditError::Invalid(
            "speed must be between 0.1 and 8.0".to_string(),
        ));
    }
    let fd = p.canvas.fps.frame_duration();
    let ids = partner_ids(p, clip_id)?;
    let mut np = p.clone();
    for id in ids {
        let c = get_clip(&np, &id)?;
        if c.speed <= 0.0 {
            return Err(EditError::Invalid(
                "a freeze clip cannot take a speed".to_string(),
            ));
        }
        let new_dur = Clip::dur_for_src(c.src_len(), speed);
        if new_dur < fd {
            return Err(EditError::Invalid(
                "speed would make the clip shorter than one frame".to_string(),
            ));
        }
        let old_dur = c.dur;
        // Fades and envelope times are timeline offsets from the clip start, so they scale with
        // the timeline length (new_dur / old_dur), not with the source length.
        let ratio = if old_dur.0 > 0 {
            new_dur.0 as f64 / old_dur.0 as f64
        } else {
            1.0
        };
        let old_end = end_of(&c);
        let change = new_dur.saturating_sub(old_dur);

        let mut nc = c.clone();
        nc.speed = speed;
        nc.dur = new_dur;
        let (fi, fo) = clamp_fade_pair(
            scale_tick(c.audio.fade_in, ratio),
            scale_tick(c.audio.fade_out, ratio),
            new_dur,
        );
        nc.audio.fade_in = fi;
        nc.audio.fade_out = fo;
        for e in nc.audio.envelope.iter_mut() {
            e.t = scale_tick(e.t, ratio);
        }
        commit(&mut np, nc)?;
        if ripple && change != Tick::ZERO {
            shift_later(&mut np, &c.track_id, old_end, &id, change)?;
        }
    }
    check_overlaps(&np)?;
    *p = np;
    Ok(())
}

/// Sets or clears reverse playback on the clip and its partners.
pub fn set_reverse(p: &mut Project, clip_id: &str, reverse: bool) -> EditResult<()> {
    let ids = partner_ids(p, clip_id)?;
    let mut np = p.clone();
    for id in ids {
        let mut c = get_clip(&np, &id)?;
        c.reverse = reverse;
        commit(&mut np, c)?;
    }
    *p = np;
    Ok(())
}

/// Merges `right_id` into `left_id` when they are adjacent on one track, use the same media,
/// have the same speed/properties and the source ranges are contiguous (a through-edit). The
/// right clip's `fade_out` replaces the left's. Returns the surviving (left) id.
///
/// Requirements: same track and kind, same media, same speed and reverse flag, `left.end() ==
/// right.start`, contiguous source (`left.src_out() == right.src_in`, or for reverse clips
/// `right.src_out() == left.src_in`), and no transition on the right clip. The merged clip keeps
/// the left's fade-in, takes the right's fade-out, and appends the right's envelope points.
pub fn join_through_edit(p: &mut Project, left_id: &str, right_id: &str) -> EditResult<String> {
    if left_id == right_id {
        return Err(EditError::Invalid(
            "cannot join a clip with itself".to_string(),
        ));
    }
    let left = get_clip(p, left_id)?;
    let right = get_clip(p, right_id)?;
    if left.track_id != right.track_id {
        return Err(EditError::Invalid(
            "join clips must be on the same track".to_string(),
        ));
    }
    if left.kind != right.kind {
        return Err(EditError::Invalid(
            "join clips must be the same kind".to_string(),
        ));
    }
    if left.media_id != right.media_id {
        return Err(EditError::Invalid(
            "join clips must use the same media".to_string(),
        ));
    }
    if (left.speed - right.speed).abs() > f64::EPSILON {
        return Err(EditError::Invalid(
            "join clips must have the same speed".to_string(),
        ));
    }
    if left.reverse != right.reverse {
        return Err(EditError::Invalid(
            "join clips must both be reversed or both forward".to_string(),
        ));
    }
    if end_of(&left) != right.start {
        return Err(EditError::Invalid("join clips must touch".to_string()));
    }
    if right.transition_in.is_some() {
        return Err(EditError::Invalid(
            "the right clip has a transition".to_string(),
        ));
    }
    let contiguous = if left.reverse {
        src_out_of(&right) == left.src_in
    } else {
        src_out_of(&left) == right.src_in
    };
    if !contiguous {
        return Err(EditError::Invalid(
            "source ranges are not contiguous".to_string(),
        ));
    }

    let mut np = p.clone();
    let mut nl = left.clone();
    nl.dur = left.dur.saturating_add(right.dur);
    nl.audio.fade_out = right.audio.fade_out;
    for e in &right.audio.envelope {
        nl.audio.envelope.push(EnvPoint {
            t: e.t.saturating_add(left.dur),
            db: e.db,
        });
    }
    if nl.group_id.is_none() {
        nl.group_id = right.group_id.clone();
    }
    np.clips.retain(|c| c.id != right_id);
    commit(&mut np, nl)?;

    if let Some(link) = right.link_id.as_deref() {
        let count = np
            .clips
            .iter()
            .filter(|c| c.link_id.as_deref() == Some(link))
            .count();
        if count == 1 {
            for c in np.clips.iter_mut() {
                if c.link_id.as_deref() == Some(link) {
                    c.link_id = None;
                }
            }
        }
    }
    check_overlaps(&np)?;
    *p = np;
    Ok(left_id.to_string())
}

/// Sets or clears the work area (in/out). `start < end`, both within 0..=timeline end rounded up
/// to a frame; otherwise `Invalid`.
pub fn set_work_area(p: &mut Project, area: Option<WorkArea>) -> EditResult<()> {
    let a = match area {
        None => {
            p.work_area = None;
            return Ok(());
        }
        Some(a) => a,
    };
    if a.start.0 < 0 || a.start >= a.end {
        return Err(EditError::Invalid(
            "work area needs 0 <= start < end".to_string(),
        ));
    }
    let rate = p.canvas.fps;
    let fd = rate.frame_duration();
    let limit = ceil_to_frame(p.timeline_end().max(fd), rate);
    if a.end > limit {
        return Err(EditError::Invalid(
            "work area ends after the timeline".to_string(),
        ));
    }
    p.work_area = Some(a);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Clone of a clip by id, or `NotFound`.
fn get_clip(p: &Project, id: &str) -> EditResult<Clip> {
    p.clip(id)
        .cloned()
        .ok_or_else(|| EditError::NotFound(id.to_string()))
}

/// `start + dur`, saturating (never panics on hostile values).
fn end_of(c: &Clip) -> Tick {
    c.start.saturating_add(c.dur)
}

/// `src_in + dur * speed`, saturating.
fn src_out_of(c: &Clip) -> Tick {
    c.src_in.saturating_add(c.src_len())
}

/// The clip's time on the requested edge.
fn edge_time(c: &Clip, edge: Edge) -> Tick {
    match edge {
        Edge::Start => c.start,
        Edge::End => end_of(c),
    }
}

/// Multiplies a tick by a float factor, rounding to the nearest tick (non-finite gives zero).
fn scale_tick(t: Tick, factor: f64) -> Tick {
    let v = (t.0 as f64 * factor).round();
    if !v.is_finite() {
        return Tick::ZERO;
    }
    Tick(v.clamp(-(MAX_TICK as f64), MAX_TICK as f64) as i64)
}

/// Rounds up to the next frame boundary at `rate` (never below `t`).
fn ceil_to_frame(t: Tick, rate: Rate) -> Tick {
    if rate.num == 0 || rate.den == 0 || t.0 <= 0 {
        return t;
    }
    let n = i128::from(t.0) * i128::from(rate.num);
    let d = i128::from(rate.den) * i128::from(TICKS_PER_SECOND);
    let frames = (n + d - 1) / d;
    let frames = i64::try_from(frames).unwrap_or(i64::MAX);
    Tick::from_frame(frames, rate).max(t)
}

/// The clip and its group/link partners, the clip first, unknown ids dropped, no duplicates.
fn partner_ids(p: &Project, clip_id: &str) -> EditResult<Vec<String>> {
    if p.clip(clip_id).is_none() {
        return Err(EditError::NotFound(clip_id.to_string()));
    }
    let mut out = vec![clip_id.to_string()];
    for id in expand_selection(p, &[clip_id.to_string()]) {
        if !out.contains(&id) && p.clip(&id).is_some() {
            out.push(id);
        }
    }
    Ok(out)
}

/// Refuses a clip whose start edge cannot move (freeze clips have no source to advance).
fn check_start_trim(c: &Clip) -> EditResult<()> {
    if c.speed <= 0.0 {
        return Err(EditError::Invalid(format!(
            "freeze clip {} cannot be trimmed on its start edge",
            c.id
        )));
    }
    Ok(())
}

/// Source range check: a video or audio clip with a known media file must start at or after 0
/// and end within the media duration (when the duration is known).
fn source_ok(p: &Project, c: &Clip) -> EditResult<()> {
    if !matches!(c.kind, ClipKind::Video | ClipKind::Audio) {
        return Ok(());
    }
    let Some(mid) = c.media_id.as_deref() else {
        return Ok(());
    };
    let Some(m) = p.media_ref(mid) else {
        return Ok(());
    };
    if c.src_in.0 < 0 {
        return Err(EditError::Invalid(format!(
            "clip {} starts before the media",
            c.id
        )));
    }
    if m.duration.0 > 0 && src_out_of(c) > m.duration {
        return Err(EditError::Invalid(format!(
            "clip {} runs past the end of the media",
            c.id
        )));
    }
    Ok(())
}

/// Keeps fades within the clip length (shrinking fade-out first) and drops envelope points at or
/// after the end.
fn clamp_fades(c: &mut Clip) {
    let dur = c.dur;
    let fi = c.audio.fade_in.max(Tick::ZERO).min(dur);
    let mut fo = c.audio.fade_out.max(Tick::ZERO).min(dur);
    if fi.saturating_add(fo) > dur {
        fo = dur.saturating_sub(fi);
    }
    c.audio.fade_in = fi;
    c.audio.fade_out = fo;
    c.audio.envelope.retain(|e| e.t < dur);
}

/// Fits two fades into a clip of length `dur`: each is clamped to `0..=dur`, and when their sum
/// exceeds `dur` both shrink by the same factor (fade-out is then capped at what is left after
/// fade-in, so rounding can never push the sum past `dur`).
fn clamp_fade_pair(fi: Tick, fo: Tick, dur: Tick) -> (Tick, Tick) {
    let fi = fi.max(Tick::ZERO);
    let fo = fo.max(Tick::ZERO);
    let sum = fi.saturating_add(fo);
    if sum <= dur {
        return (fi, fo);
    }
    // sum > dur >= 0, so sum is positive and the factor is finite.
    let factor = dur.0 as f64 / sum.0 as f64;
    let new_fi = scale_tick(fi, factor).min(dur);
    let new_fo = scale_tick(fo, factor).min(dur.saturating_sub(new_fi));
    (new_fi.max(Tick::ZERO), new_fo.max(Tick::ZERO))
}

/// Validates a replacement clip (same id) and writes it into the project. Refuses locked tracks,
/// clips shorter than one frame, clips outside the tick range and source ranges out of bounds.
fn commit(p: &mut Project, mut nc: Clip) -> EditResult<()> {
    let idx = p
        .clips
        .iter()
        .position(|c| c.id == nc.id)
        .ok_or_else(|| EditError::NotFound(nc.id.clone()))?;
    match p.track(&nc.track_id) {
        None => return Err(EditError::NotFound(nc.track_id.clone())),
        Some(t) if t.locked => return Err(EditError::Locked(nc.track_id.clone())),
        Some(_) => {}
    }
    let fd = p.canvas.fps.frame_duration();
    if nc.start.0 < 0 {
        return Err(EditError::Invalid("clip would start before 0".to_string()));
    }
    if nc.dur < fd {
        return Err(EditError::Invalid(
            "clip would be shorter than one frame".to_string(),
        ));
    }
    if end_of(&nc).0 > MAX_TICK {
        return Err(EditError::Limit(
            "clip would end past the tick limit".to_string(),
        ));
    }
    source_ok(p, &nc)?;
    clamp_fades(&mut nc);
    if let Some(slot) = p.clips.get_mut(idx) {
        *slot = nc;
    }
    Ok(())
}

/// Moves a single edge of one clip to `target`, with the same rules as [`trim_edge`]. Works on
/// the given project in place (callers pass a clone).
fn trim_one(p: &mut Project, id: &str, edge: Edge, target: Tick, ripple: bool) -> EditResult<()> {
    let c = get_clip(p, id)?;
    let old_end = end_of(&c);
    let mut nc = c.clone();
    match edge {
        Edge::Start => {
            check_start_trim(&c)?;
            let moved = target.saturating_sub(c.start);
            nc.dur = c.dur.saturating_sub(moved);
            if !ripple {
                nc.start = target;
            }
            if !c.reverse {
                nc.src_in = c.src_in.saturating_add(scale_tick(moved, c.speed));
            }
        }
        Edge::End => {
            nc.dur = target.saturating_sub(c.start);
        }
    }
    let change = nc.dur.saturating_sub(c.dur);
    commit(p, nc)?;
    if ripple && change != Tick::ZERO {
        shift_later(p, &c.track_id, old_end, id, change)?;
    }
    Ok(())
}

/// Shifts every clip on `track_id` that starts at or after `after` (except `exclude`) by `shift`.
fn shift_later(
    p: &mut Project,
    track_id: &str,
    after: Tick,
    exclude: &str,
    shift: Tick,
) -> EditResult<()> {
    let targets: Vec<String> = p
        .clips
        .iter()
        .filter(|c| c.track_id == track_id && c.id != exclude && c.start >= after)
        .map(|c| c.id.clone())
        .collect();
    for id in targets {
        if let Some(c) = p.clip_mut(&id) {
            let ns = c.start.saturating_add(shift);
            if ns.0 < 0 {
                return Err(EditError::Invalid(
                    "ripple would move a clip before 0".to_string(),
                ));
            }
            c.start = ns;
        }
    }
    Ok(())
}

/// Fails with `Overlap` when two clips on one track share time (touching is fine).
fn check_overlaps(p: &Project) -> EditResult<()> {
    for t in &p.tracks {
        let mut v: Vec<&Clip> = p.clips.iter().filter(|c| c.track_id == t.id).collect();
        v.sort_by_key(|c| c.start);
        for pair in v.iter().zip(v.iter().skip(1)) {
            if end_of(pair.0) > pair.1.start {
                return Err(EditError::Overlap(t.id.clone()));
            }
        }
    }
    Ok(())
}
