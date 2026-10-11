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
  keepMine,
  loadSettings,
  onRecorderConnected,
  resetSettings,
  revert,
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

describe('loadSettings', () => {
  it('makes the config both the saved and the draft copy', async () => {
    await loaded();
    const s = get(settings);
    expect(s.saved).toEqual(CONFIG);
    expect(s.draft).toEqual(CONFIG);
    expect(s.dirty).toBe(false);
    expect(s.loading).toBe(false);
    expect(s.error).toBeNull();
  });

  it('keeps saved and draft as separate copies', async () => {
    await loaded();
    const s = get(settings);
    expect(s.saved).not.toBe(s.draft);
  });

  it('reports an error reply', async () => {
    mocks.getConfig.mockResolvedValue({ type: 'error', data: { code: 'busy', message: 'Try later', retryable: true } });
    await loadSettings();
    expect(get(settings).error).toBe('Try later');
    expect(get(settings).saved).toBeNull();
    expect(get(settings).loading).toBe(false);
  });

  it('reports a transport failure', async () => {
    mocks.getConfig.mockRejectedValue(new Error('Recorder is not reachable'));
    await loadSettings();
    expect(get(settings).error).toBe('Recorder is not reachable');
  });
});

describe('editing and dirty state', () => {
  it('setPath changes the draft immutably and marks it dirty', async () => {
    await loaded();
    const before = get(settings).draft;
    setPath('general.close_behavior', 'exit');
    const s = get(settings);
    expect(s.draft?.general.close_behavior).toBe('exit');
    expect(s.saved?.general.close_behavior).toBe('minimize_to_tray');
    expect(s.dirty).toBe(true);
    expect(before).not.toBe(s.draft);
  });

  it('returning to the saved value is clean again and clears findings', async () => {
    await loaded();
    mocks.patchConfig.mockResolvedValue(checked());
    setPath('general.autostart', true);
    await vi.advanceTimersByTimeAsync(VALIDATE_MS);
    await flush();
    expect(get(settings).applyClass).toBe('live');

    setPath('general.autostart', false);
    const s = get(settings);
    expect(s.dirty).toBe(false);
    expect(s.applyClass).toBeNull();
    expect(s.diagnostics).toEqual([]);
  });

  it('does nothing before a load', () => {
    setPath('general.autostart', true);
    expect(get(settings).draft).toBeNull();
    expect(get(settings).dirty).toBe(false);
  });
});

describe('dry run validation', () => {
  it('waits 400 ms after the last edit, then sends only the changed leaves with dry_run', async () => {
    await loaded();
    mocks.patchConfig.mockResolvedValue(checked());
    setPath('general.close_behavior', 'exit');
    setPath('general.close_behavior', 'minimize_to_tray');
    setPath('general.close_behavior', 'exit');

    await vi.advanceTimersByTimeAsync(VALIDATE_MS - 1);
    expect(mocks.patchConfig).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    await flush();
    expect(mocks.patchConfig).toHaveBeenCalledTimes(1);
    expect(mocks.patchConfig).toHaveBeenCalledWith({ general: { close_behavior: 'exit' } }, { dryRun: true });
  });

  it('stores the apply class and restart flag from a passing check', async () => {
    await loaded();
    mocks.patchConfig.mockResolvedValue(checked(true));
    setPath('storage.per_game_folders', false);
    await validateDraft();
    const s = get(settings);
    expect(s.applyClass).toBe('restart');
    expect(s.pendingRestart).toBe(true);
    expect(s.checking).toBe(false);
  });

  it('maps rejected diagnostics to the draft and clears the apply class', async () => {
    await loaded();
    const diag = {
      code: 'CFG-S005',
      severity: 'error',
      path: 'storage.filename_template',
      message: 'unknown token',
    };
    mocks.patchConfig.mockResolvedValue(rejected([diag]));
    setPath('storage.filename_template', '{bogus} {kind}');
    await validateDraft();
    const s = get(settings);
    expect(s.diagnostics).toEqual([diag]);
    expect(s.applyClass).toBeNull();
    expect(s.pendingRestart).toBe(false);
    expect(s.draft?.storage.filename_template).toBe('{bogus} {kind}');
    expect(s.dirty).toBe(true);
  });

  it('keeps warnings from a passing check', async () => {
    await loaded();
    const warning = { code: 'CFG-S007', severity: 'warning', path: 'storage.filename_template', message: 'no kind' };
    mocks.patchConfig.mockResolvedValue(checked(false, [warning]));
    setPath('storage.filename_template', '{game} {yyyy}');
    await validateDraft();
    expect(get(settings).diagnostics).toEqual([warning]);
  });

  it('ignores a slow answer that a newer edit has overtaken', async () => {
    await loaded();
    const first = deferred<Reply>();
    const second = deferred<Reply>();
    mocks.patchConfig.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    setPath('general.autostart', true);
    await vi.advanceTimersByTimeAsync(VALIDATE_MS);
    setPath('general.autostart', false);
    setPath('general.autostart', true);
    await vi.advanceTimersByTimeAsync(VALIDATE_MS);
    expect(mocks.patchConfig).toHaveBeenCalledTimes(2);

    second.resolve(checked(true));
    await flush();
    expect(get(settings).pendingRestart).toBe(true);

    first.resolve(checked(false, [{ code: 'X', severity: 'warning', path: 'general.autostart', message: 'stale' }]));
    await flush();
    const s = get(settings);
    expect(s.pendingRestart).toBe(true);
    expect(s.diagnostics).toEqual([]);
  });

  it('reports a transport failure during the check', async () => {
    await loaded();
    mocks.patchConfig.mockRejectedValue(new Error('pipe closed'));
    setPath('general.autostart', true);
    await validateDraft();
    expect(get(settings).error).toBe('pipe closed');
    expect(get(settings).checking).toBe(false);
  });

  it('does not call the daemon when nothing changed', async () => {
    await loaded();
    await validateDraft();
    expect(mocks.patchConfig).not.toHaveBeenCalled();
  });
});

describe('save', () => {
  it('sends the real patch, adopts the daemon view, and reports Applied', async () => {
    await loaded();
    setPath('general.close_behavior', 'exit');
    mocks.patchConfig.mockResolvedValue(applied(false));
    const normalised = clone({ ...CONFIG, general: { ...CONFIG.general, close_behavior: 'exit' as const } });
    mocks.getConfig.mockResolvedValue(configReply(normalised));

    const ok = await save();

    expect(ok).toBe(true);
    expect(mocks.patchConfig).toHaveBeenCalledWith({ general: { close_behavior: 'exit' } });
    const s = get(settings);
    expect(s.dirty).toBe(false);
    expect(s.saved).toEqual(normalised);
    expect(s.notice).toBe('Applied');
    expect(s.saving).toBe(false);
    expect(s.pendingRestart).toBe(false);
  });

  it('says the change applies when recording stops when restart is required', async () => {
    await loaded();
    setPath('storage.root', 'D:\\Clips');
    mocks.patchConfig.mockResolvedValue(applied(true));
    mocks.getConfig.mockResolvedValue(configReply({ ...CONFIG, storage: { ...CONFIG.storage, root: 'D:\\Clips' } }));

    await save();

    const s = get(settings);
    expect(s.notice).toBe('Will apply when recording stops');
    expect(s.pendingRestart).toBe(true);
    expect(s.applyClass).toBe('next_part');
  });

  it('keeps the draft and shows findings when the daemon rejects the save', async () => {
    await loaded();
    setPath('storage.quota_gb', 0);
    setPath('storage.filename_template', '{game}');
    const diag = {
      code: 'CFG-S008',
      severity: 'error',
      path: 'storage.filename_template',
      message: 'needs a unique part',
    };
    mocks.patchConfig.mockResolvedValue(rejected([diag]));

    const ok = await save();

    expect(ok).toBe(false);
    const s = get(settings);
    expect(s.draft?.storage.filename_template).toBe('{game}');
    expect(s.dirty).toBe(true);
    expect(s.diagnostics).toEqual([diag]);
    expect(s.notice).toBe('Not saved. needs a unique part');
    expect(mocks.getConfig).toHaveBeenCalledTimes(1);
  });

  it('shows the rejected field\'s message when no field is on screen', async () => {
    await loaded();
    setPath('storage.filename_template', '{game}');
    const diag = {
      code: 'CFG-V006',
      severity: 'error',
      path: 'video.max_bitrate_kbps',
      message: 'video.max_bitrate_kbps (15000) must be at least video.bitrate_kbps (30000)',
    };
    mocks.patchConfig.mockResolvedValue(rejected([diag]));

    await save();

    const s = get(settings);
    expect(s.notice).toContain('video.max_bitrate_kbps');
    expect(s.notice).toBe(`Not saved. ${diag.message}`);
  });

  it('reports a transport failure and leaves the draft alone', async () => {
    await loaded();
    setPath('general.autostart', true);
    mocks.patchConfig.mockRejectedValue(new Error('Recorder is not reachable'));

    const ok = await save();

    expect(ok).toBe(false);
    const s = get(settings);
    expect(s.error).toBe('Recorder is not reachable');
    expect(s.saving).toBe(false);
    expect(s.dirty).toBe(true);
  });

  it('does nothing when there is nothing to save', async () => {
    await loaded();
    expect(await save()).toBe(false);
    expect(mocks.patchConfig).not.toHaveBeenCalled();
  });

  it('does nothing before a load', async () => {
    expect(await save()).toBe(false);
    expect(mocks.patchConfig).not.toHaveBeenCalled();
  });

  it('a save does not echo back as an external change', async () => {
    await loaded();
    setPath('general.autostart', true);
    const saved = { ...CONFIG, general: { ...CONFIG.general, autostart: true } };
    mocks.patchConfig.mockResolvedValue(applied(false));
    mocks.getConfig.mockResolvedValue(configReply(saved));
    await save();

    mocks.getConfig.mockResolvedValue(configReply(saved));
    await checkExternal();
    expect(get(settings).external).toBeNull();
    expect(get(settings).dirty).toBe(false);
  });

  it('keeps edits made while the save was in flight', async () => {
    await loaded();
    setPath('general.autostart', true);
    const reply = deferred<Reply>();
    mocks.patchConfig.mockReturnValue(reply.promise);
    mocks.getConfig.mockResolvedValue(configReply({ ...CONFIG, general: { ...CONFIG.general, autostart: true } }));

    const pending = save();
    setPath('general.start_minimized', false);
    reply.resolve(applied(false));
    await pending;

    const s = get(settings);
    expect(s.draft?.general.autostart).toBe(true);
    expect(s.draft?.general.start_minimized).toBe(false);
    expect(s.dirty).toBe(true);
  });
});

describe('revert', () => {
  it('restores the saved config and clears findings', async () => {
    await loaded();
    setPath('general.close_behavior', 'exit');
    mocks.patchConfig.mockResolvedValue(rejected([{ code: 'X', severity: 'error', path: 'general', message: 'no' }]));
    await validateDraft();

    revert();

    const s = get(settings);
    expect(s.draft).toEqual(CONFIG);
    expect(s.dirty).toBe(false);
    expect(s.diagnostics).toEqual([]);
    expect(s.notice).toBeNull();
  });

  it('cancels a pending dry run', async () => {
    await loaded();
    setPath('general.close_behavior', 'exit');
    revert();
    await vi.advanceTimersByTimeAsync(VALIDATE_MS * 2);
    expect(mocks.patchConfig).not.toHaveBeenCalled();
  });
});

describe('external changes', () => {
  it('adopts a newer daemon config silently when the draft is clean', async () => {
    await loaded();
    const outside = { ...CONFIG, storage: { ...CONFIG.storage, quota_gb: 25 } };
    mocks.getConfig.mockResolvedValue(configReply(outside));

    await checkExternal();

    const s = get(settings);
    expect(s.saved).toEqual(outside);
    expect(s.draft).toEqual(outside);
    expect(s.external).toBeNull();
    expect(s.dirty).toBe(false);
  });

  it('raises the banner instead of overwriting a dirty draft', async () => {
    await loaded();
    setPath('general.autostart', true);
    const outside = { ...CONFIG, storage: { ...CONFIG.storage, quota_gb: 25 } };
    mocks.getConfig.mockResolvedValue(configReply(outside));

    await checkExternal();

    const s = get(settings);
    expect(s.external).toEqual(outside);
    expect(s.draft?.general.autostart).toBe(true);
    expect(s.draft?.storage.quota_gb).toBe(0);
  });

  it('does nothing when the daemon config matches what we hold', async () => {
    await loaded();
    mocks.getConfig.mockResolvedValue(configReply(CONFIG));
    await checkExternal();
    expect(get(settings).external).toBeNull();
    expect(get(settings).dirty).toBe(false);
  });

  it('Reload adopts the outside config and drops the draft', async () => {
    await loaded();
    setPath('general.autostart', true);
    const outside = { ...CONFIG, storage: { ...CONFIG.storage, quota_gb: 25 } };
    mocks.getConfig.mockResolvedValue(configReply(outside));
    await checkExternal();

    acceptExternal();

    const s = get(settings);
    expect(s.draft).toEqual(outside);
    expect(s.saved).toEqual(outside);
    expect(s.dirty).toBe(false);
    expect(s.external).toBeNull();
  });

  it('Keep mine keeps the draft and makes the daemon config the baseline', async () => {
    await loaded();
    setPath('general.autostart', true);
    const outside = { ...CONFIG, storage: { ...CONFIG.storage, quota_gb: 25 } };
    mocks.getConfig.mockResolvedValue(configReply(outside));
    await checkExternal();

    keepMine();
    const s = get(settings);
    expect(s.external).toBeNull();
    expect(s.saved).toEqual(outside);
    expect(s.draft?.general.autostart).toBe(true);
    expect(s.draft?.storage.quota_gb).toBe(0);
    expect(s.dirty).toBe(true);

    // The next save sends only what the user changed, and reverts the outside quota
    // only if the user had also set it. Here the draft still has quota 0, so it is sent.
    mocks.patchConfig.mockResolvedValue(applied(false));
    mocks.getConfig.mockResolvedValue(configReply({ ...outside, general: { ...CONFIG.general, autostart: true }, storage: { ...CONFIG.storage, quota_gb: 0 } }));
    await save();
    expect(mocks.patchConfig).toHaveBeenCalledWith({ general: { autostart: true }, storage: { quota_gb: 0 } });
  });

  it('Keep mine becomes clean when the draft already equals the outside config', async () => {
    await loaded();
    setPath('storage.quota_gb', 25);
    const outside = { ...CONFIG, storage: { ...CONFIG.storage, quota_gb: 25 } };
    mocks.getConfig.mockResolvedValue(configReply(outside));
    await checkExternal();
    expect(get(settings).external).toEqual(outside);

    keepMine();

    expect(get(settings).dirty).toBe(false);
    expect(get(settings).saved).toEqual(outside);
  });

  it('does not read the daemon while a save is in flight', async () => {
    await loaded();
    setPath('general.autostart', true);
    mocks.patchConfig.mockReturnValue(new Promise(() => {}));
    void save();
    await flush();
    mocks.getConfig.mockClear();
    await checkExternal();
    expect(mocks.getConfig).not.toHaveBeenCalled();
  });

  it('ignores a failed config read', async () => {
    await loaded();
    mocks.getConfig.mockRejectedValue(new Error('down'));
    await checkExternal();
    expect(get(settings).external).toBeNull();
  });

  it('onRecorderConnected loads when nothing is loaded yet', async () => {
    mocks.getConfig.mockResolvedValue(configReply(CONFIG));
    onRecorderConnected();
    await flush();
    expect(mocks.getConfig).toHaveBeenCalledTimes(1);
    expect(get(settings).saved).toEqual(CONFIG);
  });

  it('onRecorderConnected checks for outside changes once loaded', async () => {
    await loaded();
    mocks.getConfig.mockResolvedValue(configReply(CONFIG));
    onRecorderConnected();
    await flush();
    expect(mocks.getConfig).toHaveBeenCalledTimes(2);
    expect(mocks.getConfig).toHaveBeenLastCalledWith();
  });

  it('a config_changed event from elsewhere triggers the check', async () => {
    await loaded();
    startSettingsSync();
    expect(mocks.onEvent).toHaveBeenCalledTimes(1);
    mocks.getConfig.mockResolvedValue(configReply(CONFIG));

    mocks.handler?.({ type: 'config_changed', data: { apply_class: 'live', restart_required: false } });
    await flush();
    expect(mocks.getConfig).toHaveBeenCalledTimes(2);

    mocks.handler?.({ type: 'settings_reloaded', data: { ok: true, errors: [] } });
    await flush();
    expect(mocks.getConfig).toHaveBeenCalledTimes(3);

    mocks.handler?.({ type: 'clip_saved', data: { clip_id: 'c', path: 'p', duration_ms: 1, bytes: 1 } });
    await flush();
    expect(mocks.getConfig).toHaveBeenCalledTimes(3);
  });

  it('startSettingsSync subscribes only once', () => {
    startSettingsSync();
    startSettingsSync();
    expect(mocks.onEvent).toHaveBeenCalledTimes(1);
  });
});
