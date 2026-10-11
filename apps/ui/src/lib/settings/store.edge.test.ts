import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Reply } from '../daemon';

const mocks = vi.hoisted(() => ({
  getConfig: vi.fn(),
  patchConfig: vi.fn(),
  onEvent: vi.fn(),
  handler: null as null | ((ev: unknown) => void),
}));

vi.mock('../daemon', () => ({
  getConfig: (...args: unknown[]) => mocks.getConfig(...args),
  patchConfig: (...args: unknown[]) => mocks.patchConfig(...args),
  onEvent: (handler: (ev: unknown) => void) => {
    mocks.handler = handler;
    return mocks.onEvent(handler) ?? (() => {});
  },
}));

import {
  VALIDATE_MS,
  acceptExternal,
  checkExternal,
  loadSettings,
  resetSettings,
  save,
  setPath,
  settings,
  startSettingsSync,
  validateDraft,
  type ConfigDoc,
} from './store';

const CONFIG: ConfigDoc = {
  config_version: 1,
  general: {
    autostart: false,
    start_minimized: true,
    tray_badge_recording: true,
    locale: 'system',
    close_behavior: 'minimize_to_tray',
    confirm_exit_while_recording: true,
  },
  storage: {
    root: 'C:\\Videos\\ClipCheck',
    per_game_folders: true,
    quota_gb: 0,
    quota_action: 'warn',
    filename_template: '{game} {yyyy-MM-dd HH-mm-ss} {kind}',
    delete_to_recycle_bin: true,
  },
  detection: {
    mode: 'auto',
    allow: [],
    block: [],
    auto_record: true,
    record_sessions: true,
    after_manual_stop: 'until_next_launch',
    stop_grace_s: 10,
  },
  games: {},
};

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

function configReply(doc: ConfigDoc): Reply {
  return { type: 'config', data: { config: clone(doc), config_version: 1, path: 'C:\\cfg.toml' } };
}

function checked(restart = false, diagnostics: unknown[] = []): Reply {
  return {
    type: 'config_checked',
    data: { apply_class: restart ? 'restart' : 'live', restart_required: restart, diagnostics } as never,
  };
}

function applied(restart = false, diagnostics: unknown[] = []): Reply {
  return {
    type: 'config_applied',
    data: { apply_class: restart ? 'next_part' : 'live', restart_required: restart, diagnostics } as never,
  };
}

function rejected(diagnostics: unknown[]): Reply {
  return { type: 'config_rejected', data: { diagnostics } as never };
}

/** Lets pending promise callbacks run. */
async function flush(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
}

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

beforeEach(() => {
  vi.useFakeTimers();
  mocks.getConfig.mockReset();
  mocks.patchConfig.mockReset();
  mocks.onEvent.mockReset();
  mocks.handler = null;
  resetSettings();
});

afterEach(() => {
  vi.useRealTimers();
});

async function loaded(): Promise<void> {
  mocks.getConfig.mockResolvedValue(configReply(CONFIG));
  await loadSettings();
}

describe('save edge cases', () => {
  it('returns false and sends nothing while saved is null', async () => {
    expect(await save()).toBe(false);
    expect(mocks.patchConfig).not.toHaveBeenCalled();
    expect(get(settings).saving).toBe(false);
  });

  it('a second save while the first is in flight returns false and sends no second patch', async () => {
    await loaded();
    setPath('general.autostart', true);
    const reply = deferred<Reply>();
    mocks.patchConfig.mockReturnValue(reply.promise);
    mocks.getConfig.mockResolvedValue(configReply({ ...CONFIG, general: { ...CONFIG.general, autostart: true } }));

    const first = save();
    const second = await save();
    expect(second).toBe(false);
    expect(mocks.patchConfig).toHaveBeenCalledTimes(1);

    reply.resolve(applied(false));
    expect(await first).toBe(true);
  });
});

describe('setPath with a missing parent', () => {
  it('creates the intermediate objects, marks dirty, and does not mutate saved', async () => {
    await loaded();
    const savedBefore = get(settings).saved;
    const snapshot = clone(CONFIG);

    setPath('storage.new_key.deep', 42);

    const s = get(settings);
    expect(s.draft?.storage).toMatchObject({ new_key: { deep: 42 } });
    expect(s.dirty).toBe(true);
    expect(s.saved).toBe(savedBefore);
    expect(s.saved).toEqual(snapshot);
    expect('new_key' in (s.saved as ConfigDoc).storage).toBe(false);
  });
});

describe('config_applied with a failing re-read', () => {
  it('keeps the submitted draft as saved, is clean, shows Applied, and does not throw', async () => {
    await loaded();
    setPath('general.close_behavior', 'exit');
    mocks.patchConfig.mockResolvedValue(applied(false));
    mocks.getConfig.mockRejectedValue(new Error('down'));

    const ok = await save();

    expect(ok).toBe(true);
    const s = get(settings);
    expect(s.saved?.general.close_behavior).toBe('exit');
    expect(s.draft).toEqual(s.saved);
    expect(s.dirty).toBe(false);
    expect(s.notice).toBe('Applied');
    expect(s.saving).toBe(false);
  });
});

describe('rejected save', () => {
  it('clears saving, sets the rejection notice, and keeps the user value in the draft', async () => {
    await loaded();
    setPath('storage.filename_template', '{game}');
    const diag = { code: 'CFG-S008', severity: 'error', path: 'storage.filename_template', message: 'needs a unique part' };
    mocks.patchConfig.mockResolvedValue(rejected([diag]));

    const ok = await save();

    expect(ok).toBe(false);
    const s = get(settings);
    expect(s.saving).toBe(false);
    expect(s.notice).toBe('Not saved. needs a unique part');
    expect(s.draft?.storage.filename_template).toBe('{game}');
    expect(s.diagnostics).toEqual([diag]);
  });
});

describe('late dry run after an edit back to saved', () => {
  // Gap: setPath() does not bump `seq`, so a dry run started before the draft became clean
  // still passes the `mine !== seq` check and writes its findings onto the clean draft.
  it('does not leave stale findings on a draft that is clean again', async () => {
    await loaded();
    const diag = { code: 'CFG-S005', severity: 'error', path: 'general.autostart', message: 'stale' };
    const first = deferred<Reply>();
    mocks.patchConfig.mockReturnValueOnce(first.promise);

    setPath('general.autostart', true);
    const run = validateDraft();
    setPath('general.autostart', false);
    first.resolve(checked(false, [diag]));
    await run;
    await flush();

    expect(get(settings).dirty).toBe(false);
    expect(get(settings).diagnostics).toEqual([]);
  });
});

describe('acceptExternal without an external value', () => {
  it('is a no-op', async () => {
    await loaded();
    const before = get(settings);

    acceptExternal();

    const after = get(settings);
    expect(after.saved).toBe(before.saved);
    expect(after.draft).toBe(before.draft);
    expect(after).toEqual(before);
  });
});

describe('checkExternal with an error reply on a clean draft', () => {
  it('changes nothing', async () => {
    await loaded();
    const before = get(settings);
    mocks.getConfig.mockResolvedValue({ type: 'error', data: { code: 'busy', message: 'Try later', retryable: true } });

    await checkExternal();

    const after = get(settings);
    expect(after.saved).toBe(before.saved);
    expect(after.draft).toBe(before.draft);
    expect(after).toEqual(before);
    expect(after.external).toBeNull();
  });
});

describe('repeated checkExternal on a dirty draft', () => {
  it('holds the changed daemon config as external, equal to it, after two checks', async () => {
    await loaded();
    setPath('general.autostart', true);
    const outside = { ...CONFIG, storage: { ...CONFIG.storage, quota_gb: 25 } };
    mocks.getConfig.mockResolvedValue(configReply(outside));

    await checkExternal();
    await checkExternal();

    const s = get(settings);
    expect(s.external).toEqual(outside);
    expect(s.dirty).toBe(true);
    expect(s.draft?.general.autostart).toBe(true);
    expect(s.saved).toEqual(CONFIG);
  });
});

describe('event listener lifecycle', () => {
  it('resetSettings stops the listener so a later startSettingsSync subscribes again', () => {
    startSettingsSync();
    expect(mocks.onEvent).toHaveBeenCalledTimes(1);

    resetSettings();
    startSettingsSync();

    expect(mocks.onEvent).toHaveBeenCalledTimes(2);
  });

  it('non-config events do not trigger a config read', async () => {
    await loaded();
    startSettingsSync();
    mocks.getConfig.mockClear();

    mocks.handler?.({ type: 'clip_saved', data: { clip_id: 'c', path: 'p', duration_ms: 1, bytes: 1 } });
    mocks.handler?.({ type: 'state_changed', data: {} });
    mocks.handler?.({ type: 'warning', data: { message: 'x' } });
    await flush();

    expect(mocks.getConfig).not.toHaveBeenCalled();
  });
});

describe('validation throwing during patchConfig', () => {
  it('sets error from a synchronous throw and clears checking', async () => {
    await loaded();
    mocks.patchConfig.mockImplementation(() => {
      throw new Error('boom');
    });
    setPath('general.autostart', true);

    await validateDraft();

    const s = get(settings);
    expect(s.error).toBe('boom');
    expect(s.checking).toBe(false);
  });
});
