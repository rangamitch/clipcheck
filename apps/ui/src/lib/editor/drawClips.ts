/**
 * Canvas drawing of the timeline track area: track rows, clips (with thumbnails, waveforms, fades,
 * envelopes, badges), group brackets, markers, snap line, drag ghosts and marquee. The playhead is
 * not drawn here: the Timeline draws it as a DOM element.
 *
 * Coordinates are CSS px in the track area, as produced by `layout.ts` (y includes the ruler
 * height at the top). The canvas is scaled by `devicePixelRatio`. Nothing here touches the DOM
 * beyond reading computed CSS variables from the canvas element.
 */
import type { Clip, ClipKind, MediaRef, Project, Tick, Track, WorkArea } from './types';
import { TICKS_PER_SECOND } from './ticks';
import {
  RULER_HEIGHT,
  clipRect,
  tickToX,
  type ClipRect,
  type TrackRow,
  type View,
} from './layout';
import { requestPeaks, requestThumbs, type ThumbStrip } from './mediaCache';

/** Accent colour used for selection, playhead and marquee. A constant, not read from CSS. */
export const ACCENT = '#ed1c24';

/** Clip body colour per kind, used when a clip has no colour of its own. */
export const KIND_COLORS: Record<ClipKind, string> = {
  video: '#4f6b7a',
  audio: '#2a9d8f',
  image: '#8b6fc0',
  title: '#d4a017',
  solid: '#6b7280',
};

/** Colour of a clip whose audio is muted, or whose media is missing in the waveform. */
const GREYED_COLOR = '#4b5563';
/** Colour of group brackets when the group has no colour. */
const DEFAULT_GROUP_COLOR = '#ffb703';
/** Colour of the snap line. */
const SNAP_COLOR = '#ffd166';
/** Colour of markers without a colour of their own. */
const DEFAULT_MARKER_COLOR = '#f5c542';

/** Lowest and highest gain shown on an audio clip's envelope, dB. */
const DB_MIN = -60;
const DB_MAX = 12;
/** Clip width below which fade handles and triangles are hidden, px. */
const FADE_MIN_WIDTH_PX = 60;
/** Clip height above which an audio clip shows its gain line, px. */
const GAIN_MIN_HEIGHT_PX = 40;
/** Clip width below which the name label is not drawn, px. */
const LABEL_MIN_WIDTH_PX = 40;

/** Text that fits in `maxW` px: the whole string, or a prefix with an ellipsis; '' when even that is too wide. */
function fitLabel(ctx: CanvasRenderingContext2D, text: string, maxW: number): string {
  if (maxW <= 0 || text === '') return '';
  if (ctx.measureText(text).width <= maxW) return text;
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (ctx.measureText(text.slice(0, mid) + '…').width <= maxW) lo = mid;
    else hi = mid - 1;
  }
  return lo > 0 ? text.slice(0, lo) + '…' : '';
}
/** Waveform column width, px. */
const WAVE_STEP_PX = 2;

/** Colours read from the app's CSS variables (`apps/ui/src/app.css`), with fallbacks. */
export interface Palette {
  bg: string;
  surface: string;
  surface2: string;
  border: string;
  text: string;
  textDim: string;
  danger: string;
}

const FALLBACK_PALETTE: Palette = {
  bg: '#0f1115',
  surface: '#171a21',
  surface2: '#1f232c',
  border: '#2a2f3a',
  text: '#e6e8ee',
  textDim: '#9aa3b2',
  danger: '#ff6b6b',
};

/** Palette read per element. The UI has a single theme, so an element's palette is read once. */
const paletteCache = new WeakMap<Element, Palette>();

/**
 * Reads the app palette from the CSS variables that apply to `el`. Missing values fall back.
 * The result is cached per element for the element's lifetime.
 */
export function readPalette(el: Element): Palette {
  const cached = paletteCache.get(el);
  if (cached) return cached;

  let style: CSSStyleDeclaration | null = null;
  try {
    style = getComputedStyle(el);
  } catch {
    style = null;
  }
  const read = (name: string, fallback: string): string => {
    const value = style?.getPropertyValue(name).trim() ?? '';
    return value === '' ? fallback : value;
  };
  const palette: Palette = {
    bg: read('--bg', FALLBACK_PALETTE.bg),
    surface: read('--surface', FALLBACK_PALETTE.surface),
    surface2: read('--surface-2', FALLBACK_PALETTE.surface2),
    border: read('--border', FALLBACK_PALETTE.border),
    text: read('--text', FALLBACK_PALETTE.text),
    textDim: read('--text-dim', FALLBACK_PALETTE.textDim),
    danger: read('--danger', FALLBACK_PALETTE.danger),
  };
  paletteCache.set(el, palette);
  return palette;
}

/** Everything `drawTimeline` needs for one frame. The playhead is not part of it (the Timeline draws it as a DOM element). */
export interface DrawArgs {
  project: Project;
  rows: TrackRow[];
  view: View;
  selection: string[];
  snapTick: Tick | null;
  dragGhosts: { clipId: string; rect: ClipRect }[];
  marquee: { x0: number; y0: number; x1: number; y1: number } | null;
  /** Live fade lengths (ticks) of one audio clip while its fade handle is dragged. */
  fadePreview?: { clipId: string; fadeIn: Tick; fadeOut: Tick };
  /** Gap selected by a click, drawn as a highlighted span on its track row. */
  selectedGap?: { trackId: string; start: Tick; end: Tick } | null;
  /** Clip edge under the pointer while idle (trim affordance), drawn as a light bar. */
  hoverEdge?: { clipId: string; edge: 'start' | 'end' } | null;
  devicePixelRatio: number;
  thumbs: (mediaId: string) => unknown;
  peaks: (mediaId: string, stream: number) => Float32Array | null;
}

/**
 * Ids of clips that share a non-null `link_id` or `group_id` with a selected clip, excluding the
 * selected clips themselves. Two passes over the clips: O(clips) per call.
 */
function findPartners(clips: readonly Clip[], selected: Set<string>): Set<string> {
  const partners = new Set<string>();
  if (selected.size === 0) return partners;
  const linkIds = new Set<string>();
  const groupIds = new Set<string>();
  for (const clip of clips) {
    if (!selected.has(clip.id)) continue;
    if (clip.link_id) linkIds.add(clip.link_id);
    if (clip.group_id) groupIds.add(clip.group_id);
  }
  if (linkIds.size === 0 && groupIds.size === 0) return partners;
  for (const clip of clips) {
    if (selected.has(clip.id)) continue;
    if ((clip.link_id && linkIds.has(clip.link_id)) || (clip.group_id && groupIds.has(clip.group_id))) {
      partners.add(clip.id);
    }
  }
  return partners;
}

/** Clamps `v` into `lo..hi`. */
function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/** Position of a gain in dB inside a clip of height `h`, as 0 (bottom) to 1 (top). */
function dbFraction(db: number): number {
  const safe = Number.isFinite(db) ? db : 0;
  return clamp((safe - DB_MIN) / (DB_MAX - DB_MIN), 0, 1);
}

/** Narrows a value returned by `DrawArgs.thumbs` to a `ThumbStrip`. */
function isThumbStrip(v: unknown): v is ThumbStrip {
  if (typeof v !== 'object' || v === null) return false;
  const candidate = v as Partial<ThumbStrip>;
  return Array.isArray(candidate.frames) && typeof candidate.durationTicks === 'number';
}

/** Speed as a short label: "2", "0.5", "1.25". */
function formatSpeed(speed: number): string {
  if (Number.isInteger(speed)) return String(speed);
  return speed.toFixed(2).replace(/0+$/, '').replace(/\.$/, '');
}

/**
 * Adds a rounded rectangle path to a new path (does not fill or stroke). `left` and `right` say
 * whether that end gets rounded corners; an end that is off screen passes false and stays square.
 */
function roundRectPath(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  radius: number,
  left = true,
  right = true,
): void {
  const r = Math.max(0, Math.min(radius, w / 2, h / 2));
  const rl = left ? r : 0;
  const rr = right ? r : 0;
  ctx.beginPath();
  ctx.moveTo(x + rl, y);
  ctx.arcTo(x + w, y, x + w, y + h, rr);
  ctx.arcTo(x + w, y + h, x, y + h, rr);
  ctx.arcTo(x, y + h, x, y, rl);
  ctx.arcTo(x, y, x + w, y, rl);
  ctx.closePath();
}

/** Px beyond each view edge that path shapes still reach, so rounded ends just off screen stay correct. */
const PATH_OVERSHOOT_PX = 64;

/** Horizontal extent of a clip for path drawing: clamped to just past the view. */
interface PathSpan {
  x: number;
  w: number;
  /** True when the clip's left end is on screen, so its corners are rounded. */
  left: boolean;
  /** True when the clip's right end is on screen, so its corners are rounded. */
  right: boolean;
}

/**
 * Clamps a clip's horizontal extent to `-PATH_OVERSHOOT_PX .. viewWidth + PATH_OVERSHOOT_PX`. A clip
 * far wider than the view (long media at high zoom) would otherwise hand the canvas coordinates
 * with float32 precision errors. The clamp does not change what is visible; the unclamped rect
 * still maps thumbnails and waveforms.
 */
export function pathSpan(x: number, w: number, viewWidth: number): PathSpan {
  const x0 = Math.max(x, -PATH_OVERSHOOT_PX);
  const x1 = Math.min(x + w, viewWidth + PATH_OVERSHOOT_PX);
  return {
    x: x0,
    w: Math.max(0, x1 - x0),
    left: x >= 0 && x <= viewWidth,
    right: x + w >= 0 && x + w <= viewWidth,
  };
}

/**
 * Fills `x, y, w, h` with diagonal hatch lines in `color`. Only the lines that can reach the
 * visible area `0..viewWidth` are generated, so a clip much wider than the view costs little.
 * The hatch phase is independent of the visible range (lines sit on a fixed grid).
 */
function hatch(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  color: string,
  spacing: number,
  viewWidth: number,
): void {
  if (w <= 0 || h <= 0 || spacing <= 0) return;
  // Offsets d place a line from (x + d, y + h) to (x + d + h, y). Keep those that touch the
  // clip (d in -h..w) and the view (x + d + h >= 0 and x + d <= viewWidth).
  const lo = Math.max(-h, -x - h);
  const hi = Math.min(w, viewWidth - x);
  if (hi <= lo) return;
  const steps = Math.max(0, Math.ceil((lo + h) / spacing));
  const first = -h + steps * spacing;

  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, w, h);
  ctx.clip();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let d = first; d < hi; d += spacing) {
    ctx.moveTo(x + d, y + h);
    ctx.lineTo(x + d + h, y);
  }
  ctx.stroke();
  ctx.restore();
}

/** Track rows: alternating shades, locked hatch, dim for hidden or muted tracks, separators. */
function drawRows(
  ctx: CanvasRenderingContext2D,
  rows: readonly TrackRow[],
  tracks: Map<string, Track>,
  view: View,
  palette: Palette,
  areaHeight: number,
): void {
  rows.forEach((row, i) => {
    if (row.top + row.height < RULER_HEIGHT || row.top > areaHeight) return;
    const track = tracks.get(row.trackId);
    ctx.fillStyle = i % 2 === 0 ? palette.surface : palette.surface2;
    ctx.fillRect(0, row.top, view.width, row.height);
    if (track?.locked) {
      hatch(ctx, 0, row.top, view.width, row.height, palette.border, 8, view.width);
    }
    if (track && (track.hidden || track.muted)) {
      ctx.fillStyle = 'rgba(0, 0, 0, 0.35)';
      ctx.fillRect(0, row.top, view.width, row.height);
    }
    ctx.fillStyle = palette.border;
    ctx.fillRect(0, row.top + row.height, view.width, 1);
  });
}

/** Tints the work area (in/out range) over the track area. */
function drawWorkArea(
  ctx: CanvasRenderingContext2D,
  area: WorkArea | null,
  view: View,
  areaHeight: number,
): void {
  if (!area || area.end <= area.start) return;
  const x0 = clamp(tickToX(area.start, view), 0, view.width);
  const x1 = clamp(tickToX(area.end, view), 0, view.width);
  if (x1 <= x0) return;
  ctx.fillStyle = 'rgba(255, 255, 255, 0.05)';
  ctx.fillRect(x0, RULER_HEIGHT, x1 - x0, Math.max(0, areaHeight - RULER_HEIGHT));
}

/** Video thumbnails tiled across the clip's source range. Only tiles inside the view are drawn. */
function drawThumbs(
  ctx: CanvasRenderingContext2D,
  clip: Clip,
  rect: ClipRect,
  media: MediaRef,
  strip: ThumbStrip,
  view: View,
): void {
  const count = strip.frames.length;
  if (count === 0 || media.duration <= 0 || rect.w <= 0) return;

  const top = rect.y + 1;
  const h = Math.max(0, rect.h - 2);
  let aspect = 16 / 9;
  for (const frame of strip.frames) {
    if (frame && frame.naturalHeight > 0) {
      aspect = frame.naturalWidth / frame.naturalHeight;
      break;
    }
  }
  const tileW = Math.max(8, h * aspect);
  const tiles = Math.ceil(rect.w / tileW);
  const first = Math.max(0, Math.floor((Math.max(0, -rect.x)) / tileW));
  const last = Math.min(tiles, Math.ceil((view.width - rect.x) / tileW));
  const srcLen = clip.dur * clip.speed;

  for (let i = first; i < last; i += 1) {
    const x0 = rect.x + i * tileW;
    const w = Math.min(tileW, rect.x + rect.w - x0);
    if (w <= 0) continue;
    const centre = (i * tileW + w / 2) / rect.w;
    const offset = clamp(centre, 0, 1) * srcLen;
    const srcTime = clip.reverse ? clip.src_in + srcLen - offset : clip.src_in + offset;
    const frac = clamp(srcTime / media.duration, 0, 1);
    const index = clamp(Math.round(frac * count - 0.5), 0, count - 1);
    const frame = strip.frames[index] ?? null;
    if (!frame || frame.naturalWidth <= 0) continue;
    const sw = frame.naturalWidth * (w / tileW);
    ctx.drawImage(frame, 0, 0, sw, frame.naturalHeight, x0, top, w, h);
  }
}

/**
 * X positions of the waveform columns that start inside `x0..x1`. Columns sit on a grid anchored
 * at the clip's left edge `rectX`, so the grid moves with the clip and does not shimmer while the
 * view scrolls. Pure; exported for tests.
 */
export function waveformGridXs(rectX: number, x0: number, x1: number, step: number): number[] {
  if (!(step > 0) || !(x1 > x0) || !Number.isFinite(rectX)) return [];
  const xs: number[] = [];
  const k0 = Math.ceil((x0 - rectX) / step);
  for (let k = k0; ; k += 1) {
    const x = rectX + k * step;
    if (x >= x1) break;
    xs.push(x);
  }
  return xs;
}

/**
 * Largest peak (0..1) of the buckets `[b0, b1)` covered by the fraction range `fromFrac..toFrac` of
 * a clip (0 = clip start, 1 = clip end). A reversed clip plays its source backwards, so the range
 * is mirrored. Taking the maximum keeps short transients visible when zoomed out. Pure; exported for tests.
 */
export function columnPeak(
  peaks: ArrayLike<number>,
  b0: number,
  b1: number,
  fromFrac: number,
  toFrac: number,
  reverse: boolean,
): number {
  const span = b1 - b0;
  if (span <= 0) return 0;
  const lo = clamp(reverse ? 1 - toFrac : fromFrac, 0, 1);
  const hi = clamp(reverse ? 1 - fromFrac : toFrac, 0, 1);
  const first = clamp(b0 + Math.floor(lo * span), b0, b1 - 1);
  const last = clamp(b0 + Math.ceil(hi * span) - 1, first, b1 - 1);
  let peak = 0;
  for (let i = first; i <= last; i += 1) {
    const v = clamp(peaks[i] ?? 0, 0, 1);
    if (v > peak) peak = v;
  }
  return peak;
}

/** Mirrored peak bars across the clip's source range. Only columns inside the view are drawn. */
function drawWaveform(
  ctx: CanvasRenderingContext2D,
  clip: Clip,
  rect: ClipRect,
  media: MediaRef,
  peaks: Float32Array,
  view: View,
  alpha: number,
): void {
  const buckets = peaks.length;
  if (buckets === 0 || media.duration <= 0 || rect.w <= 0) return;
  const srcLen = clip.dur * clip.speed;
  const b0 = clamp(Math.floor((clip.src_in / media.duration) * buckets), 0, buckets - 1);
  const b1 = clamp(Math.ceil(((clip.src_in + srcLen) / media.duration) * buckets), b0 + 1, buckets);
  const mid = rect.y + rect.h / 2;
  const half = Math.max(1, rect.h / 2 - 5);

  ctx.fillStyle = `rgba(255, 255, 255, ${alpha})`;
  const xs = waveformGridXs(rect.x, Math.max(rect.x, 0), Math.min(rect.x + rect.w, view.width), WAVE_STEP_PX);
  for (const x of xs) {
    const fromFrac = (x - rect.x) / rect.w;
    const toFrac = (x + WAVE_STEP_PX - rect.x) / rect.w;
    const amp = columnPeak(peaks, b0, b1, fromFrac, toFrac, clip.reverse);
    const bar = Math.max(1, amp * half);
    ctx.fillRect(x, mid - bar, 1.5, bar * 2);
  }
}

/**
 * Fade triangles and, when the clip is wide enough, the square handles at the fade apexes. Only
 * audio clips have fades. The handles sit exactly where the Timeline hit-tests them: at
 * `rect.x + fadeIn` and `rect.right - fadeOut`, at the top of the clip.
 */
function drawFades(
  ctx: CanvasRenderingContext2D,
  rect: ClipRect,
  pxPerSec: number,
  fadeInTicks: Tick,
  fadeOutTicks: Tick,
): void {
  const fadeIn = clamp((fadeInTicks / TICKS_PER_SECOND) * pxPerSec, 0, rect.w);
  const fadeOut = clamp((fadeOutTicks / TICKS_PER_SECOND) * pxPerSec, 0, rect.w);
  const bottom = rect.y + rect.h;
  const right = rect.x + rect.w;

  if (fadeIn > 0) {
    ctx.fillStyle = 'rgba(0, 0, 0, 0.4)';
    ctx.beginPath();
    ctx.moveTo(rect.x, rect.y);
    ctx.lineTo(rect.x, bottom);
    ctx.lineTo(rect.x + fadeIn, rect.y);
    ctx.closePath();
    ctx.fill();
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.8)';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(rect.x, bottom);
    ctx.lineTo(rect.x + fadeIn, rect.y);
    ctx.stroke();
  }
  if (fadeOut > 0) {
    ctx.fillStyle = 'rgba(0, 0, 0, 0.4)';
    ctx.beginPath();
    ctx.moveTo(right, rect.y);
    ctx.lineTo(right, bottom);
    ctx.lineTo(right - fadeOut, rect.y);
    ctx.closePath();
    ctx.fill();
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.8)';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(right, bottom);
    ctx.lineTo(right - fadeOut, rect.y);
    ctx.stroke();
  }

  if (rect.w >= FADE_MIN_WIDTH_PX) {
    // Squares centred on the apexes. The body clip trims any part that leaves the clip.
    ctx.fillStyle = 'rgba(255, 255, 255, 0.85)';
    ctx.fillRect(rect.x + fadeIn - 3, rect.y + 1, 6, 6);
    ctx.fillRect(right - fadeOut - 3, rect.y + 1, 6, 6);
  }
}

/** Volume envelope polyline (audio clips with points) and the gain line (audio clips taller than 40 px). */
function drawEnvelope(ctx: CanvasRenderingContext2D, clip: Clip, rect: ClipRect): void {
  if (clip.kind !== 'audio' || clip.dur <= 0) return;
  const yOf = (db: number): number => rect.y + rect.h - dbFraction(db) * rect.h;
  const xOf = (t: number): number => rect.x + (clamp(t, 0, clip.dur) / clip.dur) * rect.w;
  const points = clip.audio.envelope;
  const first = points[0];
  const last = points[points.length - 1];

  if (first && last) {
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.85)';
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(rect.x, yOf(first.db));
    for (const p of points) {
      ctx.lineTo(xOf(p.t), yOf(p.db));
    }
    ctx.lineTo(rect.x + rect.w, yOf(last.db));
    ctx.stroke();
  }

  if (rect.h > GAIN_MIN_HEIGHT_PX) {
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.5)';
    ctx.lineWidth = 1;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(rect.x, yOf(clip.audio.gain_db));
    ctx.lineTo(rect.x + rect.w, yOf(clip.audio.gain_db));
    ctx.stroke();
    ctx.setLineDash([]);
  }
}

/** Diagonal hatch over the first `duration` of the clip, marking an incoming transition. */
function drawTransition(
  ctx: CanvasRenderingContext2D,
  clip: Clip,
  rect: ClipRect,
  pxPerSec: number,
): void {
  if (!clip.transition_in) return;
  const width = clamp((clip.transition_in.duration / TICKS_PER_SECOND) * pxPerSec, 0, rect.w);
  if (width <= 0) return;
  ctx.save();
  ctx.beginPath();
  ctx.rect(rect.x, rect.y, width, rect.h);
  ctx.clip();
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.45)';
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let d = -rect.h; d < width; d += 6) {
    ctx.moveTo(rect.x + d, rect.y + rect.h);
    ctx.lineTo(rect.x + d + rect.h, rect.y);
  }
  ctx.stroke();
  ctx.restore();
}

/** A small label chip, drawn at `x` on the bottom edge of a clip. Returns the chip width. */
function drawBadge(
  ctx: CanvasRenderingContext2D,
  text: string,
  x: number,
  bottom: number,
  textColor: string,
): number {
  ctx.font = '10px system-ui, -apple-system, sans-serif';
  const width = ctx.measureText(text).width + 8;
  const height = 14;
  ctx.fillStyle = 'rgba(0, 0, 0, 0.65)';
  roundRectPath(ctx, x, bottom - height - 3, width, height, 3);
  ctx.fill();
  ctx.fillStyle = textColor;
  ctx.textBaseline = 'middle';
  ctx.fillText(text, x + 4, bottom - height / 2 - 3);
  return width;
}

/** Chain glyph (two linked rounded rectangles) in the bottom-right corner of a linked clip. */
function drawLinkGlyph(ctx: CanvasRenderingContext2D, rect: ClipRect): void {
  const cx = rect.x + rect.w - 9;
  const cy = rect.y + rect.h - 8;
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.9)';
  ctx.lineWidth = 1.2;
  roundRectPath(ctx, cx - 6, cy - 2.5, 7, 5, 2.5);
  ctx.stroke();
  roundRectPath(ctx, cx - 1, cy - 2.5, 7, 5, 2.5);
  ctx.stroke();
}

/** Draws one clip: body, content, outline, badges and link glyph. Culling is done by the caller. */
function drawClip(
  ctx: CanvasRenderingContext2D,
  clip: Clip,
  rect: ClipRect,
  args: DrawArgs,
  tracks: Map<string, Track>,
  media: Map<string, MediaRef>,
  selected: Set<string>,
  partners: Set<string>,
  palette: Palette,
): void {
  const { view } = args;
  const track = tracks.get(clip.track_id);
  const mediaRef = clip.media_id ? media.get(clip.media_id) : undefined;
  const isMissing = mediaRef?.missing === true;
  const isSelected = selected.has(clip.id);
  const isPartner = !isSelected && partners.has(clip.id);
  const audioMuted = clip.kind === 'audio' && clip.audio.muted;
  const fill = audioMuted ? GREYED_COLOR : (clip.color ?? KIND_COLORS[clip.kind]);
  const bodyX = rect.x;
  const bodyY = rect.y + 1;
  const bodyW = rect.w;
  const bodyH = Math.max(0, rect.h - 2);
  // Path shapes use the clamped span; thumbnails and waveforms keep the unclamped rect (above).
  const span = pathSpan(rect.x, rect.w, view.width);
  const pxPerSec = view.pxPerSec;

  ctx.save();
  ctx.globalAlpha = clip.enabled ? 1 : 0.4;

  // Body fill.
  roundRectPath(ctx, span.x, bodyY, span.w, bodyH, 4, span.left, span.right);
  ctx.fillStyle = fill;
  ctx.fill();

  // Content, clipped to the body.
  ctx.save();
  roundRectPath(ctx, span.x, bodyY, span.w, bodyH, 4, span.left, span.right);
  ctx.clip();

  if (clip.kind === 'video' && mediaRef && !isMissing) {
    const strip: unknown = args.thumbs(mediaRef.id);
    if (isThumbStrip(strip)) {
      drawThumbs(ctx, clip, rect, mediaRef, strip, view);
    }
  }

  if (clip.kind === 'audio' && mediaRef && !isMissing) {
    const peaks = args.peaks(mediaRef.id, clip.audio.source_track);
    if (peaks) {
      drawWaveform(ctx, clip, rect, mediaRef, peaks, view, audioMuted ? 0.25 : 0.7);
    }
  }

  drawTransition(ctx, clip, rect, pxPerSec);
  if (clip.kind === 'audio') {
    const preview = args.fadePreview?.clipId === clip.id ? args.fadePreview : null;
    drawFades(
      ctx,
      rect,
      pxPerSec,
      preview ? preview.fadeIn : clip.audio.fade_in,
      preview ? preview.fadeOut : clip.audio.fade_out,
    );
  }
  drawEnvelope(ctx, clip, rect);

  if (isMissing) {
    ctx.fillStyle = 'rgba(255, 107, 107, 0.3)';
    ctx.fillRect(span.x, bodyY, span.w, bodyH);
    hatch(ctx, bodyX, bodyY, bodyW, bodyH, palette.danger, 10, view.width);
  }

  if (bodyW >= LABEL_MIN_WIDTH_PX) {
    ctx.font = '11px system-ui, -apple-system, sans-serif';
    ctx.textBaseline = 'top';
    ctx.fillStyle = '#ffffff';
    ctx.shadowColor = 'rgba(0, 0, 0, 0.75)';
    ctx.shadowBlur = 3;
    const label = fitLabel(ctx, clip.name, bodyW - 12);
    if (label) ctx.fillText(label, bodyX + 6, bodyY + 4);
    ctx.shadowBlur = 0;
    ctx.shadowColor = 'rgba(0, 0, 0, 0)';
  }
  ctx.restore();

  // Outline: accent when selected, subtle border otherwise.
  if (isSelected) {
    roundRectPath(ctx, span.x + 1, rect.y + 1, Math.max(0, span.w - 2), Math.max(0, rect.h - 2), 4, span.left, span.right);
    ctx.strokeStyle = ACCENT;
    ctx.lineWidth = 2;
    ctx.stroke();
  } else {
    roundRectPath(ctx, span.x + 0.5, bodyY + 0.5, Math.max(0, span.w - 1), Math.max(0, bodyH - 1), 4, span.left, span.right);
    ctx.strokeStyle = 'rgba(0, 0, 0, 0.5)';
    ctx.lineWidth = 1;
    ctx.stroke();
  }
  // Linked or grouped partners of the selection: dashed, muted accent, inside the bounds.
  if (isPartner) {
    roundRectPath(ctx, span.x + 0.75, bodyY + 0.75, Math.max(0, span.w - 1.5), Math.max(0, bodyH - 1.5), 4, span.left, span.right);
    ctx.save();
    ctx.globalAlpha *= 0.55;
    ctx.strokeStyle = ACCENT;
    ctx.lineWidth = 1.5;
    ctx.setLineDash([4, 3]);
    ctx.stroke();
    ctx.restore();
  }

  // Badges along the bottom edge: missing media, speed, reverse.
  if (rect.w >= 40) {
    let x = rect.x + 4;
    const bottom = rect.y + rect.h;
    if (isMissing) {
      x += drawBadge(ctx, 'Missing media', x, bottom, palette.danger) + 4;
    }
    if (clip.speed !== 1) {
      const label = clip.speed === 0 ? 'Freeze' : `${formatSpeed(clip.speed)}x`;
      x += drawBadge(ctx, label, x, bottom, palette.text) + 4;
    }
    if (clip.reverse) {
      drawBadge(ctx, 'Rev', x, bottom, palette.text);
    }
  }

  if (clip.link_id && (track?.kind === 'audio' || track?.kind === 'video') && rect.w >= 30) {
    drawLinkGlyph(ctx, rect);
  }

  ctx.restore();
}

/** Group brackets: one coloured line above each group's clips, spanning from its first to its last clip. */
function drawGroupBrackets(
  ctx: CanvasRenderingContext2D,
  project: Project,
  rows: readonly TrackRow[],
  view: View,
): void {
  const colours = new Map<string, string | null>();
  for (const group of project.groups) {
    colours.set(group.id, group.color);
  }
  const spans = new Map<string, { x0: number; x1: number; top: number }>();
  for (const clip of project.clips) {
    if (!clip.group_id) continue;
    const rect = clipRect(clip, rows, view);
    if (!rect) continue;
    const span = spans.get(clip.group_id);
    if (span) {
      span.x0 = Math.min(span.x0, rect.x);
      span.x1 = Math.max(span.x1, rect.x + rect.w);
      span.top = Math.min(span.top, rect.y);
    } else {
      spans.set(clip.group_id, { x0: rect.x, x1: rect.x + rect.w, top: rect.y });
    }
  }
  for (const [groupId, span] of spans) {
    if (span.x1 < 0 || span.x0 > view.width) continue;
    const y = Math.max(RULER_HEIGHT + 1, span.top - 3);
    ctx.fillStyle = colours.get(groupId) ?? DEFAULT_GROUP_COLOR;
    ctx.fillRect(span.x0, y, Math.max(1, span.x1 - span.x0), 3);
  }
}

/** Markers as thin vertical lines over the track area. */
function drawMarkers(
  ctx: CanvasRenderingContext2D,
  project: Project,
  view: View,
  areaHeight: number,
): void {
  ctx.lineWidth = 1;
  ctx.globalAlpha = 0.8;
  for (const marker of project.markers) {
    const x = Math.round(tickToX(marker.time, view)) + 0.5;
    if (x < 0 || x > view.width) continue;
    ctx.strokeStyle = marker.color ?? DEFAULT_MARKER_COLOR;
    ctx.beginPath();
    ctx.moveTo(x, RULER_HEIGHT);
    ctx.lineTo(x, areaHeight);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;
}

/** Dashed vertical snap line at `tick`. */
function drawSnap(ctx: CanvasRenderingContext2D, tick: Tick, view: View, areaHeight: number): void {
  const x = Math.round(tickToX(tick, view)) + 0.5;
  if (x < 0 || x > view.width) return;
  ctx.strokeStyle = SNAP_COLOR;
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.beginPath();
  ctx.moveTo(x, RULER_HEIGHT);
  ctx.lineTo(x, areaHeight);
  ctx.stroke();
  ctx.setLineDash([]);
}

/** Semi-transparent outlines of the clips being dragged. */
function drawGhosts(ctx: CanvasRenderingContext2D, ghosts: DrawArgs['dragGhosts']): void {
  for (const ghost of ghosts) {
    const r = ghost.rect;
    ctx.fillStyle = 'rgba(255, 255, 255, 0.08)';
    ctx.fillRect(r.x, r.y, r.w, r.h);
    ctx.strokeStyle = 'rgba(255, 255, 255, 0.75)';
    ctx.lineWidth = 1;
    ctx.setLineDash([5, 3]);
    ctx.strokeRect(r.x + 0.5, r.y + 0.5, Math.max(0, r.w - 1), Math.max(0, r.h - 1));
    ctx.setLineDash([]);
  }
}

/** Width of the hovered-edge handle bar, px. */
const EDGE_HANDLE_PX = 3;

/** Light bar at the hovered clip edge, so the trim affordance is visible before pressing. */
function drawEdgeHandle(ctx: CanvasRenderingContext2D, hover: NonNullable<DrawArgs['hoverEdge']>, args: DrawArgs): void {
  const clip = args.project.clips.find((c) => c.id === hover.clipId);
  if (!clip) return;
  const rect = clipRect(clip, args.rows, args.view);
  if (!rect) return;
  const x = hover.edge === 'start' ? rect.x : rect.x + rect.w - EDGE_HANDLE_PX;
  ctx.fillStyle = 'rgba(255, 255, 255, 0.85)';
  ctx.fillRect(x, rect.y + 1, EDGE_HANDLE_PX, Math.max(0, rect.h - 2));
}

/**
 * Selected gap: a translucent accent fill across the track row between its two clip edges, with a
 * dashed outline. Spans on rows that are not drawn (scrolled out) fall outside the clip area.
 */
function drawSelectedGap(ctx: CanvasRenderingContext2D, gap: NonNullable<DrawArgs['selectedGap']>, args: DrawArgs): void {
  const row = args.rows.find((r) => r.trackId === gap.trackId);
  if (!row) return;
  const x0 = tickToX(gap.start, args.view);
  const x1 = tickToX(gap.end, args.view);
  const w = Math.max(0, x1 - x0);
  ctx.fillStyle = 'rgba(237, 28, 36, 0.18)';
  ctx.fillRect(x0, row.top, w, row.height);
  ctx.strokeStyle = ACCENT;
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.strokeRect(x0 + 0.5, row.top + 0.5, Math.max(0, w - 1), Math.max(0, row.height - 1));
  ctx.setLineDash([]);
}

/** Rubber-band selection rectangle, normalised so any drag direction works. */
function drawMarquee(ctx: CanvasRenderingContext2D, m: NonNullable<DrawArgs['marquee']>): void {
  const x = Math.min(m.x0, m.x1);
  const y = Math.min(m.y0, m.y1);
  const w = Math.abs(m.x1 - m.x0);
  const h = Math.abs(m.y1 - m.y0);
  ctx.fillStyle = 'rgba(237, 28, 36, 0.12)';
  ctx.fillRect(x, y, w, h);
  ctx.strokeStyle = ACCENT;
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 3]);
  ctx.strokeRect(x + 0.5, y + 0.5, Math.max(0, w - 1), Math.max(0, h - 1));
  ctx.setLineDash([]);
}

/**
 * Draws the whole track area for one frame. Clips outside the view are skipped. Thumbnails and
 * waveforms are looked up through `args.thumbs` and `args.peaks`; missing data draws nothing.
 */
export function drawTimeline(ctx: CanvasRenderingContext2D, args: DrawArgs): void {
  const { project, rows, view } = args;
  const width = view.width;
  const height = view.height;
  if (!(width > 0) || !(height > 0)) return;
  const dpr = Number.isFinite(args.devicePixelRatio) && args.devicePixelRatio > 0 ? args.devicePixelRatio : 1;
  const palette = readPalette(ctx.canvas);

  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = palette.bg;
  ctx.fillRect(0, 0, width, height);

  const tracks = new Map<string, Track>();
  for (const track of project.tracks) {
    tracks.set(track.id, track);
  }
  const media = new Map<string, MediaRef>();
  for (const m of project.media) {
    media.set(m.id, m);
  }
  const selected = new Set<string>(args.selection);
  // Partners: clips sharing a link or group with a selected clip, minus the selection itself.
  const partners = findPartners(project.clips, selected);

  // Everything below is clipped to the area under the ruler.
  ctx.save();
  ctx.beginPath();
  ctx.rect(0, RULER_HEIGHT, width, Math.max(0, height - RULER_HEIGHT));
  ctx.clip();

  drawRows(ctx, rows, tracks, view, palette, height);
  drawWorkArea(ctx, project.work_area, view, height);

  for (const clip of project.clips) {
    const rect = clipRect(clip, rows, view);
    if (!rect) continue;
    if (rect.x + rect.w < 0 || rect.x > width) continue;
    if (rect.y + rect.h < RULER_HEIGHT || rect.y > height) continue;
    drawClip(ctx, clip, rect, args, tracks, media, selected, partners, palette);
  }

  drawGroupBrackets(ctx, project, rows, view);
  if (args.selectedGap) {
    drawSelectedGap(ctx, args.selectedGap, args);
  }
  if (args.hoverEdge) {
    drawEdgeHandle(ctx, args.hoverEdge, args);
  }
  drawMarkers(ctx, project, view, height);
  if (args.snapTick !== null) {
    drawSnap(ctx, args.snapTick, view, height);
  }
  drawGhosts(ctx, args.dragGhosts);
  if (args.marquee) {
    drawMarquee(ctx, args.marquee);
  }

  ctx.restore();
}

/**
 * Requests thumbnails for the video clips and waveforms for the audio clips of the project.
 * Media is requested once per call; `mediaCache` deduplicates across calls. `onchange` is called
 * when data arrives, so the caller can redraw.
 */
export function ensureMediaAssets(project: Project, onchange: () => void): void {
  const media = new Map<string, MediaRef>();
  for (const m of project.media) {
    media.set(m.id, m);
  }
  const seenThumbs = new Set<string>();
  const seenPeaks = new Set<string>();
  for (const clip of project.clips) {
    if (!clip.media_id) continue;
    const m = media.get(clip.media_id);
    if (!m || m.missing) continue;
    if (clip.kind === 'video' && !seenThumbs.has(m.id)) {
      seenThumbs.add(m.id);
      requestThumbs(project, m.id, onchange);
    } else if (clip.kind === 'audio') {
      const key = `${m.id}:${clip.audio.source_track}`;
      if (!seenPeaks.has(key)) {
        seenPeaks.add(key);
        requestPeaks(project, m.id, clip.audio.source_track, onchange);
      }
    }
  }
}
