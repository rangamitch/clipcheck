import { describe, expect, it } from 'vitest';
import { columnPeak, pathSpan, waveformGridXs } from './drawClips';

describe('waveformGridXs', () => {
  it('starts each column on a grid anchored at the clip edge, not at the view edge', () => {
    const xs = waveformGridXs(10.3, 0, 30, 2);
    expect(xs.length).toBeGreaterThan(0);
    expect(xs[0]).toBeGreaterThanOrEqual(0);
    expect(xs[0]).toBeLessThan(2);
    for (const x of xs) {
      // Every column sits an exact number of steps from the clip edge.
      expect(((x - 10.3) / 2) % 1).toBeCloseTo(0, 9);
      expect(x).toBeLessThan(30);
    }
  });

  it('keeps the same column positions when the view scrolls by a whole step', () => {
    const a = waveformGridXs(-7.5, 0, 100, 2).map((x) => x - 2);
    const b = waveformGridXs(-5.5, 0, 100, 2);
    expect(b.slice(0, 5)).toEqual(a.slice(1, 6));
  });

  it('returns no columns for an empty or non-finite range', () => {
    expect(waveformGridXs(0, 10, 10, 2)).toEqual([]);
    expect(waveformGridXs(Number.NaN, 0, 10, 2)).toEqual([]);
    expect(waveformGridXs(0, 0, 10, 0)).toEqual([]);
  });
});

describe('columnPeak', () => {
  const peaks = [0, 0, 0.9, 0, 0];

  it('takes the largest bucket covered by a zoomed-out column', () => {
    // One column covering the whole clip sees the 0.9 transient.
    expect(columnPeak(peaks, 0, 5, 0, 1, false)).toBeCloseTo(0.9, 9);
    // A column covering only the first fifth sees nothing.
    expect(columnPeak(peaks, 0, 5, 0, 0.2, false)).toBe(0);
  });

  it('mirrors the range for a reversed clip', () => {
    const rev = [0.9, 0, 0, 0, 0];
    expect(columnPeak(rev, 0, 5, 0, 0.2, true)).toBe(0);
    expect(columnPeak(rev, 0, 5, 0.8, 1, true)).toBeCloseTo(0.9, 9);
  });

  it('only reads buckets inside the clip source range', () => {
    // Source range is buckets 2..4 of the file; bucket 0 is outside it.
    expect(columnPeak([0.9, 0, 0.5, 0, 0], 2, 5, 0, 1, false)).toBe(0.5);
  });

  it('returns 0 for an empty range and clamps values to 0..1', () => {
    expect(columnPeak([], 0, 0, 0, 1, false)).toBe(0);
    expect(columnPeak([3], 0, 1, 0, 1, false)).toBe(1);
  });
});

describe('pathSpan', () => {
  it('leaves a clip inside the view unchanged, with both ends rounded', () => {
    expect(pathSpan(10, 100, 800)).toEqual({ x: 10, w: 100, left: true, right: true });
  });

  it('clamps a very wide clip to just past the view and squares the off-screen ends', () => {
    const span = pathSpan(-5.8e7, 1.2e8, 800);
    expect(span.x).toBe(-64);
    expect(span.w).toBe(800 + 128);
    expect(span.left).toBe(false);
    expect(span.right).toBe(false);
  });

  it('rounds only the end that is on screen', () => {
    const span = pathSpan(700, 200, 800);
    expect(span.x).toBe(700);
    expect(span.left).toBe(true);
    expect(span.right).toBe(false);
  });
});
