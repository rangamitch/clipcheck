/**
 * Tests for the logs and diagnostics helpers (logs.ts). Pure helpers are checked directly.
 * The Tauri wrappers are checked against a mocked `invoke`; the clipboard is a stubbed global.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => mocks.invoke(...args),
}));

import {
  collectDiagnostics,
  copyText,
  exportDiagnostics,
  friendlyError,
  getLogDir,
  openFolder,
  parentDir,
} from './logs';

const DEFAULT_ERROR = 'Something went wrong. Try again.';

beforeEach(() => {
  mocks.invoke.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('parentDir: Windows paths', () => {
  it('returns the folder of a file, keeping backslashes', () => {
    expect(parentDir('C:\\Users\\me\\clip.zip')).toBe('C:\\Users\\me');
  });

  it('returns the drive root for a file directly under a drive', () => {
    expect(parentDir('C:\\file')).toBe('C:\\');
    expect(parentDir('D:\\diagnostics.txt')).toBe('D:\\');
  });

  it('keeps forward slashes when the drive path uses them', () => {
    expect(parentDir('C:/file')).toBe('C:/');
    expect(parentDir('C:/a/b.zip')).toBe('C:/a');
  });

  it('handles a UNC share path', () => {
    expect(parentDir('\\\\server\\share\\f.zip')).toBe('\\\\server\\share');
  });

  it('handles mixed separators by using the last one', () => {
    expect(parentDir('C:/a\\b.zip')).toBe('C:/a');
  });
});

describe('parentDir: POSIX paths', () => {
  it('returns the folder of a file', () => {
    expect(parentDir('/home/me/clip.zip')).toBe('/home/me');
  });

  it('returns / for a file directly under the root', () => {
    expect(parentDir('/file')).toBe('/');
  });
});

describe('parentDir: edge cases', () => {
  it('returns empty string for a bare filename', () => {
    expect(parentDir('clip.zip')).toBe('');
  });

  it('returns empty string for an empty string', () => {
    expect(parentDir('')).toBe('');
  });

  it('ignores a trailing separator on the path', () => {
    expect(parentDir('C:\\Users\\me\\')).toBe('C:\\Users\\me');
    expect(parentDir('/home/me/')).toBe('/home/me');
  });

  it('trims surrounding whitespace before splitting', () => {
    expect(parentDir('  C:\\a\\b.txt  ')).toBe('C:\\a');
  });

  it('returns empty string for a bare drive letter', () => {
    expect(parentDir('C:')).toBe('');
  });
});

describe('friendlyError', () => {
  it('uses a rejected string as-is', () => {
    expect(friendlyError('Disk full')).toBe('Disk full');
  });

  it('trims a string and falls back when it is blank', () => {
    expect(friendlyError('  Disk full  ')).toBe('Disk full');
    expect(friendlyError('   ')).toBe(DEFAULT_ERROR);
    expect(friendlyError('')).toBe(DEFAULT_ERROR);
  });

  it('uses the message of an Error', () => {
    expect(friendlyError(new Error('boom'))).toBe('boom');
  });

  it('falls back when an Error has a blank message', () => {
    expect(friendlyError(new Error('   '), 'Custom fallback')).toBe('Custom fallback');
  });

  it('uses the message property of a plain object', () => {
    expect(friendlyError({ message: 'from object' })).toBe('from object');
  });

  it('falls back when the object message is not a non-blank string', () => {
    expect(friendlyError({ message: 42 })).toBe(DEFAULT_ERROR);
    expect(friendlyError({ message: '  ' })).toBe(DEFAULT_ERROR);
    expect(friendlyError({ code: 'x' })).toBe(DEFAULT_ERROR);
  });

  it('falls back for unknown values', () => {
    expect(friendlyError(undefined)).toBe(DEFAULT_ERROR);
    expect(friendlyError(null)).toBe(DEFAULT_ERROR);
    expect(friendlyError(42)).toBe(DEFAULT_ERROR);
  });

  it('uses the supplied fallback instead of the default', () => {
    expect(friendlyError(null, 'Could not open')).toBe('Could not open');
    expect(friendlyError('', 'Could not open')).toBe('Could not open');
  });
});

describe('Tauri wrappers', () => {
  it('getLogDir invokes get_log_dir and returns the folder', async () => {
    mocks.invoke.mockResolvedValue('C:\\Users\\me\\AppData\\Local\\ClipCheck\\logs');
    await expect(getLogDir()).resolves.toBe('C:\\Users\\me\\AppData\\Local\\ClipCheck\\logs');
    expect(mocks.invoke).toHaveBeenCalledWith('get_log_dir');
  });

  it('getLogDir rejects when the command returns an empty folder', async () => {
    mocks.invoke.mockResolvedValue('');
    await expect(getLogDir()).rejects.toThrow('The log folder could not be found.');
  });

  it('getLogDir propagates an invoke rejection', async () => {
    mocks.invoke.mockRejectedValue('no logs');
    await expect(getLogDir()).rejects.toBe('no logs');
  });

  it('collectDiagnostics invokes collect_diagnostics and returns its text', async () => {
    mocks.invoke.mockResolvedValue('diag text');
    await expect(collectDiagnostics()).resolves.toBe('diag text');
    expect(mocks.invoke).toHaveBeenCalledWith('collect_diagnostics');
  });

  it('exportDiagnostics invokes export_diagnostics and returns the path', async () => {
    mocks.invoke.mockResolvedValue('C:\\out\\diag.txt');
    await expect(exportDiagnostics()).resolves.toBe('C:\\out\\diag.txt');
    expect(mocks.invoke).toHaveBeenCalledWith('export_diagnostics');
  });

  it('exportDiagnostics rejects when the command returns an empty path', async () => {
    mocks.invoke.mockResolvedValue('');
    await expect(exportDiagnostics()).rejects.toThrow('The diagnostics file could not be saved.');
  });

  it('openFolder invokes open_folder with the path argument', async () => {
    mocks.invoke.mockResolvedValue(undefined);
    await expect(openFolder('C:\\out')).resolves.toBeUndefined();
    expect(mocks.invoke).toHaveBeenCalledWith('open_folder', { path: 'C:\\out' });
  });

  it('openFolder propagates an invoke rejection', async () => {
    mocks.invoke.mockRejectedValue(new Error('no explorer'));
    await expect(openFolder('C:\\out')).rejects.toThrow('no explorer');
  });
});

describe('copyText', () => {
  it('writes the text to navigator.clipboard', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    await copyText('hello diagnostics');
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith('hello diagnostics');
  });

  it('throws when the clipboard API is missing', async () => {
    vi.stubGlobal('navigator', {});
    await expect(copyText('x')).rejects.toThrow('The clipboard is not available here.');
  });

  it('propagates a clipboard write rejection', async () => {
    vi.stubGlobal('navigator', {
      clipboard: { writeText: vi.fn().mockRejectedValue(new Error('denied')) },
    });
    await expect(copyText('x')).rejects.toThrow('denied');
  });
});
