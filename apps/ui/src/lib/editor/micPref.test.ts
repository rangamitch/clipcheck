import type { Writable } from 'svelte/store';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

// The settings store and the Tauri invoke are replaced by pure mocks. No daemon, no backend.
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../settings/store', async () => {
  const { writable: w } = await import('svelte/store');
  return { settings: w({ saved: {}, draft: null }) };
});

import { invoke } from '@tauri-apps/api/core';
import { settings } from '../settings/store';
import { pttMixGatedOf, sendMicPref, watchMicPref } from './micPref';

type Snapshot = { saved: unknown; draft: unknown };
const store = settings as unknown as Writable<Snapshot>;
const invokeMock = vi.mocked(invoke);

const cfg = (gated: unknown) => ({ audio: { mic: { ptt_mix_gated: gated } } });

let stop: (() => void) | null = null;

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  store.set({ saved: {}, draft: null });
});

afterEach(() => {
  stop?.();
  stop = null;
});

describe('pttMixGatedOf', () => {
  it('defaults to true when the value is missing or not a boolean', () => {
    expect(pttMixGatedOf(undefined)).toBe(true);
    expect(pttMixGatedOf(null)).toBe(true);
    expect(pttMixGatedOf({})).toBe(true);
    expect(pttMixGatedOf(cfg('no'))).toBe(true);
    expect(pttMixGatedOf(cfg(1))).toBe(true);
  });

  it('returns the stored boolean', () => {
    expect(pttMixGatedOf(cfg(false))).toBe(false);
    expect(pttMixGatedOf(cfg(true))).toBe(true);
  });
});

describe('sendMicPref', () => {
  it('invokes editor_set_mic_pref with the gated flag', async () => {
    await sendMicPref(false);
    expect(invokeMock).toHaveBeenCalledWith('editor_set_mic_pref', { gated: false });
  });

  it('swallows a rejected invoke', async () => {
    invokeMock.mockRejectedValueOnce(new Error('unknown command'));
    await expect(sendMicPref(true)).resolves.toBeUndefined();
  });
});

describe('watchMicPref', () => {
  it('sends the default (true) right away when the config is unknown', () => {
    stop = watchMicPref();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('editor_set_mic_pref', { gated: true });
  });

  it('sends only when the value changes, using the draft before the saved config', () => {
    store.set({ saved: cfg(true), draft: null });
    stop = watchMicPref();
    expect(invokeMock).toHaveBeenLastCalledWith('editor_set_mic_pref', { gated: true });
    invokeMock.mockClear();

    // An unrelated change that keeps the value true sends nothing.
    store.set({ saved: cfg(true), draft: { other: 1 } });
    expect(invokeMock).not.toHaveBeenCalled();

    // The draft wins over the saved config.
    store.set({ saved: cfg(true), draft: cfg(false) });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenLastCalledWith('editor_set_mic_pref', { gated: false });
  });

  it('stops sending after the returned function is called', () => {
    store.set({ saved: cfg(true), draft: null });
    const off = watchMicPref();
    invokeMock.mockClear();
    off();
    store.set({ saved: cfg(false), draft: null });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('keeps working when the backend rejects the command', async () => {
    invokeMock.mockRejectedValue(new Error('unavailable'));
    stop = watchMicPref();
    store.set({ saved: cfg(false), draft: null });
    await Promise.resolve();
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock).toHaveBeenLastCalledWith('editor_set_mic_pref', { gated: false });
  });
});
