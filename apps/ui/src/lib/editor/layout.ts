// Pure timeline geometry: time <-> x, track rows, hit testing, drag targets, ruler steps.
// No DOM, no stores, no Tauri. All x values are relative to the track area (header excluded);
// all y values are relative to the top of the timeline area (ruler included).

import type { Clip, Project, Tick, Track, TrackKind } from './types';
import { TICKS_PER_SECOND, clipEnd, formatTimecode } from './ticks';

/** Height of the time ruler at the top of the timeline area, px. */
export const RULER_HEIGHT = 28;
/** Vertical gap between consecutive track rows, px. */
export const TRACK_GAP = 2;
/** Pointer distance from a clip edge that still counts as an edge hit, px. */
export const EDGE_HIT_PX = 10;

/** Lowest gain shown on an audio clip's envelope, dB (same mapping as drawClips.ts). */
export const ENVELOPE_DB_MIN = -60;
/** Highest gain shown on an audio clip's envelope, dB. */
export const ENVELOPE_DB_MAX = 12;
/** Audio clips shorter than this, px, show no gain line and take no envelope hits. */
export const ENVELOPE_MIN_HEIGHT_PX = 40;
/** Pointer distance from an envelope point or gain line that counts as a hit, px. */
export const ENVELOPE_HIT_PX = 6;

/** Default track row height when a track carries no usable height. */
const DEFAULT_TRACK_HEIGHT = 56;
/** Minimum drawn clip width, px. */
const MIN_CLIP_WIDTH_PX = 2;

/** Visible window of the timeline. */
export interface View {
  /** Pixels per second of timeline time. */
  pxPerSec: number;
  /** Horizontal scroll offset in px. */
  scrollX: number;
  /** Width of the track area in px. */
  width: number;
  /** Height of the timeline area in px. */
  height: number;
  /** Width of the track header column in px (not used by the geometry itself). */
  headerWidth?: number;
}

/** One track row in display order. */
export interface TrackRow {
  trackId: string;
  kind: TrackKind;
  /** Index of the track in `project.tracks` (project order, not display order). */
  index: number;
  /** Top edge in timeline-area px (already offset by scrollY). */
  top: number;
  /** Row height in px. */
  height: number;
}

/** Pixel rectangle of a clip in timeline-area coordinates. */
export interface ClipRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** What is under the pointer. */
export type HitKind = 'ruler' | 'clip' | 'edge-start' | 'edge-end' | 'gap' | 'track' | 'empty';

/** Result of `hitTest`. Optional fields are set only where meaningful for `kind`. */
export interface HitResult {
  kind: HitKind;
  clipId?: string;
  trackId?: string;
  tick?: Tick;
  /** For `gap`: start of the empty span (end of the previous clip, or 0). */
  gapStart?: Tick;
  /** For `gap`: end of the empty span (start of the next clip). Always greater than `gapStart`. */
  gapEnd?: Tick;
}

/** Envelope point or gain line under the pointer (see `hitEnvelopePoint`). */
export interface EnvelopeHit {
  clipId: string;
  /** Index of the envelope point under the pointer, or null when the gain line was hit. */
  index: number | null;
  /**
   * Absolute timeline tick. For a point hit, the point's time; otherwise the pointer's time
   * inside the clip (where a new point would go).
   */
  tick: Tick;
  /** Gain in dB. For a point hit, the point's gain; otherwise the gain at the pointer. */
  db: number;
}

/** Result of `snapTick`. */
export interface SnapResult<P extends { time: number } = { time: number }> {
  tick: Tick;
  snapped: boolean;
  point?: P;
}

/** Result of `dragMoveTarget`. */
export interface DragMoveTarget {
  /** Time offset to add to every moved clip, in ticks. */
  delta: Tick;
  /** Id of the track row under the pointer when it has the grabbed clip's kind, else null. */
  trackShift: string | null;
}

/** Ruler tick spacing for the current zoom. */
export interface RulerStep {
  majorSec: number;
  minorSec: number;
}

/** A point the playhead, clips or markers can snap to. */
export interface SnapTarget {
  time: number;
  kind: string;
}

/** Ruler ladder: major step in seconds and the number of minor subdivisions (5 or 6). */
const RULER_LADDER: readonly { major: number; subdivisions: number }[] = [
  { major: 0.1, subdivisions: 5 },
  { major: 0.2, subdivisions: 5 },
  { major: 0.5, subdivisions: 5 },
  { major: 1, subdivisions: 5 },
  { major: 2, subdivisions: 5 },
  { major: 5, subdivisions: 5 },
  { major: 10, subdivisions: 5 },
  { major: 15, subdivisions: 5 },
  { major: 30, subdivisions: 6 },
  { major: 60, subdivisions: 6 },
  { major: 120, subdivisions: 6 },
  { major: 300, subdivisions: 5 },
  { major: 600, subdivisions: 5 },
  { major: 1800, subdivisions: 6 },
  { major: 3600, subdivisions: 6 },
];

/** Smallest ruler major step, in px, that the ruler aims for. */
const MIN_MAJOR_PX = 90;

/** Pixels per second, guarded so a bad zoom value never yields NaN or infinity. */
function safePxPerSec(view: View): number {
  return Number.isFinite(view.pxPerSec) && view.pxPerSec > 0 ? view.pxPerSec : 1;
}

/** Clamps a number into [lo, hi]. */
function clampNum(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/** Pointer x (track area) to timeline tick. Clamped to be >= 0. */
export function xToTick(x: number, view: View): Tick {
  const seconds = (x + view.scrollX) / safePxPerSec(view);
  return Math.max(0, Math.round(seconds * TICKS_PER_SECOND));
}

/** Timeline tick to x in the track area. Inverse of `xToTick` up to rounding. */
export function tickToX(t: Tick, view: View): number {
  return (t / TICKS_PER_SECOND) * safePxPerSec(view) - view.scrollX;
}

/** Track row height, falling back to the default for missing or invalid values. */
function rowHeight(track: Track): number {
  const h = track.height;
  return Number.isFinite(h) && h > 0 ? h : DEFAULT_TRACK_HEIGHT;
}

/**
 * Display rows. Video tracks come first, highest layer on top (reverse of project order), then
 * audio tracks in project order. `top` starts at `RULER_HEIGHT - scrollY`.
 */
export function trackLayout(project: Project, scrollY: number): TrackRow[] {
  const indexed = project.tracks.map((track, index) => ({ track, index }));
  const video = indexed.filter((e) => e.track.kind === 'video').reverse();
  const audio = indexed.filter((e) => e.track.kind === 'audio');
  const ordered = [...video, ...audio];

  const rows: TrackRow[] = [];
  let top = RULER_HEIGHT - scrollY;
  for (const { track, index } of ordered) {
    const height = rowHeight(track);
    rows.push({ trackId: track.id, kind: track.kind, index, top, height });
    top += height + TRACK_GAP;
  }
  return rows;
}

/**
 * Horizontal span of a clip in the track area: the drawn span, so the 2 px minimum width of
 * `clipRect` applies here too (a zoomed-out clip is as wide to the pointer as it is on screen).
 */
function clipSpanX(clip: Clip, view: View): { x0: number; x1: number } {
  const x0 = tickToX(clip.start, view);
  const w = Math.max(MIN_CLIP_WIDTH_PX, (clip.dur / TICKS_PER_SECOND) * safePxPerSec(view));
  return { x0, x1: x0 + w };
}

/** Rectangle of a clip on its track row, or null when the track has no row. */
export function clipRect(clip: Clip, rows: readonly TrackRow[], view: View): ClipRect | null {
  const row = rows.find((r) => r.trackId === clip.track_id);
  if (!row) return null;
  const { x0, x1 } = clipSpanX(clip, view);
  return { x: x0, y: row.top, w: x1 - x0, h: row.height };
}

/** Row whose area (including the gap below it) contains y, or undefined. */
function rowAt(rows: readonly TrackRow[], y: number): TrackRow | undefined {
  return rows.find((r) => y >= r.top && y < r.top + r.height + TRACK_GAP);
}

/** Candidate edge hit in `hitTest`. */
interface EdgeCandidate {
  kind: 'edge-start' | 'edge-end';
  clipId: string;
  /** Distance from the pointer to the edge, px. */
  dist: number;
  /** x of the edge, px. */
  edgeX: number;
}

/**
 * True when `c` is the better edge hit. Closer wins. On a tie (touching clips, pointer at the
 * shared edge), the edge on the pointer's side wins: start when x >= the edge, end when x is left
 * of it, so the later clip's start edge can be grabbed.
 */
function betterEdge(c: EdgeCandidate, b: EdgeCandidate, x: number): boolean {
  if (c.dist !== b.dist) return c.dist < b.dist;
  const sideOk = (cand: EdgeCandidate): boolean =>
    cand.kind === 'edge-start' ? x >= cand.edgeX : x <= cand.edgeX;
  const cOk = sideOk(c);
  if (cOk !== sideOk(b)) return cOk;
  return c.kind === 'edge-start' && b.kind !== 'edge-start';
}

/**
 * Hit test in timeline-area coordinates. See `HitKind` for the result kinds. Edge zones are
 * checked before the clip body. Each zone is at most a third of the clip width (EDGE_HIT_PX at
 * most), so the body keeps at least a third of a narrow clip.
 *
 * A `gap` is empty track space with a clip starting after it on the same row: from the end of the
 * previous clip (or tick 0 before the first clip) to the start of the next clip. Empty space with
 * no clip after it (trailing space) and zero-length spans are `track`.
 */
export function hitTest(
  project: Project,
  rows: readonly TrackRow[],
  view: View,
  x: number,
  y: number,
): HitResult {
  if (y < RULER_HEIGHT) {
    return { kind: 'ruler', tick: xToTick(x, view) };
  }
  const row = rowAt(rows, y);
  if (!row) {
    return { kind: 'empty' };
  }

  const tick = xToTick(x, view);
  const onTrack = project.clips.filter((c) => c.track_id === row.trackId);

  let best: EdgeCandidate | null = null;
  for (const clip of onTrack) {
    const { x0, x1 } = clipSpanX(clip, view);
    const edgePx = Math.min(EDGE_HIT_PX, Math.max(0, x1 - x0) / 3);
    const candidates: EdgeCandidate[] = [
      { kind: 'edge-start', clipId: clip.id, dist: Math.abs(x - x0), edgeX: x0 },
      { kind: 'edge-end', clipId: clip.id, dist: Math.abs(x - x1), edgeX: x1 },
    ];
    for (const cand of candidates) {
      if (cand.dist > edgePx) continue;
      if (best === null || betterEdge(cand, best, x)) best = cand;
    }
  }
  if (best) {
    return { kind: best.kind, clipId: best.clipId, trackId: row.trackId, tick };
  }

  for (const clip of onTrack) {
    const { x0, x1 } = clipSpanX(clip, view);
    if (x >= x0 && x <= x1) {
      return { kind: 'clip', clipId: clip.id, trackId: row.trackId, tick };
    }
  }

  return gapAt(onTrack, row.trackId, tick) ?? { kind: 'track', trackId: row.trackId, tick };
}

/**
 * Gap on one track row around `tick`, or null. Null when a clip covers the tick, when no clip
 * starts after it (nothing to close), or when the span is empty.
 */
function gapAt(onTrack: readonly Clip[], trackId: string, tick: Tick): HitResult | null {
  let gapStart: Tick = 0;
  let nextStart: Tick | null = null;
  for (const c of onTrack) {
    const end = c.start + c.dur;
    if (c.start <= tick && tick < end) return null;
    if (end <= tick) gapStart = Math.max(gapStart, end);
    if (c.start > tick) nextStart = nextStart === null ? c.start : Math.min(nextStart, c.start);
  }
  if (nextStart === null || nextStart <= gapStart) return null;
  return { kind: 'gap', trackId, tick, gapStart, gapEnd: nextStart };
}

/** Y of a gain in dB inside a clip rectangle (0 dB near the top, -60 dB at the bottom). */
export function envelopeY(db: number, rect: ClipRect): number {
  const safe = Number.isFinite(db) ? db : 0;
  const frac = clampNum((safe - ENVELOPE_DB_MIN) / (ENVELOPE_DB_MAX - ENVELOPE_DB_MIN), 0, 1);
  return rect.y + rect.h - frac * rect.h;
}

/** Gain in dB at timeline-area y inside a clip rectangle. Inverse of `envelopeY`, clamped. */
export function envelopeDbAt(y: number, rect: ClipRect): number {
  if (!(rect.h > 0)) return ENVELOPE_DB_MIN;
  const frac = (rect.y + rect.h - y) / rect.h;
  return clampNum(ENVELOPE_DB_MIN + frac * (ENVELOPE_DB_MAX - ENVELOPE_DB_MIN), ENVELOPE_DB_MIN, ENVELOPE_DB_MAX);
}

/** Distance from point (px, py) to the segment (ax, ay)-(bx, by). */
function distToSegment(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  const t = len2 > 0 ? clampNum(((px - ax) * dx + (py - ay) * dy) / len2, 0, 1) : 0;
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

/**
 * Envelope hit on an audio clip taller than `ENVELOPE_MIN_HEIGHT_PX`. Uses the same geometry as
 * drawClips.ts: the envelope polyline (or the flat gain line when the clip has no points) is
 * mapped over -60..12 dB across the clip height. A point within `ENVELOPE_HIT_PX` wins and gives
 * its index; otherwise a line within `ENVELOPE_HIT_PX` gives `index: null`. Locked tracks and
 * clips without a row are skipped. Returns the first clip in project order that hits, or null.
 */
export function hitEnvelopePoint(
  project: Project,
  rows: readonly TrackRow[],
  view: View,
  x: number,
  y: number,
  selectedIds?: readonly string[],
): EnvelopeHit | null {
  const selected = selectedIds === undefined ? null : new Set(selectedIds);
  for (const clip of project.clips) {
    if (clip.kind !== 'audio' || clip.dur <= 0) continue;
    const track = project.tracks.find((t) => t.id === clip.track_id);
    if (!track || track.locked) continue;
    const rect = clipRect(clip, rows, view);
    if (!rect || rect.h <= ENVELOPE_MIN_HEIGHT_PX || rect.w <= 0) continue;
    if (x < rect.x - ENVELOPE_HIT_PX || x > rect.x + rect.w + ENVELOPE_HIT_PX) continue;

    const dur = clip.dur;
    const xOf = (t: number): number => rect.x + (clampNum(t, 0, dur) / dur) * rect.w;
    const yOf = (db: number): number => envelopeY(db, rect);
    const points = clip.audio.envelope;

    let bestIndex: number | null = null;
    let bestDist = ENVELOPE_HIT_PX;
    for (const [index, p] of points.entries()) {
      const d = Math.hypot(x - xOf(p.t), y - yOf(p.db));
      if (d <= bestDist) {
        bestDist = d;
        bestIndex = index;
      }
    }
    if (bestIndex !== null) {
      const hit = points[bestIndex];
      if (hit) {
        return { clipId: clip.id, index: bestIndex, tick: clip.start + clampNum(hit.t, 0, dur), db: hit.db };
      }
    }

    const verts: { x: number; y: number }[] = [];
    const first = points[0];
    const last = points[points.length - 1];
    if (first && last) {
      verts.push({ x: rect.x, y: yOf(first.db) });
      for (const p of points) verts.push({ x: xOf(p.t), y: yOf(p.db) });
      verts.push({ x: rect.x + rect.w, y: yOf(last.db) });
    } else {
      const gy = yOf(clip.audio.gain_db);
      verts.push({ x: rect.x, y: gy }, { x: rect.x + rect.w, y: gy });
    }
    let lineDist = Infinity;
    for (let i = 0; i + 1 < verts.length; i++) {
      const a = verts[i];
      const b = verts[i + 1];
      if (!a || !b) continue;
      lineDist = Math.min(lineDist, distToSegment(x, y, a.x, a.y, b.x, b.y));
    }
    // A gain-line hit (no point) counts only on selected clips when a selection is given, so a
    // press near the line of an unselected clip still selects and moves that clip.
    if (lineDist <= ENVELOPE_HIT_PX && (selected === null || selected.has(clip.id))) {
      const frac = clampNum((x - rect.x) / rect.w, 0, 1);
      return {
        clipId: clip.id,
        index: null,
        tick: clip.start + Math.round(frac * dur),
        db: envelopeDbAt(y, rect),
      };
    }
  }
  return null;
}

/**
 * Snap `t` to the nearest point within `thresholdPx` (converted to ticks at `pxPerSec`).
 * Ties keep the earlier point in `points`.
 */
export function snapTick<P extends { time: number } = { time: number }>(
  t: Tick,
  points: readonly P[],
  thresholdPx: number,
  pxPerSec: number,
): SnapResult<P> {
  const pps = Number.isFinite(pxPerSec) && pxPerSec > 0 ? pxPerSec : 1;
  const thresholdTicks = (thresholdPx / pps) * TICKS_PER_SECOND;
  let best: P | undefined;
  let bestDist = Infinity;
  for (const p of points) {
    const d = Math.abs(p.time - t);
    if (d < bestDist) {
      best = p;
      bestDist = d;
    }
  }
  if (best !== undefined && bestDist <= thresholdTicks) {
    return { tick: best.time, snapped: true, point: best };
  }
  return { tick: t, snapped: false };
}

/**
 * Target of a move drag. `dx` is the pointer delta in px, `y` the pointer y in timeline-area
 * coordinates. The delta is clamped so that no moved clip starts before tick 0.
 */
export function dragMoveTarget(
  project: Project,
  rows: readonly TrackRow[],
  view: View,
  clipIds: readonly string[],
  dx: number,
  y: number,
  grabClipId: string,
): DragMoveTarget {
  const pps = safePxPerSec(view);
  const grab = project.clips.find((c) => c.id === grabClipId);
  const movedIds = new Set(clipIds);
  const moved = project.clips.filter((c) => movedIds.has(c.id));

  let delta = Math.round((dx / pps) * TICKS_PER_SECOND);
  if (moved.length > 0) {
    const earliest = Math.min(...moved.map((c) => c.start));
    if (delta < -earliest) delta = -earliest;
  } else {
    delta = 0;
  }

  let trackShift: string | null = null;
  if (grab && y >= RULER_HEIGHT) {
    const grabTrack = project.tracks.find((t) => t.id === grab.track_id);
    const row = rowAt(rows, y);
    if (grabTrack && row && row.kind === grabTrack.kind) {
      trackShift = row.trackId;
    }
  }
  return { delta, trackShift };
}

/** Ruler major and minor step for a zoom level. Aims for at least 90 px between majors. */
export function niceRulerStep(pxPerSec: number): RulerStep {
  const pps = safePxPerSec({ pxPerSec, scrollX: 0, width: 0, height: 0 });
  const entry =
    RULER_LADDER.find((e) => e.major * pps >= MIN_MAJOR_PX) ??
    RULER_LADDER[RULER_LADDER.length - 1] ??
    { major: 3600, subdivisions: 6 };
  return { majorSec: entry.major, minorSec: entry.major / entry.subdivisions };
}

/** Ruler label: `0:05`, `1:30`, `1:02:00`. Fractions of a second add `.d` (tenths). */
export function formatRulerLabel(sec: number): string {
  const safe = Number.isFinite(sec) && sec > 0 ? sec : 0;
  const tenths = Math.round(safe * 10);
  const whole = Math.floor(tenths / 10);
  const frac = tenths % 10;
  const s = whole % 60;
  const m = Math.floor(whole / 60) % 60;
  const h = Math.floor(whole / 3600);
  const pad = (n: number): string => String(n).padStart(2, '0');
  const body = h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
  return frac !== 0 ? `${body}.${frac}` : body;
}

/**
 * Ids of clips whose rectangle intersects the marquee. The rect may be given in any corner
 * order. Clips on locked tracks and clips without a track row are ignored.
 */
export function marqueeSelect(
  project: Project,
  rows: readonly TrackRow[],
  view: View,
  rect: { x0: number; y0: number; x1: number; y1: number },
): string[] {
  const minX = Math.min(rect.x0, rect.x1);
  const maxX = Math.max(rect.x0, rect.x1);
  const minY = Math.min(rect.y0, rect.y1);
  const maxY = Math.max(rect.y0, rect.y1);
  const ids: string[] = [];
  for (const clip of project.clips) {
    const track = project.tracks.find((t) => t.id === clip.track_id);
    if (!track || track.locked) continue;
    const r = clipRect(clip, rows, view);
    if (!r) continue;
    const hit = r.x < maxX && r.x + r.w > minX && r.y < maxY && r.y + r.h > minY;
    if (hit) ids.push(clip.id);
  }
  return ids;
}

/** Tick range currently visible in the track area. */
export function visibleRange(view: View): { startTick: Tick; endTick: Tick } {
  const pps = safePxPerSec(view);
  const startTick = Math.max(0, Math.round((view.scrollX / pps) * TICKS_PER_SECOND));
  const endTick = Math.max(startTick, Math.round(((view.scrollX + view.width) / pps) * TICKS_PER_SECOND));
  return { startTick, endTick };
}

/**
 * Local fallback snap points: timeline start, playhead, edges of enabled clips not excluded,
 * markers and work area ends.
 */
export function snapTargets(
  project: Project,
  excludeIds: readonly string[],
  playhead: Tick,
): SnapTarget[] {
  const targets: SnapTarget[] = [
    { time: 0, kind: 'start' },
    { time: playhead, kind: 'playhead' },
  ];
  const excluded = new Set(excludeIds);
  for (const clip of project.clips) {
    if (!clip.enabled || excluded.has(clip.id)) continue;
    targets.push({ time: clip.start, kind: 'clip-start' });
    targets.push({ time: clip.start + clip.dur, kind: 'clip-end' });
  }
  for (const marker of project.markers) {
    targets.push({ time: marker.time, kind: 'marker' });
  }
  if (project.work_area) {
    targets.push({ time: project.work_area.start, kind: 'work-area-start' });
    targets.push({ time: project.work_area.end, kind: 'work-area-end' });
  }
  return targets;
}

/** Cut point for the Cut tool: the raw time, or a snapped point (see `cutSnapPoint`). */
export interface CutSnap {
  tick: Tick;
  /** x of the cut point in the track area, px. */
  x: number;
  /** What the cut point snapped to, or null when it is the raw pointer time. */
  kind: 'playhead' | 'clip' | null;
}

/** Cut-tool preview over a clip: the snapped cut point plus its timecode label. */
export interface CutPreview extends CutSnap {
  snapped: boolean;
  label: string;
}

/**
 * Cut point for pointer x (track area). Candidates are the playhead and every clip's start and end
 * (enabled or not). The nearest candidate within `snapPx` wins; the playhead is listed first, so it
 * wins ties. `shift` disables snapping. Used by both the cut preview and the click that cuts.
 */
export function cutSnapPoint(
  project: Project,
  x: number,
  shift: boolean,
  playhead: Tick,
  view: View,
  snapPx: number,
): CutSnap {
  const raw = xToTick(x, view);
  if (shift) return { tick: raw, x: tickToX(raw, view), kind: null };
  const candidates: { tick: Tick; kind: 'playhead' | 'clip' }[] = [{ tick: playhead, kind: 'playhead' }];
  for (const clip of project.clips) {
    candidates.push({ tick: clip.start, kind: 'clip' }, { tick: clipEnd(clip), kind: 'clip' });
  }
  let best: { tick: Tick; x: number; kind: 'playhead' | 'clip'; dist: number } | null = null;
  for (const cand of candidates) {
    const cx = tickToX(cand.tick, view);
    const dist = Math.abs(cx - x);
    if (dist > snapPx) continue;
    if (best === null || dist < best.dist) best = { tick: cand.tick, x: cx, kind: cand.kind, dist };
  }
  if (best === null) return { tick: raw, x: tickToX(raw, view), kind: null };
  return { tick: best.tick, x: best.x, kind: best.kind };
}

/**
 * Timecode `HH:MM:SS:FF` of a cut point, at the project frame rate (`canvas.fps`). Never throws:
 * a missing project or rate gives an empty label.
 */
export function cutPreviewLabel(tick: Tick, project: Project): string {
  try {
    const rate = project?.canvas?.fps;
    if (!rate) return '';
    return formatTimecode(tick, rate);
  } catch {
    return '';
  }
}

/**
 * Cut-tool preview for a pointer at (x, y): shown only over a clip (the one a click would cut),
 * at the snapped cut point. Null when the pointer is not over a clip.
 */
export function cutPreviewFor(
  project: Project,
  rows: readonly TrackRow[],
  view: View,
  x: number,
  y: number,
  shift: boolean,
  playhead: Tick,
  snapPx: number,
): CutPreview | null {
  const hit = hitTest(project, rows, view, x, y);
  if (hit.kind !== 'clip' || !hit.clipId) return null;
  const cut = cutSnapPoint(project, x, shift, playhead, view, snapPx);
  return { ...cut, snapped: cut.kind !== null, label: cutPreviewLabel(cut.tick, project) };
}
