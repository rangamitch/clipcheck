/**
 * Canvas 2D drawing helpers for the program monitor. Mirrors the export compositor in
 * crates/render/src/reference.rs: dest_rect (fit, fill, stretch, custom), crop, quarter-turn
 * rotation, flips, the blurred backdrop and dips to colour. Titles and solids are rendered to a
 * project-size canvas and placed like media, as the export does. Pure drawing code: no stores, no
 * Tauri.
 */

import type { PlanSegment, Project, Rgba, TextSpec, Tick, Transform, VideoLayer } from '../types';
import { defaultTransform } from '../types';
import { dipAmount, layerOpacityAt } from './scheduler';

/** Source frames for media layers. A null return means the frame is not ready yet. */
export interface SourceFrames {
  video: (mediaId: string) => CanvasImageSource | null;
  image: (mediaId: string) => CanvasImageSource | null;
  /** True when the media failed to load. Failed layers are drawn as opaque black. */
  failed: (mediaId: string) => boolean;
}

/** Axis-aligned rectangle in canvas pixels. */
export interface CanvasRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Pixel size of a source. */
interface Size {
  w: number;
  h: number;
}

/** Outcome of looking up the source frame of one layer. */
type Resolution =
  | { kind: 'ready'; src: CanvasImageSource; size: Size }
  | { kind: 'failed' }
  | { kind: 'empty' }
  | { kind: 'missing' };

/** A layer of the segment together with its resolved source. */
interface ResolvedLayer {
  layer: VideoLayer;
  res: Resolution;
}

/**
 * What the target context currently shows. `shown` is true only after a frame was painted without
 * error at `w` x `h`; the context is trusted to keep that frame until its size changes.
 * `snapshot` is a copy of the last good frame, refreshed at most every SNAPSHOT_MS.
 */
interface FrameState {
  w: number;
  h: number;
  shown: boolean;
  snapshot: HTMLCanvasElement | null;
  snapshotAt: number;
}

/** Last good frame state per target context, so a missing source keeps the previous picture. */
const frameStates = new WeakMap<CanvasRenderingContext2D, FrameState>();

/** Minimum interval between copies of the last good frame (ms). */
const SNAPSHOT_MS = 250;

/** Number of generated title and solid canvases kept. Each is project sized, so keep it small. */
const LAYER_CACHE_LIMIT = 6;

/** Title and solid canvases by key (text spec JSON or colour, plus project size). */
const layerCache = new Map<string, HTMLCanvasElement>();

/** Scratch canvas for the blurred backdrop (1/8 of the target size). */
let backdropScratch: HTMLCanvasElement | null = null;

/** Blur backdrop is computed at this fraction of the target size. */
const BACKDROP_DOWNSCALE = 8;

/** Font families that CSS treats as generic: they must not be quoted. */
const GENERIC_FAMILIES = new Set(['serif', 'sans-serif', 'monospace', 'cursive', 'fantasy', 'system-ui']);

/** Font used when a title names no family. */
const DEFAULT_FONT = 'Segoe UI';

/** Transform used for the blurred backdrop: the whole source, covering the canvas. */
const BACKDROP_TRANSFORM: Transform = { ...defaultTransform(), fit: 'fill' };

/** Clamps a fraction into 0..=1. Non-finite values map to 0. */
function frac(v: number): number {
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0;
}

/** `v` when finite, otherwise `fallback`. */
function finiteOr(v: number, fallback: number): number {
  return Number.isFinite(v) ? v : fallback;
}

/** Quarter turns normalised to 0..=3. */
function quarterTurns(rotate: number): number {
  const r = Math.trunc(finiteOr(rotate, 0));
  return ((r % 4) + 4) % 4;
}

/** Clamps a colour channel or alpha value to a byte. */
function clampByte(v: number): number {
  return Number.isFinite(v) ? Math.min(255, Math.max(0, Math.round(v))) : 0;
}

/** CSS rgba() string for a project colour (alpha is 0..=255). */
function cssColor(c: Rgba): string {
  return `rgba(${clampByte(c.r)}, ${clampByte(c.g)}, ${clampByte(c.b)}, ${clampByte(c.a) / 255})`;
}

/** Clamps an opacity or alpha value into 0..=1. Non-finite values map to 0. */
function unitAlpha(v: number): number {
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0;
}

/** Scale factor from project pixels to canvas pixels along one axis. */
function axisScale(canvasPx: number, projectPx: number): number {
  return projectPx > 0 && Number.isFinite(canvasPx) && canvasPx > 0 ? canvasPx / projectPx : 1;
}

/** Pixel size of a drawable source. Returns null when it has no usable size yet (e.g. a video not loaded). */
function sourceSize(src: CanvasImageSource): Size | null {
  const rec = src as unknown as Record<string, unknown>;
  let wKey = 'width';
  let hKey = 'height';
  if ('videoWidth' in rec) {
    wKey = 'videoWidth';
    hKey = 'videoHeight';
  } else if ('naturalWidth' in rec) {
    wKey = 'naturalWidth';
    hKey = 'naturalHeight';
  }
  const w = rec[wKey];
  const h = rec[hKey];
  if (typeof w !== 'number' || typeof h !== 'number') {
    return null;
  }
  if (!(Number.isFinite(w) && Number.isFinite(h) && w > 0 && h > 0)) {
    return null;
  }
  return { w, h };
}

/**
 * Destination rectangle in the given pixel space. Mirrors `dest_rect` in reference.rs:
 * crop removes fractions from the source first; rotation by an odd number of quarter turns swaps
 * the oriented size; fit = uniform scale so the whole source is inside, centred; fill = uniform
 * scale to cover, centred; stretch = exact size; custom = fit size times scale, centred, then
 * offset by pos_x * width and pos_y * height.
 */
function destRect(srcW: number, srcH: number, cw: number, ch: number, t: Transform): CanvasRect | null {
  const keepW = 1 - frac(t.crop.left) - frac(t.crop.right);
  const keepH = 1 - frac(t.crop.top) - frac(t.crop.bottom);
  const sw = srcW * Math.max(0, keepW);
  const sh = srcH * Math.max(0, keepH);
  const odd = quarterTurns(t.rotate_90) % 2 === 1;
  const ow = odd ? sh : sw;
  const oh = odd ? sw : sh;
  if (!(ow > 0 && oh > 0 && cw > 0 && ch > 0)) {
    return null;
  }
  const fitScale = Math.min(cw / ow, ch / oh);
  let w: number;
  let h: number;
  let offX = 0;
  let offY = 0;
  if (t.fit === 'fit') {
    w = ow * fitScale;
    h = oh * fitScale;
  } else if (t.fit === 'fill') {
    const s = Math.max(cw / ow, ch / oh);
    w = ow * s;
    h = oh * s;
  } else if (t.fit === 'stretch') {
    w = cw;
    h = ch;
  } else {
    if (!(Number.isFinite(t.scale) && t.scale > 0)) {
      return null;
    }
    const s = fitScale * t.scale;
    w = ow * s;
    h = oh * s;
    offX = finiteOr(t.pos_x, 0) * cw;
    offY = finiteOr(t.pos_y, 0) * ch;
  }
  return { x: (cw - w) / 2 + offX, y: (ch - h) / 2 + offY, w, h };
}

/** Integer crop along one axis, mirroring `crop_span` in reference.rs. At least one pixel remains. */
function cropAxis(len: number, lo: number, hi: number): { start: number; n: number } {
  const total = Math.max(1, Math.floor(len));
  const a = Math.min(Math.floor(frac(lo) * total), total - 1);
  const b = Math.floor(frac(hi) * total);
  const n = Math.max(total - a - b, 1);
  return { start: a, n: Math.min(n, total - a) };
}

/** Source rectangle (in source pixels) that remains after cropping. */
function cropRegion(srcW: number, srcH: number, t: Transform): { sx: number; sy: number; sw: number; sh: number } {
  const x = cropAxis(srcW, t.crop.left, t.crop.right);
  const y = cropAxis(srcH, t.crop.top, t.crop.bottom);
  return { sx: x.start, sy: y.start, sw: x.n, sh: y.n };
}

/** Image smoothing for a quality value in 0.25..1. */
function smoothingFor(quality: number): ImageSmoothingQuality {
  if (quality >= 0.75) {
    return 'high';
  }
  if (quality >= 0.4) {
    return 'medium';
  }
  return 'low';
}

/**
 * Destination rectangle of a layer in canvas pixels (the backing store of the monitor). The
 * geometry is computed in project canvas pixels, exactly as `dest_rect` does, and then scaled.
 * Used by the monitor to draw transform handles. Returns an empty rectangle for degenerate input.
 */
export function projectToCanvasRect(
  project: Project,
  layerTransform: Transform,
  srcW: number,
  srcH: number,
  canvasW: number,
  canvasH: number,
): { x: number; y: number; w: number; h: number } {
  const pw = project.canvas.width;
  const ph = project.canvas.height;
  const r = destRect(srcW, srcH, pw, ph, layerTransform);
  if (!r) {
    return { x: 0, y: 0, w: 0, h: 0 };
  }
  const sx = axisScale(canvasW, pw);
  const sy = axisScale(canvasH, ph);
  return { x: r.x * sx, y: r.y * sy, w: r.w * sx, h: r.h * sy };
}

/**
 * Canvas id of the topmost layer whose destination rectangle contains (x, y), in canvas pixels.
 * Rotation is ignored for the hit test. Media layers without a known size are skipped. Solid and
 * title layers count as the full project canvas.
 */
export function hitLayerAt(
  project: Project,
  segment: PlanSegment,
  x: number,
  y: number,
  canvasW: number,
  canvasH: number,
  sizes: (mediaId: string) => { w: number; h: number } | null,
): string | null {
  const topDown = segment.video.slice().reverse();
  for (const layer of topDown) {
    let size: Size | null;
    if (layer.kind === 'video' || layer.kind === 'image') {
      if (layer.media_id === null) {
        continue;
      }
      size = sizes(layer.media_id);
    } else if (layer.kind === 'title' || layer.kind === 'solid') {
      size = { w: project.canvas.width, h: project.canvas.height };
    } else {
      continue;
    }
    if (!size) {
      continue;
    }
    const rect = projectToCanvasRect(project, layer.transform, size.w, size.h, canvasW, canvasH);
    if (!(rect.w > 0 && rect.h > 0)) {
      continue;
    }
    if (x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h) {
      return layer.clip_id;
    }
  }
  return null;
}

/** Creates a scratch canvas of the same document as the target. Null when there is no document. */
function createBuffer(ctx: CanvasRenderingContext2D, w: number, h: number): HTMLCanvasElement | null {
  const doc = ctx.canvas.ownerDocument;
  if (!doc) {
    return null;
  }
  const c = doc.createElement('canvas');
  c.width = w;
  c.height = h;
  return c;
}

/** Copies a composed frame onto the target context, replacing its previous content. */
function blit(ctx: CanvasRenderingContext2D, src: HTMLCanvasElement, cw: number, ch: number): void {
  ctx.save();
  try {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.globalCompositeOperation = 'source-over';
    ctx.filter = 'none';
    ctx.clearRect(0, 0, cw, ch);
    ctx.drawImage(src, 0, 0, cw, ch);
  } catch {
    // The target stays cleared; the next frame will redraw.
  } finally {
    ctx.restore();
  }
}

/**
 * Returns a cached canvas for `key`, drawing it with `draw` on first use. Null when no canvas can
 * be created. The least recently used entry is dropped when the cache is full.
 */
function cachedCanvas(
  ctx: CanvasRenderingContext2D,
  key: string,
  w: number,
  h: number,
  draw: (c: CanvasRenderingContext2D) => void,
): HTMLCanvasElement | null {
  const hit = layerCache.get(key);
  if (hit) {
    layerCache.delete(key);
    layerCache.set(key, hit);
    return hit;
  }
  const canvas = createBuffer(ctx, w, h);
  if (!canvas) {
    return null;
  }
  const cctx = canvas.getContext('2d');
  if (!cctx) {
    return null;
  }
  try {
    draw(cctx);
  } catch {
    return null;
  }
  if (layerCache.size >= LAYER_CACHE_LIMIT) {
    const oldest = layerCache.keys().next();
    if (!oldest.done) {
      layerCache.delete(oldest.value);
    }
  }
  layerCache.set(key, canvas);
  return canvas;
}

/**
 * Project-size canvas for a title or solid layer, or null when the layer has no content. The
 * canvas is drawn at alpha 1; the layer opacity is applied when it is placed.
 */
function generatedCanvas(ctx: CanvasRenderingContext2D, layer: VideoLayer, pw: number, ph: number): HTMLCanvasElement | null {
  if (layer.kind === 'title' && layer.text) {
    const spec = layer.text;
    return cachedCanvas(ctx, `title:${pw}x${ph}:${JSON.stringify(spec)}`, pw, ph, (c) => drawTitle(c, pw, ph, spec, 1));
  }
  if (layer.kind === 'solid' && layer.solid) {
    const color = layer.solid;
    return cachedCanvas(ctx, `solid:${pw}x${ph}:${cssColor(color)}`, pw, ph, (c) => drawSolid(c, pw, ph, color, 1));
  }
  return null;
}

/** Looks up the source of a layer. Titles and solids use their generated canvas. */
function resolveLayer(layer: VideoLayer, sources: SourceFrames, project: Project, ctx: CanvasRenderingContext2D): Resolution {
  if (layer.kind === 'title' || layer.kind === 'solid') {
    const pw = Math.max(1, Math.round(project.canvas.width));
    const ph = Math.max(1, Math.round(project.canvas.height));
    const canvas = generatedCanvas(ctx, layer, pw, ph);
    return canvas ? { kind: 'ready', src: canvas, size: { w: pw, h: ph } } : { kind: 'empty' };
  }
  if (layer.kind !== 'video' && layer.kind !== 'image') {
    return { kind: 'empty' };
  }
  if (layer.media_id === null) {
    return { kind: 'empty' };
  }
  let failed = false;
  try {
    failed = sources.failed(layer.media_id);
  } catch {
    failed = false;
  }
  if (failed) {
    return { kind: 'failed' };
  }
  let src: CanvasImageSource | null = null;
  try {
    src = layer.kind === 'video' ? sources.video(layer.media_id) : sources.image(layer.media_id);
  } catch {
    src = null;
  }
  if (!src) {
    return { kind: 'missing' };
  }
  const size = sourceSize(src);
  if (!size) {
    return { kind: 'missing' };
  }
  return { kind: 'ready', src, size };
}

/**
 * Blurred, darkened backdrop behind fit-mode clips (see `blurred_cover` in reference.rs). The
 * source is drawn into a canvas 1/8 of the target size with a CSS blur of (w / 40) * 0.58 / 8
 * pixels (a Gaussian, about as wide as the export's box blur), then scaled up with a 40 % black
 * overlay.
 */
function drawBlurBackdrop(
  bctx: CanvasRenderingContext2D,
  cw: number,
  ch: number,
  project: Project,
  src: CanvasImageSource,
  size: Size,
): void {
  const rect = projectToCanvasRect(project, BACKDROP_TRANSFORM, size.w, size.h, cw, ch);
  if (!(rect.w > 0 && rect.h > 0)) {
    return;
  }
  const sw = Math.max(1, Math.round(cw / BACKDROP_DOWNSCALE));
  const sh = Math.max(1, Math.round(ch / BACKDROP_DOWNSCALE));
  if (!backdropScratch || backdropScratch.width !== sw || backdropScratch.height !== sh) {
    backdropScratch = createBuffer(bctx, sw, sh);
  }
  const small = backdropScratch;
  const sctx = small ? small.getContext('2d') : null;
  if (small && sctx) {
    const kx = sw / cw;
    const ky = sh / ch;
    const blurPx = ((cw / 40) * 0.58) / BACKDROP_DOWNSCALE;
    // Grow the covering rectangle so the blur fade falls outside the scratch canvas.
    const m = blurPx * 4;
    sctx.save();
    try {
      sctx.setTransform(1, 0, 0, 1, 0, 0);
      sctx.globalAlpha = 1;
      sctx.clearRect(0, 0, sw, sh);
      sctx.filter = `blur(${blurPx}px)`;
      sctx.drawImage(
        src,
        0,
        0,
        size.w,
        size.h,
        rect.x * kx - m,
        rect.y * ky - m,
        rect.w * kx + 2 * m,
        rect.h * ky + 2 * m,
      );
    } catch {
      // A source that cannot be drawn leaves only the background.
    } finally {
      sctx.restore();
    }
    bctx.save();
    try {
      bctx.filter = 'none';
      bctx.drawImage(small, 0, 0, sw, sh, 0, 0, cw, ch);
    } catch {
      // The scratch canvas could not be drawn: keep the background only.
    } finally {
      bctx.restore();
    }
  }
  bctx.save();
  bctx.fillStyle = 'rgba(0, 0, 0, 0.4)';
  bctx.fillRect(0, 0, cw, ch);
  bctx.restore();
}

/** Draws a video or image layer with crop, rotation, flips, opacity and its destination rectangle. */
function drawMedia(
  bctx: CanvasRenderingContext2D,
  cw: number,
  ch: number,
  project: Project,
  layer: VideoLayer,
  src: CanvasImageSource,
  size: Size,
  alpha: number,
): void {
  const t = layer.transform;
  const rect = projectToCanvasRect(project, t, size.w, size.h, cw, ch);
  if (!(rect.w > 0 && rect.h > 0)) {
    return;
  }
  const crop = cropRegion(size.w, size.h, t);
  const rot = quarterTurns(t.rotate_90);
  // The destination rectangle is oriented; the unrotated drawing box is swapped for odd turns.
  const drawW = rot % 2 === 1 ? rect.h : rect.w;
  const drawH = rot % 2 === 1 ? rect.w : rect.h;
  const cx = rect.x + rect.w / 2;
  const cy = rect.y + rect.h / 2;
  bctx.save();
  try {
    bctx.globalAlpha = alpha;
    bctx.translate(cx, cy);
    bctx.rotate((rot * Math.PI) / 2);
    bctx.scale(t.flip_h ? -1 : 1, t.flip_v ? -1 : 1);
    bctx.drawImage(src, crop.sx, crop.sy, crop.sw, crop.sh, -drawW / 2, -drawH / 2, drawW, drawH);
  } catch {
    // A video element that is not ready yet draws nothing for this layer.
  } finally {
    bctx.restore();
  }
}

/**
 * Draws a failed media layer as opaque black over its destination rectangle, with the layer alpha.
 * The source size is unknown, so the project canvas size stands in for it.
 */
function drawFailed(bctx: CanvasRenderingContext2D, cw: number, ch: number, project: Project, layer: VideoLayer, alpha: number): void {
  const rect = projectToCanvasRect(project, layer.transform, project.canvas.width, project.canvas.height, cw, ch);
  if (!(rect.w > 0 && rect.h > 0)) {
    return;
  }
  bctx.save();
  try {
    bctx.globalAlpha = alpha;
    bctx.fillStyle = 'rgb(0, 0, 0)';
    bctx.fillRect(rect.x, rect.y, rect.w, rect.h);
  } finally {
    bctx.restore();
  }
}

/** Splits text into lines that fit `maxW`, wrapping at spaces. Explicit newlines are handled by the caller. */
function wrapParagraph(bctx: CanvasRenderingContext2D, text: string, maxW: number): string[] {
  const out: string[] = [];
  let current = '';
  for (const word of text.split(' ')) {
    const candidate = current === '' ? word : `${current} ${word}`;
    if (current !== '' && bctx.measureText(candidate).width > maxW) {
      out.push(current);
      current = word;
    } else {
      current = candidate;
    }
  }
  out.push(current);
  return out;
}

/**
 * Draws a title in the layout of `drawTitle` in crates/render/src/titles.rs: the text box is 90 %
 * of the canvas width with its left edge at 5 %, lines wrap inside it, the block is centred
 * vertically, and left, right or centre alignment anchors to the box. The background box has
 * padding round(px * 0.3). The outline is a stroke of width 2 * max(1, round(px * 0.05)).
 */
function drawTitle(bctx: CanvasRenderingContext2D, cw: number, ch: number, spec: TextSpec, alpha: number): void {
  const pxSize = (spec.size * ch) / 1080;
  if (!(Number.isFinite(pxSize) && pxSize > 0)) {
    return;
  }
  const name = spec.font.replace(/"/g, '').trim() || DEFAULT_FONT;
  const family = GENERIC_FAMILIES.has(name.toLowerCase()) ? name : `"${name}"`;
  const style = `${spec.italic ? 'italic ' : ''}${spec.bold ? 'bold ' : ''}`;
  const wrapW = Math.max(1, Math.round(cw * 0.9));
  const left = (cw - wrapW) / 2;
  bctx.save();
  try {
    bctx.globalAlpha = alpha;
    bctx.font = `${style}${pxSize}px ${family}`;
    bctx.textBaseline = 'middle';

    const lines: string[] = [];
    for (const para of spec.text.split('\n')) {
      lines.push(...wrapParagraph(bctx, para, wrapW));
    }
    const widths = lines.map((line) => bctx.measureText(line).width);
    const blockW = Math.max(0, ...widths);
    const lineH = pxSize * 1.2;
    const blockH = lines.length * lineH;
    const top = (ch - blockH) / 2;
    const align: CanvasTextAlign = spec.align === 'left' || spec.align === 'right' ? spec.align : 'center';
    const pad = Math.round(pxSize * 0.3);

    if (spec.box_bg) {
      const x0 = align === 'left' ? left : align === 'right' ? left + wrapW - blockW : left + (wrapW - blockW) / 2;
      bctx.fillStyle = cssColor(spec.box_bg);
      bctx.fillRect(x0 - pad, top - pad, blockW + 2 * pad, blockH + 2 * pad);
    }

    bctx.textAlign = align;
    const anchorX = align === 'left' ? left : align === 'right' ? left + wrapW : left + wrapW / 2;
    const ow = Math.max(1, Math.round(pxSize * 0.05));

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i] ?? '';
      const y = top + (i + 0.5) * lineH;
      if (spec.outline) {
        bctx.lineJoin = 'round';
        bctx.lineWidth = 2 * ow;
        bctx.strokeStyle = cssColor(spec.outline);
        bctx.strokeText(line, anchorX, y);
      }
      bctx.fillStyle = cssColor(spec.color);
      bctx.fillText(line, anchorX, y);
    }
  } catch {
    // Text drawing failures leave the layer out for this frame.
  } finally {
    bctx.restore();
  }
}

/** Fills the whole canvas with a solid colour. */
function drawSolid(bctx: CanvasRenderingContext2D, cw: number, ch: number, color: Rgba, alpha: number): void {
  bctx.save();
  bctx.globalAlpha = alpha;
  bctx.fillStyle = cssColor(color);
  bctx.fillRect(0, 0, cw, ch);
  bctx.restore();
}

/** Draws one layer (media, failed media, title or solid). Dips are applied to the whole frame later. */
function drawLayer(
  bctx: CanvasRenderingContext2D,
  cw: number,
  ch: number,
  project: Project,
  layer: VideoLayer,
  res: Resolution,
  t: Tick,
): void {
  const alpha = unitAlpha(layerOpacityAt(layer, t));
  if (res.kind === 'ready') {
    drawMedia(bctx, cw, ch, project, layer, res.src, res.size, alpha);
  } else if (res.kind === 'failed') {
    drawFailed(bctx, cw, ch, project, layer, alpha);
  }
}

/**
 * Paints the whole frame into `bctx`, whose sources are all ready. The strongest dip over all
 * layers is applied once to the composite, as the export does.
 */
function paintFrame(
  bctx: CanvasRenderingContext2D,
  cw: number,
  ch: number,
  project: Project,
  resolved: ResolvedLayer[],
  t: Tick,
  quality: number,
): void {
  bctx.setTransform(1, 0, 0, 1, 0, 0);
  bctx.globalAlpha = 1;
  bctx.globalCompositeOperation = 'source-over';
  bctx.filter = 'none';
  bctx.imageSmoothingEnabled = true;
  bctx.imageSmoothingQuality = smoothingFor(quality);
  bctx.clearRect(0, 0, cw, ch);

  const bg = project.canvas.background;
  bctx.fillStyle = cssColor(bg);
  bctx.fillRect(0, 0, cw, ch);

  const first = resolved[0];
  if (project.canvas.blur_background && first && first.res.kind === 'ready' && first.layer.transform.fit === 'fit') {
    drawBlurBackdrop(bctx, cw, ch, project, first.res.src, first.res.size);
  }

  for (const { layer, res } of resolved) {
    drawLayer(bctx, cw, ch, project, layer, res, t);
  }

  // Strongest dip wins; on ties the first layer keeps its colour, as in pipeline.rs.
  let dip: { color: 'black' | 'white'; amount: number } | null = null;
  for (const { layer } of resolved) {
    const d = dipAmount(layer, t);
    if (!d) continue;
    const amount = unitAlpha(d.amount);
    if (amount > 0 && (dip === null || amount > dip.amount)) {
      dip = { color: d.color, amount };
    }
  }
  if (dip) {
    bctx.save();
    bctx.globalAlpha = dip.amount;
    bctx.fillStyle = dip.color === 'white' ? 'rgb(255, 255, 255)' : 'rgb(0, 0, 0)';
    bctx.fillRect(0, 0, cw, ch);
    bctx.restore();
  }
}

/** Copies the target's current frame into the snapshot, at most once per SNAPSHOT_MS. */
function refreshSnapshot(ctx: CanvasRenderingContext2D, st: FrameState, cw: number, ch: number, now: number): void {
  let snap = st.snapshot;
  if (!snap || snap.width !== cw || snap.height !== ch) {
    snap = createBuffer(ctx, cw, ch);
    if (!snap) return;
    st.snapshot = snap;
  }
  const sctx = snap.getContext('2d');
  if (!sctx) return;
  sctx.save();
  try {
    sctx.setTransform(1, 0, 0, 1, 0, 0);
    sctx.globalAlpha = 1;
    sctx.globalCompositeOperation = 'source-over';
    sctx.clearRect(0, 0, cw, ch);
    sctx.drawImage(ctx.canvas, 0, 0, cw, ch);
  } catch {
    // The snapshot stays stale; the next refresh retries.
  } finally {
    sctx.restore();
  }
  st.snapshotAt = now;
}

/**
 * Draws the program frame for one plan segment at timeline time `t` into `ctx`.
 *
 * `canvasW` and `canvasH` are the backing-store size of `ctx.canvas` (the project canvas scaled
 * by `quality`, 0.25..1). Layers are drawn bottom to top in the order of `segment.video`. When a
 * media source is not ready (missing), the frame already on the context is kept, or the last good
 * copy is shown when the context lost its content, so the monitor does not flicker to blank.
 * Failed media draw as black. A complete frame is painted straight into `ctx`. Never throws.
 */
export function composeFrame(
  ctx: CanvasRenderingContext2D,
  canvasW: number,
  canvasH: number,
  project: Project,
  segment: PlanSegment,
  t: Tick,
  sources: SourceFrames,
  quality: number,
): void {
  try {
    const cw = Math.round(canvasW);
    const ch = Math.round(canvasH);
    if (!(Number.isFinite(cw) && Number.isFinite(ch) && cw > 0 && ch > 0)) {
      return;
    }
    if (!(project.canvas.width > 0 && project.canvas.height > 0)) {
      return;
    }

    const resolved: ResolvedLayer[] = segment.video.map((layer) => ({
      layer,
      res: resolveLayer(layer, sources, project, ctx),
    }));
    const state = frameStates.get(ctx);
    const holdsFrame =
      state !== undefined &&
      state.shown &&
      state.w === cw &&
      state.h === ch &&
      ctx.canvas.width === cw &&
      ctx.canvas.height === ch;

    if (resolved.some((r) => r.res.kind === 'missing')) {
      if (holdsFrame || !state) return;
      if (state.snapshot && state.snapshot.width === cw && state.snapshot.height === ch) {
        blit(ctx, state.snapshot, cw, ch);
        state.shown = true;
        state.w = cw;
        state.h = ch;
      }
      return;
    }

    const st: FrameState = state ?? { w: 0, h: 0, shown: false, snapshot: null, snapshotAt: Number.NEGATIVE_INFINITY };
    frameStates.set(ctx, st);
    st.shown = false;
    ctx.save();
    try {
      paintFrame(ctx, cw, ch, project, resolved, t, quality);
    } finally {
      ctx.restore();
    }
    st.shown = true;
    st.w = cw;
    st.h = ch;

    const now = Date.now();
    if (now - st.snapshotAt >= SNAPSHOT_MS || !st.snapshot || st.snapshot.width !== cw || st.snapshot.height !== ch) {
      refreshSnapshot(ctx, st, cw, ch, now);
    }
  } catch {
    // Drawing must never throw into the render loop.
  }
}
