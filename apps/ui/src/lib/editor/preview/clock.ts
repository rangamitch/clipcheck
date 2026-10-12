// Preview clock (pure TS, no DOM access at import time).
//
// Design decision M6: the audio device clock is master. While an audio context is attached (the
// caller passes it only when it is running), `now()` follows `currentTime`; otherwise it follows the
// wall clock. Picture follows the returned tick, so video drops frames rather than audio glitching.

import { MAX_TICK, TICKS_PER_SECOND } from '../ticks';
import type { Tick } from '../types';

/** Largest playback speed in either direction (reverse shuttle down to -8, forward up to 8). */
export const MAX_CLOCK_RATE = 8;

/** Returns wall time in seconds. Injected so tests can drive the clock. */
export type TimeSource = () => number;

/** The part of a Web Audio context the clock needs. */
export interface AudioClockSource {
  /** Seconds of audio rendered so far (the audio device clock). */
  readonly currentTime: number;
  /** Seconds between rendering and the output device, when the browser reports it. */
  readonly outputLatency?: number;
  /** Seconds of base processing latency, when the browser reports it. */
  readonly baseLatency?: number;
}

/** Playback clock in ticks. */
export interface Clock {
  /** Current playhead in ticks. Never below 0. */
  now(): Tick;
  /** Starts (or restarts) the clock at `from` with the given rate. Rate is clamped to -8..=8. */
  start(from: Tick, rate?: number): void;
  /** Stops the clock and returns the tick where it stopped. */
  stop(): Tick;
  /** Changes the rate without a jump in `now()`. Rate is clamped to -8..=8. */
  setRate(rate: number): void;
  /** True between `start` and `stop`. */
  isRunning(): boolean;
  /**
   * Attaches the audio master clock, or detaches it with `null`. Re-bases the clock on the current
   * tick, so `now()` does not jump. Pass `null` when the audio context is not running.
   */
  attachAudio(ctx: AudioClockSource | null): void;
}

/** Wall time in seconds from `performance.now()`. Only evaluated when called. */
function defaultTimeSource(): number {
  return performance.now() / 1000;
}

/** Clamps a finite tick into 0..=MAX_TICK and rounds it. Non-finite values give 0. */
function toPlayTick(v: number): Tick {
  if (!Number.isFinite(v)) return 0;
  return Math.min(MAX_TICK, Math.max(0, Math.round(v)));
}

/** Output latency of the audio context in seconds (0 when unknown). */
function outputLatencySec(audio: AudioClockSource): number {
  const s = (audio.outputLatency ?? 0) + (audio.baseLatency ?? 0);
  return Number.isFinite(s) && s > 0 ? s : 0;
}

/** Clamps a rate into -MAX_CLOCK_RATE..=MAX_CLOCK_RATE. NaN gives 1 (normal speed). */
function clampRate(rate: number): number {
  if (Number.isNaN(rate)) return 1;
  return Math.min(MAX_CLOCK_RATE, Math.max(-MAX_CLOCK_RATE, rate));
}

/**
 * Creates a clock. `timeSource` returns wall seconds (default `performance.now() / 1000`).
 * The clock starts stopped at tick 0 with rate 1.
 */
export function createClock(timeSource: TimeSource = defaultTimeSource): Clock {
  let running = false;
  let rate = 1;
  // Anchor: the tick at the last re-base, plus the wall and audio times read at that moment.
  let anchorTick: Tick = 0;
  let anchorWall = 0;
  let anchorAudio: number | null = null;
  let audio: AudioClockSource | null = null;

  /** Wall time in seconds; a non-finite reading counts as 0. */
  function wallNow(): number {
    const v = timeSource();
    return Number.isFinite(v) ? v : 0;
  }

  /**
   * Seconds elapsed since the anchor, from the audio clock when attached, else the wall clock.
   * The audio reading is taken `outputLatency + baseLatency` late, because the listener hears the
   * sound that much after the audio clock reads it. Before that delay the clock holds at the anchor.
   */
  function elapsedSec(): number {
    let v: number;
    if (anchorAudio !== null && audio !== null && Number.isFinite(audio.currentTime)) {
      v = Math.max(0, audio.currentTime - outputLatencySec(audio) - anchorAudio);
    } else {
      v = wallNow() - anchorWall;
    }
    return Number.isFinite(v) ? v : 0;
  }

  /** Playhead tick now. While stopped this is the stored anchor tick. */
  function nowTick(): Tick {
    if (!running) return anchorTick;
    return toPlayTick(anchorTick + elapsedSec() * rate * TICKS_PER_SECOND);
  }

  /** Re-bases the anchor so that `t` is the tick at this instant, using the current audio source. */
  function anchorAt(t: Tick): void {
    anchorTick = t;
    anchorWall = wallNow();
    anchorAudio = audio !== null && Number.isFinite(audio.currentTime) ? audio.currentTime : null;
  }

  return {
    now(): Tick {
      return nowTick();
    },

    start(from: Tick, nextRate: number = 1): void {
      rate = clampRate(nextRate);
      anchorAt(toPlayTick(from));
      running = true;
    },

    stop(): Tick {
      if (!running) return anchorTick;
      const t = nowTick();
      running = false;
      anchorAt(t);
      return t;
    },

    setRate(nextRate: number): void {
      if (running) {
        const t = nowTick();
        rate = clampRate(nextRate);
        anchorAt(t);
      } else {
        rate = clampRate(nextRate);
      }
    },

    isRunning(): boolean {
      return running;
    },

    attachAudio(ctx: AudioClockSource | null): void {
      if (running) {
        const t = nowTick();
        audio = ctx;
        anchorAt(t);
      } else {
        audio = ctx;
      }
    },
  };
}

/**
 * Clamps a playhead into `0..=endTick`. NaN gives 0; a negative end counts as 0.
 */
export function clampPlayhead(t: Tick, endTick: Tick): Tick {
  const hi = toPlayTick(endTick);
  if (Number.isNaN(t)) return 0;
  return Math.min(hi, Math.max(0, Math.round(t)));
}
