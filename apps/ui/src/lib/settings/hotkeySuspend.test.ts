import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { beginHotkeyCapture, createHotkeySuspender, HOTKEY_SUSPEND_KEEPALIVE_MS } from './hotkeySuspend';

/** A sender that records every call, plus the fake interval timers that drive the keepalive. */
function harness() {
  const sent: boolean[] = [];
  const send = vi.fn((suspended: boolean) => {
    sent.push(suspended);
    return Promise.resolve({ type: 'ok' });
  });
  const timers = {
    setInterval: (fn: () => void, ms: number) => setInterval(fn, ms),
    clearInterval: (handle: unknown) => clearInterval(handle as ReturnType<typeof setInterval>),
  };
  const suspender = createHotkeySuspender(send, timers);
  return { sent, send, suspender };
}

beforeEach(() => {
  vi.useFakeTimers();
  invokeMock.mockReset();
  invokeMock.mockResolvedValue({ type: 'ok' });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('createHotkeySuspender', () => {
  it('the first begin sends suspended=true', () => {
    const { sent, suspender } = harness();
    suspender.begin();
    expect(sent).toEqual([true]);
    expect(suspender.activeCount()).toBe(1);
  });

  it('a second begin while one is open sends nothing', () => {
    const { sent, suspender } = harness();
    suspender.begin();
    suspender.begin();
    expect(sent).toEqual([true]);
    expect(suspender.activeCount()).toBe(2);
  });

  it('only the last release sends suspended=false', () => {
    const { sent, suspender } = harness();
    const first = suspender.begin();
    const second = suspender.begin();
    first();
    expect(sent).toEqual([true]);
    second();
    expect(sent).toEqual([true, false]);
    expect(suspender.activeCount()).toBe(0);
  });

  it('releases are idempotent', () => {
    const { sent, suspender } = harness();
    const first = suspender.begin();
    const second = suspender.begin();
    first();
    first();
    first();
    expect(suspender.activeCount()).toBe(1);
    expect(sent).toEqual([true]);
    second();
    second();
    expect(sent).toEqual([true, false]);
    expect(suspender.activeCount()).toBe(0);
  });

  it('a single capture releases to false exactly once', () => {
    const { sent, suspender } = harness();
    const release = suspender.begin();
    release();
    release();
    expect(sent).toEqual([true, false]);
  });

  it('a new begin after everything was released sends true again', () => {
    const { sent, suspender } = harness();
    suspender.begin()();
    suspender.begin();
    expect(sent).toEqual([true, false, true]);
  });

  it('the keepalive re-sends true every 20 s while a capture is open', () => {
    const { sent, suspender } = harness();
    suspender.begin();
    expect(sent).toEqual([true]);
    vi.advanceTimersByTime(HOTKEY_SUSPEND_KEEPALIVE_MS - 1);
    expect(sent).toEqual([true]);
    vi.advanceTimersByTime(1);
    expect(sent).toEqual([true, true]);
    vi.advanceTimersByTime(HOTKEY_SUSPEND_KEEPALIVE_MS * 2);
    expect(sent).toEqual([true, true, true, true]);
  });

  it('the keepalive stops on the last release, and no further true is sent', () => {
    const { sent, suspender } = harness();
    const release = suspender.begin();
    vi.advanceTimersByTime(HOTKEY_SUSPEND_KEEPALIVE_MS);
    release();
    expect(sent).toEqual([true, true, false]);
    vi.advanceTimersByTime(HOTKEY_SUSPEND_KEEPALIVE_MS * 5);
    expect(sent).toEqual([true, true, false]);
  });

  it('a rejected send does not throw and does not stop the keepalive', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const send = vi.fn(() => Promise.reject(new Error('daemon offline')));
    const suspender = createHotkeySuspender(send, {
      setInterval: (fn, ms) => setInterval(fn, ms),
      clearInterval: (h) => clearInterval(h as ReturnType<typeof setInterval>),
    });
    expect(() => suspender.begin()).not.toThrow();
    expect(() => vi.advanceTimersByTime(HOTKEY_SUSPEND_KEEPALIVE_MS)).not.toThrow();
    expect(send).toHaveBeenCalledTimes(2);
    // The rejection handler runs in a microtask, so let the pending promises settle first.
    for (let i = 0; i < 5; i += 1) await Promise.resolve();
    expect(warn).toHaveBeenCalledTimes(2);
  });

  it('a synchronous throw from the sender does not escape begin or release', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const suspender = createHotkeySuspender(
      () => {
        throw new Error('bridge gone');
      },
      {
        setInterval: (fn, ms) => setInterval(fn, ms),
        clearInterval: (h) => clearInterval(h as ReturnType<typeof setInterval>),
      },
    );
    let release: (() => void) | undefined;
    expect(() => {
      release = suspender.begin();
    }).not.toThrow();
    expect(() => release?.()).not.toThrow();
    expect(warn).toHaveBeenCalledTimes(2);
  });
});

describe('beginHotkeyCapture (daemon wiring)', () => {
  it('outside Tauri it is a no-op: no invoke, no throw', () => {
    const release = beginHotkeyCapture();
    expect(() => release()).not.toThrow();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('inside Tauri it sends set_hotkeys_suspended through daemon_request', async () => {
    const g = globalThis as { __TAURI_INTERNALS__?: unknown };
    g.__TAURI_INTERNALS__ = {};
    try {
      const release = beginHotkeyCapture();
      release();
      await vi.runOnlyPendingTimersAsync();
      expect(invokeMock).toHaveBeenNthCalledWith(1, 'daemon_request', {
        command: { type: 'set_hotkeys_suspended', data: { suspended: true } },
      });
      expect(invokeMock).toHaveBeenNthCalledWith(2, 'daemon_request', {
        command: { type: 'set_hotkeys_suspended', data: { suspended: false } },
      });
    } finally {
      delete g.__TAURI_INTERNALS__;
    }
  });
});
