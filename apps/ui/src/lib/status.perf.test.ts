import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

// The daemon bridge is replaced wholesale so the test controls every pushed event,
// connection change and reply. Nothing touches Tauri.
const mocks = vi.hoisted(() => ({
  getStatus: vi.fn(),
  getConfig: vi.fn(),
  subscribe: vi.fn(),
  daemonConnected: vi.fn(),
  onEvent: vi.fn(),
  onConnection: vi.fn(),
  unsubEvent: vi.fn(),
  unsubConnection: vi.fn(),
  eventHandler: null as null | ((ev: unknown) => void),
  connectionHandler: null as null | ((connected: boolean) => void),
}));

vi.mock('./daemon', () => ({
  STATUS_TOPICS: ['state', 'clips', 'sessions', 'health', 'warnings', 'settings', 'library'],
  getStatus: (...args: unknown[]) => mocks.getStatus(...args),
  getConfig: (...args: unknown[]) => mocks.getConfig(...args),
  subscribe: (...args: unknown[]) => mocks.subscribe(...args),
  daemonConnected: (...args: unknown[]) => mocks.daemonConnected(...args),
  onEvent: (handler: (ev: unknown) => void) => {
    mocks.eventHandler = handler;
    mocks.onEvent(handler);
    return mocks.unsubEvent;
  },
  onConnection: (handler: (connected: boolean) => void) => {
    mocks.connectionHandler = handler;
    mocks.onConnection(handler);
    return mocks.unsubConnection;
  },
}));

import {
  INITIAL_CONFIG,
  INITIAL_STATUS,
  fetchConfigShared,
  liveConfig,
  liveStatus,
  refreshStatus,
  startLiveStatus,
} from './status';

/** Lets pending promise callbacks and fake-timer callbacks settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
}

function snap(over: Record<string, unknown> = {}) {
  return {
    state: 'idle',
    game: null,
    session: null,
    buffer_seconds: 0,
    buffer_bytes: 0,
    encoder: null,
    codec: null,
    fps_target: 60,
    bitrate_kbps: null,
    mic_muted: false,
    detection_paused: false,
    autostart: 'enabled',
    worker_pid: null,
    worker_restarts_10min: 0,
    uptime_s: 0,
    degraded_reason: null,
    game_info: null,
    buffer_secs: 0,
    settings_pending_restart: false,
    server_unix_ms: Date.now(),
    ...over,
  };
}

function statusReply(over: Record<string, unknown> = {}) {
  return { type: 'status', data: snap(over) };
}

function configReply() {
  return { type: 'config', data: { config: {}, config_version: 1, path: 'cfg.toml' } };
}

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

let stopLive: (() => void) | null = null;

beforeEach(() => {
  vi.useFakeTimers();
  for (const m of Object.values(mocks)) {
    if (typeof m === 'function' && 'mockReset' in m) (m as { mockReset: () => void }).mockReset();
  }
  mocks.eventHandler = null;
  mocks.connectionHandler = null;
  mocks.getStatus.mockResolvedValue(statusReply());
  mocks.getConfig.mockResolvedValue(configReply());
  mocks.subscribe.mockResolvedValue({ type: 'ok' });
  mocks.daemonConnected.mockResolvedValue(true);
  liveStatus.set(INITIAL_STATUS);
  liveConfig.set(INITIAL_CONFIG);
});

afterEach(() => {
  stopLive?.();
  stopLive = null;
  delete (globalThis as { document?: unknown }).document;
  vi.useRealTimers();
});

describe('#72 fetchConfigShared: one get_config for concurrent callers', () => {
  it('returns the same in-flight promise to every concurrent caller', async () => {
    const d = deferred<unknown>();
    mocks.getConfig.mockReturnValue(d.promise);

    const a = fetchConfigShared();
    const b = fetchConfigShared();
    const c = fetchConfigShared();

    expect(mocks.getConfig).toHaveBeenCalledTimes(1);
    expect(b).toBe(a);
    expect(c).toBe(a);

    const reply = configReply();
    d.resolve(reply);
    await expect(a).resolves.toEqual(reply);
    await expect(b).resolves.toEqual(reply);
    expect(mocks.getConfig).toHaveBeenCalledTimes(1);
  });

  it('starts a fresh fetch for a call made after the reply', async () => {
    const first = configReply();
    const second = { type: 'config', data: { config: { config_version: 2 }, config_version: 2, path: 'x' } };
    mocks.getConfig.mockResolvedValueOnce(first).mockResolvedValueOnce(second);

    await expect(fetchConfigShared()).resolves.toEqual(first);
    const next = fetchConfigShared();
    expect(mocks.getConfig).toHaveBeenCalledTimes(2);
    await expect(next).resolves.toEqual(second);
  });

  it('delivers a rejection to every waiter and does not poison later calls', async () => {
    const ok = configReply();
    mocks.getConfig.mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce(ok);

    const a = fetchConfigShared().then(
      () => 'resolved',
      (e: Error) => e.message,
    );
    const b = fetchConfigShared().then(
      () => 'resolved',
      (e: Error) => e.message,
    );
    await expect(a).resolves.toBe('boom');
    await expect(b).resolves.toBe('boom');
    expect(mocks.getConfig).toHaveBeenCalledTimes(1);

    await expect(fetchConfigShared()).resolves.toEqual(ok);
    expect(mocks.getConfig).toHaveBeenCalledTimes(2);
  });
});

describe('#74 liveStatus notifies only on a real change', () => {
  it('counts notifications across events, no-op events and offset drift', async () => {
    stopLive = startLiveStatus();
    await settle();
    expect(mocks.eventHandler).toBeTypeOf('function');
    expect(get(liveStatus).connected).toBe(true);
    expect(get(liveStatus).state).toBe('idle');

    const emit = mocks.eventHandler as (ev: unknown) => void;
    let notes = 0;
    const unsub = liveStatus.subscribe(() => {
      notes += 1;
    });
    notes = 0; // ignore the initial synchronous call

    // A changing event notifies.
    emit({ type: 'state_changed', data: { state: 'recording', game: null } });
    expect(notes).toBe(1);
    expect(get(liveStatus).state).toBe('recording');

    // No-op events do not notify, even when repeated.
    emit({ type: 'capture_health', data: { fps: 60, dropped_frames: 0, black_frames_active: false } });
    emit({ type: 'capture_health', data: { fps: 60, dropped_frames: 0, black_frames_active: false } });
    emit({ type: 'audio_levels', data: { tracks: [] } });
    emit({ type: 'audio_levels', data: { tracks: [] } });
    expect(notes).toBe(1);

    // A real change notifies again.
    emit({ type: 'state_changed', data: { state: 'buffer_only', game: null } });
    expect(notes).toBe(2);

    // Offset drift under 1 s counts as equal: a poll re-measuring the clock does not notify.
    mocks.getStatus.mockResolvedValue(
      statusReply({ state: 'buffer_only', server_unix_ms: Date.now() + 500 }),
    );
    await refreshStatus();
    expect(notes).toBe(2);

    // Drift of 1 s or more is a real change to the offset and does notify (sanity check
    // that the drift path is live, so the case above is not vacuous).
    mocks.getStatus.mockResolvedValue(
      statusReply({ state: 'buffer_only', server_unix_ms: Date.now() + 5000 }),
    );
    await refreshStatus();
    expect(notes).toBe(3);

    unsub();
  });
});

describe('issue_184 identical refreshes do not notify', () => {
  it('issue_184 an identical snapshot does not notify, a changed one does', async () => {
    mocks.getStatus.mockResolvedValue(statusReply({ state: 'buffer_only' }));
    await refreshStatus();
    expect(get(liveStatus).state).toBe('buffer_only');

    let notes = 0;
    const unsub = liveStatus.subscribe(() => {
      notes += 1;
    });
    notes = 0; // ignore the initial synchronous call

    mocks.getStatus.mockResolvedValue(statusReply({ state: 'buffer_only' }));
    await refreshStatus();
    await refreshStatus();
    expect(notes).toBe(0);

    mocks.getStatus.mockResolvedValue(statusReply({ state: 'recording' }));
    await refreshStatus();
    expect(notes).toBe(1);
    expect(get(liveStatus).state).toBe('recording');

    unsub();
  });
});

describe('#184 status poll pauses while the document is hidden', () => {
  it('stops polling while hidden, refreshes on show, resumes at 30 s, and cleans up', async () => {
    let hidden = false;
    const listeners = new Map<string, Set<() => void>>();
    (globalThis as { document?: unknown }).document = {
      get hidden() {
        return hidden;
      },
      addEventListener(type: string, fn: () => void) {
        if (!listeners.has(type)) listeners.set(type, new Set());
        listeners.get(type)!.add(fn);
      },
      removeEventListener(type: string, fn: () => void) {
        listeners.get(type)?.delete(fn);
      },
    };
    const dispatch = (): void => {
      for (const fn of listeners.get('visibilitychange') ?? []) fn();
    };

    stopLive = startLiveStatus();
    await settle();
    expect(get(liveStatus).connected).toBe(true);

    // Visible: the 30 s poll runs.
    mocks.getStatus.mockClear();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.getStatus).toHaveBeenCalledTimes(1);

    // Hidden: 30 s of fake time yields no status reads.
    hidden = true;
    dispatch();
    mocks.getStatus.mockClear();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.getStatus).not.toHaveBeenCalled();

    // Shown again: an immediate refresh, then the poll resumes at 30 s.
    hidden = false;
    dispatch();
    expect(mocks.getStatus).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(29_999);
    expect(mocks.getStatus).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(mocks.getStatus).toHaveBeenCalledTimes(2);

    // Cleanup clears the timer and both listeners.
    stopLive();
    stopLive = null;
    expect(listeners.get('visibilitychange')?.size ?? 0).toBe(0);
    expect(mocks.unsubEvent).toHaveBeenCalledTimes(1);
    expect(mocks.unsubConnection).toHaveBeenCalledTimes(1);
    mocks.getStatus.mockClear();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(mocks.getStatus).not.toHaveBeenCalled();
  });
});
