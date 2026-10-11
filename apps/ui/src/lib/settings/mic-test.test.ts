import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  request: vi.fn(),
  onEvent: vi.fn(),
}));

vi.mock('../daemon', () => ({
  request: (...a: unknown[]) => mocks.request(...a),
  onEvent: (...a: unknown[]) => mocks.onEvent(...a),
}));

import { recordMicTest } from '../discovery';

function sent(): Array<Record<string, any>> {
  return mocks.request.mock.calls.map((c) => c[0] as Record<string, any>);
}

function binaryString(bytes: number[]): string {
  return String.fromCharCode(...bytes);
}

beforeEach(() => {
  mocks.request.mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('recordMicTest request', () => {
  it('with no argument sends exactly one mic_test request with device null', async () => {
    mocks.request.mockResolvedValue({
      type: 'mic_test',
      data: { wav_base64: btoa('RIFF'), duration_ms: 5000, peak_db: -12.5 },
    });
    await recordMicTest();
    expect(mocks.request).toHaveBeenCalledTimes(1);
    expect(sent()).toEqual([{ type: 'mic_test', data: { device: null } }]);
  });

  it('with a device id sends that id as the device', async () => {
    mocks.request.mockResolvedValue({
      type: 'mic_test',
      data: { wav_base64: btoa('RIFF'), duration_ms: 5000, peak_db: -12.5 },
    });
    await recordMicTest('{device-id-x}');
    expect(sent()).toEqual([{ type: 'mic_test', data: { device: '{device-id-x}' } }]);
  });

  it('decodes wav_base64 to bytes and passes duration and peak through', async () => {
    mocks.request.mockResolvedValue({
      type: 'mic_test',
      data: { wav_base64: btoa(binaryString([82, 73, 70, 70, 0x00, 0xff])), duration_ms: 5000, peak_db: -12.5 },
    });
    const r = await recordMicTest();
    expect(r.wav).toBeInstanceOf(Uint8Array);
    expect(Array.from(r.wav)).toEqual([82, 73, 70, 70, 0, 255]);
    expect(r.durationMs).toBe(5000);
    expect(r.peakDb).toBe(-12.5);
  });

  it('rejects with an Error carrying the daemon error message on an error reply', async () => {
    mocks.request.mockResolvedValue({
      type: 'error',
      data: { code: 'worker_unavailable', message: 'nope', retryable: true },
    });
    const p = recordMicTest();
    await expect(p).rejects.toBeInstanceOf(Error);
    await expect(recordMicTest()).rejects.toThrow('nope');
  });

  it('rejects when the reply is not a mic_test reply', async () => {
    mocks.request.mockResolvedValue({ type: 'ok' });
    await expect(recordMicTest()).rejects.toThrow('expected mic_test reply, got ok');
  });

  it('decodes a large (about 470 KiB) WAV payload to the full length quickly', async () => {
    const len = 480044;
    const chunk = 0x8000;
    const parts: string[] = [];
    for (let off = 0; off < len; off += chunk) {
      const n = Math.min(chunk, len - off);
      parts.push(String.fromCharCode(...new Array<number>(n).fill(0)));
    }
    const wav_base64 = btoa(parts.join(''));
    mocks.request.mockResolvedValue({
      type: 'mic_test',
      data: { wav_base64, duration_ms: 5000, peak_db: -100 },
    });
    const started = performance.now();
    const r = await recordMicTest();
    const elapsed = performance.now() - started;
    expect(r.wav.length).toBe(len);
    expect(r.wav.every((b) => b === 0)).toBe(true);
    expect(elapsed).toBeLessThan(2000);
  });

  it('does not create an object URL (the component does that)', async () => {
    const createObjectURL = vi.fn(() => 'blob:x');
    const original = (URL as unknown as { createObjectURL?: unknown }).createObjectURL;
    (URL as unknown as { createObjectURL: unknown }).createObjectURL = createObjectURL;
    try {
      mocks.request.mockResolvedValue({
        type: 'mic_test',
        data: { wav_base64: btoa('RIFF'), duration_ms: 5000, peak_db: -12.5 },
      });
      await recordMicTest();
      expect(createObjectURL).not.toHaveBeenCalled();
    } finally {
      (URL as unknown as { createObjectURL: unknown }).createObjectURL = original;
    }
  });
});
