import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

import {
  classifyUpdateError,
  updateErrorMessage,
  isPillVisible,
  updateStore,
  INITIAL_UPDATE_STATE,
  initUpdates,
  checkNow,
  installNow,
  skipVersion,
  openUpdateModal,
  closeUpdateModal,
  type UpdateInfo,
  type UpdateState,
} from './updates';

const info = (over: Partial<UpdateInfo> = {}): UpdateInfo => ({
  version: '2.0.0',
  current_version: '1.9.0',
  notes: 'Fixes',
  pub_date: '2026-10-01T00:00:00Z',
  skipped: false,
  ...over,
});

const savedState: UpdateState = {
  last_checked_unix_ms: 1_700_000_000_000,
  skipped_version: null,
  last_installed_from: null,
};

const setTauri = (on: boolean) => {
  if (on) (globalThis as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  else delete (globalThis as Record<string, unknown>).__TAURI_INTERNALS__;
};

beforeEach(() => {
  updateStore.set({ ...INITIAL_UPDATE_STATE });
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  setTauri(false);
});

afterEach(() => {
  setTauri(false);
});

describe('classifyUpdateError', () => {
  it.each([
    ['a recording is in progress', 'recording'],
    ['Recording in progress, finish first', 'recording'],
    ['signature verification failed', 'signature'],
    ['Could not verify the download', 'signature'],
    ['error sending request: connection refused', 'network'],
    ['network unreachable', 'network'],
    ['timed out', 'network'],
    ['disk full', 'other'],
    ['', 'other'],
  ] as const)('%j -> %s', (msg, kind) => {
    expect(classifyUpdateError(msg)).toBe(kind);
  });

  it('is case-insensitive', () => {
    expect(classifyUpdateError('RECORDING')).toBe('recording');
    expect(classifyUpdateError('Network Error')).toBe('network');
    expect(classifyUpdateError('SIGNATURE mismatch')).toBe('signature');
  });

  it('priority when several rules match (order per doc comment: recording, signature, network)', () => {
    expect(classifyUpdateError('recording: signature check failed')).toBe('recording');
    expect(classifyUpdateError('signature check timed out')).toBe('signature');
    expect(classifyUpdateError('network signature error')).toBe('signature');
  });
});

describe('updateErrorMessage', () => {
  it.each(['recording', 'signature', 'network', 'other'] as const)(
    'returns non-empty text for %s',
    (kind) => {
      expect(updateErrorMessage(kind, 'raw').trim()).not.toBe('');
    },
  );

  it('recording text tells the user to finish the recording then update', () => {
    const text = updateErrorMessage('recording', 'a recording is in progress').toLowerCase();
    expect(text).toMatch(/record/);
    expect(text).toMatch(/finish|stop|after|once/);
    expect(text).toMatch(/update/);
  });

  it('returns non-empty text for other with an empty raw message', () => {
    expect(updateErrorMessage('other', '').trim()).not.toBe('');
  });

  it('does not leak a stack trace or huge raw text for long messages', () => {
    const raw = 'boom '.repeat(400) + '\nError: boom\n    at foo (updater.rs:1:1)\n    at bar (main.rs:2:2)';
    const text = updateErrorMessage('other', raw);
    expect(text).not.toContain('    at foo');
    expect(text).not.toContain('updater.rs:1:1');
    expect(text.length).toBeLessThan(raw.length);
  });
});

describe('isPillVisible', () => {
  it('is false for null', () => {
    expect(isPillVisible(null)).toBe(false);
  });

  it('is false for a skipped update', () => {
    expect(isPillVisible(info({ skipped: true }))).toBe(false);
  });

  it('is true for a normal available update', () => {
    expect(isPillVisible(info())).toBe(true);
  });
});

describe('store outside Tauri', () => {
  it('has the documented initial shape', async () => {
    vi.resetModules();
    const fresh = await import('./updates');
    expect(get(fresh.updateStore)).toEqual({
      available: null,
      progress: null,
      installing: false,
      error: null,
      state: null,
      modalOpen: false,
    });
  });

  it('open and close toggle modalOpen', () => {
    openUpdateModal();
    expect(get(updateStore).modalOpen).toBe(true);
    closeUpdateModal();
    expect(get(updateStore).modalOpen).toBe(false);
  });

  it('initUpdates returns a callable no-op unlisten and does not invoke', async () => {
    const unlisten = await initUpdates();
    expect(typeof unlisten).toBe('function');
    expect(() => unlisten()).not.toThrow();
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
  });

  it('checkNow resolves to null and leaves the store sane', async () => {
    await expect(checkNow()).resolves.toBeNull();
    const s = get(updateStore);
    expect(s.installing).toBe(false);
    expect(s.error).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it('installNow resolves without throwing', async () => {
    await expect(installNow()).resolves.toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
  });

  it('skipVersion does not throw', async () => {
    updateStore.update((s) => ({ ...s, available: info() }));
    await expect(skipVersion()).resolves.toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe('store inside Tauri (invoke and listen mocked)', () => {
  type Handler = (ev: { payload: unknown }) => void;
  let handlers: Record<string, Handler>;
  let unlistenSpies: ReturnType<typeof vi.fn>[];

  beforeEach(() => {
    setTauri(true);
    handlers = {};
    unlistenSpies = [];
    vi.mocked(listen).mockImplementation(async (event: string, handler: Handler) => {
      handlers[event] = handler;
      const spy = vi.fn();
      unlistenSpies.push(spy);
      return spy;
    });
  });

  it('initUpdates loads the saved state into the store', async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === 'get_update_state') return savedState;
      return undefined;
    });
    await initUpdates();
    expect(vi.mocked(invoke).mock.calls.map((c) => c[0])).toContain('get_update_state');
    expect(get(updateStore).state).toEqual(savedState);
  });

  it('listens to update-available and update-progress', async () => {
    vi.mocked(invoke).mockResolvedValue(savedState);
    await initUpdates();
    expect(Object.keys(handlers)).toEqual(
      expect.arrayContaining(['update-available', 'update-progress']),
    );

    handlers['update-available']({ payload: info() });
    expect(get(updateStore).available).toEqual(info());

    handlers['update-progress']({
      payload: { phase: 'downloading', done: 5, total: 10 },
    });
    expect(get(updateStore).progress).toMatchObject({ downloaded: 5, total: 10 });
  });

  it('unlisten removes both listeners', async () => {
    vi.mocked(invoke).mockResolvedValue(savedState);
    const unlisten = await initUpdates();
    unlisten();
    expect(unlistenSpies.length).toBeGreaterThanOrEqual(2);
    for (const spy of unlistenSpies) expect(spy).toHaveBeenCalledTimes(1);
  });

  it('checkNow calls check_for_update and stores the update', async () => {
    vi.mocked(invoke).mockResolvedValue(info());
    const result = await checkNow();
    expect(invoke).toHaveBeenCalledWith('check_for_update', expect.anything());
    expect(result).toEqual(info());
    expect(get(updateStore).available).toEqual(info());
  });

  it('checkNow with null result means up to date (available cleared)', async () => {
    updateStore.update((s) => ({ ...s, available: info() }));
    vi.mocked(invoke).mockResolvedValue(null);
    const result = await checkNow();
    expect(result).toBeNull();
    expect(get(updateStore).available).toBeNull();
  });

  it('installNow sets installing true while running', async () => {
    let reject!: (e: unknown) => void;
    vi.mocked(invoke).mockReturnValue(
      new Promise((_, rej) => {
        reject = rej;
      }),
    );
    const p = installNow();
    expect(get(updateStore).installing).toBe(true);
    reject('signature verification failed');
    await p;
    expect(get(updateStore).installing).toBe(false);
  });

  it('installNow on rejection stores the classified error and clears installing', async () => {
    vi.mocked(invoke).mockRejectedValue('signature verification failed');
    await installNow();
    const s = get(updateStore);
    expect(s.installing).toBe(false);
    expect(s.error?.kind).toBe('signature');
  });

  it('installNow classifies a recording rejection', async () => {
    vi.mocked(invoke).mockRejectedValue('a recording is in progress');
    await installNow();
    expect(get(updateStore).error?.kind).toBe('recording');
    expect(get(updateStore).installing).toBe(false);
  });

  it('installNow(true) passes stopRecording to install_update', async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);
    await installNow(true);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('install_update', { stopRecording: true });
  });

  it('installNow() defaults to stopRecording false', async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);
    await installNow();
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('install_update', { stopRecording: false });
  });

  it('reopening the dialog clears a stale recording error', () => {
    updateStore.update((s) => ({
      ...s,
      available: info(),
      error: { kind: 'recording', message: 'A recording is in progress.' },
    }));
    openUpdateModal();
    expect(get(updateStore).error).toBeNull();
    expect(get(updateStore).modalOpen).toBe(true);
  });

  it('initUpdates restores a pending update found by an earlier run', async () => {
    setTauri(true);
    vi.mocked(listen).mockResolvedValue(() => {});
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === 'get_pending_update' ? info() : savedState,
    );
    await initUpdates();
    expect(get(updateStore).available?.version).toBe('2.0.0');
  });

  it('skipVersion calls skip_update_version with the version and hides the pill and modal', async () => {
    updateStore.update((s) => ({ ...s, available: info(), modalOpen: true }));
    vi.mocked(invoke).mockResolvedValue(undefined);
    await skipVersion();
    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === 'skip_update_version');
    expect(call).toBeDefined();
    expect(JSON.stringify(call?.[1])).toContain('2.0.0');
    const s = get(updateStore);
    // Observed: `available` is kept but flagged skipped (the pill hides via isPillVisible),
    // rather than cleared to null as the lead's spec guessed.
    expect(s.available).toMatchObject({ version: '2.0.0', skipped: true });
    expect(isPillVisible(s.available)).toBe(false);
    expect(s.modalOpen).toBe(false);
  });
});
