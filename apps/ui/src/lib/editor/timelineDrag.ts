// Pure drag state machines for the editor timeline. No DOM, no stores, no Tauri.
//
// Flow: `beginDrag` picks a state from a hit test on pointer down, `updateDrag` recomputes the
// preview on every pointer move (and returns it in the same shape the canvas needs), and
// `finishDrag` returns the commands to dispatch on pointer up. Every state keeps its latest preview
// values, so `finishDrag` needs no pointer event.
//
// Coordinates are timeline-area px as produced by layout.ts: x relative to the track area, y
// relative to the top of the ruler. The caller converts client coordinates.

import type { Clip, ClipMove, Command, EnvPoint, Project, Rate, Tick, TrackKind } from './types';
import {
  MAX_PROJECT_TICKS,
  TICKS_PER_SECOND,
  clampTick,
  frameDurationTicks,
  snapToFrame,
} from './ticks';
import {
  ENVELOPE_DB_MAX,
  ENVELOPE_DB_MIN,
  clipRect,
  dragMoveTarget,
  envelopeDbAt,
  marqueeSelect,
  snapTick,
  xToTick,
} from './layout';
import type { ClipRect, EnvelopeHit, HitResult, TrackRow, View } from './layout';

export { hitEnvelopePoint } from './layout';
export type { EnvelopeHit } from './layout';

/** Pointer data the drag logic reads. x and y are timeline-area px. */
export interface PointerLike {
  x: number;
  y: number;
  shiftKey: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}

/** Movement in px before a press counts as a drag rather than a click. */
export const DRAG_THRESHOLD_PX = 3;

/** Snap threshold in px (design doc F-TL-10). */
const SNAP_PX = 8;
/** Fallback minimum clip length when the frame rate is unusable: one frame at 30 fps. */
const FALLBACK_MIN_TICKS = Math.round(TICKS_PER_SECOND / 30);

/** Move drag: the selection (and its group/link partners) slides in time and across tracks. */
export interface MoveDrag {
  kind: 'move';
  grabClipId: string;
  /** Selected clips that are moved (partners are expanded by the backend). */
  clipIds: string[];
  originX: number;
  originY: number;
  /** Latest time offset in ticks. */
  delta: Tick;
  /** Latest track offset among tracks of the grabbed clip's kind (0 = no change). */
  trackOffset: number;
  /**
   * Alt was held on a clip body at press: finish copies the clips (`duplicate_clips`) instead of
   * moving them. Snapping stays on for a duplicating drag.
   */
  duplicate: boolean;
}

/** Trim drag on one clip edge. */
export interface TrimDrag {
  kind: 'trim-start' | 'trim-end';
  clipId: string;
  originX: number;
  /** Edge time before the drag, in ticks. */
  originTime: Tick;
  /** Latest edge time (frame aligned, inside the allowed range). */
  newTime: Tick;
  /** Shift held: ripple trim. */
  ripple: boolean;
}

/** Roll drag on the edit point between two adjacent clips. */
export interface RollDrag {
  kind: 'roll';
  leftId: string;
  rightId: string;
  originX: number;
  originCut: Tick;
  /** Latest edit point (frame aligned, inside the allowed range). */
  newCut: Tick;
}

/** Playhead drag (started on the ruler). */
export interface PlayheadDrag {
  kind: 'playhead';
}

/** Marquee selection on empty track space. */
export interface MarqueeDrag {
  kind: 'marquee';
  originX: number;
  originY: number;
  x: number;
  y: number;
  /** Selection before the drag, kept when `additive`. */
  base: string[];
  /** Shift or Ctrl held: the marquee adds to the base selection. */
  additive: boolean;
}

/** Hand-tool or middle-button pan. The caller applies `panScroll` to the scroll store. */
export interface PanDrag {
  kind: 'pan';
  originX: number;
  originY: number;
  scrollX0: number;
  scrollY0: number;
}

/**
 * Drag of one audio envelope point (clip-relative time, dB). For a point inserted by
 * `beginEnvelopeHitDrag`, `base` already contains the new point, so a press without movement
 * sends no command.
 */
export interface EnvelopeDrag {
  kind: 'envelope-point';
  clipId: string;
  index: number;
  originX: number;
  originY: number;
  origin: EnvPoint;
  /** Envelope before the drag (with the inserted point, for an insert). */
  base: EnvPoint[];
  /** Envelope with the dragged point moved. */
  points: EnvPoint[];
}

/** Drag of a clip's audio fade handle. */
export interface FadeDrag {
  kind: 'fade-handle';
  clipId: string;
  edge: 'in' | 'out';
  originX: number;
  origin: Tick;
  value: Tick;
}

/** Every drag the timeline can run. */
export type DragState =
  | MoveDrag
  | TrimDrag
  | RollDrag
  | PlayheadDrag
  | MarqueeDrag
  | PanDrag
  | EnvelopeDrag
  | FadeDrag;

/** A clip rectangle drawn while dragging. */
export interface DragGhost {
  clipId: string;
  rect: ClipRect;
}

/** Marquee rectangle in timeline-area px. */
export interface MarqueeRect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** Inputs of `updateDrag` and `finishDrag`. */
export interface DragContext {
  project: Project;
  rows: readonly TrackRow[];
  view: View;
  /** Points the moving edge can snap to. The caller excludes the dragged clips. */
  snapPoints: { time: number }[];
  /** Project snapping mode. Alt disables snapping during move and trim drags. */
  snapEnabled: boolean;
}

/** Result of `updateDrag`. */
export interface DragUpdate {
  state: DragState;
  ghosts: DragGhost[];
  /** Snap target to draw as a guide, or null. */
  snapTick: Tick | null;
  /** Playhead position to show (playhead drags only), or null. */
  previewTick: Tick | null;
  /** Marquee rectangle to draw (marquee drags only), or null. */
  marquee: MarqueeRect | null;
  /** Selection to show (marquee drags only), or null. Ids are not group-expanded. */
  selectionIds: string[] | null;
  /** Fade lengths to draw while a fade handle is dragged. */
  fadePreview?: { clipId: string; fadeIn: Tick; fadeOut: Tick };
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

/** Clamps a number into [lo, hi]. */
function clampNum(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/** Clip with the given id, or undefined. */
function findClip(project: Project, id: string): Clip | undefined {
  return project.clips.find((c) => c.id === id);
}

/** Kind of the track with the given id, or null. */
function trackKindOf(project: Project, trackId: string): TrackKind | null {
  return project.tracks.find((t) => t.id === trackId)?.kind ?? null;
}

/** True when the clip sits on a locked track (or on no known track). */
function isLocked(project: Project, clip: Clip): boolean {
  const track = project.tracks.find((t) => t.id === clip.track_id);
  return !track || track.locked;
}

/** Pixel delta to ticks at `pxPerSec`. Guards against a bad zoom value. */
function pxToTicks(dx: number, pxPerSec: number): Tick {
  const pps = Number.isFinite(pxPerSec) && pxPerSec > 0 ? pxPerSec : 1;
  return Math.round((dx / pps) * TICKS_PER_SECOND);
}

/** Minimum clip length for trims: one frame of the project rate. */
function minClipTicks(project: Project): Tick {
  return frameDurationTicks(project.canvas.fps) || FALLBACK_MIN_TICKS;
}

/** Removes duplicate ids, keeping the first occurrence. */
function dedupe(ids: readonly string[]): string[] {
  return Array.from(new Set(ids));
}

/** Time of a clip edge: its start or its end. */
function edgeTime(c: Clip, edge: 'start' | 'end'): Tick {
  return edge === 'start' ? c.start : c.start + c.dur;
}

/**
 * Clamps `t` into [lo, hi] and snaps it to the frame grid of `rate`. When the snapped value falls
 * outside the range it steps one frame back inside. Returns `lo` when the range is empty.
 */
function alignedClamp(t: Tick, lo: Tick, hi: Tick, rate: Rate): Tick {
  const fd = frameDurationTicks(rate) || FALLBACK_MIN_TICKS;
  let v = snapToFrame(clampTick(t, lo, hi), rate);
  if (v > hi) v -= fd;
  if (v < lo) v += fd;
  return clampTick(v, lo, hi);
}

/** Clips that move with `ids`: the clips themselves plus group and link partners. */
export function movingClips(project: Project, ids: readonly string[]): Clip[] {
  const selected = project.clips.filter((c) => ids.includes(c.id));
  const groups = new Set<string>();
  const links = new Set<string>();
  for (const c of selected) {
    if (c.group_id) groups.add(c.group_id);
    if (c.link_id) links.add(c.link_id);
  }
  const partners = project.clips.filter(
    (c) =>
      !ids.includes(c.id) &&
      ((c.group_id !== null && groups.has(c.group_id)) || (c.link_id !== null && links.has(c.link_id))),
  );
  return [...selected, ...partners];
}

/**
 * Track offset that carries the grabbed clip from its track to `targetTrackId`, counted among
 * tracks of the same kind in project order. Zero when the target has another kind or is missing.
 */
function trackOffsetFor(project: Project, grab: Clip, targetTrackId: string | null): number {
  if (targetTrackId === null) return 0;
  const grabKind = trackKindOf(project, grab.track_id);
  const targetKind = trackKindOf(project, targetTrackId);
  if (grabKind === null || grabKind !== targetKind) return 0;
  const same = project.tracks.filter((t) => t.kind === grabKind).map((t) => t.id);
  return same.indexOf(targetTrackId) - same.indexOf(grab.track_id);
}

/**
 * Track offset clamped once for a whole move group: every moved clip of the grabbed kind must stay
 * on a track of that kind. The nearest legal offset is returned when the requested one is not.
 */
function legalTrackOffset(
  project: Project,
  clips: readonly Clip[],
  grabKind: TrackKind | null,
  offset: number,
): number {
  if (offset === 0 || grabKind === null) return offset;
  const same = project.tracks.filter((t) => t.kind === grabKind).map((t) => t.id);
  let lo = -Infinity;
  let hi = Infinity;
  for (const c of clips) {
    if (trackKindOf(project, c.track_id) !== grabKind) continue;
    const idx = same.indexOf(c.track_id);
    if (idx < 0) continue;
    lo = Math.max(lo, -idx);
    hi = Math.min(hi, same.length - 1 - idx);
  }
  if (!Number.isFinite(lo) || !Number.isFinite(hi) || lo > hi) return offset;
  return clampNum(offset, lo, hi);
}

/**
 * Track a clip lands on after a track offset. Clips of another kind than the grabbed clip keep
 * their track. The result is clamped to the tracks that exist.
 */
function shiftedTrack(project: Project, clip: Clip, grabKind: TrackKind | null, offset: number): string {
  if (offset === 0 || grabKind === null) return clip.track_id;
  const kind = trackKindOf(project, clip.track_id);
  if (kind !== grabKind) return clip.track_id;
  const same = project.tracks.filter((t) => t.kind === grabKind).map((t) => t.id);
  const idx = same.indexOf(clip.track_id);
  if (idx < 0 || same.length === 0) return clip.track_id;
  const next = Math.min(same.length - 1, Math.max(0, idx + offset));
  return same[next] ?? clip.track_id;
}

/** Snap candidates against the context's points. Returns the smallest correction, or null. */
function snapNearest(
  candidates: readonly Tick[],
  ctx: DragContext,
): { adjust: Tick; tick: Tick } | null {
  let best: { adjust: Tick; tick: Tick } | null = null;
  for (const c of candidates) {
    const r = snapTick(c, ctx.snapPoints, SNAP_PX, ctx.view.pxPerSec);
    if (!r.snapped) continue;
    const adjust = r.tick - c;
    if (best === null || Math.abs(adjust) < Math.abs(best.adjust)) {
      best = { adjust, tick: r.tick };
    }
  }
  return best;
}

/**
 * Whether snapping applies to this update. Alt disables it for move and trim drags, except for a
 * duplicating move (Alt + drag copies and still snaps). Roll always snaps when enabled.
 */
function snappingOn(state: DragState, e: PointerLike, ctx: DragContext): boolean {
  if (!ctx.snapEnabled) return false;
  if (state.kind === 'roll') return true;
  if (state.kind === 'move' && state.duplicate) return true;
  return !e.altKey;
}

/** Update result that changes nothing (used when a referenced clip has gone). */
function unchanged(state: DragState): DragUpdate {
  return {
    state,
    ghosts: [],
    snapTick: null,
    previewTick: null,
    marquee: null,
    selectionIds: null,
  };
}

/** Source range of a clip's media when it is bounded: video and audio with a known duration. */
function boundedMediaDuration(project: Project, clip: Clip): Tick | null {
  if (clip.kind !== 'video' && clip.kind !== 'audio') return null;
  if (clip.media_id === null) return null;
  const media = project.media.find((m) => m.id === clip.media_id);
  return media && media.duration > 0 ? media.duration : null;
}

/** End of the nearest other clip on the same track that ends at or before `clip` starts (0 when none). */
function previousEnd(project: Project, clip: Clip): Tick {
  let best: Tick = 0;
  for (const c of project.clips) {
    if (c.id === clip.id || c.track_id !== clip.track_id) continue;
    const end = c.start + c.dur;
    if (c.start < clip.start && end <= clip.start && end > best) best = end;
  }
  return best;
}

/** Start of the nearest other clip on the same track that starts at or after `end` (max when none). */
function nextStart(project: Project, clip: Clip, end: Tick): Tick {
  let best: Tick = MAX_PROJECT_TICKS;
  for (const c of project.clips) {
    if (c.id === clip.id || c.track_id !== clip.track_id) continue;
    if (c.start >= end && c.start < best) best = c.start;
  }
  return best;
}

/** Absolute limits of one clip edge. */
interface EdgeRange {
  lo: Tick;
  hi: Tick;
}

/**
 * Absolute limits of one clip edge, matching the backend rules (ops_trim.rs). Covers the one-frame
 * minimum, the tick range, source handles (video and audio with a known duration: the start edge
 * cannot pass the source start, the end edge cannot pass the source end) and, when `neighbours`
 * is set, the adjacent clips on the track. Reverse clips may only shorten at the start edge. Freeze
 * clips (speed 0) cannot move their start edge.
 */
function edgeRange(project: Project, clip: Clip, edge: 'start' | 'end', neighbours: boolean): EdgeRange {
  const minLen = minClipTicks(project);
  const end = clip.start + clip.dur;
  const media = boundedMediaDuration(project, clip);
  if (edge === 'start') {
    if (clip.speed <= 0) return { lo: clip.start, hi: clip.start };
    let lo = 0;
    if (clip.reverse) {
      lo = clip.start;
    } else if (media !== null) {
      lo = Math.max(0, Math.ceil(clip.start - clip.src_in / clip.speed));
    }
    if (neighbours) lo = Math.max(lo, previousEnd(project, clip));
    return { lo, hi: end - minLen };
  }
  let hi = MAX_PROJECT_TICKS;
  if (media !== null && clip.speed > 0) {
    hi = Math.min(hi, clip.start + Math.floor((media - clip.src_in) / clip.speed));
  }
  if (neighbours) hi = Math.min(hi, nextStart(project, clip, end));
  return { lo: clip.start + minLen, hi };
}

/**
 * Range of time offsets for a trim of `edge` on `clip`, shared by the clip and its group and link
 * partners so that every member moves by the same delta. Offsets are relative to the current edge.
 */
function sharedTrimDelta(
  project: Project,
  clip: Clip,
  edge: 'start' | 'end',
  neighbours: boolean,
): { dLo: number; dHi: number } {
  let dLo = -Infinity;
  let dHi = Infinity;
  for (const c of movingClips(project, [clip.id])) {
    const r = edgeRange(project, c, edge, neighbours);
    const e0 = edgeTime(c, edge);
    dLo = Math.max(dLo, r.lo - e0);
    dHi = Math.min(dHi, r.hi - e0);
  }
  return { dLo, dHi };
}

/** True when the clip's edge is the outer edge of its group (no member reaches past it). */
function isGroupOuterEdge(project: Project, clip: Clip, edge: 'start' | 'end'): boolean {
  if (!clip.group_id) return true;
  const members = project.clips.filter((c) => c.group_id === clip.group_id);
  if (edge === 'start') return members.every((c) => clip.start <= c.start);
  const end = clip.start + clip.dur;
  return members.every((c) => end >= c.start + c.dur);
}

// ---------------------------------------------------------------------------------------------
// Begin
// ---------------------------------------------------------------------------------------------

/**
 * Finds the adjacent clip that shares the edit point of `clip`'s `edge` on the same track, for a
 * roll edit. Returns null when there is no touching neighbour.
 */
function findRollPartner(
  project: Project,
  clip: Clip,
  edge: 'start' | 'end',
): { leftId: string; rightId: string; cut: Tick } | null {
  if (edge === 'end') {
    const cut = clip.start + clip.dur;
    const right = project.clips.find(
      (c) => c.id !== clip.id && c.track_id === clip.track_id && c.start === cut,
    );
    return right ? { leftId: clip.id, rightId: right.id, cut } : null;
  }
  const cut = clip.start;
  const left = project.clips.find(
    (c) => c.id !== clip.id && c.track_id === clip.track_id && c.start + c.dur === cut,
  );
  return left ? { leftId: left.id, rightId: clip.id, cut } : null;
}

/** Move drag for a press on `grab`. The selection moves when it contains the grabbed clip. */
function moveDragFor(
  project: Project,
  grab: Clip,
  selection: string[],
  e: PointerLike,
  duplicate: boolean,
): MoveDrag {
  const ids = selection.includes(grab.id) ? selection : [grab.id];
  const moved = ids.filter((id) => {
    const c = findClip(project, id);
    return c !== undefined && !isLocked(project, c);
  });
  return {
    kind: 'move',
    grabClipId: grab.id,
    clipIds: dedupe(moved),
    originX: e.x,
    originY: e.y,
    delta: 0,
    trackOffset: 0,
    duplicate,
  };
}

/**
 * Drag state for a pointer press. `selection` is the selection after the press was applied
 * (the caller selects the grabbed clip first if it was not selected). Returns null when the press
 * starts no drag (for example on a locked clip).
 *
 * - ruler: playhead drag
 * - clip: move; with Alt the move duplicates the clips on release
 * - edge: trim; with Alt and an adjacent clip on the same track, roll instead. On a grouped clip,
 *   an edge that is not the group's outer edge starts a move instead (groups trim as a block)
 * - gap, track, empty: marquee (Shift or Ctrl makes it additive)
 */
export function beginDrag(
  hit: HitResult,
  e: PointerLike,
  project: Project,
  selection: string[],
): DragState | null {
  switch (hit.kind) {
    case 'ruler':
      return { kind: 'playhead' };

    case 'clip': {
      if (!hit.clipId) return null;
      const grab = findClip(project, hit.clipId);
      if (!grab || isLocked(project, grab)) return null;
      return moveDragFor(project, grab, selection, e, e.altKey);
    }

    case 'edge-start':
    case 'edge-end': {
      if (!hit.clipId) return null;
      const clip = findClip(project, hit.clipId);
      if (!clip || isLocked(project, clip)) return null;
      const edge = hit.kind === 'edge-start' ? 'start' : 'end';
      if (!isGroupOuterEdge(project, clip, edge)) {
        return moveDragFor(project, clip, selection, e, false);
      }
      if (e.altKey) {
        const partner = findRollPartner(project, clip, edge);
        if (partner) {
          return {
            kind: 'roll',
            leftId: partner.leftId,
            rightId: partner.rightId,
            originX: e.x,
            originCut: partner.cut,
            newCut: partner.cut,
          };
        }
      }
      const originTime = edgeTime(clip, edge);
      return {
        kind: edge === 'start' ? 'trim-start' : 'trim-end',
        clipId: clip.id,
        originX: e.x,
        originTime,
        newTime: originTime,
        ripple: e.shiftKey,
      };
    }

    case 'gap':
    case 'track':
    case 'empty':
      return {
        kind: 'marquee',
        originX: e.x,
        originY: e.y,
        x: e.x,
        y: e.y,
        base: [...selection],
        additive: e.shiftKey || e.ctrlKey || e.metaKey,
      };
  }
}

/**
 * Envelope point drag. `index` is the point in `clip.audio.envelope`. Returns null when the clip
 * or the point does not exist.
 */
export function beginEnvelopeDrag(
  project: Project,
  clipId: string,
  index: number,
  e: PointerLike,
): DragState | null {
  const clip = findClip(project, clipId);
  if (!clip) return null;
  const origin = clip.audio.envelope[index];
  if (origin === undefined) return null;
  return {
    kind: 'envelope-point',
    clipId,
    index,
    originX: e.x,
    originY: e.y,
    origin,
    base: clip.audio.envelope,
    points: clip.audio.envelope.slice(),
  };
}

/**
 * Envelope drag for a hit from `hitEnvelopePoint`. A hit on a point drags that point. A hit on the
 * gain line (no index) inserts a point at the hit time and gain and drags it. The inserted point
 * is part of the drag's base, so the command is only sent once the pointer moves. Returns null
 * when the clip is missing.
 */
export function beginEnvelopeHitDrag(project: Project, hit: EnvelopeHit, e: PointerLike): DragState | null {
  if (hit.index !== null) return beginEnvelopeDrag(project, hit.clipId, hit.index, e);
  const clip = findClip(project, hit.clipId);
  if (!clip) return null;
  const t = clampTick(hit.tick - clip.start, 0, clip.dur);
  const point: EnvPoint = { t, db: clampNum(hit.db, ENVELOPE_DB_MIN, ENVELOPE_DB_MAX) };
  const current = clip.audio.envelope;
  const at = current.findIndex((p) => p.t > t);
  const insertAt = at < 0 ? current.length : at;
  const list = [...current.slice(0, insertAt), point, ...current.slice(insertAt)];
  return {
    kind: 'envelope-point',
    clipId: clip.id,
    index: insertAt,
    originX: e.x,
    originY: e.y,
    origin: point,
    base: list,
    points: list.slice(),
  };
}

/** Fade handle drag on the clip's `in` or `out` fade. Returns null when the clip is missing. */
export function beginFadeDrag(
  project: Project,
  clipId: string,
  edge: 'in' | 'out',
  e: PointerLike,
): DragState | null {
  const clip = findClip(project, clipId);
  if (!clip) return null;
  const value = edge === 'in' ? clip.audio.fade_in : clip.audio.fade_out;
  return { kind: 'fade-handle', clipId, edge, originX: e.x, origin: value, value };
}

/** Pan drag that starts at the given scroll offsets. */
export function beginPan(e: PointerLike, scrollX: number, scrollY: number): DragState {
  return { kind: 'pan', originX: e.x, originY: e.y, scrollX0: scrollX, scrollY0: scrollY };
}

/** Typed variant of `beginPan` that returns a `PanDrag` directly. Throws if `beginPan` ever stops returning a pan drag. */
export function beginPanTyped(startX: number, startY: number, scrollX: number, scrollY: number): PanDrag {
  const state = beginPan({ x: startX, y: startY, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false }, scrollX, scrollY);
  if (state.kind !== 'pan') throw new Error('beginPanTyped: beginPan did not return a pan drag');
  return state;
}

/**
 * Scroll offsets for a pan drag at pointer `e`. Both values are clamped to be non-negative; the
 * caller applies its own upper limits.
 */
export function panScroll(state: PanDrag, e: PointerLike): { scrollX: number; scrollY: number } {
  return {
    scrollX: Math.max(0, state.scrollX0 - (e.x - state.originX)),
    scrollY: Math.max(0, state.scrollY0 - (e.y - state.originY)),
  };
}

/**
 * Moves the drag origin by `dx` px. Called when the view auto-scrolls during a drag, so the drag
 * keeps following the pointer. Playhead and pan drags are returned unchanged.
 */
export function shiftDragOrigin(state: DragState, dx: number): DragState {
  if (state.kind === 'playhead' || state.kind === 'pan') return state;
  return { ...state, originX: state.originX - dx };
}

// ---------------------------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------------------------

/** Ghosts for the moving clips at the given delta and track offset. */
function moveGhosts(
  project: Project,
  clips: readonly Clip[],
  grabKind: TrackKind | null,
  delta: Tick,
  offset: number,
  ctx: DragContext,
): DragGhost[] {
  const out: DragGhost[] = [];
  for (const c of clips) {
    const moved: Clip = {
      ...c,
      start: c.start + delta,
      track_id: shiftedTrack(project, c, grabKind, offset),
    };
    const rect = clipRect(moved, ctx.rows, ctx.view);
    if (rect) out.push({ clipId: c.id, rect });
  }
  return out;
}

/**
 * Move update: time delta (frame aligned, clamped, snapped) and one track offset, clamped for the
 * whole group, for the selection and its partners.
 */
function updateMove(s: MoveDrag, e: PointerLike, ctx: DragContext): DragUpdate {
  const { project, rows, view } = ctx;
  const grab = findClip(project, s.grabClipId);
  if (!grab) return unchanged(s);

  const target = dragMoveTarget(project, rows, view, s.clipIds, e.x - s.originX, e.y, s.grabClipId);
  let delta = target.delta;
  let snap: Tick | null = null;
  if (snappingOn(s, e, ctx)) {
    const hit = snapNearest([grab.start + delta, grab.start + grab.dur + delta], ctx);
    if (hit) {
      delta += hit.adjust;
      snap = hit.tick;
    }
  }
  // Whole frames: the grabbed clip lands on the frame grid.
  delta = snapToFrame(grab.start + delta, project.canvas.fps) - grab.start;

  const group = movingClips(project, s.clipIds);
  // Never move anything before tick 0, partners included.
  let earliest = Infinity;
  for (const c of group) earliest = Math.min(earliest, c.start);
  if (Number.isFinite(earliest)) delta = Math.max(delta, -earliest);

  const grabKind = trackKindOf(project, grab.track_id);
  const trackOffset = legalTrackOffset(
    project,
    group,
    grabKind,
    trackOffsetFor(project, grab, target.trackShift),
  );
  const ghosts = moveGhosts(project, group, grabKind, delta, trackOffset, ctx);
  return {
    state: { ...s, delta, trackOffset },
    ghosts,
    snapTick: snap,
    previewTick: null,
    marquee: null,
    selectionIds: null,
  };
}

/**
 * Trim update: the new edge time, frame aligned and limited to the range every member of the
 * group can take (see `edgeRange`). Partners move by the same delta as the grabbed clip.
 */
function updateTrim(s: TrimDrag, e: PointerLike, ctx: DragContext): DragUpdate {
  const { project } = ctx;
  const clip = findClip(project, s.clipId);
  if (!clip) return unchanged(s);

  const isStart = s.kind === 'trim-start';
  const edge = isStart ? 'start' : 'end';
  const ripple = e.shiftKey;
  const minLen = minClipTicks(project);
  const edge0 = edgeTime(clip, edge);
  let raw = s.originTime + pxToTicks(e.x - s.originX, ctx.view.pxPerSec);
  let snap: Tick | null = null;
  if (snappingOn(s, e, ctx)) {
    const hit = snapNearest([raw], ctx);
    if (hit) {
      raw += hit.adjust;
      snap = hit.tick;
    }
  }
  const { dLo, dHi } = sharedTrimDelta(project, clip, edge, !ripple);
  const newTime = alignedClamp(raw, edge0 + dLo, edge0 + dHi, project.canvas.fps);
  const delta = newTime - edge0;

  const ghosts: DragGhost[] = [];
  for (const c of movingClips(project, [clip.id])) {
    const cEnd = c.start + c.dur;
    const ns = isStart ? c.start + delta : c.start;
    const ne = isStart ? cEnd : cEnd + delta;
    if (ne - ns < minLen) continue;
    const rect = clipRect({ ...c, start: ns, dur: ne - ns }, ctx.rows, ctx.view);
    if (rect) ghosts.push({ clipId: c.id, rect });
  }

  return {
    state: { ...s, newTime, ripple },
    ghosts,
    snapTick: snap,
    previewTick: null,
    marquee: null,
    selectionIds: null,
  };
}

/** Roll update: new edit point between the two clips (frame aligned, limited by both clips). */
function updateRoll(s: RollDrag, e: PointerLike, ctx: DragContext): DragUpdate {
  const { project } = ctx;
  const left = findClip(project, s.leftId);
  const right = findClip(project, s.rightId);
  if (!left || !right) return unchanged(s);

  const rightEnd = right.start + right.dur;
  const leftRange = edgeRange(project, left, 'end', false);
  const rightRange = edgeRange(project, right, 'start', false);
  const lo = Math.max(leftRange.lo, rightRange.lo);
  const hi = Math.min(leftRange.hi, rightRange.hi);

  let raw = s.originCut + pxToTicks(e.x - s.originX, ctx.view.pxPerSec);
  let snap: Tick | null = null;
  if (snappingOn(s, e, ctx)) {
    const hit = snapNearest([raw], ctx);
    if (hit) {
      raw += hit.adjust;
      snap = hit.tick;
    }
  }
  const newCut = alignedClamp(raw, lo, hi, project.canvas.fps);

  const ghosts: DragGhost[] = [];
  const leftRect = clipRect({ ...left, dur: newCut - left.start }, ctx.rows, ctx.view);
  if (leftRect) ghosts.push({ clipId: left.id, rect: leftRect });
  const rightRect = clipRect(
    { ...right, start: newCut, dur: rightEnd - newCut },
    ctx.rows,
    ctx.view,
  );
  if (rightRect) ghosts.push({ clipId: right.id, rect: rightRect });

  return {
    state: { ...s, newCut },
    ghosts,
    snapTick: snap,
    previewTick: null,
    marquee: null,
    selectionIds: null,
  };
}

/** Marquee update: the rectangle and the clips it touches. */
function updateMarquee(s: MarqueeDrag, e: PointerLike, ctx: DragContext): DragUpdate {
  const rect: MarqueeRect = { x0: s.originX, y0: s.originY, x1: e.x, y1: e.y };
  const ids = marqueeSelect(ctx.project, ctx.rows, ctx.view, rect);
  const selectionIds = s.additive ? dedupe([...s.base, ...ids]) : ids;
  return {
    state: { ...s, x: e.x, y: e.y },
    ghosts: [],
    snapTick: null,
    previewTick: null,
    marquee: rect,
    selectionIds,
  };
}

/**
 * Envelope point update: time clamped between the neighbouring points and the clip (0..dur), gain
 * from the pointer's y in the clip's geometry (keeping the grab offset), clamped to -60..12 dB.
 */
function updateEnvelope(s: EnvelopeDrag, e: PointerLike, ctx: DragContext): DragUpdate {
  const clip = findClip(ctx.project, s.clipId);
  if (!clip) return unchanged(s);
  const rect = clipRect(clip, ctx.rows, ctx.view);
  if (!rect) return unchanged(s);
  const before = s.index > 0 ? s.base[s.index - 1] : undefined;
  const after = s.base[s.index + 1];
  const lo = clampNum(before?.t ?? 0, 0, clip.dur);
  const hi = clampNum(after?.t ?? clip.dur, 0, clip.dur);
  const t = clampTick(s.origin.t + pxToTicks(e.x - s.originX, ctx.view.pxPerSec), lo, hi);
  const grabOffset = s.origin.db - envelopeDbAt(s.originY, rect);
  const db = clampNum(envelopeDbAt(e.y, rect) + grabOffset, ENVELOPE_DB_MIN, ENVELOPE_DB_MAX);
  const points = s.base.map((p, i) => (i === s.index ? { t, db } : p));
  return unchanged({ ...s, points });
}

/**
 * Fade update: the fade length, limited by the clip length and the opposite fade. The result
 * carries both fade lengths as `fadePreview` so the canvas can draw the fade while dragging.
 */
function updateFade(s: FadeDrag, e: PointerLike, ctx: DragContext): DragUpdate {
  const clip = findClip(ctx.project, s.clipId);
  if (!clip) return unchanged(s);
  const dt = pxToTicks(e.x - s.originX, ctx.view.pxPerSec);
  const other = s.edge === 'in' ? clip.audio.fade_out : clip.audio.fade_in;
  const raw = s.edge === 'in' ? s.origin + dt : s.origin - dt;
  const value = clampTick(raw, 0, Math.max(0, clip.dur - other));
  const fadeIn = s.edge === 'in' ? value : clip.audio.fade_in;
  const fadeOut = s.edge === 'out' ? value : clip.audio.fade_out;
  return { ...unchanged({ ...s, value }), fadePreview: { clipId: clip.id, fadeIn, fadeOut } };
}

/**
 * Recomputes the drag preview for a pointer move. Pure: the returned state replaces the old one.
 * Pan drags return the state unchanged; apply `panScroll` for the new scroll offsets.
 */
export function updateDrag(state: DragState, e: PointerLike, ctx: DragContext): DragUpdate {
  switch (state.kind) {
    case 'move':
      return updateMove(state, e, ctx);
    case 'trim-start':
    case 'trim-end':
      return updateTrim(state, e, ctx);
    case 'roll':
      return updateRoll(state, e, ctx);
    case 'playhead':
      return { ...unchanged(state), previewTick: xToTick(e.x, ctx.view) };
    case 'marquee':
      return updateMarquee(state, e, ctx);
    case 'envelope-point':
      return updateEnvelope(state, e, ctx);
    case 'fade-handle':
      return updateFade(state, e, ctx);
    case 'pan':
      return unchanged(state);
  }
}

// ---------------------------------------------------------------------------------------------
// Finish
// ---------------------------------------------------------------------------------------------

/** Same time and gain at every envelope index. */
function sameEnvelope(a: readonly EnvPoint[], b: readonly EnvPoint[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((p, i) => {
    const q = b[i];
    return q !== undefined && p.t === q.t && p.db === q.db;
  });
}

/**
 * Commands for a finished drag. Empty when nothing changed or the drag only moves the view or the
 * playhead.
 *
 * Move sends one `move_clips` for the selection and its group and link partners, each with the
 * time delta and its shifted track. A duplicating move (Alt) with no track change sends one
 * `duplicate_clips` instead; with a track change it falls back to a plain move.
 */
export function finishDrag(state: DragState, ctx: { project: Project }): Command[] {
  const { project } = ctx;
  switch (state.kind) {
    case 'move': {
      if (state.delta === 0 && state.trackOffset === 0) return [];
      if (state.duplicate && state.trackOffset === 0) {
        return [{ type: 'duplicate_clips', ids: state.clipIds, offset: state.delta }];
      }
      const grab = findClip(project, state.grabClipId);
      const grabKind = grab ? trackKindOf(project, grab.track_id) : null;
      const moves: ClipMove[] = [];
      for (const c of movingClips(project, state.clipIds)) {
        const track = shiftedTrack(project, c, grabKind, state.trackOffset);
        moves.push({
          clip_id: c.id,
          new_start: c.start + state.delta,
          new_track: track === c.track_id ? null : track,
        });
      }
      return moves.length > 0 ? [{ type: 'move_clips', moves }] : [];
    }

    case 'trim-start':
    case 'trim-end': {
      if (state.newTime === state.originTime) return [];
      return [
        {
          type: 'trim_edge',
          clip_id: state.clipId,
          edge: state.kind === 'trim-start' ? 'start' : 'end',
          new_time: state.newTime,
          ripple: state.ripple,
        },
      ];
    }

    case 'roll': {
      if (state.newCut === state.originCut) return [];
      return [
        {
          type: 'roll_edit',
          left_id: state.leftId,
          right_id: state.rightId,
          new_cut: state.newCut,
        },
      ];
    }

    case 'fade-handle': {
      if (state.value === state.origin) return [];
      const patch = state.edge === 'in' ? { fade_in: state.value } : { fade_out: state.value };
      return [{ type: 'patch_clips', ids: [state.clipId], patch }];
    }

    case 'envelope-point': {
      const clip = findClip(project, state.clipId);
      if (!clip || sameEnvelope(state.points, state.base)) return [];
      return [{ type: 'set_envelope', clip_id: state.clipId, points: state.points }];
    }

    case 'playhead':
    case 'marquee':
    case 'pan':
      return [];
  }
}
