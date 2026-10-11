import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { debounce } from './debounce';

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('debounce', () => {
  it('runs once, waitMs after a single call', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    d.call();
    vi.advanceTimersByTime(499);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('restarts the wait on each call (trailing edge only)', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    d.call();
    vi.advanceTimersByTime(300);
    d.call();
    vi.advanceTimersByTime(499);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('collapses a burst of calls into one run after the last call', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    for (let i = 0; i < 10; i++) {
      d.call();
      vi.advanceTimersByTime(100);
    }
    // Last call was 100 ms before this point, so the run lands 400 ms later.
    vi.advanceTimersByTime(399);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('two triggers 300 ms apart produce one run 500 ms after the second', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    d.call();
    vi.advanceTimersByTime(300);
    d.call();
    vi.advanceTimersByTime(500);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('pending is true while a run is scheduled and false after it fires', () => {
    const d = debounce(() => {}, 500);
    expect(d.pending).toBe(false);
    d.call();
    expect(d.pending).toBe(true);
    vi.advanceTimersByTime(500);
    expect(d.pending).toBe(false);
  });

  it('cancel drops a pending run', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    d.call();
    d.cancel();
    expect(d.pending).toBe(false);
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
  });

  it('cancel with nothing scheduled is a no-op', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    expect(() => d.cancel()).not.toThrow();
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
  });

  it('can be called again after a run has fired', () => {
    const fn = vi.fn();
    const d = debounce(fn, 500);
    d.call();
    vi.advanceTimersByTime(500);
    d.call();
    vi.advanceTimersByTime(500);
    expect(fn).toHaveBeenCalledTimes(2);
  });
});
