import { describe, expect, it } from 'vitest';
import {
  ENVELOPE_HIT_PX,
  ENVELOPE_MIN_HEIGHT_PX,
  EDGE_HIT_PX,
  RULER_HEIGHT,
  TRACK_GAP,
  clipRect,
  cutPreviewFor,
  cutPreviewLabel,
  cutSnapPoint,
  dragMoveTarget,
  envelopeDbAt,
  envelopeY,
  formatRulerLabel,
  hitEnvelopePoint,
  hitTest,
  marqueeSelect,
  niceRulerStep,
  snapTargets,
  snapTick,
  tickToX,
  trackLayout,
  visibleRange,
  xToTick,
} from './layout';
import type { TrackRow, View } from './layout';
import { TICKS_PER_SECOND } from './ticks';
import type { Canvas, Clip, Marker, Project, Track } from './types';
import { defaultCanvas, defaultClipAudio, defaultTransform } from './types';

const S = TICKS_PER_SECOND;

function track(id: string, kind: 'video' | 'audio', over: Partial<Track> = {}): Track {
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

function clip(id: string, trackId: string, startSec: number, durSec: number, over: Partial<Clip> = {}): Clip {
  return {
    id,
    track_id: trackId,
    kind: 'video',
    media_id: 'm1',
    name: id,
    start: Math.round(startSec * S),
    dur: Math.round(durSec * S),
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

function project(tracks: Track[], clips: Clip[] = [], extra: Partial<Project> = {}): Project {
  const canvas: Canvas = defaultCanvas();
  return {
    schema_version: 1,
    id: 'p1',
    name: 'Test',
    created_ms: 0,
    modified_ms: 0,
    canvas,
    media: [],
    tracks,
    clips,
    groups: [],
    markers: [],
    work_area: null,
    modes: { magnetic: false, snap: true },
    export_defaults: {
      preset: 'balanced_1080p30',
      name_template: '{project}',
      dir: null,
      range: { kind: 'entire' },
      width: 1920,
      height: 1080,
      fps: null,
      codec: 'h264',
      encoder: 'auto',
      quality: { mode: 'quality', value: 70 },
      audio: { layout: 'mix', bitrate_kbps: 192, sample_rate: 48000, loudness: { mode: 'off' }, limiter: true },
      kind: { kind: 'video' },
      lossless: false,
    },
    ui_state: { zoom_px_per_sec: 100, scroll_x: 0, playhead: 0 },
    ...extra,
  };
}

const VIEW: View = { pxPerSec: 100, scrollX: 0, width: 800, height: 400 };

describe('xToTick and tickToX', () => {
  it('maps 100 px to one second at 100 px/s', () => {
    expect(xToTick(100, VIEW)).toBe(S);
  });
  it('maps 50 px to half a second', () => {
    expect(xToTick(50, VIEW)).toBe(S / 2);
  });
  it('adds the horizontal scroll offset', () => {
    expect(xToTick(0, { ...VIEW, scrollX: 200 })).toBe(2 * S);
  });
  it('clamps negative x to tick 0', () => {
    expect(xToTick(-500, VIEW)).toBe(0);
  });
  it('maps one second to 100 px and subtracts scroll', () => {
    expect(tickToX(S, VIEW)).toBe(100);
    expect(tickToX(S, { ...VIEW, scrollX: 200 })).toBe(-100);
  });
  it('round trips x -> tick -> x at higher zoom and with scroll', () => {
    const v: View = { pxPerSec: 37.5, scrollX: 412, width: 900, height: 300 };
    for (const x of [0, 137, 600.25, 1234]) {
      expect(tickToX(xToTick(x, v), v)).toBeCloseTo(x, 4);
    }
  });
  it('scales with zoom: 50 px/s maps 100 px to two seconds', () => {
    expect(xToTick(100, { ...VIEW, pxPerSec: 50 })).toBe(2 * S);
  });
  it('falls back to 1 px/s for zero, negative or NaN zoom', () => {
    expect(xToTick(3, { ...VIEW, pxPerSec: 0 })).toBe(3 * S);
    expect(xToTick(3, { ...VIEW, pxPerSec: -4 })).toBe(3 * S);
    expect(tickToX(S, { ...VIEW, pxPerSec: Number.NaN })).toBe(1);
  });
});

describe('trackLayout', () => {
  it('returns no rows for a project without tracks', () => {
    expect(trackLayout(project([]), 0)).toEqual([]);
  });
  it('lists video tracks highest layer first, then audio tracks', () => {
    const p = project([
      track('V0', 'video'),
      track('V1', 'video'),
      track('A0', 'audio'),
      track('V2', 'video'),
      track('A1', 'audio'),
    ]);
    const ids = trackLayout(p, 0).map((r) => r.trackId);
    expect(ids).toEqual(['V2', 'V1', 'V0', 'A0', 'A1']);
  });
  it('stacks rows with the gap and starts below the ruler', () => {
    const p = project([track('V0', 'video'), track('A0', 'audio')]);
    const rows = trackLayout(p, 0);
    expect(rows[0]?.top).toBe(RULER_HEIGHT);
    expect(rows[1]?.top).toBe(RULER_HEIGHT + 56 + TRACK_GAP);
  });
  it('offsets every row by the vertical scroll', () => {
    const p = project([track('V0', 'video')]);
    expect(trackLayout(p, 10)[0]?.top).toBe(RULER_HEIGHT - 10);
  });
  it('uses each track height and reports the project index', () => {
    const p = project([track('V0', 'video'), track('V1', 'video', { height: 80 }), track('A0', 'audio')]);
    const rows = trackLayout(p, 0);
    const v1 = rows.find((r) => r.trackId === 'V1');
    const v0 = rows.find((r) => r.trackId === 'V0');
    expect(v1?.height).toBe(80);
    expect(v1?.index).toBe(1);
    expect(v0?.top).toBe(RULER_HEIGHT + 80 + TRACK_GAP);
    expect(rows.find((r) => r.trackId === 'A0')?.index).toBe(2);
  });
  it('uses the default 56 px height for NaN, zero or negative heights', () => {
    const p = project([
      track('V0', 'video', { height: Number.NaN }),
      track('V1', 'video', { height: 0 }),
      track('A0', 'audio', { height: -5 }),
    ]);
    for (const row of trackLayout(p, 0)) {
      expect(row.height).toBe(56);
    }
  });
  it('keeps the row kind', () => {
    const rows = trackLayout(project([track('A0', 'audio'), track('V0', 'video')]), 0);
    expect(rows.map((r) => r.kind)).toEqual(['video', 'audio']);
  });
});

describe('clipRect', () => {
  const p = project([track('V0', 'video')]);
  const rows = trackLayout(p, 0);
  it('places a clip at its start and sizes it by duration', () => {
    const r = clipRect(clip('a', 'V0', 2, 1), rows, VIEW);
    expect(r).toEqual({ x: 200, y: RULER_HEIGHT, w: 100, h: 56 });
  });
  it('applies horizontal scroll', () => {
    const r = clipRect(clip('a', 'V0', 2, 1), rows, { ...VIEW, scrollX: 50 });
    expect(r?.x).toBe(150);
  });
  it('keeps a minimum width of 2 px for very short clips', () => {
    const r = clipRect(clip('a', 'V0', 2, 0.0001), rows, VIEW);
    expect(r?.w).toBe(2);
  });
  it('returns null for a clip on a track with no row', () => {
    expect(clipRect(clip('a', 'NOPE', 0, 1), rows, VIEW)).toBeNull();
  });
});

describe('hitTest', () => {
  // Row V0 spans y 28..84 (gap to 86). Clips in pixels at 100 px/s:
  //   A 100..300, B 300..500 (touches A), C 600..700. Gap 500..600.
  const p = project(
    [track('V0', 'video')],
    [clip('A', 'V0', 1, 2), clip('B', 'V0', 3, 2), clip('C', 'V0', 6, 1)],
  );
  const rows = trackLayout(p, 0);
  const Y = 56;

  it('reports the ruler above the first track row', () => {
    const h = hitTest(p, rows, VIEW, 250, 20);
    expect(h.kind).toBe('ruler');
    expect(h.tick).toBe(2.5 * S);
  });
  it('reports the clip body between its edges', () => {
    const h = hitTest(p, rows, VIEW, 150, Y);
    expect(h).toMatchObject({ kind: 'clip', clipId: 'A', trackId: 'V0' });
    expect(h.tick).toBe(1.5 * S);
  });
  it('reports the start edge within EDGE_HIT_PX', () => {
    expect(hitTest(p, rows, VIEW, 100 + EDGE_HIT_PX, Y)).toMatchObject({ kind: 'edge-start', clipId: 'A' });
  });
  it('reports the body just outside EDGE_HIT_PX', () => {
    expect(hitTest(p, rows, VIEW, 100 + EDGE_HIT_PX + 1, Y)).toMatchObject({ kind: 'clip', clipId: 'A' });
  });
  it('reports the end edge of a clip', () => {
    expect(hitTest(p, rows, VIEW, 297, Y)).toMatchObject({ kind: 'edge-end', clipId: 'A' });
  });
  it('on touching clips, the pointer side decides: left of the join is the earlier end edge', () => {
    expect(hitTest(p, rows, VIEW, 299, Y)).toMatchObject({ kind: 'edge-end', clipId: 'A' });
  });
  it('on touching clips, right of the join is the later clip start edge', () => {
    expect(hitTest(p, rows, VIEW, 301, Y)).toMatchObject({ kind: 'edge-start', clipId: 'B' });
  });
  it('on touching clips exactly at the join the later clip start edge wins (order A,B)', () => {
    expect(hitTest(p, rows, VIEW, 300, Y)).toMatchObject({ kind: 'edge-start', clipId: 'B' });
  });
  it('on touching clips exactly at the join the later clip start edge wins (reversed order B,A)', () => {
    const reversed = project([track('V0', 'video')], [clip('B', 'V0', 3, 2), clip('A', 'V0', 1, 2)]);
    const r = trackLayout(reversed, 0);
    expect(hitTest(reversed, r, VIEW, 300, Y)).toMatchObject({ kind: 'edge-start', clipId: 'B' });
  });
  it('reports the end edge of a clip that ends on empty space', () => {
    expect(hitTest(p, rows, VIEW, 500, Y)).toMatchObject({ kind: 'edge-end', clipId: 'B' });
  });
  it('reports the start edge of a clip that starts after a gap', () => {
    expect(hitTest(p, rows, VIEW, 600, Y)).toMatchObject({ kind: 'edge-start', clipId: 'C' });
  });
  it('reports a gap between two clips on the same track', () => {
    const h = hitTest(p, rows, VIEW, 550, Y);
    expect(h).toMatchObject({ kind: 'gap', trackId: 'V0' });
    expect(h.tick).toBe(5.5 * S);
  });
  it('reports a plain track area after the last clip', () => {
    expect(hitTest(p, rows, VIEW, 750, Y)).toMatchObject({ kind: 'track', trackId: 'V0' });
  });
  it('reports a plain track area on an empty track', () => {
    const empty = project([track('V0', 'video')]);
    expect(hitTest(empty, trackLayout(empty, 0), VIEW, 50, Y)).toMatchObject({ kind: 'track', trackId: 'V0' });
  });
  it('treats the gap below a row as part of that row', () => {
    // x 750 is past clip C (ends at 700), so no clip follows: plain track space. y 85 is the gap below row V0.
    expect(hitTest(p, rows, VIEW, 750, 85)).toMatchObject({ kind: 'track', trackId: 'V0' });
  });
  it('reports empty below all rows', () => {
    expect(hitTest(p, rows, VIEW, 50, 200)).toEqual({ kind: 'empty' });
  });
  it('ignores clips on other tracks', () => {
    const two = project([track('V0', 'video'), track('V1', 'video')], [clip('X', 'V1', 0, 10)]);
    const r = trackLayout(two, 0);
    // V1 is the top row (y 28..84); V0 is the second row (y 86..142).
    expect(hitTest(two, r, VIEW, 50, 120)).toMatchObject({ kind: 'track', trackId: 'V0' });
  });
});

describe('snapTick', () => {
  const points = [
    { time: 0, kind: 'start' },
    { time: 2 * S, kind: 'clip-end' },
    { time: 5 * S, kind: 'marker' },
  ];
  it('snaps to a point within the pixel threshold', () => {
    // 6 px at 100 px/s is 0.06 s; 1.95 s is 0.05 s from 2 s.
    const r = snapTick(Math.round(1.95 * S), points, 6, 100);
    expect(r.snapped).toBe(true);
    expect(r.tick).toBe(2 * S);
    expect(r.point).toEqual({ time: 2 * S, kind: 'clip-end' });
  });
  it('does not snap beyond the threshold', () => {
    const t = Math.round(1.9 * S);
    const r = snapTick(t, points, 6, 100);
    expect(r.snapped).toBe(false);
    expect(r.tick).toBe(t);
    expect(r.point).toBeUndefined();
  });
  it('snaps when the point is 0.055 s away (inside a 0.06 s threshold)', () => {
    expect(snapTick(Math.round(1.945 * S), points, 6, 100).snapped).toBe(true);
  });
  it('keeps the earlier point when two are equally close', () => {
    const r = snapTick(1 * S, points, 200, 100);
    expect(r.tick).toBe(0);
  });
  it('never snaps with an empty point list', () => {
    expect(snapTick(S, [], 10, 100)).toEqual({ tick: S, snapped: false });
  });
  it('uses 1 px/s when the zoom is invalid', () => {
    // threshold 6 px at 1 px/s is 6 s; 5 s away snaps, 8 s away does not.
    expect(snapTick(5 * S, [{ time: 0, kind: 'start' }], 6, 0).snapped).toBe(true);
    expect(snapTick(8 * S, [{ time: 0, kind: 'start' }], 6, 0).snapped).toBe(false);
  });
});

describe('dragMoveTarget', () => {
  // Rows (scroll 0): V1 y 28..84, V0 y 86..142, A0 y 144..200.
  const p = project(
    [track('V0', 'video'), track('V1', 'video'), track('A0', 'audio')],
    [clip('c1', 'V0', 2, 1), clip('c2', 'V0', 4, 1), clip('c3', 'A0', 0.5, 1)],
  );
  const rows = trackLayout(p, 0);

  it('converts the pixel delta to ticks', () => {
    const t = dragMoveTarget(p, rows, VIEW, ['c1'], 50, 100, 'c1');
    expect(t.delta).toBe(S / 2);
  });
  it('reports the video row under the pointer as the track shift', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], 0, 100, 'c1').trackShift).toBe('V0');
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], 0, 50, 'c1').trackShift).toBe('V1');
  });
  it('rejects a row of another kind', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], 0, 150, 'c1').trackShift).toBeNull();
  });
  it('accepts an audio row for an audio clip', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c3'], 0, 150, 'c3').trackShift).toBe('A0');
  });
  it('rejects a video row for an audio clip', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c3'], 0, 100, 'c3').trackShift).toBeNull();
  });
  it('returns no track shift over the ruler or below all rows', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], 0, 10, 'c1').trackShift).toBeNull();
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], 0, 400, 'c1').trackShift).toBeNull();
  });
  it('clamps a leftward move so the earliest clip starts at tick 0', () => {
    // Clip starts at 2 s, so a move of -5 s is clamped to -2 s.
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], -500, 100, 'c1').delta).toBe(-2 * S);
  });
  it('allows a leftward move that stays at or after tick 0', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], -100, 100, 'c1').delta).toBe(-S);
  });
  it('clamps using the earliest of several moved clips', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c1', 'c2'], -300, 100, 'c1').delta).toBe(-2 * S);
  });
  it('returns zero delta when no clips are moved', () => {
    expect(dragMoveTarget(p, rows, VIEW, [], 200, 100, 'c1').delta).toBe(0);
  });
  it('returns a null track shift for an unknown grabbed clip', () => {
    expect(dragMoveTarget(p, rows, VIEW, ['c1'], 0, 100, 'nope').trackShift).toBeNull();
  });
});

describe('niceRulerStep', () => {
  it('picks a one second major step at 100 px/s', () => {
    const s = niceRulerStep(100);
    expect(s.majorSec).toBe(1);
    expect(s.minorSec).toBeCloseTo(0.2, 10);
  });
  it('picks a tenth of a second at 1000 px/s', () => {
    expect(niceRulerStep(1000).majorSec).toBe(0.1);
  });
  it('picks a five second major step at 30 px/s', () => {
    expect(niceRulerStep(30).majorSec).toBe(5);
  });
  it('picks a 120 second major step at 1 px/s', () => {
    const s = niceRulerStep(1);
    expect(s.majorSec).toBe(120);
    expect(s.minorSec).toBeCloseTo(20, 10);
  });
  it('picks a 30 minute major step at 0.1 px/s', () => {
    expect(niceRulerStep(0.1).majorSec).toBe(1800);
  });
  it('falls back to the largest step when zoomed out further', () => {
    expect(niceRulerStep(0.01).majorSec).toBe(3600);
  });
  it('keeps at least 90 px between major ticks for a range of zooms', () => {
    for (const pps of [0.5, 3, 7, 20, 200, 2000]) {
      const s = niceRulerStep(pps);
      expect(s.majorSec * pps).toBeGreaterThanOrEqual(90);
    }
  });
  it('uses the 120 second step for NaN zoom (falls back to 1 px/s)', () => {
    expect(niceRulerStep(Number.NaN).majorSec).toBe(120);
  });
});

describe('formatRulerLabel', () => {
  it('formats zero as 0:00', () => {
    expect(formatRulerLabel(0)).toBe('0:00');
  });
  it('formats seconds as M:SS', () => {
    expect(formatRulerLabel(5)).toBe('0:05');
    expect(formatRulerLabel(90)).toBe('1:30');
  });
  it('formats hours as H:MM:SS', () => {
    expect(formatRulerLabel(3720)).toBe('1:02:00');
    expect(formatRulerLabel(7200)).toBe('2:00:00');
  });
  it('adds one tenth digit for fractional seconds', () => {
    expect(formatRulerLabel(0.5)).toBe('0:00.5');
    expect(formatRulerLabel(61.5)).toBe('1:01.5');
  });
  it('shows negative and NaN as 0:00', () => {
    expect(formatRulerLabel(-4)).toBe('0:00');
    expect(formatRulerLabel(Number.NaN)).toBe('0:00');
  });
});

describe('marqueeSelect', () => {
  // Video track V0 (lower row) and locked track VL (upper row). Rows at scroll 0:
  //   VL y 28..84, V0 y 86..142.
  const p = project(
    [track('V0', 'video'), track('VL', 'video', { locked: true })],
    [clip('c1', 'V0', 2, 1), clip('c2', 'V0', 4, 1), clip('cl', 'VL', 2, 1)],
  );
  const rows: TrackRow[] = trackLayout(p, 0);

  it('selects clips that intersect the rectangle', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 150, y0: 90, x1: 350, y1: 120 })).toEqual(['c1']);
  });
  it('accepts the rectangle corners in any order', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 350, y0: 120, x1: 150, y1: 90 })).toEqual(['c1']);
  });
  it('selects a clip overlapped by just a few pixels', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 290, y0: 90, x1: 310, y1: 100 })).toEqual(['c1']);
  });
  it('ignores clips on locked tracks', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 0, y0: 30, x1: 800, y1: 60 })).toEqual([]);
  });
  it('does not select a clip the rectangle only touches', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 100, y0: 90, x1: 200, y1: 120 })).toEqual([]);
  });
  it('selects several clips in one sweep', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 0, y0: 86, x1: 800, y1: 142 })).toEqual(['c1', 'c2']);
  });
  it('selects nothing in an empty area', () => {
    expect(marqueeSelect(p, rows, VIEW, { x0: 600, y0: 90, x1: 700, y1: 120 })).toEqual([]);
  });
});

describe('visibleRange', () => {
  it('starts at the scroll offset and spans the width', () => {
    const r = visibleRange({ pxPerSec: 100, scrollX: 200, width: 400, height: 0 });
    expect(r.startTick).toBe(2 * S);
    expect(r.endTick).toBe(6 * S);
  });
  it('starts at tick 0 with no scroll', () => {
    expect(visibleRange(VIEW).startTick).toBe(0);
  });
  it('clamps a negative scroll so the range starts at 0', () => {
    const r = visibleRange({ pxPerSec: 100, scrollX: -50, width: 400, height: 0 });
    expect(r.startTick).toBe(0);
    expect(r.endTick).toBe(Math.round(3.5 * S));
  });
  it('uses 1 px/s for a zero zoom', () => {
    expect(visibleRange({ pxPerSec: 0, scrollX: 0, width: 10, height: 0 }).endTick).toBe(10 * S);
  });
});

describe('snapTargets', () => {
  const markers: Marker[] = [{ id: 'm', time: 7 * S, label: 'm', color: null }];
  const p = project(
    [track('V0', 'video')],
    [
      clip('a', 'V0', 1, 2),
      clip('b', 'V0', 4, 1, { enabled: false }),
      clip('c', 'V0', 5, 1),
    ],
    { markers, work_area: { start: 2 * S, end: 8 * S } },
  );

  it('lists the timeline start and the playhead first', () => {
    const t = snapTargets(p, [], 3 * S);
    expect(t[0]).toEqual({ time: 0, kind: 'start' });
    expect(t[1]).toEqual({ time: 3 * S, kind: 'playhead' });
  });
  it('includes edges of enabled clips, markers and work area ends', () => {
    const kinds = snapTargets(p, ['c'], 0).map((x) => x.kind);
    expect(kinds).toContain('clip-start');
    expect(kinds).toContain('clip-end');
    expect(kinds).toContain('marker');
    expect(kinds).toContain('work-area-start');
    expect(kinds).toContain('work-area-end');
  });
  it('skips disabled clips and excluded clips', () => {
    const times = snapTargets(p, ['c'], 0).filter((x) => x.kind.startsWith('clip')).map((x) => x.time);
    expect(times).toEqual([1 * S, 3 * S]);
  });
  it('omits work area targets when there is no work area', () => {
    const none = project([track('V0', 'video')], [clip('a', 'V0', 1, 2)]);
    expect(snapTargets(none, [], 0).some((x) => x.kind.startsWith('work-area'))).toBe(false);
  });
});

describe('hitTest edge zone width', () => {
  // A 16 px clip: the edge zone is a quarter of the width (4 px), not the full EDGE_HIT_PX.
  const narrow = project([track('V0', 'video')], [clip('N', 'V0', 2, 0.16)]);
  const rows = trackLayout(narrow, 0);
  const n = narrow.clips[0]!;
  const x0 = tickToX(n.start, VIEW);
  const x1 = tickToX(n.start + n.dur, VIEW);

  it('a narrow clip is about 16 px wide in this view', () => {
    expect(x1 - x0).toBeCloseTo(16, 0);
  });
  it('a narrow clip edge zone is a quarter of its width', () => {
    expect(hitTest(narrow, rows, VIEW, x0 + 3, 56)).toMatchObject({ kind: 'edge-start', clipId: 'N' });
    expect(hitTest(narrow, rows, VIEW, x0 + 6, 56)).toMatchObject({ kind: 'clip', clipId: 'N' });
  });
  it('a narrow clip end edge zone is also a quarter of its width', () => {
    expect(hitTest(narrow, rows, VIEW, x1 - 3, 56)).toMatchObject({ kind: 'edge-end', clipId: 'N' });
    expect(hitTest(narrow, rows, VIEW, x1 - 6, 56)).toMatchObject({ kind: 'clip', clipId: 'N' });
  });
  it('a wide clip keeps the full EDGE_HIT_PX zone', () => {
    const wide = project([track('V0', 'video')], [clip('W', 'V0', 0, 10)]);
    const r = trackLayout(wide, 0);
    expect(hitTest(wide, r, VIEW, EDGE_HIT_PX, 56)).toMatchObject({ kind: 'edge-start', clipId: 'W' });
    expect(hitTest(wide, r, VIEW, EDGE_HIT_PX + 1, 56)).toMatchObject({ kind: 'clip', clipId: 'W' });
  });
  it('a zero-width clip still reports an edge at its position', () => {
    const zero = project([track('V0', 'video')], [clip('Z', 'V0', 2, 0)]);
    const r = trackLayout(zero, 0);
    const h = hitTest(zero, r, VIEW, tickToX(zero.clips[0]!.start, VIEW), 56);
    expect(h.kind === 'edge-start' || h.kind === 'edge-end').toBe(true);
    expect(h).toMatchObject({ clipId: 'Z' });
  });
});

describe('hitEnvelopePoint', () => {
  // Audio track row: y 28..84 (height 56 >= ENVELOPE_MIN_HEIGHT_PX). Clip at 1 s..3 s = px 100..300.
  const audioTrack = track('A0', 'audio');
  const withEnv = (envelope: { t: number; db: number }[], gain_db = 0): Clip =>
    clip('E', 'A0', 1, 2, {
      kind: 'audio',
      audio: { ...defaultClipAudio(), envelope: envelope.map((p) => ({ t: Math.round(p.t * S), db: p.db })), gain_db },
    });
  const build = (c: Clip) => {
    const proj = project([audioTrack], [c]);
    return { proj, rows: trackLayout(proj, 0) };
  };

  it('exports the envelope hit distance and minimum height constants', () => {
    expect(ENVELOPE_HIT_PX).toBe(6);
    expect(ENVELOPE_MIN_HEIGHT_PX).toBe(40);
  });

  it('a pointer on an envelope point returns that point index', () => {
    const c = withEnv([{ t: 0, db: 0 }, { t: 1, db: -6 }]);
    const { proj, rows } = build(c);
    const rect = clipRect(c, rows, VIEW)!;
    const hit = hitEnvelopePoint(proj, rows, VIEW, rect.x, envelopeY(0, rect));
    expect(hit?.clipId).toBe('E');
    expect(hit?.index).toBe(0);
    expect(hit?.db).toBe(0);
  });

  it('a point hit reports the absolute tick of the point', () => {
    const c = withEnv([{ t: 0, db: 0 }, { t: 1, db: -6 }]);
    const { proj, rows } = build(c);
    const rect = clipRect(c, rows, VIEW)!;
    const hit = hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, envelopeY(-6, rect));
    expect(hit?.index).toBe(1);
    expect(hit?.tick).toBe(c.start + S);
  });

  it('a pointer near the gain line of a clip with no points returns index null', () => {
    const c = withEnv([], -6);
    const { proj, rows } = build(c);
    const rect = clipRect(c, rows, VIEW)!;
    const hit = hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, envelopeY(-6, rect) + 2);
    expect(hit).not.toBeNull();
    expect(hit?.index).toBeNull();
  });

  it('the gain line hit reports the tick under the pointer and the dB at that height', () => {
    const c = withEnv([], 0);
    const { proj, rows } = build(c);
    const rect = clipRect(c, rows, VIEW)!;
    const hit = hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, envelopeY(0, rect));
    expect(hit?.index).toBeNull();
    expect(hit?.tick).toBe(c.start + Math.round(c.dur / 2));
    expect(hit?.db).toBeCloseTo(0, 5);
  });

  it('a pointer far from points and lines returns null', () => {
    const c = withEnv([{ t: 0, db: 0 }]);
    const { proj, rows } = build(c);
    const rect = clipRect(c, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, rect.y + rect.h - 1)).toBeNull();
  });

  it('a pointer outside the clip horizontally (beyond the hit margin) returns null', () => {
    const c = withEnv([], 0);
    const { proj, rows } = build(c);
    const rect = clipRect(c, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x - ENVELOPE_HIT_PX - 5, envelopeY(0, rect))).toBeNull();
  });

  it('ignores video clips', () => {
    const c = clip('V', 'V0', 1, 2, { kind: 'video' });
    const proj = project([track('V0', 'video')], [c]);
    const rows = trackLayout(proj, 0);
    const rect = clipRect(c, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + 10, envelopeY(0, rect))).toBeNull();
  });

  it('ignores clips on a locked track', () => {
    const c = withEnv([], 0);
    const proj = project([track('A0', 'audio', { locked: true })], [c]);
    const rows = trackLayout(proj, 0);
    const rect = clipRect(c, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + 10, envelopeY(0, rect))).toBeNull();
  });

  it('ignores clips on a track too short for an envelope', () => {
    const c = withEnv([], 0);
    const proj = project([track('A0', 'audio', { height: 30 })], [c]);
    const rows = trackLayout(proj, 0);
    const rect = clipRect(c, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + 10, envelopeY(0, rect))).toBeNull();
  });

  it('returns the first hit in project order', () => {
    const a = clip('A', 'A0', 1, 2, { kind: 'audio', audio: { ...defaultClipAudio(), gain_db: 0 } });
    const b = clip('B', 'A0', 1, 2, { kind: 'audio', audio: { ...defaultClipAudio(), gain_db: 0 } });
    const proj = project([audioTrack], [a, b]);
    const rows = trackLayout(proj, 0);
    const rect = clipRect(a, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + 10, envelopeY(0, rect))?.clipId).toBe('A');
  });

  it('ignores zero-duration audio clips', () => {
    const c = clip('Z', 'A0', 1, 0, { kind: 'audio' });
    const proj = project([audioTrack], [c]);
    const rows = trackLayout(proj, 0);
    const rect = clipRect(c, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x, rect.y + rect.h / 2)).toBeNull();
  });
});

describe('envelope geometry helpers', () => {
  const rect = { x: 0, y: 10, w: 100, h: 72 };

  it('envelopeY maps 12 dB to the top and -60 dB to the bottom', () => {
    expect(envelopeY(12, rect)).toBeCloseTo(10, 5);
    expect(envelopeY(-60, rect)).toBeCloseTo(82, 5);
  });
  it('envelopeY clamps out-of-range gains', () => {
    expect(envelopeY(40, rect)).toBeCloseTo(10, 5);
    expect(envelopeY(-200, rect)).toBeCloseTo(82, 5);
  });
  it('envelopeY treats a non-finite gain as 0 dB', () => {
    expect(envelopeY(Number.NaN, rect)).toBeCloseTo(envelopeY(0, rect), 5);
  });
  it('envelopeDbAt inverts envelopeY inside the clip', () => {
    for (const db of [-40, -6, 0, 6]) {
      expect(envelopeDbAt(envelopeY(db, rect), rect)).toBeCloseTo(db, 5);
    }
  });
  it('envelopeDbAt clamps to the dB range outside the rectangle', () => {
    expect(envelopeDbAt(-1000, rect)).toBe(12);
    expect(envelopeDbAt(1000, rect)).toBe(-60);
  });
  it('envelopeDbAt returns the minimum for a zero-height rectangle', () => {
    expect(envelopeDbAt(5, { x: 0, y: 0, w: 10, h: 0 })).toBe(-60);
  });
});

describe('hitTest uses the drawn width of clips narrower than the minimum', () => {
  // 0.012 s at 100 px/s is 1.2 px of time; the clip is drawn 2 px wide, so the pointer must hit it there.
  const tiny = project([track('V0', 'video')], [clip('T', 'V0', 2, 0.012)]);
  const rows = trackLayout(tiny, 0);
  const x0 = tickToX(tiny.clips[0]!.start, VIEW);

  it('the tiny clip is drawn 2 px wide', () => {
    expect(clipRect(tiny.clips[0]!, rows, VIEW)?.w).toBe(2);
  });
  it('a pointer on the drawn right half of a tiny clip hits that clip, not empty track', () => {
    const h = hitTest(tiny, rows, VIEW, x0 + 1.9, 56);
    expect(h.clipId).toBe('T');
    expect(h.kind === 'clip' || h.kind === 'edge-end').toBe(true);
  });
});

describe('hitEnvelopePoint with a selection', () => {
  // Flat gain line (no points) at 0 dB on an audio clip at 1 s..3 s, drawn 200 px wide.
  const audioTrack = track('A0', 'audio');
  const flat = clip('E', 'A0', 1, 2, { kind: 'audio', audio: { ...defaultClipAudio(), gain_db: 0 } });
  const withPoint = clip('E', 'A0', 1, 2, {
    kind: 'audio',
    audio: { ...defaultClipAudio(), envelope: [{ t: 0, db: 0 }] },
  });
  const build = (c: Clip) => {
    const proj = project([audioTrack], [c]);
    return { proj, rows: trackLayout(proj, 0) };
  };

  it('a gain-line hit on an unselected clip is ignored when a selection is given', () => {
    const { proj, rows } = build(flat);
    const rect = clipRect(flat, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, envelopeY(0, rect), [])).toBeNull();
  });
  it('a gain-line hit on a selected clip still counts', () => {
    const { proj, rows } = build(flat);
    const rect = clipRect(flat, rows, VIEW)!;
    const hit = hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, envelopeY(0, rect), ['E']);
    expect(hit?.clipId).toBe('E');
    expect(hit?.index).toBeNull();
  });
  it('a point hit counts on an unselected clip', () => {
    const { proj, rows } = build(withPoint);
    const rect = clipRect(withPoint, rows, VIEW)!;
    const hit = hitEnvelopePoint(proj, rows, VIEW, rect.x, envelopeY(0, rect), []);
    expect(hit?.index).toBe(0);
  });
  it('without a selection argument the gain line counts as before', () => {
    const { proj, rows } = build(flat);
    const rect = clipRect(flat, rows, VIEW)!;
    expect(hitEnvelopePoint(proj, rows, VIEW, rect.x + rect.w / 2, envelopeY(0, rect))?.index).toBeNull();
  });
});

describe('cut tool snapping and preview', () => {
  const CUT_VIEW: View = { pxPerSec: 100, scrollX: 0, width: 1000, height: 300 };
  // Clip a spans 0..5 s (track area x 0..500). Its end is the only clip boundary nearby in these tests.
  const cutProj = (playSec = 0): { proj: Project; rows: TrackRow[]; play: number } => {
    const proj = project([track('v1', 'video')], [clip('a', 'v1', 0, 5)]);
    return { proj, rows: trackLayout(proj, 0), play: Math.round(playSec * S) };
  };

  it('snaps to a clip end within the threshold', () => {
    const { proj, play } = cutProj(0);
    const cut = cutSnapPoint(proj, 503, false, play, CUT_VIEW, 8);
    expect(cut.kind).toBe('clip');
    expect(cut.tick).toBe(5 * S);
    expect(cut.x).toBeCloseTo(500, 6);
  });

  it('snaps to the playhead when it is nearer than a clip boundary', () => {
    const { proj, play } = cutProj(4.99);
    const cut = cutSnapPoint(proj, 497, false, play, CUT_VIEW, 8);
    expect(cut.kind).toBe('playhead');
    expect(cut.tick).toBe(play);
    expect(cut.x).toBeCloseTo(499, 6);
  });

  it('does not snap beyond the threshold', () => {
    const { proj, play } = cutProj(0);
    const cut = cutSnapPoint(proj, 520, false, play, CUT_VIEW, 8);
    expect(cut.kind).toBeNull();
    expect(cut.tick).toBe(xToTick(520, CUT_VIEW));
    expect(cut.x).toBeCloseTo(520, 6);
  });

  it('Shift ignores snapping', () => {
    const { proj, play } = cutProj(0);
    const cut = cutSnapPoint(proj, 503, true, play, CUT_VIEW, 8);
    expect(cut.kind).toBeNull();
    expect(cut.tick).toBe(xToTick(503, CUT_VIEW));
  });

  it('preview is shown only over a clip, with the snapped point and a canvas-rate label', () => {
    const { proj, rows, play } = cutProj(0);
    const row = rows[0]!;
    const over = cutPreviewFor(proj, rows, CUT_VIEW, 250, row.top + 5, false, play, 8);
    expect(over).not.toBeNull();
    expect(over!.snapped).toBe(false);
    expect(over!.kind).toBeNull();
    expect(over!.label).toBe('00:00:02:15');
    const off = cutPreviewFor(proj, rows, CUT_VIEW, 900, row.top + 5, false, play, 8);
    expect(off).toBeNull();
  });

  it('over a clip end edge zone the preview snaps to that existing boundary', () => {
    const { proj, rows, play } = cutProj(0);
    const row = rows[0]!;
    // The end edge is at x 500; the pointer is 4 px inside the edge zone (EDGE_HIT_PX = 10).
    const edge = cutPreviewFor(proj, rows, CUT_VIEW, 504, row.top + 5, false, play, 8);
    expect(edge).not.toBeNull();
    expect(edge!.tick).toBe(5 * S);
    expect(edge!.x).toBeCloseTo(500, 6);
    expect(edge!.snapped).toBe(true);
    expect(edge!.kind).toBe('clip');
    expect(edge!.label).toBe(cutPreviewLabel(5 * S, proj));
  });

  it('over a clip start edge zone the preview snaps to that existing boundary', () => {
    const { proj, rows, play } = cutProj(0);
    const row = rows[0]!;
    const edge = cutPreviewFor(proj, rows, CUT_VIEW, 4, row.top + 5, false, play, 8);
    expect(edge).not.toBeNull();
    expect(edge!.tick).toBe(0);
    expect(edge!.x).toBeCloseTo(0, 6);
    expect(edge!.snapped).toBe(true);
    expect(edge!.kind).toBe('clip');
  });

  it('Shift over an edge zone keeps the preview at the raw pointer position, unsnapped', () => {
    const { proj, rows, play } = cutProj(0);
    const row = rows[0]!;
    const edge = cutPreviewFor(proj, rows, CUT_VIEW, 504, row.top + 5, true, play, 8);
    expect(edge).not.toBeNull();
    expect(edge!.tick).toBe(xToTick(504, CUT_VIEW));
    expect(edge!.x).toBeCloseTo(504, 6);
    expect(edge!.snapped).toBe(false);
    expect(edge!.kind).toBeNull();
  });

  it('labels a cut point as HH:MM:SS:FF at the canvas frame rate', () => {
    const { proj } = cutProj(0);
    expect(cutPreviewLabel(Math.round(1.5 * S), proj)).toBe('00:00:01:15');
  });

  it('label is empty instead of throwing when the frame rate is missing', () => {
    const { proj } = cutProj(0);
    const broken = { ...proj, canvas: { ...proj.canvas, fps: undefined as unknown as { num: number; den: number } } };
    expect(cutPreviewLabel(Math.round(1.5 * S), broken)).toBe('');
  });
});
