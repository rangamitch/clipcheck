import { describe, expect, it } from 'vitest';
import { TICKS_PER_SECOND } from '../ticks';
import {
  activeMedia,
  audioGainAt,
  dipAmount,
  editPoints,
  gainRamp,
  layerOpacityAt,
  nextEdit,
  panGains,
  prevEdit,
  segmentAt,
  sourceTimeFor,
  transitionProgress,
  upcoming,
} from './scheduler';
import type { AudioSource, PlanSegment, RenderPlan, Tick, Transition, VideoLayer } from '../types';
import { defaultCanvas, defaultClipAudio, defaultTransform } from '../types';

const S = TICKS_PER_SECOND;

function vlayer(clipId: string, mediaId: string | null, over: Partial<VideoLayer> = {}): VideoLayer {
  return {
    clip_id: clipId,
    kind: 'video',
    media_id: mediaId,
    z: 0,
    src_time: 0,
    speed: 1,
    reverse: false,
    transform: defaultTransform(),
    opacity: 1,
    transition: null,
    clip_start: 0,
    text: null,
    solid: null,
    ...over,
  };
}

function alayer(clipId: string, mediaId: string, over: Partial<AudioSource> = {}): AudioSource {
  return {
    clip_id: clipId,
    track_id: 'A0',
    track_index: 0,
    media_id: mediaId,
    source_track: 0,
    src_time: 0,
    speed: 1,
    reverse: false,
    clip_start: 0,
    clip_dur: 2 * S,
    audio: defaultClipAudio(),
    track_db: 0,
    ...over,
  };
}

function seg(start: Tick, end: Tick, video: VideoLayer[] = [], audio: AudioSource[] = []): PlanSegment {
  return { start, end, video, audio };
}

function plan(segments: PlanSegment[], start: Tick, end: Tick): RenderPlan {
  return { canvas: defaultCanvas(), start, end, segments };
}

/**
 * Main fixture (ticks, S = one second):
 *   S1 [0, 1s):  video c1 (m1)
 *   S2 [1s, 3s): video c1 (m1, src 1s), c2 (m2, src 5s, z 1); audio a1 (m3)
 *   S3 [3s, 4s): empty
 */
function mainPlan(): RenderPlan {
  return plan(
    [
      seg(0, S, [vlayer('c1', 'm1')]),
      seg(S, 3 * S, [vlayer('c1', 'm1', { src_time: S }), vlayer('c2', 'm2', { z: 1, src_time: 5 * S })], [
        alayer('a1', 'm3'),
      ]),
      seg(3 * S, 4 * S),
    ],
    0,
    4 * S,
  );
}

/** Gain fixture: clip from 0 for 2 s on an audio track at 0 dB. */
function gainSrc(over: Partial<AudioSource> = {}, audioOver: Partial<AudioSource['audio']> = {}): AudioSource {
  return alayer('a', 'm', { ...over, audio: { ...defaultClipAudio(), ...audioOver } });
}

describe('segmentAt', () => {
  const p = mainPlan();
  it('returns the first segment at tick 0', () => {
    expect(segmentAt(p, 0)).toBe(p.segments[0]);
  });
  it('returns the segment before a boundary just below it', () => {
    expect(segmentAt(p, S - 1)).toBe(p.segments[0]);
  });
  it('returns the segment that starts on a boundary', () => {
    expect(segmentAt(p, S)).toBe(p.segments[1]);
    expect(segmentAt(p, 3 * S)).toBe(p.segments[2]);
  });
  it('returns an empty segment when it covers the time', () => {
    const s = segmentAt(p, 3 * S + 5);
    expect(s).toBe(p.segments[2]);
    expect(s?.video).toEqual([]);
  });
  it('returns null at and after the plan end', () => {
    expect(segmentAt(p, 4 * S - 1)).toBe(p.segments[2]);
    expect(segmentAt(p, 4 * S)).toBeNull();
    expect(segmentAt(p, 5 * S)).toBeNull();
  });
  it('returns null before the first segment and for NaN', () => {
    expect(segmentAt(p, -1)).toBeNull();
    expect(segmentAt(p, Number.NaN)).toBeNull();
  });
  it('returns null for an empty plan', () => {
    expect(segmentAt(plan([], 0, 0), 0)).toBeNull();
  });
  it('finds the right segment in a long plan (binary search)', () => {
    const segs: PlanSegment[] = [];
    for (let i = 0; i < 50; i++) segs.push(seg(i * S, (i + 1) * S));
    const long = plan(segs, 0, 50 * S);
    for (const i of [0, 1, 17, 33, 48, 49]) {
      expect(segmentAt(long, i * S + 1)).toBe(segs[i]);
      expect(segmentAt(long, i * S)).toBe(segs[i]);
    }
  });
});

describe('editPoints', () => {
  it('lists every segment boundary once, sorted', () => {
    expect(editPoints(mainPlan())).toEqual([0, S, 3 * S, 4 * S]);
  });
  it('returns an empty list for an empty plan', () => {
    expect(editPoints(plan([], 0, 0))).toEqual([]);
  });
});

describe('prevEdit and nextEdit', () => {
  const p = mainPlan();
  it('finds the previous edit strictly before a time', () => {
    expect(prevEdit(p, 2 * S)).toBe(S);
    expect(prevEdit(p, 3.5 * S)).toBe(3 * S);
  });
  it('does not return the time itself when it is an edit', () => {
    expect(prevEdit(p, S)).toBe(0);
  });
  it('returns the last edit when the time is past the end', () => {
    expect(prevEdit(p, 10 * S)).toBe(4 * S);
  });
  it('falls back to the plan start when there is no earlier edit', () => {
    const late = plan([seg(2 * S, 3 * S)], 2 * S, 3 * S);
    expect(prevEdit(late, 2 * S)).toBe(2 * S);
  });
  it('falls back to the plan start for NaN', () => {
    expect(prevEdit(p, Number.NaN)).toBe(0);
  });
  it('finds the next edit strictly after a time', () => {
    expect(prevEdit(p, 0)).toBe(0);
    expect(nextEdit(p, 0)).toBe(S);
    expect(nextEdit(p, S - 1)).toBe(S);
  });
  it('does not return the time itself when it is an edit', () => {
    expect(nextEdit(p, S)).toBe(3 * S);
    expect(nextEdit(p, 3 * S)).toBe(4 * S);
  });
  it('falls back to the plan end when there is no later edit', () => {
    expect(nextEdit(p, 4 * S)).toBe(4 * S);
    expect(nextEdit(p, 10 * S)).toBe(4 * S);
  });
  it('falls back to the plan end for NaN', () => {
    expect(nextEdit(p, Number.NaN)).toBe(4 * S);
  });
});

describe('sourceTimeFor', () => {
  const base = (over: Partial<VideoLayer>): VideoLayer => vlayer('c', 'm', { src_time: 1000, ...over });

  it('adds the elapsed timeline time at speed 1', () => {
    expect(sourceTimeFor(base({}), 0, 500)).toBe(1500);
  });
  it('scales the elapsed time by speed 2', () => {
    expect(sourceTimeFor(base({ speed: 2 }), 0, 500)).toBe(2000);
  });
  it('scales the elapsed time by speed 0.5', () => {
    expect(sourceTimeFor(base({ speed: 0.5 }), 0, 500)).toBe(1250);
  });
  it('measures elapsed time from the segment start', () => {
    expect(sourceTimeFor(base({}), 200, 700)).toBe(1500);
  });
  it('subtracts the elapsed time when reversed', () => {
    expect(sourceTimeFor(base({ reverse: true }), 0, 500)).toBe(500);
    expect(sourceTimeFor(base({ reverse: true, speed: 2 }), 0, 500)).toBe(0);
  });
  it('clamps a reversed source time at zero', () => {
    expect(sourceTimeFor(base({ reverse: true, src_time: 100 }), 0, 500)).toBe(0);
  });
  it('holds the source time for a freeze frame (speed 0)', () => {
    expect(sourceTimeFor(base({ speed: 0 }), 0, 500)).toBe(1000);
    expect(sourceTimeFor(base({ speed: 0 }), 0, 99_999)).toBe(1000);
  });
  it('holds the source time for negative, NaN or infinite speed', () => {
    expect(sourceTimeFor(base({ speed: -1 }), 0, 500)).toBe(1000);
    expect(sourceTimeFor(base({ speed: Number.NaN }), 0, 500)).toBe(1000);
    expect(sourceTimeFor(base({ speed: Number.POSITIVE_INFINITY }), 0, 500)).toBe(1000);
  });
  it('works the same for an audio source', () => {
    expect(sourceTimeFor(alayer('a', 'm', { src_time: 0, speed: 2 }), 0, 100)).toBe(200);
  });
  it('returns the base time for a non-finite timeline time', () => {
    expect(sourceTimeFor(base({}), 0, Number.NaN)).toBe(1000);
  });
});

describe('upcoming', () => {
  const p = mainPlan();
  it('lists media that starts within the horizon and is not active now', () => {
    expect(upcoming(p, 0, 1.5 * S)).toEqual([
      { mediaId: 'm2', startsAt: S, srcTime: 5 * S },
      { mediaId: 'm3', startsAt: S, srcTime: 0 },
    ]);
  });
  it('excludes a segment that starts after the horizon', () => {
    expect(upcoming(p, 0, 0.5 * S)).toEqual([]);
  });
  it('includes a segment that starts exactly at the horizon limit', () => {
    expect(upcoming(p, 0, S).map((u) => u.mediaId)).toEqual(['m2', 'm3']);
  });
  it('does not list media already active at the time', () => {
    expect(upcoming(p, 0, 10 * S).some((u) => u.mediaId === 'm1')).toBe(false);
  });
  it('returns nothing when no later segment has media', () => {
    expect(upcoming(p, S, 10 * S)).toEqual([]);
  });
  it('returns nothing for a zero, negative or NaN horizon, or NaN time', () => {
    expect(upcoming(p, 0, 0)).toEqual([]);
    expect(upcoming(p, 0, -S)).toEqual([]);
    expect(upcoming(p, Number.NaN, S)).toEqual([]);
  });
  it('keeps only the first appearance of a media id', () => {
    const repeated = plan(
      [
        seg(0, S),
        seg(S, 2 * S, [vlayer('x', 'm9')]),
        seg(2 * S, 3 * S, [vlayer('y', 'm9', { src_time: 4 * S })]),
      ],
      0,
      3 * S,
    );
    expect(upcoming(repeated, 0, 10 * S)).toEqual([{ mediaId: 'm9', startsAt: S, srcTime: 0 }]);
  });
  it('merges audio and video of the same media, keeping the earliest', () => {
    const merged = plan(
      [seg(0, S), seg(S, 2 * S, [], [alayer('a', 'm3', { src_time: 7 * S })]), seg(2 * S, 3 * S, [vlayer('v', 'm3')])],
      0,
      3 * S,
    );
    expect(upcoming(merged, 0, 10 * S)).toEqual([{ mediaId: 'm3', startsAt: S, srcTime: 7 * S }]);
  });
  it('sorts results by start time', () => {
    const sorted = plan(
      [seg(0, S), seg(S, 2 * S, [vlayer('v', 'mA')]), seg(2 * S, 3 * S, [vlayer('w', 'mB')])],
      0,
      3 * S,
    );
    const starts = upcoming(sorted, 0, 10 * S).map((u) => u.startsAt);
    expect(starts).toEqual([...starts].sort((a, b) => a - b));
  });
});

describe('activeMedia', () => {
  const p = mainPlan();
  it('lists the video media of the segment at a time', () => {
    expect(activeMedia(p, 0)).toEqual({ video: ['m1'], audio: [] });
  });
  it('lists video and audio media when both are active', () => {
    expect(activeMedia(p, S)).toEqual({ video: ['m1', 'm2'], audio: ['m3'] });
  });
  it('returns nothing for an empty segment', () => {
    expect(activeMedia(p, 3 * S)).toEqual({ video: [], audio: [] });
  });
  it('returns nothing outside the plan', () => {
    expect(activeMedia(p, 4 * S)).toEqual({ video: [], audio: [] });
    expect(activeMedia(p, -1)).toEqual({ video: [], audio: [] });
  });
  it('skips video layers without media (titles and solids)', () => {
    const titled = plan([seg(0, S, [vlayer('t', null)])], 0, S);
    expect(activeMedia(titled, 0)).toEqual({ video: [], audio: [] });
  });
});

describe('audioGainAt', () => {
  it('is unity at 0 dB with no track fader', () => {
    expect(audioGainAt(gainSrc(), S)).toBe(1);
  });
  it('gives 0.5 for -6.0206 dB', () => {
    expect(audioGainAt(gainSrc({}, { gain_db: -6.0206 }), S)).toBeCloseTo(0.5, 6);
  });
  it('gives about 2 for +6.0206 dB', () => {
    expect(audioGainAt(gainSrc({}, { gain_db: 6.0206 }), S)).toBeCloseTo(2, 3);
  });
  it('gives 0.1 for -20 dB', () => {
    expect(audioGainAt(gainSrc({}, { gain_db: -20 }), S)).toBeCloseTo(0.1, 10);
  });
  it('applies the track fader in dB', () => {
    expect(audioGainAt(gainSrc({ track_db: -20 }), S)).toBeCloseTo(0.1, 10);
  });
  it('multiplies clip gain and track fader', () => {
    expect(audioGainAt(gainSrc({ track_db: -20 }, { gain_db: -20 }), S)).toBeCloseTo(0.01, 10);
  });
  it('returns 0 for a muted clip at any time', () => {
    const muted = gainSrc({}, { muted: true });
    expect(audioGainAt(muted, 0)).toBe(0);
    expect(audioGainAt(muted, S)).toBe(0);
  });
  it('returns 0 for gains at or below -120 dB', () => {
    expect(audioGainAt(gainSrc({}, { gain_db: -120 }), S)).toBe(0);
    expect(audioGainAt(gainSrc({}, { gain_db: -150 }), S)).toBe(0);
  });
  it('holds the clamped result before the clip starts', () => {
    const late = gainSrc({ clip_start: S, clip_dur: 2 * S });
    expect(audioGainAt(late, 0)).toBe(1);
  });
  it('holds the clamped result after the clip ends', () => {
    expect(audioGainAt(gainSrc({ clip_start: 0, clip_dur: 2 * S }), 10 * S)).toBe(1);
  });
  it('is measured from the clip start, not from tick 0', () => {
    const offset = gainSrc({ clip_start: 2 * S, clip_dur: 2 * S }, { envelope: [{ t: 0, db: 0 }, { t: S, db: -6.0206 }] });
    expect(audioGainAt(offset, 2.5 * S)).toBeCloseTo(Math.SQRT1_2, 4);
  });
  it('interpolates the envelope linearly in dB', () => {
    const env = gainSrc({}, { envelope: [{ t: 0, db: 0 }, { t: S, db: -6.0206 }] });
    expect(audioGainAt(env, 0.5 * S)).toBeCloseTo(Math.SQRT1_2, 4);
  });
  it('holds the last envelope value after the last point', () => {
    const env = gainSrc({}, { envelope: [{ t: 0, db: 0 }, { t: S, db: -6.0206 }] });
    expect(audioGainAt(env, 1.5 * S)).toBeCloseTo(0.5, 6);
  });
  it('holds the first envelope value before the first point', () => {
    const env = gainSrc({}, { envelope: [{ t: S / 2, db: -6.0206 }] });
    expect(audioGainAt(env, 0)).toBeCloseTo(0.5, 6);
  });
  it('adds no envelope change for an empty envelope', () => {
    expect(audioGainAt(gainSrc({}, { envelope: [] }), S)).toBe(1);
  });
  it('applies a linear fade in', () => {
    const f = gainSrc({}, { fade_in: S, fade_curve: 'linear' });
    expect(audioGainAt(f, 0)).toBe(0);
    expect(audioGainAt(f, 0.25 * S)).toBeCloseTo(0.25, 10);
    expect(audioGainAt(f, 0.5 * S)).toBeCloseTo(0.5, 10);
    expect(audioGainAt(f, S)).toBe(1);
  });
  it('applies an equal power fade in', () => {
    const f = gainSrc({}, { fade_in: S, fade_curve: 'equal_power' });
    expect(audioGainAt(f, 0.5 * S)).toBeCloseTo(Math.SQRT1_2, 10);
    expect(audioGainAt(f, 0.25 * S)).toBeCloseTo(Math.sin(Math.PI / 8), 10);
  });
  it('applies a linear fade out measured to the clip end', () => {
    const f = gainSrc({ clip_dur: 2 * S }, { fade_out: S, fade_curve: 'linear' });
    expect(audioGainAt(f, 1.5 * S)).toBeCloseTo(0.5, 10);
    expect(audioGainAt(f, 2 * S)).toBe(0);
    expect(audioGainAt(f, S)).toBe(1);
  });
  it('applies an equal power fade out', () => {
    const f = gainSrc({ clip_dur: 2 * S }, { fade_out: S, fade_curve: 'equal_power' });
    expect(audioGainAt(f, 1.5 * S)).toBeCloseTo(Math.SQRT1_2, 10);
  });
  it('multiplies overlapping fade in and fade out', () => {
    const f = gainSrc({ clip_dur: S }, { fade_in: S, fade_out: S, fade_curve: 'linear' });
    expect(audioGainAt(f, 0.5 * S)).toBeCloseTo(0.25, 10);
  });
  it('ignores a zero-length fade', () => {
    expect(audioGainAt(gainSrc({}, { fade_in: 0, fade_out: 0 }), 0)).toBe(1);
  });
});

describe('panGains', () => {
  it('gives equal gains at the centre', () => {
    const g = panGains(0);
    expect(g.l).toBeCloseTo(1, 10);
    expect(g.r).toBeCloseTo(1, 10);
  });
  it('is fully left at -1 with no right channel', () => {
    const g = panGains(-1);
    expect(g.l).toBeCloseTo(Math.SQRT2, 10);
    expect(g.r).toBeCloseTo(0, 10);
  });
  it('is fully right at +1 with no left channel', () => {
    const g = panGains(1);
    expect(g.l).toBeCloseTo(0, 10);
    expect(g.r).toBeCloseTo(Math.SQRT2, 10);
  });
  it('treats NaN as centre', () => {
    expect(panGains(Number.NaN)).toEqual(panGains(0));
  });
  it('clamps out-of-range pans', () => {
    expect(panGains(5)).toEqual(panGains(1));
    expect(panGains(-5)).toEqual(panGains(-1));
  });
  it('keeps constant power (l squared plus r squared is 2) across the range', () => {
    for (const p of [-0.9, -0.7, -0.2, 0.3, 0.9]) {
      const g = panGains(p);
      expect(g.l * g.l + g.r * g.r).toBeCloseTo(2, 10);
    }
  });
  it('moves energy to the right as pan increases', () => {
    expect(panGains(-0.5).l).toBeGreaterThan(panGains(0.5).l);
    expect(panGains(-0.5).r).toBeLessThan(panGains(0.5).r);
  });
});

describe('gainRamp', () => {
  it('samples both ends with a quarter-second step', () => {
    const pts = gainRamp(gainSrc(), 0, S, 0.25);
    expect(pts.length).toBe(5);
    expect(pts.map((p) => p.offsetSec)).toEqual([0, 0.25, 0.5, 0.75, 1]);
  });
  it('gives unity gain for a flat clip', () => {
    for (const p of gainRamp(gainSrc(), 0, S, 0.25)) {
      expect(p.gain).toBe(1);
    }
  });
  it('returns one point when the range is empty', () => {
    const pts = gainRamp(gainSrc(), S, S);
    expect(pts).toEqual([{ offsetSec: 0, gain: 1 }]);
  });
  it('returns nothing when the end is before the start', () => {
    expect(gainRamp(gainSrc(), S, 0)).toEqual([]);
  });
  it('returns nothing for non-finite bounds', () => {
    expect(gainRamp(gainSrc(), Number.NaN, S)).toEqual([]);
    expect(gainRamp(gainSrc(), 0, Number.POSITIVE_INFINITY)).toEqual([]);
  });
  it('uses a 0.05 s step by default (21 points for one second)', () => {
    expect(gainRamp(gainSrc(), 0, S).length).toBe(21);
  });
  it('falls back to the default step for zero, negative or NaN steps', () => {
    expect(gainRamp(gainSrc(), 0, S, 0).length).toBe(21);
    expect(gainRamp(gainSrc(), 0, S, -1).length).toBe(21);
    expect(gainRamp(gainSrc(), 0, S, Number.NaN).length).toBe(21);
  });
  it('caps the number of points at 400 for long ranges', () => {
    const pts = gainRamp(gainSrc({ clip_dur: 10_000 * S }), 0, 10_000 * S, 0.001);
    expect(pts.length).toBe(400);
    expect(pts[399]?.offsetSec).toBe(10_000);
  });
  it('follows the fade shape across samples', () => {
    const f = gainSrc({}, { fade_in: S, fade_curve: 'linear' });
    const pts = gainRamp(f, 0, S, 0.5);
    expect(pts.map((p) => p.gain)).toEqual([0, 0.5, 1]);
  });
});

describe('transitionProgress', () => {
  const tr = (kind: Transition['kind'], duration: Tick): Transition => ({ kind, duration });
  const layer = (t: Transition | null, clipStart: Tick = S): VideoLayer =>
    vlayer('c', 'm', { transition: t, clip_start: clipStart });

  it('is 0 at the transition start and 0.5 halfway through', () => {
    const l = layer(tr('crossfade', 2 * S));
    expect(transitionProgress(l, S)).toBe(0);
    expect(transitionProgress(l, 2 * S)).toBe(0.5);
  });
  it('is 0.75 three quarters of the way', () => {
    expect(transitionProgress(layer(tr('crossfade', 2 * S)), 2.5 * S)).toBe(0.75);
  });
  it('is null at and after the transition end', () => {
    const l = layer(tr('crossfade', 2 * S));
    expect(transitionProgress(l, 3 * S)).toBeNull();
    expect(transitionProgress(l, 4 * S)).toBeNull();
  });
  it('clamps to 0 before the clip start', () => {
    expect(transitionProgress(layer(tr('crossfade', 2 * S)), 0)).toBe(0);
  });
  it('is null when there is no transition', () => {
    expect(transitionProgress(layer(null), S)).toBeNull();
  });
  it('is null for a zero, negative or NaN duration', () => {
    expect(transitionProgress(layer(tr('crossfade', 0)), S)).toBeNull();
    expect(transitionProgress(layer(tr('crossfade', -S)), S)).toBeNull();
    expect(transitionProgress(layer(tr('crossfade', Number.NaN)), S)).toBeNull();
  });
});

describe('layerOpacityAt', () => {
  it('returns the layer opacity when there is no transition', () => {
    expect(layerOpacityAt(vlayer('c', 'm', { opacity: 0.5 }), 0)).toBe(0.5);
  });
  it('clamps opacity into 0..1', () => {
    expect(layerOpacityAt(vlayer('c', 'm', { opacity: 2 }), 0)).toBe(1);
    expect(layerOpacityAt(vlayer('c', 'm', { opacity: -1 }), 0)).toBe(0);
  });
  it('treats a NaN opacity as 0', () => {
    expect(layerOpacityAt(vlayer('c', 'm', { opacity: Number.NaN }), 0)).toBe(0);
  });
  it('multiplies opacity by the transition progress', () => {
    const l = vlayer('c', 'm', {
      opacity: 0.8,
      clip_start: 0,
      transition: { kind: 'crossfade', duration: 2 * S },
    });
    expect(layerOpacityAt(l, S)).toBeCloseTo(0.4, 10);
  });
  it('is 0 at the very start of a transition with opacity 1', () => {
    const l = vlayer('c', 'm', { clip_start: 0, transition: { kind: 'crossfade', duration: S } });
    expect(layerOpacityAt(l, 0)).toBe(0);
  });
  it('falls back to base opacity once the transition has ended', () => {
    const l = vlayer('c', 'm', { opacity: 0.8, clip_start: 0, transition: { kind: 'crossfade', duration: S } });
    expect(layerOpacityAt(l, 2 * S)).toBe(0.8);
  });
});

describe('dipAmount', () => {
  const dip = (kind: 'dip_to_black' | 'dip_to_white' | 'crossfade'): VideoLayer =>
    vlayer('c', 'm', { clip_start: 0, transition: { kind, duration: 2 * S } });

  it('is null for a crossfade', () => {
    expect(dipAmount(dip('crossfade'), S)).toBeNull();
  });
  it('is null when there is no transition', () => {
    expect(dipAmount(vlayer('c', 'm'), S)).toBeNull();
  });
  it('is full black at the start of a dip to black', () => {
    expect(dipAmount(dip('dip_to_black'), 0)).toEqual({ color: 'black', amount: 1 });
  });
  it('is half at a quarter of the transition', () => {
    const d = dipAmount(dip('dip_to_black'), Math.round(0.5 * S));
    expect(d?.color).toBe('black');
    expect(d?.amount).toBeCloseTo(0.5, 10);
  });
  it('is zero from the midpoint onward', () => {
    expect(dipAmount(dip('dip_to_black'), S)).toEqual({ color: 'black', amount: 0 });
    expect(dipAmount(dip('dip_to_black'), Math.round(1.5 * S))).toEqual({ color: 'black', amount: 0 });
  });
  it('uses white for a dip to white', () => {
    const d = dipAmount(dip('dip_to_white'), Math.round(0.5 * S));
    expect(d?.color).toBe('white');
    expect(d?.amount).toBeCloseTo(0.5, 10);
  });
  it('is null at and after the transition end', () => {
    expect(dipAmount(dip('dip_to_black'), 2 * S)).toBeNull();
  });
  it('reports full amount before the clip start', () => {
    expect(dipAmount(dip('dip_to_black'), -S)).toEqual({ color: 'black', amount: 1 });
  });
});
