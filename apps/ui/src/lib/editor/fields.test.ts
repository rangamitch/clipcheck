import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clamp, debounce, formatDb, parseDb, parseDimension, parsePercent, roundEven } from './fields';

describe('clamp', () => {
  it('keeps a value inside the range', () => {
    expect(clamp(5, 0, 10)).toBe(5);
  });
  it('clamps below and above the range', () => {
    expect(clamp(-1, 0, 10)).toBe(0);
    expect(clamp(11, 0, 10)).toBe(10);
  });
  it('returns the bounds unchanged at the edges', () => {
    expect(clamp(0, 0, 10)).toBe(0);
    expect(clamp(10, 0, 10)).toBe(10);
  });
  it('returns the lower bound for NaN and both infinities', () => {
    expect(clamp(Number.NaN, 2, 10)).toBe(2);
    expect(clamp(Number.POSITIVE_INFINITY, 2, 10)).toBe(2);
    expect(clamp(Number.NEGATIVE_INFINITY, 2, 10)).toBe(2);
  });
  it('keeps negative ranges', () => {
    expect(clamp(-50, -20, -5)).toBe(-20);
    expect(clamp(-7, -20, -5)).toBe(-7);
  });
});

describe('roundEven', () => {
  it('leaves even integers unchanged', () => {
    expect(roundEven(4)).toBe(4);
    expect(roundEven(6)).toBe(6);
    expect(roundEven(0)).toBe(0);
  });
  it('rounds odd integers up (half rounds towards positive infinity)', () => {
    expect(roundEven(3)).toBe(4);
    expect(roundEven(5)).toBe(6);
    expect(roundEven(1)).toBe(2);
  });
  it('rounds a value just above an even number down to it', () => {
    expect(roundEven(7.4)).toBe(8);
    expect(roundEven(4.9)).toBe(4);
  });
  it('rounds large values such as a 1081 px dimension to 1082', () => {
    expect(roundEven(1081)).toBe(1082);
  });
  it('rounds negative values with the same half rule', () => {
    expect(roundEven(-3)).toBe(-2);
    expect(roundEven(-2)).toBe(-2);
  });
});

describe('parseDimension', () => {
  it('parses a whole number', () => {
    expect(parseDimension('1920')).toBe(1920);
  });
  it('ignores surrounding whitespace', () => {
    expect(parseDimension(' 1080 ')).toBe(1080);
  });
  it('accepts zero (range checks are the caller job)', () => {
    expect(parseDimension('0')).toBe(0);
  });
  it.each([
    ['empty text', ''],
    ['decimal', '12.5'],
    ['negative', '-5'],
    ['letters', 'abc'],
    ['exponent form', '1e3'],
    ['inner space', '19 20'],
    ['plus sign', '+1080'],
  ])('rejects %s', (_label, text) => {
    expect(parseDimension(text)).toBeNull();
  });
  it('rejects integers beyond the safe range', () => {
    expect(parseDimension('99999999999999999999')).toBeNull();
  });
});

describe('parseDb', () => {
  it('parses a plain negative value', () => {
    expect(parseDb('-6')).toBe(-6);
  });
  it('strips a trailing dB unit', () => {
    expect(parseDb('-6.5 dB')).toBe(-6.5);
  });
  it('accepts a signed value without a space before the unit', () => {
    expect(parseDb('+3dB')).toBe(3);
  });
  it('accepts the unit in lowercase', () => {
    expect(parseDb('6 db')).toBe(6);
  });
  it('accepts zero and surrounding whitespace', () => {
    expect(parseDb(' 0 ')).toBe(0);
  });
  it.each([
    ['empty text', ''],
    ['words', 'loud'],
    ['sign only', '-'],
    ['two decimal points', '1.2.3'],
    ['unit only', 'dB'],
    ['infinity word', 'Infinity'],
    ['a trailing dot', '3.'],
  ])('rejects %s', (_label, text) => {
    expect(parseDb(text)).toBeNull();
  });
});

describe('formatDb', () => {
  it('shows a negative value with one decimal and no plus sign', () => {
    expect(formatDb(-6)).toBe('-6.0 dB');
  });
  it('shows a positive value with a plus sign', () => {
    expect(formatDb(3)).toBe('+3.0 dB');
  });
  it('shows zero without a sign', () => {
    expect(formatDb(0)).toBe('0.0 dB');
  });
  it('shows a tiny negative value as zero without a sign', () => {
    expect(formatDb(-0.04)).toBe('0.0 dB');
  });
  it('rounds to one decimal', () => {
    expect(formatDb(-6.04)).toBe('-6.0 dB');
    expect(formatDb(2.36)).toBe('+2.4 dB');
  });
  it('shows NaN and infinities as 0.0 dB', () => {
    expect(formatDb(Number.NaN)).toBe('0.0 dB');
    expect(formatDb(Number.POSITIVE_INFINITY)).toBe('0.0 dB');
  });
  it('round trips through parseDb for values at one decimal', () => {
    expect(parseDb(formatDb(-12.5))).toBe(-12.5);
  });
});

describe('parsePercent', () => {
  it('parses a bare number', () => {
    expect(parsePercent('50')).toBe(50);
  });
  it('strips a trailing percent sign', () => {
    expect(parsePercent('50%')).toBe(50);
  });
  it('allows a space before the percent sign and decimals', () => {
    expect(parsePercent('12.5 %')).toBe(12.5);
  });
  it('keeps the sign', () => {
    expect(parsePercent('-3%')).toBe(-3);
    expect(parsePercent('+10')).toBe(10);
  });
  it('does not enforce a 0..100 range', () => {
    expect(parsePercent('250')).toBe(250);
  });
  it.each([
    ['empty text', ''],
    ['percent sign only', '%'],
    ['letters', 'abc'],
    ['comma decimal', '1,5'],
    ['trailing dot', '3.'],
  ])('rejects %s', (_label, text) => {
    expect(parsePercent(text)).toBeNull();
  });
});

describe('debounce', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('runs once, after the delay, with the arguments of the call', () => {
    const fn = vi.fn<(a: number, b: string) => void>();
    const d = debounce(fn, 100);
    d.call(1, 'x');
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(99);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith(1, 'x');
  });

  it('collapses repeated calls into one run with the last arguments', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(1);
    vi.advanceTimersByTime(50);
    d.call(2);
    vi.advanceTimersByTime(50);
    d.call(3);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith(3);
  });

  it('restarts the wait on each call', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(1);
    vi.advanceTimersByTime(80);
    d.call(2);
    vi.advanceTimersByTime(80);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(20);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith(2);
  });

  it('runs again for a call made after the previous run finished', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 10);
    d.call(1);
    vi.advanceTimersByTime(10);
    d.call(2);
    vi.advanceTimersByTime(10);
    expect(fn.mock.calls).toEqual([[1], [2]]);
  });

  it('flush runs the pending call immediately', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 1000);
    d.call(7);
    d.flush();
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith(7);
  });

  it('flush does not run the call a second time when the timer would fire', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(7);
    d.flush();
    vi.advanceTimersByTime(500);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('flush with nothing pending does nothing', () => {
    const fn = vi.fn<() => void>();
    const d = debounce(fn, 100);
    d.flush();
    expect(fn).not.toHaveBeenCalled();
  });

  it('flush twice runs the call only once', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(1);
    d.flush();
    d.flush();
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('cancel drops the pending call', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(1);
    d.cancel();
    vi.advanceTimersByTime(500);
    expect(fn).not.toHaveBeenCalled();
  });

  it('cancel followed by flush runs nothing', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(1);
    d.cancel();
    d.flush();
    expect(fn).not.toHaveBeenCalled();
  });

  it('accepts new calls after a cancel', () => {
    const fn = vi.fn<(v: number) => void>();
    const d = debounce(fn, 100);
    d.call(1);
    d.cancel();
    d.call(2);
    vi.advanceTimersByTime(100);
    expect(fn.mock.calls).toEqual([[2]]);
  });

  it('keeps the wrapped function a no-op when cancelled before any call', () => {
    const fn = vi.fn<() => void>();
    const d = debounce(fn, 5);
    d.cancel();
    vi.advanceTimersByTime(10);
    expect(fn).not.toHaveBeenCalled();
  });
});
