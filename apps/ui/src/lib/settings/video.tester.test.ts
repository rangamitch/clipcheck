import { describe, expect, it } from 'vitest';
import {
  BITRATE_STOPS_KBPS,
  PRESET_ROWS,
  bitrateChoice,
  bitrateOptions,
  codecAvailability,
  codecOptions,
  cqMax,
  cqToSlider,
  effectiveVideoKbps,
  estimateMBPerMinute,
  formatMbps,
  gpuOptions,
  hardwareLines,
  isBitrateStop,
  presetBitrate,
  presetCards,
  presetPatch,
  sliderToCq,
  vendorLabel,
  videoSummary,
  vramLabel,
  type GpuInfo,
  type ProbeRow,
} from './pure';

describe('preset table', () => {
  it('has the exact low/medium/high/ultra values', () => {
    expect(PRESET_ROWS.low).toEqual({ name: 'Low', resolution: '480p', fps: 30, kbps: 4000 });
    expect(PRESET_ROWS.medium).toEqual({ name: 'Standard', resolution: '720p', fps: 60, kbps: 8000 });
    expect(PRESET_ROWS.high).toEqual({ name: 'High', resolution: '1080p', fps: 60, kbps: 14000 });
    expect(PRESET_ROWS.ultra).toEqual({ name: 'Ultra', resolution: '1440p', fps: 60, kbps: 30000 });
  });

  it('presetBitrate returns the table kbps for each named preset', () => {
    expect(presetBitrate('low')).toBe(4000);
    expect(presetBitrate('medium')).toBe(8000);
    expect(presetBitrate('high')).toBe(14000);
    expect(presetBitrate('ultra')).toBe(30000);
  });

  it('presetBitrate returns null for custom, empty, and inherited Object keys', () => {
    expect(presetBitrate('custom')).toBeNull();
    expect(presetBitrate('')).toBeNull();
    expect(presetBitrate('constructor')).toBeNull();
    expect(presetBitrate('hasOwnProperty')).toBeNull();
    expect(presetBitrate('toString')).toBeNull();
  });

  it('effectiveVideoKbps uses the table for named presets and the custom value otherwise', () => {
    expect(effectiveVideoKbps({ preset: 'high', bitrate_kbps: 1000 })).toBe(14000);
    expect(effectiveVideoKbps({ preset: 'custom', bitrate_kbps: 12000 })).toBe(12000);
    expect(effectiveVideoKbps({ preset: 'constructor', bitrate_kbps: 6000 })).toBe(6000);
  });
});

describe('presetCards', () => {
  it('returns five cards in order low, medium, high, ultra, custom', () => {
    expect(presetCards().map((c) => c.value)).toEqual(['low', 'medium', 'high', 'ultra', 'custom']);
  });

  it('gives "resolution fps" details and null detail for custom', () => {
    const cards = presetCards();
    expect(cards[0].detail).toBe('480p 30 FPS');
    expect(cards[1].detail).toBe('720p 60 FPS');
    expect(cards[2].detail).toBe('1080p 60 FPS');
    expect(cards[3].detail).toBe('1440p 60 FPS');
    expect(cards[4].detail).toBeNull();
  });
});

describe('presetPatch', () => {
  it('named presets write exactly preset, resolution, fps and bitrate_kbps', () => {
    for (const p of ['low', 'medium', 'high', 'ultra']) {
      expect(Object.keys(presetPatch(p)).sort()).toEqual(['bitrate_kbps', 'fps', 'preset', 'resolution']);
    }
    expect(presetPatch('ultra')).toEqual({ preset: 'ultra', resolution: '1440p', fps: 60, bitrate_kbps: 30000 });
  });

  it('custom writes only the preset name', () => {
    expect(presetPatch('custom')).toEqual({ preset: 'custom' });
    expect(Object.keys(presetPatch('custom'))).toEqual(['preset']);
  });
});

describe('videoSummary', () => {
  it('low preset summary uses table kbps', () => {
    expect(videoSummary({ preset: 'low', codec: 'h264', resolution: '480p', fps: 30, bitrate_kbps: 1000 })).toBe(
      'Records 480p · 30 FPS · H.264 · 4 Mbps (≈30 MB per minute)',
    );
  });

  it('medium, high and ultra summaries use table kbps', () => {
    expect(videoSummary({ preset: 'medium', codec: 'h264', resolution: '720p', fps: 60, bitrate_kbps: 1000 })).toBe(
      'Records 720p · 60 FPS · H.264 · 8 Mbps (≈60 MB per minute)',
    );
    expect(videoSummary({ preset: 'high', codec: 'h264', resolution: '1080p', fps: 60, bitrate_kbps: 1000 })).toBe(
      'Records 1080p · 60 FPS · H.264 · 14 Mbps (≈105 MB per minute)',
    );
  });

  it('ultra with native resolution gives the documented example line', () => {
    expect(videoSummary({ preset: 'ultra', codec: 'h264', resolution: 'native', fps: 60, bitrate_kbps: 1000 })).toBe(
      'Records native · 60 FPS · H.264 · 30 Mbps (≈225 MB per minute)',
    );
  });

  it('custom preset uses bitrate_kbps and the hevc label H.265', () => {
    expect(videoSummary({ preset: 'custom', codec: 'hevc', resolution: '1080p', fps: 144, bitrate_kbps: 12000 })).toBe(
      'Records 1080p · 144 FPS · H.265 · 12 Mbps (≈90 MB per minute)',
    );
  });

  it('custom av1 gives the AV1 label and a fractional Mbps value', () => {
    expect(videoSummary({ preset: 'custom', codec: 'av1', resolution: '720p', fps: 30, bitrate_kbps: 1500 })).toBe(
      'Records 720p · 30 FPS · AV1 · 1.5 Mbps (≈11 MB per minute)',
    );
  });
});

describe('estimateMBPerMinute', () => {
  it('zero inputs give 0', () => {
    expect(estimateMBPerMinute(0, 0, 0)).toBe(0);
    expect(estimateMBPerMinute(0, 3, 0)).toBe(0);
  });

  it('14000 kbps video only is 105 MB per minute', () => {
    expect(estimateMBPerMinute(14000, 0, 0)).toBe(105);
  });

  it('audio tracks add linearly', () => {
    // (8000 + 2 * 160) * 0.0075 = 62.4
    expect(estimateMBPerMinute(8000, 2, 160)).toBe(62);
    // (0 + 4 * 128) * 0.0075 = 3.84
    expect(estimateMBPerMinute(0, 4, 128)).toBe(4);
    // Adding one track equals adding its kbps to the video.
    expect(estimateMBPerMinute(8000, 1, 160)).toBe(estimateMBPerMinute(8160, 0, 0));
  });

  it('rounds .5 up (Math.round semantics)', () => {
    // 200 kbps * 0.0075 = 1.5 -> 2
    expect(estimateMBPerMinute(200, 0, 0)).toBe(2);
    // 600 kbps * 0.0075 = 4.5 -> 5
    expect(estimateMBPerMinute(600, 0, 0)).toBe(5);
  });
});

describe('formatMbps', () => {
  it('formats kbps as Mbps with at most one decimal', () => {
    expect(formatMbps(1000)).toBe('1');
    expect(formatMbps(1500)).toBe('1.5');
    expect(formatMbps(80000)).toBe('80');
    expect(formatMbps(1)).toBe('0');
  });
});

describe('bitrate stops and choice', () => {
  it('has 12 stops from 1 to 80 Mbps', () => {
    expect(BITRATE_STOPS_KBPS).toHaveLength(12);
    expect(BITRATE_STOPS_KBPS[0]).toBe(1000);
    expect(BITRATE_STOPS_KBPS[11]).toBe(80000);
  });

  it('bitrateOptions is 12 stops followed by Custom', () => {
    const opts = bitrateOptions();
    expect(opts).toHaveLength(13);
    expect(opts[12]).toEqual({ value: 'custom', label: 'Custom…' });
    expect(opts[0]).toEqual({ value: '1000', label: '1 Mbps' });
    expect(opts[11]).toEqual({ value: '80000', label: '80 Mbps' });
  });

  it('bitrateChoice on every stop returns its String(kbps)', () => {
    for (const kbps of BITRATE_STOPS_KBPS) {
      expect(isBitrateStop(kbps)).toBe(true);
      expect(bitrateChoice(kbps)).toBe(String(kbps));
    }
  });

  it('bitrateChoice on 0 or 1 returns custom', () => {
    expect(bitrateChoice(0)).toBe('custom');
    expect(bitrateChoice(1)).toBe('custom');
  });

  it('bitrateChoice returns custom when the custom panel is open, even on a stop', () => {
    expect(bitrateChoice(14000, true)).toBe('custom');
  });
});

describe('cq and slider mapping', () => {
  it('cqMax is 63 for av1 and 51 for h264 and hevc', () => {
    expect(cqMax('av1')).toBe(63);
    expect(cqMax('h264')).toBe(51);
    expect(cqMax('hevc')).toBe(51);
  });

  it('cq 0 maps to the slider max and the max cq maps to 0', () => {
    expect(cqToSlider(0, 'h264')).toBe(51);
    expect(cqToSlider(51, 'h264')).toBe(0);
    expect(cqToSlider(0, 'av1')).toBe(63);
    expect(cqToSlider(63, 'av1')).toBe(0);
  });

  it('round trips every integer cq for h264, hevc and av1', () => {
    for (const codec of ['h264', 'hevc', 'av1']) {
      for (let cq = 0; cq <= cqMax(codec); cq++) {
        expect(sliderToCq(cqToSlider(cq, codec), codec)).toBe(cq);
      }
    }
  });

  it('non-integer inputs are rounded before mapping', () => {
    expect(cqToSlider(20.6, 'hevc')).toBe(30); // rounds to 21
    expect(sliderToCq(30.4, 'hevc')).toBe(21); // rounds to 30
  });

  it('out-of-range inputs are clamped', () => {
    expect(cqToSlider(-3, 'h264')).toBe(51);
    expect(cqToSlider(60, 'h264')).toBe(0);
    expect(sliderToCq(-1, 'av1')).toBe(63);
    expect(sliderToCq(100, 'av1')).toBe(0);
  });
});

describe('codecAvailability', () => {
  const allNvenc: ProbeRow[] = [
    { backend: 'nvenc', codec: 'h264', works: true },
    { backend: 'nvenc', codec: 'h265', works: true },
    { backend: 'nvenc', codec: 'av1', works: true },
  ];

  it('auto with all nvenc rows working makes every codec available', () => {
    expect(codecAvailability('h264', 'auto', allNvenc, true)).toEqual({ available: true, reason: null });
    expect(codecAvailability('hevc', 'auto', allNvenc, true)).toEqual({ available: true, reason: null });
    expect(codecAvailability('av1', 'auto', allNvenc, true)).toEqual({ available: true, reason: null });
  });

  it('nvenc av1 not working makes av1 unavailable with an NVENC reason', () => {
    const rows: ProbeRow[] = [
      { backend: 'nvenc', codec: 'h264', works: true },
      { backend: 'nvenc', codec: 'av1', works: false },
    ];
    const a = codecAvailability('av1', 'nvenc', rows, true);
    expect(a.available).toBe(false);
    expect(a.reason).toContain('NVIDIA NVENC');
  });

  it('software + av1 is unavailable with the fixed reason, even when not ready and rows say it works', () => {
    const rows: ProbeRow[] = [{ backend: 'software', codec: 'av1', works: true }];
    expect(codecAvailability('av1', 'software', rows, false)).toEqual({
      available: false,
      reason: 'AV1 needs a hardware encoder.',
    });
    expect(codecAvailability('av1', 'software', rows, true)).toEqual({
      available: false,
      reason: 'AV1 needs a hardware encoder.',
    });
  });

  it('ready=false disables nothing except CPU AV1', () => {
    expect(codecAvailability('h264', 'amf', [], false).available).toBe(true);
    expect(codecAvailability('hevc', 'nvenc', [], false).available).toBe(true);
    expect(codecAvailability('av1', 'auto', [], false).available).toBe(true);
    expect(codecAvailability('h264', 'software', [], false).available).toBe(true);
  });

  it('auto with only a software h264 row keeps h264 available', () => {
    const rows: ProbeRow[] = [{ backend: 'software', codec: 'h264', works: true }];
    expect(codecAvailability('h264', 'auto', rows, true).available).toBe(true);
  });

  it('auto with only a software av1 row makes av1 unavailable', () => {
    const rows: ProbeRow[] = [{ backend: 'software', codec: 'av1', works: true }];
    const a = codecAvailability('av1', 'auto', rows, true);
    expect(a.available).toBe(false);
    expect(a.reason).toBeTruthy();
  });

  it('amf encoder with only nvenc rows makes h264 unavailable with an AMD AMF reason', () => {
    const a = codecAvailability('h264', 'amf', allNvenc, true);
    expect(a.available).toBe(false);
    expect(a.reason).toContain('AMD AMF');
  });

  it('a probe row with codec hevc does not satisfy hevc (probe name is h265)', () => {
    const rows: ProbeRow[] = [{ backend: 'nvenc', codec: 'hevc', works: true }];
    expect(codecAvailability('hevc', 'auto', rows, true).available).toBe(false);
  });

  it('an explicit encoder does not fall back to a different backend', () => {
    const rows: ProbeRow[] = [
      { backend: 'nvenc', codec: 'h264', works: false },
      { backend: 'software', codec: 'h264', works: true },
    ];
    expect(codecAvailability('h264', 'nvenc', rows, true).available).toBe(false);
  });

  it('software encoder accepts a working software h264 row', () => {
    const rows: ProbeRow[] = [{ backend: 'software', codec: 'h264', works: true }];
    expect(codecAvailability('h264', 'software', rows, true).available).toBe(true);
  });
});

describe('codecOptions', () => {
  const rows: ProbeRow[] = [
    { backend: 'nvenc', codec: 'h264', works: true },
    { backend: 'nvenc', codec: 'h265', works: true },
  ];

  it('returns h264, hevc, av1 in that order', () => {
    expect(codecOptions('auto', rows, true).map((o) => o.value)).toEqual(['h264', 'hevc', 'av1']);
  });

  it('sets disabled and title consistently with codecAvailability', () => {
    const opts = codecOptions('auto', rows, true);
    expect(opts[0]).toMatchObject({ disabled: false, title: undefined });
    expect(opts[1]).toMatchObject({ disabled: false, title: undefined });
    // No nvenc av1 row, so av1 is unavailable under auto.
    expect(opts[2].disabled).toBe(true);
    expect(opts[2].title).toBe(codecAvailability('av1', 'auto', rows, true).reason);
  });

  it('keeps the choice labels from CODEC_CHOICES', () => {
    expect(codecOptions('software', rows, true)[2].label).toBe('AV1 (smallest, needs a hardware encoder)');
  });
});

describe('gpuOptions', () => {
  const gpu: GpuInfo = { name: 'NVIDIA GeForce RTX 5080', vendor: 'nvidia', dedicatedMb: 16384 };

  it('first option is Automatic', () => {
    expect(gpuOptions([gpu], 'auto', true)[0]).toEqual({ value: 'auto', label: 'Automatic (best available)' });
  });

  it('a GPU with dedicatedMb 0 has no VRAM text', () => {
    const [, opt] = gpuOptions([{ name: 'Intel UHD', vendor: 'intel', dedicatedMb: 0 }], 'auto', true);
    expect(opt.label).toBe('Intel UHD · Intel');
  });

  it('2048 MB shows as 2 GB', () => {
    const [, opt] = gpuOptions([{ name: 'Card', vendor: 'amd', dedicatedMb: 2048 }], 'auto', true);
    expect(opt.label).toBe('Card · AMD · 2 GB');
  });

  it('a saved GPU not detected is appended last with Not found when ready', () => {
    const opts = gpuOptions([gpu], 'Old Card', true);
    const last = opts[opts.length - 1];
    expect(last.value).toBe('Old Card');
    expect(last.label).toContain('Not found');
  });

  it('a saved GPU not detected while not ready is appended with the bare name', () => {
    const opts = gpuOptions([gpu], 'Old Card', false);
    expect(opts[opts.length - 1]).toEqual({ value: 'Old Card', label: 'Old Card' });
  });

  it('saved auto never appends an extra option', () => {
    expect(gpuOptions([gpu], 'auto', true)).toHaveLength(2);
    expect(gpuOptions([], 'auto', false)).toEqual([{ value: 'auto', label: 'Automatic (best available)' }]);
  });

  it('duplicated GPU names are not deduplicated by the helper', () => {
    const opts = gpuOptions([gpu, { ...gpu, dedicatedMb: 8192 }], 'auto', true);
    expect(opts.filter((o) => o.value === gpu.name)).toHaveLength(2);
    expect(opts).toHaveLength(3);
  });
});

describe('hardwareLines', () => {
  const rtx: GpuInfo = { name: 'NVIDIA GeForce RTX 5080', vendor: 'nvidia', dedicatedMb: 16384 };

  it('NVIDIA GPU with working rows gives the documented line', () => {
    const rows: ProbeRow[] = [
      { backend: 'nvenc', codec: 'h264', works: true },
      { backend: 'nvenc', codec: 'h265', works: true },
      { backend: 'nvenc', codec: 'av1', works: true },
    ];
    expect(hardwareLines([rtx], rows)).toEqual(['NVIDIA GeForce RTX 5080 — NVENC H.264 ✓ H.265 ✓ AV1 ✓']);
  });

  it('software rows add a Software (CPU) line', () => {
    const rows: ProbeRow[] = [
      { backend: 'nvenc', codec: 'h264', works: true },
      { backend: 'software', codec: 'h264', works: true },
      { backend: 'software', codec: 'h265', works: true },
      { backend: 'software', codec: 'av1', works: false },
    ];
    expect(hardwareLines([rtx], rows)).toEqual([
      'NVIDIA GeForce RTX 5080 — NVENC H.264 ✓ H.265 ✗ AV1 ✗',
      'Software (CPU) — H.264 ✓ H.265 ✓ AV1 ✗',
    ]);
  });

  it('an NVIDIA GPU with no rows shows crosses for every codec', () => {
    expect(hardwareLines([rtx], [])).toEqual(['NVIDIA GeForce RTX 5080 — NVENC H.264 ✗ H.265 ✗ AV1 ✗']);
  });

  it('no rows and no software means no software line', () => {
    const lines = hardwareLines([rtx], []);
    expect(lines.some((l) => l.startsWith('Software (CPU)'))).toBe(false);
  });

  it('empty gpus and empty rows give no lines', () => {
    expect(hardwareLines([], [])).toEqual([]);
  });

  it('an unknown vendor says no supported hardware encoder', () => {
    expect(hardwareLines([{ name: 'Mystery', vendor: 'matrox', dedicatedMb: 0 }], [])).toEqual([
      'Mystery — no supported hardware encoder',
    ]);
  });

  it('a hevc probe row does not count as H.265 in the hardware line', () => {
    const rows: ProbeRow[] = [{ backend: 'nvenc', codec: 'hevc', works: true }];
    expect(hardwareLines([rtx], rows)[0]).toContain('H.265 ✗');
  });
});

describe('vramLabel and vendorLabel', () => {
  it('1023 MB stays in MB and 1024 MB becomes 1 GB', () => {
    expect(vramLabel(1023)).toBe('1023 MB');
    expect(vramLabel(1024)).toBe('1 GB');
  });

  it('zero and negative VRAM give an empty label', () => {
    expect(vramLabel(0)).toBe('');
    expect(vramLabel(-1)).toBe('');
  });

  it('vendorLabel maps known vendors case-insensitively and passes others through', () => {
    expect(vendorLabel('nvidia')).toBe('NVIDIA');
    expect(vendorLabel('AMD')).toBe('AMD');
    expect(vendorLabel('intel')).toBe('Intel');
    expect(vendorLabel('Matrox')).toBe('Matrox');
  });
});

describe('BITRATE_STOPS_KBPS consistency', () => {
  it('every stop is a valid bitrate within the daemon range', () => {
    for (const kbps of BITRATE_STOPS_KBPS) {
      expect(kbps).toBeGreaterThanOrEqual(1000);
      expect(kbps).toBeLessThanOrEqual(100000);
    }
  });
});
