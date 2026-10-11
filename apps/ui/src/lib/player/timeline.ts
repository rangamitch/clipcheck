// Pure timeline geometry and playback confinement. Contract: docs/impl/player-v2.md. No DOM, no Svelte.

/** Nice major tick steps, in ms, smallest first. */
const TICK_STEPS_MS = [
  1000, 2000, 5000, 10000, 15000, 30000, 60000, 120000, 300000, 600000, 1800000, 3600000,
];

/** Frame-count cap for the filmstrip. */
const FILMSTRIP_MAX = 40;

/** Seeking further than this before the in-point snaps forward to the in-point. */
const CONFINE_LEAD_MS = 100;

function usableDuration(durMs: number): boolean {
  return Number.isFinite(durMs) && durMs > 0;
}

function usableWidth(widthPx: number): number {
  return Number.isFinite(widthPx) && widthPx > 0 ? widthPx : 0;
}

/**
 * Linear map from a time to an x position in [0, widthPx]. Times outside the
 * clip are clamped. A duration <= 0 or NaN, or a NaN time, gives 0.
 */
export function msToX(ms: number, widthPx: number, durMs: number): number {
  if (!usableDuration(durMs) || Number.isNaN(ms)) {
    return 0;
  }
  const w = usableWidth(widthPx);
  const x = (ms / durMs) * w;
  return Math.min(w, Math.max(0, x));
}

/**
 * Inverse of `msToX`: an x position to a time in integer ms, clamped to
 * [0, durMs]. A width <= 0 or NaN, a duration <= 0 or NaN, or a NaN x gives 0.
 */
export function xToMs(x: number, widthPx: number, durMs: number): number {
  if (!usableWidth(widthPx) || !usableDuration(durMs) || Number.isNaN(x)) {
    return 0;
  }
  const w = usableWidth(widthPx);
  const d = Math.floor(durMs);
  const ms = Math.round((Math.min(w, Math.max(0, x)) / w) * durMs);
  return Math.min(d, Math.max(0, ms));
}

/**
 * Smallest nice major step whose on-screen spacing is at least `minPx`.
 * Falls back to the largest step when none is wide enough. The minor step is
 * a fifth of the major step (15 s uses 5 s). Invalid input gives 1 s / 200 ms.
 */
export function tickStep(
  durMs: number,
  widthPx: number,
  minPx: number = 60,
): { majorMs: number; minorMs: number } {
  const fallback = { majorMs: 1000, minorMs: 200 };
  if (!usableDuration(durMs) || !usableWidth(widthPx) || !Number.isFinite(minPx)) {
    return fallback;
  }
  const w = usableWidth(widthPx);
  let major = TICK_STEPS_MS[TICK_STEPS_MS.length - 1];
  for (const step of TICK_STEPS_MS) {
    if ((step / durMs) * w >= minPx) {
      major = step;
      break;
    }
  }
  const minor = major === 15000 ? 5000 : major / 5;
  return { majorMs: major, minorMs: minor };
}

/**
 * Number of filmstrip frames: one per frame-width of the timeline, clamped to
 * [1, 40]. Invalid input gives 1.
 */
export function filmstripCount(widthPx: number, aspect: number, heightPx: number): number {
  if (
    !usableWidth(widthPx) ||
    !Number.isFinite(aspect) ||
    aspect <= 0 ||
    !Number.isFinite(heightPx) ||
    heightPx <= 0
  ) {
    return 1;
  }
  const n = Math.ceil(widthPx / (heightPx * aspect));
  return Math.min(FILMSTRIP_MAX, Math.max(1, n));
}

/**
 * Playback confinement for a trim selection. When the selection covers the
 * whole clip nothing changes. At or past the out-point the player pauses and
 * parks on the out-point frame. Well before the in-point (more than 250 ms) it also
 * seeks to the in-point; a little before it plays on. NaN input gives none.
 */
export function confinePlayback(
  nowMs: number,
  sel: { startMs: number; endMs: number },
  durMs: number,
): { action: 'none' | 'pause-and-seek'; toMs: number } {
  const none = { action: 'none' as const, toMs: Number.isFinite(nowMs) ? nowMs : 0 };
  if (Number.isNaN(nowMs) || Number.isNaN(sel.startMs) || Number.isNaN(sel.endMs)) {
    return none;
  }
  if (sel.startMs <= 0 && sel.endMs >= durMs) {
    return none;
  }
  // Reaching the out-point stops on the out-point frame; it does not jump back to the in-point.
  if (nowMs >= sel.endMs) {
    return { action: 'pause-and-seek', toMs: sel.endMs };
  }
  if (nowMs < sel.startMs - CONFINE_LEAD_MS) {
    return { action: 'pause-and-seek', toMs: sel.startMs };
  }
  return none;
}

/** True when the selection is narrower than the clip (1 ms tolerance at each end). False for unknown duration. */
export function isTrimmed(sel: { startMs: number; endMs: number }, durMs: number): boolean {
  if (!usableDuration(durMs) || Number.isNaN(sel.startMs) || Number.isNaN(sel.endMs)) {
    return false;
  }
  return sel.startMs > 1 || sel.endMs < durMs - 1;
}

/** Highest zoom level (visible span never below ~10 s). */
const ZOOM_MAX = 64;
/** Shortest visible span the zoom allows, in ms. */
const ZOOM_MIN_SPAN_MS = 10000;
/** Clips shorter than this do not zoom at all. */
const ZOOM_MIN_DURATION_MS = 120000;

/**
 * Highest zoom level for a clip: the largest power of two (1..64) that keeps at
 * least ~10 s visible. Clips under 2 minutes, or an invalid duration, give 1.
 */
export function maxZoom(durMs: number): number {
  if (!usableDuration(durMs) || durMs < ZOOM_MIN_DURATION_MS) {
    return 1;
  }
  const ratio = durMs / ZOOM_MIN_SPAN_MS;
  let z = 1;
  while (z * 2 <= ratio && z < ZOOM_MAX) {
    z *= 2;
  }
  return z;
}

/** Clamps a zoom level to [1, maxZoom(durMs)]. NaN gives 1. */
export function clampZoom(z: number, durMs: number): number {
  if (Number.isNaN(z)) {
    return 1;
  }
  return Math.min(maxZoom(durMs), Math.max(1, z));
}

/**
 * The scrollLeft that keeps `anchorMs` at `anchorScreenX` (px from the left of the
 * scroll viewport) on a content strip `contentWidthPx` wide. Never negative; the
 * browser clamps the upper end. Invalid input gives 0.
 */
export function zoomAnchorScroll(
  anchorMs: number,
  anchorScreenX: number,
  contentWidthPx: number,
  durMs: number,
): number {
  if (Number.isNaN(anchorScreenX)) {
    return 0;
  }
  return Math.max(0, msToX(anchorMs, contentWidthPx, durMs) - anchorScreenX);
}

/**
 * Time range of the viewport widened by `overscanScreens` view-widths each side,
 * clamped to [0, durMs]. Used to draw only the ticks near the visible area.
 * Invalid input gives the whole clip.
 */
export function visibleMsRange(
  durMs: number,
  contentWidthPx: number,
  scrollLeftPx: number,
  viewWidthPx: number,
  overscanScreens: number = 1,
): { fromMs: number; toMs: number } {
  if (!usableDuration(durMs)) {
    return { fromMs: 0, toMs: 0 };
  }
  const w = usableWidth(contentWidthPx);
  if (w <= 0 || !Number.isFinite(scrollLeftPx) || !Number.isFinite(viewWidthPx) || !Number.isFinite(overscanScreens)) {
    return { fromMs: 0, toMs: durMs };
  }
  const pad = Math.max(0, viewWidthPx) * Math.max(0, overscanScreens);
  const from = ((scrollLeftPx - pad) / w) * durMs;
  const to = ((scrollLeftPx + Math.max(0, viewWidthPx) + pad) / w) * durMs;
  return { fromMs: Math.min(durMs, Math.max(0, from)), toMs: Math.min(durMs, Math.max(0, to)) };
}
