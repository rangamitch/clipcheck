import { describe, expect, it } from 'vitest';
import { LONG_PRESS_MS, defaultPressFor, pressOf } from './clips';
import type { ClipHotkeyConfig } from './store';
import { findDuplicateChords, isTapLongPair, tapLongPairFlags, type ChordEntry } from './hotkeys';

function clip(name: string, keys: string, press?: 'tap' | 'long'): ClipHotkeyConfig {
  return { name, keys, seconds: 30, press: press as ClipHotkeyConfig['press'] };
}

function entry(label: string, chord: string, press?: 'tap' | 'long'): ChordEntry {
  return press === undefined ? { label, chord } : { label, chord, press };
}

describe('hold threshold', () => {
  it('is 400 ms, the daemon long-press threshold', () => {
    expect(LONG_PRESS_MS).toBe(400);
  });
});

describe('pressOf', () => {
  it('reads long as long', () => {
    expect(pressOf({ press: 'long' })).toBe('long');
  });

  it('treats tap and a missing value (older config) as tap', () => {
    expect(pressOf({ press: 'tap' })).toBe('tap');
    expect(pressOf({ press: undefined as unknown as 'tap' })).toBe('tap');
  });
});

describe('defaultPressFor', () => {
  it('is tap when no clip uses the chord', () => {
    expect(defaultPressFor('Alt+F9', [clip('Clip 1', 'Alt+F10', 'tap')])).toBe('tap');
  });

  it('is long when a tap clip already uses the chord', () => {
    expect(defaultPressFor('alt+f9', [clip('Clip 1', 'Alt+F9', 'tap')])).toBe('long');
  });

  it('is tap when only a long clip uses the chord', () => {
    expect(defaultPressFor('Alt+F9', [clip('Clip 1', 'Alt+F9', 'long')])).toBe('tap');
  });
});

describe('Tap + Long press pairs', () => {
  it('a tap and a long clip on one chord is a pair, not a conflict', () => {
    const entries = [entry('"Clip 1" (tap)', 'Alt+F9', 'tap'), entry('"Clip 2" (long press)', 'Alt+F9', 'long')];
    expect(isTapLongPair(entries)).toBe(true);
    expect(findDuplicateChords(entries)).toEqual([]);
    expect(tapLongPairFlags(entries)).toEqual([true, true]);
  });

  it('two tap clips on one chord still conflict', () => {
    const entries = [entry('a (tap)', 'Alt+F9', 'tap'), entry('b (tap)', 'Alt+F9', 'tap')];
    expect(findDuplicateChords(entries)).toEqual([{ chord: 'Alt+F9', labels: ['a (tap)', 'b (tap)'] }]);
    expect(tapLongPairFlags(entries)).toEqual([false, false]);
  });

  it('two long clips on one chord still conflict', () => {
    const entries = [entry('a (long press)', 'Alt+F9', 'long'), entry('b (long press)', 'Alt+F9', 'long')];
    expect(findDuplicateChords(entries)).toHaveLength(1);
  });

  it('a third clip on a tap + long pair is a conflict', () => {
    const entries = [
      entry('a (tap)', 'Alt+F9', 'tap'),
      entry('b (long press)', 'Alt+F9', 'long'),
      entry('c (long press)', 'Alt+F9', 'long'),
    ];
    expect(findDuplicateChords(entries)).toHaveLength(1);
    expect(tapLongPairFlags(entries)).toEqual([false, false, false]);
  });

  it('a clip that shares a chord with a session hotkey conflicts, whatever its press', () => {
    const tapWithSession = [entry('"Clip 1" (tap)', 'Alt+F11', 'tap'), entry('split the session', 'Alt+F11')];
    const longWithSession = [entry('"Clip 1" (long press)', 'Alt+F11', 'long'), entry('split the session', 'Alt+F11')];
    expect(findDuplicateChords(tapWithSession)).toHaveLength(1);
    expect(findDuplicateChords(longWithSession)).toHaveLength(1);
    expect(tapLongPairFlags(longWithSession)).toEqual([false, false]);
  });

  it('matches chords in canonical form, so spelling does not hide a pair', () => {
    const entries = [entry('a (tap)', 'alt+f9', 'tap'), entry('b (long press)', 'Alt+F9', 'long')];
    expect(findDuplicateChords(entries)).toEqual([]);
  });
});
