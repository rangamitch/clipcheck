import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import { INITIAL_STATUS, describeChip, type LiveStatus } from '../status';
import { setRecorderEnabled } from '../daemon';
import { gateReason, RECORDER_OFF_REASON, type GateAction } from './gates';
import {
  OFF_CONFIRM_TEXT,
  needsOffConfirm,
  nextRecorderEnabled,
  switchDisabledReason,
  switchLabel,
} from './recorderToggle';

const LOCAL = 1_000_000;
const g = globalThis as unknown as Record<string, unknown>;

/** A connected status with the recorder on, nothing attached and no session. */
function live(over: Partial<LiveStatus> = {}): LiveStatus {
  return { ...INITIAL_STATUS, connected: true, recorderEnabled: true, ...over };
}

const RECORDING = { id: 's1', part: 1, partsSaved: 0, startedMs: LOCAL - 60_000 };
const GAME = { name: 'Apex', exe: 'Apex.exe', sinceMs: LOCAL - 5_000, gameId: 'g1' };

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue({ type: 'ok' });
  g.__TAURI_INTERNALS__ = {};
});

afterEach(() => {
  delete g.__TAURI_INTERNALS__;
});

describe('switch labels and state', () => {
  it('names the action a press performs', () => {
    expect(switchLabel(true)).toBe('Turn recorder off');
    expect(switchLabel(false)).toBe('Turn recorder on');
  });

  it('is disabled while offline, with a reason', () => {
    expect(switchDisabledReason(live({ connected: false }), false)).toMatch(/not running/);
  });

  it('is disabled while another action is in flight', () => {
    expect(switchDisabledReason(live(), true)).toBe('Another action is still running.');
  });

  it('is enabled when connected and idle', () => {
    expect(switchDisabledReason(live(), false)).toBeNull();
  });

  it('a press sends the opposite of the current state', () => {
    expect(nextRecorderEnabled(live({ recorderEnabled: true }))).toBe(false);
    expect(nextRecorderEnabled(live({ recorderEnabled: false }))).toBe(true);
  });
});

describe('turn-off confirm', () => {
  it('asks when the recorder is on and a session is recording', () => {
    expect(needsOffConfirm(live({ session: RECORDING }))).toBe(true);
  });

  it('does not ask when nothing is recording', () => {
    expect(needsOffConfirm(live({ game: GAME }))).toBe(false);
    expect(needsOffConfirm(live())).toBe(false);
  });

  it('does not ask when the recorder is already off', () => {
    expect(needsOffConfirm(live({ recorderEnabled: false, session: RECORDING }))).toBe(false);
  });

  it('does not ask while offline', () => {
    expect(needsOffConfirm(live({ connected: false, session: RECORDING }))).toBe(false);
  });

  it('uses the agreed confirm wording', () => {
    expect(OFF_CONFIRM_TEXT).toBe('Turn off the recorder? The current session will be saved up to now.');
  });
});

describe('set_recorder_enabled request', () => {
  it('sends the exact command shape for off', async () => {
    await setRecorderEnabled(false);
    expect(invokeMock).toHaveBeenCalledWith('daemon_request', {
      command: { type: 'set_recorder_enabled', data: { enabled: false } },
    });
  });

  it('sends the exact command shape for on', async () => {
    await setRecorderEnabled(true);
    expect(invokeMock).toHaveBeenCalledWith('daemon_request', {
      command: { type: 'set_recorder_enabled', data: { enabled: true } },
    });
  });

  it('resolves with the daemon reply, including an error reply', async () => {
    invokeMock.mockResolvedValueOnce({ type: 'error', data: { code: 'busy', message: 'nope', retryable: true } });
    const reply = await setRecorderEnabled(false);
    expect(reply).toEqual({ type: 'error', data: { code: 'busy', message: 'nope', retryable: true } });
  });
});

describe('chip gating while the recorder is off', () => {
  const actions: GateAction[] = ['clip', 'start', 'stop', 'split'];

  it.each(actions)('%s is disabled with the recorder-off tooltip', (action) => {
    const s = live({ recorderEnabled: false, game: GAME, session: action === 'stop' || action === 'split' ? RECORDING : null });
    expect(gateReason(s, action)).toBe('Recorder is off');
    expect(RECORDER_OFF_REASON).toBe('Recorder is off');
  });

  it('offline still reports the offline reason first', () => {
    expect(gateReason(live({ connected: false, recorderEnabled: false }), 'clip')).toMatch(/not running/);
  });

  it('with the recorder on, gating falls back to the attached rules', () => {
    expect(gateReason(live(), 'clip')).toBe('No game window is attached. Start a supported game first.');
    expect(gateReason(live({ game: GAME }), 'clip')).toBeNull();
    expect(gateReason(live({ game: GAME }), 'start')).toBeNull();
    expect(gateReason(live(), 'stop')).toBe('No session is recording.');
  });
});

describe('chip view while the recorder is off', () => {
  it('shows Recorder off, dimmed, ahead of the waiting-for-game state', () => {
    const chip = describeChip(live({ recorderEnabled: false }), LOCAL);
    expect(chip).toMatchObject({ tone: 'recorder_off', label: 'Recorder off', game: null, clock: null });
    expect(chip.tone).not.toBe('idle');
  });

  it('wins over an attached game that is not recording', () => {
    const chip = describeChip(live({ recorderEnabled: false, game: GAME }), LOCAL);
    expect(chip.tone).toBe('recorder_off');
  });

  it('the recorder on still shows the idle state when nothing is attached', () => {
    expect(describeChip(live(), LOCAL).tone).toBe('idle');
  });
});
