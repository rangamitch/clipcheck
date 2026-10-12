import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { composeFrame, hitLayerAt, projectToCanvasRect, type SourceFrames } from './compose';
import type { PlanSegment, Project, Rgba, TextSpec, Transform, VideoLayer } from '../types';
import { defaultCanvas, defaultExportSettings, defaultTransform } from '../types';

const TPS = 254_016_000_000;
const sec = (s: number): number => Math.round(s * TPS);

// ---------------------------------------------------------------------------------------------
// Fake 2D context. Every drawing call is logged with a snapshot of the state it ran under.
// Scratch canvases made by compose draw into the same log as the context that created them.
// compose keeps module-level caches (title and solid canvases, the blur scratch), so tests use
// unique titles and colours to avoid picking up a canvas bound to an earlier test's log.
// ---------------------------------------------------------------------------------------------

interface Snap {
  fillStyle: unknown;
  globalAlpha: number;
  filter: string;
  font: string;
  textAlign: string;
  imageSmoothingQuality: string;
}

interface Entry {
  op: string;
  args: unknown[];
  s: Snap;
}

interface Rec {
  log: Entry[];
  created: Array<{ width: number; height: number }>;
}

interface Opts {
  drawImageThrows?: boolean;
  noDocument?: boolean;
}

function newRec(): Rec {
  return { log: [], created: [] };
}

function make2d(rec: Rec, canvas: unknown, opts: Opts): Record<string, unknown> {
  const c: Record<string, unknown> = {
    canvas,
    fillStyle: '#000000',
    strokeStyle: '#000000',
    globalAlpha: 1,
    filter: 'none',
    font: '10px sans-serif',
    textAlign: 'start',
    textBaseline: 'alphabetic',
    lineJoin: 'miter',
    lineWidth: 1,
    globalCompositeOperation: 'source-over',
    imageSmoothingEnabled: true,
    imageSmoothingQuality: 'low',
  };
  const put = (op: string, args: unknown[]): void => {
    rec.log.push({
      op,
      args,
      s: {
        fillStyle: c.fillStyle,
        globalAlpha: c.globalAlpha as number,
        filter: c.filter as string,
        font: c.font as string,
        textAlign: c.textAlign as string,
        imageSmoothingQuality: c.imageSmoothingQuality as string,
      },
    });
  };
  c.save = (): void => put('save', []);
  c.restore = (): void => put('restore', []);
  c.setTransform = (...args: unknown[]): void => put('setTransform', args);
  c.clearRect = (...args: unknown[]): void => put('clearRect', args);
  c.fillRect = (...args: unknown[]): void => put('fillRect', args);
  c.fillText = (...args: unknown[]): void => put('fillText', args);
  c.strokeText = (...args: unknown[]): void => put('strokeText', args);
  c.translate = (...args: unknown[]): void => put('translate', args);
  c.rotate = (...args: unknown[]): void => put('rotate', args);
  c.scale = (...args: unknown[]): void => put('scale', args);
  c.measureText = (t: string): { width: number } => ({ width: t.length * 10 });
  c.drawImage = (...args: unknown[]): void => {
    if (opts.drawImageThrows) throw new Error('drawImage failed');
    put('drawImage', args);
  };
  return c;
}

interface Target {
  ctx: CanvasRenderingContext2D;
  canvas: Record<string, unknown>;
  rec: Rec;
}

/** A target context of the given backing size. Scratch canvases are created through its document. */
function makeTarget(width: number, height: number, opts: Opts = {}): Target {
  const rec = newRec();
  const doc = {
    createElement: (_tag: string): Record<string, unknown> => {
      const buf: Record<string, unknown> = { width: 0, height: 0 };
      buf.getContext = (): Record<string, unknown> => make2d(rec, buf, opts);
      rec.created.push({ width: 0, height: 0 });
      return buf;
    },
  };
  const canvas: Record<string, unknown> = {
    width,
    height,
    ownerDocument: opts.noDocument ? undefined : doc,
  };
  const ctx = make2d(rec, canvas, opts);
  return { ctx: ctx as unknown as CanvasRenderingContext2D, canvas, rec };
}

function ops(rec: Rec, op: string): Entry[] {
  return rec.log.filter((e) => e.op === op);
}

/** Index of the nth log entry with the given op (or Infinity when there is none). */
function indexOfOp(rec: Rec, op: string, nth = 0): number {
  let seen = 0;
  for (let i = 0; i < rec.log.length; i++) {
    if (rec.log[i]?.op === op) {
      if (seen === nth) return i;
      seen++;
    }
  }
  return Number.POSITIVE_INFINITY;
}

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/** Portrait project (1080x1920 by default), black background, no blur unless asked. */
function projectWith(over: Partial<Project['canvas']> = {}): Project {
  const canvas = { ...defaultCanvas(), width: 1080, height: 1920, ...over };
  return {
    schema_version: 1,
    id: 'p',
    name: 'P',
    created_ms: 0,
    modified_ms: 0,
    canvas,
    media: [],
    tracks: [],
    clips: [],
    groups: [],
    markers: [],
    work_area: null,
    modes: { magnetic: true, snap: true },
    export_defaults: defaultExportSettings(),
    ui_state: { zoom_px_per_sec: 80, scroll_x: 0, playhead: 0 },
  };
}

function tf(over: Partial<Transform> = {}): Transform {
  return { ...defaultTransform(), ...over };
}

function layer(over: Partial<VideoLayer> = {}): VideoLayer {
  return {
    clip_id: 'c1',
    kind: 'image',
    media_id: 'm1',
    z: 0,
    src_time: 0,
    speed: 1,
    reverse: false,
    transform: tf(),
    opacity: 1,
    transition: null,
    clip_start: 0,
    text: null,
    solid: null,
    ...over,
  };
}

function segment(video: VideoLayer[]): PlanSegment {
  return { start: 0, end: sec(10), video, audio: [] };
}

const FULL_HD = { width: 1920, height: 1080 };

/** Source frames backed by a map of media id to drawable size, with optional failed ids. */
function sourcesOf(sizes: Record<string, { width: number; height: number }>, failed: string[] = []): SourceFrames {
  const lookup = (id: string): CanvasImageSource | null => {
    const s = sizes[id];
    return s === undefined ? null : ({ width: s.width, height: s.height, id } as unknown as CanvasImageSource);
  };
  return {
    video: lookup,
    image: lookup,
    failed: (id: string) => failed.includes(id),
  };
}

const NO_SOURCES: SourceFrames = { video: () => null, image: () => null, failed: () => false };

let uid = 0;
/** A title text that no other test uses, so the title cache never hands back an old canvas. */
function uniqueText(base: string): string {
  uid++;
  return `${base}-${uid}`;
}

function titleSpec(over: Partial<TextSpec> = {}): TextSpec {
  return {
    text: uniqueText('Hello'),
    font: 'Arial',
    size: 54,
    bold: false,
    italic: false,
    color: { r: 255, g: 255, b: 255, a: 255 },
    outline: null,
    box_bg: null,
    align: 'center',
    ...over,
  };
}

let solidN = 0;
/** A solid colour unique within this file, so the solid cache keys never collide. */
function uniqueSolid(): Rgba {
  solidN = (solidN % 200) + 1;
  return { r: 255, g: 0, b: solidN, a: 255 };
}

/** CSS form of a `uniqueSolid()` colour at full alpha. */
function solidCss(c: Rgba): string {
  return `rgba(${c.r}, ${c.g}, ${c.b}, 1)`;
}

const RED: Rgba = { r: 255, g: 0, b: 0, a: 255 };

// ---------------------------------------------------------------------------------------------
describe('projectToCanvasRect', () => {
  const p = projectWith();

  it('fit: a 1920x1080 source into a 1080x1920 canvas is 1080 wide, 607.5 tall, centred at y 656.25', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fit' }), 1920, 1080, 1080, 1920);
    expect(r.x).toBeCloseTo(0, 9);
    expect(r.y).toBeCloseTo(656.25, 9);
    expect(r.w).toBeCloseTo(1080, 9);
    expect(r.h).toBeCloseTo(607.5, 9);
  });

  it('fit scales the rectangle to the backing store size', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fit' }), 1920, 1080, 540, 960);
    expect(r.y).toBeCloseTo(328.125, 9);
    expect(r.w).toBeCloseTo(540, 9);
    expect(r.h).toBeCloseTo(303.75, 9);
  });

  it('fill: a 1920x1080 source covers the portrait canvas and overflows horizontally', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fill' }), 1920, 1080, 1080, 1920);
    expect(r.h).toBeCloseTo(1920, 6);
    expect(r.w).toBeCloseTo(3413.3333, 3);
    expect(r.x).toBeCloseTo(-1166.6667, 3);
    expect(r.y).toBeCloseTo(0, 9);
  });

  it('fill of a square source covers the canvas exactly on the long side', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fill' }), 1000, 1000, 1080, 1920);
    expect(r.h).toBeCloseTo(1920, 6);
    expect(r.w).toBeCloseTo(1920, 6);
    expect(r.x).toBeCloseTo(-420, 6);
  });

  it('stretch fills the whole canvas exactly', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'stretch' }), 1920, 1080, 1080, 1920)).toEqual({ x: 0, y: 0, w: 1080, h: 1920 });
  });

  it('custom scale 0.5 with pos_x 0.1 matches the Rust dest_rect numbers', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'custom', scale: 0.5, pos_x: 0.1, pos_y: 0 }), 1920, 1080, 1080, 1920);
    expect(r.w).toBeCloseTo(540, 9);
    expect(r.h).toBeCloseTo(303.75, 9);
    expect(r.x).toBeCloseTo(378, 9);
    expect(r.y).toBeCloseTo(808.125, 9);
  });

  it('custom scale 1 with no offset equals fit', () => {
    const fit = projectToCanvasRect(p, tf({ fit: 'fit' }), 1920, 1080, 1080, 1920);
    const custom = projectToCanvasRect(p, tf({ fit: 'custom', scale: 1 }), 1920, 1080, 1080, 1920);
    expect(custom).toEqual(fit);
  });

  it('custom pos_y 0.5 moves the rectangle down by half the canvas height', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'custom', scale: 1, pos_y: 0.5 }), 1920, 1080, 1080, 1920);
    expect(r.y).toBeCloseTo(656.25 + 960, 9);
  });

  it('custom with a zero scale gives an empty rectangle', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'custom', scale: 0 }), 1920, 1080, 1080, 1920)).toEqual({ x: 0, y: 0, w: 0, h: 0 });
  });

  it('custom with a NaN scale gives an empty rectangle', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'custom', scale: NaN }), 1920, 1080, 1080, 1920)).toEqual({ x: 0, y: 0, w: 0, h: 0 });
  });

  it('a quarter turn swaps the oriented size', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'fit', rotate_90: 1 }), 1920, 1080, 1080, 1920)).toEqual({ x: 0, y: 0, w: 1080, h: 1920 });
  });

  it('three quarter turns also swap the size', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fit', rotate_90: 3 }), 1920, 1080, 1080, 1920);
    expect(r.w).toBeCloseTo(1080, 9);
    expect(r.h).toBeCloseTo(1920, 9);
  });

  it('two quarter turns do not swap the size', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'fit', rotate_90: 2 }), 1920, 1080, 1080, 1920).h).toBeCloseTo(607.5, 9);
  });

  it('a negative quarter turn is normalised (-1 behaves like 3)', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'fit', rotate_90: -1 }), 1920, 1080, 1080, 1920)).toEqual(
      projectToCanvasRect(p, tf({ fit: 'fit', rotate_90: 3 }), 1920, 1080, 1080, 1920),
    );
  });

  it('crop of half the source width sizes the fit from the cropped source', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fit', crop: { left: 0.5, top: 0, right: 0, bottom: 0 } }), 1920, 1080, 1080, 1920);
    expect(r.w).toBeCloseTo(1080, 9);
    expect(r.h).toBeCloseTo(1215, 9);
    expect(r.y).toBeCloseTo(352.5, 9);
  });

  it('crop fractions that remove the whole source give an empty rectangle', () => {
    const r = projectToCanvasRect(p, tf({ fit: 'fit', crop: { left: 0.7, top: 0, right: 0.7, bottom: 0 } }), 1920, 1080, 1080, 1920);
    expect(r).toEqual({ x: 0, y: 0, w: 0, h: 0 });
  });

  it('a zero-size source gives an empty rectangle', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'fit' }), 0, 0, 1080, 1920)).toEqual({ x: 0, y: 0, w: 0, h: 0 });
  });

  it('a NaN source size gives an empty rectangle', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'fit' }), NaN, 1080, 1080, 1920)).toEqual({ x: 0, y: 0, w: 0, h: 0 });
  });

  it('a non-finite backing store size falls back to scale 1 (project pixels)', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'stretch' }), 1920, 1080, NaN, NaN)).toEqual({ x: 0, y: 0, w: 1080, h: 1920 });
  });

  it('a backing store a quarter of the project scales every value', () => {
    expect(projectToCanvasRect(p, tf({ fit: 'stretch' }), 1920, 1080, 270, 480)).toEqual({ x: 0, y: 0, w: 270, h: 480 });
  });

  it('never throws for an infinite scale', () => {
    expect(() => projectToCanvasRect(p, tf({ fit: 'custom', scale: Infinity }), 1920, 1080, 1, 1)).not.toThrow();
  });
});

// ---------------------------------------------------------------------------------------------
describe('hitLayerAt', () => {
  const p = projectWith();
  const sizes = (id: string): { w: number; h: number } | null => (id === 'm1' || id === 'm2' ? { w: 1920, h: 1080 } : null);

  /** Full-canvas fill of m1 at the bottom, a small centred custom layer m2 on top. */
  function twoLayers(): PlanSegment {
    return segment([
      layer({ clip_id: 'bottom', media_id: 'm1', transform: tf({ fit: 'fill' }) }),
      layer({ clip_id: 'top', media_id: 'm2', transform: tf({ fit: 'custom', scale: 0.2 }) }),
    ]);
  }

  it('the topmost layer wins where two layers overlap', () => {
    expect(hitLayerAt(p, twoLayers(), 540, 960, 1080, 1920, sizes)).toBe('top');
  });

  it('a point outside the top layer hits the layer below', () => {
    expect(hitLayerAt(p, twoLayers(), 100, 100, 1080, 1920, sizes)).toBe('bottom');
  });

  it('returns null when no layer covers the point', () => {
    const seg = segment([layer({ clip_id: 'small', media_id: 'm2', transform: tf({ fit: 'custom', scale: 0.2 }) })]);
    expect(hitLayerAt(p, seg, 5, 5, 1080, 1920, sizes)).toBeNull();
  });

  it('an empty segment gives null', () => {
    expect(hitLayerAt(p, segment([]), 540, 960, 1080, 1920, sizes)).toBeNull();
  });

  it('a media layer with unknown size is skipped', () => {
    const seg = segment([
      layer({ clip_id: 'bottom', media_id: 'm1', transform: tf({ fit: 'fill' }) }),
      layer({ clip_id: 'unknown', media_id: 'nope', transform: tf({ fit: 'fill' }) }),
    ]);
    expect(hitLayerAt(p, seg, 540, 960, 1080, 1920, sizes)).toBe('bottom');
  });

  it('a media layer without a media id is skipped', () => {
    const seg = segment([
      layer({ clip_id: 'bottom', media_id: 'm1', transform: tf({ fit: 'fill' }) }),
      layer({ clip_id: 'nomedia', media_id: null, transform: tf({ fit: 'fill' }) }),
    ]);
    expect(hitLayerAt(p, seg, 540, 960, 1080, 1920, sizes)).toBe('bottom');
  });

  it('a solid layer on top covers the whole canvas', () => {
    const seg = segment([
      layer({ clip_id: 'bottom', media_id: 'm1', transform: tf({ fit: 'fill' }) }),
      layer({ clip_id: 'solid', kind: 'solid', media_id: null, solid: RED }),
    ]);
    expect(hitLayerAt(p, seg, 3, 1900, 1080, 1920, sizes)).toBe('solid');
  });

  it('a title layer counts as the full project canvas', () => {
    const seg = segment([layer({ clip_id: 'title', kind: 'title', media_id: null, text: titleSpec() })]);
    expect(hitLayerAt(p, seg, 1079, 1919, 1080, 1920, sizes)).toBe('title');
  });

  it('a solid layer is hit anywhere on the canvas, even when media sizes are unknown', () => {
    const seg = segment([layer({ clip_id: 'solid', kind: 'solid', media_id: null, solid: RED })]);
    expect(hitLayerAt(p, seg, 0, 0, 1080, 1920, () => null)).toBe('solid');
  });

  it('the canvas corners are inside a stretched layer (edges are inclusive)', () => {
    const seg = segment([layer({ clip_id: 'stretched', media_id: 'm1', transform: tf({ fit: 'stretch' }) })]);
    expect(hitLayerAt(p, seg, 0, 0, 1080, 1920, sizes)).toBe('stretched');
    expect(hitLayerAt(p, seg, 1080, 1920, 1080, 1920, sizes)).toBe('stretched');
  });

  it('a point just outside the canvas hits nothing', () => {
    const seg = segment([layer({ clip_id: 'stretched', media_id: 'm1', transform: tf({ fit: 'stretch' }) })]);
    expect(hitLayerAt(p, seg, 1081, 10, 1080, 1920, sizes)).toBeNull();
  });

  it('a layer that cannot be laid out (zero scale) is never hit', () => {
    const seg = segment([layer({ clip_id: 'zero', media_id: 'm1', transform: tf({ fit: 'custom', scale: 0 }) })]);
    expect(hitLayerAt(p, seg, 540, 960, 1080, 1920, sizes)).toBeNull();
  });

  it('a quarter turn swaps the hit box sides (axis-aligned oriented rectangle, not a tilted one)', () => {
    const seg = segment([layer({ clip_id: 'rot', media_id: 'm2', transform: tf({ fit: 'custom', scale: 0.2, rotate_90: 1 }) })]);
    // Oriented box: 216 wide, 384 tall, from y 768. The unrotated box would start at y 899.25.
    expect(hitLayerAt(p, seg, 540, 800, 1080, 1920, sizes)).toBe('rot');
  });
});

// ---------------------------------------------------------------------------------------------
describe('composeFrame: safety', () => {
  const p = projectWith();
  const seg = segment([layer({ clip_id: 'a', media_id: 'm1' })]);
  const srcs = sourcesOf({ m1: FULL_HD });

  it('a NaN canvas width draws nothing and does not throw', () => {
    const t = makeTarget(1080, 1920);
    expect(() => composeFrame(t.ctx, NaN, 1920, p, seg, 0, srcs, 1)).not.toThrow();
    expect(t.rec.log).toEqual([]);
  });

  it('an infinite canvas height draws nothing', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, Infinity, p, seg, 0, srcs, 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a zero-sized canvas draws nothing', () => {
    const t = makeTarget(0, 0);
    composeFrame(t.ctx, 0, 0, p, seg, 0, srcs, 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a negative canvas size draws nothing', () => {
    const t = makeTarget(10, 10);
    composeFrame(t.ctx, -10, 10, p, seg, 0, srcs, 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a project with zero canvas width draws nothing', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, projectWith({ width: 0 }), seg, 0, srcs, 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a target without an owning document still paints the frame straight into it', () => {
    const t = makeTarget(1080, 1920, { noDocument: true });
    expect(() => composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1)).not.toThrow();
    expect(ops(t.rec, 'fillRect').length).toBe(1);
  });

  it('a drawImage that throws is absorbed', () => {
    const t = makeTarget(1080, 1920, { drawImageThrows: true });
    expect(() => composeFrame(t.ctx, 1080, 1920, p, seg, 0, srcs, 1)).not.toThrow();
  });

  it('a source lookup that throws is treated as missing and draws nothing', () => {
    const t = makeTarget(1080, 1920);
    const bad: SourceFrames = {
      video: () => {
        throw new Error('decoder gone');
      },
      image: () => {
        throw new Error('decoder gone');
      },
      failed: () => false,
    };
    expect(() => composeFrame(t.ctx, 1080, 1920, p, seg, 0, bad, 1)).not.toThrow();
    expect(t.rec.log).toEqual([]);
  });

  it('a source with a zero size is treated as missing', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, seg, 0, sourcesOf({ m1: { width: 0, height: 0 } }), 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a source with a NaN size is treated as missing', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, seg, 0, sourcesOf({ m1: { width: NaN, height: 1080 } }), 1);
    expect(t.rec.log).toEqual([]);
  });

  it.each([
    [NaN, NaN],
    [0, NaN],
    [Infinity, 1],
    [-Infinity, -Infinity],
  ])('never throws for canvas size (%s, %s)', (w, h) => {
    const t = makeTarget(10, 10);
    expect(() => composeFrame(t.ctx, w, h, p, seg, 0, srcs, 1)).not.toThrow();
  });

  it('never throws for a layer with a NaN transform', () => {
    const t = makeTarget(1080, 1920);
    const bad = segment([layer({ media_id: 'm1', transform: tf({ fit: 'custom', scale: NaN, pos_x: NaN }) })]);
    expect(() => composeFrame(t.ctx, 1080, 1920, p, bad, 0, srcs, 1)).not.toThrow();
  });

  it('never throws for NaN time', () => {
    const t = makeTarget(1080, 1920);
    expect(() => composeFrame(t.ctx, 1080, 1920, p, seg, NaN, srcs, 1)).not.toThrow();
  });
});

// ---------------------------------------------------------------------------------------------
describe('composeFrame: background and media', () => {
  const p = projectWith();

  it('paints the background colour first', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    const first = ops(t.rec, 'fillRect')[0];
    expect(first?.args).toEqual([0, 0, 1080, 1920]);
    expect(first?.s.fillStyle).toBe('rgba(0, 0, 0, 1)');
  });

  it('uses the project background colour', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, projectWith({ background: { r: 10, g: 20, b: 30, a: 255 } }), segment([]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillRect')[0]?.s.fillStyle).toBe('rgba(10, 20, 30, 1)');
  });

  it('clamps out-of-range colour channels into bytes', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, projectWith({ background: { r: 300, g: -5, b: 12.4, a: 255 } }), segment([]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillRect')[0]?.s.fillStyle).toBe('rgba(255, 0, 12, 1)');
  });

  it('draws a ready image layer with drawImage using its source', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1' })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    const draws = ops(t.rec, 'drawImage');
    expect(draws.length).toBeGreaterThan(0);
    expect((draws[0]?.args[0] as { id: string }).id).toBe('m1');
  });

  it('a video layer uses the video frame source, not the image source', () => {
    const t = makeTarget(1080, 1920);
    const srcs: SourceFrames = {
      video: () => ({ width: 1920, height: 1080, id: 'from-video' }) as unknown as CanvasImageSource,
      image: () => null,
      failed: () => false,
    };
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'video', media_id: 'v1' })]), 0, srcs, 1);
    expect((ops(t.rec, 'drawImage')[0]?.args[0] as { id: string }).id).toBe('from-video');
  });

  it('a missing source with no earlier frame draws nothing at all', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'gone' })]), 0, sourcesOf({}), 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a missing source after a good frame leaves the frame on the target untouched', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1' })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    const before = t.rec.log.length;
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'gone' })]), 0, sourcesOf({}), 1);
    expect(t.rec.log.length).toBe(before);
  });

  describe('target that lost its content', () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it('shows the last good copy when the target was resized under it and a source is missing', () => {
      const t = makeTarget(1080, 1920);
      composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1' })]), 0, sourcesOf({ m1: FULL_HD }), 1);
      // The element was resized, which clears its content.
      t.canvas.width = 540;
      const before = t.rec.log.length;
      composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'gone' })]), 0, sourcesOf({}), 1);
      const added = t.rec.log.slice(before);
      const draw = added.find((e) => e.op === 'drawImage');
      expect(draw).toBeDefined();
      expect(draw?.args[0]).toHaveProperty('getContext'); // the saved copy, not a source
      expect(draw?.args[0]).not.toHaveProperty('id');
    });
  });

  it('a layer opacity of 0.5 is applied as the alpha of its draw', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', opacity: 0.5 })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'drawImage')[0]?.s.globalAlpha).toBe(0.5);
  });

  it('an opacity above 1 is clamped to 1', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', opacity: 7 })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'drawImage')[0]?.s.globalAlpha).toBe(1);
  });

  it('a NaN opacity draws fully transparent', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', opacity: NaN })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'drawImage')[0]?.s.globalAlpha).toBe(0);
  });

  it('a horizontal flip mirrors the draw with scale(-1, 1)', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit', flip_h: true }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'scale')[0]?.args).toEqual([-1, 1]);
  });

  it('a vertical flip mirrors the draw with scale(1, -1)', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit', flip_v: true }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'scale')[0]?.args).toEqual([1, -1]);
  });

  it('a quarter turn rotates the draw by pi/2', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit', rotate_90: 1 }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'rotate')[0]?.args[0] as number).toBeCloseTo(Math.PI / 2, 9);
  });

  it('a crop of the left half passes the cropped source rectangle to drawImage', () => {
    const t = makeTarget(1080, 1920);
    const crop = tf({ fit: 'fit', crop: { left: 0.5, top: 0, right: 0, bottom: 0 } });
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', transform: crop })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    const args = ops(t.rec, 'drawImage')[0]?.args ?? [];
    expect(args.slice(1, 5)).toEqual([960, 0, 960, 1080]);
    expect(args[7]).toBeCloseTo(1080, 6);
    expect(args[8]).toBeCloseTo(1215, 6);
  });

  it('a solid layer is generated once in its colour and then placed with its opacity', () => {
    const t = makeTarget(1080, 1920);
    const c = uniqueSolid();
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'solid', media_id: null, solid: c, opacity: 0.25 })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === solidCss(c) && (e.args as number[])[2] === 1080)).toBe(true);
    expect(ops(t.rec, 'drawImage')[0]?.s.globalAlpha).toBe(0.25);
  });

  it('a layer with no source, no text and no solid draws nothing extra', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'solid', media_id: null, solid: null })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillRect').length).toBe(1);
  });

  it('layers are drawn bottom to top in segment order', () => {
    const t = makeTarget(1080, 1920);
    const bottom = uniqueSolid();
    const top = uniqueSolid();
    const seg = segment([
      layer({ clip_id: 'bottom', kind: 'solid', media_id: null, solid: bottom }),
      layer({ clip_id: 'top', kind: 'solid', media_id: null, solid: top }),
    ]);
    composeFrame(t.ctx, 1080, 1920, p, seg, 0, NO_SOURCES, 1);
    // The two layer draws (the snapshot copy also draws the target, so it is excluded).
    const layerDraws = ops(t.rec, 'drawImage').filter((e) => e.args[0] !== t.canvas);
    expect(layerDraws.length).toBe(2);
    // Each generated canvas is made in segment order, so the bottom colour is generated first.
    const generated = ops(t.rec, 'fillRect').filter((e) => e.s.fillStyle === solidCss(bottom) || e.s.fillStyle === solidCss(top));
    expect(generated.map((e) => e.s.fillStyle)).toEqual([solidCss(bottom), solidCss(top)]);
  });

  it('a failed media layer is drawn as opaque black over its destination rectangle', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'bad' })]), 0, sourcesOf({}, ['bad']), 1);
    const black = ops(t.rec, 'fillRect').find((e) => e.s.fillStyle === 'rgb(0, 0, 0)');
    expect(black?.args).toEqual([0, 0, 1080, 1920]);
  });

  it('a failed layer uses its opacity as the alpha of the black fill', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'bad', opacity: 0.5 })]), 0, sourcesOf({}, ['bad']), 1);
    expect(ops(t.rec, 'fillRect').find((e) => e.s.fillStyle === 'rgb(0, 0, 0)')?.s.globalAlpha).toBe(0.5);
  });

  it('a failed layer does not hold the frame back (the frame still paints)', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'bad' })]), 0, sourcesOf({}, ['bad']), 1);
    expect(ops(t.rec, 'clearRect').length).toBeGreaterThanOrEqual(1);
  });

  it('a missing layer holds the frame even when another layer has failed', () => {
    const t = makeTarget(1080, 1920);
    const seg = segment([layer({ clip_id: 'bad', media_id: 'bad' }), layer({ clip_id: 'gone', media_id: 'gone' })]);
    composeFrame(t.ctx, 1080, 1920, p, seg, 0, sourcesOf({}, ['bad']), 1);
    expect(t.rec.log).toEqual([]);
  });

  it('a failing failed-check is read as not failed', () => {
    const t = makeTarget(1080, 1920);
    const srcs: SourceFrames = {
      video: () => null,
      image: () => ({ width: 1920, height: 1080, id: 'ok' }) as unknown as CanvasImageSource,
      failed: () => {
        throw new Error('no status');
      },
    };
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1' })]), 0, srcs, 1);
    expect((ops(t.rec, 'drawImage')[0]?.args[0] as { id: string }).id).toBe('ok');
  });
});

// ---------------------------------------------------------------------------------------------
describe('composeFrame: smoothing, sizes and the snapshot', () => {
  const p = projectWith();

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it.each([
    [1, 'high'],
    [0.75, 'high'],
    [0.5, 'medium'],
    [0.4, 'medium'],
    [0.25, 'low'],
  ])('quality %s selects %s smoothing', (q, expected) => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, q);
    expect(ops(t.rec, 'clearRect')[0]?.s.imageSmoothingQuality).toBe(expected);
  });

  it('a backing store size that is fractional is rounded to whole pixels', () => {
    const t = makeTarget(540, 960);
    composeFrame(t.ctx, 539.6, 959.4, p, segment([]), 0, NO_SOURCES, 0.5);
    expect(ops(t.rec, 'clearRect')[0]?.args).toEqual([0, 0, 540, 959]);
  });

  it('keeps one snapshot canvas for frames of the same size', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    expect(t.rec.created.length).toBe(1);
  });

  it('creates a new snapshot canvas when the size changes', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    composeFrame(t.ctx, 540, 960, p, segment([]), 0, NO_SOURCES, 1);
    expect(t.rec.created.length).toBe(2);
  });

  it('a second frame of the same size still paints the background', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 1, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillRect').length).toBe(2);
  });

  it('copies the frame into the snapshot at most once every 250 ms', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    const copiesAfterFirst = ops(t.rec, 'drawImage').filter((e) => e.args[0] === t.canvas).length;
    expect(copiesAfterFirst).toBe(1);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'drawImage').filter((e) => e.args[0] === t.canvas).length).toBe(1);
    vi.advanceTimersByTime(300);
    composeFrame(t.ctx, 1080, 1920, p, segment([]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'drawImage').filter((e) => e.args[0] === t.canvas).length).toBe(2);
  });
});

// ---------------------------------------------------------------------------------------------
describe('composeFrame: titles', () => {
  const p = projectWith();

  it('draws the title text', () => {
    const t = makeTarget(1080, 1920);
    const spec = titleSpec();
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: spec })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText').map((e) => e.args[0])).toEqual([spec.text]);
  });

  it('two paragraphs produce two lines', () => {
    const t = makeTarget(1080, 1920);
    const spec = titleSpec({ text: `${uniqueText('First')}\n${uniqueText('Second')}` });
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: spec })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText').length).toBe(2);
  });

  it('a long line wraps at word boundaries', () => {
    const t = makeTarget(1080, 1920);
    const spec = titleSpec({ text: `${'word '.repeat(300).trim()}${uniqueText('')}` });
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: spec })]), 0, NO_SOURCES, 1);
    const lines = ops(t.rec, 'fillText').map((e) => e.args[0] as string);
    expect(lines.length).toBeGreaterThan(1);
    expect(lines[0]?.startsWith('word')).toBe(true);
  });

  it('a title with zero size draws no text', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ size: 0 }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText')).toEqual([]);
  });

  it('a title font with a family name is quoted', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ font: '"Segoe UI"' }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText')[0]?.s.font).toContain('"Segoe UI"');
  });

  it('a generic family such as serif is not quoted', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ font: 'serif' }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText')[0]?.s.font).toMatch(/ serif$/);
  });

  it('bold and italic styles are added to the font', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ bold: true, italic: true }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText')[0]?.s.font).toMatch(/^italic bold /);
  });

  it('a right alignment is applied to the text', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ align: 'right' }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText')[0]?.s.textAlign).toBe('right');
  });

  it('an unknown alignment falls back to centre', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ align: 'justify' }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'fillText')[0]?.s.textAlign).toBe('center');
  });

  it('an outline is stroked before the fill', () => {
    const t = makeTarget(1080, 1920);
    const outline: Rgba = { r: 0, g: 0, b: 0, a: 255 };
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ outline }) })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'strokeText').length).toBe(1);
    expect(indexOfOp(t.rec, 'strokeText')).toBeLessThan(indexOfOp(t.rec, 'fillText'));
  });

  it('a background box is filled before the text', () => {
    const t = makeTarget(1080, 1920);
    const box: Rgba = { r: 0, g: 0, b: 0, a: 128 };
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ box_bg: box }) })]), 0, NO_SOURCES, 1);
    expect(indexOfOp(t.rec, 'fillRect', 0)).toBeLessThan(indexOfOp(t.rec, 'fillText'));
  });

  it('a title with a NaN opacity is placed with zero alpha', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec(), opacity: NaN })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'drawImage')[0]?.s.globalAlpha).toBe(0);
  });

  it('a title with empty text draws one empty line without throwing', () => {
    const t = makeTarget(1080, 1920);
    expect(() =>
      composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: titleSpec({ text: '' }) })]), 0, NO_SOURCES, 1),
    ).not.toThrow();
    expect(ops(t.rec, 'fillText').map((e) => e.args[0])).toEqual(['']);
  });
});

// ---------------------------------------------------------------------------------------------
describe('composeFrame: dips and blur backdrop', () => {
  const p = projectWith();

  /** A one-second transition starting at timeline time 0 (the t passed to composeFrame). */
  function dipLayer(kind: 'dip_to_black' | 'dip_to_white' | 'crossfade', duration = sec(1)): VideoLayer {
    return layer({ media_id: 'm1', transition: { kind, duration }, clip_start: 0, opacity: 1 });
  }

  it('a dip to black at a quarter of its length fills black at 0.5 alpha', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([dipLayer('dip_to_black')]), sec(0.25), sourcesOf({ m1: FULL_HD }), 1);
    const dip = ops(t.rec, 'fillRect').find((e) => e.s.fillStyle === 'rgb(0, 0, 0)');
    expect(dip?.s.globalAlpha).toBeCloseTo(0.5, 9);
  });

  it('a dip to white fills white', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([dipLayer('dip_to_white')]), sec(0.25), sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === 'rgb(255, 255, 255)')).toBe(true);
  });

  it('a dip is gone after half its length', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([dipLayer('dip_to_black')]), sec(0.75), sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === 'rgb(0, 0, 0)')).toBe(false);
  });

  it('a crossfade never adds a dip fill', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([dipLayer('crossfade')]), sec(0.25), sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === 'rgb(0, 0, 0)')).toBe(false);
  });

  it('a dip at the very start is a full black fill', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([dipLayer('dip_to_black')]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').find((e) => e.s.fillStyle === 'rgb(0, 0, 0)')?.s.globalAlpha).toBeCloseTo(1, 9);
  });

  it('the strongest of several dips wins', () => {
    const t = makeTarget(1080, 1920);
    // At 0.25 s: a 1 s black dip is at 0.5, a 2 s white dip is at 0.75.
    const seg = segment([dipLayer('dip_to_black', sec(1)), dipLayer('dip_to_white', sec(2))]);
    composeFrame(t.ctx, 1080, 1920, p, seg, sec(0.25), sourcesOf({ m1: FULL_HD }), 1);
    const last = ops(t.rec, 'fillRect').at(-1);
    expect(last?.s.fillStyle).toBe('rgb(255, 255, 255)');
    expect(last?.s.globalAlpha).toBeCloseTo(0.75, 9);
  });

  it('on a tie the first layer keeps its colour', () => {
    const t = makeTarget(1080, 1920);
    const seg = segment([dipLayer('dip_to_black', sec(1)), dipLayer('dip_to_white', sec(1))]);
    composeFrame(t.ctx, 1080, 1920, p, seg, sec(0.25), sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').at(-1)?.s.fillStyle).toBe('rgb(0, 0, 0)');
  });

  it('a blur backdrop is drawn at one eighth size with a blur filter', () => {
    // A size no other test uses, so the blur scratch canvas is created fresh for this test.
    const t = makeTarget(1000, 1600);
    const blurred = projectWith({ width: 1000, height: 1600, blur_background: true });
    composeFrame(t.ctx, 1000, 1600, blurred, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit' }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    // blur = (1000 / 40) * 0.58 / 8 px, about 1.8125 (float rounding may shift the last digits)
    expect(ops(t.rec, 'drawImage').some((e) => /^blur\(1\.81\d*px\)$/.test(e.s.filter))).toBe(true);
  });

  it('the blur backdrop is darkened with a 40 percent black veil', () => {
    const t = makeTarget(1080, 1920);
    const blurred = projectWith({ blur_background: true });
    composeFrame(t.ctx, 1080, 1920, blurred, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit' }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === 'rgba(0, 0, 0, 0.4)')).toBe(true);
  });

  it('no blur backdrop without blur_background', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit' }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === 'rgba(0, 0, 0, 0.4)')).toBe(false);
  });

  it('no blur backdrop when the first layer is not in fit mode', () => {
    const t = makeTarget(1080, 1920);
    const blurred = projectWith({ blur_background: true });
    composeFrame(t.ctx, 1080, 1920, blurred, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fill' }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    expect(ops(t.rec, 'fillRect').some((e) => e.s.fillStyle === 'rgba(0, 0, 0, 0.4)')).toBe(false);
  });

  it('no blur backdrop when the first layer is missing its source', () => {
    const t = makeTarget(1080, 1920);
    const blurred = projectWith({ blur_background: true });
    composeFrame(t.ctx, 1080, 1920, blurred, segment([layer({ media_id: 'gone' })]), 0, sourcesOf({}), 1);
    expect(t.rec.log).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
// Added with the compose fixes: failed media, one dip per frame, titles and solids through the
// transform, title layout constants and the blur backdrop scratch cache.
// ---------------------------------------------------------------------------------------------

describe('composeFrame: failed media and dips', () => {
  const p = projectWith();

  it("a failed layer is drawn over the crop-adjusted destination rectangle", () => {
    const t = makeTarget(1080, 1920);
    const cropped = layer({ media_id: 'bad', transform: tf({ crop: { left: 0.25, top: 0, right: 0, bottom: 0 } }) });
    composeFrame(t.ctx, 1080, 1920, p, segment([cropped]), 0, sourcesOf({}, ['bad']), 1);
    const black = ops(t.rec, 'fillRect').find((e) => e.s.fillStyle === 'rgb(0, 0, 0)');
    expect(black?.args).toEqual([135, 0, 810, 1920]);
  });

  it('a failed layer is not drawn with drawImage', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ media_id: 'bad' })]), 0, sourcesOf({}, ['bad']), 1);
    expect(ops(t.rec, 'drawImage')).toEqual([]);
  });

  it('a failed layer stays black over a ready layer below it when both are on the frame', () => {
    const t = makeTarget(1080, 1920);
    const seg = segment([layer({ clip_id: 'low', media_id: 'm1', z: 0 }), layer({ clip_id: 'top', media_id: 'bad', z: 1 })]);
    composeFrame(t.ctx, 1080, 1920, p, seg, 0, sourcesOf({ m1: FULL_HD }, ['bad']), 1);
    expect(ops(t.rec, "drawImage")).toHaveLength(1);
    const drawAt = t.rec.log.findIndex((e) => e.op === "drawImage");
    const blackAt = t.rec.log.findIndex((e) => e.op === "fillRect" && e.s.fillStyle === "rgb(0, 0, 0)");
    expect(drawAt).toBeGreaterThanOrEqual(0);
    expect(blackAt).toBeGreaterThan(drawAt);
  });

  it('a dip is applied once to the frame even when several layers carry one', () => {
    const t = makeTarget(1080, 1920);
    const dip = (over: Partial<VideoLayer>): VideoLayer =>
      layer({ media_id: 'm1', transition: { kind: 'dip_to_black', duration: sec(1) }, clip_start: 0, ...over });
    const seg = segment([dip({ clip_id: 'a', z: 0 }), dip({ clip_id: 'b', z: 1 }), dip({ clip_id: 'c', z: 2 })]);
    composeFrame(t.ctx, 1080, 1920, p, seg, sec(0.25), sourcesOf({ m1: FULL_HD }), 1);
    const dipFills = ops(t.rec, 'fillRect').filter((e) => e.s.fillStyle === 'rgb(0, 0, 0)' && e.args[2] === 1080 && e.args[3] === 1920);
    // One full-frame black fill for the dip itself (the failed-media fill is not used here).
    expect(dipFills).toHaveLength(1);
    expect(dipFills[0]?.s.globalAlpha).toBeCloseTo(0.5, 9);
  });

  it('a solid with a horizontal flip mirrors its draw', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(
      t.ctx,
      1080,
      1920,
      p,
      segment([layer({ kind: 'solid', media_id: null, solid: uniqueSolid(), transform: tf({ flip_h: true }) })]),
      0,
      NO_SOURCES,
      1,
    );
    expect(ops(t.rec, 'scale')[0]?.args).toEqual([-1, 1]);
  });

  it('a title with a quarter turn is rotated like media', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(
      t.ctx,
      1080,
      1920,
      p,
      segment([layer({ kind: 'title', media_id: null, text: titleSpec({ text: uniqueText('Turn') }), transform: tf({ rotate_90: 1 }) })]),
      0,
      NO_SOURCES,
      1,
    );
    expect(ops(t.rec, 'rotate')[0]?.args[0]).toBeCloseTo(Math.PI / 2, 9);
  });

  it('a title is generated at the project size and drawn over the whole frame', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(
      t.ctx,
      1080,
      1920,
      p,
      segment([layer({ kind: 'title', media_id: null, text: titleSpec() })]),
      0,
      NO_SOURCES,
      1,
    );
    const draw = ops(t.rec, 'drawImage')[0];
    expect(draw?.args[7]).toBe(1080);
    expect(draw?.args[8]).toBe(1920);
  });

  it('a title that is not ready yet (no text) draws nothing', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(t.ctx, 1080, 1920, p, segment([layer({ kind: 'title', media_id: null, text: null })]), 0, NO_SOURCES, 1);
    expect(ops(t.rec, 'drawImage')).toEqual([]);
  });
});

describe('composeFrame: title layout constants', () => {
  // Project 1080 x 1920: wrap width is 90 % (972 px) and the block is centred, so each side has a
  // 5 % margin (54 px). The font size scales with the project height (size * 1920 / 1080).
  const p = projectWith();

  function anchorFor(align: TextSpec['align']): number {
    const t = makeTarget(1080, 1920);
    composeFrame(
      t.ctx,
      1080,
      1920,
      p,
      segment([layer({ kind: 'title', media_id: null, text: titleSpec({ align, text: uniqueText('Anchor') }) })]),
      0,
      NO_SOURCES,
      1,
    );
    return ops(t.rec, 'fillText')[0]?.args[1] as number;
  }

  it('a centred title is anchored at the middle of the 90 % wrap width', () => {
    expect(anchorFor('center')).toBeCloseTo(540, 6);
  });

  it('a left-aligned title starts at the 5 % margin', () => {
    expect(anchorFor('left')).toBeCloseTo(54, 6);
  });

  it('a right-aligned title ends at the 95 % mark', () => {
    expect(anchorFor('right')).toBeCloseTo(1026, 6);
  });

  it('the title font size scales with the project height', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(
      t.ctx,
      1080,
      1920,
      p,
      segment([layer({ kind: 'title', media_id: null, text: titleSpec({ size: 54, text: uniqueText('Size') }) })]),
      0,
      NO_SOURCES,
      1,
    );
    // 54 * 1920 / 1080 = 96 px.
    expect(ops(t.rec, 'fillText')[0]?.s.font).toContain('96px');
  });

  it('a title line is drawn at its vertical middle', () => {
    const t = makeTarget(1080, 1920);
    composeFrame(
      t.ctx,
      1080,
      1920,
      p,
      segment([layer({ kind: 'title', media_id: null, text: titleSpec({ text: uniqueText('Mid') }) })]),
      0,
      NO_SOURCES,
      1,
    );
    // One 115.2 px line block centred on the 1920 px frame: its middle is 960.
    const y = ops(t.rec, 'fillText')[0]?.args[2] as number;
    expect(y).toBeCloseTo(960, 4);
  });
});

describe('composeFrame: blur backdrop scratch cache', () => {
  function blurScratchWidthEntries(rec: Rec, sw: number): unknown[] {
    return ops(rec, 'drawImage')
      .map((e) => e.args[0])
      .filter((a) => (a as { width?: number }).width === sw);
  }

  it('frames of the same size reuse one blur scratch canvas', () => {
    const t = makeTarget(1200, 1600);
    const blurred = projectWith({ width: 1200, height: 1600, blur_background: true });
    const seg = segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit' }) })]);
    const srcs = sourcesOf({ m1: FULL_HD });
    composeFrame(t.ctx, 1200, 1600, blurred, seg, 0, srcs, 1);
    const first = blurScratchWidthEntries(t.rec, 150)[0];
    t.rec.log.length = 0;
    composeFrame(t.ctx, 1200, 1600, blurred, seg, sec(0.5), srcs, 1);
    const second = blurScratchWidthEntries(t.rec, 150)[0];
    expect(first).toBeDefined();
    expect(second).toBe(first);
  });

  it('a frame at a new size gets a new blur scratch canvas', () => {
    const t = makeTarget(1200, 1600);
    const seg = segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit' }) })]);
    const srcs = sourcesOf({ m1: FULL_HD });
    composeFrame(t.ctx, 1200, 1600, projectWith({ width: 1200, height: 1600, blur_background: true }), seg, 0, srcs, 1);
    const first = blurScratchWidthEntries(t.rec, 150)[0];
    const t2 = makeTarget(1200, 1000);
    composeFrame(t2.ctx, 1200, 1000, projectWith({ width: 1200, height: 1000, blur_background: true }), seg, 0, srcs, 1);
    const resized = ops(t2.rec, 'drawImage')
      .map((e) => e.args[0])
      .find((a) => (a as { width?: number; height?: number }).width === 150 && (a as { height?: number }).height === 125);
    expect(resized).toBeDefined();
    expect(resized).not.toBe(first);
  });

  it('the blur backdrop covers the whole target with a veil on top', () => {
    const t = makeTarget(1080, 1920);
    const blurred = projectWith({ blur_background: true });
    composeFrame(t.ctx, 1080, 1920, blurred, segment([layer({ media_id: 'm1', transform: tf({ fit: 'fit' }) })]), 0, sourcesOf({ m1: FULL_HD }), 1);
    const veil = ops(t.rec, 'fillRect').find((e) => e.s.fillStyle === 'rgba(0, 0, 0, 0.4)');
    expect(veil?.args).toEqual([0, 0, 1080, 1920]);
  });
});
