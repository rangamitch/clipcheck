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
  fieldState,
  loadSettings,
  resetSettings,
  save,
  setPath,
  settings,
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

function rejected(diagnostics: unknown[]): Reply {
  return { type: 'config_rejected', data: { diagnostics } as never };
}

const GENERIC = 'Not saved. Fix the highlighted fields.';

/** Holds a row mounted (its fieldState subscription stays open) until the test ends. */
let unmounts: Array<() => void> = [];

function mount(path: string): void {
  unmounts.push(fieldState(path).subscribe(() => {}));
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
  for (const unmount of unmounts) unmount();
  unmounts = [];
  vi.useRealTimers();
});

async function loaded(): Promise<void> {
  mocks.getConfig.mockResolvedValue(configReply(CONFIG));
  await loadSettings();
}

describe('rejected save notice (tester)', () => {
  it('rejected field not on screen shows its message', async () => {
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

    expect(get(settings).notice).toBe(`Not saved. ${diag.message}`);
  });

  it('rejected field on screen shows the generic text', async () => {
    await loaded();
    mount('storage.filename_template');
    setPath('storage.filename_template', '{game}');
    const diag = {
      code: 'CFG-S008',
      severity: 'error',
      path: 'storage.filename_template',
      message: 'needs a unique part',
    };
    mocks.patchConfig.mockResolvedValue(rejected([diag]));

    await save();

    expect(get(settings).notice).toBe(GENERIC);
  });

  it('two errors, one on screen and one off screen, shows the generic text', async () => {
    await loaded();
    mount('storage.filename_template');
    setPath('storage.filename_template', '{game}');
    const onScreen = {
      code: 'CFG-S008',
      severity: 'error',
      path: 'storage.filename_template',
      message: 'needs a unique part',
    };
    const offScreen = {
      code: 'CFG-V006',
      severity: 'error',
      path: 'video.max_bitrate_kbps',
      message: 'video.max_bitrate_kbps (15000) must be at least video.bitrate_kbps (30000)',
    };
    mocks.patchConfig.mockResolvedValue(rejected([offScreen, onScreen]));

    await save();

    expect(get(settings).notice).toBe(GENERIC);
  });

  it('warnings only shows the generic text', async () => {
    await loaded();
    setPath('storage.filename_template', '{game}');
    const warning = {
      code: 'CFG-W001',
      severity: 'warning',
      path: 'video.max_bitrate_kbps',
      message: 'a warning message that is not an error',
    };
    mocks.patchConfig.mockResolvedValue(rejected([warning]));

    await save();

    expect(get(settings).notice).toBe(GENERIC);
  });

  it('errors with no field mounted name the first error with its long message exactly', async () => {
    await loaded();
    setPath('storage.filename_template', '{game}');
    const longMessage =
      'storage.filename_template must contain a unique part such as {yyyy-MM-dd HH-mm-ss} or {counter} so that ' +
      'two recordings of the same game never write to the same file, and the template you entered has neither ' +
      'of those parts, so the recorder refuses to save it until one of them is added to the template text';
    const first = {
      code: 'CFG-S008',
      severity: 'error',
      path: 'storage.filename_template',
      message: longMessage,
    };
    const second = {
      code: 'CFG-V006',
      severity: 'error',
      path: 'video.max_bitrate_kbps',
      message: 'second error message',
    };
    mocks.patchConfig.mockResolvedValue(rejected([first, second]));

    await save();

    expect(get(settings).notice).toBe('Not saved. ' + longMessage);
  });
});
