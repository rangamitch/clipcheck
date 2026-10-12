import { beforeEach, describe, expect, it } from 'vitest';
import { TICKS_PER_SECOND } from '../ticks';
import { MAX_CLOCK_RATE, clampPlayhead, createClock } from './clock';
import type { Clock } from './clock';

const S = TICKS_PER_SECOND;

/** Injected wall clock, in seconds, plus a fake audio context whose time the test controls. */
let wall = 0;
let clock: Clock;

function audio(currentTime: number): { currentTime: number } {
  return { currentTime };
}

beforeEach(() => {
  wall = 0;
  clock = createClock(() => wall);
});

describe('createClock basics', () => {
  it('starts stopped at tick 0', () => {
    expect(clock.now()).toBe(0);
    expect(clock.isRunning()).toBe(false);
  });
  it('exposes a rate limit of 8', () => {
    expect(MAX_CLOCK_RATE).toBe(8);
  });
  it('advances with wall time once started', () => {
    clock.start(0);
    expect(clock.isRunning()).toBe(true);
    expect(clock.now()).toBe(0);
    wall = 1;
    expect(clock.now()).toBe(S);
  });
  it('starts from a given tick', () => {
    clock.start(5 * S);
    expect(clock.now()).toBe(5 * S);
    wall = 0.5;
    expect(clock.now()).toBe(Math.round(5.5 * S));
  });
  it('rounds a fractional start tick to an integer', () => {
    clock.start(1.4);
    expect(clock.now()).toBe(1);
  });
  it('treats a negative start as tick 0', () => {
    clock.start(-5 * S);
    expect(clock.now()).toBe(0);
  });
  it('treats a NaN start as tick 0', () => {
    clock.start(Number.NaN);
    expect(clock.now()).toBe(0);
  });
  it('restarts from a new tick when started again', () => {
    clock.start(0);
    wall = 1;
    clock.start(3 * S);
    expect(clock.now()).toBe(3 * S);
    wall = 2;
    expect(clock.now()).toBe(4 * S);
  });
});

describe('stop', () => {
  it('returns the tick where the clock stopped', () => {
    clock.start(0);
    wall = 2.5;
    expect(clock.stop()).toBe(Math.round(2.5 * S));
    expect(clock.isRunning()).toBe(false);
  });
  it('holds the stopped tick while wall time moves on', () => {
    clock.start(0);
    wall = 2.5;
    clock.stop();
    wall = 10;
    expect(clock.now()).toBe(Math.round(2.5 * S));
  });
  it('returns the last tick again when stopped twice', () => {
    clock.start(0);
    wall = 2;
    clock.stop();
    expect(clock.stop()).toBe(2 * S);
  });
  it('returns 0 when never started', () => {
    expect(clock.stop()).toBe(0);
  });
});

describe('rates', () => {
  it('plays at double speed with rate 2', () => {
    clock.start(0, 2);
    wall = 1;
    expect(clock.now()).toBe(2 * S);
  });
  it('plays backwards at rate -1', () => {
    clock.start(10 * S, -1);
    wall = 3;
    expect(clock.now()).toBe(7 * S);
  });
  it('never goes below zero when reversing', () => {
    clock.start(S, -1);
    wall = 5;
    expect(clock.now()).toBe(0);
  });
  it('stays at zero after reaching the start', () => {
    clock.start(S, -1);
    wall = 10;
    expect(clock.now()).toBe(0);
  });
  it('clamps a rate above 8 to 8', () => {
    clock.start(0, 100);
    wall = 1;
    expect(clock.now()).toBe(8 * S);
  });
  it('clamps a rate below -8 to -8', () => {
    clock.start(10 * S, -100);
    wall = 1;
    expect(clock.now()).toBe(2 * S);
  });
  it('treats a NaN rate as normal speed', () => {
    clock.start(0, Number.NaN);
    wall = 1;
    expect(clock.now()).toBe(S);
  });
  it('defaults to normal speed when no rate is given', () => {
    clock.start(0);
    wall = 1;
    expect(clock.now()).toBe(S);
  });
});

describe('setRate', () => {
  it('changes speed without jumping the playhead', () => {
    clock.start(0, 1);
    wall = 2;
    expect(clock.now()).toBe(2 * S);
    clock.setRate(2);
    expect(clock.now()).toBe(2 * S);
    wall = 3;
    expect(clock.now()).toBe(4 * S);
  });
  it('reverses direction from the current tick', () => {
    clock.start(0, 1);
    wall = 2;
    clock.setRate(-1);
    wall = 3;
    expect(clock.now()).toBe(S);
  });
  it('sets the rate for the next start when stopped', () => {
    clock.setRate(3);
    clock.start(0);
    wall = 1;
    expect(clock.now()).toBe(3 * S);
  });
  it('keeps the stopped playhead when set while stopped', () => {
    clock.start(0);
    wall = 1;
    clock.stop();
    clock.setRate(4);
    expect(clock.now()).toBe(S);
  });
  it('clamps the rate like start does', () => {
    clock.start(0, 1);
    wall = 1;
    clock.setRate(50);
    expect(clock.now()).toBe(S);
    wall = 2;
    expect(clock.now()).toBe(9 * S);
  });
  it('does not move the playhead when the rate is changed by the same amount', () => {
    clock.start(0, 1);
    wall = 1;
    const before = clock.now();
    clock.setRate(1);
    expect(clock.now()).toBe(before);
  });
});

describe('audio master clock', () => {
  it('re-bases on attach so the playhead does not jump', () => {
    clock.start(0);
    wall = 1;
    expect(clock.now()).toBe(S);
    clock.attachAudio(audio(10));
    expect(clock.now()).toBe(S);
  });
  it('then follows the audio clock rather than wall time', () => {
    const a = audio(10);
    clock.start(0);
    wall = 1;
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(3 * S);
    wall = 5;
    expect(clock.now()).toBe(3 * S);
  });
  it('detaching re-bases on the wall clock without a jump', () => {
    const a = audio(10);
    clock.start(0);
    wall = 1;
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(3 * S);
    clock.attachAudio(null);
    expect(clock.now()).toBe(3 * S);
    wall = 2;
    expect(clock.now()).toBe(4 * S);
  });
  it('attaching while stopped does not change the stopped tick', () => {
    clock.start(0);
    wall = 1;
    clock.stop();
    clock.attachAudio(audio(99));
    expect(clock.now()).toBe(S);
  });
  it('uses the audio time from the next start after attaching while stopped', () => {
    const a = audio(5);
    clock.attachAudio(a);
    clock.start(0);
    a.currentTime = 6;
    expect(clock.now()).toBe(S);
    wall = 100;
    expect(clock.now()).toBe(S);
  });
  it('falls back to the wall clock when the audio time is NaN', () => {
    clock.start(0);
    clock.attachAudio(audio(Number.NaN));
    wall = 2;
    expect(clock.now()).toBe(2 * S);
  });
  it('scales audio time by the playback rate', () => {
    const a = audio(0);
    clock.start(0, 2);
    clock.attachAudio(a);
    a.currentTime = 1;
    expect(clock.now()).toBe(2 * S);
  });
});

describe('robustness', () => {
  it('does not throw and stays at zero when the time source returns NaN', () => {
    const c = createClock(() => Number.NaN);
    c.start(0);
    expect(c.now()).toBe(0);
    expect(c.isRunning()).toBe(true);
  });
  it('treats an infinite time source reading as zero', () => {
    const c = createClock(() => Number.POSITIVE_INFINITY);
    c.start(S);
    expect(c.now()).toBe(S);
  });
  it('stop returns the same value that now() reported just before', () => {
    clock.start(0, 1);
    wall = 1.75;
    const reported = clock.now();
    expect(clock.stop()).toBe(reported);
  });
});

describe('clampPlayhead', () => {
  it('keeps a playhead inside the range', () => {
    expect(clampPlayhead(5, 10)).toBe(5);
  });
  it('clamps past the end', () => {
    expect(clampPlayhead(15, 10)).toBe(10);
    expect(clampPlayhead(10, 10)).toBe(10);
  });
  it('clamps a negative playhead to 0', () => {
    expect(clampPlayhead(-3, 10)).toBe(0);
  });
  it('rounds to the nearest tick', () => {
    expect(clampPlayhead(4.6, 100)).toBe(5);
  });
  it('gives 0 for a NaN playhead', () => {
    expect(clampPlayhead(Number.NaN, 10)).toBe(0);
  });
  it('treats a negative or NaN end as 0', () => {
    expect(clampPlayhead(3, -5)).toBe(0);
    expect(clampPlayhead(5, Number.NaN)).toBe(0);
  });
});

describe('audio output latency', () => {
  /** An audio clock source that also reports output and base latency (seconds). */
  function latent(
    currentTime: number,
    over: { outputLatency?: number; baseLatency?: number } = {},
  ): { currentTime: number; outputLatency?: number; baseLatency?: number } {
    return { currentTime, ...over };
  }

  it('elapsed time subtracts the output latency', () => {
    const a = latent(10, { outputLatency: 0.5 });
    clock.start(0);
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(Math.round(1.5 * S));
  });

  it('elapsed time subtracts the base latency', () => {
    const a = latent(10, { baseLatency: 0.25 });
    clock.start(0);
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(Math.round(1.75 * S));
  });

  it('output and base latency are both subtracted', () => {
    const a = latent(10, { outputLatency: 0.1, baseLatency: 0.2 });
    clock.start(0);
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(Math.round(1.7 * S));
  });

  it('the clock holds at the anchor until the latency has passed', () => {
    const a = latent(10, { outputLatency: 0.5 });
    clock.start(0);
    wall = 1;
    clock.attachAudio(a);
    a.currentTime = 10.3;
    expect(clock.now()).toBe(S);
    a.currentTime = 10.5;
    expect(clock.now()).toBe(S);
  });

  it('a NaN latency counts as no latency', () => {
    const a = latent(10, { outputLatency: Number.NaN });
    clock.start(0);
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(2 * S);
  });

  it('a negative latency counts as no latency', () => {
    const a = latent(10, { outputLatency: -1, baseLatency: -0.5 });
    clock.start(0);
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(2 * S);
  });

  it('latency is removed before the playback rate scales the elapsed time', () => {
    const a = latent(0, { outputLatency: 0.5 });
    clock.start(0, 2);
    clock.attachAudio(a);
    a.currentTime = 1;
    // (1 s - 0.5 s latency) at rate 2 is 1 s of timeline.
    expect(clock.now()).toBe(S);
  });

  it('a missing latency field behaves like zero', () => {
    const a = latent(10);
    clock.start(0);
    clock.attachAudio(a);
    a.currentTime = 11;
    expect(clock.now()).toBe(S);
  });

  it('detaching the audio source ignores its latency from then on', () => {
    const a = latent(10, { outputLatency: 0.5 });
    clock.start(0);
    wall = 1;
    // Attaching re-bases on tick 1 s; then 2 s of audio minus 0.5 s latency adds 1.5 s.
    clock.attachAudio(a);
    a.currentTime = 12;
    expect(clock.now()).toBe(Math.round(2.5 * S));
    clock.attachAudio(null);
    expect(clock.now()).toBe(Math.round(2.5 * S));
    // Detached: wall time counts with no latency subtracted.
    wall = 2;
    expect(clock.now()).toBe(Math.round(3.5 * S));
  });
});
