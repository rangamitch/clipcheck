import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

const mocks = vi.hoisted(() => ({
  request: vi.fn(),
  onEvent: vi.fn(),
  handlers: new Set<(e: any) => void>(),
}));

vi.mock('./daemon', () => ({
  request: (...a: unknown[]) => mocks.request(...a),
  onEvent: (...a: unknown[]) => mocks.onEvent(...a),
}));

type D = typeof import('./discovery');
let d: D;
let doc: EventTarget & { hidden: boolean };
let win: EventTarget;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function defaultReply(cmd: { type: string }): unknown {
  switch (cmd.type) {
    case 'list_audio_apps':
      return { type: 'audio_apps', data: { apps: [] } };
    case 'list_audio_devices':
      return { type: 'audio_devices', data: { capture: [], render: [] } };
    case 'list_encoders':
      return { type: 'encoders', data: { encoder_ready: true, gpus: [], encoders: [] } };
    case 'list_running_apps':
      return { type: 'running_apps', data: { apps: [] } };
    default:
      return { type: 'ok' };
  }
}

/** All commands sent through `request`, in order. */
function sent(): Array<Record<string, any>> {
  return mocks.request.mock.calls.map((c) => c[0] as Record<string, any>);
}

function countType(type: string): number {
  return sent().filter((c) => c.type === type).length;
}

function countEnable(enable: boolean): number {
  return sent().filter((c) => c.type === 'mic_meter' && c.data?.enable === enable).length;
}

function fireMeter(event: unknown): void {
  for (const h of [...mocks.handlers]) h(event);
}

function encReply(ready: boolean): unknown {
  return { type: 'encoders', data: {
    encoder_ready: ready,
    gpus: [{ name: 'RTX', vendor: 'nvidia', index: 0, dedicated_mb: 8192 }],
    encoders: [{ backend: 'nvenc', codec: 'h264', encoder_name: 'h264_nvenc', works: true }],
  } };
}

beforeEach(async () => {
  vi.useFakeTimers();

  mocks.request.mockReset();
  mocks.request.mockImplementation(async (cmd: { type: string }) => defaultReply(cmd));
  mocks.onEvent.mockReset();
  mocks.handlers.clear();
  mocks.onEvent.mockImplementation((h: (e: any) => void) => {
    mocks.handlers.add(h);
    return () => {
      mocks.handlers.delete(h);
    };
  });

  doc = Object.assign(new EventTarget(), { hidden: false });
  win = new EventTarget();
  vi.stubGlobal('document', doc);
  vi.stubGlobal('window', win);
  vi.stubGlobal('navigator', { onLine: true });

  vi.resetModules();
  d = await import('./discovery');
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------
// 1. Polling
// ---------------------------------------------------------------------------

describe('startAudioAppsPolling: timing and refcount', () => {
  it('requests immediately, then once per 2 s', async () => {
    const stop = d.startAudioAppsPolling();
    expect(countType('list_audio_apps')).toBe(1);

    await vi.advanceTimersByTimeAsync(2000);
    expect(countType('list_audio_apps')).toBe(2);

    await vi.advanceTimersByTimeAsync(2000);
    expect(countType('list_audio_apps')).toBe(3);
    stop();
  });

  it('two consumers share one timer', async () => {
    const s1 = d.startAudioAppsPolling();
    const s2 = d.startAudioAppsPolling();
    expect(countType('list_audio_apps')).toBe(1);

    await vi.advanceTimersByTimeAsync(2000);
    expect(countType('list_audio_apps')).toBe(2);
    s1();
    s2();
  });

  it('stopping one consumer keeps polling', async () => {
    const s1 = d.startAudioAppsPolling();
    const s2 = d.startAudioAppsPolling();
    s1();
    const before = countType('list_audio_apps');

    await vi.advanceTimersByTimeAsync(4000);
    expect(countType('list_audio_apps')).toBe(before + 2);
    s2();
  });

  it('stopping the last consumer stops polling', async () => {
    const s1 = d.startAudioAppsPolling();
    const s2 = d.startAudioAppsPolling();
    s1();
    s2();
    const before = countType('list_audio_apps');

    await vi.advanceTimersByTimeAsync(10_000);
    expect(countType('list_audio_apps')).toBe(before);
  });

  it('calling the same stop function twice does not drop another consumer', async () => {
    const s1 = d.startAudioAppsPolling();
    const s2 = d.startAudioAppsPolling();
    s1();
    s1();
    const before = countType('list_audio_apps');

    await vi.advanceTimersByTimeAsync(2000);
    expect(countType('list_audio_apps')).toBe(before + 1);
    s2();
  });
});

// ---------------------------------------------------------------------------
// 2. Pause and resume
// ---------------------------------------------------------------------------

describe('startAudioAppsPolling: pause and resume', () => {
  it('pauses while the document is hidden and resumes with an immediate refresh', async () => {
    const stop = d.startAudioAppsPolling();
    expect(countType('list_audio_apps')).toBe(1);

    doc.hidden = true;
    doc.dispatchEvent(new Event('visibilitychange'));
    await vi.advanceTimersByTimeAsync(10_000);
    expect(countType('list_audio_apps')).toBe(1);

    doc.hidden = false;
    doc.dispatchEvent(new Event('visibilitychange'));
    expect(countType('list_audio_apps')).toBe(2);

    await vi.advanceTimersByTimeAsync(2000);
    expect(countType('list_audio_apps')).toBe(3);
    stop();
  });

  it('does not request at all when started while hidden', async () => {
    doc.hidden = true;
    const stop = d.startAudioAppsPolling();
    await vi.advanceTimersByTimeAsync(6000);
    expect(countType('list_audio_apps')).toBe(0);
    stop();
  });

  it('pauses while offline and resumes when back online', async () => {
    const stop = d.startAudioAppsPolling();
    expect(countType('list_audio_apps')).toBe(1);

    vi.stubGlobal('navigator', { onLine: false });
    win.dispatchEvent(new Event('offline'));
    await vi.advanceTimersByTimeAsync(10_000);
    expect(countType('list_audio_apps')).toBe(1);

    vi.stubGlobal('navigator', { onLine: true });
    win.dispatchEvent(new Event('online'));
    expect(countType('list_audio_apps')).toBe(2);

    await vi.advanceTimersByTimeAsync(2000);
    expect(countType('list_audio_apps')).toBe(3);
    stop();
  });
});

// ---------------------------------------------------------------------------
// 3. Errors
// ---------------------------------------------------------------------------

describe('error handling keeps previous store values', () => {
  it('a rejected request keeps the previous audio app list and does not reject', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'audio_apps', data: {
      apps: [{ exe: 'a.exe', name: 'A', active: false, peak: 0, last_active_unix_ms: 1 }],
    } });
    await d.refreshAudioApps();
    const prev = get(d.audioApps);
    expect(prev).toHaveLength(1);

    mocks.request.mockRejectedValueOnce(new Error('pipe closed'));
    await expect(d.refreshAudioApps()).resolves.toBeUndefined();
    expect(get(d.audioApps)).toEqual(prev);
  });

  it('an error reply keeps the previous audio app list', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'audio_apps', data: {
      apps: [{ exe: 'a.exe', name: 'A', active: false, peak: 0, last_active_unix_ms: 1 }],
    } });
    await d.refreshAudioApps();
    const prev = get(d.audioApps);

    mocks.request.mockResolvedValueOnce({
      type: 'error',
      data: { code: 'not_implemented', message: 'x', retryable: false },
    });
    await expect(d.refreshAudioApps()).resolves.toBeUndefined();
    expect(get(d.audioApps)).toEqual(prev);
  });

  it('a wrong reply type keeps the previous value', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'ok' });
    await d.loadAudioDevices();
    expect(get(d.audioDevices)).toEqual({ capture: [], render: [] });
  });

  it('polling with failing requests does not throw', async () => {
    mocks.request.mockRejectedValue(new Error('down'));
    const stop = d.startAudioAppsPolling();
    await expect(vi.advanceTimersByTimeAsync(10_000)).resolves.not.toThrow();
    stop();
  });

  it('a rejected device load keeps the previous devices', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'audio_devices', data: {
      capture: [{ id: 'c1', name: 'Mic', is_default: true }],
      render: [],
    } });
    await d.loadAudioDevices();
    const prev = get(d.audioDevices);

    mocks.request.mockRejectedValueOnce(new Error('nope'));
    await expect(d.loadAudioDevices()).resolves.toBeUndefined();
    expect(get(d.audioDevices)).toEqual(prev);
  });

  it('a rejected running-app refresh keeps the previous list', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'running_apps', data: {
      apps: [
        { exe: 'g.exe', name: 'G', icon_png_b64: null, pid: 1, is_game: true, in_allow_list: false, suggested: true },
      ],
    } });
    await d.refreshRunningApps();
    const prev = get(d.runningApps);
    expect(prev).toHaveLength(1);

    mocks.request.mockRejectedValueOnce(new Error('nope'));
    await expect(d.refreshRunningApps()).resolves.toBeUndefined();
    expect(get(d.runningApps)).toEqual(prev);
  });
});

// ---------------------------------------------------------------------------
// 4. Mapping
// ---------------------------------------------------------------------------

describe('wire to UI mapping', () => {
  it('maps audio apps (icon null when absent) and sorts active first', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'audio_apps', data: {
      apps: [
        { exe: 'a.exe', name: 'Alpha', icon_png_b64: 'QUJD', active: false, peak: 0.1, last_active_unix_ms: 10 },
        { exe: 'b.exe', name: 'Beta', active: true, peak: 0.5, last_active_unix_ms: 5 },
        { exe: 'c.exe', name: 'Gamma', icon_png_b64: null, active: false, peak: 0, last_active_unix_ms: 1 },
      ],
    } });
    await d.refreshAudioApps();
    expect(get(d.audioApps)).toEqual([
      { exe: 'b.exe', name: 'Beta', iconB64: null, active: true, peak: 0.5, lastActiveUnixMs: 5 },
      { exe: 'a.exe', name: 'Alpha', iconB64: 'QUJD', active: false, peak: 0.1, lastActiveUnixMs: 10 },
      { exe: 'c.exe', name: 'Gamma', iconB64: null, active: false, peak: 0, lastActiveUnixMs: 1 },
    ]);
  });

  it('maps audio devices (is_default to isDefault)', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'audio_devices', data: {
      capture: [{ id: 'c1', name: 'Mic', is_default: true }],
      render: [
        { id: 'r1', name: 'Speakers', is_default: false },
        { id: 'r2', name: 'Headset', is_default: true },
      ],
    } });
    await d.loadAudioDevices();
    expect(get(d.audioDevices)).toEqual({
      capture: [{ id: 'c1', name: 'Mic', isDefault: true }],
      render: [
        { id: 'r1', name: 'Speakers', isDefault: false },
        { id: 'r2', name: 'Headset', isDefault: true },
      ],
    });
  });

  it('maps encoder state (gpus and encoders)', async () => {
    mocks.request.mockResolvedValueOnce(encReply(true));
    await d.loadEncoders();
    expect(get(d.encoders)).toEqual({
      encoderReady: true,
      gpus: [{ name: 'RTX', vendor: 'nvidia', index: 0, dedicatedMb: 8192 }],
      encoders: [{ backend: 'nvenc', codec: 'h264', encoderName: 'h264_nvenc', works: true }],
    });
  });

  it('maps running apps (is_game and in_allow_list)', async () => {
    mocks.request.mockResolvedValueOnce({ type: 'running_apps', data: {
      apps: [
        { exe: 'g.exe', name: 'Game', icon_png_b64: 'ICON', pid: 42, is_game: true, in_allow_list: true, suggested: false },
        { exe: 'n.exe', name: 'Notes', icon_png_b64: null, pid: 7, is_game: false, in_allow_list: false, suggested: true },
      ],
    } });
    await d.refreshRunningApps();
    const apps = get(d.runningApps);
    const game = apps.find((a) => a.exe === 'g.exe');
    const notes = apps.find((a) => a.exe === 'n.exe');
    expect(game).toEqual({
      exe: 'g.exe',
      name: 'Game',
      iconB64: 'ICON',
      pid: 42,
      isGame: true,
      inAllowList: true,
      suggested: false,
    });
    expect(notes).toEqual({
      exe: 'n.exe',
      name: 'Notes',
      iconB64: null,
      pid: 7,
      isGame: false,
      inAllowList: false,
      suggested: true,
    });
  });
});

// ---------------------------------------------------------------------------
// 5. Encoder retry
// ---------------------------------------------------------------------------

describe('loadEncoders retry', () => {
  it('resolves after the first attempt and stores a not-ready state', async () => {
    mocks.request.mockImplementation(async () => encReply(false));
    await d.loadEncoders();
    expect(countType('list_encoders')).toBe(1);
    expect(get(d.encoders).encoderReady).toBe(false);
    expect(get(d.encoders).gpus).toHaveLength(1);
  });

  it('retries at 1500 ms and stops once ready', async () => {
    mocks.request.mockResolvedValueOnce(encReply(false)).mockResolvedValueOnce(encReply(true));
    await d.loadEncoders();
    expect(countType('list_encoders')).toBe(1);

    await vi.advanceTimersByTimeAsync(1499);
    expect(countType('list_encoders')).toBe(1);

    await vi.advanceTimersByTimeAsync(1);
    expect(countType('list_encoders')).toBe(2);
    expect(get(d.encoders).encoderReady).toBe(true);

    await vi.advanceTimersByTimeAsync(60_000);
    expect(countType('list_encoders')).toBe(2);
  });

  it('retries a failed request', async () => {
    mocks.request.mockRejectedValueOnce(new Error('daemon starting')).mockResolvedValueOnce(encReply(true));
    await d.loadEncoders();
    expect(get(d.encoders).encoderReady).toBe(false);

    await vi.advanceTimersByTimeAsync(d.ENCODER_RETRY_MS);
    expect(countType('list_encoders')).toBe(2);
    expect(get(d.encoders).encoderReady).toBe(true);
  });

  it('never ready: 21 requests in total, then no more', async () => {
    mocks.request.mockImplementation(async () => encReply(false));
    await d.loadEncoders();

    await vi.advanceTimersByTimeAsync(d.ENCODER_RETRY_MS * d.ENCODER_MAX_RETRIES);
    expect(countType('list_encoders')).toBe(21);

    await vi.advanceTimersByTimeAsync(60_000);
    expect(countType('list_encoders')).toBe(21);
  });

  it('a second call cancels the earlier retry chain', async () => {
    mocks.request.mockImplementation(async () => encReply(false));
    void d.loadEncoders(); // chain A: request at t=0, retry due at t=1500
    await vi.advanceTimersByTimeAsync(500);

    await d.loadEncoders(); // chain B: request at t=500, retry due at t=2000
    expect(countType('list_encoders')).toBe(2);

    // Up to t=2000: A1, B1, B2. Without cancellation A2 at t=1500 would add one more.
    await vi.advanceTimersByTimeAsync(1500);
    expect(countType('list_encoders')).toBe(3);
  });
});

// ---------------------------------------------------------------------------
// 6. Mic meter
// ---------------------------------------------------------------------------

describe('startMicMeter', () => {
  it('first start subscribes, enables the meter on the default device and registers a listener', () => {
    const stop = d.startMicMeter();
    expect(sent()).toContainEqual({ type: 'subscribe', data: { topics: ['meter'] } });
    expect(sent()).toContainEqual({ type: 'mic_meter', data: { enable: true, device: null } });
    expect(mocks.onEvent).toHaveBeenCalledTimes(1);
    stop();
  });

  it('first start with a device enables that device', () => {
    const stop = d.startMicMeter('mic-1');
    expect(sent()).toContainEqual({ type: 'mic_meter', data: { enable: true, device: 'mic-1' } });
    stop();
  });

  it('renews the enable command every 10 s', async () => {
    const stop = d.startMicMeter();
    expect(countEnable(true)).toBe(1);

    await vi.advanceTimersByTimeAsync(d.MIC_METER_RENEW_MS);
    expect(countEnable(true)).toBe(2);

    await vi.advanceTimersByTimeAsync(d.MIC_METER_RENEW_MS);
    expect(countEnable(true)).toBe(3);
    stop();
  });

  it('a second start shares the lease without re-subscribing', () => {
    const a = d.startMicMeter();
    const b = d.startMicMeter();
    expect(countType('subscribe')).toBe(1);
    expect(countEnable(true)).toBe(1);
    expect(mocks.onEvent).toHaveBeenCalledTimes(1);
    a();
    b();
  });

  it('a later start with a different device switches the meter once', async () => {
    const a = d.startMicMeter('mic-a');
    const b = d.startMicMeter('mic-b');
    expect(countEnable(true)).toBe(2);
    expect(sent().at(-1)).toEqual({ type: 'mic_meter', data: { enable: true, device: 'mic-b' } });

    // Renewals keep the new device.
    await vi.advanceTimersByTimeAsync(d.MIC_METER_RENEW_MS);
    expect(sent().at(-1)).toEqual({ type: 'mic_meter', data: { enable: true, device: 'mic-b' } });
    a();
    b();
  });

  it('a later start without a device does not switch', () => {
    const a = d.startMicMeter('mic-a');
    const b = d.startMicMeter(undefined);
    expect(countEnable(true)).toBe(1);
    a();
    b();
  });

  it('stopping one consumer keeps the lease and renewals', async () => {
    const a = d.startMicMeter();
    const b = d.startMicMeter();
    a();
    expect(countEnable(false)).toBe(0);
    expect(countType('unsubscribe')).toBe(0);

    const before = countEnable(true);
    await vi.advanceTimersByTimeAsync(d.MIC_METER_RENEW_MS);
    expect(countEnable(true)).toBe(before + 1);
    b();
  });

  it('stopping the same consumer twice does not release the other consumer', () => {
    const a = d.startMicMeter();
    const b = d.startMicMeter();
    a();
    a();
    expect(countEnable(false)).toBe(0);
    b();
    expect(countEnable(false)).toBe(1);
  });

  it('stopping the last consumer disables the meter, unsubscribes and removes the listener', async () => {
    const stop = d.startMicMeter('mic-x');
    stop();
    expect(sent()).toContainEqual({ type: 'mic_meter', data: { enable: false, device: null } });
    expect(sent()).toContainEqual({ type: 'unsubscribe', data: { topics: ['meter'] } });
    expect(mocks.handlers.size).toBe(0);

    // Renewal timer is stopped.
    const enables = countEnable(true);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(countEnable(true)).toBe(enables);
  });

  it('stopping the last consumer resets micLevel', () => {
    const stop = d.startMicMeter();
    fireMeter({ type: 'mic_level', data: { rms_db: -20, peak_db: -10 } });
    expect(get(d.micLevel).active).toBe(true);

    stop();
    expect(get(d.micLevel)).toEqual({ rmsDb: -100, peakDb: -100, active: false });
  });

  it('a mic_level event updates micLevel and sets active', () => {
    const stop = d.startMicMeter();
    fireMeter({ type: 'mic_level', data: { rms_db: -30, peak_db: -12 } });
    expect(get(d.micLevel)).toEqual({ rmsDb: -30, peakDb: -12, active: true });
    stop();
  });

  it('other event types leave micLevel unchanged', () => {
    const stop = d.startMicMeter();
    const before = get(d.micLevel);
    fireMeter({ type: 'status', data: {} });
    expect(get(d.micLevel)).toEqual(before);
    stop();
  });
});

// ---------------------------------------------------------------------------
// 7. Pure helpers
// ---------------------------------------------------------------------------

describe('iconSrc', () => {
  it('returns undefined for null, undefined and empty', () => {
    expect(d.iconSrc(null)).toBeUndefined();
    expect(d.iconSrc(undefined)).toBeUndefined();
    expect(d.iconSrc('')).toBeUndefined();
  });

  it('builds a PNG data URL', () => {
    expect(d.iconSrc('QUJD')).toBe('data:image/png;base64,QUJD');
  });
});

describe('sortApps', () => {
  it('puts playing apps first', () => {
    const sorted = d.sortApps([
      { exe: 'a.exe', name: 'A', active: false, lastActiveUnixMs: 999 },
      { exe: 'b.exe', name: 'B', active: true, lastActiveUnixMs: 1 },
    ]);
    expect(sorted.map((a) => a.exe)).toEqual(['b.exe', 'a.exe']);
  });

  it('then the most recently active first', () => {
    const sorted = d.sortApps([
      { exe: 'x.exe', name: 'X', lastActiveUnixMs: 5 },
      { exe: 'y.exe', name: 'Y', lastActiveUnixMs: 10 },
    ]);
    expect(sorted.map((a) => a.exe)).toEqual(['y.exe', 'x.exe']);
  });

  it('treats a missing lastActiveUnixMs as 0', () => {
    const sorted = d.sortApps([
      { exe: 'n.exe', name: 'N' },
      { exe: 'r.exe', name: 'R', lastActiveUnixMs: 1 },
    ]);
    expect(sorted.map((a) => a.exe)).toEqual(['r.exe', 'n.exe']);
  });

  it('sorts names case-insensitively', () => {
    const sorted = d.sortApps([
      { exe: '1.exe', name: 'beta' },
      { exe: '2.exe', name: 'Alpha' },
    ]);
    expect(sorted.map((a) => a.name)).toEqual(['Alpha', 'beta']);
  });

  it('falls back to exe when the name is blank', () => {
    const sorted = d.sortApps([
      { exe: 'zed.exe', name: '' },
      { exe: 'beta.exe', name: '' },
    ]);
    expect(sorted.map((a) => a.exe)).toEqual(['beta.exe', 'zed.exe']);
  });

  it('breaks ties by exe', () => {
    const sorted = d.sortApps([
      { exe: 'b.exe', name: 'Same' },
      { exe: 'a.exe', name: 'Same' },
    ]);
    expect(sorted.map((a) => a.exe)).toEqual(['a.exe', 'b.exe']);
  });

  it('returns a new array and does not mutate the input', () => {
    const input = [
      { exe: 'x.exe', name: 'X', lastActiveUnixMs: 1 },
      { exe: 'y.exe', name: 'Y', lastActiveUnixMs: 2 },
    ];
    const copy = [...input];
    const out = d.sortApps(input);
    expect(out).not.toBe(input);
    expect(input).toEqual(copy);
    expect(out.map((a) => a.exe)).toEqual(['y.exe', 'x.exe']);
  });
});

describe('dbToMeterFraction', () => {
  it('maps -100..0 dB linearly onto 0..1', () => {
    expect(d.dbToMeterFraction(-100)).toBe(0);
    expect(d.dbToMeterFraction(-50)).toBe(0.5);
    expect(d.dbToMeterFraction(0)).toBe(1);
  });

  it('clamps out-of-range values and maps NaN to 0', () => {
    expect(d.dbToMeterFraction(-150)).toBe(0);
    expect(d.dbToMeterFraction(5)).toBe(1);
    expect(d.dbToMeterFraction(NaN)).toBe(0);
  });
});

describe('volume curve', () => {
  it('volumePercentToDb hits the anchor points', () => {
    expect(d.volumePercentToDb(0)).toBe(-60);
    expect(d.volumePercentToDb(50)).toBe(-30);
    expect(d.volumePercentToDb(100)).toBe(0);
    expect(d.volumePercentToDb(150)).toBe(12);
  });

  it('volumePercentToDb clamps and treats NaN as mute', () => {
    expect(d.volumePercentToDb(-10)).toBe(-60);
    expect(d.volumePercentToDb(200)).toBe(12);
    expect(d.volumePercentToDb(NaN)).toBe(-60);
  });

  it('dbToVolumePercent hits the anchor points', () => {
    expect(d.dbToVolumePercent(-60)).toBe(0);
    expect(d.dbToVolumePercent(-30)).toBe(50);
    expect(d.dbToVolumePercent(0)).toBe(100);
    expect(d.dbToVolumePercent(12)).toBe(150);
  });

  it('dbToVolumePercent clamps and maps NaN to 0', () => {
    expect(d.dbToVolumePercent(-100)).toBe(0);
    expect(d.dbToVolumePercent(40)).toBe(150);
    expect(d.dbToVolumePercent(NaN)).toBe(0);
  });

  it('round-trips every integer percent from 0 to 150', () => {
    for (let pct = 0; pct <= 150; pct++) {
      expect(d.dbToVolumePercent(d.volumePercentToDb(pct))).toBeCloseTo(pct, 9);
    }
  });
});

describe('fallbackColor', () => {
  it('returns an hsl() string', () => {
    expect(d.fallbackColor('obs64.exe')).toMatch(/^hsl\(\d+ 55% 42%\)$/);
  });

  it('is stable for the same input', () => {
    expect(d.fallbackColor('obs64.exe')).toBe(d.fallbackColor('obs64.exe'));
  });

  it('is case-insensitive', () => {
    expect(d.fallbackColor('Game.EXE')).toBe(d.fallbackColor('game.exe'));
  });

  it('usually differs between different exes', () => {
    expect(d.fallbackColor('obs64.exe')).not.toBe(d.fallbackColor('discord.exe'));
  });
});

describe('initialOf', () => {
  it('uses the first character of the trimmed name, upper-cased', () => {
    expect(d.initialOf('  spotify', 'x.exe')).toBe('S');
  });

  it('falls back to the exe when the name is blank', () => {
    expect(d.initialOf('', 'obs64.exe')).toBe('O');
    expect(d.initialOf('   ', 'obs64.exe')).toBe('O');
  });

  it('returns ? when both are blank', () => {
    expect(d.initialOf('', '')).toBe('?');
  });
});

describe('constants', () => {
  it('exports the documented values', () => {
    expect(d.METER_FLOOR_DB).toBe(-100);
    expect(d.METER_CEIL_DB).toBe(0);
    expect(d.VOLUME_MAX_PCT).toBe(150);
    expect(d.VOLUME_FLOOR_DB).toBe(-60);
    expect(d.VOLUME_MAX_DB).toBe(12);
    expect(d.AUDIO_APPS_POLL_MS).toBe(2000);
    expect(d.ENCODER_RETRY_MS).toBe(1500);
    expect(d.ENCODER_MAX_RETRIES).toBe(20);
    expect(d.MIC_METER_RENEW_MS).toBe(10_000);
  });
});

describe('recordMicTest', () => {
  it('sends mic_test with a null device when none is given', async () => {
    mocks.request.mockImplementation(async () => ({
      type: 'mic_test',
      data: { wav_base64: btoa('RIFF'), duration_ms: 5000, peak_db: -12.5 },
    }));
    await d.recordMicTest();
    expect(sent()).toContainEqual({ type: 'mic_test', data: { device: null } });
  });

  it('decodes the WAV bytes and passes the metadata through', async () => {
    mocks.request.mockImplementation(async () => ({
      type: 'mic_test',
      data: { wav_base64: btoa('RIFF'), duration_ms: 5000, peak_db: -12.5 },
    }));
    const r = await d.recordMicTest('Mic A');
    expect(Array.from(r.wav)).toEqual([82, 73, 70, 70]);
    expect(r.durationMs).toBe(5000);
    expect(r.peakDb).toBe(-12.5);
    expect(sent()).toContainEqual({ type: 'mic_test', data: { device: 'Mic A' } });
  });

  it('rejects with the daemon error message', async () => {
    mocks.request.mockImplementation(async () => ({
      type: 'error',
      data: { code: 'worker_unavailable', message: 'nope', retryable: true },
    }));
    await expect(d.recordMicTest()).rejects.toThrow('nope');
  });
});

// ---------------------------------------------------------------------------
// D63 encoder wire names: new `encoder_ready`/`encoder_name` and old
// `ffmpeg_ready`/`ffmpeg_name` must both be accepted.
// ---------------------------------------------------------------------------

describe('D63 encoder wire names', () => {
  function newReply(ready: boolean, encoderName?: string): unknown {
    const entry: Record<string, unknown> = { backend: 'nvenc', codec: 'h264', works: true };
    if (encoderName !== undefined) entry.encoder_name = encoderName;
    return { type: 'encoders', data: { encoder_ready: ready, gpus: [], encoders: [entry] } };
  }

  function oldReply(ready: boolean, ffmpegName?: string): unknown {
    const entry: Record<string, unknown> = { backend: 'nvenc', codec: 'h264', works: true };
    if (ffmpegName !== undefined) entry.ffmpeg_name = ffmpegName;
    return { type: 'encoders', data: { ffmpeg_ready: ready, gpus: [], encoders: [entry] } };
  }

  it('1. new names: encoder_ready true + encoder_name are stored', async () => {
    mocks.request.mockImplementation(async () => newReply(true, 'h264_nvenc'));
    await d.loadEncoders();
    expect(get(d.encoders).encoderReady).toBe(true);
    expect(get(d.encoders).encoders[0].encoderName).toBe('h264_nvenc');
  });

  it('2. old names only: ffmpeg_ready true + ffmpeg_name are stored', async () => {
    mocks.request.mockImplementation(async () => oldReply(true, 'h264_nvenc'));
    await d.loadEncoders();
    expect(get(d.encoders).encoderReady).toBe(true);
    expect(get(d.encoders).encoders[0].encoderName).toBe('h264_nvenc');
  });

  it('3. both present: new names win', async () => {
    mocks.request.mockImplementation(async () => ({
      type: 'encoders',
      data: {
        encoder_ready: false,
        ffmpeg_ready: true,
        gpus: [],
        encoders: [{ backend: 'nvenc', codec: 'h264', works: true, encoder_name: 'new', ffmpeg_name: 'old' }],
      },
    }));
    await d.loadEncoders();
    expect(get(d.encoders).encoderReady).toBe(false);
    expect(get(d.encoders).encoders[0].encoderName).toBe('new');
  });

  it('4. neither ready field: encoderReady false and a retry is scheduled', async () => {
    mocks.request.mockImplementation(async () => ({ type: 'encoders', data: { gpus: [], encoders: [] } }));
    await d.loadEncoders();
    expect(get(d.encoders).encoderReady).toBe(false);
    expect(countType('list_encoders')).toBe(1);

    await vi.advanceTimersByTimeAsync(d.ENCODER_RETRY_MS);
    expect(countType('list_encoders')).toBe(2);
  });

  it('5. old-daemon reply: not ready, then ready after one retry', async () => {
    mocks.request.mockResolvedValueOnce(oldReply(false, 'h264_nvenc')).mockResolvedValueOnce(oldReply(true, 'h264_nvenc'));
    await d.loadEncoders();
    expect(get(d.encoders).encoderReady).toBe(false);

    await vi.advanceTimersByTimeAsync(d.ENCODER_RETRY_MS);
    expect(countType('list_encoders')).toBe(2);
    expect(get(d.encoders).encoderReady).toBe(true);
    expect(get(d.encoders).encoders[0].encoderName).toBe('h264_nvenc');
  });

  it('6. encoder entry with neither name: encoderName is empty string', async () => {
    mocks.request.mockImplementation(async () => newReply(true));
    await d.loadEncoders();
    expect(get(d.encoders).encoders[0].encoderName).toBe('');
  });
});
