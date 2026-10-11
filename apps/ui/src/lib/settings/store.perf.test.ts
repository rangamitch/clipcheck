import { get } from 'svelte/store';
import { performance } from 'node:perf_hooks';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Reply } from '../daemon';

const mocks = vi.hoisted(() => ({
  getConfig: vi.fn(),
  patchConfig: vi.fn(),
  onEvent: vi.fn(),
}));

vi.mock('../daemon', () => ({
  getConfig: (...args: unknown[]) => mocks.getConfig(...args),
  patchConfig: (...args: unknown[]) => mocks.patchConfig(...args),
  onEvent: (handler: (ev: unknown) => void) => mocks.onEvent(handler) ?? (() => {}),
}));

import {
  VALIDATE_MS,
  loadSettings,
  resetSettings,
  save,
  setPath,
  settings,
  updateDraft,
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

function applied(restart = false): Reply {
  return {
    type: 'config_applied',
    data: { apply_class: restart ? 'next_part' : 'live', restart_required: restart, diagnostics: [] } as never,
  };
}

function checked(): Reply {
  return {
    type: 'config_checked',
    data: { apply_class: 'live', restart_required: false, diagnostics: [] } as never,
  };
}

async function flush(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
  mocks.getConfig.mockReset();
  mocks.patchConfig.mockReset();
  mocks.onEvent.mockReset();
  resetSettings();
});

afterEach(() => {
  vi.useRealTimers();
});

async function loaded(): Promise<void> {
  mocks.getConfig.mockResolvedValue(configReply(CONFIG));
  await loadSettings();
}

describe('#182 draft equality and patch scope', () => {
  it('a nested edit marks the draft dirty and the patch holds only that path', async () => {
    await loaded();
    mocks.patchConfig.mockResolvedValue(applied(false));

    setPath('storage.quota_gb', 5);
    expect(get(settings).dirty).toBe(true);

    await save();
    expect(mocks.patchConfig).toHaveBeenCalledTimes(1);
    expect(mocks.patchConfig).toHaveBeenCalledWith({ storage: { quota_gb: 5 } });
  });

  it('setting the value back clears dirty', async () => {
    await loaded();
    setPath('storage.quota_gb', 5);
    expect(get(settings).dirty).toBe(true);

    setPath('storage.quota_gb', 0);
    expect(get(settings).dirty).toBe(false);
  });

  it('updateDraft with a nested change also marks dirty, and reverting clears it', async () => {
    await loaded();
    updateDraft((d) => ({ ...d, detection: { ...d.detection, stop_grace_s: 30 } }));
    expect(get(settings).dirty).toBe(true);

    updateDraft((d) => ({ ...d, detection: { ...d.detection, stop_grace_s: 10 } }));
    expect(get(settings).dirty).toBe(false);
  });

  it('sections not touched by an edit keep referential identity', async () => {
    await loaded();
    const before = get(settings).draft as ConfigDoc;

    setPath('storage.quota_gb', 5);
    const after = get(settings).draft as ConfigDoc;

    expect(after).not.toBe(before);
    expect(after.storage).not.toBe(before.storage);
    expect(after.general).toBe(before.general);
    expect(after.detection).toBe(before.detection);
    expect(after.games).toBe(before.games);
  });
});

describe('#182 rapid slider updates', () => {
  it('1000 rapid updates to one path finish quickly and leave a consistent state', async () => {
    await loaded();
    mocks.patchConfig.mockResolvedValue(checked());

    const start = performance.now();
    for (let i = 1; i <= 1000; i += 1) {
      setPath('storage.quota_gb', i);
    }
    const elapsed = performance.now() - start;

    expect(elapsed).toBeLessThan(500);

    const s = get(settings);
    expect((s.draft as ConfigDoc).storage.quota_gb).toBe(1000);
    expect(s.dirty).toBe(true);
    expect((s.saved as ConfigDoc).storage.quota_gb).toBe(0);

    // The debounce coalesces the burst into one dry run after the quiet period.
    await vi.advanceTimersByTimeAsync(VALIDATE_MS);
    await flush();
    expect(mocks.patchConfig).toHaveBeenCalledTimes(1);
    expect(mocks.patchConfig).toHaveBeenCalledWith({ storage: { quota_gb: 1000 } }, { dryRun: true });
  });
});
