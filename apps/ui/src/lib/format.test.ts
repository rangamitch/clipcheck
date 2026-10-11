import { describe, expect, it } from 'vitest';
import { formatBytes, formatDuration, formatRelativeDate } from './format';

describe('formatBytes', () => {
  it('handles zero, negatives and non-finite input', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(-5)).toBe('0 B');
    expect(formatBytes(Number.NaN)).toBe('0 B');
  });
  it('uses plain bytes below 1 KB', () => {
    expect(formatBytes(512)).toBe('512 B');
  });
  it('uses one decimal below 10 and none from 10 up', () => {
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(10 * 1024)).toBe('10 KB');
    expect(formatBytes(700 * 1024 * 1024)).toBe('700 MB');
  });
  it('reaches GB and TB', () => {
    expect(formatBytes(3 * 1024 ** 3)).toBe('3.0 GB');
    expect(formatBytes(2 * 1024 ** 4)).toBe('2.0 TB');
  });
});

describe('formatDuration', () => {
  it('formats zero and invalid input as 0:00', () => {
    expect(formatDuration(0)).toBe('0:00');
    expect(formatDuration(-1)).toBe('0:00');
  });
  it('formats m:ss and truncates fractions', () => {
    expect(formatDuration(5_000)).toBe('0:05');
    expect(formatDuration(93_000)).toBe('1:33');
    expect(formatDuration(59_999)).toBe('0:59');
  });
  it('switches to h:mm:ss from one hour', () => {
    expect(formatDuration(3_723_000)).toBe('1:02:03');
    expect(formatDuration(3_600_000)).toBe('1:00:00');
  });
});

describe('formatRelativeDate', () => {
  const now = Date.UTC(2026, 9, 9, 12, 0, 0);
  it('reports recent times in minutes and hours', () => {
    expect(formatRelativeDate(now - 30_000, now)).toBe('just now');
    expect(formatRelativeDate(now + 60_000, now)).toBe('just now');
    expect(formatRelativeDate(now - 5 * 60_000, now)).toBe('5 min ago');
    expect(formatRelativeDate(now - 3 * 3_600_000, now)).toBe('3 h ago');
  });
  it('reports days up to a week', () => {
    expect(formatRelativeDate(now - 2 * 86_400_000, now)).toBe('2 d ago');
  });
  it('falls back to a local YYYY-MM-DD date after a week', () => {
    const old = new Date(2026, 0, 3, 10, 0, 0).getTime();
    expect(formatRelativeDate(old, now)).toBe('2026-01-03');
  });
  it('returns an empty string for non-finite timestamps', () => {
    expect(formatRelativeDate(Number.NaN, now)).toBe('');
  });
});
