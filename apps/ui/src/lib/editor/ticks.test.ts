import { describe, expect, it } from 'vitest';
import {
  MAX_PROJECT_TICKS,
  TICKS_PER_SECOND,
  clampTick,
  clipEnd,
  formatDuration,
  formatTimecode,
  frameDurationTicks,
  msToTick,
  parseTimecode,
  secToTick,
  snapToFrame,
  srcLen,
  tickToMs,
  tickToSec,
} from './ticks';
import type { Clip, Rate } from './types';
import { defaultClipAudio, defaultTransform } from './types';

const R24: Rate = { num: 24, den: 1 };
const R25: Rate = { num: 25, den: 1 };
const R30: Rate = { num: 30, den: 1 };
const R60: Rate = { num: 60, den: 1 };
const NTSC: Rate = { num: 30000, den: 1001 };

/** Exact duration of one frame at 30 fps. */
const F30 = 8_467_200_000;

function clip(over: Partial<Clip> = {}): Clip {
  return {
    id: 'c1',
    track_id: 'V0',
    kind: 'video',
    media_id: 'm1',
    name: 'clip',
    start: 0,
    dur: TICKS_PER_SECOND,
    src_in: 0,
    speed: 1,
    reverse: false,
    enabled: true,
    color: null,
    transform: defaultTransform(),
    opacity: 1,
    audio: defaultClipAudio(),
    link_id: null,
    group_id: null,
    transition_in: null,
    text: null,
    solid: null,
    ...over,
  };
}

describe('constants', () => {
  it('uses 254,016,000,000 ticks per second', () => {
    expect(TICKS_PER_SECOND).toBe(254_016_000_000);
  });
  it('caps a project at four hours of ticks, inside the safe integer range', () => {
    expect(MAX_PROJECT_TICKS).toBe(14_400 * TICKS_PER_SECOND);
    expect(MAX_PROJECT_TICKS).toBeLessThanOrEqual(Number.MAX_SAFE_INTEGER);
  });
});

describe('seconds and milliseconds conversion', () => {
  it('converts whole and fractional seconds exactly', () => {
    expect(secToTick(1)).toBe(TICKS_PER_SECOND);
    expect(secToTick(0.5)).toBe(127_008_000_000);
    expect(secToTick(12.5)).toBe(3_175_200_000_000);
  });
  it('returns 0 for NaN and infinities', () => {
    expect(secToTick(Number.NaN)).toBe(0);
    expect(secToTick(Number.POSITIVE_INFINITY)).toBe(0);
    expect(secToTick(Number.NEGATIVE_INFINITY)).toBe(0);
  });
  it('keeps the sign of negative seconds', () => {
    expect(secToTick(-1)).toBe(-TICKS_PER_SECOND);
  });
  it('round trips tick -> seconds -> tick', () => {
    const t = 3 * TICKS_PER_SECOND + 12_345;
    expect(secToTick(tickToSec(t))).toBe(t);
  });
  it('tickToSec returns 1 for one second and 0 for non-finite input', () => {
    expect(tickToSec(TICKS_PER_SECOND)).toBe(1);
    expect(tickToSec(Number.NaN)).toBe(0);
    expect(tickToSec(Number.POSITIVE_INFINITY)).toBe(0);
  });
  it('converts milliseconds to ticks', () => {
    expect(msToTick(1000)).toBe(TICKS_PER_SECOND);
    expect(msToTick(1)).toBe(254_016_000);
  });
  it('returns 0 from msToTick for non-finite input', () => {
    expect(msToTick(Number.NaN)).toBe(0);
    expect(msToTick(Number.POSITIVE_INFINITY)).toBe(0);
  });
  it('converts ticks to whole milliseconds, rounding to nearest', () => {
    expect(tickToMs(TICKS_PER_SECOND)).toBe(1000);
    expect(tickToMs(254_016_000)).toBe(1);
    expect(tickToMs(101_606_400)).toBe(0); // 0.4 ms
    expect(tickToMs(152_409_600)).toBe(1); // 0.6 ms
  });
  it('tickToMs returns 0 for non-finite input', () => {
    expect(tickToMs(Number.NaN)).toBe(0);
  });
});

describe('frameDurationTicks', () => {
  it.each([
    ['24 fps', R24, 10_584_000_000],
    ['25 fps', R25, 10_160_640_000],
    ['30 fps', R30, 8_467_200_000],
    ['60 fps', R60, 4_233_600_000],
    ['29.97 fps (30000:1001)', NTSC, 8_475_667_200],
  ])('gives the exact frame length at %s', (_label, rate, expected) => {
    expect(frameDurationTicks(rate)).toBe(expected);
  });
  it('returns 0 for a zero, negative or fractional rate', () => {
    expect(frameDurationTicks({ num: 0, den: 1 })).toBe(0);
    expect(frameDurationTicks({ num: -30, den: 1 })).toBe(0);
    expect(frameDurationTicks({ num: 30.5, den: 1 })).toBe(0);
  });
});

describe('snapToFrame', () => {
  it('maps zero, negative and non-finite input to 0', () => {
    expect(snapToFrame(0, R30)).toBe(0);
    expect(snapToFrame(-5 * F30, R30)).toBe(0);
    expect(snapToFrame(Number.NaN, R30)).toBe(0);
    expect(snapToFrame(Number.POSITIVE_INFINITY, R30)).toBe(0);
  });
  it('leaves exact frame multiples unchanged', () => {
    expect(snapToFrame(5 * F30, R30)).toBe(5 * F30);
    expect(snapToFrame(3 * frameDurationTicks(R25), R25)).toBe(3 * frameDurationTicks(R25));
  });
  it('snaps down when the time is less than half a frame past a boundary', () => {
    expect(snapToFrame(2 * F30 + 100, R30)).toBe(2 * F30);
  });
  it('snaps up when the time is more than half a frame past a boundary', () => {
    expect(snapToFrame(2 * F30 + Math.round(F30 * 0.7), R30)).toBe(3 * F30);
  });
  it('snaps 1.6 frames to frame 2', () => {
    expect(snapToFrame(Math.round(F30 * 1.6), R30)).toBe(2 * F30);
  });
  it('falls back to 30 fps for an invalid rate', () => {
    expect(snapToFrame(F30, { num: 0, den: 0 })).toBe(F30);
  });
  it('snaps at 29.97 fps to the NTSC frame grid', () => {
    const f = frameDurationTicks(NTSC);
    expect(snapToFrame(f + 1000, NTSC)).toBe(f);
  });
});

describe('formatTimecode', () => {
  it('shows 1.5 s as frame 15 of second 1 at 30 fps', () => {
    expect(formatTimecode(secToTick(1.5), R30)).toBe('00:00:01:15');
  });
  it('shows zero as all zeros', () => {
    expect(formatTimecode(0, R30)).toBe('00:00:00:00');
  });
  it('counts frames at 24 and 25 fps', () => {
    expect(formatTimecode(25 * frameDurationTicks(R24), R24)).toBe('00:00:01:01');
    expect(formatTimecode(25 * frameDurationTicks(R25), R25)).toBe('00:00:01:00');
    expect(formatTimecode(24 * frameDurationTicks(R24), R24)).toBe('00:00:01:00');
  });
  it('shows the last frame before a second boundary', () => {
    expect(formatTimecode(29 * F30, R30)).toBe('00:00:00:29');
  });
  it('rolls over minutes and hours', () => {
    expect(formatTimecode(107_999 * F30, R30)).toBe('00:59:59:29');
    expect(formatTimecode(108_000 * F30, R30)).toBe('01:00:00:00');
  });
  it('uses 30 as the nominal frame count at 30000:1001', () => {
    const f = frameDurationTicks(NTSC);
    expect(formatTimecode(29 * f, NTSC)).toBe('00:00:00:29');
    expect(formatTimecode(30 * f, NTSC)).toBe('00:00:01:00');
  });
  it('clamps negative times to zero', () => {
    expect(formatTimecode(-5 * TICKS_PER_SECOND, R30)).toBe('00:00:00:00');
  });
  it('shows NaN and infinity as zero', () => {
    expect(formatTimecode(Number.NaN, R30)).toBe('00:00:00:00');
    expect(formatTimecode(Number.POSITIVE_INFINITY, R30)).toBe('00:00:00:00');
  });
});

describe('formatDuration', () => {
  it('formats short durations as M:SS', () => {
    expect(formatDuration(secToTick(5))).toBe('0:05');
    expect(formatDuration(secToTick(65))).toBe('1:05');
  });
  it('switches to H:MM:SS from one hour', () => {
    expect(formatDuration(secToTick(3600))).toBe('1:00:00');
    expect(formatDuration(secToTick(3723))).toBe('1:02:03');
  });
  it('floors fractions of a second', () => {
    expect(formatDuration(secToTick(59.9))).toBe('0:59');
  });
  it('shows zero, negative and non-finite values as 0:00', () => {
    expect(formatDuration(0)).toBe('0:00');
    expect(formatDuration(-TICKS_PER_SECOND)).toBe('0:00');
    expect(formatDuration(Number.NaN)).toBe('0:00');
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBe('0:00');
  });
});

describe('parseTimecode accepted forms', () => {
  it('parses HH:MM:SS:FF', () => {
    // 01:02:03:04 at 30 fps = (1 h + 2 min + 3 s) = 3723 s, plus 4 frames = 111694 frames.
    expect(parseTimecode('01:02:03:04', R30)).toBe(111_694 * F30);
  });
  it('parses MM:SS:FF as frames', () => {
    expect(parseTimecode('02:03:04', R30)).toBe(3694 * F30);
  });
  it('parses SS:FF as frames', () => {
    expect(parseTimecode('3:04', R30)).toBe(94 * F30);
  });
  it('parses plain decimal seconds', () => {
    expect(parseTimecode('12.5', R30)).toBe(secToTick(12.5));
  });
  it('parses M:SS.s decimal seconds', () => {
    expect(parseTimecode('1:23.5', R30)).toBe(secToTick(83.5));
  });
  it('parses H:MM:SS.s decimal seconds', () => {
    expect(parseTimecode('1:02:03.25', R30)).toBe(secToTick(3723.25));
  });
  it('parses a plain whole number as seconds', () => {
    expect(parseTimecode('12', R30)).toBe(secToTick(12));
  });
  it('ignores surrounding whitespace', () => {
    expect(parseTimecode('  12  ', R30)).toBe(secToTick(12));
  });
  it('round trips through formatTimecode', () => {
    const t = parseTimecode('01:02:03:04', R30);
    expect(t).not.toBeNull();
    expect(formatTimecode(t ?? -1, R30)).toBe('01:02:03:04');
  });
  it('accepts frame 24 at 25 fps but rejects it at 24 fps', () => {
    expect(parseTimecode('00:00:00:24', R25)).not.toBeNull();
    expect(parseTimecode('00:00:00:24', R24)).toBeNull();
  });
});

describe('parseTimecode rejected input', () => {
  it.each([
    ['empty string', ''],
    ['whitespace only', '   '],
    ['garbage text', 'abc'],
    ['negative number', '-5'],
    ['frame field at or above the rate', '1:99'],
    ['frame 30 at 30 fps', '00:00:00:30'],
    ['minutes of 60 in frame form', '00:60:00:00'],
    ['seconds of 60 in decimal form', '1:60.5'],
    ['more than four fields', '1:2:3:4:5'],
    ['two dots', '1.2.3'],
    ['scientific notation', '1e3'],
    ['NaN text', 'NaN'],
    ['trailing dot', '12.'],
    ['negative field', '1:-5'],
  ])('rejects %s', (_label, text) => {
    expect(parseTimecode(text, R30)).toBeNull();
  });
});

describe('clipEnd and srcLen', () => {
  it('ends a clip at start plus duration', () => {
    expect(clipEnd(clip({ start: 100, dur: 50 }))).toBe(150);
  });
  it('maps a non-finite start to 0 end', () => {
    expect(clipEnd(clip({ start: Number.NaN, dur: 50 }))).toBe(0);
  });
  it('computes source length as duration times speed', () => {
    expect(srcLen(clip({ dur: 1000, speed: 2 }))).toBe(2000);
  });
  it('gives zero source length for a freeze frame', () => {
    expect(srcLen(clip({ dur: 1000, speed: 0 }))).toBe(0);
  });
  it('rounds the source length to the nearest tick', () => {
    expect(srcLen(clip({ dur: 3, speed: 0.5 }))).toBe(2);
  });
  it('gives zero source length for NaN speed or negative duration', () => {
    expect(srcLen(clip({ dur: 1000, speed: Number.NaN }))).toBe(0);
    expect(srcLen(clip({ dur: -1000, speed: 1 }))).toBe(0);
  });
});

describe('clampTick', () => {
  it('keeps a value inside the range', () => {
    expect(clampTick(5, 0, 10)).toBe(5);
  });
  it('clamps below and above the range', () => {
    expect(clampTick(-3, 0, 10)).toBe(0);
    expect(clampTick(20, 0, 10)).toBe(10);
  });
  it('rounds to an integer', () => {
    expect(clampTick(5.4, 0, 10)).toBe(5);
  });
  it('returns the lower bound when the range is inverted', () => {
    expect(clampTick(5, 10, 0)).toBe(10);
  });
  it('returns the lower bound for NaN and infinities', () => {
    expect(clampTick(Number.NaN, 2, 10)).toBe(2);
    expect(clampTick(Number.POSITIVE_INFINITY, 2, 10)).toBe(2);
    expect(clampTick(Number.NEGATIVE_INFINITY, 2, 10)).toBe(2);
  });
});
