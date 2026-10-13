// Pure helpers over a RenderPlan for preview scheduling and audio gain (no DOM).
//
// Audio gain maths is a TypeScript port of crates/timeline/src/audiomath.rs; the plan walks follow
// crates/timeline/src/plan.rs. Every function is total: NaN, Infinity and empty plans never throw.

import { MAX_TICK, TICKS_PER_SECOND } from '../ticks';
import type {
  AudioSource,
  EnvPoint,
  FadeCurve,
  PlanSegment,
  RenderPlan,
  Tick,
  VideoLayer,
} from '../types';

/** Default step for `gainRamp`, in seconds. */
const DEFAULT_RAMP_STEP_SEC = 0.05;

/** Most points `gainRamp` returns. */
const MAX_RAMP_POINTS = 400;

/** Clamps a finite tick into 0..=MAX_TICK. Non-finite values give 0. */
function clampTick(v: number): Tick {
  if (!Number.isFinite(v)) return 0;
  return Math.min(MAX_TICK, Math.max(0, Math.round(v)));
}

/** dB to linear amplitude. NaN and values <= -120 dB give 0. Huge values are capped. */
function dbToLin(db: number): number {
  if (Number.isNaN(db) || db <= -120) return 0;
  return 10 ** (Math.min(db, 600) / 20);
}

/** Fade factor 0..=1 at `pos` inside a fade of length `len` (see audiomath.rs `fade_factor`). */
function fadeFactor(curve: FadeCurve, pos: number, len: number): number {
  if (!(len > 0) || !Number.isFinite(len)) return 1;
  const r = Number.isNaN(pos) ? 0 : Math.min(1, Math.max(0, pos / len));
  return curve === 'equal_power' ? Math.sin((Math.PI / 2) * r) : r;
}

/** Envelope gain in dB at `t` relative to the clip start (linear in dB between points). */
function envelopeDbAt(env: EnvPoint[], t: number): number {
  const first = env[0];
  const last = env[env.length - 1];
  if (first === undefined || last === undefined) return 0;
  if (t <= first.t) return first.db;
  if (t >= last.t) return last.db;
  for (let i = 0; i + 1 < env.length; i++) {
    const a = env[i];
    const b = env[i + 1];
    if (a === undefined || b === undefined) continue;
    if (t >= a.t && t <= b.t) {
      const span = b.t - a.t;
      if (!(span > 0)) return b.db;
      return a.db + (b.db - a.db) * ((t - a.t) / span);
    }
  }
  return last.db;
}

/**
 * Index of the first element of the sorted `points` that is `>= t` (or `> t` when `strict`).
 * Returns `points.length` when none qualifies. NaN `t` gives 0.
 */
function lowerBound(points: Tick[], t: Tick, strict: boolean): number {
  let lo = 0;
  let hi = points.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const p = points[mid] ?? 0;
    const before = strict ? p <= t : p < t;
    if (before) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/**
 * Segment of `plan` that contains `t`, or null. Segments are sorted and gap-free, so `t` on a
 * boundary belongs to the segment that starts there. `t >= plan.end` and NaN give null.
 */
export function segmentAt(plan: RenderPlan, t: Tick): PlanSegment | null {
  const segs = plan.segments;
  if (segs.length === 0 || Number.isNaN(t) || !(t < plan.end)) return null;
  // Last segment whose start is <= t.
  let lo = 0;
  let hi = segs.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >>> 1;
    const s = segs[mid];
    if (s === undefined) return null;
    if (s.start <= t) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  if (found < 0) return null;
  const seg = segs[found];
  if (seg === undefined) return null;
  return t < seg.end ? seg : null;
}

/** Sorted, unique segment boundaries of the plan (every segment start and end). */
export function editPoints(plan: RenderPlan): Tick[] {
  const set = new Set<Tick>();
  for (const s of plan.segments) {
    if (Number.isFinite(s.start)) set.add(s.start);
    if (Number.isFinite(s.end)) set.add(s.end);
  }
  return Array.from(set).sort((a, b) => a - b);
}

/**
 * Latest edit point strictly before `t`. Falls back to `plan.start` when there is none (or `t` is
 * NaN).
 */
export function prevEdit(plan: RenderPlan, t: Tick): Tick {
  if (Number.isNaN(t)) return clampTick(plan.start);
  const points = editPoints(plan);
  const idx = lowerBound(points, t, false) - 1;
  const p = idx >= 0 ? points[idx] : undefined;
  return p !== undefined ? p : clampTick(plan.start);
}

/**
 * Earliest edit point strictly after `t`. Falls back to `plan.end` when there is none (or `t` is
 * NaN).
 */
export function nextEdit(plan: RenderPlan, t: Tick): Tick {
  if (Number.isNaN(t)) return clampTick(plan.end);
  const points = editPoints(plan);
  const p = points[lowerBound(points, t, true)];
  return p !== undefined ? p : clampTick(plan.end);
}

/**
 * Source time of a layer or audio source at timeline time `t`, given the source time at the
 * segment start (`src_time`). Forward adds `(t - segStart) * speed`; reverse subtracts it.
 * Freeze (speed 0 or invalid) returns `src_time`. The result is clamped into 0..=MAX_TICK.
 */
export function sourceTimeFor(
  layer: VideoLayer | AudioSource,
  segStart: Tick,
  t: Tick,
): Tick {
  const base = layer.src_time;
  if (!Number.isFinite(layer.speed) || layer.speed <= 0 || !Number.isFinite(segStart) || !Number.isFinite(t)) {
    return clampTick(base);
  }
  const off = Math.round((t - segStart) * layer.speed);
  return clampTick(layer.reverse ? base - off : base + off);
}

/** Media that becomes active in the plan within `horizon` ticks after `t`. */
export interface UpcomingMedia {
  mediaId: string;
  /** Start of the segment in which the media first becomes active. */
  startsAt: Tick;
  /** Source time at `startsAt`. */
  srcTime: Tick;
}

/** Index of the first segment whose start is greater than `t` (segments are sorted). */
function firstSegmentAfter(plan: RenderPlan, t: Tick): number {
  const segs = plan.segments;
  let lo = 0;
  let hi = segs.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const s = segs[mid];
    if (s !== undefined && s.start <= t) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/**
 * Media with a video layer that starts within `(t, t + horizon]` and is not shown at `t`, for
 * preloading the pooled <video> elements (one per media id, keeping the earliest). Audio sources of
 * every stream, the default one included, have their own elements, see {@link upcomingStreams}.
 * The result is sorted by `startsAt`. A non-positive or NaN horizon, or NaN `t`, gives an empty list.
 */
export function upcoming(plan: RenderPlan, t: Tick, horizon: Tick): UpcomingMedia[] {
  if (Number.isNaN(t) || !(horizon > 0)) return [];
  const limit = t + horizon;
  const cur = segmentAt(plan, t);
  const busy = new Set<string>();
  if (cur) {
    for (const l of cur.video) if (l.media_id !== null) busy.add(l.media_id);
  }
  const seen = new Map<string, UpcomingMedia>();
  for (let i = firstSegmentAfter(plan, t); i < plan.segments.length; i++) {
    const seg = plan.segments[i];
    if (seg === undefined || seg.start > limit) break;
    for (const layer of seg.video) {
      addUpcoming(seen, busy, layer.media_id, seg.start, layer.src_time);
    }
  }
  return Array.from(seen.values()).sort((a, b) => a.startsAt - b.startsAt);
}

/** An audio stream (default stream 0 included) that becomes active soon. */
export interface UpcomingStream {
  mediaId: string;
  /** Source stream index (0 is the default stream, played from the original media file). */
  stream: number;
  startsAt: Tick;
  srcTime: Tick;
}

/**
 * Audio streams (every source_track, 0 included) that are not active at `t` but start within
 * `(t, t + horizon]`, earliest first, one entry per (media, stream). The key is `<mediaId>#<stream>`.
 */
export function upcomingStreams(plan: RenderPlan, t: Tick, horizon: Tick): UpcomingStream[] {
  if (Number.isNaN(t) || !(horizon > 0)) return [];
  const limit = t + horizon;
  const cur = segmentAt(plan, t);
  const busy = new Set<string>();
  if (cur) for (const a of cur.audio) busy.add(`${a.media_id}#${a.source_track}`);
  const seen = new Map<string, UpcomingStream>();
  for (let i = firstSegmentAfter(plan, t); i < plan.segments.length; i++) {
    const seg = plan.segments[i];
    if (seg === undefined || seg.start > limit) break;
    for (const src of seg.audio) {
      const key = `${src.media_id}#${src.source_track}`;
      if (busy.has(key) || seen.has(key)) continue;
      seen.set(key, { mediaId: src.media_id, stream: src.source_track, startsAt: seg.start, srcTime: src.src_time });
    }
  }
  return Array.from(seen.values());
}

/** Records `mediaId` in `seen` unless it is busy or already recorded. */
function addUpcoming(
  seen: Map<string, UpcomingMedia>,
  busy: Set<string>,
  mediaId: string | null,
  startsAt: Tick,
  srcTime: Tick,
): void {
  if (mediaId === null || busy.has(mediaId) || seen.has(mediaId)) return;
  seen.set(mediaId, { mediaId, startsAt, srcTime });
}

/** Media ids of the segment active at `t`, as used by `activeMedia` and `upcoming`. */
function activeNow(plan: RenderPlan, t: Tick): { video: string[]; audio: string[] } {
  const seg = segmentAt(plan, t);
  if (seg === null) return { video: [], audio: [] };
  const video = new Set<string>();
  for (const l of seg.video) {
    if (l.media_id !== null) video.add(l.media_id);
  }
  const audio = new Set<string>();
  for (const a of seg.audio) audio.add(a.media_id);
  return { video: Array.from(video), audio: Array.from(audio) };
}

/** Media ids that are active at `t` (empty when `t` is outside the plan). */
export function activeMedia(plan: RenderPlan, t: Tick): { video: string[]; audio: string[] } {
  return activeNow(plan, t);
}

/**
 * Linear gain of an audio source at absolute timeline time `t`, including the track fader.
 * Port of `clip_gain_at` (clip gain, envelope, fade in, fade out) times `db_to_lin(track_db)`.
 * Muted clips and times outside the clip give the clamped result (0 when muted).
 */
export function audioGainAt(src: AudioSource, t: Tick): number {
  const a = src.audio;
  if (a.muted) return 0;
  const dur = Number.isFinite(src.clip_dur) && src.clip_dur > 0 ? src.clip_dur : 0;
  const relRaw = t - src.clip_start;
  const rel = Number.isNaN(relRaw) ? 0 : Math.min(dur, Math.max(0, relRaw));
  const gainDb = a.gain_db + envelopeDbAt(a.envelope, rel);
  let gain = dbToLin(gainDb);
  if (rel < a.fade_in) gain *= fadeFactor(a.fade_curve, rel, a.fade_in);
  const remaining = dur - rel;
  if (remaining < a.fade_out) gain *= fadeFactor(a.fade_curve, remaining, a.fade_out);
  return gain * dbToLin(src.track_db);
}

/**
 * Left and right gains for a pan in -1..=1 (constant power, centre gives 1.0 on both sides).
 * NaN counts as centre; out-of-range values are clamped.
 */
export function panGains(pan: number): { l: number; r: number } {
  const p = Number.isNaN(pan) ? 0 : Math.min(1, Math.max(-1, pan));
  const angle = ((p + 1) * Math.PI) / 4;
  const scale = Math.SQRT2;
  return { l: Math.cos(angle) * scale, r: Math.sin(angle) * scale };
}

/** One sample of a gain curve: seconds after `from`, and the linear gain there. */
export interface GainPoint {
  offsetSec: number;
  gain: number;
}

/**
 * Samples `audioGainAt` along `[from, to]` every `stepSec` seconds, including both ends. At most
 * 400 points are returned, so long ranges are sampled more coarsely. Returns [] when `to < from`
 * or the inputs are not finite.
 */
export function gainRamp(
  src: AudioSource,
  from: Tick,
  to: Tick,
  stepSec: number = DEFAULT_RAMP_STEP_SEC,
): GainPoint[] {
  if (!Number.isFinite(from) || !Number.isFinite(to) || to < from) return [];
  const step = stepSec > 0 && Number.isFinite(stepSec) ? stepSec : DEFAULT_RAMP_STEP_SEC;
  const span = to - from;
  const stepTicks = Math.max(1, Math.round(step * TICKS_PER_SECOND));
  // Number of intervals: at least one step, at most MAX_RAMP_POINTS - 1.
  const steps = span === 0 ? 0 : Math.min(MAX_RAMP_POINTS - 1, Math.max(1, Math.ceil(span / stepTicks)));
  const out: GainPoint[] = [];
  for (let i = 0; i <= steps; i++) {
    const t = i === steps ? to : from + Math.round((span * i) / steps);
    out.push({ offsetSec: (t - from) / TICKS_PER_SECOND, gain: audioGainAt(src, t) });
  }
  return out;
}

/**
 * Progress 0..=1 of a layer's transition at `t`, computed as `(t - clip_start) / duration`.
 * Returns null when there is no transition, its duration is not positive, or `t` is at or past
 * its end.
 */
export function transitionProgress(layer: VideoLayer, t: Tick): number | null {
  const tr = layer.transition;
  if (tr === null || !(tr.duration > 0) || !Number.isFinite(tr.duration)) return null;
  const elapsed = t - layer.clip_start;
  if (Number.isNaN(elapsed) || elapsed >= tr.duration) return null;
  return Math.min(1, Math.max(0, elapsed / tr.duration));
}

/**
 * Opacity of a layer at `t`: `layer.opacity` (clamped to 0..=1) times the transition progress
 * when a transition is active. Crossfades and dips both use the progress.
 */
export function layerOpacityAt(layer: VideoLayer, t: Tick): number {
  const base = Number.isFinite(layer.opacity) ? Math.min(1, Math.max(0, layer.opacity)) : 0;
  const p = transitionProgress(layer, t);
  return p === null ? base : base * p;
}

/**
 * Colour and amount of a dip transition at `t`. The amount is `1 - 2 * progress` while progress is
 * below 0.5, otherwise 0. Returns null for crossfades, when no transition is active, or past its end.
 */
export function dipAmount(
  layer: VideoLayer,
  t: Tick,
): { color: 'black' | 'white'; amount: number } | null {
  const tr = layer.transition;
  if (tr === null || tr.kind === 'crossfade') return null;
  const p = transitionProgress(layer, t);
  if (p === null) return null;
  return {
    color: tr.kind === 'dip_to_white' ? 'white' : 'black',
    amount: p < 0.5 ? 1 - 2 * p : 0,
  };
}
