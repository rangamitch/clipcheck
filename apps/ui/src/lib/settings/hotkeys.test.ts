import { describe, expect, it } from 'vitest';
import {
  SUPPORTED_KEYS,
  canonicalizeChord,
  chordProblem,
  eventToChord,
  findDuplicateChords,
  formatChord,
  heldModifiersLabel,
  isBareAllowed,
  isReservedChord,
  keyFromCode,
  keyFromKey,
  modifiersOf,
  parseChord,
  type ChordEvent,
} from './hotkeys';

const NONE = { ctrl: false, alt: false, shift: false, win: false };

type Mods = Partial<Pick<ChordEvent, 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey'>>;

/** Builds a KeyboardEvent-like object. Nothing is held unless passed in `mods`. */
function ev(code: string, key: string, mods: Mods = {}, altGraph?: boolean): ChordEvent {
  return {
    code,
    key,
    ctrlKey: mods.ctrlKey ?? false,
    altKey: mods.altKey ?? false,
    shiftKey: mods.shiftKey ?? false,
    metaKey: mods.metaKey ?? false,
    ...(altGraph === undefined ? {} : { getModifierState: (n: string) => n === 'AltGraph' && altGraph }),
  };
}

const CTRL: Mods = { ctrlKey: true };
const ALT: Mods = { altKey: true };
const SHIFT: Mods = { shiftKey: true };
const WIN: Mods = { metaKey: true };

describe('SUPPORTED_KEYS', () => {
  it('has every key name in the contract table (125 keys)', () => {
    expect(SUPPORTED_KEYS).toHaveLength(125);
  });

  it.each(['A', 'Z', '0', '9', 'F1', 'F24', 'Numpad5', 'Space', 'Pause', 'AppsKey', 'Up'])('includes %s', (key) => {
    expect(SUPPORTED_KEYS).toContain(key);
  });

  it.each(['F0', 'F25', 'Escape', 'Menu', 'Mouse1'])('excludes %s', (key) => {
    expect(SUPPORTED_KEYS).not.toContain(key);
  });
});

describe('isBareAllowed', () => {
  it.each([
    ['F9', true],
    ['F12', true],
    ['F13', true],
    ['F24', true],
    ['F25', false],
    ['A', true],
    ['1', true],
    ['Numpad5', true],
    ['Space', true],
    ['Escape', false],
    ['', false],
  ])('%s -> %s', (key, expected) => {
    expect(isBareAllowed(key)).toBe(expected);
  });
});

describe('keyFromCode', () => {
  it.each([
    ['KeyA', 'A'],
    ['KeyZ', 'Z'],
    ['Digit5', '5'],
    ['Digit0', '0'],
    ['F1', 'F1'],
    ['F12', 'F12'],
    ['F13', 'F13'],
    ['F24', 'F24'],
    ['F25', null],
    ['F0', null],
    ['Numpad5', 'Numpad5'],
    ['NumpadEnter', 'Enter'],
    ['NumpadAdd', 'NumpadAdd'],
    ['ArrowLeft', 'Left'],
    ['ArrowUp', 'Up'],
    ['Space', 'Space'],
    ['Enter', 'Enter'],
    ['Escape', null],
    ['', null],
  ])('%s -> %s', (code, expected) => {
    expect(keyFromCode(code)).toBe(expected);
  });
});

describe('keyFromKey', () => {
  it.each([
    ['a', 'A'],
    ['Z', 'Z'],
    ['7', '7'],
    ['f9', 'F9'],
    ['F9', 'F9'],
    ['F24', 'F24'],
    ['F25', null],
    ['Enter', 'Enter'],
    [' ', 'Space'],
    ['Escape', null],
    ['ab', null],
    ['', null],
  ])('%s -> %s', (key, expected) => {
    expect(keyFromKey(key)).toBe(expected);
  });
});

describe('modifiersOf', () => {
  it('reads all four modifier flags', () => {
    expect(modifiersOf(ev('KeyA', 'a', { ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }))).toEqual({
      ctrl: true,
      alt: true,
      shift: true,
      win: true,
    });
  });

  it('reports none when nothing is held', () => {
    expect(modifiersOf(ev('KeyA', 'a'))).toEqual(NONE);
  });

  it('drops Ctrl and Alt when AltGraph is active', () => {
    expect(modifiersOf(ev('KeyQ', 'q', { ctrlKey: true, altKey: true, shiftKey: true }, true))).toEqual({
      ctrl: false,
      alt: false,
      shift: true,
      win: false,
    });
  });

  it('keeps Ctrl and Alt when AltGraph is reported false', () => {
    expect(modifiersOf(ev('KeyQ', 'q', { ctrlKey: true, altKey: true }, false))).toEqual({
      ctrl: true,
      alt: true,
      shift: false,
      win: false,
    });
  });

  it('keeps Win when AltGraph is active', () => {
    const mods = modifiersOf(ev('KeyQ', 'q', { ctrlKey: true, metaKey: true }, true));
    expect(mods.win).toBe(true);
    expect(mods.ctrl).toBe(false);
  });
});

describe('formatChord', () => {
  it.each([
    ['no modifiers', NONE, 'F13', 'F13'],
    ['Alt+F9', { ...NONE, alt: true }, 'F9', 'Alt+F9'],
    ['all four', { ctrl: true, alt: true, shift: true, win: true }, 'A', 'Ctrl+Alt+Shift+Win+A'],
    ['Shift+Win+S', { ...NONE, win: true, shift: true }, 'S', 'Shift+Win+S'],
    ['Win+Alt+R goes Alt before Win', { ...NONE, win: true, alt: true }, 'R', 'Alt+Win+R'],
    ['Ctrl+Shift+1', { ...NONE, shift: true, ctrl: true }, '1', 'Ctrl+Shift+1'],
  ])('%s', (_label, mods, key, expected) => {
    expect(formatChord(mods, key)).toBe(expected);
  });
});

describe('heldModifiersLabel', () => {
  it.each([
    [NONE, ''],
    [{ ...NONE, ctrl: true }, 'Ctrl'],
    [{ ...NONE, ctrl: true, shift: true }, 'Ctrl + Shift'],
    [{ ctrl: true, alt: true, shift: true, win: true }, 'Ctrl + Alt + Shift + Win'],
    [{ ...NONE, win: true }, 'Win'],
    [{ ...NONE, alt: true, win: true }, 'Alt + Win'],
  ])('%j -> %j', (mods, expected) => {
    expect(heldModifiersLabel(mods)).toBe(expected);
  });
});

describe('eventToChord: accepted chords', () => {
  it.each<[string, ChordEvent, string]>([
    ['Ctrl+Shift+KeyA', ev('KeyA', 'a', { ctrlKey: true, shiftKey: true }), 'Ctrl+Shift+A'],
    ['Ctrl+Shift+KeyA with uppercase key', ev('KeyA', 'A', { shiftKey: true, ctrlKey: true }), 'Ctrl+Shift+A'],
    ['Alt+F9', ev('F9', 'F9', ALT), 'Alt+F9'],
    ['Ctrl+Alt+KeyK', ev('KeyK', 'k', { ctrlKey: true, altKey: true }), 'Ctrl+Alt+K'],
    ['Shift+Digit1 gives the physical digit', ev('Digit1', '!', SHIFT), 'Shift+1'],
    ['Ctrl+Digit0', ev('Digit0', '0', CTRL), 'Ctrl+0'],
    ['Win+KeyM', ev('KeyM', 'm', WIN), 'Win+M'],
    ['Win+Alt+KeyK', ev('KeyK', 'k', { metaKey: true, altKey: true }), 'Alt+Win+K'],
    ['all four modifiers', ev('KeyJ', 'j', { ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }), 'Ctrl+Alt+Shift+Win+J'],
    ['bare F13', ev('F13', 'F13'), 'F13'],
    ['bare F18', ev('F18', 'F18'), 'F18'],
    ['bare F24', ev('F24', 'F24'), 'F24'],
    ['Ctrl+F12', ev('F12', 'F12', CTRL), 'Ctrl+F12'],
    ['Shift+F24', ev('F24', 'F24', SHIFT), 'Shift+F24'],
    ['Ctrl+Digit9 with key (', ev('Digit9', '(', CTRL), 'Ctrl+9'],
    ['Shift+KeyA', ev('KeyA', 'A', SHIFT), 'Shift+A'],
    ['Ctrl+Numpad5 (code Numpad5, key ArrowLeft, NumLock off)', ev('Numpad5', 'ArrowLeft', CTRL), 'Ctrl+Numpad5'],
    ['Ctrl+Numpad0', ev('Numpad0', '0', CTRL), 'Ctrl+Numpad0'],
    ['Ctrl+NumpadAdd', ev('NumpadAdd', '+', CTRL), 'Ctrl+NumpadAdd'],
    ['Ctrl+ArrowLeft reads as Ctrl+Left', ev('ArrowLeft', 'ArrowLeft', CTRL), 'Ctrl+Left'],
    ['Ctrl+Space', ev('Space', ' ', CTRL), 'Ctrl+Space'],
    ['Alt+Tab (accepted here; reserved is checked by chordProblem)', ev('Tab', 'Tab', ALT), 'Alt+Tab'],
  ])('%s', (_label, event, chord) => {
    expect(eventToChord(event)).toEqual({ ok: true, chord });
  });
});

describe('eventToChord: bare keys are accepted (no modifier needed)', () => {
  it.each<[string, ChordEvent, string]>([
    ['bare F9', ev('F9', 'F9'), 'F9'],
    ['bare F12', ev('F12', 'F12'), 'F12'],
    ['bare KeyA', ev('KeyA', 'a'), 'A'],
    ['bare Digit5', ev('Digit5', '5'), '5'],
    ['bare Numpad5', ev('Numpad5', 'ArrowLeft'), 'Numpad5'],
    ['bare ArrowUp', ev('ArrowUp', 'ArrowUp'), 'Up'],
    ['bare Enter', ev('Enter', 'Enter'), 'Enter'],
    ['bare Space', ev('Space', ' '), 'Space'],
    ['bare Pause', ev('Pause', 'Pause'), 'Pause'],
  ])('%s', (_label, event, chord) => {
    expect(eventToChord(event)).toEqual({ ok: true, chord });
  });
});

describe('eventToChord: modifier-only presses', () => {
  it.each([
    ['Control', 'ControlLeft'],
    ['Shift', 'ShiftLeft'],
    ['Alt', 'AltLeft'],
    ['Meta', 'MetaLeft'],
    ['AltGraph', 'AltRight'],
    ['OS', 'OSLeft'],
  ])('key %s gives modifier_only', (key, code) => {
    expect(eventToChord(ev(code, key))).toEqual({ ok: false, reason: 'modifier_only' });
  });

  it('Ctrl held then Shift pressed is modifier_only', () => {
    expect(eventToChord(ev('ShiftLeft', 'Shift', { ctrlKey: true, shiftKey: true }))).toEqual({
      ok: false,
      reason: 'modifier_only',
    });
  });

  it('modifier_only wins even when the code is a letter', () => {
    expect(eventToChord(ev('KeyA', 'Control', CTRL))).toEqual({ ok: false, reason: 'modifier_only' });
  });
});

describe('eventToChord: refused keys', () => {
  it.each<[string, ChordEvent]>([
    ['Escape with Ctrl (Escape cancels capture, never bound)', ev('Escape', 'Escape', CTRL)],
    ['Escape bare', ev('Escape', 'Escape')],
    ['F25 out of range', ev('F25', 'F25', CTRL)],
    ['F25 bare', ev('F25', 'F25')],
  ])('%s -> unsupported_key', (_label, event) => {
    const result = eventToChord(event);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe('unsupported_key');
  });

  it('Escape is refused as unsupported_key (no key field, issue_67)', () => {
    expect(eventToChord(ev('Escape', 'Escape', CTRL))).toEqual({
      ok: false,
      reason: 'unsupported_key',
    });
  });

  it('Escape with an empty code is refused as unsupported_key', () => {
    expect(eventToChord(ev('', 'Escape', CTRL))).toEqual({
      ok: false,
      reason: 'unsupported_key',
    });
  });

  it('numpad 5 with NumLock on (code Numpad5, key "5") is Numpad5, not Digit 5', () => {
    expect(eventToChord(ev('Numpad5', '5', CTRL))).toEqual({
      ok: true,
      chord: 'Ctrl+Numpad5',
    });
  });
});

describe('eventToChord: empty code falls back to key', () => {
  it.each<[string, ChordEvent, string]>([
    ['empty code, key a, Ctrl', ev('', 'a', CTRL), 'Ctrl+A'],
    ['empty code, key 7, Alt', ev('', '7', ALT), 'Alt+7'],
    ['empty code, key f9, Ctrl+Shift', ev('', 'f9', { ctrlKey: true, shiftKey: true }), 'Ctrl+Shift+F9'],
    ['empty code, key F15 bare', ev('', 'F15'), 'F15'],
  ])('%s', (_label, event, chord) => {
    expect(eventToChord(event)).toEqual({ ok: true, chord });
  });

  it('empty code, key F9 bare is accepted as F9', () => {
    expect(eventToChord(ev('', 'F9'))).toEqual({ ok: true, chord: 'F9' });
  });
});

describe('eventToChord: AltGr quirk', () => {
  it('Ctrl+Alt+KeyQ with AltGraph reads as bare Q (bare keys are accepted)', () => {
    expect(eventToChord(ev('KeyQ', 'q', { ctrlKey: true, altKey: true }, true))).toEqual({
      ok: true,
      chord: 'Q',
    });
  });

  it('AltGraph with Ctrl+Alt+Shift keeps Shift only', () => {
    expect(eventToChord(ev('KeyQ', 'Q', { ctrlKey: true, altKey: true, shiftKey: true }, true))).toEqual({
      ok: true,
      chord: 'Shift+Q',
    });
  });

  it('AltGraph false keeps Ctrl+Alt', () => {
    expect(eventToChord(ev('KeyQ', 'q', { ctrlKey: true, altKey: true }, false))).toEqual({
      ok: true,
      chord: 'Ctrl+Alt+Q',
    });
  });

  it('AltGraph with Win keeps Win', () => {
    expect(eventToChord(ev('KeyQ', 'q', { ctrlKey: true, altKey: true, metaKey: true }, true))).toEqual({
      ok: true,
      chord: 'Win+Q',
    });
  });
});

describe('parseChord', () => {
  it.each<[string, { mods: typeof NONE; key: string } | null]>([
    ['F9', { mods: NONE, key: 'F9' }],
    ['alt+f9', { mods: { ...NONE, alt: true }, key: 'F9' }],
    ['ALT + F9', { mods: { ...NONE, alt: true }, key: 'F9' }],
    ['shift+ctrl+a', { mods: { ...NONE, ctrl: true, shift: true }, key: 'A' }],
    ['win+alt+r', { mods: { ...NONE, alt: true, win: true }, key: 'R' }],
    ['Alt+Alt+F9', null],
    ['Foo+F9', null],
    ['Alt+', null],
    ['+F9', null],
    ['Alt+F25', null],
    ['Alt+Escape', null],
    ['Alt+Mouse1', null],
    ['', null],
  ])('%s', (text, expected) => {
    expect(parseChord(text)).toEqual(expected);
  });

  it('upper-cases the key', () => {
    expect(parseChord('ctrl+k')?.key).toBe('K');
  });

  it('reads Numpad5 as a key name (not Digit 5)', () => {
    expect(parseChord('Alt+Numpad5')).toEqual({ mods: { ...NONE, alt: true }, key: 'Numpad5' });
  });

  it('reads the alias Menu as AppsKey', () => {
    expect(parseChord('Ctrl+Menu')).toEqual({ mods: { ...NONE, ctrl: true }, key: 'AppsKey' });
  });
});

describe('canonicalizeChord', () => {
  it.each<[string, string | null]>([
    ['shift+alt+f9', 'Alt+Shift+F9'],
    ['f9', 'F9'],
    ['ctrl+win+alt+shift+a', 'Ctrl+Alt+Shift+Win+A'],
    ['Win+Ctrl+1', 'Ctrl+Win+1'],
    ['  alt +  f9 ', 'Alt+F9'],
    ['', null],
    ['Foo+F9', null],
    ['Alt+Alt+F9', null],
    ['Alt+Numpad5', 'Alt+Numpad5'],
    ['ctrl+menu', 'Ctrl+AppsKey'],
    ['shift+space', 'Shift+Space'],
    ['Alt+Escape', null],
  ])('%s -> %s', (text, expected) => {
    expect(canonicalizeChord(text)).toBe(expected);
  });
});

describe('isReservedChord', () => {
  it.each<[string, string | null]>([
    ['Alt+F4', 'Alt+F4 (close window)'],
    ['alt+f4', 'Alt+F4 (close window)'],
    ['Win+L', 'Win+L (lock workstation)'],
    ['Win+G', 'Win+G (Xbox Game Bar)'],
    ['Win+Alt+R', 'Win+Alt+R (Game DVR)'],
    ['Win+Alt+G', 'Win+Alt+G (Game DVR)'],
    ['Win+Shift+S', 'Win+Shift+S (screenshot)'],
    ['Ctrl+Alt+Delete', 'Ctrl+Alt+Delete (secure attention sequence)'],
    ['Ctrl+Shift+Escape', 'Ctrl+Shift+Esc (Task Manager)'],
    ['Alt+F9', null],
    ['Win+Alt+L', null],
    ['Alt+F13', null],
    ['Win+F4', null],
    ['Win+Alt+Shift+G', null],
    ['F13', null],
    ['Foo', null],
  ])('%s -> %s', (text, expected) => {
    expect(isReservedChord(text)).toBe(expected);
  });
});

describe('chordProblem', () => {
  it.each<[string, 'reserved' | 'invalid' | null]>([
    ['F9', null],
    ['A', null],
    ['Numpad5', null],
    ['Space', null],
    ['F13', null],
    ['F24', null],
    ['Alt+F9', null],
    ['Alt+Numpad5', null],
    ['Alt+F4', 'reserved'],
    ['Alt+Space', 'reserved'],
    ['Win+L', 'reserved'],
    ['Foo', 'invalid'],
    ['', 'invalid'],
    ['Escape', 'invalid'],
    ['Alt+Escape', 'invalid'],
  ])('%s -> %s', (text, expected) => {
    expect(chordProblem(text)).toBe(expected);
  });
});

describe('findDuplicateChords', () => {
  it('returns nothing for an empty list', () => {
    expect(findDuplicateChords([])).toEqual([]);
  });

  it('returns nothing when all chords differ', () => {
    expect(
      findDuplicateChords([
        { label: 'Split', chord: 'Alt+F9' },
        { label: 'Stop', chord: 'Alt+F10' },
      ]),
    ).toEqual([]);
  });

  it('groups case differences as duplicates and ignores a lone chord', () => {
    expect(
      findDuplicateChords([
        { label: 'Split', chord: 'alt+f9' },
        { label: 'Clip 1', chord: 'Alt+F9' },
        { label: 'Mute', chord: 'F9' },
      ]),
    ).toEqual([{ chord: 'Alt+F9', labels: ['Split', 'Clip 1'] }]);
  });

  it('matches shift+ctrl against Ctrl+Shift', () => {
    expect(
      findDuplicateChords([
        { label: 'A', chord: 'shift+ctrl+f13' },
        { label: 'B', chord: 'Ctrl+Shift+F13' },
      ]),
    ).toEqual([{ chord: 'Ctrl+Shift+F13', labels: ['A', 'B'] }]);
  });

  it('ignores blank and invalid chords', () => {
    expect(
      findDuplicateChords([
        { label: 'x', chord: '' },
        { label: 'y', chord: '' },
        { label: 'z', chord: 'Foo+F9' },
        { label: 'w', chord: 'Foo+F9' },
      ]),
    ).toEqual([]);
  });

  it('returns one group per duplicated chord, in first-seen order', () => {
    expect(
      findDuplicateChords([
        { label: 'a', chord: 'Alt+F9' },
        { label: 'b', chord: 'F13' },
        { label: 'c', chord: 'alt+f9' },
        { label: 'd', chord: 'f13' },
        { label: 'e', chord: 'Alt+F10' },
      ]),
    ).toEqual([
      { chord: 'Alt+F9', labels: ['a', 'c'] },
      { chord: 'F13', labels: ['b', 'd'] },
    ]);
  });

  it('reports three-way duplicates with all labels', () => {
    expect(
      findDuplicateChords([
        { label: 'one', chord: 'Win+M' },
        { label: 'two', chord: 'win+m' },
        { label: 'three', chord: 'WIN+M' },
      ]),
    ).toEqual([{ chord: 'Win+M', labels: ['one', 'two', 'three'] }]);
  });

  it('whitespace-only chords are ignored, so they never pair up', () => {
    expect(
      findDuplicateChords([
        { label: 'x', chord: '   ' },
        { label: 'y', chord: '   ' },
        { label: 'z', chord: 'F13' },
      ]),
    ).toEqual([]);
  });
});
