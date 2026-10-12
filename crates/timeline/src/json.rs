//! Project (de)serialisation, migration, sanitising and validation. Owner: worker `tl-json`.

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use crate::model::{
    new_id, Canvas, Clip, ClipKind, EnvPoint, MediaKind, MediaRef, Project, Track, TrackKind,
    WorkArea, SCHEMA_VERSION,
};
use crate::time::{Rate, Tick, MAX_PROJECT_TICKS, MAX_TICK, TICKS_PER_SECOND};

/// Largest project file accepted, in bytes.
const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
/// Most video tracks a project may hold.
const MAX_VIDEO_TRACKS: usize = 6;
/// Most audio tracks a project may hold.
const MAX_AUDIO_TRACKS: usize = 8;
/// Most clips a project may hold.
const MAX_CLIPS: usize = 20_000;
/// Most markers a project may hold.
const MAX_MARKERS: usize = 2_000;
/// Most volume envelope points per clip.
const MAX_ENV_POINTS: usize = 64;
/// Shortest and longest transition (0.1 s and 5 s).
const TRANSITION_MIN: Tick = Tick(TICKS_PER_SECOND / 10);
const TRANSITION_MAX: Tick = Tick(5 * TICKS_PER_SECOND);
/// Highest frame rate the canvas accepts.
const MAX_FPS: u64 = 120;

/// Why a project file could not be loaded.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LoadError {
    /// Not valid JSON.
    #[error("not valid JSON: {0}")]
    Json(String),
    /// Valid JSON but not a project object.
    #[error("not a project file: {0}")]
    Shape(String),
    /// Written by a newer, incompatible version.
    #[error("project schema {found} is newer than supported {supported}")]
    TooNew {
        /// Version found in the file.
        found: u32,
        /// Highest version this build reads.
        supported: u32,
    },
    /// The file is larger than the 64 MiB limit.
    #[error("project file too large")]
    TooLarge,
}

/// A problem found by [`validate`].
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// Short machine code ("overlap", "dangling_track", ...).
    pub code: String,
    /// Human message.
    pub message: String,
    /// Related clip, track or media id.
    pub id: Option<String>,
}

/// Serialises pretty-printed JSON (stable field order, newline at end).
pub fn to_json(p: &Project) -> String {
    match serde_json::to_string_pretty(p) {
        Ok(mut s) => {
            s.push('\n');
            s
        }
        Err(_) => "{}\n".to_string(),
    }
}

/// Parses a project. Treats the input as hostile: size limit 64 MiB, unknown fields kept in
/// `extra`, missing fields defaulted, `schema_version` migrated by [`migrate`], then
/// [`sanitize`]d. Never panics.
pub fn from_json(text: &str) -> Result<Project, LoadError> {
    if text.len() > MAX_JSON_BYTES {
        return Err(LoadError::TooLarge);
    }
    let mut value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| LoadError::Json(e.to_string()))?;
    if !value.is_object() {
        return Err(LoadError::Shape("top level must be an object".to_string()));
    }
    migrate(&mut value)?;
    let mut project: Project =
        serde_json::from_value(value).map_err(|e| LoadError::Shape(e.to_string()))?;
    sanitize(&mut project);
    Ok(project)
}

/// Upgrades a parsed JSON value from an older schema to the current one in place. Version 1 is
/// current, so this only stamps a missing version as 1 and rejects versions above 1 (use the
/// error in `from_json`).
pub fn migrate(value: &mut serde_json::Value) -> Result<(), LoadError> {
    let Some(obj) = value.as_object_mut() else {
        return Err(LoadError::Shape("top level must be an object".to_string()));
    };
    let found = obj.get("schema_version").and_then(|v| v.as_u64());
    let Some(found) = found else {
        obj.insert(
            "schema_version".to_string(),
            serde_json::Value::from(SCHEMA_VERSION),
        );
        return Ok(());
    };
    if found > u64::from(SCHEMA_VERSION) {
        return Err(LoadError::TooNew {
            found: u32::try_from(found).unwrap_or(u32::MAX),
            supported: SCHEMA_VERSION,
        });
    }
    // Future migrations (found < SCHEMA_VERSION) would rewrite the object here.
    Ok(())
}

/// Repairs a loaded project so every invariant holds: ticks clamped to 0..=MAX_TICK, speeds and
/// gains clamped to their ranges, clips whose track is missing dropped, clips with media_id that
/// does not exist keep their id but are left (renderer shows missing), duplicate ids regenerated,
/// dangling group/link ids cleared, overlapping clips on a track pushed apart is NOT done (they
/// are reported by [`validate`] instead), at least one video and one audio track present, canvas
/// dimensions clamped to even values within limits.
pub fn sanitize(p: &mut Project) {
    p.schema_version = SCHEMA_VERSION;
    if p.name.trim().is_empty() {
        p.name = "Untitled".to_string();
    }
    sanitize_canvas(&mut p.canvas);
    p.ui_state.playhead = tick_clamp(p.ui_state.playhead);
    p.work_area = sanitize_work_area(p.work_area);

    // Ids that other records refer to are assigned first, so duplicates are renamed on the
    // later records: tracks, media, groups, then clips and markers.
    let mut seen: HashSet<String> = HashSet::new();
    sanitize_tracks(p, &mut seen);
    for m in p.media.iter_mut() {
        m.id = unique_id(&m.id, &mut seen);
        m.duration = tick_clamp(m.duration);
        if !m.fps.is_finite() || m.fps < 0.0 {
            m.fps = 0.0;
        }
    }
    for g in p.groups.iter_mut() {
        g.id = unique_id(&g.id, &mut seen);
    }
    sanitize_clips(p, &mut seen);
    sanitize_markers(p, &mut seen);
}

/// Reports invariant violations without changing the project: overlaps, clips on tracks of the
/// wrong kind, dangling ids, out-of-range values, project longer than 4 hours.
pub fn validate(p: &Project) -> Vec<Issue> {
    let mut out = Vec::new();
    check_ids(p, &mut out);
    check_canvas(&p.canvas, &mut out);

    let mut tracks: HashMap<&str, &Track> = HashMap::new();
    for t in &p.tracks {
        tracks.entry(t.id.as_str()).or_insert(t);
        check_f(&mut out, Some(&t.id), "volume_db", t.volume_db, -60.0, 12.0);
    }
    let mut media: HashMap<&str, &MediaRef> = HashMap::new();
    for m in &p.media {
        media.entry(m.id.as_str()).or_insert(m);
        check_tick(&mut out, Some(&m.id), "duration", m.duration);
    }
    let groups: HashSet<&str> = p.groups.iter().map(|g| g.id.as_str()).collect();
    let mut link_counts: HashMap<&str, usize> = HashMap::new();
    for c in &p.clips {
        if let Some(l) = c.link_id.as_deref() {
            *link_counts.entry(l).or_insert(0) += 1;
        }
    }

    let mut per_track: HashMap<&str, Vec<&Clip>> = HashMap::new();
    let mut end: i64 = 0;
    for c in &p.clips {
        match tracks.get(c.track_id.as_str()).copied() {
            None => push_issue(
                &mut out,
                "dangling_track",
                Some(&c.id),
                format!("clip {} is on missing track {}", c.id, c.track_id),
            ),
            Some(t) => {
                if !track_fits(c.kind, t.kind) {
                    push_issue(
                        &mut out,
                        "wrong_track_kind",
                        Some(&c.id),
                        format!("clip {} ({:?}) is on a {:?} track", c.id, c.kind, t.kind),
                    );
                }
                per_track.entry(t.id.as_str()).or_default().push(c);
            }
        }
        if let Some(mid) = c.media_id.as_deref() {
            match media.get(mid).copied() {
                None => push_issue(
                    &mut out,
                    "dangling_media",
                    Some(&c.id),
                    format!("clip {} uses missing media {}", c.id, mid),
                ),
                Some(m) => check_source(c, m, &mut out),
            }
        }
        if let Some(g) = c.group_id.as_deref() {
            if !groups.contains(g) {
                push_issue(
                    &mut out,
                    "dangling_group",
                    Some(&c.id),
                    format!("clip {} is in missing group {}", c.id, g),
                );
            }
        }
        if let Some(l) = c.link_id.as_deref() {
            if link_counts.get(l).copied().unwrap_or(0) < 2 {
                push_issue(
                    &mut out,
                    "dangling_link",
                    Some(&c.id),
                    format!("clip {} has a link no other clip shares", c.id),
                );
            }
        }
        check_clip_ranges(c, &mut out);
        end = end.max(c.start.0.saturating_add(c.dur.0));
    }
    check_overlaps(per_track, &mut out);

    if end > MAX_PROJECT_TICKS {
        push_issue(
            &mut out,
            "too_long",
            None,
            format!("timeline is {end} ticks long, the limit is {MAX_PROJECT_TICKS}"),
        );
    }
    for m in &p.markers {
        check_tick(&mut out, Some(&m.id), "marker time", m.time);
    }
    if let Some(w) = p.work_area {
        if !tick_ok(w.start) || !tick_ok(w.end) || w.end < w.start {
            push_issue(
                &mut out,
                "range",
                None,
                format!("work area {}..{} is invalid", w.start.0, w.end.0),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Sanitising helpers
// ---------------------------------------------------------------------------------------------

/// Clamps a tick to 0..=MAX_TICK.
fn tick_clamp(t: Tick) -> Tick {
    Tick(t.0.clamp(0, MAX_TICK))
}

/// Clamps a tick to 0..=hi (hi is never negative here).
fn tick_within(t: Tick, hi: Tick) -> Tick {
    Tick(t.0.clamp(0, hi.0.max(0)))
}

/// True when a tick lies in 0..=MAX_TICK.
fn tick_ok(t: Tick) -> bool {
    (0..=MAX_TICK).contains(&t.0)
}

/// Clamps a float to lo..=hi; NaN becomes `nan`.
fn clamp_f(v: f64, lo: f64, hi: f64, nan: f64) -> f64 {
    if v.is_nan() {
        nan
    } else {
        v.clamp(lo, hi)
    }
}

/// Returns `id` when it is non-empty and unused, otherwise a fresh unique id. Records the result
/// in `seen`.
fn unique_id(id: &str, seen: &mut HashSet<String>) -> String {
    let fresh = if !id.is_empty() && !seen.contains(id) {
        id.to_string()
    } else {
        let mut candidate = new_id();
        while seen.contains(&candidate) {
            candidate = new_id();
        }
        candidate
    };
    seen.insert(fresh.clone());
    fresh
}

/// Speed in 0.1..=8.0; 0.0 (freeze) is kept for video clips only.
fn fix_speed(speed: f64, kind: ClipKind) -> f64 {
    if !speed.is_finite() || speed < 0.0 {
        1.0
    } else if speed <= 0.0 {
        if kind == ClipKind::Video {
            0.0
        } else {
            0.1
        }
    } else {
        speed.clamp(0.1, 8.0)
    }
}

fn sanitize_canvas(c: &mut Canvas) {
    c.width = c.width.clamp(128, 8192) & !1;
    c.height = c.height.clamp(128, 8192) & !1;
    if c.fps.num == 0 || c.fps.den == 0 {
        c.fps = Rate::whole(30);
    } else if u64::from(c.fps.num) > MAX_FPS * u64::from(c.fps.den) {
        c.fps = Rate::whole(MAX_FPS as u32);
    }
}

fn sanitize_work_area(w: Option<WorkArea>) -> Option<WorkArea> {
    let w = w?;
    let start = tick_clamp(w.start);
    let end = tick_clamp(w.end);
    if end.0 <= start.0 {
        None
    } else {
        Some(WorkArea { start, end })
    }
}

/// Dedupes track ids, clamps volumes, caps the track counts and makes sure V1 and A1 exist.
fn sanitize_tracks(p: &mut Project, seen: &mut HashSet<String>) {
    for t in p.tracks.iter_mut() {
        t.id = unique_id(&t.id, seen);
        t.volume_db = clamp_f(t.volume_db, -60.0, 12.0, 0.0);
    }
    let mut video = 0usize;
    let mut audio = 0usize;
    p.tracks.retain(|t| match t.kind {
        TrackKind::Video => {
            video += 1;
            video <= MAX_VIDEO_TRACKS
        }
        TrackKind::Audio => {
            audio += 1;
            audio <= MAX_AUDIO_TRACKS
        }
    });
    if !p.tracks.iter().any(|t| t.kind == TrackKind::Video) {
        let id = unique_id("", seen);
        p.tracks.push(Track {
            id,
            kind: TrackKind::Video,
            name: "V1".to_string(),
            ..Track::default()
        });
    }
    if !p.tracks.iter().any(|t| t.kind == TrackKind::Audio) {
        let id = unique_id("", seen);
        p.tracks.push(Track {
            id,
            kind: TrackKind::Audio,
            name: "A1".to_string(),
            ..Track::default()
        });
    }
}

/// Clamps the fields of one clip. Ticks are handled by the caller.
fn sanitize_clip(c: &mut Clip) {
    c.speed = fix_speed(c.speed, c.kind);
    c.opacity = clamp_f(c.opacity, 0.0, 1.0, 1.0);

    c.audio.gain_db = clamp_f(c.audio.gain_db, -60.0, 12.0, 0.0);
    c.audio.pan = clamp_f(c.audio.pan, -1.0, 1.0, 0.0);
    let fade_in = tick_within(c.audio.fade_in, c.dur);
    let fade_out = tick_within(c.audio.fade_out, Tick(c.dur.0 - fade_in.0));
    c.audio.fade_in = fade_in;
    c.audio.fade_out = fade_out;
    sanitize_envelope(&mut c.audio.envelope);

    let tf = &mut c.transform;
    tf.scale = clamp_f(tf.scale, 0.05, 8.0, 1.0);
    tf.pos_x = clamp_f(tf.pos_x, -2.0, 2.0, 0.0);
    tf.pos_y = clamp_f(tf.pos_y, -2.0, 2.0, 0.0);
    tf.rotate_90 %= 4;
    tf.crop.left = clamp_f(tf.crop.left, 0.0, 0.9, 0.0);
    tf.crop.top = clamp_f(tf.crop.top, 0.0, 0.9, 0.0);
    tf.crop.right = clamp_f(tf.crop.right, 0.0, 0.9, 0.0);
    tf.crop.bottom = clamp_f(tf.crop.bottom, 0.0, 0.9, 0.0);

    if let Some(tr) = c.transition_in.as_mut() {
        tr.duration = Tick(tr.duration.0.clamp(TRANSITION_MIN.0, TRANSITION_MAX.0));
    }
}

/// Sorts envelope points by time, drops duplicate times, limits the count and clamps dB.
fn sanitize_envelope(env: &mut Vec<EnvPoint>) {
    for pt in env.iter_mut() {
        pt.t = tick_clamp(pt.t);
        pt.db = clamp_f(pt.db, -60.0, 12.0, 0.0);
    }
    env.sort_by_key(|pt| pt.t);
    env.dedup_by_key(|pt| pt.t);
    env.truncate(MAX_ENV_POINTS);
}

/// True when a clip kind may sit on a track kind.
fn track_fits(clip: ClipKind, track: TrackKind) -> bool {
    match clip {
        ClipKind::Audio => track == TrackKind::Audio,
        _ => track == TrackKind::Video,
    }
}

/// Dedupes group ids, repairs clips (ticks, kinds, references), drops empty groups, caps the
/// clip count.
fn sanitize_clips(p: &mut Project, seen: &mut HashSet<String>) {
    let track_kinds: HashMap<String, TrackKind> =
        p.tracks.iter().map(|t| (t.id.clone(), t.kind)).collect();
    let group_ids: HashSet<String> = p.groups.iter().map(|g| g.id.clone()).collect();

    let clips = std::mem::take(&mut p.clips);
    let mut kept: Vec<Clip> = Vec::new();
    for mut c in clips {
        if kept.len() >= MAX_CLIPS {
            break;
        }
        c.start = tick_clamp(c.start);
        c.dur = tick_clamp(c.dur);
        c.src_in = tick_clamp(c.src_in);
        if c.dur.0 == 0 {
            continue;
        }
        let Some(kind) = track_kinds.get(&c.track_id) else {
            continue;
        };
        if !track_fits(c.kind, *kind) {
            continue;
        }
        c.id = unique_id(&c.id, seen);
        sanitize_clip(&mut c);
        c.group_id = c.group_id.take().filter(|g| group_ids.contains(g));
        c.link_id = c.link_id.take().filter(|l| !l.is_empty());
        kept.push(c);
    }

    // A link shared by fewer than two clips is dangling.
    let mut link_counts: HashMap<String, usize> = HashMap::new();
    for c in &kept {
        if let Some(l) = &c.link_id {
            *link_counts.entry(l.clone()).or_insert(0) += 1;
        }
    }
    for c in kept.iter_mut() {
        let single = c
            .link_id
            .as_ref()
            .is_some_and(|l| link_counts.get(l).copied().unwrap_or(0) < 2);
        if single {
            c.link_id = None;
        }
    }

    // Groups without members are removed.
    let used: HashSet<String> = kept.iter().filter_map(|c| c.group_id.clone()).collect();
    p.groups.retain(|g| used.contains(&g.id));
    p.clips = kept;
}

fn sanitize_markers(p: &mut Project, seen: &mut HashSet<String>) {
    let markers = std::mem::take(&mut p.markers);
    for mut m in markers.into_iter().take(MAX_MARKERS) {
        m.time = tick_clamp(m.time);
        m.id = unique_id(&m.id, seen);
        p.markers.push(m);
    }
}

// ---------------------------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------------------------

fn push_issue(out: &mut Vec<Issue>, code: &str, id: Option<&String>, message: String) {
    out.push(Issue {
        code: code.to_string(),
        message,
        id: id.cloned(),
    });
}

fn check_f(out: &mut Vec<Issue>, id: Option<&String>, name: &str, v: f64, lo: f64, hi: f64) {
    if !(lo..=hi).contains(&v) {
        push_issue(
            out,
            "range",
            id,
            format!("{name} = {v} is outside {lo}..={hi}"),
        );
    }
}

fn check_tick(out: &mut Vec<Issue>, id: Option<&String>, name: &str, t: Tick) {
    if !tick_ok(t) {
        push_issue(
            out,
            "range",
            id,
            format!("{name} = {} is outside 0..={MAX_TICK}", t.0),
        );
    }
}

/// Reports empty and duplicate ids across tracks, media, groups, markers and clips.
fn check_ids(p: &Project, out: &mut Vec<Issue>) {
    let mut seen: HashSet<&str> = HashSet::new();
    let ids = p
        .tracks
        .iter()
        .map(|t| &t.id)
        .chain(p.media.iter().map(|m| &m.id))
        .chain(p.groups.iter().map(|g| &g.id))
        .chain(p.markers.iter().map(|m| &m.id))
        .chain(p.clips.iter().map(|c| &c.id));
    for id in ids {
        if id.is_empty() {
            push_issue(out, "empty_id", None, "an item has an empty id".to_string());
        } else if !seen.insert(id.as_str()) {
            push_issue(
                out,
                "duplicate_id",
                Some(id),
                format!("id {id} is used more than once"),
            );
        }
    }
}

fn check_canvas(c: &Canvas, out: &mut Vec<Issue>) {
    let dim_ok = |v: u32| (128..=8192).contains(&v) && v.is_multiple_of(2);
    if !dim_ok(c.width) || !dim_ok(c.height) {
        push_issue(
            out,
            "range",
            None,
            format!(
                "canvas {}x{} must be even and within 128..=8192",
                c.width, c.height
            ),
        );
    }
    if c.fps.num == 0 || c.fps.den == 0 || u64::from(c.fps.num) > MAX_FPS * u64::from(c.fps.den) {
        push_issue(
            out,
            "range",
            None,
            format!(
                "frame rate {}/{} is outside 1..=120 fps",
                c.fps.num, c.fps.den
            ),
        );
    }
}

/// Checks a video or audio clip's source range against its media.
fn check_source(c: &Clip, m: &MediaRef, out: &mut Vec<Issue>) {
    if !matches!(c.kind, ClipKind::Video | ClipKind::Audio) || m.kind == MediaKind::Image {
        return;
    }
    // Freeze frames (speed 0) and unknown lengths are not checked.
    if c.speed.is_nan() || c.speed <= 0.0 || m.duration.0 <= 0 {
        return;
    }
    let frame = frame_ticks(m.fps);
    let limit = m.duration.0.saturating_add(frame);
    let src_out = c.src_in.0.saturating_add(c.src_len().0);
    if src_out > limit {
        push_issue(
            out,
            "src_out_of_bounds",
            Some(&c.id),
            format!(
                "clip {} reads to {src_out}, past the media end {}",
                c.id, m.duration.0
            ),
        );
    }
}

/// One frame of a media file in ticks (1/30 s when the rate is unknown).
fn frame_ticks(fps: f32) -> i64 {
    let f = f64::from(fps);
    if f.is_finite() && f > 0.0 {
        (TICKS_PER_SECOND as f64 / f).round() as i64
    } else {
        TICKS_PER_SECOND / 30
    }
}

/// Range checks for one clip's own fields.
fn check_clip_ranges(c: &Clip, out: &mut Vec<Issue>) {
    let id = Some(&c.id);
    check_tick(out, id, "start", c.start);
    check_tick(out, id, "dur", c.dur);
    check_tick(out, id, "src_in", c.src_in);

    let freeze = (0.0..=0.0).contains(&c.speed) && c.kind == ClipKind::Video;
    if !freeze {
        check_f(out, id, "speed", c.speed, 0.1, 8.0);
    }
    check_f(out, id, "opacity", c.opacity, 0.0, 1.0);
    check_f(out, id, "gain_db", c.audio.gain_db, -60.0, 12.0);
    check_f(out, id, "pan", c.audio.pan, -1.0, 1.0);
    check_f(out, id, "scale", c.transform.scale, 0.05, 8.0);
    check_f(out, id, "pos_x", c.transform.pos_x, -2.0, 2.0);
    check_f(out, id, "pos_y", c.transform.pos_y, -2.0, 2.0);
    if c.transform.rotate_90 > 3 {
        push_issue(
            out,
            "range",
            id,
            format!("rotate_90 = {} is outside 0..=3", c.transform.rotate_90),
        );
    }
    check_f(out, id, "crop.left", c.transform.crop.left, 0.0, 0.9);
    check_f(out, id, "crop.top", c.transform.crop.top, 0.0, 0.9);
    check_f(out, id, "crop.right", c.transform.crop.right, 0.0, 0.9);
    check_f(out, id, "crop.bottom", c.transform.crop.bottom, 0.0, 0.9);

    check_tick(out, id, "fade_in", c.audio.fade_in);
    check_tick(out, id, "fade_out", c.audio.fade_out);
    if c.audio.fade_in.0.saturating_add(c.audio.fade_out.0) > c.dur.0 {
        push_issue(
            out,
            "range",
            id,
            "fade in and fade out are longer than the clip".to_string(),
        );
    }
    if c.audio.envelope.len() > MAX_ENV_POINTS {
        push_issue(
            out,
            "range",
            id,
            format!("envelope has more than {MAX_ENV_POINTS} points"),
        );
    }
    for pt in &c.audio.envelope {
        check_tick(out, id, "envelope t", pt.t);
        check_f(out, id, "envelope db", pt.db, -60.0, 12.0);
    }
    if let Some(tr) = c.transition_in {
        if !(TRANSITION_MIN.0..=TRANSITION_MAX.0).contains(&tr.duration.0) {
            push_issue(
                out,
                "range",
                id,
                "transition is outside 0.1 s to 5 s".to_string(),
            );
        }
    }
}

/// Reports clips that overlap an earlier clip on the same track. O(n log n) per track.
fn check_overlaps(per_track: HashMap<&str, Vec<&Clip>>, out: &mut Vec<Issue>) {
    for (track, mut clips) in per_track {
        clips.sort_by_key(|c| c.start);
        let mut max_end: Option<i64> = None;
        for c in clips {
            if let Some(m) = max_end {
                if c.start.0 < m {
                    push_issue(
                        out,
                        "overlap",
                        Some(&c.id),
                        format!("clip {} overlaps another clip on track {track}", c.id),
                    );
                }
            }
            let end = c.start.0.saturating_add(c.dur.0);
            max_end = Some(max_end.map_or(end, |m| m.max(end)));
        }
    }
}
