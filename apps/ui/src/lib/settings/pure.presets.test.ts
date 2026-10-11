import { describe, expect, it } from 'vitest';
import type { DiagnosticDto } from '../daemon';
import { diagnosticAt, estimateBufferMB, formatSpanSeconds, formatMB } from './pure';

const diag = (path: string, severity: 'error' | 'warning', code = 'x.code'): DiagnosticDto => ({
  code,
  severity,
  path,
  message: `${severity} at ${path}`,
});

describe('estimateBufferMB', () => {
  it.each<[string, number, number, number[], number]>([
    ['spec example: 12000 video + two 192k tracks, 300 s', 300, 12000, [192, 192], 454],
    ['nothing recorded', 300, 0, [], 0],
    ['video only, 6000 kbps, 60 s', 60, 6000, [], 44],
    ['video only, 20000 kbps, 10 s', 10, 20000, [], 24],
    ['video only, 40000 kbps, 600 s', 600, 40000, [], 2930],
    ['zero length', 0, 12000, [192], 0],
    ['one second of 1000 kbps is under 1 MB', 1, 1000, [], 0],
  ])('%s', (_label, seconds, video, audio, expected) => {
    expect(estimateBufferMB(seconds, video, audio)).toBe(expected);
  });

  it('one 128k track for 60 s rounds 0.9375 MB up to 1', () => {
    expect(estimateBufferMB(60, 0, [128])).toBe(1);
  });

  it('doubling the length roughly doubles the estimate', () => {
    const one = estimateBufferMB(300, 12000, [192]);
    const two = estimateBufferMB(600, 12000, [192]);
    expect(Math.abs(two - one * 2)).toBeLessThanOrEqual(1);
  });

  it('sums every audio track', () => {
    expect(estimateBufferMB(100, 0, [1024, 1024])).toBe(estimateBufferMB(100, 2048, []));
  });
});

describe('formatMB', () => {
  it.each([
    [0, '≈ 0 MB'],
    [1, '≈ 1 MB'],
    [450, '≈ 450 MB'],
    [1023, '≈ 1023 MB'],
    [1024, '≈ 1.0 GB'],
    [1536, '≈ 1.5 GB'],
    [2048, '≈ 2.0 GB'],
    [2930, '≈ 2.9 GB'],
  ])('%s MB -> %s', (mb, expected) => {
    expect(formatMB(mb)).toBe(expected);
  });
});

describe('formatSpanSeconds', () => {
  it.each([
    [0, '0 s'],
    [45, '45 s'],
    [59, '59 s'],
    [60, '1 min'],
    [61, '1 min 1 s'],
    [119, '1 min 59 s'],
    [150, '2 min 30 s'],
    [300, '5 min'],
    [3600, '60 min'],
    [-5, '0 s'],
    [45.4, '45 s'],
    [45.6, '46 s'],
  ])('%s -> %s', (seconds, expected) => {
    expect(formatSpanSeconds(seconds)).toBe(expected);
  });
});

describe('diagnosticAt (exact path only)', () => {
  it('matches the exact path', () => {
    expect(diagnosticAt('audio.tracks', [diag('audio.tracks', 'error', 'audio.count')])).toEqual({
      code: 'audio.count',
      severity: 'error',
      message: 'error at audio.tracks',
    });
  });

  it('does not match a row below the list', () => {
    expect(diagnosticAt('audio.tracks', [diag('audio.tracks[0].name', 'error')])).toBeNull();
  });

  it('does not match an indexed row for its parent path', () => {
    expect(diagnosticAt('a.b', [diag('a.b[0]', 'error')])).toBeNull();
  });

  it('does not match a child key', () => {
    expect(diagnosticAt('a.b', [diag('a.b.c', 'error')])).toBeNull();
  });

  it('does not match a sibling that shares a prefix', () => {
    expect(diagnosticAt('a.b', [diag('a.bc', 'error')])).toBeNull();
  });

  it('error beats warning on the same path, regardless of order', () => {
    const list = [diag('x', 'warning', 'w'), diag('x', 'error', 'e')];
    expect(diagnosticAt('x', list)?.severity).toBe('error');
    expect(diagnosticAt('x', [...list].reverse())?.severity).toBe('error');
  });

  it('a warning alone is returned as a warning', () => {
    expect(diagnosticAt('x', [diag('x', 'warning', 'w')])?.severity).toBe('warning');
  });

  it('returns null for an empty list', () => {
    expect(diagnosticAt('x', [])).toBeNull();
  });

  it('picks only the matching entry from a mixed list', () => {
    const list = [diag('hotkeys.clips[1].keys', 'error', 'k'), diag('hotkeys.clips', 'warning', 'c')];
    expect(diagnosticAt('hotkeys.clips', list)?.code).toBe('c');
  });
});
