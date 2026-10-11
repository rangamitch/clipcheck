import { describe, expect, it } from 'vitest';
import { MAX_CLIPS, addClip, suggestNewChord } from './clips';
import { canonicalizeChord, isReservedChord } from './hotkeys';
import type { ClipHotkeyConfig } from './store';

/** The 19 candidates in the order the spec gives them: 7 Alt chords, then bare F13..F24. */
const ALL_CANDIDATES = [
  'Alt+F9',
  'Alt+F10',
  'Alt+F6',
  'Alt+F5',
  'Alt+F3',
  'Alt+F2',
  'Alt+F1',
  'F13',
  'F14',
  'F15',
  'F16',
  'F17',
  'F18',
  'F19',
  'F20',
  'F21',
  'F22',
  'F23',
  'F24',
];

/** Repeatedly suggests and marks each suggestion used, returning the order produced. */
function suggestionSequence(start: string[] = []): string[] {
  const used = [...start];
  const out: string[] = [];
  for (;;) {
    const next = suggestNewChord(used);
    if (next === null) return out;
    out.push(next);
    used.push(next);
  }
}

describe('suggestNewChord: candidate order and exhaustion', () => {
  it('produces all 19 candidates in spec order, then null', () => {
    expect(suggestionSequence()).toEqual(ALL_CANDIDATES);
  });

  it('returns null when every one of the 19 candidates is used', () => {
    expect(suggestNewChord(ALL_CANDIDATES)).toBeNull();
  });

  it('returns F14 when all 7 Alt candidates and F13 are used', () => {
    expect(suggestNewChord([...ALL_CANDIDATES.slice(0, 7), 'F13'])).toBe('F14');
  });

  it('returns F24 when only the last candidate is free', () => {
    const allButLast = ALL_CANDIDATES.slice(0, -1);
    expect(suggestNewChord(allButLast)).toBe('F24');
  });

  it('skips a used middle candidate and returns the first free one', () => {
    expect(suggestNewChord(['Alt+F5'])).toBe('Alt+F9');
    expect(suggestNewChord(['Alt+F9', 'Alt+F10', 'Alt+F6'])).toBe('Alt+F5');
  });

  it('matches used chords case-insensitively', () => {
    expect(suggestNewChord(['ALT+F9'])).toBe('Alt+F10');
    expect(suggestNewChord(['alt+F9', 'alt+f10'])).toBe('Alt+F6');
    expect(suggestNewChord(['f13'])).toBe('Alt+F9');
  });

  it('does not treat Alt+Shift+F9 as Alt+F9 (different chord, does not block it)', () => {
    // No candidate has two modifiers, so modifier-order equivalence cannot be observed here.
    expect(suggestNewChord(['shift+alt+f9'])).toBe('Alt+F9');
  });

  it('ignores used entries that are not candidates', () => {
    expect(suggestNewChord(['Ctrl+Shift+F20', 'Ctrl+F1'])).toBe('Alt+F9');
  });

  it('never returns a reserved chord, for any prefix of the candidate order', () => {
    for (let i = 0; i <= ALL_CANDIDATES.length; i++) {
      const out = suggestNewChord(ALL_CANDIDATES.slice(0, i));
      if (out === null) continue;
      expect(isReservedChord(out), `prefix ${i} gave ${out}`).toBeNull();
    }
  });

  it('never returns Alt+F4 or other reserved chords', () => {
    expect(ALL_CANDIDATES).not.toContain('Alt+F4');
    for (const c of ALL_CANDIDATES) {
      expect(isReservedChord(canonicalizeChord(c) ?? c), c).toBeNull();
    }
  });

  it('returns canonical spelling', () => {
    expect(suggestNewChord([])).toBe(canonicalizeChord('Alt+F9'));
    expect(suggestNewChord(['Alt+F9'])).toBe(canonicalizeChord('Alt+F10'));
  });
});

describe('addClip: chord uniqueness', () => {
  it('returns an unchanged copy (same length, no new chord) when no chord is free', () => {
    const clips: ClipHotkeyConfig[] = [{ name: 'Clip 1', keys: 'Alt+F9', seconds: 30, press: 'tap' }];
    const out = addClip(clips, ALL_CANDIDATES);
    expect(out).toHaveLength(1);
    expect(out).toEqual(clips);
    expect(out).not.toBe(clips);
  });

  it('never produces a duplicate chord across repeated adds up to MAX_CLIPS', () => {
    let list: ClipHotkeyConfig[] = [];
    for (let i = 0; i < MAX_CLIPS; i++) {
      list = addClip(list);
    }
    expect(list).toHaveLength(MAX_CLIPS);
    const keys = list.map((c) => c.keys);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).toEqual(ALL_CANDIDATES.slice(0, MAX_CLIPS));
  });

  it('after MAX_CLIPS the list is full and addClip leaves it unchanged', () => {
    let list: ClipHotkeyConfig[] = [];
    for (let i = 0; i < MAX_CLIPS; i++) list = addClip(list);
    const again = addClip(list);
    expect(again).toEqual(list);
  });

  it('does not reuse a chord already on a clip when the default used list is taken from the clips', () => {
    const list: ClipHotkeyConfig[] = [
      { name: 'Clip 1', keys: 'Alt+F9', seconds: 30, press: 'tap' },
      { name: 'Clip 2', keys: 'alt+f10', seconds: 30, press: 'tap' },
    ];
    const out = addClip(list);
    expect(out).toHaveLength(3);
    expect(out[2].keys).toBe('Alt+F6');
    const keys = out.map((c) => (canonicalizeChord(c.keys) ?? c.keys).toLowerCase());
    expect(new Set(keys).size).toBe(keys.length);
  });
});
