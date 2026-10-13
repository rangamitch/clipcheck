import { describe, expect, it } from 'vitest';
import {
  DEBUG_STORAGE_KEY,
  bufferedAhead,
  defaultDebugFlags,
  formatDiagLine,
  loadDebugFlags,
  ratePerSec,
  saveDebugFlags,
} from './debug';

function memory(initial?: string): { store: Storage; data: Map<string, string> } {
  const data = new Map<string, string>();
  if (initial !== undefined) data.set(DEBUG_STORAGE_KEY, initial);
  const store = {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
  } as unknown as Storage;
  return { store, data };
}

const throwing = {
  getItem: () => {
    throw new Error('blocked');
  },
  setItem: () => {
    throw new Error('blocked');
  },
} as unknown as Storage;

describe('debug flags', () => {
  it('defaults to all on', () => {
    expect(defaultDebugFlags()).toEqual({ multiAudio: true, webAudio: true, direct: true });
    expect(loadDebugFlags(memory().store)).toEqual(defaultDebugFlags());
  });

  it('round-trips through storage', () => {
    const m = memory();
    const flags = { multiAudio: false, webAudio: true, direct: false };
    expect(saveDebugFlags(flags, m.store)).toBe(true);
    expect(loadDebugFlags(m.store)).toEqual(flags);
  });

  it('survives storage that throws', () => {
    expect(loadDebugFlags(throwing)).toEqual(defaultDebugFlags());
    expect(saveDebugFlags(defaultDebugFlags(), throwing)).toBe(false);
  });

  it('ignores malformed or partial data', () => {
    expect(loadDebugFlags(memory('not json').store)).toEqual(defaultDebugFlags());
    expect(loadDebugFlags(memory('42').store)).toEqual(defaultDebugFlags());
    expect(loadDebugFlags(memory('{"webAudio":false,"direct":"no"}').store)).toEqual({
      multiAudio: true,
      webAudio: false,
      direct: true,
    });
  });

  it('handles missing storage', () => {
    expect(loadDebugFlags(null)).toEqual(defaultDebugFlags());
    expect(saveDebugFlags(defaultDebugFlags(), null)).toBe(false);
  });
});

describe('diagnostic helpers', () => {
  it('computes per-second rates', () => {
    expect(ratePerSec(60, 1000)).toBe(60);
    expect(ratePerSec(30, 500)).toBe(60);
    expect(ratePerSec(-5, 1000)).toBe(0);
    expect(ratePerSec(10, 0)).toBe(0);
    expect(ratePerSec(Number.NaN, 1000)).toBe(0);
  });

  it('measures buffered time ahead', () => {
    const r = { length: 2, start: (i: number) => [0, 10][i] ?? 0, end: (i: number) => [5, 20][i] ?? 0 };
    expect(bufferedAhead(r, 2)).toBe(3);
    expect(bufferedAhead(r, 12.34)).toBe(7.7);
    expect(bufferedAhead(r, 7)).toBe(0);
    expect(bufferedAhead({ length: 0, start: () => 0, end: () => 0 }, 1)).toBe(0);
  });

  it('formats the line', () => {
    const line = formatDiagLine({
      paintFps: 60,
      videoFps: 58,
      dropped: 0,
      ctxState: 'running',
      baseLatencyMs: 10,
      outputLatencyMs: 35,
      rs: 4,
      ns: 2,
      aheadS: 3.2,
      decodedFps: 58,
      droppedFrames: 0,
      waiting: 0,
      stalled: 0,
      drift: 0.01,
      audioEls: 6,
      audioPlaying: 6,
      longTasks: 3,
      longTaskMaxMs: 90,
      mode: 'canvas',
    });
    expect(line).toBe(
      'ctx running lat 10/35ms | v rs4 ns2 ahead 3.2s dec 58/s drop 0 wait 0 stall 0 drift +0.01 | a 6/6 | long 3 max 90ms | canvas',
    );
  });

  it('formats a bare stats object', () => {
    expect(formatDiagLine({ paintFps: 0, videoFps: 0, dropped: 0 })).toBe(
      'ctx none | v none | a 0/0 | long 0 max 0ms | canvas',
    );
    expect(formatDiagLine({ paintFps: 0, videoFps: 0, dropped: 0, rs: 1, drift: -0.5 })).toContain('drift -0.50');
  });
});
