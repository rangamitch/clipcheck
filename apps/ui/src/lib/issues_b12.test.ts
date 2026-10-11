import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Regression tests for GitHub issues #50, #52 and #56 (UI lib, non-settings).
// Only public exports are used. No Tauri IPC is reached: core/event/window are mocked.

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => ({})),
}));

import { isTauri, daemonConnected } from './daemon';
import { formatClock as formatClockFromFormat, formatDuration } from './format';
import { formatClock as formatClockFromStatus, describeChip, INITIAL_STATUS, type LiveStatus } from './status';
import { isTauriRuntime } from './window';

const g = globalThis as unknown as Record<string, unknown>;

function setInternals(where: 'global' | 'window' | 'none'): void {
  delete g.__TAURI_INTERNALS__;
  delete g.window;
  if (where === 'global') g.__TAURI_INTERNALS__ = {};
  if (where === 'window') g.window = { __TAURI_INTERNALS__: {} };
}

beforeEach(() => {
  invokeMock.mockReset();
  setInternals('none');
});

afterEach(() => {
  setInternals('none');
});

// ---------------------------------------------------------------------------
// #50: recording chip tooltip must not end the session line with a stray "in"
// ---------------------------------------------------------------------------

function liveRecording(over: Partial<LiveStatus> = {}): LiveStatus {
  return {
    ...INITIAL_STATUS,
    connected: true,
    session: { id: 's1', part: 1, partsSaved: 0, startedMs: 0 },
    ...over,
  };
}

describe('issue_50_recording_tooltip_session_line', () => {
  it('issue_50_session_line_reads_elapsed_without_trailing_in', () => {
    const chip = describeChip(liveRecording({ serverOffsetMs: 0 }), 5_000);
    const sessionLine = chip.tooltip.split('\n').find((l) => l.startsWith('Session '));
    expect(sessionLine).toBe('Session 00:05 elapsed');
    expect(chip.tooltip).not.toMatch(/ in$/m);
    expect(chip.tooltip).not.toContain(' in\n');
  });

  it('issue_50_session_line_keeps_clock_and_part_lines', () => {
    const chip = describeChip(
      liveRecording({
        game: { name: 'Apex', exe: 'Apex.exe', sinceMs: 0, gameId: 'g1' },
        session: { id: 's1', part: 2, partsSaved: 1, startedMs: 0 },
        serverOffsetMs: 0,
      }),
      125_000,
    );
    expect(chip.tooltip.split('\n')).toEqual([
      'Recording Apex',
      'Session 02:05 elapsed',
      'Part 2 · 1 saved',
    ]);
    expect(chip.clock).toBe('02:05');
  });
});

// ---------------------------------------------------------------------------
// #52: one clock formatter in format.ts, re-exported from status.ts
// ---------------------------------------------------------------------------

describe('issue_52_clock_formatter_location', () => {
  it('issue_52_status_formatClock_is_the_format_module_function', () => {
    expect(formatClockFromStatus).toBe(formatClockFromFormat);
  });

  it.each([
    [0, '00:00'],
    [5_000, '00:05'],
    [93_000, '01:33'],
    [3_723_000, '1:02:03'],
    [86_400_000, '1d 0:00:00'],
    [2 * 86_400_000 + 3 * 3_600_000 + 4 * 60_000 + 5_000, '2d 3:04:05'],
  ])('issue_52_formatClock_from_format_ms_%d_is_%s', (ms, expected) => {
    expect(formatClockFromFormat(ms)).toBe(expected);
  });

  it('issue_52_formatClock_treats_negative_and_nan_as_zero', () => {
    expect(formatClockFromFormat(-1)).toBe('00:00');
    expect(formatClockFromFormat(Number.NaN)).toBe('00:00');
    expect(formatClockFromFormat(Number.POSITIVE_INFINITY)).toBe('00:00');
  });

  it('issue_52_formatDuration_keeps_media_length_format_m_ss', () => {
    // formatDuration (media length) and formatClock (live clock) differ only by
    // the zero-padded minute field from one hour up.
    expect(formatDuration(3_723_000)).toBe('1:02:03');
    expect(formatClockFromFormat(3_723_000)).toBe('1:02:03');
    expect(formatDuration(93_000)).toBe('1:33');
    expect(formatClockFromFormat(93_000)).toBe('01:33');
  });
});

// ---------------------------------------------------------------------------
// #56: one Tauri-detection helper (isTauriRuntime); isTauri is an alias of it
// ---------------------------------------------------------------------------

describe('issue_56_tauri_detection', () => {
  it('issue_56_isTauri_is_the_same_function_as_isTauriRuntime', () => {
    expect(isTauri).toBe(isTauriRuntime);
  });

  it('issue_56_both_report_false_with_no_internals', () => {
    setInternals('none');
    expect(isTauriRuntime()).toBe(false);
    expect(isTauri()).toBe(false);
  });

  it('issue_56_both_report_true_when_internals_are_on_globalThis', () => {
    setInternals('global');
    expect(isTauriRuntime()).toBe(true);
    expect(isTauri()).toBe(true);
  });

  it('issue_56_both_report_true_when_internals_are_only_on_window', () => {
    // Before the fix daemon.ts read `window` while window.ts read `globalThis`,
    // so a stub on one side made the two checks disagree.
    setInternals('window');
    expect(isTauriRuntime()).toBe(true);
    expect(isTauri()).toBe(true);
  });

  it('issue_56_daemon_bridge_does_not_invoke_when_detection_is_false', async () => {
    setInternals('none');
    await expect(daemonConnected()).resolves.toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('issue_56_daemon_bridge_invokes_when_window_only_stub_is_present', async () => {
    setInternals('window');
    invokeMock.mockResolvedValueOnce(true);
    await expect(daemonConnected()).resolves.toBe(true);
    expect(invokeMock).toHaveBeenCalledWith('daemon_connected');
  });
});
