/**
 * Regression tests for the settings batch (#59 #69 #174 #175 #178 #180 #182 #187).
 * Pure helpers and the store only: no component rendering (the repo has no Svelte test library).
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DiagnosticDto, Reply } from '../daemon';

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

import { MAX_APP_NAME_CHARS, appDisplayName } from './audio';
import {
  AAC_BITRATE_MAX_KBPS,
  AAC_BITRATE_MIN_KBPS,
  APP_NAME_MAX_CHARS,
  BITRATE_MAX_KBPS,
  BITRATE_MIN_KBPS,
  BUFFER_MAX_SECONDS,
  BUFFER_MIN_SECONDS,
  CLIP_MAX_SECONDS as BOUNDS_CLIP_MAX_SECONDS,
  CLIP_MIN_SECONDS as BOUNDS_CLIP_MIN_SECONDS,
  CLIP_NAME_MAX_CHARS as BOUNDS_CLIP_NAME_MAX_CHARS,
  MAX_CLIPS as BOUNDS_MAX_CLIPS,
  QUOTA_GB_MAX,
  RAM_MAX_MB_MAX,
  RAM_MAX_MB_MIN,
  STOP_GRACE_MAX_S,
  STOP_GRACE_MIN_S,
} from './bounds';
import {
  CLIP_MAX_SECONDS,
  CLIP_MIN_SECONDS,
  CLIP_NAME_MAX_CHARS,
  CLIP_PRESETS,
  MAX_CLIPS,
} from './clips';
import { frameCoalesced } from './frame';
import { formatChord, heldModifiersLabel, sessionChordsOf } from './hotkeys';
import {
  BITRATE_MAX_KBPS as PURE_BITRATE_MAX_KBPS,
  BITRATE_MIN_KBPS as PURE_BITRATE_MIN_KBPS,
  appliesAfterGameEnds,
  charCount,
  diagnosticAt,
  fieldDiagnostic,
  presetCards,
} from './pure';
import {
  VALIDATE_MS,
  fieldState,
  loadSettings,
  resetSettings,
  setPath,
  settings,
  type ConfigDoc,
  type FieldState,
  type SessionHotkeysConfig,
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

function checkedWithRestart(restart: boolean): Reply {
  return {
    type: 'config_checked',
    data: {
      apply_class: restart ? 'next_part' : 'live',
      restart_required: restart,
      diagnostics: [],
    } as never,
  };
}

async function flush(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
}

async function loaded(): Promise<void> {
  mocks.getConfig.mockResolvedValue(configReply(CONFIG));
  await loadSettings();
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
  vi.unstubAllGlobals();
});

const diag = (path: string, severity: 'error' | 'warning', code: string): DiagnosticDto => ({
  code,
  severity,
  path,
  message: `message ${code}`,
});

// ---------------------------------------------------------------------------
// #59: the Coming-next placeholder and its dead branch are gone
// ---------------------------------------------------------------------------

/** Every .svelte file under settings/, found at build time (no Node file APIs needed). */
const SETTINGS_SVELTE = import.meta.glob('./**/*.svelte');

describe('issue_59 placeholder removal', () => {
  it('issue_59 no ComingNext.svelte remains anywhere under settings/', () => {
    expect(Object.keys(SETTINGS_SVELTE).some((p) => p.endsWith('/ComingNext.svelte'))).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// #175: "Applies after game ends" mirrors apply_class NextPart in validate.rs
// ---------------------------------------------------------------------------

describe('issue_175 appliesAfterGameEnds mirrors apply_class', () => {
  it.each([
    'storage.root',
    'storage.per_game_folders',
    'storage.filename_template',
    'video.container',
    'games.abc.folder',
  ])('issue_175 %s is NextPart (deferred)', (path) => {
    expect(appliesAfterGameEnds(path)).toBe(true);
  });

  it.each([
    'advanced.log_dir',
    'advanced.log_level',
    'advanced.encoder_extra_args',
    'games.abc.display_name',
    'games.abc.auto_record',
    'games.abc.video',
    'video.preset',
    'audio.bitrate_kbps',
    'general.close_behavior',
  ])('issue_175 %s is not deferred', (path) => {
    expect(appliesAfterGameEnds(path)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// #178: names are counted in code points, like the daemon's chars().count()
// ---------------------------------------------------------------------------

describe('issue_178 name length counts characters', () => {
  it('issue_178 charCount counts an emoji as one character', () => {
    const name = '😀'.repeat(10) + 'a'.repeat(10);
    expect(name.length).toBe(30);
    expect(charCount(name)).toBe(20);
  });

  it('issue_178 a 32-emoji clip name is exactly at the limit, not over it', () => {
    const name = '😀'.repeat(CLIP_NAME_MAX_CHARS);
    expect(name.length).toBeGreaterThan(CLIP_NAME_MAX_CHARS);
    expect(charCount(name)).toBe(CLIP_NAME_MAX_CHARS);
    expect(charCount(name) > CLIP_NAME_MAX_CHARS).toBe(false);
  });

  it('issue_178 a 33-emoji clip name is over the limit', () => {
    expect(charCount('😀'.repeat(CLIP_NAME_MAX_CHARS + 1)) > CLIP_NAME_MAX_CHARS).toBe(true);
  });

  it('issue_178 appDisplayName caps by characters and never splits a surrogate pair', () => {
    const shown = appDisplayName('😀'.repeat(40), 'game.exe');
    expect(Array.from(shown)).toHaveLength(MAX_APP_NAME_CHARS);
    expect(shown).toBe('😀'.repeat(MAX_APP_NAME_CHARS));
  });
});

// ---------------------------------------------------------------------------
// #180: the bounds module is the one source, and the re-exports agree with it
// ---------------------------------------------------------------------------

describe('issue_180 shared bounds', () => {
  it('issue_180 bounds hold the values the UI enforces', () => {
    expect(BUFFER_MIN_SECONDS).toBe(30);
    expect(BUFFER_MAX_SECONDS).toBe(3600);
    expect(RAM_MAX_MB_MIN).toBe(64);
    expect(RAM_MAX_MB_MAX).toBe(4096);
    expect(CLIP_MIN_SECONDS).toBe(5);
    expect(CLIP_MAX_SECONDS).toBe(3600);
    expect(CLIP_NAME_MAX_CHARS).toBe(32);
    expect(APP_NAME_MAX_CHARS).toBe(32);
    expect(MAX_CLIPS).toBe(8);
    expect(QUOTA_GB_MAX).toBe(100000);
    expect(BITRATE_MIN_KBPS).toBe(1000);
    expect(BITRATE_MAX_KBPS).toBe(100000);
    expect(AAC_BITRATE_MIN_KBPS).toBe(128);
    expect(AAC_BITRATE_MAX_KBPS).toBe(320);
    expect(STOP_GRACE_MIN_S).toBe(0);
    expect(STOP_GRACE_MAX_S).toBe(300);
  });

  it('issue_180 clips.ts and audio.ts re-export the bounds module values, not copies', () => {
    expect(CLIP_MIN_SECONDS).toBe(BOUNDS_CLIP_MIN_SECONDS);
    expect(CLIP_MAX_SECONDS).toBe(BOUNDS_CLIP_MAX_SECONDS);
    expect(CLIP_NAME_MAX_CHARS).toBe(BOUNDS_CLIP_NAME_MAX_CHARS);
    expect(MAX_CLIPS).toBe(BOUNDS_MAX_CLIPS);
    expect(MAX_APP_NAME_CHARS).toBe(APP_NAME_MAX_CHARS);
  });

  it('issue_180 pure.ts bitrate limits equal the bounds module', () => {
    expect(PURE_BITRATE_MIN_KBPS).toBe(BITRATE_MIN_KBPS);
    expect(PURE_BITRATE_MAX_KBPS).toBe(BITRATE_MAX_KBPS);
  });

  it('issue_180 the clip-length presets all fit inside the clip bounds', () => {
    for (const p of CLIP_PRESETS) {
      expect(p.seconds).toBeGreaterThanOrEqual(BOUNDS_CLIP_MIN_SECONDS);
      expect(p.seconds).toBeLessThanOrEqual(BOUNDS_CLIP_MAX_SECONDS);
    }
  });

  it('issue_180 every mirrored bound still appears in crates/core/src/config/validate.rs', () => {
    // Read via node fs: a `?raw` import cannot reach outside the Vite root (apps/ui).
    const validatePath = fileURLToPath(
      new URL('../../../../../crates/core/src/config/validate.rs', import.meta.url),
    );
    const rust = readFileSync(validatePath, 'utf8');
    expect(rust.length, 'validate.rs was not found').toBeGreaterThan(0);
    // Each needle is the literal check in validate.rs that a bounds.ts constant mirrors.
    const needles = [
      'in_range(s.quota_gb, 1, 100_000)', // QUOTA_GB_MAX
      'in_range(d.stop_grace_s, 0, 300)', // STOP_GRACE_MIN_S / MAX_S
      'in_range(kbps, 1000, 100_000)', // BITRATE_MIN / MAX_KBPS
      'in_range(a.bitrate_kbps, 128, 320)', // AAC_BITRATE_MIN / MAX_KBPS
      'in_range(r.max_seconds, 30, 3600)', // BUFFER_MIN / MAX_SECONDS
      'in_range(r.ram_max_mb, 64, 4096)', // RAM_MAX_MB_MIN / MAX
      'const MAX_CLIP_HOTKEYS: usize = 8;', // MAX_CLIPS
      'in_range(clip.name.chars().count(), 1, 32)', // CLIP_NAME_MAX_CHARS
      'in_range(app.name.trim().chars().count(), 1, 32)', // APP_NAME_MAX_CHARS
      'clip.seconds < 5', // CLIP_MIN_SECONDS
    ];
    for (const needle of needles) {
      expect(rust, `validate.rs no longer contains: ${needle}`).toContain(needle);
    }
  });
});

// ---------------------------------------------------------------------------
// #174: no preset card claims to be the default
// ---------------------------------------------------------------------------

describe('issue_174 preset labels', () => {
  it('issue_174 no preset card name carries a "(default)" claim that the UI cannot back', () => {
    for (const card of presetCards()) {
      expect(card.name).not.toMatch(/default/i);
    }
  });
});

// ---------------------------------------------------------------------------
// #69: one shared severity rule for both diagnostic lookups, one modifier list for chords
// ---------------------------------------------------------------------------

describe('issue_69 shared diagnostic rule', () => {
  it('issue_69 an error beats an earlier warning in both lookups', () => {
    const list = [diag('audio.tracks', 'warning', 'w1'), diag('audio.tracks', 'error', 'e1')];
    expect(fieldDiagnostic('audio.tracks', list)).toEqual({ code: 'e1', severity: 'error', message: 'message e1' });
    expect(diagnosticAt('audio.tracks', list)).toEqual({ code: 'e1', severity: 'error', message: 'message e1' });
  });

  it('issue_69 the first warning wins in both lookups', () => {
    const list = [diag('storage.root', 'warning', 'w1'), diag('storage.root', 'warning', 'w2')];
    expect(fieldDiagnostic('storage.root', list)?.code).toBe('w1');
    expect(diagnosticAt('storage.root', list)?.code).toBe('w1');
  });
});

describe('issue_69 chord text uses one modifier order', () => {
  const all = { ctrl: true, alt: true, shift: true, win: true };

  it('issue_69 formatChord writes Ctrl, Alt, Shift, Win then the key', () => {
    expect(formatChord(all, 'F13')).toBe('Ctrl+Alt+Shift+Win+F13');
  });

  it('issue_69 heldModifiersLabel uses the same order with spaced separators', () => {
    expect(heldModifiersLabel(all)).toBe('Ctrl + Alt + Shift + Win');
  });

  it('issue_69 a partial set keeps the order and skips the rest', () => {
    const mods = { ctrl: false, alt: true, shift: false, win: true };
    expect(formatChord(mods, 'A')).toBe('Alt+Win+A');
    expect(heldModifiersLabel(mods)).toBe('Alt + Win');
  });

  it('issue_69 with nothing held the label is empty', () => {
    expect(heldModifiersLabel({ ctrl: false, alt: false, shift: false, win: false })).toBe('');
  });
});

// ---------------------------------------------------------------------------
// #187: one session-chord selector for the Replay and Hotkeys tabs
// ---------------------------------------------------------------------------

describe('issue_187 session chord list', () => {
  it('issue_187 lists split, start/stop, buffer and mute in that order, leaving out unbound entries', () => {
    const session = {
      split: 'Ctrl+F13',
      stop_start: '',
      toggle_buffer: 'Ctrl+F14',
      mute_mic: 'Ctrl+F15',
    } as SessionHotkeysConfig;
    expect(sessionChordsOf(session)).toEqual(['Ctrl+F13', 'Ctrl+F14', 'Ctrl+F15']);
  });

  it('issue_187 returns an empty list when there is no session config or nothing is bound', () => {
    expect(sessionChordsOf(undefined)).toEqual([]);
    expect(
      sessionChordsOf({ split: '', stop_start: '', toggle_buffer: '', mute_mic: '' } as SessionHotkeysConfig),
    ).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// #182: per-row state notifies only on its own change; slider writes coalesce per frame
// ---------------------------------------------------------------------------

describe('issue_182 fieldState notifies a row only for its own change', () => {
  it('issue_182 writes to one path do not notify another row', async () => {
    await loaded();
    const quota: FieldState[] = [];
    const grace: FieldState[] = [];
    const unsubQuota = fieldState('storage.quota_gb').subscribe((s) => quota.push(s));
    const unsubGrace = fieldState('detection.stop_grace_s').subscribe((s) => grace.push(s));
    const graceBefore = grace.length;

    setPath('storage.quota_gb', 5);
    expect(quota.at(-1)).toEqual({ changed: true, deferred: false, diag: null });

    // Repeated writes that keep the row changed do not re-notify it, and the other row stays quiet.
    const quotaAfterFirst = quota.length;
    for (let i = 6; i < 60; i += 1) setPath('storage.quota_gb', i);
    expect(quota.length).toBe(quotaAfterFirst);
    expect(grace.length).toBe(graceBefore);

    // Setting it back to saved is a real change for this row, so it notifies once.
    setPath('storage.quota_gb', 0);
    expect(quota.at(-1)?.changed).toBe(false);
    expect(grace.length).toBe(graceBefore);

    unsubQuota();
    unsubGrace();
  });

  it('issue_182 a change that waits for the game to end is reported as deferred', async () => {
    await loaded();
    mocks.patchConfig.mockResolvedValue(checkedWithRestart(true));
    const root: FieldState[] = [];
    const general: FieldState[] = [];
    const unsubRoot = fieldState('storage.root').subscribe((s) => root.push(s));
    const unsubGeneral = fieldState('general.close_behavior').subscribe((s) => general.push(s));

    setPath('storage.root', 'D:\\Clips');
    await vi.advanceTimersByTimeAsync(VALIDATE_MS);
    await flush();

    expect(get(settings).pendingRestart).toBe(true);
    expect(root.at(-1)).toEqual({ changed: true, deferred: true, diag: null });
    expect(general.at(-1)?.deferred).toBe(false);

    unsubRoot();
    unsubGeneral();
  });
});

describe('issue_182 frameCoalesced', () => {
  /** Captures frame callbacks instead of running them, so a test decides when a frame happens. */
  function fakeFrames(): { runFrame: () => void; pending: () => number } {
    const queue: FrameRequestCallback[] = [];
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      queue.push(cb);
      return queue.length;
    });
    return {
      runFrame: () => {
        const batch = queue.splice(0);
        for (const cb of batch) cb(0);
      },
      pending: () => queue.length,
    };
  }

  it('issue_182 many calls in one frame apply once, with the last value', () => {
    const { runFrame, pending } = fakeFrames();
    const apply = vi.fn<(v: number) => void>();
    const coalesced = frameCoalesced(apply);

    for (let v = 1; v <= 100; v += 1) coalesced(v);
    expect(apply).not.toHaveBeenCalled();
    expect(pending()).toBe(1);

    runFrame();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(apply).toHaveBeenCalledWith(100);
  });

  it('issue_182 a call after a frame schedules a new frame', () => {
    const { runFrame, pending } = fakeFrames();
    const apply = vi.fn<(v: number) => void>();
    const coalesced = frameCoalesced(apply);

    coalesced(1);
    runFrame();
    coalesced(2);
    expect(pending()).toBe(1);
    runFrame();

    expect(apply.mock.calls).toEqual([[1], [2]]);
  });

  it('issue_182 applies at once when requestAnimationFrame is missing', () => {
    vi.stubGlobal('requestAnimationFrame', undefined);
    const apply = vi.fn<(v: number) => void>();
    const coalesced = frameCoalesced(apply);

    coalesced(7);
    coalesced(8);
    expect(apply.mock.calls).toEqual([[7], [8]]);
  });
});
