import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { cacheInfo, clearCache, type AppCacheInfo } from './cache';

const info: AppCacheInfo = {
  dir: 'C:/Users/me/AppData/Local/ClipCheck/cache',
  playerBytes: 1024,
  editorBytes: 2048,
  bufferBytes: 0,
  totalBytes: 3072,
};

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
});

describe('cacheInfo', () => {
  it('invokes the app_cache_info command with no arguments', async () => {
    invokeMock.mockResolvedValueOnce(info);
    await cacheInfo();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('app_cache_info');
    expect(invokeMock.mock.calls[0]).toHaveLength(1);
  });

  it('returns the DTO the backend resolves with, unchanged', async () => {
    invokeMock.mockResolvedValueOnce(info);
    await expect(cacheInfo()).resolves.toBe(info);
  });

  it('propagates a backend error string unchanged', async () => {
    invokeMock.mockRejectedValueOnce('cache folder missing');
    await expect(cacheInfo()).rejects.toBe('cache folder missing');
  });
});

describe('clearCache', () => {
  it('invokes the app_clear_cache command with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ ...info, totalBytes: 0, playerBytes: 0, editorBytes: 0 });
    await clearCache();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('app_clear_cache');
    expect(invokeMock.mock.calls[0]).toHaveLength(1);
  });

  it('returns the size left after the clear', async () => {
    const left: AppCacheInfo = { ...info, totalBytes: 512, playerBytes: 512, editorBytes: 0 };
    invokeMock.mockResolvedValueOnce(left);
    await expect(clearCache()).resolves.toEqual(left);
  });

  it('propagates the rejection string unchanged (for example while an export runs)', async () => {
    invokeMock.mockRejectedValueOnce('an export is running');
    await expect(clearCache()).rejects.toBe('an export is running');
  });

  it('does not call the info command', async () => {
    invokeMock.mockResolvedValueOnce(info);
    await clearCache();
    expect(invokeMock).not.toHaveBeenCalledWith('app_cache_info');
  });
});

describe('command names', () => {
  it('cacheInfo and clearCache use different command names', async () => {
    invokeMock.mockResolvedValue(info);
    await cacheInfo();
    await clearCache();
    const names = invokeMock.mock.calls.map((c) => c[0]);
    expect(names).toEqual(['app_cache_info', 'app_clear_cache']);
  });
});
