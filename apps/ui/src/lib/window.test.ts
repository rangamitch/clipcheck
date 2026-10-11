import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { win, getCurrentWindowMock } = vi.hoisted(() => {
  const win = {
    minimize: vi.fn(async () => {}),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    isMaximized: vi.fn(async (): Promise<boolean> => false),
    onResized: vi.fn(async (_cb: () => void): Promise<() => void> => () => {}),
  };
  return { win, getCurrentWindowMock: vi.fn(() => win) };
});

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: getCurrentWindowMock,
}));

import {
  WINDOW_PERMISSIONS,
  close,
  isMaximized,
  minimize,
  onMaximizeChange,
  toggleMaximize,
} from './window';

function setTauri(present: boolean) {
  if (present) (globalThis as any).__TAURI_INTERNALS__ = {};
  else delete (globalThis as any).__TAURI_INTERNALS__;
}

beforeEach(() => {
  vi.clearAllMocks();
  win.isMaximized.mockResolvedValue(false);
});

afterEach(() => {
  setTauri(false);
});

describe('outside Tauri', () => {
  beforeEach(() => setTauri(false));

  it('minimize, toggleMaximize and close resolve without touching the window', async () => {
    await expect(minimize()).resolves.toBeUndefined();
    await expect(toggleMaximize()).resolves.toBeUndefined();
    await expect(close()).resolves.toBeUndefined();
    expect(getCurrentWindowMock).not.toHaveBeenCalled();
  });

  it('isMaximized resolves false', async () => {
    await expect(isMaximized()).resolves.toBe(false);
    expect(getCurrentWindowMock).not.toHaveBeenCalled();
  });

  it('onMaximizeChange resolves to a callable no-op and never touches the window', async () => {
    const unlisten = await onMaximizeChange(() => {});
    expect(typeof unlisten).toBe('function');
    expect(() => unlisten()).not.toThrow();
    expect(getCurrentWindowMock).not.toHaveBeenCalled();
  });
});

describe('inside Tauri', () => {
  beforeEach(() => setTauri(true));

  it('minimize calls win.minimize once', async () => {
    await minimize();
    expect(win.minimize).toHaveBeenCalledTimes(1);
  });

  it('toggleMaximize calls win.toggleMaximize once', async () => {
    await toggleMaximize();
    expect(win.toggleMaximize).toHaveBeenCalledTimes(1);
  });

  it('close calls win.close once', async () => {
    await close();
    expect(win.close).toHaveBeenCalledTimes(1);
  });

  it('isMaximized returns true when the window reports maximized', async () => {
    win.isMaximized.mockResolvedValueOnce(true);
    await expect(isMaximized()).resolves.toBe(true);
  });

  it('isMaximized returns false when the window reports not maximized', async () => {
    win.isMaximized.mockResolvedValueOnce(false);
    await expect(isMaximized()).resolves.toBe(false);
  });

  it('onMaximizeChange subscribes via win.onResized', async () => {
    await onMaximizeChange(() => {});
    expect(win.onResized).toHaveBeenCalledTimes(1);
    expect(typeof win.onResized.mock.calls[0][0]).toBe('function');
  });

  it('handler receives true when the resize callback fires and the window is maximized', async () => {
    const handler = vi.fn();
    win.isMaximized.mockResolvedValue(true);
    await onMaximizeChange(handler);
    const cb = win.onResized.mock.calls[0][0];
    await cb();
    expect(handler).toHaveBeenCalledWith(true);
  });

  it('handler receives false when the resize callback fires and the window is not maximized', async () => {
    const handler = vi.fn();
    win.isMaximized.mockResolvedValue(false);
    await onMaximizeChange(handler);
    const cb = win.onResized.mock.calls[0][0];
    await cb();
    expect(handler).toHaveBeenCalledWith(false);
  });

  it('returns a function that calls the unlisten produced by win.onResized', async () => {
    // The wrapper must also cancel a pending trailing read (issue #76), so identity is not required.
    const unlisten = vi.fn();
    win.onResized.mockResolvedValueOnce(unlisten);
    const returned = await onMaximizeChange(() => {});
    expect(unlisten).not.toHaveBeenCalled();
    returned();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});

describe('WINDOW_PERMISSIONS', () => {
  it('maps each wrapper to its exact permission string', () => {
    expect(Object.values(WINDOW_PERMISSIONS).sort()).toEqual(
      [
        'core:window:allow-minimize',
        'core:window:allow-toggle-maximize',
        'core:window:allow-close',
        'core:window:allow-is-maximized',
      ].sort(),
    );
  });

  it('every value starts with core:window:allow-', () => {
    for (const value of Object.values(WINDOW_PERMISSIONS)) {
      expect(typeof value).toBe('string');
      expect(value.startsWith('core:window:allow-')).toBe(true);
    }
  });
});
