import { describe, expect, it } from 'vitest';
import {
  canAddPushToTalkKey,
  canonicalizeChord,
  chordLabel,
  chordProblem,
  findDuplicateChords,
  formatChord,
  MAX_PUSH_TO_TALK_KEYS,
  modifiersOf,
  MOUSE_KEY_NAMES,
  mouseButtonKey,
  parseChord,
  sessionChordsOf,
  SUPPORTED_KEYS,
  type ChordEntry,
} from './hotkeys';
import type { SessionHotkeysConfig } from './store';

const NONE = { ctrl: false, alt: false, shift: false, win: false };

describe('mouse key names', () => {
  it('are Mouse3, Mouse4 and Mouse5 only', () => {
    expect(MOUSE_KEY_NAMES).toEqual(['Mouse3', 'Mouse4', 'Mouse5']);
  });

  it('are kept out of SUPPORTED_KEYS, which stays the keyboard contract table', () => {
    for (const name of MOUSE_KEY_NAMES) expect(SUPPORTED_KEYS).not.toContain(name);
  });
});

describe('mouseButtonKey', () => {
  it.each<[number, string | null]>([
    [1, 'Mouse3'],
    [3, 'Mouse4'],
    [4, 'Mouse5'],
    [0, null],
    [2, null],
    [5, null],
    [-1, null],
  ])('button %i -> %s', (button, expected) => {
    expect(mouseButtonKey(button)).toBe(expected);
  });
});

describe('mouse chords parse', () => {
  it.each<[string, { mods: typeof NONE; key: string } | null]>([
    ['Mouse4', { mods: NONE, key: 'Mouse4' }],
    ['mouse5', { mods: NONE, key: 'Mouse5' }],
    ['Ctrl+Mouse5', { mods: { ...NONE, ctrl: true }, key: 'Mouse5' }],
    ['Mouse1', null],
    ['Mouse2', null],
    ['Alt+Mouse1', null],
    ['Mouse6', null],
  ])('%s', (text, expected) => {
    expect(parseChord(text)).toEqual(expected);
  });

  it('canonicalizes a mouse chord in any case', () => {
    expect(canonicalizeChord('ctrl+mouse4')).toBe('Ctrl+Mouse4');
  });
});

describe('chordProblem for mouse keys', () => {
  it.each<[string, 'needs_modifier' | 'reserved' | 'invalid' | null]>([
    ['Mouse4', null],
    ['Mouse3', null],
    ['Ctrl+Mouse5', null],
    ['Mouse1', 'invalid'],
    ['Mouse2', 'invalid'],
  ])('%s -> %s', (text, expected) => {
    expect(chordProblem(text)).toBe(expected);
  });
});

describe('mouse chord labels', () => {
  it.each<[string, string]>([
    ['Mouse3', 'Middle mouse button'],
    ['Mouse4', 'Mouse button 4 (back)'],
    ['Mouse5', 'Mouse button 5 (forward)'],
    ['Ctrl+Mouse4', 'Ctrl+Mouse button 4 (back)'],
    ['Shift+Mouse5', 'Shift+Mouse button 5 (forward)'],
  ])('%s reads as %s', (chord, label) => {
    expect(chordLabel(chord)).toBe(label);
  });
});

describe('modifiers on a mouse event', () => {
  it('a Ctrl-held side button forms Ctrl+Mouse5', () => {
    const mods = modifiersOf({ ctrlKey: true, altKey: false, shiftKey: false, metaKey: false });
    expect(formatChord(mods, 'Mouse5')).toBe('Ctrl+Mouse5');
  });
});

describe('push-to-talk key cap', () => {
  it('the cap is 4, as CFG-H009 allows', () => {
    expect(MAX_PUSH_TO_TALK_KEYS).toBe(4);
  });

  it.each<[number, boolean]>([
    [0, true],
    [1, true],
    [3, true],
    [4, false],
    [5, false],
  ])('%i keys -> can add %s', (count, expected) => {
    expect(canAddPushToTalkKey(count)).toBe(expected);
  });
});

describe('push-to-talk chords count as session chords', () => {
  const session = {
    split: 'Alt+F11',
    stop_start: '',
    toggle_buffer: 'Alt+F8',
    mute_mic: 'Alt+F7',
    mute_mic_mode: 'toggle',
    push_to_talk: ['Mouse4', 'Ctrl+Space'],
  } as SessionHotkeysConfig;

  it('lists each PTT key after the session keys', () => {
    expect(sessionChordsOf(session)).toEqual(['Alt+F11', 'Alt+F8', 'Alt+F7', 'Mouse4', 'Ctrl+Space']);
  });

  it('leaves out an empty PTT entry and tolerates a config without the field', () => {
    expect(sessionChordsOf({ ...session, push_to_talk: [''] })).toEqual(['Alt+F11', 'Alt+F8', 'Alt+F7']);
    const older = { ...session } as Partial<SessionHotkeysConfig>;
    delete older.push_to_talk;
    expect(sessionChordsOf(older as SessionHotkeysConfig)).toEqual(['Alt+F11', 'Alt+F8', 'Alt+F7']);
  });
});

describe('push-to-talk duplicates', () => {
  const clip = (chord: string): ChordEntry => ({ label: '"Clip 1" (tap)', chord, press: 'tap' });

  it('flags a PTT key that clashes with a clip hotkey', () => {
    const dups = findDuplicateChords([clip('Mouse4'), { label: 'push to talk key 1', chord: 'Mouse4' }]);
    expect(dups).toEqual([{ chord: 'Mouse4', labels: ['"Clip 1" (tap)', 'push to talk key 1'] }]);
  });

  it('flags a PTT key that clashes with a session hotkey, in either spelling', () => {
    const dups = findDuplicateChords([
      { label: 'mute or unmute the microphone (toggle)', chord: 'Alt+F7' },
      { label: 'push to talk key 1', chord: 'alt+f7' },
    ]);
    expect(dups.map((d) => d.chord)).toEqual(['Alt+F7']);
  });

  it('flags two PTT keys with the same chord', () => {
    const dups = findDuplicateChords([
      { label: 'push to talk key 1', chord: 'Ctrl+Mouse5' },
      { label: 'push to talk key 2', chord: 'Ctrl+Mouse5' },
    ]);
    expect(dups).toEqual([{ chord: 'Ctrl+Mouse5', labels: ['push to talk key 1', 'push to talk key 2'] }]);
  });

  it('does not flag distinct PTT keys', () => {
    expect(
      findDuplicateChords([
        { label: 'push to talk key 1', chord: 'Mouse4' },
        { label: 'push to talk key 2', chord: 'Mouse5' },
      ]),
    ).toEqual([]);
  });
});
