import { describe, expect, it } from 'vitest';
import { playingSeekDue } from './engine';

describe('playingSeekDue', () => {
  it('resync seeks a video element that is off by 0.1 s (below the free-running threshold)', () => {
    // A ruler click while playing 0.4 s ahead, or a source jump of 0.3 s, must not stay as an offset.
    expect(playingSeekDue(0.1, false, true, false)).toBe(true);
    expect(playingSeekDue(-0.3, false, true, false)).toBe(true);
  });

  it('resync seeks bypass the re-seek gap', () => {
    expect(playingSeekDue(0.4, false, true, false)).toBe(true);
    expect(playingSeekDue(0.4, true, true, false)).toBe(true);
  });

  it('a resync does not seek an element that is already within the paused threshold', () => {
    expect(playingSeekDue(0.01, false, true, true)).toBe(false);
    expect(playingSeekDue(-0.02, true, true, true)).toBe(false);
  });

  it('free-running video drift below 0.5 s is not re-seeked', () => {
    expect(playingSeekDue(0.4, false, false, true)).toBe(false);
    expect(playingSeekDue(-0.45, false, false, true)).toBe(false);
  });

  it('free-running video drift above 0.5 s is re-seeked when the gap is open', () => {
    expect(playingSeekDue(0.6, false, false, true)).toBe(true);
    expect(playingSeekDue(0.6, false, false, false)).toBe(false);
  });

  it('free-running audio drift uses the 0.25 s threshold', () => {
    expect(playingSeekDue(0.3, true, false, true)).toBe(true);
    expect(playingSeekDue(0.2, true, false, true)).toBe(false);
  });

  it('a segment boundary keeps a continuous element within 0.25 s (decoder latency, no seek)', () => {
    expect(playingSeekDue(0.05, false, false, true, true)).toBe(false);
    expect(playingSeekDue(-0.08, false, false, false, true)).toBe(false);
    expect(playingSeekDue(0.2, true, false, false, true)).toBe(false);
  });

  it('a segment boundary re-seeks an element whose source jumped by more than 0.25 s, bypassing the gap', () => {
    expect(playingSeekDue(0.3, false, false, false, true)).toBe(true);
    expect(playingSeekDue(-1.5, true, false, false, true)).toBe(true);
  });

  it('a seek or rate change still resyncs exactly, even at a boundary-sized offset', () => {
    expect(playingSeekDue(0.05, false, true, false, false)).toBe(true);
  });
});
