import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

type Handler = (e: { payload: unknown }) => void;
const listenMock = vi.fn();
vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import {
  DaemonUnavailableError,
  STATUS_TOPICS,
  clipNow,
  daemonConnected,
  getConfig,
  getStatus,
  isErrorReply,
  isTauri,
  onConnection,
  onEvent,
  patchConfig,
  request,
  splitSession,
  startSession,
  stopSession,
  subscribe,
  type Reply,
} from './daemon';

const g = globalThis as unknown as Record<string, unknown>;

/** Makes `isTauri()` true for the duration of a test. */
function enterTauri(): void {
  g.window = { __TAURI_INTERNALS__: {} };
}

const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

/** Returns the command object sent to the last `daemon_request` call. */
function lastCommand(): Record<string, unknown> {
  const call = invokeMock.mock.calls.at(-1);
  expect(call?.[0]).toBe('daemon_request');
  return (call?.[1] as { command: Record<string, unknown> }).command;
}

let handlers: Map<string, Handler>;
let unlistenMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  handlers = new Map();
  unlistenMock = vi.fn();
  listenMock.mockImplementation(async (name: string, h: Handler) => {
    handlers.set(name, h);
    return unlistenMock;
  });
  delete g.window;
});

afterEach(() => {
  delete g.window;
});

describe('isTauri and isErrorReply', () => {
  it('is false when there is no window', () => {
    expect(isTauri()).toBe(false);
  });
  it('is false for a window without Tauri internals', () => {
    g.window = {};
    expect(isTauri()).toBe(false);
  });
  it('is true when Tauri internals are present', () => {
    enterTauri();
    expect(isTauri()).toBe(true);
  });
  it('detects error replies only', () => {
    const err: Reply = { type: 'error', data: { code: 'busy', message: 'x', retryable: true } };
    const ok: Reply = { type: 'ok' };
    expect(isErrorReply(err)).toBe(true);
    expect(isErrorReply(ok)).toBe(false);
  });
});

describe('request', () => {
  it('rejects with DaemonUnavailableError outside Tauri without calling invoke', async () => {
    await expect(request({ type: 'get_status' })).rejects.toBeInstanceOf(DaemonUnavailableError);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('sends the command through daemon_request and returns the reply', async () => {
    enterTauri();
    const reply: Reply = { type: 'ok' };
    invokeMock.mockResolvedValue(reply);
    await expect(request({ type: 'ping' })).resolves.toEqual(reply);
    expect(invokeMock).toHaveBeenCalledWith('daemon_request', { command: { type: 'ping' } });
  });

  it('maps an invoke failure to DaemonUnavailableError', async () => {
    enterTauri();
    invokeMock.mockRejectedValue(new Error('pipe closed'));
    const err = await request({ type: 'ping' }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DaemonUnavailableError);
    expect((err as Error).name).toBe('DaemonUnavailableError');
    expect((err as Error).message).toBe('pipe closed');
  });

  it('maps a non-Error invoke failure to DaemonUnavailableError', async () => {
    enterTauri();
    invokeMock.mockRejectedValue('nope');
    await expect(request({ type: 'ping' })).rejects.toBeInstanceOf(DaemonUnavailableError);
  });
});

describe('command wrappers', () => {
  beforeEach(() => {
    enterTauri();
    invokeMock.mockResolvedValue({ type: 'ok' });
  });

  it('unit commands carry no data key', async () => {
    await getStatus();
    expect(lastCommand()).toEqual({ type: 'get_status' });
    expect(lastCommand()).not.toHaveProperty('data');

    await startSession();
    expect(lastCommand()).toEqual({ type: 'start_session' });
    expect(lastCommand()).not.toHaveProperty('data');

    await stopSession();
    expect(lastCommand()).toEqual({ type: 'stop_session' });
    expect(lastCommand()).not.toHaveProperty('data');

    await splitSession();
    expect(lastCommand()).toEqual({ type: 'split_session' });
    expect(lastCommand()).not.toHaveProperty('data');

    await getConfig();
    expect(lastCommand()).toEqual({ type: 'get_config' });
    expect(lastCommand()).not.toHaveProperty('data');
  });

  it('clipNow sends seconds in data', async () => {
    await clipNow(30);
    expect(lastCommand()).toEqual({ type: 'clip_now', data: { seconds: 30 } });
  });

  it('patchConfig defaults dry_run to false', async () => {
    await patchConfig({ replay: { max_seconds: 600 } });
    expect(lastCommand()).toEqual({
      type: 'patch_config',
      data: { patch: { replay: { max_seconds: 600 } }, dry_run: false },
    });
  });

  it('patchConfig passes dry_run true when requested', async () => {
    await patchConfig({ a: 1 }, { dryRun: true });
    expect((lastCommand().data as { dry_run: boolean }).dry_run).toBe(true);
  });

  it('subscribe sends the topics', async () => {
    await subscribe(STATUS_TOPICS);
    expect(lastCommand()).toEqual({ type: 'subscribe', data: { topics: STATUS_TOPICS } });
  });

  it('STATUS_TOPICS leaves out levels', () => {
    expect(STATUS_TOPICS).not.toContain('levels');
    expect(STATUS_TOPICS).toContain('state');
  });
});

describe('daemonConnected', () => {
  it('is false outside Tauri without calling invoke', async () => {
    await expect(daemonConnected()).resolves.toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });
  it('returns the backend value inside Tauri', async () => {
    enterTauri();
    invokeMock.mockResolvedValue(true);
    await expect(daemonConnected()).resolves.toBe(true);
    expect(invokeMock).toHaveBeenCalledWith('daemon_connected');
  });
  it('treats a non-true value as disconnected', async () => {
    enterTauri();
    invokeMock.mockResolvedValue('yes');
    await expect(daemonConnected()).resolves.toBe(false);
  });
  it('is false when invoke throws', async () => {
    enterTauri();
    invokeMock.mockRejectedValue(new Error('boom'));
    await expect(daemonConnected()).resolves.toBe(false);
  });
});

describe('onEvent', () => {
  it('is a no-op outside Tauri', () => {
    const handler = vi.fn();
    const unsub = onEvent(handler);
    expect(listenMock).not.toHaveBeenCalled();
    expect(() => unsub()).not.toThrow();
  });

  it('listens on daemon-event and forwards payloads', async () => {
    enterTauri();
    const handler = vi.fn();
    onEvent(handler);
    await flush();
    expect(listenMock).toHaveBeenCalledWith('daemon-event', expect.any(Function));
    const event = { type: 'mic_mute_changed', data: { muted: true } };
    handlers.get('daemon-event')!({ payload: event });
    expect(handler).toHaveBeenCalledWith(event);
  });

  it('ignores events fired after unsubscribe and unlistens', async () => {
    enterTauri();
    const handler = vi.fn();
    const unsub = onEvent(handler);
    await flush();
    unsub();
    handlers.get('daemon-event')!({ payload: { type: 'mic_mute_changed', data: { muted: true } } });
    expect(handler).not.toHaveBeenCalled();
    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });

  it('unlistens when unsubscribed before listen resolves', async () => {
    enterTauri();
    let resolveListen: (fn: () => void) => void = () => {};
    listenMock.mockImplementationOnce(
      () => new Promise<() => void>((res) => (resolveListen = res)),
    );
    const handler = vi.fn();
    const unsub = onEvent(handler);
    unsub();
    resolveListen(unlistenMock);
    await flush();
    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});

describe('onConnection', () => {
  it('is a no-op outside Tauri', () => {
    onConnection(vi.fn());
    expect(listenMock).not.toHaveBeenCalled();
  });

  it('listens on daemon-connection and passes a boolean', async () => {
    enterTauri();
    const handler = vi.fn();
    onConnection(handler);
    await flush();
    expect(listenMock).toHaveBeenCalledWith('daemon-connection', expect.any(Function));
    handlers.get('daemon-connection')!({ payload: { connected: true } });
    handlers.get('daemon-connection')!({ payload: { connected: false } });
    expect(handler.mock.calls).toEqual([[true], [false]]);
  });

  it('treats a malformed payload as disconnected', async () => {
    enterTauri();
    const handler = vi.fn();
    onConnection(handler);
    await flush();
    handlers.get('daemon-connection')!({ payload: null });
    handlers.get('daemon-connection')!({ payload: { connected: 'yes' } });
    expect(handler.mock.calls).toEqual([[false], [false]]);
  });
});
