import { describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

const listenMock = vi.fn();
vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import {
  CONFIRM_EXIT_EVENT,
  INITIAL_EXIT_CONFIRM,
  createExitConfirmStore,
  listenForExitConfirm,
  reduceExitConfirm,
  type ExitConfirmState,
} from './exitconfirm';

/** Returns a promise plus its resolve/reject functions, for controlling invoke. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('reduceExitConfirm', () => {
  it('starts closed and not busy', () => {
    expect(INITIAL_EXIT_CONFIRM).toEqual({ open: false, busy: false });
  });

  it('open shows the dialog and is idempotent', () => {
    const once = reduceExitConfirm(INITIAL_EXIT_CONFIRM, 'open');
    expect(once).toEqual({ open: true, busy: false });
    const twice = reduceExitConfirm(once, 'open');
    expect(twice).toEqual({ open: true, busy: false });
  });

  it('cancel closes the dialog', () => {
    const open: ExitConfirmState = { open: true, busy: false };
    expect(reduceExitConfirm(open, 'cancel')).toEqual({ open: false, busy: false });
  });

  it('cancel while busy is ignored', () => {
    const busy: ExitConfirmState = { open: true, busy: true };
    expect(reduceExitConfirm(busy, 'cancel')).toEqual({ open: true, busy: true });
  });

  it('confirm while closed does nothing', () => {
    expect(reduceExitConfirm(INITIAL_EXIT_CONFIRM, 'confirm')).toEqual({
      open: false,
      busy: false,
    });
  });

  it('confirm while open becomes busy, and a second confirm stays busy', () => {
    const open: ExitConfirmState = { open: true, busy: false };
    const busy = reduceExitConfirm(open, 'confirm');
    expect(busy).toEqual({ open: true, busy: true });
    expect(reduceExitConfirm(busy, 'confirm')).toEqual({ open: true, busy: true });
  });

  it('failed returns to open and not busy, and is a no-op when not busy', () => {
    const busy: ExitConfirmState = { open: true, busy: true };
    expect(reduceExitConfirm(busy, 'failed')).toEqual({ open: true, busy: false });
    const open: ExitConfirmState = { open: true, busy: false };
    expect(reduceExitConfirm(open, 'failed')).toEqual({ open: true, busy: false });
  });
});

describe('createExitConfirmStore', () => {
  it('exposes the initial closed state', () => {
    const store = createExitConfirmStore(vi.fn());
    let state: ExitConfirmState | undefined;
    const unsub = store.subscribe((s) => (state = s));
    expect(state).toEqual({ open: false, busy: false });
    unsub();
  });

  it('open then cancel toggles the state', () => {
    const store = createExitConfirmStore(vi.fn());
    let state: ExitConfirmState | undefined;
    const unsub = store.subscribe((s) => (state = s));
    store.open();
    expect(state).toEqual({ open: true, busy: false });
    store.cancel();
    expect(state).toEqual({ open: false, busy: false });
    unsub();
  });

  it('confirm while closed does not call invoke', async () => {
    const invokeFn = vi.fn(async () => undefined);
    const store = createExitConfirmStore(invokeFn);
    await store.confirm();
    expect(invokeFn).not.toHaveBeenCalled();
  });

  it('confirm when open calls exit_app once with stopDaemon and becomes busy', async () => {
    const d = deferred<unknown>();
    const invokeFn = vi.fn(() => d.promise);
    const store = createExitConfirmStore(invokeFn);
    let state: ExitConfirmState | undefined;
    const unsub = store.subscribe((s) => (state = s));
    store.open();

    const pending = store.confirm();
    expect(invokeFn).toHaveBeenCalledTimes(1);
    expect(invokeFn).toHaveBeenCalledWith('exit_app', { stopDaemon: true });
    expect(state).toEqual({ open: true, busy: true });

    d.resolve(undefined);
    await pending;
    unsub();
  });

  it('a second confirm while busy does not call invoke again', async () => {
    const d = deferred<unknown>();
    const invokeFn = vi.fn(() => d.promise);
    const store = createExitConfirmStore(invokeFn);
    store.open();

    const first = store.confirm();
    const second = store.confirm();
    expect(invokeFn).toHaveBeenCalledTimes(1);

    d.resolve(undefined);
    await Promise.all([first, second]);
    expect(invokeFn).toHaveBeenCalledTimes(1);
  });

  it('cancel while busy is ignored', async () => {
    const d = deferred<unknown>();
    const store = createExitConfirmStore(vi.fn(() => d.promise));
    let state: ExitConfirmState | undefined;
    const unsub = store.subscribe((s) => (state = s));
    store.open();
    const pending = store.confirm();
    store.cancel();
    expect(state).toEqual({ open: true, busy: true });
    d.resolve(undefined);
    await pending;
    unsub();
  });

  it('an invoke rejection returns to open and not busy, and a later confirm retries', async () => {
    const invokeFn = vi
      .fn<(command: string, args?: Record<string, unknown>) => Promise<unknown>>()
      .mockRejectedValueOnce(new Error('daemon unavailable'))
      .mockResolvedValueOnce(undefined);
    const store = createExitConfirmStore(invokeFn);
    let state: ExitConfirmState | undefined;
    const unsub = store.subscribe((s) => (state = s));
    store.open();

    await store.confirm();
    expect(state).toEqual({ open: true, busy: false });
    expect(invokeFn).toHaveBeenCalledTimes(1);

    await store.confirm();
    expect(invokeFn).toHaveBeenCalledTimes(2);
    expect(invokeFn).toHaveBeenLastCalledWith('exit_app', { stopDaemon: true });
    unsub();
  });

  it('confirm() resolves only after invoke settles', async () => {
    const d = deferred<unknown>();
    const store = createExitConfirmStore(vi.fn(() => d.promise));
    store.open();
    let settled = false;
    const pending = store.confirm().then(() => {
      settled = true;
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(settled).toBe(false);
    d.resolve(undefined);
    await pending;
    expect(settled).toBe(true);
  });
});

describe('listenForExitConfirm', () => {
  it('outside Tauri resolves to a no-op function and does not open the dialog', async () => {
    expect('__TAURI_INTERNALS__' in globalThis).toBe(false);
    const open = vi.fn();
    const unlisten = await listenForExitConfirm({ open });
    expect(typeof unlisten).toBe('function');
    expect(() => unlisten()).not.toThrow();
    expect(open).not.toHaveBeenCalled();
    expect(listenMock).not.toHaveBeenCalled();
  });

  it('exports the confirm-exit event name', () => {
    expect(CONFIRM_EXIT_EVENT).toBe('confirm-exit');
  });
});
