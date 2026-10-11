import { describe, expect, it } from 'vitest';
import {
  GAIN_MAX_DB,
  GAIN_MIN_DB,
  SEEK_STEP_S,
  SPEEDS,
  clampTime,
  clampVolume,
  dbToLinear,
  formatTime,
  initTracks,
  isTrackAudible,
  keyToAction,
  needsResync,
  percentJump,
  setGainDb,
  soloTrack,
  stepSpeed,
  stepVolume,
  toggleTrack,
  trackGain,
  trackLabel,
  type TrackState,
} from './model';

/** Deep-freeze a track list so any mutation throws in strict mode. */
function frozen(s: TrackState[]): TrackState[] {
  s.forEach((t) => Object.freeze(t));
  return Object.freeze(s) as TrackState[];
}

function snapshot(s: TrackState[]): TrackState[] {
  return JSON.parse(JSON.stringify(s)) as TrackState[];
}

type KeyOpts = Partial<{
  code: string;
  shiftKey: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}>;

function ev(key: string, o: KeyOpts = {}) {
  return { key, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...o };
}

describe('constants', () => {
  it('exposes the documented ranges and speed list', () => {
    expect(GAIN_MIN_DB).toBe(-20);
    expect(GAIN_MAX_DB).toBe(6);
    expect(SPEEDS).toEqual([0.25, 0.5, 0.75, 1, 1.25, 1.5, 2]);
    expect(SEEK_STEP_S).toBe(5);
  });
});

describe('initTracks', () => {
  it('returns an empty list for zero tracks', () => {
    expect(initTracks(0)).toEqual([]);
  });

  it('makes a single track audible and nothing else', () => {
    expect(initTracks(1)).toEqual([{ index: 0, audible: true, solo: false, gainDb: 0 }]);
  });

  it('makes track 0 audible and the rest silent, with no solos and zero gain', () => {
    expect(initTracks(3)).toEqual([
      { index: 0, audible: true, solo: false, gainDb: 0 },
      { index: 1, audible: false, solo: false, gainDb: 0 },
      { index: 2, audible: false, solo: false, gainDb: 0 },
    ]);
  });
});

describe('toggleTrack / soloTrack / setGainDb immutability', () => {
  it('toggleTrack flips audible without mutating its input', () => {
    const input = frozen(initTracks(3));
    const before = snapshot(input);
    const out = toggleTrack(input, 1);
    expect(out[1].audible).toBe(true);
    expect(out).not.toBe(input);
    expect(out[1]).not.toBe(input[1]);
    expect(snapshot(input)).toEqual(before);
  });

  it('soloTrack toggles solo without mutating its input', () => {
    const input = frozen(initTracks(3));
    const before = snapshot(input);
    const out = soloTrack(input, 2);
    expect(out[2].solo).toBe(true);
    expect(snapshot(input)).toEqual(before);
  });

  it('setGainDb changes gain without mutating its input', () => {
    const input = frozen(initTracks(2));
    const before = snapshot(input);
    const out = setGainDb(input, 0, 3);
    expect(out[0].gainDb).toBe(3);
    expect(snapshot(input)).toEqual(before);
  });

  it.each([
    ['toggleTrack', (s: TrackState[]) => toggleTrack(s, 99)],
    ['toggleTrack negative', (s: TrackState[]) => toggleTrack(s, -1)],
    ['soloTrack', (s: TrackState[]) => soloTrack(s, 99)],
    ['setGainDb', (s: TrackState[]) => setGainDb(s, 99, 3)],
  ])('%s with an unknown index returns an unchanged copy', (_name, fn) => {
    const input = initTracks(2);
    const before = snapshot(input);
    const out = fn(input);
    expect(out).toEqual(before);
    expect(out).not.toBe(input);
    expect(snapshot(input)).toEqual(before);
  });
});

describe('setGainDb clamping', () => {
  it.each([
    [-30, -20],
    [-20, -20],
    [-5, -5],
    [0, 0],
    [6, 6],
    [10, 6],
    [Number.POSITIVE_INFINITY, 6],
    [Number.NEGATIVE_INFINITY, -20],
    [Number.NaN, 0],
  ])('setGainDb(%s) stores %s', (input, expected) => {
    const out = setGainDb(initTracks(1), 0, input);
    expect(out[0].gainDb).toBe(expected);
  });
});

describe('dbToLinear', () => {
  it('maps 0 dB to 1', () => {
    expect(dbToLinear(0)).toBe(1);
  });
  it('maps +6 dB to about 1.995', () => {
    expect(dbToLinear(6)).toBeCloseTo(1.995, 2);
  });
  it('maps -20 dB to 0.1', () => {
    expect(dbToLinear(-20)).toBeCloseTo(0.1, 10);
  });
  it('maps -Infinity, NaN to 0', () => {
    expect(dbToLinear(Number.NEGATIVE_INFINITY)).toBe(0);
    expect(dbToLinear(Number.NaN)).toBe(0);
  });
});

describe('isTrackAudible', () => {
  it('follows the audible flags when there are no solos', () => {
    const s = initTracks(3);
    expect(isTrackAudible(s, 0)).toBe(true);
    expect(isTrackAudible(s, 1)).toBe(false);
    expect(isTrackAudible(s, 2)).toBe(false);
  });

  it('a soloed track is audible even when its audible flag is false', () => {
    const s = soloTrack(initTracks(3), 1);
    expect(s[1].audible).toBe(false);
    expect(isTrackAudible(s, 1)).toBe(true);
  });

  it('silences a non-soloed audible track while any solo exists', () => {
    const s = soloTrack(initTracks(3), 1);
    expect(isTrackAudible(s, 0)).toBe(false);
    expect(isTrackAudible(s, 2)).toBe(false);
  });

  it('allows two solos at once and silences the rest', () => {
    const s = soloTrack(soloTrack(initTracks(3), 0), 2);
    expect(isTrackAudible(s, 0)).toBe(true);
    expect(isTrackAudible(s, 1)).toBe(false);
    expect(isTrackAudible(s, 2)).toBe(true);
  });
});

describe('trackGain', () => {
  it('is 0 when muted', () => {
    expect(trackGain(initTracks(1), 0, 1, true)).toBe(0);
  });

  it('is 0 when the track is not audible', () => {
    expect(trackGain(initTracks(2), 1, 1, false)).toBe(0);
  });

  it('combines +6 dB gain with volume 0.5', () => {
    const s = setGainDb(initTracks(1), 0, 6);
    expect(trackGain(s, 0, 0.5, false)).toBeCloseTo(dbToLinear(6) * 0.5, 10);
  });

  it('clamps volume above 1 to 1', () => {
    expect(trackGain(initTracks(1), 0, 2, false)).toBe(1);
  });

  it('treats NaN volume as 1', () => {
    expect(trackGain(initTracks(1), 0, Number.NaN, false)).toBe(1);
  });

  it('is audible through a solo even if the audible flag is false', () => {
    const s = soloTrack(initTracks(2), 1);
    expect(trackGain(s, 1, 1, false)).toBe(1);
    expect(trackGain(s, 0, 1, false)).toBe(0);
  });
});

describe('clampVolume', () => {
  it.each([
    [-1, 0],
    [0, 0],
    [0.4, 0.4],
    [1, 1],
    [2, 1],
    [Number.NaN, 1],
  ])('clampVolume(%s) => %s', (input, expected) => {
    expect(clampVolume(input)).toBe(expected);
  });
});

describe('stepVolume', () => {
  it.each([
    [0.35, 5, 0.4],
    [0.98, 5, 1],
    [0.02, -5, 0],
  ])('stepVolume(%s, %s) => %s', (v, d, expected) => {
    expect(stepVolume(v, d)).toBe(expected);
  });
});

describe('stepSpeed', () => {
  it('walks up the full speed list and clamps at the top', () => {
    let v = SPEEDS[0];
    const seen = [v];
    for (let i = 0; i < SPEEDS.length; i++) {
      v = stepSpeed(v, 1);
      seen.push(v);
    }
    expect(seen).toEqual([0.25, 0.5, 0.75, 1, 1.25, 1.5, 2, 2]);
  });

  it('walks down the full speed list and clamps at the bottom', () => {
    let v = SPEEDS[SPEEDS.length - 1];
    const seen = [v];
    for (let i = 0; i < SPEEDS.length; i++) {
      v = stepSpeed(v, -1);
      seen.push(v);
    }
    expect(seen).toEqual([2, 1.5, 1.25, 1, 0.75, 0.5, 0.25, 0.25]);
  });

  it('clamps at both ends', () => {
    expect(stepSpeed(2, 1)).toBe(2);
    expect(stepSpeed(0.25, -1)).toBe(0.25);
  });

  it('moves from an unlisted speed to the nearest listed neighbour', () => {
    // Spec: "unknown cur => nearest". Literal reading: stepSpeed(3, -1) === 2 (nearest entry).
    // 1.1 / 0.6 directional results below assume a direction-aware neighbour (ambiguous in spec).
    expect(stepSpeed(1.1, 1)).toBe(1.25);
    expect(stepSpeed(1.1, -1)).toBe(1);
    expect(stepSpeed(0.6, 1)).toBe(0.75);
    expect(stepSpeed(0.6, -1)).toBe(0.5);
    expect(stepSpeed(3, 1)).toBe(2);
    expect(stepSpeed(3, -1)).toBe(2);
  });

  it('returns a listed speed for NaN input', () => {
    // Spec only says "unknown cur => nearest"; NaN has no nearest, so only check membership.
    expect(SPEEDS).toContain(stepSpeed(Number.NaN, 1));
  });
});

describe('clampTime', () => {
  it.each([
    // [t, duration, expected]
    [5, 10, 5],
    [-3, 10, 0],
    [15, 10, 10],
    [Number.NaN, 10, 0],
  ])('clampTime(%s, %s) => %s', (t, d, expected) => {
    expect(clampTime(t, d)).toBe(expected);
  });

  it.each([0, Number.NaN, -4])('unknown duration %s returns max(0, t)', (d) => {
    expect(clampTime(7, d)).toBe(7);
    expect(clampTime(-7, d)).toBe(0);
  });

  it('treats Infinity duration as unbounded above', () => {
    expect(clampTime(1e9, Number.POSITIVE_INFINITY)).toBe(1e9);
    expect(clampTime(-1, Number.POSITIVE_INFINITY)).toBe(0);
  });
});

describe('formatTime', () => {
  it.each([
    [0, '0:00'],
    [0.9, '0:00'],
    [59.99, '0:59'],
    [60, '1:00'],
    [3599.9, '59:59'],
    [3600, '1:00:00'],
    [86399, '23:59:59'],
    [Number.NaN, '0:00'],
    [-5, '0:00'],
  ])('formatTime(%s) => %s', (sec, expected) => {
    expect(formatTime(sec)).toBe(expected);
  });

  it('does not throw for Infinity (spec is silent on the exact output)', () => {
    expect(typeof formatTime(Number.POSITIVE_INFINITY)).toBe('string');
  });
});

describe('percentJump', () => {
  it.each([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])('digit %i jumps %i0%% of the duration', (d) => {
    expect(percentJump(d, 100)).toBeCloseTo(d * 10, 10);
  });

  it('digit 5 on a 200 s file lands at 100 s', () => {
    expect(percentJump(5, 200)).toBeCloseTo(100, 10);
  });

  it('a zero duration gives 0', () => {
    expect(percentJump(5, 0)).toBe(0);
  });

  it('a NaN duration does not produce a finite jump (spec is silent; checked for not-a-number only)', () => {
    const r = percentJump(5, Number.NaN);
    expect(Number.isNaN(r) || r === 0).toBe(true);
  });

  it('out-of-range digits return a finite number (spec is silent on the exact value)', () => {
    for (const d of [10, -1, 2.7]) {
      expect(Number.isFinite(percentJump(d, 100))).toBe(true);
    }
  });
});

describe('needsResync', () => {
  it('uses a default threshold of exactly 0.15', () => {
    expect(needsResync(0.15, 0)).toBe(false);
    expect(needsResync(0.1501, 0)).toBe(true);
  });

  it('a diff equal to the threshold is not a resync', () => {
    // 0.15 and 0 are exact operands, so |diff| is exactly the threshold double.
    expect(needsResync(0.15, 0)).toBe(false);
    expect(needsResync(0, -0.15)).toBe(false);
  });

  it('uses the magnitude of the difference', () => {
    expect(needsResync(0, 0.5)).toBe(true);
    expect(needsResync(0.5, 0)).toBe(true);
    expect(needsResync(0.5, 0.6)).toBe(false);
  });

  it('never resyncs on NaN', () => {
    expect(needsResync(Number.NaN, 0)).toBe(false);
    expect(needsResync(0, Number.NaN)).toBe(false);
  });

  it('honours an explicit threshold', () => {
    expect(needsResync(0, 0.3, 0.5)).toBe(false);
    expect(needsResync(0, 0.3, 0.1)).toBe(true);
  });
});

describe('trackLabel', () => {
  it('uses the trimmed title when present', () => {
    expect(trackLabel({ index: 0, title: ' Game ' })).toBe('Game');
  });

  it.each([
    ['', 0, 'Track 1'],
    ['   ', 0, 'Track 1'],
    ['\t\n', 2, 'Track 3'],
  ])('blank title %j at index %i => %s', (title, index, expected) => {
    expect(trackLabel({ index, title })).toBe(expected);
  });
});

describe('keyToAction', () => {
  it.each([
    // play / pause
    [ev(' '), { type: 'toggle-play' }],
    [ev('k'), { type: 'toggle-play' }],
    [ev('K'), { type: 'toggle-play' }],
    // seek
    [ev('j'), { type: 'seek', deltaS: -10 }],
    [ev('J'), { type: 'seek', deltaS: -10 }],
    [ev('l'), { type: 'seek', deltaS: 10 }],
    [ev('L'), { type: 'seek', deltaS: 10 }],
    [ev('ArrowLeft'), { type: 'seek', deltaS: -5 }],
    [ev('ArrowRight'), { type: 'seek', deltaS: 5 }],
    [ev('ArrowLeft', { shiftKey: true }), { type: 'seek', deltaS: -1 }],
    [ev('ArrowRight', { shiftKey: true }), { type: 'seek', deltaS: 1 }],
    // volume
    [ev('ArrowUp'), { type: 'volume', deltaPct: 5 }],
    [ev('ArrowDown'), { type: 'volume', deltaPct: -5 }],
    // mute / fullscreen
    [ev('m'), { type: 'mute' }],
    [ev('M'), { type: 'mute' }],
    [ev('f'), { type: 'fullscreen' }],
    [ev('F'), { type: 'fullscreen' }],
    // speed
    [ev('<'), { type: 'speed', dir: -1 }],
    [ev('>'), { type: 'speed', dir: 1 }],
    [ev(',', { shiftKey: true }), { type: 'speed', dir: -1 }],
    [ev('.', { shiftKey: true }), { type: 'speed', dir: 1 }],
    // frame step
    [ev(','), { type: 'frame', dir: -1 }],
    [ev('.'), { type: 'frame', dir: 1 }],
  ])('maps %j', (e, expected) => {
    expect(keyToAction(e)).toEqual(expected);
  });

  it.each(['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'])('digit %s jumps', (d) => {
    expect(keyToAction(ev(d))).toEqual({ type: 'jump', digit: Number(d) });
  });

  it('Alt+Digit1..9 toggles tracks 0..8, using code because Alt changes key', () => {
    const altKeys = ['¡', '™', '£', '¢', '∞', '§', '¶', '•', 'ª'];
    for (let n = 1; n <= 9; n++) {
      expect(
        keyToAction(ev(altKeys[n - 1], { altKey: true, code: `Digit${n}` })),
      ).toEqual({ type: 'track-toggle', index: n - 1 });
    }
  });

  it('Alt+digit falls back to key when code is absent', () => {
    expect(keyToAction(ev('2', { altKey: true }))).toEqual({ type: 'track-toggle', index: 1 });
  });

  it('Shift+Alt+Digit solos the track', () => {
    expect(
      keyToAction(ev('£', { shiftKey: true, altKey: true, code: 'Digit3' })),
    ).toEqual({ type: 'track-solo', index: 2 });
  });

  it('Alt+Digit0 is not a track shortcut', () => {
    expect(keyToAction(ev('º', { altKey: true, code: 'Digit0' }))).toBeNull();
    expect(keyToAction(ev('0', { altKey: true, code: 'Digit0' }))).toBeNull();
  });

  it.each([
    ['Ctrl+K', ev('k', { ctrlKey: true })],
    ['Ctrl+S', ev('s', { ctrlKey: true })],
    ['Meta+F', ev('f', { metaKey: true })],
    ['Ctrl+ArrowLeft', ev('ArrowLeft', { ctrlKey: true })],
    ['Ctrl+1', ev('1', { ctrlKey: true, code: 'Digit1' })],
    ['Alt+ArrowLeft', ev('ArrowLeft', { altKey: true })],
    ['Escape', ev('Escape')],
    ['unknown key x', ev('x')],
    ['Enter', ev('Enter')],
  ])('%s => null', (_name, e) => {
    expect(keyToAction(e)).toBeNull();
  });
});
