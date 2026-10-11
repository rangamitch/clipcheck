import { describe, expect, it } from 'vitest';
import { gpuOptions, hardwareLines } from './pure';
import { VIDEO_DEFAULTS, uniqueGpus, videoDefaultWrites } from './video';

const NV = { name: 'NVIDIA GeForce RTX 5080', vendor: 'nvidia', index: 0, dedicatedMb: 16384 };
const AMD = { name: 'AMD Radeon(TM) Graphics', vendor: 'amd', index: 1, dedicatedMb: 512 };

describe('uniqueGpus', () => {
  it('keeps the first entry for each name, in order', () => {
    expect(uniqueGpus([NV, AMD, { ...NV, index: 2 }])).toEqual([NV, AMD]);
  });

  it('leaves a list without duplicates unchanged', () => {
    expect(uniqueGpus([NV, AMD])).toEqual([NV, AMD]);
    expect(uniqueGpus([])).toEqual([]);
  });

  it('makes the GPU option values and hardware lines unique (the probe-cache case)', () => {
    const gpus = uniqueGpus([NV, AMD, { ...NV, index: 2 }]);
    const rows = [{ backend: 'nvenc', codec: 'h264', works: true }];
    const values = gpuOptions(gpus, 'auto', true).map((o) => o.value);
    expect(new Set(values).size).toBe(values.length);
    const lines = hardwareLines(gpus, rows);
    expect(new Set(lines).size).toBe(lines.length);
  });
});

describe('video defaults', () => {
  it('match schema.rs Video::default', () => {
    expect(VIDEO_DEFAULTS).toEqual({
      encoder: 'auto',
      codec: 'h264',
      preset: 'high',
      resolution: '1080p',
      fps: 60,
      rate_control: 'quality',
      quality_cq: 23,
      bitrate_kbps: 14000,
      max_bitrate_kbps: 15000,
      keyframe_interval_s: 2,
      cursor: true,
      gpu: 'auto',
      encoder_speed: 'balanced',
      b_frames: 2,
      no_border_capture: false,
    });
  });

  it('videoDefaultWrites gives dotted video paths', () => {
    const writes = videoDefaultWrites();
    expect(writes).toContainEqual(['video.preset', 'high']);
    expect(writes.every(([path]) => path.startsWith('video.'))).toBe(true);
  });
});
