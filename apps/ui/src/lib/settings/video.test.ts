import { describe, expect, it } from 'vitest';
import {
  AUTO_GPU,
  BITRATE_STOPS_KBPS,
  PRESET_ROWS,
  bitrateChoice,
  bitrateOptions,
  codecAvailability,
  codecOptions,
  cqMax,
  cqToSlider,
  diffPatch,
  effectiveVideoKbps,
  estimateMBPerMinute,
  formatMbps,
  gpuOptions,
  hardwareLines,
  isBitrateStop,
  presetBitrate,
  presetCards,
  presetPatch,
  setPath,
  sliderToCq,
  vendorLabel,
  videoSummary,
  vramLabel,
  type ProbeRow,
} from './pure';

const row = (backend: string, codec: string, works = true): ProbeRow => ({ backend, codec, works });

/** Every codec works on NVENC and on software H.264/H.265. */
const NVIDIA_FULL: ProbeRow[] = [
  row('nvenc', 'h264'),
  row('nvenc', 'h265'),
  row('nvenc', 'av1'),
  row('software', 'h264'),
  row('software', 'h265'),
];

describe('preset table (schema.rs Preset::values)', () => {
  it('has the design values: 480p/30/4000, 720p/60/8000, 1080p/60/14000, 1440p/60/30000', () => {
    expect(PRESET_ROWS).toEqual({
      low: { name: 'Low', resolution: '480p', fps: 30, kbps: 4000 },
      medium: { name: 'Standard', resolution: '720p', fps: 60, kbps: 8000 },
      high: { name: 'High', resolution: '1080p', fps: 60, kbps: 14000 },
      ultra: { name: 'Ultra', resolution: '1440p', fps: 60, kbps: 30000 },
    });
  });

  it('presetBitrate returns the table kbps for each named preset', () => {
    expect(presetBitrate('low')).toBe(4000);
    expect(presetBitrate('medium')).toBe(8000);
    expect(presetBitrate('high')).toBe(14000);
    expect(presetBitrate('ultra')).toBe(30000);
  });

  it.each(['custom', 'bogus', 'toString', ''])('presetBitrate(%s) is null', (name) => {
    expect(presetBitrate(name)).toBeNull();
  });

  it('effectiveVideoKbps uses the table for a named preset and bitrate_kbps for custom', () => {
    expect(effectiveVideoKbps({ preset: 'high', bitrate_kbps: 5000 })).toBe(14000);
    expect(effectiveVideoKbps({ preset: 'custom', bitrate_kbps: 9000 })).toBe(9000);
    expect(effectiveVideoKbps({ preset: 'bogus', bitrate_kbps: 7000 })).toBe(7000);
  });
});

describe('presetCards', () => {
  it('gives five cards in order: Low, Standard, High, Ultra, Custom', () => {
    expect(presetCards().map((c) => c.value)).toEqual(['low', 'medium', 'high', 'ultra', 'custom']);
  });

  it('uses the card wording from the design', () => {
    const cards = presetCards();
    expect(cards[0]).toEqual({
      value: 'low',
      name: 'Low',
      blurb: 'Lower-end PCs & small files',
      detail: '480p 30 FPS',
    });
    expect(cards[1]).toEqual({ value: 'medium', name: 'Standard', blurb: 'Balanced quality & size', detail: '720p 60 FPS' });
    expect(cards[2]).toEqual({ value: 'high', name: 'High', blurb: 'Sharp footage', detail: '1080p 60 FPS' });
    expect(cards[3]).toEqual({ value: 'ultra', name: 'Ultra', blurb: 'Max detail', detail: '1440p 60 FPS' });
    expect(cards[4]).toEqual({ value: 'custom', name: 'Custom', blurb: 'Dial in your own settings', detail: null });
  });
});

describe('presetPatch (what a card click writes)', () => {
  it.each([
    ['low', { preset: 'low', resolution: '480p', fps: 30, bitrate_kbps: 4000 }],
    ['medium', { preset: 'medium', resolution: '720p', fps: 60, bitrate_kbps: 8000 }],
    ['high', { preset: 'high', resolution: '1080p', fps: 60, bitrate_kbps: 14000 }],
    ['ultra', { preset: 'ultra', resolution: '1440p', fps: 60, bitrate_kbps: 30000 }],
  ])('%s writes preset, resolution, fps and bitrate only', (name, expected) => {
    expect(presetPatch(name)).toEqual(expected);
  });

  it('custom writes only the preset name, so the last numbers stay', () => {
    expect(presetPatch('custom')).toEqual({ preset: 'custom' });
  });

  it('an unknown name writes only the preset name', () => {
    expect(presetPatch('bogus')).toEqual({ preset: 'bogus' });
  });

  it('clicking a preset in the draft changes only those four video keys in the patch', () => {
    const saved = {
      video: { preset: 'custom', resolution: '720p', fps: 30, bitrate_kbps: 5000, codec: 'h264', cursor: true },
    };
    let draft: Record<string, unknown> = saved;
    for (const [key, value] of Object.entries(presetPatch('high'))) {
      draft = setPath(draft, `video.${key}`, value);
    }
    expect(diffPatch(saved, draft)).toEqual({
      video: { preset: 'high', resolution: '1080p', fps: 60, bitrate_kbps: 14000 },
    });
  });

  it('clicking Custom from a named preset patches only video.preset', () => {
    const saved = { video: { preset: 'high', resolution: '1080p', fps: 60, bitrate_kbps: 14000 } };
    let draft: Record<string, unknown> = saved;
    for (const [key, value] of Object.entries(presetPatch('custom'))) {
      draft = setPath(draft, `video.${key}`, value);
    }
    expect(diffPatch(saved, draft)).toEqual({ video: { preset: 'custom' } });
  });

  it('clicking the preset that is already selected produces an empty patch', () => {
    const saved = { video: { preset: 'high', resolution: '1080p', fps: 60, bitrate_kbps: 14000 } };
    let draft: Record<string, unknown> = saved;
    for (const [key, value] of Object.entries(presetPatch('high'))) {
      draft = setPath(draft, `video.${key}`, value);
    }
    expect(diffPatch(saved, draft)).toEqual({});
  });
});

describe('videoSummary', () => {
  it('matches the design example exactly', () => {
    expect(
      videoSummary({ preset: 'high', codec: 'h264', resolution: '1080p', fps: 60, bitrate_kbps: 14000 }),
    ).toBe('Records 1080p · 60 FPS · H.264 · 14 Mbps (≈105 MB per minute)');
  });

  it('a named preset uses the table bitrate, not the draft bitrate_kbps', () => {
    expect(
      videoSummary({ preset: 'low', codec: 'h264', resolution: '480p', fps: 30, bitrate_kbps: 99000 }),
    ).toBe('Records 480p · 30 FPS · H.264 · 4 Mbps (≈30 MB per minute)');
  });

  it('custom uses bitrate_kbps and the H.265 label', () => {
    expect(
      videoSummary({ preset: 'custom', codec: 'hevc', resolution: '720p', fps: 60, bitrate_kbps: 8500 }),
    ).toBe('Records 720p · 60 FPS · H.265 · 8.5 Mbps (≈64 MB per minute)');
  });

  it('native and AV1 at 30 Mbps', () => {
    expect(
      videoSummary({ preset: 'ultra', codec: 'av1', resolution: 'native', fps: 60, bitrate_kbps: 1 }),
    ).toBe('Records native · 60 FPS · AV1 · 30 Mbps (≈225 MB per minute)');
  });
});

describe('estimateMBPerMinute', () => {
  it.each<[string, number, number, number, number]>([
    ['14 Mbps video only', 14000, 0, 0, 105],
    ['14 Mbps video + two 192k tracks', 14000, 2, 192, 108],
    ['4 Mbps video + one 128k track', 4000, 1, 128, 31],
    ['nothing recorded', 0, 0, 0, 0],
    ['1 Mbps video rounds 7.5 MB up to 8', 1000, 0, 0, 8],
    ['30 Mbps video', 30000, 0, 0, 225],
    ['audio only, 3 tracks at 192k (576 kbps x 7.5 = 4.32 MB)', 0, 3, 192, 4],
  ])('%s -> %s MB', (_label, video, tracks, audio, expected) => {
    expect(estimateMBPerMinute(video, tracks, audio)).toBe(expected);
  });
});

describe('bitrate stops and custom', () => {
  it('has the twelve stops from 1 to 80 Mbps', () => {
    expect(BITRATE_STOPS_KBPS).toEqual([1000, 2000, 4000, 6000, 8000, 10000, 14000, 20000, 30000, 40000, 60000, 80000]);
  });

  it('isBitrateStop is true only for the stops', () => {
    expect(isBitrateStop(14000)).toBe(true);
    expect(isBitrateStop(1000)).toBe(true);
    expect(isBitrateStop(14500)).toBe(false);
    expect(isBitrateStop(0)).toBe(false);
  });

  it('a stop value selects its own option', () => {
    expect(bitrateChoice(14000)).toBe('14000');
    expect(bitrateChoice(80000)).toBe('80000');
  });

  it('an off-stop value selects Custom', () => {
    expect(bitrateChoice(14500)).toBe('custom');
    expect(bitrateChoice(150)).toBe('custom');
  });

  it('opening Custom on a stop value still selects Custom', () => {
    expect(bitrateChoice(14000, true)).toBe('custom');
  });

  it('options are the stops in Mbps, then Custom…', () => {
    const opts = bitrateOptions();
    expect(opts[0]).toEqual({ value: '1000', label: '1 Mbps' });
    expect(opts[6]).toEqual({ value: '14000', label: '14 Mbps' });
    expect(opts[opts.length - 1]).toEqual({ value: 'custom', label: 'Custom…' });
    expect(opts).toHaveLength(BITRATE_STOPS_KBPS.length + 1);
  });

  it.each([
    [12000, '12'],
    [3900, '3.9'],
    [500, '0.5'],
    [0, '0'],
    [16300, '16.3'],
  ])('formatMbps(%s) -> %s', (kbps, text) => {
    expect(formatMbps(kbps)).toBe(text);
  });
});

describe('quality (CQ) slider mapping', () => {
  it('cqMax is 51 for H.264 and H.265 and 63 for AV1', () => {
    expect(cqMax('h264')).toBe(51);
    expect(cqMax('hevc')).toBe(51);
    expect(cqMax('av1')).toBe(63);
  });

  it('larger slider position means better quality (lower CQ)', () => {
    expect(cqToSlider(0, 'h264')).toBe(51);
    expect(cqToSlider(51, 'h264')).toBe(0);
    expect(cqToSlider(23, 'h264')).toBe(28);
  });

  it('round trips every CQ value for H.264/H.265 and AV1', () => {
    for (let cq = 0; cq <= 51; cq += 1) {
      expect(sliderToCq(cqToSlider(cq, 'h264'), 'h264')).toBe(cq);
    }
    for (let cq = 0; cq <= 63; cq += 1) {
      expect(sliderToCq(cqToSlider(cq, 'av1'), 'av1')).toBe(cq);
    }
  });

  it('clamps out-of-range inputs', () => {
    expect(sliderToCq(-5, 'h264')).toBe(51);
    expect(sliderToCq(99, 'h264')).toBe(0);
    expect(cqToSlider(60, 'h264')).toBe(0);
    expect(cqToSlider(-3, 'av1')).toBe(63);
  });
});

describe('codecAvailability: per backend and works combination', () => {
  it('CPU never does AV1, whatever the probe says', () => {
    expect(codecAvailability('av1', 'software', NVIDIA_FULL, true)).toEqual({
      available: false,
      reason: 'AV1 needs a hardware encoder.',
    });
    expect(codecAvailability('av1', 'software', [], false).available).toBe(false);
  });

  it('before the probe is ready nothing is disabled (except CPU AV1)', () => {
    expect(codecAvailability('hevc', 'nvenc', [], false)).toEqual({ available: true, reason: null });
    expect(codecAvailability('av1', 'auto', [], false)).toEqual({ available: true, reason: null });
  });

  it.each<[string, string, ProbeRow[], boolean]>([
    ['h264', 'auto', NVIDIA_FULL, true],
    ['hevc', 'auto', NVIDIA_FULL, true],
    ['av1', 'auto', NVIDIA_FULL, true],
    ['h264', 'nvenc', NVIDIA_FULL, true],
    ['av1', 'nvenc', NVIDIA_FULL, true],
    ['h264', 'software', NVIDIA_FULL, true],
    ['hevc', 'software', NVIDIA_FULL, true],
    ['av1', 'auto', [row('amf', 'av1')], true],
    ['hevc', 'amf', [row('amf', 'h265')], true],
    ['h264', 'qsv', [row('qsv', 'h264')], true],
    ['hevc', 'auto', [row('software', 'h265')], true],
    ['h264', 'auto', [row('software', 'h264')], true],
  ])('%s with %s on %j is available', (codec, encoder, rows, ready) => {
    expect(codecAvailability(codec as 'h264', encoder, rows, ready)).toEqual({ available: true, reason: null });
  });

  it.each<[string, string, ProbeRow[], string]>([
    ['av1', 'auto', [row('software', 'av1')], 'No working encoder for AV1 on this PC.'],
    ['av1', 'auto', [], 'No working encoder for AV1 on this PC.'],
    ['h264', 'auto', [], 'No working encoder for H.264 on this PC.'],
    ['av1', 'nvenc', [row('nvenc', 'h264'), row('nvenc', 'h265')], "NVIDIA NVENC can't encode AV1 on this PC."],
    ['h264', 'amf', [row('nvenc', 'h264')], "AMD AMF can't encode H.264 on this PC."],
    ['hevc', 'qsv', [], "Intel Quick Sync can't encode H.265 on this PC."],
    ['h264', 'software', [], "Software (CPU) can't encode H.264 on this PC."],
  ])('%s with %s is unavailable: %s', (codec, encoder, rows, reason) => {
    expect(codecAvailability(codec as 'h264', encoder, rows, true)).toEqual({ available: false, reason });
  });

  it('a row that does not work does not count', () => {
    const rows = [row('nvenc', 'av1', false)];
    expect(codecAvailability('av1', 'nvenc', rows, true).available).toBe(false);
    expect(codecAvailability('av1', 'auto', rows, true).available).toBe(false);
  });

  it('the probe names H.265 as h265, so a hevc row does not match the config codec', () => {
    const rows = [row('nvenc', 'hevc')];
    expect(codecAvailability('hevc', 'nvenc', rows, true).available).toBe(false);
  });

  it('Automatic does not fall back to software for AV1', () => {
    const rows = [row('software', 'av1')];
    expect(codecAvailability('av1', 'auto', rows, true).available).toBe(false);
  });

  it('codecOptions gives three options with disabled flags and tooltips', () => {
    const opts = codecOptions('nvenc', [row('nvenc', 'h264'), row('nvenc', 'h265')], true);
    expect(opts.map((o) => o.value)).toEqual(['h264', 'hevc', 'av1']);
    expect(opts.map((o) => o.disabled)).toEqual([false, false, true]);
    expect(opts[0].title).toBeUndefined();
    expect(opts[2].title).toBe("NVIDIA NVENC can't encode AV1 on this PC.");
  });

  it('codecOptions disables nothing while the probe is not ready, except CPU AV1', () => {
    expect(codecOptions('auto', [], false).map((o) => o.disabled)).toEqual([false, false, false]);
    expect(codecOptions('software', [], false).map((o) => o.disabled)).toEqual([false, false, true]);
  });
});

describe('gpuOptions', () => {
  const rtx = { name: 'NVIDIA GeForce RTX 5080', vendor: 'nvidia', dedicatedMb: 16384 };
  const igpu = { name: 'Intel UHD Graphics', vendor: 'intel', dedicatedMb: 0 };
  const radeon = { name: 'Radeon RX 6600', vendor: 'amd', dedicatedMb: 512 };

  it('with no GPUs, only Automatic', () => {
    expect(gpuOptions([], AUTO_GPU, true)).toEqual([{ value: 'auto', label: 'Automatic (best available)' }]);
  });

  it('lists Automatic first, then each GPU with its vendor and VRAM', () => {
    expect(gpuOptions([rtx, igpu, radeon], 'auto', true)).toEqual([
      { value: 'auto', label: 'Automatic (best available)' },
      { value: 'NVIDIA GeForce RTX 5080', label: 'NVIDIA GeForce RTX 5080 · NVIDIA · 16 GB' },
      { value: 'Intel UHD Graphics', label: 'Intel UHD Graphics · Intel' },
      { value: 'Radeon RX 6600', label: 'Radeon RX 6600 · AMD · 512 MB' },
    ]);
  });

  it('keeps a saved GPU that is missing and marks it Not found', () => {
    const opts = gpuOptions([rtx], 'Old Card 1080', true);
    expect(opts[opts.length - 1]).toEqual({ value: 'Old Card 1080', label: 'Old Card 1080 (Not found on this PC)' });
    expect(opts).toHaveLength(3);
  });

  it('a saved GPU that is missing while the probe is not ready is shown without the Not found mark', () => {
    const opts = gpuOptions([], 'NVIDIA GeForce RTX 5080', false);
    expect(opts[opts.length - 1]).toEqual({ value: 'NVIDIA GeForce RTX 5080', label: 'NVIDIA GeForce RTX 5080' });
  });

  it('adds no extra entry when the saved GPU is present or Automatic', () => {
    expect(gpuOptions([rtx], rtx.name, true)).toHaveLength(2);
    expect(gpuOptions([rtx], AUTO_GPU, true)).toHaveLength(2);
  });

  it('vramLabel and vendorLabel', () => {
    expect(vramLabel(0)).toBe('');
    expect(vramLabel(512)).toBe('512 MB');
    expect(vramLabel(1024)).toBe('1 GB');
    expect(vramLabel(16384)).toBe('16 GB');
    expect(vendorLabel('nvidia')).toBe('NVIDIA');
    expect(vendorLabel('AMD')).toBe('AMD');
    expect(vendorLabel('other')).toBe('other');
  });
});

describe('hardwareLines', () => {
  it('builds the design example line and a CPU line', () => {
    const lines = hardwareLines(
      [{ name: 'NVIDIA GeForce RTX 5080', vendor: 'nvidia', dedicatedMb: 16384 }],
      NVIDIA_FULL,
    );
    expect(lines).toEqual([
      'NVIDIA GeForce RTX 5080 — NVENC H.264 ✓ H.265 ✓ AV1 ✓',
      'Software (CPU) — H.264 ✓ H.265 ✓ AV1 ✗',
    ]);
  });

  it('marks a missing or failing codec with a cross', () => {
    const lines = hardwareLines(
      [{ name: 'Intel UHD Graphics', vendor: 'intel', dedicatedMb: 0 }],
      [row('qsv', 'h264'), row('qsv', 'h265', false)],
    );
    expect(lines).toEqual(['Intel UHD Graphics — Quick Sync H.264 ✓ H.265 ✗ AV1 ✗']);
  });

  it('a GPU from an unknown vendor has no hardware encoder line', () => {
    expect(hardwareLines([{ name: 'Mystery GPU', vendor: 'other', dedicatedMb: 0 }], [])).toEqual([
      'Mystery GPU — no supported hardware encoder',
    ]);
  });

  it('no GPUs and no rows gives no lines', () => {
    expect(hardwareLines([], [])).toEqual([]);
  });
});
