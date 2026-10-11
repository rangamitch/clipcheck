import { describe, expect, it } from 'vitest';
import { bytesToBase64 } from './api';

describe('bytesToBase64', () => {
  it('encodes PNG signature bytes', () => {
    expect(bytesToBase64([137, 80, 78, 71])).toBe('iVBORw==');
  });

  it('returns an empty string for no bytes', () => {
    expect(bytesToBase64([])).toBe('');
  });

  it('handles inputs longer than one chunk', () => {
    const bytes = Array.from({ length: 70000 }, (_, i) => i % 256);
    const expected = Buffer.from(bytes).toString('base64');
    expect(bytesToBase64(bytes)).toBe(expected);
  });
});
