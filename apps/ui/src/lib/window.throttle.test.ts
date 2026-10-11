import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { win, getCurrentWindowMock } = vi.hoisted(() => {
  const win = {
    isMaximized: vi.fn(async (): Promise<boolean> => false),
    onResized: vi.fn(),
  };
  return { win, getCurrentWindowMock: vi.fn(() => win) };
});

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: getCurrentWindowMock,
}));

import { RESIZE_THROTTLE_MS, onMaximizeChange } from './window';

// Captured resize callback and the unlisten spy handed back by onResized.
let resizeCb: () => Promise<void>;
let unlistenSpy: ReturnType<typeof vi.fn>;

function setTauri(present: boolean) {
  if (present) (globalThis as any).__TAURI_INTERNALS__ = {};
  else delete (globalThis as any).__TAURI_INTERNALS__;
}

beforeEach(() => {
  vi.useFakeTimers();
  setTauri(true);
  win.isMaximized.mockReset();
  win.isMaximized.mockResolvedValue(false);
  unlistenSpy = vi.fn();
  win.onResized.mockReset();
  win.onResized.mockImplementation(async (cb: () => Promise<void>) => {
    resizeCb = cb;
    return unlistenSpy;
  });
});

afterEach(() => {
  vi.useRealTimers();
  setTauri(false);
});

describe('onMaximizeChange throttle (#76)', () => {
  it('uses a 100 ms throttle window', () => {
    expect(RESIZE_THROTTLE_MS).toBe(100);
  });

  it('reads twice for a burst of 20 resize events within 50 ms: leading plus trailing', async () => {
    const handler = vi.fn();
    await onMaximizeChange(handler);

    // 20 events, 2 ms apart (0 to 38 ms).
    for (let i = 0; i < 20; i += 1) {
      await resizeCb();
      await vi.advanceTimersByTimeAsync(2);
    }
    // Only the leading read has happened so far.
    expect(win.isMaximized).toHaveBeenCalledTimes(1);

    // The trailing read runs when the throttle window closes.
    await vi.advanceTimersByTimeAsync(300);
    expect(win.isMaximized).toHaveBeenCalledTimes(2);
    expect(handler).toHaveBeenCalledTimes(2);
  });

  it('reads on every event spaced more than 100 ms apart', async () => {
    const handler = vi.fn();
    await onMaximizeChange(handler);

    await resizeCb();
    expect(win.isMaximized).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(150);

    await resizeCb();
    expect(win.isMaximized).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(150);

    await resizeCb();
    expect(win.isMaximized).toHaveBeenCalledTimes(3);

    await vi.advanceTimersByTimeAsync(1000);
    expect(win.isMaximized).toHaveBeenCalledTimes(3);
    expect(handler).toHaveBeenCalledTimes(3);
  });

  it('the trailing read reports the final maximized state', async () => {
    const handler = vi.fn();
    win.isMaximized.mockReset();
    win.isMaximized.mockResolvedValueOnce(false).mockResolvedValue(true);
    await onMaximizeChange(handler);

    await resizeCb(); // leading read: false
    await vi.advanceTimersByTimeAsync(20);
    await resizeCb(); // pending: the window is now maximized
    await vi.advanceTimersByTimeAsync(300); // trailing read: true

    expect(handler.mock.calls.map((c) => c[0])).toEqual([false, true]);
  });

  it('unlisten stops further reads and cancels a pending trailing read', async () => {
    const handler = vi.fn();
    const unlisten = await onMaximizeChange(handler);

    await resizeCb(); // leading read
    await vi.advanceTimersByTimeAsync(20);
    await resizeCb(); // marks a trailing read as pending
    expect(win.isMaximized).toHaveBeenCalledTimes(1);

    unlisten();
    expect(unlistenSpy).toHaveBeenCalledTimes(1);

    // The pending trailing read must not fire after unlisten.
    await vi.advanceTimersByTimeAsync(500);
    expect(win.isMaximized).toHaveBeenCalledTimes(1);
    expect(handler).toHaveBeenCalledTimes(1);
  });
});
