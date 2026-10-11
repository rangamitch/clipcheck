// Pure clip trimmer model. Contract: docs/impl/features-f1.md D27. No DOM, no Svelte.
//
// Invariants for a selection (when duration >= minMs):
//   0 <= startMs < endMs <= durationMs and endMs - startMs >= minMs.
// When the duration is below minMs the selection is the whole clip, {0, duration}.

export interface TrimSelection {
  startMs: number;
  endMs: number;
}

export type TrimMode = 'lossless' | 'precise';
export type HandleKind = 'start' | 'end';

/** Shortest selection the trimmer accepts. */
export const DEFAULT_MIN_MS = 100;
/** One frame at ~30 fps, used by plain arrow / [ ] nudges. */
export const FRAME_MS = 33;
/** Coarse nudge (Shift+arrow, Shift+[ / ]). */
export const COARSE_MS = 1000;

export type TrimAction =
  | { type: 'set-in' }
  | { type: 'set-out' }
  | { type: 'nudge'; dir: 1 | -1; coarse: boolean }
  | { type: 'save' };

function clampNum(x: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, x));
}

function finiteOr(x: number, fallback: number): number {
  return Number.isFinite(x) ? x : fallback;
}

/** Usable duration: finite and positive, otherwise 0. */
function usableDuration(durationMs: number): number {
  return Number.isFinite(durationMs) && durationMs > 0 ? durationMs : 0;
}

/** Smallest gap the handles may have on this clip (the whole clip when it is shorter than minMs). */
function gapFor(d: number, minMs: number): number {
  return Math.min(minMs, d);
}

/**
 * Forces a selection into the invariants. Swaps inverted handles, clamps to
 * [0, duration], and widens a too-short range to minMs (anchored at the start
 * when possible, else at the end). NaN start becomes 0, NaN end becomes duration.
 */
export function clampSelection(
  sel: TrimSelection,
  durationMs: number,
  minMs: number = DEFAULT_MIN_MS,
): TrimSelection {
  const d = usableDuration(durationMs);
  if (d < minMs) {
    return { startMs: 0, endMs: d };
  }
  let s = clampNum(finiteOr(sel.startMs, 0), 0, d);
  let e = clampNum(finiteOr(sel.endMs, d), 0, d);
  if (s > e) {
    [s, e] = [e, s];
  }
  if (e - s < minMs) {
    e = Math.min(d, s + minMs);
    if (e - s < minMs) {
      s = d - minMs;
      e = d;
    }
  }
  return { startMs: s, endMs: e };
}

/**
 * Moves one handle by `deltaMs`. The moved handle is clamped to the clip and
 * may not pass the other handle (it stays at least minMs away from it).
 * NaN delta is treated as 0. `which` other than 'start' moves the end handle.
 */
export function nudge(
  sel: TrimSelection,
  which: HandleKind,
  deltaMs: number,
  durationMs: number,
  minMs: number = DEFAULT_MIN_MS,
): TrimSelection {
  const d = usableDuration(durationMs);
  const cur = clampSelection(sel, d, minMs);
  const gap = gapFor(d, minMs);
  const delta = finiteOr(deltaMs, 0);
  if (which === 'start') {
    const s = clampNum(cur.startMs + delta, 0, Math.max(0, cur.endMs - gap));
    return { startMs: s, endMs: cur.endMs };
  }
  const e = clampNum(cur.endMs + delta, cur.startMs + gap, d);
  return { startMs: cur.startMs, endMs: e };
}

/**
 * Sets the in-handle to the playhead position. The position is clamped so the
 * start stays at least minMs before the end. NaN position keeps the current start.
 */
export function setIn(
  sel: TrimSelection,
  positionMs: number,
  durationMs: number,
  minMs: number = DEFAULT_MIN_MS,
): TrimSelection {
  const d = usableDuration(durationMs);
  const cur = clampSelection(sel, d, minMs);
  const gap = gapFor(d, minMs);
  const pos = finiteOr(positionMs, cur.startMs);
  const s = clampNum(pos, 0, Math.max(0, cur.endMs - gap));
  return { startMs: s, endMs: cur.endMs };
}

/**
 * Sets the out-handle to the playhead position. The position is clamped so the
 * end stays at least minMs after the start and inside the clip. NaN position
 * keeps the current end.
 */
export function setOut(
  sel: TrimSelection,
  positionMs: number,
  durationMs: number,
  minMs: number = DEFAULT_MIN_MS,
): TrimSelection {
  const d = usableDuration(durationMs);
  const cur = clampSelection(sel, d, minMs);
  const gap = gapFor(d, minMs);
  const pos = finiteOr(positionMs, cur.endMs);
  const e = clampNum(pos, cur.startMs + gap, d);
  return { startMs: cur.startMs, endMs: e };
}

function pad(n: number, width: number): string {
  return String(n).padStart(width, '0');
}

/**
 * Formats milliseconds as `HH:MM:SS.mmm` (floored). NaN, Infinity and negative
 * values give "00:00:00.000". Hours grow past 99 without truncation.
 */
export function formatHmsMs(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) {
    return '00:00:00.000';
  }
  const total = Math.floor(ms);
  const h = Math.floor(total / 3_600_000);
  const m = Math.floor(total / 60_000) % 60;
  const s = Math.floor(total / 1000) % 60;
  const f = total % 1000;
  return `${pad(h, 2)}:${pad(m, 2)}:${pad(s, 2)}.${pad(f, 3)}`;
}

// Forms: ss, mm:ss, hh:mm:ss, each with an optional .1-3 digit fraction on the last field.
const TIMECODE = /^(\d+)(?::(\d+))?(?::(\d+))?(?:\.(\d{1,3}))?$/;

/**
 * Parses `ss`, `mm:ss` or `hh:mm:ss` (optional `.mmm` fraction, 1 to 3 digits,
 * right-padded: `.5` is 500 ms) into milliseconds. Returns null for empty,
 * negative or malformed input, and when minutes or seconds exceed 59. Hours are unbounded.
 */
export function parseTimecode(input: string): number | null {
  if (typeof input !== 'string') {
    return null;
  }
  const match = TIMECODE.exec(input.trim());
  if (!match) {
    return null;
  }
  let h = 0;
  let m = 0;
  let s: number;
  if (match[3] !== undefined) {
    h = Number(match[1]);
    m = Number(match[2]);
    s = Number(match[3]);
  } else if (match[2] !== undefined) {
    m = Number(match[1]);
    s = Number(match[2]);
  } else {
    s = Number(match[1]);
  }
  if (s > 59 || (match[2] !== undefined && m > 59)) {
    return null;
  }
  const frac = match[4] !== undefined ? Number(match[4].padEnd(3, '0')) : 0;
  return ((h * 60 + m) * 60 + s) * 1000 + frac;
}

/**
 * Readout sentence for lossless mode, e.g.
 * "Requested 00:12:04.200 – 00:12:31.900, output 00:12:02.000 – 00:12:31.900 (start snapped back 2.2 s)".
 * The snap clause is omitted when the start did not move.
 */
export function snapReadout(requested: TrimSelection, actual: TrimSelection): string {
  const base =
    `Requested ${formatHmsMs(requested.startMs)} – ${formatHmsMs(requested.endMs)}, ` +
    `output ${formatHmsMs(actual.startMs)} – ${formatHmsMs(actual.endMs)}`;
  const delta = requested.startMs - actual.startMs;
  if (!Number.isFinite(delta) || delta === 0) {
    return base;
  }
  const secs = (Math.abs(delta) / 1000).toFixed(1);
  return `${base} (start snapped ${delta > 0 ? 'back' : 'forward'} ${secs} s)`;
}

/**
 * Estimated output size: sizeBytes times the selected fraction of the duration,
 * rounded. Returns 0 when the duration is not positive or finite, or sizeBytes is not finite.
 */
export function estimateOutputBytes(
  sizeBytes: number,
  durationMs: number,
  sel: TrimSelection,
): number {
  if (!Number.isFinite(durationMs) || durationMs <= 0 || !Number.isFinite(sizeBytes)) {
    return 0;
  }
  const span = Math.max(0, sel.endMs - sel.startMs);
  return Math.round((sizeBytes * span) / durationMs);
}

/**
 * Maps a key event to a trimmer action, or null. Keys: i / I (in), o / O (out),
 * [ and ] (nudge focused handle one frame), Shift+[ / ] or { / } (one second),
 * Ctrl+Enter (save as new clip; Ctrl+Shift+Enter is not mapped). Alt and Meta
 * combinations always return null.
 */
export function trimKeyToAction(e: {
  key: string;
  shiftKey?: boolean;
  altKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
}): TrimAction | null {
  if (e.altKey || e.metaKey) {
    return null;
  }
  const shift = e.shiftKey === true;
  if (e.ctrlKey) {
    return e.key === 'Enter' && !shift ? { type: 'save' } : null;
  }
  switch (e.key) {
    case 'i':
    case 'I':
      return { type: 'set-in' };
    case 'o':
    case 'O':
      return { type: 'set-out' };
    case '[':
      return { type: 'nudge', dir: -1, coarse: shift };
    case '{':
      return { type: 'nudge', dir: -1, coarse: true };
    case ']':
      return { type: 'nudge', dir: 1, coarse: shift };
    case '}':
      return { type: 'nudge', dir: 1, coarse: true };
    default:
      return null;
  }
}
