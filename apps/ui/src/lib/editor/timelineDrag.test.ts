import { describe, expect, it } from 'vitest';
import {
  DRAG_THRESHOLD_PX,
  beginEnvelopeDrag,
  beginDrag,
  hitEnvelopePoint,
  beginFadeDrag,
  beginPan,
  beginPanTyped,
  beginEnvelopeHitDrag,
  finishDrag,
  movingClips,
  panScroll,
  shiftDragOrigin,
  updateDrag,
  type DragContext,
  type DragState,
  type EnvelopeDrag,
  type FadeDrag,
  type MoveDrag,
  type PanDrag,
  type PointerLike,
  type RollDrag,
  type TrimDrag,
} from './timelineDrag';
import { clipRect, envelopeY, hitEnvelopePoint as layoutHitEnvelopePoint, tickToX, trackLayout, type HitResult, type View } from './layout';
import type { Clip, EnvPoint, MediaRef, Project, Track, TrackKind } from './types';
import { defaultCanvas, defaultClipAudio, defaultExportSettings, defaultTransform } from './types';

const TPS = 254_016_000_000;
const sec = (s: number): number => Math.round(s * TPS);
const FRAME = TPS / 30;

/** 100 px per second, no scroll: 1 px = 0.01 s. */
const VIEW: View = { pxPerSec: 100, scrollX: 0, width: 1000, height: 400 };

/** Pointer at (x, y) with no modifiers, optionally overridden. */
function pt(x: number, y: number, over: Partial<PointerLike> = {}): PointerLike {
  return { x, y, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...over };
}

function track(id: string, kind: TrackKind, over: Partial<Track> = {}): Track {
  return {
    id,
    kind,
    name: id,
    muted: false,
    solo: false,
    locked: false,
    hidden: false,
    volume_db: 0,
    targeted: false,
    height: 56,
    ...over,
  };
}

function clip(id: string, trackId: string, start: number, dur: number, over: Partial<Clip> = {}): Clip {
  return {
    id,
    track_id: trackId,
    kind: 'video',
    media_id: 'm1',
    name: id,
    start,
    dur,
    src_in: 0,
    speed: 1,
    reverse: false,
    enabled: true,
    color: null,
    transform: defaultTransform(),
    opacity: 1,
    audio: defaultClipAudio(),
    link_id: null,
    group_id: null,
    transition_in: null,
    text: null,
    solid: null,
    ...over,
  };
}

function project(tracks: Track[], clips: Clip[], over: Partial<Project> = {}): Project {
  return {
    schema_version: 1,
    id: 'p1',
    name: 'Test',
    created_ms: 0,
    modified_ms: 0,
    canvas: defaultCanvas(),
    media: [],
    tracks,
    clips,
    groups: [],
    markers: [],
    work_area: null,
    modes: { magnetic: true, snap: true },
    export_defaults: defaultExportSettings(),
    ui_state: { zoom_px_per_sec: 100, scroll_x: 0, playhead: 0 },
    ...over,
  };
}

function ctxFor(p: Project, over: Partial<DragContext> = {}): DragContext {
  return { project: p, rows: trackLayout(p, 0), view: VIEW, snapPoints: [], snapEnabled: true, ...over };
}

/**
 * Main fixture. Rows (px, top): V2 at 28, V1 at 86, A1 at 144, each 56 px tall.
 * A: V1 0..2 s (linked to AA). B: V1 3..5 s. C: V2 0..1 s. AA: A1 0..2 s, link L1.
 */
function mainProject(opts: { lockV1?: boolean; lockA1?: boolean } = {}): Project {
  const tracks = [
    track('v1', 'video', { locked: opts.lockV1 ?? false }),
    track('v2', 'video'),
    track('a1', 'audio', { locked: opts.lockA1 ?? false }),
  ];
  return project(tracks, [
    clip('A', 'v1', 0, sec(2), { link_id: 'L1' }),
    clip('B', 'v1', sec(3), sec(2)),
    clip('C', 'v2', 0, sec(1)),
    clip('AA', 'a1', 0, sec(2), { kind: 'audio', link_id: 'L1' }),
  ]);
}

/** V1 with two touching clips: A 0..2 s and R 2..4 s. */
function rollProject(): Project {
  return project([track('v1', 'video')], [clip('A', 'v1', 0, sec(2)), clip('R', 'v1', sec(2), sec(2))]);
}

/** B (3..5 s, group G) with BG (6..7 s, group G) on the same track. */
function groupProject(): Project {
  return project([track('v1', 'video')], [
    clip('B', 'v1', sec(3), sec(2), { group_id: 'G' }),
    clip('BG', 'v1', sec(6), sec(1), { group_id: 'G' }),
  ]);
}

/** B (3..5 s, link LB) and its audio partner BP (1..2 s, link LB). */
function movePartnerProject(): Project {
  return project([track('v1', 'video'), track('a1', 'audio')], [
    clip('B', 'v1', sec(3), sec(2), { link_id: 'LB' }),
    clip('BP', 'a1', sec(1), sec(1), { kind: 'audio', link_id: 'LB' }),
  ]);
}

/** A 0..2 s with an envelope of three points; A has a fade-out of 1.5 s and a fade-in of 0. */
function envProject(): Project {
  const env: EnvPoint[] = [
    { t: 0, db: 0 },
    { t: sec(1), db: -6 },
    { t: sec(1.8), db: -12 },
  ];
  return project([track('v1', 'video')], [
    clip('A', 'v1', 0, sec(2), { audio: { ...defaultClipAudio(), envelope: env, fade_out: sec(1.5) } }),
  ]);
}

/** A 0..1 s on V1, B 3..5 s on V1, C 0..1 s on V2, AA 0..2 s on A1 (linked to nothing). */
function marqueeProject(lockA1 = false): Project {
  return project(
    [track('v1', 'video'), track('v2', 'video'), track('a1', 'audio', { locked: lockA1 })],
    [
      clip('A', 'v1', 0, sec(2)),
      clip('B', 'v1', sec(3), sec(2)),
      clip('C', 'v2', 0, sec(1)),
      clip('AA', 'a1', 0, sec(2), { kind: 'audio' }),
    ],
  );
}

// ---------------------------------------------------------------------------------------------
describe('beginDrag', () => {
  const p = mainProject();

  it('a ruler press starts a playhead drag', () => {
    const hit: HitResult = { kind: 'ruler', tick: 0 };
    expect(beginDrag(hit, pt(50, 10), p, [])).toEqual({ kind: 'playhead' });
  });

  it('a press on an unselected clip starts a move of that clip', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), p, []) as MoveDrag;
    expect(s.kind).toBe('move');
    expect(s.grabClipId).toBe('A');
    expect(s.clipIds).toEqual(['A']);
    expect(s.delta).toBe(0);
    expect(s.trackOffset).toBe(0);
    expect(s.originX).toBe(50);
    expect(s.originY).toBe(100);
  });

  it('a press on a selected clip moves the whole selection', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), p, ['A', 'B']) as MoveDrag;
    expect(s.clipIds).toEqual(['A', 'B']);
  });

  it('a press on an unselected clip ignores the current selection', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), p, ['B']) as MoveDrag;
    expect(s.clipIds).toEqual(['A']);
  });

  it('duplicate ids in the selection are removed from the move list', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), p, ['A', 'A', 'B']) as MoveDrag;
    expect(s.clipIds).toEqual(['A', 'B']);
  });

  it('a selected clip on a locked track is left out of the move list', () => {
    const q = mainProject({ lockA1: true });
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), q, ['A', 'AA']) as MoveDrag;
    expect(s.clipIds).toEqual(['A']);
  });

  it('a press on a clip on a locked track starts nothing', () => {
    const q = mainProject({ lockV1: true });
    expect(beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), q, [])).toBeNull();
  });

  it('a clip hit without a clip id starts nothing', () => {
    expect(beginDrag({ kind: 'clip' }, pt(50, 100), p, [])).toBeNull();
  });

  it('a clip hit for an unknown clip starts nothing', () => {
    expect(beginDrag({ kind: 'clip', clipId: 'nope' }, pt(50, 100), p, [])).toBeNull();
  });

  it('Alt on a clip body still starts a move', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100, { altKey: true }), p, []);
    expect(s?.kind).toBe('move');
  });

  it('a start edge press starts a trim-start with the clip start as origin', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    expect(s.kind).toBe('trim-start');
    expect(s.originTime).toBe(0);
    expect(s.newTime).toBe(0);
    expect(s.ripple).toBe(false);
  });

  it('an end edge press starts a trim-end with the clip end as origin', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    expect(s.kind).toBe('trim-end');
    expect(s.originTime).toBe(sec(2));
  });

  it('Shift on an edge press starts a ripple trim', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { shiftKey: true }), p, []) as TrimDrag;
    expect(s.ripple).toBe(true);
  });

  it('an edge press on a locked track starts nothing', () => {
    const q = mainProject({ lockV1: true });
    expect(beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), q, [])).toBeNull();
  });

  it('an edge press without a clip id starts nothing', () => {
    expect(beginDrag({ kind: 'edge-start' }, pt(0, 100), p, [])).toBeNull();
  });

  it('Alt on an edge with a touching neighbour starts a roll on the shared edit point', () => {
    const q = rollProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), q, []);
    expect(s).toMatchObject({ kind: 'roll', leftId: 'A', rightId: 'R', originCut: sec(2), newCut: sec(2) });
  });

  it('Alt on the start edge of a clip rolls with the clip ending at that point', () => {
    const q = rollProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'R' }, pt(200, 100, { altKey: true }), q, []);
    expect(s).toMatchObject({ kind: 'roll', leftId: 'A', rightId: 'R', originCut: sec(2) });
  });

  it('Alt on an edge with no touching neighbour falls back to a trim', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    expect(s?.kind).toBe('trim-end');
  });

  it('a press on a gap starts a marquee with the current selection as base', () => {
    const sel = ['B'];
    const s = beginDrag({ kind: 'gap', trackId: 'v1' }, pt(10, 40), p, sel);
    expect(s).toMatchObject({ kind: 'marquee', originX: 10, originY: 40, x: 10, y: 40, base: ['B'], additive: false });
  });

  it('a press on empty space starts a marquee', () => {
    expect(beginDrag({ kind: 'empty' }, pt(10, 40), p, [])?.kind).toBe('marquee');
  });

  it('a press on a track row starts a marquee', () => {
    expect(beginDrag({ kind: 'track', trackId: 'v1' }, pt(10, 40), p, [])?.kind).toBe('marquee');
  });

  it('Shift makes the marquee additive', () => {
    const s = beginDrag({ kind: 'empty' }, pt(1, 1, { shiftKey: true }), p, []);
    expect(s).toMatchObject({ additive: true });
  });

  it('Ctrl makes the marquee additive', () => {
    const s = beginDrag({ kind: 'empty' }, pt(1, 1, { ctrlKey: true }), p, []);
    expect(s).toMatchObject({ additive: true });
  });

  it('Meta makes the marquee additive', () => {
    const s = beginDrag({ kind: 'empty' }, pt(1, 1, { metaKey: true }), p, []);
    expect(s).toMatchObject({ additive: true });
  });

  it('the marquee base is a copy of the selection', () => {
    const sel = ['B'];
    const s = beginDrag({ kind: 'empty' }, pt(1, 1), p, sel) as { base: string[] };
    expect(s.base).toEqual(['B']);
    expect(s.base).not.toBe(sel);
  });
});

// ---------------------------------------------------------------------------------------------
describe('envelope, fade, pan and origin helpers', () => {
  const ep = envProject();

  it('an envelope drag starts from the chosen point', () => {
    const s = beginEnvelopeDrag(ep, 'A', 1, pt(40, 40)) as EnvelopeDrag;
    expect(s.kind).toBe('envelope-point');
    expect(s.origin).toEqual({ t: sec(1), db: -6 });
    expect(s.points).toEqual(s.base);
    expect(s.index).toBe(1);
  });

  it('an envelope drag on a missing point starts nothing', () => {
    expect(beginEnvelopeDrag(ep, 'A', 9, pt(0, 0))).toBeNull();
  });

  it('an envelope drag on a missing clip starts nothing', () => {
    expect(beginEnvelopeDrag(ep, 'nope', 0, pt(0, 0))).toBeNull();
  });

  it('a fade-in drag starts from the clip fade-in value', () => {
    const s = beginFadeDrag(ep, 'A', 'in', pt(0, 0)) as FadeDrag;
    expect(s).toMatchObject({ kind: 'fade-handle', edge: 'in', origin: 0, value: 0 });
  });

  it('a fade-out drag starts from the clip fade-out value', () => {
    const s = beginFadeDrag(ep, 'A', 'out', pt(0, 0)) as FadeDrag;
    expect(s).toMatchObject({ edge: 'out', origin: sec(1.5), value: sec(1.5) });
  });

  it('a fade drag on a missing clip starts nothing', () => {
    expect(beginFadeDrag(ep, 'nope', 'in', pt(0, 0))).toBeNull();
  });

  it('beginPan records the pointer and the scroll offsets', () => {
    expect(beginPan(pt(50, 60), 100, 200)).toEqual({
      kind: 'pan',
      originX: 50,
      originY: 60,
      scrollX0: 100,
      scrollY0: 200,
    });
  });

  it('panScroll moves the scroll opposite to the pointer', () => {
    const s = beginPanTyped(50, 60, 100, 200);
    expect(panScroll(s, pt(30, 60))).toEqual({ scrollX: 120, scrollY: 200 });
  });

  it('panScroll follows vertical pointer motion too', () => {
    const s = beginPanTyped(50, 60, 100, 200);
    expect(panScroll(s, pt(50, 90))).toEqual({ scrollX: 100, scrollY: 170 });
  });

  it('panScroll never returns a negative horizontal scroll', () => {
    const s = beginPanTyped(0, 0, 10, 0);
    expect(panScroll(s, pt(100, 0)).scrollX).toBe(0);
  });

  it('panScroll never returns a negative vertical scroll', () => {
    const s = beginPanTyped(0, 0, 0, 10);
    expect(panScroll(s, pt(0, 100)).scrollY).toBe(0);
  });

  it('shiftDragOrigin moves the origin by minus dx for a move drag', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(50, 100), mainProject(), []) as MoveDrag;
    expect(shiftDragOrigin(s, 20)).toMatchObject({ originX: 30 });
  });

  it('shiftDragOrigin moves the origin for a trim drag', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), mainProject(), []);
    expect(shiftDragOrigin(s as DragState, -10)).toMatchObject({ originX: 210 });
  });

  it('shiftDragOrigin leaves playhead drags unchanged', () => {
    const s: DragState = { kind: 'playhead' };
    expect(shiftDragOrigin(s, 40)).toBe(s);
  });

  it('shiftDragOrigin leaves pan drags unchanged', () => {
    const s = beginPanTyped(5, 5, 0, 0);
    expect(shiftDragOrigin(s, 40)).toBe(s);
  });
});

// ---------------------------------------------------------------------------------------------
describe('movingClips', () => {
  it('returns the clips plus their link partners', () => {
    expect(movingClips(mainProject(), ['A']).map((c) => c.id)).toEqual(['A', 'AA']);
  });

  it('returns the clips plus their group partners', () => {
    expect(movingClips(groupProject(), ['B']).map((c) => c.id)).toEqual(['B', 'BG']);
  });

  it('does not repeat a partner that is also selected', () => {
    expect(movingClips(mainProject(), ['A', 'AA']).map((c) => c.id)).toEqual(['A', 'AA']);
  });

  it('a clip without partners moves alone', () => {
    expect(movingClips(mainProject(), ['C']).map((c) => c.id)).toEqual(['C']);
  });

  it('unknown ids give an empty list', () => {
    expect(movingClips(mainProject(), ['nope'])).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('move drag updates', () => {
  it('dragging right by 50 px moves the clip by half a second', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect((u.state as MoveDrag).delta).toBe(sec(0.5));
    expect((u.state as MoveDrag).trackOffset).toBe(0);
  });

  it('the ghost of the grabbed clip is drawn at its new x position', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    const a = u.ghosts.find((g) => g.clipId === 'A');
    expect(a?.rect).toEqual({ x: 50, y: 86, w: 200, h: 56 });
  });

  it('link partners get a ghost too', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(u.ghosts.map((g) => g.clipId).sort()).toEqual(['A', 'AA']);
    expect(u.ghosts.find((g) => g.clipId === 'AA')?.rect).toEqual({ x: 50, y: 144, w: 200, h: 56 });
  });

  it('group partners get a ghost too', () => {
    const p = groupProject();
    const s = beginDrag({ kind: 'clip', clipId: 'B' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(u.ghosts.find((g) => g.clipId === 'BG')?.rect).toEqual({ x: 650, y: 86, w: 100, h: 56 });
  });

  it('moving up to the track above sets a track offset of minus one for a video clip', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 40), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 100), ctxFor(p));
    expect((u.state as MoveDrag).trackOffset).toBe(-1);
  });

  it('moving a video clip into the track below sets a track offset of plus one', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 40), ctxFor(p));
    expect((u.state as MoveDrag).trackOffset).toBe(1);
    expect(u.ghosts.find((g) => g.clipId === 'A')?.rect.y).toBe(28);
  });

  it('a video clip dragged onto the audio row keeps its track', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 150), ctxFor(p));
    expect((u.state as MoveDrag).trackOffset).toBe(0);
  });

  it('dragging left past the start is clamped so nothing moves before tick 0', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(200, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 100), ctxFor(p));
    expect((u.state as MoveDrag).delta).toBe(0);
  });

  it('dragging a clip left moves it left by the pointer distance', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'B' }, pt(300, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect((u.state as MoveDrag).delta).toBe(-sec(2.5));
  });

  it('a partner that would start before zero limits a leftward move', () => {
    const p = movePartnerProject();
    const s = beginDrag({ kind: 'clip', clipId: 'B' }, pt(300, 100), p, []) as MoveDrag;
    // B alone could move 3 s left, but its partner BP starts at 1 s.
    const u = updateDrag(s, pt(0, 100), ctxFor(p));
    expect((u.state as MoveDrag).delta).toBe(-sec(1));
  });

  it('a snap to a point within 8 px moves the clip edge onto the point', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(95, 100), ctxFor(p, { snapPoints: [{ time: sec(3) }] }));
    // The end edge (2.95 s) is 0.05 s = 5 px from 3 s, so it snaps there.
    expect((u.state as MoveDrag).delta).toBe(sec(1));
    expect(u.snapTick).toBe(sec(3));
  });

  it('no snap happens when the nearest point is outside 8 px', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(95, 100), ctxFor(p, { snapPoints: [{ time: sec(3.2) }] }));
    expect((u.state as MoveDrag).delta).toBe(245_548_800_000);
    expect(u.snapTick).toBeNull();
  });

  it('holding Alt disables snapping for a move', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(95, 100, { altKey: true }), ctxFor(p, { snapPoints: [{ time: sec(3) }] }));
    expect((u.state as MoveDrag).delta).toBe(245_548_800_000);
    expect(u.snapTick).toBeNull();
  });

  it('snapping is off when the project snap mode is off', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(95, 100), ctxFor(p, { snapPoints: [{ time: sec(3) }], snapEnabled: false }));
    expect(u.snapTick).toBeNull();
    expect((u.state as MoveDrag).delta).toBe(245_548_800_000);
  });

  it('the state keeps the grabbed clip and selection after an update', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, ['A', 'B']) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(u.state).toMatchObject({ kind: 'move', grabClipId: 'A', clipIds: ['A', 'B'] });
  });

  it('a move whose grabbed clip no longer exists changes nothing', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const gone = project([track('v1', 'video')], [clip('B', 'v1', 0, sec(1))]);
    const u = updateDrag(s, pt(50, 100), ctxFor(gone));
    expect(u.ghosts).toEqual([]);
    expect(u.state).toBe(s);
  });
});

// ---------------------------------------------------------------------------------------------
describe('move drag finish', () => {
  it('no movement gives no commands', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    expect(finishDrag(s, { project: p })).toEqual([]);
  });

  it('a time move sends one move_clips with the new start and no track change', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'move_clips', moves: [
        { clip_id: 'A', new_start: sec(0.5), new_track: null },
        { clip_id: 'AA', new_start: sec(0.5), new_track: null },
      ] },
    ]);
  });

  it('a track change sends the target track for the clip', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 40), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 100), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'move_clips', moves: [{ clip_id: 'C', new_start: 0, new_track: 'v1' }] },
    ]);
  });

  it('link partners are listed in the moves with the same delta', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    const [cmd] = finishDrag(u.state, { project: p });
    expect(cmd).toMatchObject({ type: 'move_clips' });
    expect((cmd as { moves: { clip_id: string }[] }).moves.map((m) => m.clip_id)).toEqual(['A', 'AA']);
  });

  it('Alt on a clip body finishes as a duplicate at the dragged offset', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 40, { altKey: true }), p, []) as MoveDrag;
    expect(s.duplicate).toBe(true);
    const u = updateDrag(s, pt(50, 40), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'duplicate_clips', ids: ['C'], offset: sec(0.5) },
    ]);
  });

  it('every selected clip moves by the same delta', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, ['A', 'B']) as MoveDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      {
        type: 'move_clips',
        moves: [
          { clip_id: 'A', new_start: sec(0.5), new_track: null },
          { clip_id: 'B', new_start: sec(3.5), new_track: null },
          { clip_id: 'AA', new_start: sec(0.5), new_track: null },
        ],
      },
    ]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('trim drag', () => {
  const p = mainProject();

  it('trim-start dragged 50 px right starts the clip half a second later', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(0.5));
  });

  it('the trim-start ghost is shorter and starts at the new edge', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(u.ghosts.find((g) => g.clipId === 'A')?.rect).toEqual({ x: 50, y: 86, w: 150, h: 56 });
  });

  it('a trim ghost is also drawn for a link partner at the same edge', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(u.ghosts.map((g) => g.clipId).sort()).toEqual(['A', 'AA']);
  });

  it('trim-start cannot pass the clip end minus one frame', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(300, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(2) - FRAME);
  });

  it('trim-start cannot move before tick zero', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(-500, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(0);
  });

  it('trim-end dragged 50 px right extends the clip by half a second', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(250, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(2.5));
  });

  it('trim-end cannot pass the clip start plus one frame', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(-300, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(FRAME);
  });

  it('a ripple trim-end (Shift) cannot pass the project length limit', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(1_000_000, 100, { shiftKey: true }), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(4 * 3600 * TPS);
  });

  it('a plain trim-end stops at the start of the next clip on its track', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(1_000_000, 100), ctxFor(p));
    // B starts at 3 s on the same track, so A can extend to 3 s but no further.
    expect((u.state as TrimDrag).newTime).toBe(sec(3));
  });

  it('Shift during the update marks the trim as a ripple', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(250, 100, { shiftKey: true }), ctxFor(p));
    expect((u.state as TrimDrag).ripple).toBe(true);
  });

  it('without Shift the trim is not a ripple', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(250, 100), ctxFor(p));
    expect((u.state as TrimDrag).ripple).toBe(false);
  });

  it('finish sends trim_edge for the start edge', () => {
    const s = beginDrag({ kind: 'edge-start', clipId: 'A' }, pt(0, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(50, 100), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'trim_edge', clip_id: 'A', edge: 'start', new_time: sec(0.5), ripple: false },
    ]);
  });

  it('finish sends trim_edge for the end edge with ripple when Shift was held', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(150, 100, { shiftKey: true }), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'trim_edge', clip_id: 'A', edge: 'end', new_time: sec(1.5), ripple: true },
    ]);
  });

  it('a trim that did not change the edge sends nothing', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    expect(finishDrag(s, { project: p })).toEqual([]);
  });

  it('a trim-end snaps to a nearby point', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(250, 100), ctxFor(p, { snapPoints: [{ time: sec(2.5) }] }));
    expect(u.snapTick).toBe(sec(2.5));
    expect((u.state as TrimDrag).newTime).toBe(sec(2.5));
  });

  it('Alt disables snapping for a trim', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(250, 100, { altKey: true }), ctxFor(p, { snapPoints: [{ time: sec(2.5) }] }));
    expect(u.snapTick).toBeNull();
  });

  it('a trim updates the partner ghost to the same end edge', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(250, 100), ctxFor(p));
    expect(u.ghosts.find((g) => g.clipId === 'AA')?.rect).toEqual({ x: 0, y: 144, w: 250, h: 56 });
  });
});

// ---------------------------------------------------------------------------------------------
describe('roll drag', () => {
  const p = rollProject();

  it('a roll moves the shared edit point with the pointer', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    const u = updateDrag(s as DragState, pt(250, 100, { altKey: true }), ctxFor(p));
    expect((u.state as RollDrag).newCut).toBe(sec(2.5));
  });

  it('a roll draws both clips with the new edit point', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    const u = updateDrag(s as DragState, pt(250, 100, { altKey: true }), ctxFor(p));
    expect(u.ghosts.find((g) => g.clipId === 'A')?.rect).toEqual({ x: 0, y: 86, w: 250, h: 56 });
    expect(u.ghosts.find((g) => g.clipId === 'R')?.rect).toEqual({ x: 250, y: 86, w: 150, h: 56 });
  });

  it('a roll cannot make the left clip shorter than a frame', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    const u = updateDrag(s as DragState, pt(-300, 100, { altKey: true }), ctxFor(p));
    expect((u.state as RollDrag).newCut).toBe(FRAME);
  });

  it('a roll cannot make the right clip shorter than a frame', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    const u = updateDrag(s as DragState, pt(900, 100, { altKey: true }), ctxFor(p));
    expect((u.state as RollDrag).newCut).toBe(sec(4) - FRAME);
  });

  it('a roll still snaps while Alt is held', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    const u = updateDrag(
      s as DragState,
      pt(250, 100, { altKey: true }),
      ctxFor(p, { snapPoints: [{ time: sec(2.5) }] }),
    );
    expect(u.snapTick).toBe(sec(2.5));
  });

  it('finish sends roll_edit with the new cut', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    const u = updateDrag(s as DragState, pt(250, 100, { altKey: true }), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'roll_edit', left_id: 'A', right_id: 'R', new_cut: sec(2.5) },
    ]);
  });

  it('a roll that did not move sends nothing', () => {
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []);
    expect(finishDrag(s as DragState, { project: p })).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('marquee drag', () => {
  const p = marqueeProject();

  function marqueeFrom(x0: number, y0: number, shift = false): DragState {
    const s = beginDrag({ kind: 'empty' }, pt(x0, y0, { shiftKey: shift }), p, ['B']);
    if (s === null) throw new Error('marquee expected');
    return s;
  }

  it('a marquee rectangle runs from the press to the pointer', () => {
    const s = marqueeFrom(10, 40);
    const u = updateDrag(s, pt(250, 120), ctxFor(p));
    expect(u.marquee).toEqual({ x0: 10, y0: 40, x1: 250, y1: 120 });
  });

  it('a marquee selects the clips it touches', () => {
    const s = marqueeFrom(10, 40);
    const u = updateDrag(s, pt(250, 120), ctxFor(p));
    expect(u.selectionIds).toEqual(['A', 'C']);
  });

  it('a marquee dragged from bottom right to top left selects the same clips', () => {
    const s = marqueeFrom(250, 120);
    const u = updateDrag(s, pt(10, 40), ctxFor(p));
    expect(u.selectionIds).toEqual(['A', 'C']);
  });

  it('a Shift marquee adds to the selection that was there before', () => {
    const s = marqueeFrom(10, 40, true);
    const u = updateDrag(s, pt(250, 120), ctxFor(p));
    expect(u.selectionIds).toEqual(['B', 'A', 'C']);
  });

  it('a plain marquee replaces the selection', () => {
    const s = marqueeFrom(10, 40, false);
    const u = updateDrag(s, pt(250, 120), ctxFor(p));
    expect(u.selectionIds).not.toContain('B');
  });

  it('a marquee that only touches a clip edge does not select it', () => {
    const s = marqueeFrom(200, 40);
    const u = updateDrag(s, pt(300, 120), ctxFor(p));
    expect(u.selectionIds).toEqual([]);
  });

  it('a marquee ignores clips on a locked track', () => {
    const q = marqueeProject(true);
    const s = beginDrag({ kind: 'empty' }, pt(0, 146), q, []) as DragState;
    const u = updateDrag(s, pt(100, 200), ctxFor(q));
    expect(u.selectionIds).toEqual([]);
  });

  it('a marquee over an unlocked audio clip selects it (and not the clip above it)', () => {
    const s = beginDrag({ kind: 'empty' }, pt(0, 146), p, []) as DragState;
    const u = updateDrag(s, pt(100, 200), ctxFor(p));
    expect(u.selectionIds).toEqual(['AA']);
  });

  it('a marquee draws no ghosts and no snap guide', () => {
    const s = marqueeFrom(10, 40);
    const u = updateDrag(s, pt(250, 120), ctxFor(p));
    expect(u.ghosts).toEqual([]);
    expect(u.snapTick).toBeNull();
  });

  it('finish of a marquee sends nothing', () => {
    const s = marqueeFrom(10, 40);
    const u = updateDrag(s, pt(250, 120), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('envelope point drag', () => {
  const ep = envProject();

  /**
   * Starts a drag on point `index` with the pointer on its gain line. The clip row is 56 px tall
   * at y 86..142, so y 100 reads -6 dB (the point's own value) and the scale is 72 dB / 56 px.
   */
  function start(index: number): DragState {
    const s = beginEnvelopeDrag(ep, 'A', index, pt(0, 100));
    if (s === null) throw new Error('envelope drag expected');
    return s;
  }

  it('dragging a point right moves its time by the px distance', () => {
    const u = updateDrag(start(1), pt(50, 100), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]).toEqual({ t: sec(1.5), db: -6 });
  });

  it('dragging a point up by 10 px raises its gain by 10 * 72 / 56 dB', () => {
    const u = updateDrag(start(1), pt(0, 90), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]?.db).toBeCloseTo(-60 + (52 / 56) * 72, 6);
  });

  it('dragging a point down by 14 px lowers its gain by exactly 18 dB', () => {
    const u = updateDrag(start(1), pt(0, 114), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]?.db).toBe(-24);
  });

  it('gain cannot go below minus 60 dB', () => {
    const u = updateDrag(start(1), pt(0, 1000), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]?.db).toBe(-60);
  });

  it('gain cannot go above plus 12 dB', () => {
    const u = updateDrag(start(1), pt(0, -1000), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]?.db).toBe(12);
  });

  it('time cannot pass the next point', () => {
    const u = updateDrag(start(1), pt(300, 100), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]?.t).toBe(sec(1.8));
  });

  it('time cannot go before the previous point', () => {
    const u = updateDrag(start(1), pt(-300, 100), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[1]?.t).toBe(0);
  });

  it('the last point is limited by the clip length', () => {
    const u = updateDrag(start(2), pt(300, 100), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[2]?.t).toBe(sec(2));
  });

  it('the first point starts at time zero', () => {
    const u = updateDrag(start(0), pt(-300, 100), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[0]?.t).toBe(0);
  });

  it('other points are not changed by the drag', () => {
    const u = updateDrag(start(1), pt(50, -10), ctxFor(ep));
    expect((u.state as EnvelopeDrag).points[0]).toEqual({ t: 0, db: 0 });
    expect((u.state as EnvelopeDrag).points[2]).toEqual({ t: sec(1.8), db: -12 });
  });

  it('finish sends set_envelope with the new points', () => {
    const u = updateDrag(start(1), pt(50, 100), ctxFor(ep));
    const [cmd] = finishDrag(u.state, { project: ep });
    expect(cmd).toMatchObject({ type: 'set_envelope', clip_id: 'A' });
    expect((cmd as { points: EnvPoint[] }).points[1]).toEqual({ t: sec(1.5), db: -6 });
  });

  it('finish sends nothing when the point did not change', () => {
    expect(finishDrag(start(1), { project: ep })).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('fade handle drag', () => {
  const fp = envProject();

  it('dragging the fade-in handle 50 px right sets a 0.5 s fade-in', () => {
    const s = beginFadeDrag(fp, 'A', 'in', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(50, 0), ctxFor(fp));
    expect((u.state as FadeDrag).value).toBe(sec(0.5));
  });

  it('finish of a fade-in sends patch_clips with fade_in', () => {
    const s = beginFadeDrag(fp, 'A', 'in', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(50, 0), ctxFor(fp));
    expect(finishDrag(u.state, { project: fp })).toEqual([
      { type: 'patch_clips', ids: ['A'], patch: { fade_in: sec(0.5) } },
    ]);
  });

  it('dragging the fade-out handle 50 px left sets a 0.5 s fade-out', () => {
    // The fade-out starts at 1.5 s. Dragging left lengthens it by 0.5 s, up to the 2 s clip length.
    const s = beginFadeDrag(fp, 'A', 'out', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(-50, 0), ctxFor(fp));
    expect((u.state as FadeDrag).value).toBe(sec(2));
  });

  it('finish of a fade-out sends patch_clips with fade_out', () => {
    const q = project([track('v1', 'video')], [clip('A', 'v1', 0, sec(2))]);
    const s = beginFadeDrag(q, 'A', 'out', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(-50, 0), ctxFor(q));
    expect(finishDrag(u.state, { project: q })).toEqual([
      { type: 'patch_clips', ids: ['A'], patch: { fade_out: sec(0.5) } },
    ]);
  });

  it('fade-in is limited so that it does not overlap the fade-out', () => {
    const s = beginFadeDrag(fp, 'A', 'in', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(300, 0), ctxFor(fp));
    // Clip is 2 s and fade-out is 1.5 s, so fade-in may reach 0.5 s.
    expect((u.state as FadeDrag).value).toBe(sec(0.5));
  });

  it('fade-in cannot go below zero', () => {
    const s = beginFadeDrag(fp, 'A', 'in', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(-300, 0), ctxFor(fp));
    expect((u.state as FadeDrag).value).toBe(0);
  });

  it('fade-out is limited by the fade-in length', () => {
    const q = project([track('v1', 'video')], [
      clip('A', 'v1', 0, sec(2), { audio: { ...defaultClipAudio(), fade_in: sec(1.8) } }),
    ]);
    const s = beginFadeDrag(q, 'A', 'out', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(-300, 0), ctxFor(q));
    expect((u.state as FadeDrag).value).toBe(sec(2) - sec(1.8));
  });

  it('a fade drag with no change sends nothing', () => {
    const s = beginFadeDrag(fp, 'A', 'in', pt(0, 0)) as FadeDrag;
    expect(finishDrag(s, { project: fp })).toEqual([]);
  });

  it('a fade drag draws no ghosts', () => {
    const s = beginFadeDrag(fp, 'A', 'in', pt(0, 0)) as FadeDrag;
    const u = updateDrag(s, pt(50, 0), ctxFor(fp));
    expect(u.ghosts).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('playhead and pan drags', () => {
  it('a playhead drag previews the tick under the pointer', () => {
    const p = mainProject();
    const u = updateDrag({ kind: 'playhead' }, pt(250, 10), ctxFor(p));
    expect(u.previewTick).toBe(sec(2.5));
  });

  it('a playhead drag accounts for the horizontal scroll', () => {
    const p = mainProject();
    const u = updateDrag({ kind: 'playhead' }, pt(250, 10), ctxFor(p, { view: { ...VIEW, scrollX: 100 } }));
    expect(u.previewTick).toBe(sec(3.5));
  });

  it('a playhead drag never reports a snap', () => {
    const p = mainProject();
    const u = updateDrag({ kind: 'playhead' }, pt(250, 10), ctxFor(p, { snapPoints: [{ time: sec(2.5) }] }));
    expect(u.snapTick).toBeNull();
  });

  it('finish of a playhead drag sends nothing', () => {
    expect(finishDrag({ kind: 'playhead' }, { project: mainProject() })).toEqual([]);
  });

  it('a pan drag returns the state unchanged', () => {
    const s = beginPan(pt(0, 0), 0, 0);
    const u = updateDrag(s, pt(40, 40), ctxFor(mainProject()));
    expect(u.state).toBe(s);
    expect(u.ghosts).toEqual([]);
  });

  it('finish of a pan drag sends nothing', () => {
    expect(finishDrag(beginPan(pt(0, 0), 0, 0), { project: mainProject() })).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('constants', () => {
  it('the drag threshold is 3 px (enforced by the caller, not by this module)', () => {
    expect(DRAG_THRESHOLD_PX).toBe(3);
  });
});

// ---------------------------------------------------------------------------------------------
// Added with the drag fixes: pan typing, envelope insert, fade preview, duplicate, source
// handles, neighbours, group trim edges, partner moves and frame alignment.
// ---------------------------------------------------------------------------------------------

/** A media entry of the given duration (ticks). Only the fields the drag code reads matter. */
function mediaRef(id: string, duration: number): MediaRef {
  return {
    id,
    kind: 'video',
    name: id,
    path: `D:/clips/${id}.mp4`,
    library_recording_id: null,
    duration,
    width: 1920,
    height: 1080,
    fps: 30,
    video_codec: 'h264',
    audio: [],
    missing: false,
  } as unknown as MediaRef;
}

describe('beginPanTyped', () => {
  it('returns a pan drag with the press origin and the scroll offsets', () => {
    const s = beginPanTyped(12, 34, 100, 200);
    expect(s).toEqual({ kind: 'pan', originX: 12, originY: 34, scrollX0: 100, scrollY0: 200 });
  });

  it('keeps the offsets it is given without clamping them', () => {
    const s = beginPanTyped(0, 0, -5, -7);
    expect(s.scrollX0).toBe(-5);
    expect(s.scrollY0).toBe(-7);
  });

  it('the pan drag moves the scroll with the pointer through panScroll', () => {
    const s = beginPanTyped(100, 100, 300, 300);
    expect(panScroll(s, pt(60, 130))).toEqual({ scrollX: 340, scrollY: 270 });
  });
});

describe('hitEnvelopePoint re-export', () => {
  it('timelineDrag exports the same function as layout', () => {
    expect(hitEnvelopePoint).toBe(layoutHitEnvelopePoint);
  });

  it('finds an envelope point through the drag module', () => {
    const ep = envProject();
    const rows = trackLayout(ep, 0);
    const rect = clipRect(ep.clips[0]!, rows, VIEW)!;
    const hit = hitEnvelopePoint(ep, rows, VIEW, rect.x, envelopeY(0, rect));
    expect(hit?.index).toBe(0);
  });
});

describe('beginEnvelopeHitDrag', () => {
  // envProject: A 0..2 s with points at 0 s (0 dB), 1 s (-6 dB), 1.8 s (-12 dB).
  const ep = envProject();

  it('a hit on a point drags that point', () => {
    const hit = { clipId: 'A', index: 1, tick: sec(1), db: -6 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(100, 60));
    expect(s).toMatchObject({ kind: 'envelope-point', clipId: 'A', index: 1 });
    expect((s as EnvelopeDrag).origin).toEqual({ t: sec(1), db: -6 });
  });

  it('a hit on the gain line inserts a point at the hit time in time order', () => {
    const hit = { clipId: 'A', index: null, tick: sec(1.5), db: -9 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(150, 60)) as EnvelopeDrag;
    expect(s.base).toHaveLength(4);
    expect(s.index).toBe(2);
    expect(s.base[2]).toEqual({ t: sec(1.5), db: -9 });
  });

  it('a gain-line hit between the first and second point inserts at index 1', () => {
    const hit = { clipId: 'A', index: null, tick: sec(0.2), db: -3 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(20, 60)) as EnvelopeDrag;
    expect(s.index).toBe(1);
    expect(s.base[1]).toEqual({ t: sec(0.2), db: -3 });
  });

  it('a gain-line hit after the last point appends', () => {
    const hit = { clipId: 'A', index: null, tick: sec(1.95), db: -20 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(195, 60)) as EnvelopeDrag;
    expect(s.index).toBe(3);
    expect(s.base).toHaveLength(4);
  });

  it('the inserted gain is clamped to the envelope range', () => {
    const hit = { clipId: 'A', index: null, tick: sec(0.5), db: 50 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(50, 60)) as EnvelopeDrag;
    expect(s.origin.db).toBe(12);
  });

  it('a press on an inserted point without movement sends no command', () => {
    const hit = { clipId: 'A', index: null, tick: sec(1.5), db: -9 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(150, 60));
    expect(finishDrag(s!, { project: ep })).toEqual([]);
  });

  it('an inserted point sends set_envelope once the pointer moves it', () => {
    const hit = { clipId: 'A', index: null, tick: sec(1.5), db: -9 };
    const s = beginEnvelopeHitDrag(ep, hit, pt(150, 60))!;
    const u = updateDrag(s, pt(170, 60), ctxFor(ep));
    const cmds = finishDrag(u.state, { project: ep });
    expect(cmds).toHaveLength(1);
    expect(cmds[0]).toMatchObject({ type: 'set_envelope', clip_id: 'A' });
    expect((cmds[0] as { points: EnvPoint[] }).points).toHaveLength(4);
  });

  it('a hit on a missing clip starts nothing', () => {
    const hit = { clipId: 'nope', index: null, tick: 0, db: 0 };
    expect(beginEnvelopeHitDrag(ep, hit, pt(0, 0))).toBeNull();
  });

  it('a hit on a point index that is out of range starts nothing', () => {
    const hit = { clipId: 'A', index: 9, tick: 0, db: 0 };
    expect(beginEnvelopeHitDrag(ep, hit, pt(0, 0))).toBeNull();
  });
});

describe('fade preview on update results', () => {
  const ep = envProject();

  it('a fade-in drag reports both fade lengths for the canvas', () => {
    const s = beginFadeDrag(ep, 'A', 'in', pt(0, 60)) as FadeDrag;
    const u = updateDrag(s, pt(30, 60), ctxFor(ep));
    expect(u.fadePreview).toEqual({ clipId: 'A', fadeIn: sec(0.3), fadeOut: sec(1.5) });
  });

  it('a fade-out drag keeps the fade-in length in the preview', () => {
    const withIn = project([track('v1', 'video')], [
      clip('A', 'v1', 0, sec(2), { audio: { ...defaultClipAudio(), fade_in: sec(0.25), fade_out: sec(0.5) } }),
    ]);
    const s = beginFadeDrag(withIn, 'A', 'out', pt(200, 60)) as FadeDrag;
    const u = updateDrag(s, pt(150, 60), ctxFor(withIn));
    expect(u.fadePreview).toEqual({ clipId: 'A', fadeIn: sec(0.25), fadeOut: sec(1) });
  });

  it('a non-fade update has no fade preview', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 100), mainProject(), []) as MoveDrag;
    const u = updateDrag(s, pt(20, 100), ctxFor(mainProject()));
    expect(u.fadePreview).toBeUndefined();
  });

  it('the fade preview is limited by the clip length minus the opposite fade', () => {
    const s = beginFadeDrag(ep, 'A', 'in', pt(0, 60)) as FadeDrag;
    // Dragging 300 px (3 s) would make the fade-in longer than the clip allows.
    const u = updateDrag(s, pt(300, 60), ctxFor(ep));
    expect(u.fadePreview?.fadeIn).toBe(sec(2) - sec(1.5));
  });
});

describe('duplicate on Alt', () => {
  const p = mainProject();

  it('Alt on a clip body marks the move as a duplicate', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 100, { altKey: true }), p, []) as MoveDrag;
    expect(s.duplicate).toBe(true);
  });

  it('a plain press on a clip body is not a duplicate', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 100), p, []) as MoveDrag;
    expect(s.duplicate).toBe(false);
  });

  it('a duplicating move keeps snapping on while Alt is held', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 40, { altKey: true }), p, []) as MoveDrag;
    const u = updateDrag(s, pt(50, 40, { altKey: true }), ctxFor(p, { snapPoints: [{ time: sec(0.5) }] }));
    expect(u.snapTick).toBe(sec(0.5));
  });

  it('a duplicating move with a track change falls back to a plain move_clips', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 40, { altKey: true }), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 100, { altKey: true }), ctxFor(p));
    const cmds = finishDrag(u.state, { project: p });
    expect(cmds).toEqual([
      { type: 'move_clips', moves: [{ clip_id: 'C', new_start: 0, new_track: 'v1' }] },
    ]);
  });

  it('a duplicating move with no change sends nothing', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 100, { altKey: true }), p, []) as MoveDrag;
    expect(finishDrag(s, { project: p })).toEqual([]);
  });

  it('a duplicating move sends duplicate_clips with the selection ids', () => {
    const s = beginDrag({ kind: 'clip', clipId: 'C' }, pt(0, 40, { altKey: true }), p, ['C', 'A']) as MoveDrag;
    const u = updateDrag(s, pt(100, 40, { altKey: true }), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'duplicate_clips', ids: ['C', 'A'], offset: sec(1) },
    ]);
  });
});

describe('linked partners and track moves', () => {
  it('a track change sends one move_clips with a new track only for the grabbed kind', () => {
    const p = mainProject();
    // A (video, link L1) is dragged up from v1 to v2 with no time change. AA is audio.
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(0, 40), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      {
        type: 'move_clips',
        moves: [
          { clip_id: 'A', new_start: 0, new_track: 'v2' },
          { clip_id: 'AA', new_start: 0, new_track: null },
        ],
      },
    ]);
  });

  it('linked partners move by the same time delta as the grabbed clip', () => {
    const p = movePartnerProject();
    const s = beginDrag({ kind: 'clip', clipId: 'B' }, pt(300, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(250, 100), ctxFor(p));
    const cmds = finishDrag(u.state, { project: p });
    const moves = (cmds[0] as { moves: { clip_id: string; new_start: number }[] }).moves;
    const b = moves.find((m) => m.clip_id === 'B')!;
    const bp = moves.find((m) => m.clip_id === 'BP')!;
    expect(b.new_start - sec(3)).toBe(-sec(0.5));
    expect(bp.new_start - sec(1)).toBe(-sec(0.5));
  });

  it('a partner ghost is drawn at the same shifted time as the grabbed clip', () => {
    const p = movePartnerProject();
    const s = beginDrag({ kind: 'clip', clipId: 'B' }, pt(300, 100), p, []) as MoveDrag;
    const u = updateDrag(s, pt(250, 100), ctxFor(p));
    const b = u.ghosts.find((g) => g.clipId === 'B')!;
    const bp = u.ghosts.find((g) => g.clipId === 'BP')!;
    expect(b.rect.x).toBe(tickToX(sec(2.5), VIEW));
    expect(bp.rect.x).toBe(tickToX(sec(0.5), VIEW));
  });

  it('a move that is smaller than one frame does not change the clip', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'B' }, pt(300, 100), p, []) as MoveDrag;
    // 0.001 s is a fraction of a 1/30 s frame, so the drag lands on the original frame.
    const u = updateDrag(s, pt(300.1, 100), ctxFor(p));
    expect((u.state as MoveDrag).delta).toBe(0);
    expect(finishDrag(u.state, { project: p })).toEqual([]);
  });

  it('a move delta is always a whole number of frames', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'clip', clipId: 'A' }, pt(0, 100), p, []) as MoveDrag;
    for (const x of [1, 7, 13, 29, 53.5]) {
      const u = updateDrag(s, pt(x, 100), ctxFor(p));
      expect((u.state as MoveDrag).delta % FRAME).toBe(0);
    }
  });
});

describe('trim limits from source handles', () => {
  it('trim-end cannot extend past the end of the source media', () => {
    const p = project([track('v1', 'video')], [clip('A', 'v1', 0, sec(2))], { media: [mediaRef('m1', sec(2.5))] });
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(1000, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(2.5));
  });

  it('trim-start cannot extend before the start of the source media', () => {
    // Clip 2 s..4 s with 1 s of source material before it: the start can reach 1 s at most.
    const p = project([track('v1', 'video')], [clip('X', 'v1', sec(2), sec(2), { src_in: sec(1) })], {
      media: [mediaRef('m1', sec(10))],
    });
    const s = beginDrag({ kind: 'edge-start', clipId: 'X' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(-300, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(1));
  });

  it('a double-speed clip may extend only to half the media length on the timeline', () => {
    const p = project([track('v1', 'video')], [clip('F', 'v1', 0, sec(1), { speed: 2 })], {
      media: [mediaRef('m1', sec(10))],
    });
    const s = beginDrag({ kind: 'edge-end', clipId: 'F' }, pt(100, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(1000, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(5));
  });

  it('without a known media duration the source does not limit the trim', () => {
    const p = project([track('v1', 'video')], [clip('A', 'v1', 0, sec(2))]);
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(600, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(6));
  });

  it('a freeze clip (speed 0) cannot move its start edge', () => {
    const p = project([track('v1', 'video')], [clip('Z', 'v1', sec(1), sec(2), { speed: 0 })]);
    const s = beginDrag({ kind: 'edge-start', clipId: 'Z' }, pt(100, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(150, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(1));
    expect(finishDrag(u.state, { project: p })).toEqual([]);
  });

  it('a reverse clip can only shorten at the start edge', () => {
    const p = project([track('v1', 'video')], [clip('R', 'v1', sec(1), sec(2), { reverse: true })]);
    const s = beginDrag({ kind: 'edge-start', clipId: 'R' }, pt(100, 100), p, []) as TrimDrag;
    const extend = updateDrag(s, pt(0, 100), ctxFor(p));
    expect((extend.state as TrimDrag).newTime).toBe(sec(1));
    const shorten = updateDrag(s, pt(150, 100), ctxFor(p));
    expect((shorten.state as TrimDrag).newTime).toBe(sec(1.5));
  });

  it('trim results are whole frames', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    for (const x of [202, 233, 251.7, 180]) {
      const u = updateDrag(s, pt(x, 100), ctxFor(p));
      expect((u.state as TrimDrag).newTime % FRAME).toBe(0);
    }
  });
});

describe('trim limits from neighbours and ripple', () => {
  it('a trim-start stops at the end of the previous clip on its track', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'B' }, pt(300, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(0, 100), ctxFor(p));
    // A ends at 2 s on the same track, so B cannot start before it.
    expect((u.state as TrimDrag).newTime).toBe(sec(2));
  });

  it('a ripple trim-start (Shift) ignores the previous clip', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'B' }, pt(300, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(0, 100, { shiftKey: true }), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(0);
  });

  it('a trim-start stops one frame before the clip end', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'B' }, pt(300, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(1000, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(5) - FRAME);
  });

  it('a trim-end dragged far right stops at the next clip start', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(900, 100), ctxFor(p));
    expect((u.state as TrimDrag).newTime).toBe(sec(3));
  });
});

describe('group trim edges', () => {
  it('the start edge of the first clip in a group is a trim', () => {
    const p = groupProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'B' }, pt(300, 100), p, []);
    expect(s?.kind).toBe('trim-start');
  });

  it('the end edge of the last clip in a group is a trim', () => {
    const p = groupProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'BG' }, pt(700, 100), p, []);
    expect(s?.kind).toBe('trim-end');
  });

  it('an inner group edge starts a move instead of a trim', () => {
    const p = groupProject();
    // B ends at 5 s, but BG runs to 7 s: that end edge is inside the group.
    const s = beginDrag({ kind: 'edge-end', clipId: 'B' }, pt(500, 100), p, []);
    expect(s?.kind).toBe('move');
  });

  it('the start edge of the second group clip is inside the group and starts a move', () => {
    const p = groupProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'BG' }, pt(600, 100), p, []);
    expect(s?.kind).toBe('move');
  });

  it('a group trim sends one trim_edge for the grabbed clip only', () => {
    const p = groupProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'BG' }, pt(700, 100), p, []) as TrimDrag;
    const u = updateDrag(s, pt(750, 100), ctxFor(p));
    expect(finishDrag(u.state, { project: p })).toEqual([
      { type: 'trim_edge', clip_id: 'BG', edge: 'end', new_time: sec(7.5), ripple: false },
    ]);
  });
});

describe('roll frame alignment and Alt edges', () => {
  it('a sub-frame roll move returns the original edit point', () => {
    const p = rollProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []) as RollDrag;
    const u = updateDrag(s, pt(200.5, 100, { altKey: true }), ctxFor(p));
    expect((u.state as RollDrag).newCut).toBe(sec(2));
    expect(finishDrag(u.state, { project: p })).toEqual([]);
  });

  it('a roll edit point is always a whole frame', () => {
    const p = rollProject();
    const s = beginDrag({ kind: 'edge-end', clipId: 'A' }, pt(200, 100, { altKey: true }), p, []) as RollDrag;
    for (const x of [203, 217.3, 249]) {
      const u = updateDrag(s, pt(x, 100, { altKey: true }), ctxFor(p));
      expect((u.state as RollDrag).newCut % FRAME).toBe(0);
    }
  });

  it('Alt on the start edge with no touching neighbour is a trim, not a roll', () => {
    const p = mainProject();
    const s = beginDrag({ kind: 'edge-start', clipId: 'C' }, pt(0, 100, { altKey: true }), p, []);
    expect(s?.kind).toBe('trim-start');
  });
});
