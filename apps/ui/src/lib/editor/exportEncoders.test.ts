import { describe, expect, it } from 'vitest';
import { encoderLabelFor, encoderOptionsFor } from './exportEncoders';

describe('encoderLabelFor', () => {
  it('uses the friendly label when present', () => {
    expect(
      encoderLabelFor({
        name: 'NVIDIA H.264 Encoder MFT',
        hardware: true,
        label: 'NVIDIA GeForce RTX 5080 (NVENC H.264)',
      }),
    ).toBe('NVIDIA GeForce RTX 5080 (NVENC H.264)');
  });

  it('ignores an empty label and falls back to name plus GPU/CPU', () => {
    expect(encoderLabelFor({ name: 'X MFT', hardware: true, label: '  ' })).toBe('X MFT (GPU)');
    expect(encoderLabelFor({ name: 'Y MFT', hardware: false })).toBe('Y MFT (CPU)');
  });

  it('keeps Software (CPU) for the software encoder', () => {
    expect(encoderLabelFor({ name: 'software', hardware: false })).toBe('Software (CPU)');
  });
});

describe('encoderOptionsFor', () => {
  it('uses labels and keeps Auto first with the best hardware label appended', () => {
    const opts = encoderOptionsFor(
      [
        {
          name: 'NVIDIA H.264 Encoder MFT',
          hardware: true,
          label: 'NVIDIA GeForce RTX 5080 (NVENC H.264)',
        },
        { name: 'AMD H.264 Encoder MFT', hardware: true, label: 'AMD Radeon(TM) Graphics (AMF H.264)' },
        { name: 'software', hardware: false, label: 'Software (CPU)' },
      ],
      'auto',
    );
    expect(opts).toEqual([
      {
        value: 'auto',
        label: 'Auto (best available: NVIDIA GeForce RTX 5080 (NVENC H.264))',
      },
      { value: 'NVIDIA H.264 Encoder MFT', label: 'NVIDIA GeForce RTX 5080 (NVENC H.264)' },
      { value: 'AMD H.264 Encoder MFT', label: 'AMD Radeon(TM) Graphics (AMF H.264)' },
      { value: 'software', label: 'Software (CPU)' },
    ]);
  });

  it('uses plain Auto text when there is no hardware entry', () => {
    const opts = encoderOptionsFor([{ name: 'software', hardware: false }], 'auto');
    expect(opts[0]).toEqual({ value: 'auto', label: 'Auto (best available)' });
    expect(opts[1]).toEqual({ value: 'software', label: 'Software (CPU)' });
  });

  it('uses plain Auto text when the hardware entry has no label', () => {
    const opts = encoderOptionsFor([{ name: 'NV MFT', hardware: true }], 'auto');
    expect(opts[0].label).toBe('Auto (best available)');
    expect(opts[1]).toEqual({ value: 'NV MFT', label: 'NV MFT (GPU)' });
  });

  it('dedupes by name, keeping the first occurrence', () => {
    const opts = encoderOptionsFor(
      [
        { name: 'NV MFT', hardware: true, label: 'First' },
        { name: 'NV MFT', hardware: true, label: 'Second' },
      ],
      'auto',
    );
    expect(opts.filter((o) => o.value === 'NV MFT')).toEqual([{ value: 'NV MFT', label: 'First' }]);
    expect(opts).toHaveLength(2);
  });

  it('skips a stray auto entry from the backend', () => {
    const opts = encoderOptionsFor([{ name: 'auto', hardware: false }, { name: 'software', hardware: false }], 'auto');
    expect(opts.map((o) => o.value)).toEqual(['auto', 'software']);
  });

  it('offers only software while the list is still loading (null)', () => {
    expect(encoderOptionsFor(null, 'auto').map((o) => o.value)).toEqual(['auto', 'software']);
  });

  it('appends a current value missing from the list', () => {
    const opts = encoderOptionsFor([{ name: 'software', hardware: false }], 'Gone MFT');
    expect(opts[opts.length - 1]).toEqual({ value: 'Gone MFT', label: 'Gone MFT (CPU)' });
    expect(opts).toHaveLength(3);
  });

  it('does not append a current value already in the list', () => {
    const opts = encoderOptionsFor(
      [
        { name: 'NV MFT', hardware: true, label: 'NV' },
      ],
      'NV MFT',
    );
    expect(opts.map((o) => o.value)).toEqual(['auto', 'NV MFT']);
  });
});
