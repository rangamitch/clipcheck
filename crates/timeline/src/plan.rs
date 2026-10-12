//! Render plan: the single definition of "what the output is" (D69). Preview and export both read
//! this. Types are final (contract); [`derive_plan`] flattens the project over a range.

use serde::{Deserialize, Serialize};

use crate::model::{
    Canvas, Clip, ClipAudio, ClipKind, Project, Rgba, TextSpec, Track, TrackKind, Transform,
    Transition,
};
use crate::time::{Tick, MAX_TICK};

/// One visible video layer during a segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoLayer {
    /// Clip id.
    pub clip_id: String,
    /// Kind (Video, Image, Title or Solid).
    pub kind: ClipKind,
    /// Media id (None for Title/Solid).
    pub media_id: Option<String>,
    /// Stacking order: index of the clip's track among video tracks (0 = bottom).
    pub z: u32,
    /// Source time at the START of the segment.
    pub src_time: Tick,
    /// Speed (0.0 = freeze: always show `src_time`).
    pub speed: f64,
    /// Reverse playback.
    pub reverse: bool,
    /// Geometry.
    pub transform: Transform,
    /// Opacity at the start of the segment (1.0 unless a transition/fade is active, see
    /// `fade`).
    pub opacity: f64,
    /// Transition into this clip when the segment lies inside it: progress is computed by the
    /// renderer as `(t - clip_start) / transition.duration`.
    pub transition: Option<Transition>,
    /// Clip start on the timeline (for transition progress).
    pub clip_start: Tick,
    /// Title text.
    pub text: Option<TextSpec>,
    /// Solid colour.
    pub solid: Option<Rgba>,
}

/// One audible audio source during a segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSource {
    /// Clip id.
    pub clip_id: String,
    /// Timeline audio track id (for per-track export and track fader).
    pub track_id: String,
    /// Index of the track among audio tracks (0 = first).
    pub track_index: u32,
    /// Media id.
    pub media_id: String,
    /// Source audio stream.
    pub source_track: u32,
    /// Source time at the START of the segment.
    pub src_time: Tick,
    /// Speed (> 0).
    pub speed: f64,
    /// Reverse playback.
    pub reverse: bool,
    /// Clip start on the timeline.
    pub clip_start: Tick,
    /// Clip length on the timeline.
    pub clip_dur: Tick,
    /// Clip audio properties (gain, pan, fades, envelope, mono).
    pub audio: ClipAudio,
    /// Track fader in dB.
    pub track_db: f64,
}

/// A span of the timeline during which the set of layers and sources is constant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanSegment {
    /// Start (inclusive).
    pub start: Tick,
    /// End (exclusive).
    pub end: Tick,
    /// Visible video layers, bottom first.
    pub video: Vec<VideoLayer>,
    /// Audible sources.
    pub audio: Vec<AudioSource>,
}

/// The flattened timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPlan {
    /// Canvas copy.
    pub canvas: Canvas,
    /// Range covered (start of first segment .. end of last).
    pub start: Tick,
    /// End of the range.
    pub end: Tick,
    /// Segments in time order, gap-free (empty segments have empty `video` and `audio`).
    pub segments: Vec<PlanSegment>,
}

/// Whether a track takes part in the plan: video tracks that are hidden or muted and audio tracks
/// that are muted do not; when any track of the same kind is solo, only solo tracks do.
fn track_active(p: &Project, track: &Track) -> bool {
    match track.kind {
        TrackKind::Video => {
            if track.hidden || track.muted {
                return false;
            }
        }
        TrackKind::Audio => {
            if track.muted {
                return false;
            }
        }
    }
    let any_solo = p.tracks.iter().any(|t| t.kind == track.kind && t.solo);
    !any_solo || track.solo
}

/// Whether a clip may appear in the plan, ignoring its track.
fn clip_usable(p: &Project, clip: &Clip) -> bool {
    if !clip.enabled {
        return false;
    }
    if clip.kind == ClipKind::Audio && clip.audio.muted {
        return false;
    }
    if let Some(mid) = clip.media_id.as_deref() {
        let ok = p.media_ref(mid).is_some_and(|m| !m.missing);
        if !ok {
            return false;
        }
    }
    true
}

/// Clips that may appear in the plan, each with its track, in project clip order.
fn usable_clips(p: &Project) -> Vec<(&Clip, &Track)> {
    p.clips
        .iter()
        .filter_map(|c| {
            let t = p.track(&c.track_id)?;
            if track_active(p, t) && clip_usable(p, c) {
                Some((c, t))
            } else {
                None
            }
        })
        .collect()
}

/// Index of a track among video tracks.
fn video_index(p: &Project, track_id: &str) -> Option<u32> {
    let i = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .position(|t| t.id == track_id)?;
    u32::try_from(i).ok()
}

/// Index of a track among audio tracks.
fn audio_index(p: &Project, track_id: &str) -> Option<u32> {
    let i = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .position(|t| t.id == track_id)?;
    u32::try_from(i).ok()
}

/// Flattens `p` over `range` (None = 0..timeline end). Segment boundaries are every enabled clip's
/// start and end inside the range plus the range ends. Rules: clips on muted/hidden tracks, and on
/// non-solo tracks when any track of the same kind is solo, are skipped; disabled clips are skipped;
/// audio clips with `audio.muted` are skipped; media marked `missing` is skipped (the renderer
/// shows black/silence). `z` orders by track position among video tracks. Layers inside a segment
/// are sorted by `z` ascending. `src_time` accounts for speed and reverse.
///
/// The range is clamped into `0..=timeline end`. A start at or after the end gives an empty plan.
/// Segments are not merged, even when adjacent segments have identical content.
pub fn derive_plan(p: &Project, range: Option<(Tick, Tick)>) -> RenderPlan {
    let tl_end = p.timeline_end();
    let (start, end) = match range {
        None => (Tick::ZERO, tl_end),
        Some((a, b)) => (a.clamp(Tick::ZERO, tl_end), b.clamp(Tick::ZERO, tl_end)),
    };
    let mut plan = RenderPlan {
        canvas: p.canvas.clone(),
        start,
        end,
        segments: Vec::new(),
    };
    if start >= end {
        return plan;
    }

    let clips = usable_clips(p);

    // Cut points: the range ends plus every usable clip edge strictly inside the range.
    let mut cuts: Vec<Tick> = vec![start, end];
    for (c, _) in &clips {
        for x in [c.start, c.end()] {
            if x > start && x < end {
                cuts.push(x);
            }
        }
    }
    cuts.sort();
    cuts.dedup();

    for w in cuts.windows(2) {
        let &[a, b] = w else { continue };
        let mut video = Vec::new();
        let mut audio = Vec::new();
        for (c, t) in &clips {
            if !(c.start <= a && c.end() >= b) {
                continue;
            }
            match (c.kind, t.kind) {
                (ClipKind::Audio, TrackKind::Audio) => {
                    let Some(media_id) = c.media_id.clone() else {
                        continue;
                    };
                    let Some(track_index) = audio_index(p, &t.id) else {
                        continue;
                    };
                    audio.push(AudioSource {
                        clip_id: c.id.clone(),
                        track_id: t.id.clone(),
                        track_index,
                        media_id,
                        source_track: c.audio.source_track,
                        src_time: source_time_at(c, a),
                        speed: c.speed,
                        reverse: c.reverse,
                        clip_start: c.start,
                        clip_dur: c.dur,
                        audio: c.audio.clone(),
                        track_db: t.volume_db,
                    });
                }
                (
                    ClipKind::Video | ClipKind::Image | ClipKind::Title | ClipKind::Solid,
                    TrackKind::Video,
                ) => {
                    let Some(z) = video_index(p, &t.id) else {
                        continue;
                    };
                    video.push(VideoLayer {
                        clip_id: c.id.clone(),
                        kind: c.kind,
                        media_id: c.media_id.clone(),
                        z,
                        src_time: source_time_at(c, a),
                        speed: c.speed,
                        reverse: c.reverse,
                        transform: c.transform,
                        opacity: c.opacity,
                        transition: c.transition_in,
                        clip_start: c.start,
                        text: c.text.clone(),
                        solid: c.solid,
                    });
                }
                _ => {}
            }
        }
        // Stable sort keeps project clip order within one z.
        video.sort_by_key(|v| v.z);
        plan.segments.push(PlanSegment {
            start: a,
            end: b,
            video,
            audio,
        });
    }
    plan
}

/// Source time for timeline time `t` inside a clip: `src_in + (t - start) * speed` (forward) or
/// `src_out - (t - start) * speed` (reverse); `src_in` for freeze clips (speed 0).
///
/// `t` is clamped into `start..=end`. Speeds that are zero, negative or not finite are treated as
/// freeze. The result is clamped into `0..=MAX_TICK`.
pub fn source_time_at(clip: &Clip, t: Tick) -> Tick {
    if !clip.speed.is_finite() || clip.speed <= 0.0 {
        return clip.src_in;
    }
    let t = t.clamp(clip.start, clip.end());
    let elapsed = i128::from(t.0) - i128::from(clip.start.0);
    // f64 cast from i128 is fine for elapsed <= 2^53; the float-to-int cast saturates.
    let off = ((elapsed as f64) * clip.speed).round() as i128;
    let base = if clip.reverse {
        i128::from(clip.src_out().0) - off
    } else {
        i128::from(clip.src_in.0) + off
    };
    Tick(base.clamp(0, i128::from(MAX_TICK)) as i64)
}
