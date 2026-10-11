import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

const invokeMock = vi.fn();
const listenMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import type { Event, StatusDto } from './daemon';
import {
  INITIAL_CONFIG,
  INITIAL_STATUS,
  POLL_MS,
  TOAST_MS,
  applyEvent,
  applySnapshot,
  baseName,
  describeChip,
  elapsedMs,
  exeDisplayName,
  formatClock,
  handleConnection,
  liveConfig,
  liveStatus,
  now,
  parseLiveConfig,
  pushToast,
  reduceConnection,
  reduceEvent,
  toasts,
  type LiveStatus,
} from './status';

const LOCAL = 1_000_000;
const gameRef = { game_id: 'g1', exe: 'Apex.exe', pid: 42, title: 'Apex' };

function live(over: Partial<LiveStatus> = {}): LiveStatus {
  return { ...INITIAL_STATUS, connected: true, ...over };
}

function snap(over: Partial<StatusDto> = {}): StatusDto {
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
    server_unix_ms: LOCAL,
    ...over,
  } as StatusDto;
}

beforeEach(() => {
  invokeMock.mockReset();
  liveStatus.set(INITIAL_STATUS);
  liveConfig.set(INITIAL_CONFIG);
  toasts.set([]);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('constants', () => {
  it('uses a 4 s toast and a 30 s poll', () => {
    expect(TOAST_MS).toBe(4000);
    expect(POLL_MS).toBe(30000);
  });
});

describe('elapsedMs', () => {
  it('returns the difference on a shared clock', () => {
    expect(elapsedMs(1000, 5000, 0)).toBe(4000);
  });
  it('applies the server offset', () => {
    // Server reads 2 s ahead of local, so local 5 s plus offset 2 s minus start 1 s is 6 s.
    expect(elapsedMs(1_000, 5_000, 2_000)).toBe(6000);
  });
  it('stays correct when the server clock is behind local time', () => {
    // Local is 3 s ahead of the server. The session started 10 s before the snapshot.
    const serverStart = LOCAL - 3000 - 10_000;
    expect(elapsedMs(serverStart, LOCAL + 60_000, -3000)).toBe(70_000);
  });
  it('clamps to zero when the start is in the future', () => {
    expect(elapsedMs(10_000, 5_000, 0)).toBe(0);
  });
  it('returns zero for non-finite inputs', () => {
    expect(elapsedMs(Number.NaN, 5_000, 0)).toBe(0);
    expect(elapsedMs(1_000, Number.POSITIVE_INFINITY, 0)).toBe(0);
  });
});

describe('formatClock', () => {
  it.each([
    [0, '00:00'],
    [59_999, '00:59'],
    [3_600_000, '1:00:00'],
    [3_599_999, '59:59'],
    [86_399_000, '23:59:59'],
    [86_400_000, '1d 0:00:00'],
    [2 * 86_400_000 + 3 * 3_600_000 + 4 * 60_000 + 5_000, '2d 3:04:05'],
  ])('formats %d ms as %s', (ms, expected) => {
    expect(formatClock(ms)).toBe(expected);
  });
  it('treats negative and NaN input as zero', () => {
    expect(formatClock(-5_000)).toBe('00:00');
    expect(formatClock(Number.NaN)).toBe('00:00');
  });
});

describe('now store', () => {
  type FakeDoc = {
    hidden: boolean;
    listeners: Map<string, () => void>;
    addEventListener: (t: string, h: () => void) => void;
    removeEventListener: (t: string) => void;
  };
  const g = globalThis as unknown as Record<string, unknown>;
  let doc: FakeDoc;
  let unsub: (() => void) | undefined;

  function fireVisibility(): void {
    doc.listeners.get('visibilitychange')?.();
  }

  beforeEach(() => {
    vi.useFakeTimers();
    doc = {
      hidden: false,
      listeners: new Map(),
      addEventListener(t, h) {
        this.listeners.set(t, h);
      },
      removeEventListener(t) {
        this.listeners.delete(t);
      },
    };
    g.document = doc;
  });

  afterEach(() => {
    unsub?.();
    unsub = undefined;
    delete g.document;
  });

  it('ticks once per second while subscribed', () => {
    const values: number[] = [];
    unsub = now.subscribe((v) => values.push(v));
    const start = values[values.length - 1];
    vi.advanceTimersByTime(3000);
    expect(values[values.length - 1] - start).toBe(3000);
  });

  it('pauses while the document is hidden and resumes when visible', () => {
    const values: number[] = [];
    unsub = now.subscribe((v) => values.push(v));
    doc.hidden = true;
    fireVisibility();
    const frozen = values[values.length - 1];
    vi.advanceTimersByTime(5000);
    expect(values[values.length - 1]).toBe(frozen);

    doc.hidden = false;
    fireVisibility();
    vi.advanceTimersByTime(1000);
    expect(values[values.length - 1]).toBe(frozen + 6000);
  });

  it('does not tick when the document starts hidden', () => {
    doc.hidden = true;
    const values: number[] = [];
    unsub = now.subscribe((v) => values.push(v));
    const count = values.length;
    vi.advanceTimersByTime(3000);
    expect(values.length).toBe(count);
  });

  it('ticks without a document (node)', () => {
    delete g.document;
    const values: number[] = [];
    unsub = now.subscribe((v) => values.push(v));
    const start = values[values.length - 1];
    vi.advanceTimersByTime(2000);
    expect(values[values.length - 1] - start).toBe(2000);
  });
});

describe('exeDisplayName and baseName', () => {
  it('strips a trailing .exe case-insensitively', () => {
    expect(exeDisplayName('Apex.exe')).toBe('Apex');
    expect(exeDisplayName('GAME.EXE')).toBe('GAME');
  });
  it('keeps names without an .exe suffix', () => {
    expect(exeDisplayName('launcher')).toBe('launcher');
  });
  it('takes the last segment of Windows and POSIX paths', () => {
    expect(baseName('C:\\x\\y\\name.mp4')).toBe('name.mp4');
    expect(baseName('/a/b/c.mp4')).toBe('c.mp4');
    expect(baseName('plain.mp4')).toBe('plain.mp4');
  });
});

describe('reduceEvent: state_changed', () => {
  it('sets the state and keeps the session while recording', () => {
    const prev = live({ session: { id: 's1', part: 1, partsSaved: 0, startedMs: 0 } });
    const next = reduceEvent(prev, { type: 'state_changed', data: { state: 'recording', game: null } }, LOCAL);
    expect(next.state).toBe('recording');
    expect(next.session).toEqual(prev.session);
  });
  it.each(['idle', 'buffer_only'] as const)('clears the session on %s', (state) => {
    const prev = live({ session: { id: 's1', part: 1, partsSaved: 0, startedMs: 0 } });
    const next = reduceEvent(prev, { type: 'state_changed', data: { state, game: null } }, LOCAL);
    expect(next.session).toBeNull();
  });
  it('attaches a new game with attach time on the server clock', () => {
    const prev = live({ serverOffsetMs: 2000 });
    const next = reduceEvent(prev, { type: 'state_changed', data: { state: 'armed', game: gameRef } }, LOCAL);
    expect(next.game).toEqual({ name: 'Apex', exe: 'Apex.exe', sinceMs: LOCAL + 2000, gameId: 'g1' });
  });
  it('keeps the attach time when the same game id is reported again', () => {
    const prev = live({ game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 500, gameId: 'g1' } });
    const next = reduceEvent(prev, { type: 'state_changed', data: { state: 'recording', game: gameRef } }, LOCAL);
    expect(next.game?.sinceMs).toBe(500);
  });
  it('clears the game when game is null', () => {
    const prev = live({ game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 500, gameId: 'g1' } });
    const next = reduceEvent(prev, { type: 'state_changed', data: { state: 'idle', game: null } }, LOCAL);
    expect(next.game).toBeNull();
  });
});

describe('reduceEvent: game events', () => {
  it('game_detected names the game from the exe and stamps attach time', () => {
    const prev = live({ serverOffsetMs: 1500 });
    const next = reduceEvent(prev, { type: 'game_detected', data: { game: gameRef } }, LOCAL);
    expect(next.game?.name).toBe('Apex');
    expect(next.game?.sinceMs).toBe(LOCAL + 1500);
  });
  it('game_detected for the same id keeps the earlier attach time', () => {
    const prev = live({ game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 500, gameId: 'g1' } });
    const next = reduceEvent(prev, { type: 'game_detected', data: { game: gameRef } }, LOCAL);
    expect(next.game?.sinceMs).toBe(500);
  });
  it('game_lost for the attached game clears it and the pending restart flag', () => {
    const prev = live({
      game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 500, gameId: 'g1' },
      settingsPendingRestart: true,
    });
    const next = reduceEvent(prev, { type: 'game_lost', data: { game_id: 'g1', reason: 'exited' } }, LOCAL);
    expect(next.game).toBeNull();
    expect(next.settingsPendingRestart).toBe(false);
  });
  it('game_lost for another game returns the same object', () => {
    const prev = live({ game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 500, gameId: 'g1' } });
    const next = reduceEvent(prev, { type: 'game_lost', data: { game_id: 'other', reason: 'exited' } }, LOCAL);
    expect(next).toBe(prev);
  });
});

describe('reduceEvent: session events', () => {
  const started = {
    type: 'session_started',
    data: {
      session: { session_id: 's1', part: 1, started_unix_ms: 5000, path: 'C:\\a\\p1.mp4', parts_saved: 0 },
    },
  } as Event;

  it('session_started maps the session', () => {
    const next = reduceEvent(live(), started, LOCAL);
    expect(next.session).toEqual({ id: 's1', part: 1, partsSaved: 0, startedMs: 5000 });
  });
  it('session_part_started updates the part for the current session', () => {
    const prev = reduceEvent(live(), started, LOCAL);
    const next = reduceEvent(
      prev,
      { type: 'session_part_started', data: { session_id: 's1', part: 2, path: 'p2' } },
      LOCAL,
    );
    expect(next.session?.part).toBe(2);
    expect(next.session?.startedMs).toBe(5000);
  });
  it('session_part_started for another session is ignored', () => {
    const prev = reduceEvent(live(), started, LOCAL);
    const next = reduceEvent(
      prev,
      { type: 'session_part_started', data: { session_id: 'other', part: 9, path: 'x' } },
      LOCAL,
    );
    expect(next).toBe(prev);
  });
  it('session_part_finished updates partsSaved and keeps startedMs', () => {
    const prev = reduceEvent(live(), started, LOCAL);
    const next = reduceEvent(
      prev,
      {
        type: 'session_part_finished',
        data: {
          session_id: 's1',
          part: 1,
          path: 'p1',
          duration_ms: 1000,
          bytes: 10,
          recovered: false,
          started_unix_ms: 5000,
          parts_saved: 1,
        },
      },
      LOCAL,
    );
    expect(next.session?.partsSaved).toBe(1);
    expect(next.session?.startedMs).toBe(5000);
  });
  it('session_part_finished for another session is ignored', () => {
    const prev = reduceEvent(live(), started, LOCAL);
    const next = reduceEvent(
      prev,
      {
        type: 'session_part_finished',
        data: {
          session_id: 'other',
          part: 1,
          path: 'p',
          duration_ms: 1,
          bytes: 1,
          recovered: false,
          started_unix_ms: 1,
          parts_saved: 5,
        },
      },
      LOCAL,
    );
    expect(next).toBe(prev);
  });
  it('the session clock keeps counting across part changes', () => {
    const T0 = 1_000_000;
    let s = reduceEvent(
      live(),
      {
        type: 'session_started',
        data: { session: { session_id: 's1', part: 1, started_unix_ms: T0, path: 'p1', parts_saved: 0 } },
      },
      T0,
    );
    s = reduceEvent(s, { type: 'session_part_started', data: { session_id: 's1', part: 2, path: 'p2' } }, T0);
    s = reduceEvent(
      s,
      {
        type: 'session_part_finished',
        data: {
          session_id: 's1',
          part: 1,
          path: 'p1',
          duration_ms: 1,
          bytes: 1,
          recovered: false,
          started_unix_ms: T0,
          parts_saved: 1,
        },
      },
      T0,
    );
    expect(s.session?.startedMs).toBe(T0);
    const elapsed = elapsedMs(s.session!.startedMs, T0 + 42 * 60_000, 0);
    expect(formatClock(elapsed)).toBe('42:00');
  });
});

describe('reduceEvent: other events', () => {
  it('clip_saved returns prev unchanged', () => {
    const prev = live();
    const next = reduceEvent(
      prev,
      { type: 'clip_saved', data: { clip_id: 'c', path: 'C:\\x\\a.mp4', duration_ms: 1, bytes: 1 } },
      LOCAL,
    );
    expect(next).toBe(prev);
  });
  it.each<Event>([
    { type: 'capture_health', data: { fps: 60, dropped_frames: 0, black_frames_active: false } },
    { type: 'audio_levels', data: { tracks: [] } },
    { type: 'mic_mute_changed', data: { muted: true } },
    { type: 'detection_pause_changed', data: { paused: true } },
    { type: 'worker_restarted', data: { attempt: 1, reason: 'crash', exit_code: 1 } },
    { type: 'shutting_down', data: { reason: 'quit' } },
    { type: 'settings_reloaded', data: { ok: true, errors: [] } },
  ])('ignores $type and returns the same object', (ev) => {
    const prev = live();
    expect(reduceEvent(prev, ev, LOCAL)).toBe(prev);
  });
  it('encoder_selected sets the encoder', () => {
    const next = reduceEvent(live(), { type: 'encoder_selected', data: { codec: 'h264', encoder: 'nvenc' } }, LOCAL);
    expect(next.encoder).toBe('nvenc');
  });
  it('config_changed toggles settingsPendingRestart from restart_required', () => {
    const on = reduceEvent(live(), { type: 'config_changed', data: { apply_class: 'restart', restart_required: true } }, LOCAL);
    expect(on.settingsPendingRestart).toBe(true);
    const off = reduceEvent(on, { type: 'config_changed', data: { apply_class: 'live', restart_required: false } }, LOCAL);
    expect(off.settingsPendingRestart).toBe(false);
  });
  it('degraded sets state and reason', () => {
    const next = reduceEvent(live({ state: 'recording' }), { type: 'degraded', data: { reason: 'gpu lost' } }, LOCAL);
    expect(next.state).toBe('degraded');
    expect(next.degradedReason).toBe('gpu lost');
  });
  it('warning stores code and message', () => {
    const next = reduceEvent(live(), { type: 'warning', data: { code: 'disk_low', message: 'Low disk' } }, LOCAL);
    expect(next.warning).toEqual({ code: 'disk_low', message: 'Low disk' });
  });
});

describe('applySnapshot', () => {
  it('estimates the offset as server time minus local time', () => {
    const next = applySnapshot(live(), snap({ server_unix_ms: 6000 }), 1000);
    expect(next.serverOffsetMs).toBe(5000);
  });
  it('keeps the previous offset when server_unix_ms is non-positive', () => {
    const next = applySnapshot(live({ serverOffsetMs: 777 }), snap({ server_unix_ms: 0 }), 1000);
    expect(next.serverOffsetMs).toBe(777);
  });
  it('keeps the previous offset when server_unix_ms is not finite', () => {
    const next = applySnapshot(live({ serverOffsetMs: 777 }), snap({ server_unix_ms: Number.NaN }), 1000);
    expect(next.serverOffsetMs).toBe(777);
  });
  it('marks the store connected and copies state', () => {
    const next = applySnapshot(live({ connected: false }), snap({ state: 'recording' }), LOCAL);
    expect(next.connected).toBe(true);
    expect(next.state).toBe('recording');
  });
  it('builds the game from game_info with its attach time', () => {
    const next = applySnapshot(
      live(),
      snap({
        game: gameRef,
        game_info: { name: 'Apex Legends', exe: 'Apex.exe', hwnd: 1, attached_since_unix_ms: 9000 },
      }),
      LOCAL,
    );
    expect(next.game).toEqual({ name: 'Apex Legends', exe: 'Apex.exe', sinceMs: 9000, gameId: 'g1' });
  });
  it('falls back to the exe name when game_info has no name', () => {
    const next = applySnapshot(
      live(),
      snap({ game_info: { name: '', exe: 'Apex.exe', hwnd: 1, attached_since_unix_ms: 9000 } }),
      LOCAL,
    );
    expect(next.game?.name).toBe('Apex');
  });
  it('uses the game ref with the server clock when game_info is absent', () => {
    const next = applySnapshot(
      live({ serverOffsetMs: 0 }),
      snap({ game: gameRef, server_unix_ms: LOCAL + 300 }),
      LOCAL,
    );
    expect(next.game).toEqual({ name: 'Apex', exe: 'Apex.exe', sinceMs: LOCAL + 300, gameId: 'g1' });
  });
  it('maps the session fields', () => {
    const next = applySnapshot(
      live(),
      snap({
        session: { session_id: 's9', part: 3, started_unix_ms: 4242, path: 'p', parts_saved: 2 },
      }),
      LOCAL,
    );
    expect(next.session).toEqual({ id: 's9', part: 3, partsSaved: 2, startedMs: 4242 });
  });
  it('clears the session when the snapshot has none', () => {
    const prev = live({ session: { id: 's1', part: 1, partsSaved: 0, startedMs: 0 } });
    expect(applySnapshot(prev, snap(), LOCAL).session).toBeNull();
  });
  it('reads buffer_secs when present', () => {
    const next = applySnapshot(live(), snap({ buffer_secs: 12.5, buffer_seconds: 99 }), LOCAL);
    expect(next.bufferSecs).toBe(12.5);
  });
  it('issue_184 returns prev itself for an identical snapshot', () => {
    const s = snap({ state: 'recording', server_unix_ms: LOCAL });
    const first = applySnapshot(live({ connected: false }), s, LOCAL);
    expect(applySnapshot(first, s, LOCAL)).toBe(first);
  });
  it('issue_184 returns a new object when a field changed', () => {
    const s = snap({ server_unix_ms: LOCAL });
    const first = applySnapshot(live({ connected: false }), s, LOCAL);
    const next = applySnapshot(first, snap({ state: 'recording', server_unix_ms: LOCAL }), LOCAL);
    expect(next).not.toBe(first);
    expect(next.state).toBe('recording');
  });
  it('maps the pending restart flag and degraded reason', () => {
    const next = applySnapshot(
      live(),
      snap({ settings_pending_restart: true, degraded_reason: 'encoder fell back' }),
      LOCAL,
    );
    expect(next.settingsPendingRestart).toBe(true);
    expect(next.degradedReason).toBe('encoder fell back');
  });
});

describe('reduceConnection', () => {
  const full = live({
    state: 'recording',
    game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 0, gameId: 'g1' },
    session: { id: 's1', part: 1, partsSaved: 0, startedMs: 0 },
    bufferSecs: 40,
    encoder: 'nvenc',
    settingsPendingRestart: true,
    degradedReason: 'gpu',
  });

  it('going offline clears all live state', () => {
    const next = reduceConnection(full, false);
    expect(next).toMatchObject({
      connected: false,
      state: null,
      game: null,
      session: null,
      bufferSecs: 0,
      encoder: null,
      settingsPendingRestart: false,
      degradedReason: null,
    });
  });
  it('coming online only sets connected', () => {
    const next = reduceConnection({ ...full, connected: false }, true);
    expect(next.connected).toBe(true);
    expect(next.state).toBe('recording');
    expect(next.session).toEqual(full.session);
  });
});

describe('parseLiveConfig', () => {
  it('reads replay.max_seconds and hotkeys.clips', () => {
    const parsed = parseLiveConfig({
      replay: { max_seconds: 600 },
      hotkeys: { clips: [{ name: 'Quick', seconds: 30 }, { name: 'Long', seconds: 300 }] },
    });
    expect(parsed).toEqual({
      maxBufferSecs: 600,
      clipPresets: [
        { name: 'Quick', seconds: 30 },
        { name: 'Long', seconds: 300 },
      ],
    });
  });
  it('ignores clip entries without a numeric seconds value', () => {
    const parsed = parseLiveConfig({
      hotkeys: { clips: [{ name: 'Bad', seconds: '30' }, { name: 'NoSecs' }, { name: 'Ok', seconds: 15 }] },
    });
    expect(parsed.clipPresets).toEqual([{ name: 'Ok', seconds: 15 }]);
  });
  it('names a blank clip preset by its length', () => {
    const parsed = parseLiveConfig({ hotkeys: { clips: [{ name: '', seconds: 45 }] } });
    expect(parsed.clipPresets).toEqual([{ name: '45s', seconds: 45 }]);
  });
  it('returns null and no presets when sections are missing', () => {
    expect(parseLiveConfig({})).toEqual({ maxBufferSecs: null, clipPresets: [] });
  });
});

describe('describeChip', () => {
  it('shows offline when not connected', () => {
    const chip = describeChip(live({ connected: false }), LOCAL);
    expect(chip).toMatchObject({ tone: 'offline', label: 'Recorder offline', clock: null, game: null });
  });

  it('shows recording with the whole-session clock and game name', () => {
    const s = live({
      game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 90_000, gameId: 'g1' },
      session: { id: 's1', part: 2, partsSaved: 1, startedMs: 0 },
    });
    const chip = describeChip(s, 125_000);
    expect(chip.tone).toBe('recording');
    expect(chip.game).toBe('Apex');
    expect(chip.clock).toBe('02:05');
  });

  it('recording tooltip reports the part and saved count', () => {
    const s = live({ session: { id: 's1', part: 2, partsSaved: 1, startedMs: 0 } });
    expect(describeChip(s, 1000).tooltip).toContain('Part 2 · 1 saved');
  });

  it('buffering shows the time since the game was attached', () => {
    const s = live({
      game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 0, gameId: 'g1' },
      bufferSecs: 125,
    });
    const chip = describeChip(s, 61_000);
    expect(chip).toMatchObject({ tone: 'buffering', game: 'Apex', label: 'Buffering', clock: '01:01' });
    expect(chip.tooltip).toContain('Buffer 02:05');
  });

  it('idle shows the waiting label with an em dash and no game', () => {
    const chip = describeChip(live(), LOCAL);
    expect(chip).toMatchObject({
      tone: 'idle',
      game: null,
      label: 'Idle \u2014 waiting for a game',
      clock: null,
    });
  });
});

describe('stores and applyEvent', () => {
  it('handleConnection(false) after a connected snapshot leaves the store offline', () => {
    liveStatus.set(live({ state: 'recording', encoder: 'nvenc' }));
    liveConfig.set({ maxBufferSecs: 600, clipPresets: [{ name: 'Q', seconds: 30 }] });
    handleConnection(false);
    const s = get(liveStatus);
    expect(s.connected).toBe(false);
    expect(s.state).toBeNull();
    expect(s.encoder).toBeNull();
    expect(get(liveConfig)).toEqual(INITIAL_CONFIG);
  });

  it('handleConnection(true) marks the store connected without throwing outside Tauri', () => {
    expect(() => handleConnection(true)).not.toThrow();
    expect(get(liveStatus).connected).toBe(true);
  });

  it('applyEvent folds an event into liveStatus', () => {
    liveStatus.set(live());
    applyEvent({ type: 'encoder_selected', data: { codec: 'h264', encoder: 'nvenc' } });
    expect(get(liveStatus).encoder).toBe('nvenc');
  });

  it('pushToast removes the toast after TOAST_MS', () => {
    vi.useFakeTimers();
    pushToast('hello');
    expect(get(toasts)).toHaveLength(1);
    vi.advanceTimersByTime(TOAST_MS - 1);
    expect(get(toasts)).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(get(toasts)).toHaveLength(0);
  });

  it('issue_67 reduceEvent returns prev for each event with no live-panel state', () => {
    const prev = live({ state: 'recording', encoder: 'nvenc' });
    const noOp: Event[] = [
      { type: 'clip_saved', data: { clip_id: 'c', path: 'C:\\x\\a.mp4', duration_ms: 1, bytes: 1 } },
      { type: 'settings_reloaded', data: { ok: true, errors: [] } },
      { type: 'audio_levels', data: { tracks: [] } },
      { type: 'capture_health', data: { fps: 60, dropped_frames: 0, black_frames_active: false } },
      { type: 'mic_mute_changed', data: { muted: true } },
      { type: 'detection_pause_changed', data: { paused: true } },
      { type: 'worker_restarted', data: { attempt: 1, reason: 'crash', exit_code: 1 } },
      { type: 'shutting_down', data: { reason: 'quit' } },
    ];
    for (const ev of noOp) {
      expect(reduceEvent(prev, ev, LOCAL), ev.type).toBe(prev);
    }
  });

  it('applyEvent for clip_saved shows the saved file name', () => {
    vi.useFakeTimers();
    applyEvent({
      type: 'clip_saved',
      data: { clip_id: 'c1', path: 'C:\\x\\y\\name.mp4', duration_ms: 1000, bytes: 10 },
    });
    expect(get(toasts).map((t) => t.text)).toEqual(['Saved clip: name.mp4']);
  });
});

describe('recorder_enabled (recorder on/off)', () => {
  it('maps recorder_enabled false from the snapshot', () => {
    const next = applySnapshot(live(), snap({ recorder_enabled: false }), LOCAL);
    expect(next.recorderEnabled).toBe(false);
  });

  it('a missing recorder_enabled field means on', () => {
    const next = applySnapshot(live({ recorderEnabled: false }), snap(), LOCAL);
    expect(next.recorderEnabled).toBe(true);
  });

  it('starts on before any snapshot', () => {
    expect(INITIAL_STATUS.recorderEnabled).toBe(true);
  });

  it('a change of recorder state is a real status change for subscribers', () => {
    const on = applySnapshot(live(), snap({ recorder_enabled: true }), LOCAL);
    const off = applySnapshot(on, snap({ recorder_enabled: false }), LOCAL);
    expect(off).not.toBe(on);
    expect(off.recorderEnabled).toBe(false);
  });

  it('an unchanged snapshot returns the same object', () => {
    const prev = applySnapshot(live(), snap({ recorder_enabled: false }), LOCAL);
    expect(applySnapshot(prev, snap({ recorder_enabled: false }), LOCAL)).toBe(prev);
  });

  it('describeChip shows recorder_off with the recorder off', () => {
    expect(describeChip(live({ recorderEnabled: false }), LOCAL)).toMatchObject({
      tone: 'recorder_off',
      label: 'Recorder off',
    });
  });

  it('describeChip keeps the offline chip when the daemon is unreachable', () => {
    expect(describeChip(live({ connected: false, recorderEnabled: false }), LOCAL).tone).toBe('offline');
  });

  it('describeChip keeps showing the recording state while a session still runs', () => {
    const s = live({
      recorderEnabled: false,
      session: { id: 's1', part: 1, partsSaved: 0, startedMs: 0 },
    });
    expect(describeChip(s, LOCAL).tone).toBe('recording');
  });
});
