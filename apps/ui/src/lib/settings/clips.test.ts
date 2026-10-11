import { describe, expect, it } from 'vitest';
import {
  CLIP_MAX_SECONDS,
  CLIP_MIN_SECONDS,
  CLIP_NAME_MAX_CHARS,
  CLIP_PRESETS,
  MAX_CLIPS,
  addClip,
  canAddClip,
  clipCountText,
  clipSecondsMessage,
  clipSecondsProblem,
  clipsOverLimit,
  nextClipName,
  removeClip,
  suggestClipSeconds,
  suggestNewChord,
  updateClip,
} from './clips';
import type { ClipHotkeyConfig } from './store';

function clip(name: string, keys = 'F13', seconds = 30): ClipHotkeyConfig {
  return { name, keys, seconds, press: 'tap' };
}

/** n clips named "Clip 1".."Clip n" with distinct F13-based chords. */
function clips(n: number, seconds = 30): ClipHotkeyConfig[] {
  return Array.from({ length: n }, (_, i) => clip(`Clip ${i + 1}`, `Ctrl+Shift+F${13 + (i % 12)}`, seconds));
}

describe('clip constants', () => {
  it('match the schema limits', () => {
    expect(MAX_CLIPS).toBe(8);
    expect(CLIP_MIN_SECONDS).toBe(5);
    expect(CLIP_MAX_SECONDS).toBe(3600);
    expect(CLIP_NAME_MAX_CHARS).toBe(32);
  });

  it('CLIP_PRESETS are 15 s, 30 s, 1, 2 and 5 minutes', () => {
    expect(CLIP_PRESETS.map((p) => p.seconds)).toEqual([15, 30, 60, 120, 300]);
    expect(CLIP_PRESETS.map((p) => p.label)).toEqual(['15 s', '30 s', '1 min', '2 min', '5 min']);
  });
});

describe('clipSecondsProblem', () => {
  it.each<[number, number, ReturnType<typeof clipSecondsProblem>]>([
    [7.5, 300, 'not_whole'],
    [NaN, 300, 'not_whole'],
    [4, 300, 'too_short'],
    [0, 300, 'too_short'],
    [-10, 300, 'too_short'],
    [5, 300, null],
    [30, 30, null],
    [31, 30, 'too_long'],
    [3601, 3600, 'too_long'],
    [3601, 99999, 'too_long'],
    [3600, 99999, null],
    [60, 3600, null],
    [60, 59, 'too_long'],
  ])('seconds %s with max %s -> %s', (seconds, max, expected) => {
    expect(clipSecondsProblem(seconds, max)).toBe(expected);
  });

  it('not_whole is checked before too_short (4.5 is not whole, not short)', () => {
    expect(clipSecondsProblem(4.5, 300)).toBe('not_whole');
  });

  it('a fractional value over the max is not_whole, not too_long', () => {
    expect(clipSecondsProblem(31.5, 30)).toBe('not_whole');
  });

  it('every CLIP_PRESETS length fits a 300 s buffer', () => {
    for (const p of CLIP_PRESETS) expect(clipSecondsProblem(p.seconds, 300)).toBeNull();
  });
});

describe('clipSecondsMessage', () => {
  it('is null when the length is fine', () => {
    expect(clipSecondsMessage(30, 300)).toBeNull();
    expect(clipSecondsMessage(5, 300)).toBeNull();
  });

  it('not_whole message asks for whole seconds', () => {
    expect(clipSecondsMessage(7.5, 300)).toBe('Enter whole seconds.');
  });

  it('too_short message names the 5 second minimum', () => {
    expect(clipSecondsMessage(4, 300)).toBe('A clip must be at least 5 seconds.');
  });

  it('too_long message mentions the number and the replay buffer', () => {
    const msg = clipSecondsMessage(31, 30);
    expect(msg).not.toBeNull();
    expect(msg).toContain('30');
    expect(msg).toContain('replay buffer');
  });

  it('too_long message reports the buffer length, not the requested length', () => {
    expect(clipSecondsMessage(600, 120)).toContain('(120 s)');
  });

  it.each([
    [4, 300],
    [7.5, 300],
    [301, 300],
  ])('gives a non-null message for %s with max %s', (seconds, max) => {
    expect(typeof clipSecondsMessage(seconds, max)).toBe('string');
  });
});

describe('suggestClipSeconds', () => {
  it.each<[number, number, number]>([
    [2, 300, 5],
    [5, 300, 5],
    [30, 300, 30],
    [30.4, 300, 30],
    [30.6, 300, 31],
    [600, 120, 120],
    [400, 3600, 400],
    [99999, 99999, 3600],
    [60, 30, 30],
    [NaN, 300, 5],
    [Infinity, 300, 5],
    [-Infinity, 300, 5],
  ])('seconds %s with max %s -> %s', (seconds, max, expected) => {
    expect(suggestClipSeconds(seconds, max)).toBe(expected);
  });

  it('a suggestion is always accepted by clipSecondsProblem', () => {
    for (const s of [0, 1, 4.4, 7.5, 31, 500, 9999, NaN]) {
      const out = suggestClipSeconds(s, 120);
      expect(clipSecondsProblem(out, 120)).toBeNull();
    }
  });
});

describe('clipsOverLimit', () => {
  it('is empty when every clip fits', () => {
    expect(clipsOverLimit(clips(3, 30), 300)).toEqual([]);
  });

  it('returns the indexes of clips over the buffer', () => {
    const list = [clip('a', 'F13', 30), clip('b', 'F14', 600), clip('c', 'F15', 10), clip('d', 'F16', 900)];
    expect(clipsOverLimit(list, 300)).toEqual([1, 3]);
  });

  it('returns indexes of clips under the minimum too', () => {
    expect(clipsOverLimit([clip('a', 'F13', 4), clip('b', 'F14', 5)], 300)).toEqual([0]);
  });

  it('flags a fractional length', () => {
    expect(clipsOverLimit([clip('a', 'F13', 7.5)], 300)).toEqual([0]);
  });

  it('a clip exactly at the buffer length is fine', () => {
    expect(clipsOverLimit([clip('a', 'F13', 120)], 120)).toEqual([]);
  });

  it('empty list gives empty result', () => {
    expect(clipsOverLimit([], 300)).toEqual([]);
  });
});

describe('canAddClip', () => {
  it.each([
    [0, true],
    [7, true],
    [8, false],
    [9, false],
  ])('%i clips -> %s', (n, expected) => {
    expect(canAddClip(clips(n))).toBe(expected);
  });
});

describe('nextClipName', () => {
  it('is Clip 1 for an empty list', () => {
    expect(nextClipName([])).toBe('Clip 1');
  });

  it('is one more than the count for a plain list', () => {
    expect(nextClipName(clips(3))).toBe('Clip 4');
  });

  it('skips a taken name, ignoring case', () => {
    expect(nextClipName([clip('clip 2'), clip('x'), clip('y')])).toBe('Clip 4');
  });

  it('skips several taken names in a row', () => {
    expect(nextClipName([clip('CLIP 3'), clip('Clip 4'), clip('z')])).toBe('Clip 5');
  });
});

describe('suggestNewChord', () => {
  it.each<[string[], string]>([
    [[], 'Alt+F9'],
    [['Alt+F9'], 'Alt+F10'],
    [['Alt+F9', 'Alt+F10'], 'Alt+F6'],
    [['Alt+F9', 'Alt+F10', 'Alt+F6'], 'Alt+F5'],
    [['Alt+F9', 'Alt+F10', 'Alt+F6', 'Alt+F5'], 'Alt+F3'],
    [['Alt+F9', 'Alt+F10', 'Alt+F6', 'Alt+F5', 'Alt+F3'], 'Alt+F2'],
    [['Alt+F9', 'Alt+F10', 'Alt+F6', 'Alt+F5', 'Alt+F3', 'Alt+F2'], 'Alt+F1'],
    [['Alt+F9', 'Alt+F10', 'Alt+F6', 'Alt+F5', 'Alt+F3', 'Alt+F2', 'Alt+F1'], 'F13'],
  ])('used %j -> %s', (used, expected) => {
    expect(suggestNewChord(used)).toBe(expected);
  });

  it('ignores case and modifier order in used chords', () => {
    expect(suggestNewChord(['alt+f9'])).toBe('Alt+F10');
    expect(suggestNewChord(['Alt+f9'])).toBe('Alt+F10');
  });

  it('skips a suggestion when the same chord is typed in another order', () => {
    expect(suggestNewChord(['shift+ctrl+f13'])).toBe('Alt+F9');
  });

  it('never suggests a reserved chord (Alt+F4 is not in the list anyway, F13 is not reserved)', () => {
    const out = suggestNewChord([]);
    expect(out).not.toBe('Alt+F4');
    expect(out).toBe('Alt+F9');
  });

  it('falls back to F13 when every candidate is used', () => {
    const all = ['Alt+F9', 'Alt+F10', 'Alt+F6', 'Alt+F5', 'Alt+F3', 'Alt+F2', 'Alt+F1'];
    expect(suggestNewChord(all)).toBe('F13');
  });
});

describe('addClip', () => {
  it('appends a clip named Clip N with the suggested chord and 30 s', () => {
    const out = addClip(clips(2));
    expect(out).toHaveLength(3);
    expect(out[2]).toEqual({ name: 'Clip 3', keys: 'Alt+F9', seconds: 30, press: 'tap' });
  });

  it('by default avoids chords already used by existing clips', () => {
    const list = [clip('Clip 1', 'Alt+F9'), clip('Clip 2', 'Alt+F10')];
    expect(addClip(list)[2].keys).toBe('Alt+F6');
  });

  it('uses an explicit usedChords list when given (for session hotkeys too)', () => {
    const list = [clip('Clip 1', 'Alt+F9')];
    expect(addClip(list, ['Alt+F9', 'Alt+F10'])[1].keys).toBe('Alt+F6');
  });

  it('an explicit empty usedChords ignores the clips chords', () => {
    const list = [clip('Clip 1', 'Alt+F9')];
    expect(addClip(list, [])[1].keys).toBe('Alt+F9');
  });

  it('at 8 clips returns a copy of length 8 and adds nothing', () => {
    const before = clips(8);
    const out = addClip(before);
    expect(out).toHaveLength(8);
    expect(out).toEqual(before);
    expect(out).not.toBe(before);
  });

  it('does not mutate the input', () => {
    const before = clips(2);
    const snapshot = structuredClone(before);
    addClip(before);
    expect(before).toEqual(snapshot);
  });
});

describe('removeClip', () => {
  it('removes the clip at the index', () => {
    expect(removeClip(clips(3), 1).map((c) => c.name)).toEqual(['Clip 1', 'Clip 3']);
  });

  it('removes the last clip', () => {
    expect(removeClip(clips(2), 1).map((c) => c.name)).toEqual(['Clip 1']);
  });

  it('removing from an empty list is a no-op', () => {
    expect(removeClip([], 0)).toEqual([]);
  });

  it.each([-1, 3, 50])('out of range index %i is a no-op', (index) => {
    expect(removeClip(clips(3), index)).toEqual(clips(3));
  });

  it('does not mutate the input', () => {
    const before = clips(2);
    removeClip(before, 0);
    expect(before).toHaveLength(2);
  });
});

describe('updateClip', () => {
  it('changes only the addressed clip', () => {
    const before = clips(3);
    const out = updateClip(before, 1, { seconds: 120 });
    expect(out[1]).toEqual({ ...before[1], seconds: 120 });
    expect(out[0]).toEqual(before[0]);
    expect(out[2]).toEqual(before[2]);
  });

  it('can change the name and chord together', () => {
    const out = updateClip(clips(1), 0, { name: 'Highlight', keys: 'Alt+F8' });
    expect(out[0]).toEqual({ name: 'Highlight', keys: 'Alt+F8', seconds: 30, press: 'tap' });
  });

  it('out-of-range index changes nothing', () => {
    const before = clips(2);
    expect(updateClip(before, 9, { seconds: 5 })).toEqual(before);
  });

  it('does not mutate the input', () => {
    const before = clips(1);
    updateClip(before, 0, { seconds: 999 });
    expect(before[0].seconds).toBe(30);
  });
});

describe('clipCountText', () => {
  it.each([
    [0, '0 of 8 clip hotkeys'],
    [3, '3 of 8 clip hotkeys'],
    [8, '8 of 8 clip hotkeys'],
  ])('%s -> %s', (n, expected) => {
    expect(clipCountText(n)).toBe(expected);
  });
});
