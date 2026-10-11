import { describe, expect, it } from 'vitest';
import {
  ALREADY_DETECTED,
  addBlocked,
  addGame,
  displayNameFor,
  gameRowsFor,
  isDisplayNameOnly,
  pickerAppsFor,
  removeBlocked,
  removeGame,
  safeDisplayName,
  type GameCandidate,
  type GamesSlice,
} from './games';

const app = (exe: string, extra: Partial<GameCandidate> = {}): GameCandidate => ({
  exe,
  name: exe.replace(/\.exe$/i, ''),
  iconB64: null,
  isGame: false,
  suggested: false,
  ...extra,
});

describe('mixed-case exes', () => {
  it('addGame refuses an exe already in allow when the stored case differs', () => {
    const slice: GamesSlice = { allow: ['CS2.exe'], block: [], games: {} };
    const r = addGame(slice, 'cs2.EXE', 'X');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toMatch(/already in your games/);
  });

  it('addGame refuses an exe in block when the stored case differs', () => {
    const slice: GamesSlice = { allow: [], block: ['Launcher.EXE'], games: {} };
    const r = addGame(slice, 'LAUNCHER.exe', 'Launcher');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toMatch(/Never record/);
  });

  it('addBlocked refuses a duplicate when the stored case differs', () => {
    const slice: GamesSlice = { allow: [], block: ['Launcher.Exe'], games: {} };
    const r = addBlocked(slice, 'LAUNCHER.EXE');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toMatch(/already in Never record/);
  });

  it('addGame reuses a games key stored in another case and keeps its override', () => {
    const slice: GamesSlice = {
      allow: [],
      block: [],
      games: { 'Cs2.Exe': { display_name: 'My CS', folder: 'D:\\clips' } },
    };
    const r = addGame(slice, 'CS2.exe', 'Counter-Strike 2');
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.value.allow).toEqual(['cs2.exe']);
      expect(Object.keys(r.value.games)).toEqual(['Cs2.Exe']);
      expect(r.value.games['Cs2.Exe']).toEqual({ display_name: 'My CS', folder: 'D:\\clips' });
    }
  });

  it('addGame fills a blank display_name under the existing key in another case', () => {
    const slice: GamesSlice = { allow: [], block: [], games: { 'CS2.exe': { display_name: '' } } };
    const r = addGame(slice, 'cs2.exe', 'Counter-Strike 2');
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(Object.keys(r.value.games)).toEqual(['CS2.exe']);
      expect(r.value.games['CS2.exe']).toEqual({ display_name: 'Counter-Strike 2' });
    }
  });

  it('removeGame removes a display-name-only entry stored in another case', () => {
    const slice: GamesSlice = { allow: ['cs2.exe'], block: [], games: { 'Cs2.EXE': { display_name: 'CS' } } };
    expect(removeGame(slice, 'CS2.exe')).toEqual({ allow: [], block: [], games: {} });
  });

  it('removeGame keeps a richer entry stored in another case', () => {
    const games = { 'Cs2.EXE': { display_name: 'CS', auto_record: false } };
    const slice: GamesSlice = { allow: ['CS2.exe'], block: [], games };
    expect(removeGame(slice, 'cs2.exe')).toEqual({ allow: [], block: [], games });
  });

  it('removeBlocked matches a mixed-case entry', () => {
    const slice: GamesSlice = { allow: [], block: ['A.exe', 'B.Exe'], games: {} };
    expect(removeBlocked(slice, 'b.EXE').block).toEqual(['A.exe']);
  });

  it('gameRowsFor finds a games key in another case than the allow entry', () => {
    const rows = gameRowsFor(['CS2.EXE'], { 'cs2.exe': { display_name: 'CS' } }, []);
    expect(rows).toEqual([{ exe: 'CS2.EXE', name: 'CS', iconB64: null, running: false }]);
  });
});

describe('removeGame override shapes', () => {
  it.each([
    ['display_name plus null folder and undefined video', { display_name: 'X', folder: null, video: undefined }, true],
    ['display_name plus null video and null folder', { display_name: 'X', video: null, folder: null }, true],
    ['only null keys and no display_name', { folder: null, video: null }, true],
    ['display_name plus auto_record false (a real value)', { display_name: 'X', auto_record: false }, false],
    ['display_name plus replay_enabled true', { display_name: 'X', replay_enabled: true }, false],
    ['display_name plus a folder string', { display_name: 'X', folder: 'D:\\clips' }, false],
    ['display_name plus a video object', { display_name: 'X', video: { preset: 'high' } }, false],
  ])('%s keeps the entry only when a non-null key is present', (_label, entry, removed) => {
    const slice: GamesSlice = { allow: ['cs2.exe'], block: [], games: { 'cs2.exe': entry } };
    const out = removeGame(slice, 'cs2.exe');
    expect(out.allow).toEqual([]);
    expect(out.games['cs2.exe'] === undefined).toBe(removed);
    expect(isDisplayNameOnly(entry)).toBe(removed);
  });
});

describe('safeDisplayName boundaries', () => {
  it('keeps a name of exactly 64 characters', () => {
    const name = 'a'.repeat(64);
    expect(safeDisplayName(name, 'x.exe')).toBe(name);
  });

  it('cuts a name of 65 characters to its first 64', () => {
    expect(safeDisplayName('a'.repeat(64) + 'b', 'x.exe')).toBe('a'.repeat(64));
  });

  it('keeps 64 emoji (64 code points, 128 UTF-16 units)', () => {
    const name = '🎮'.repeat(64);
    expect(safeDisplayName(name, 'x.exe')).toBe(name);
  });

  it('cuts 65 emoji to 64 whole emoji', () => {
    expect(safeDisplayName('🎮'.repeat(65), 'x.exe')).toBe('🎮'.repeat(64));
  });

  it('keeps a surrogate pair that starts at index 63 whole', () => {
    const got = safeDisplayName('a'.repeat(63) + '🎮' + 'bcd', 'x.exe');
    expect(got).toBe('a'.repeat(63) + '🎮');
    expect(got).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
  });

  it('drops an emoji that would start at index 64', () => {
    const got = safeDisplayName('a'.repeat(64) + '🎮', 'x.exe');
    expect(got).toBe('a'.repeat(64));
    expect(got).not.toMatch(/[\uD800-\uDBFF]/);
  });

  it('trims a space left at the 64 boundary', () => {
    expect(safeDisplayName('a'.repeat(63) + ' b', 'x.exe')).toBe('a'.repeat(63));
  });

  it('trims surrounding spaces before counting 64 characters', () => {
    expect(safeDisplayName('  ' + 'a'.repeat(64) + '  ', 'x.exe')).toBe('a'.repeat(64));
  });

  it.each([
    ['spaces, tab and newline', '   \t\n '],
    ['ideographic spaces', '\u3000\u3000'],
  ])('whitespace-only input (%s) falls back to the stem', (_label, name) => {
    expect(safeDisplayName(name, 'x.exe')).toBe('x');
    expect(safeDisplayName(name, 'C:\\Games\\Bar.EXE')).toBe('Bar');
  });
});

describe('pickerAppsFor edge cases', () => {
  it('excludes a running app whose exe is in block in another case, even when allow is empty', () => {
    const got = pickerAppsFor([app('x.exe'), app('y.exe')], [], ['X.EXE']).map((p) => p.exe);
    expect(got).toEqual(['y.exe']);
  });

  it('places a suggested and isGame app after enabled suggested apps', () => {
    const running = [
      app('plain-game.exe', { isGame: true }),
      app('sugg-game.exe', { suggested: true, isGame: true }),
      app('sugg-plain.exe', { suggested: true }),
      app('plain.exe'),
    ];
    const got = pickerAppsFor(running, [], []);
    expect(got.map((p) => p.exe)).toEqual([
      'sugg-plain.exe',
      'sugg-game.exe',
      'plain.exe',
      'plain-game.exe',
    ]);
    expect(got[1]).toMatchObject({ suggested: true, disabled: true, badge: ALREADY_DETECTED });
  });

  it('does not mutate the running, allow or block inputs', () => {
    const running = [
      app('a.exe', { suggested: true, isGame: true, iconB64: 'AA' }),
      app('b.exe'),
      app('c.exe', { isGame: true }),
    ];
    const allow = ['Z.exe'];
    const block = ['Q.exe'];
    const before = structuredClone({ running, allow, block });
    pickerAppsFor(running, allow, block);
    expect({ running, allow, block }).toStrictEqual(before);
  });

  it('returns new objects, so editing an output does not change the input', () => {
    const running = [app('a.exe', { name: 'Alpha' })];
    const out = pickerAppsFor(running, [], []);
    out[0].name = 'changed';
    expect(running[0].name).toBe('Alpha');
  });
});

describe('gameRowsFor edge cases', () => {
  it('marks a game not running with null icon, even when another running app has an icon', () => {
    const running = [app('other.exe', { iconB64: 'OTHER' }), app('cs2.exe', { iconB64: null })];
    const rows = gameRowsFor(['elden.exe', 'cs2.exe'], {}, running);
    expect(rows).toEqual([
      { exe: 'elden.exe', name: 'elden', iconB64: null, running: false },
      { exe: 'cs2.exe', name: 'cs2', iconB64: null, running: true },
    ]);
  });

  it('keeps allow order exactly, including case', () => {
    const allow = ['z.exe', 'M.EXE', 'a.exe'];
    const rows = gameRowsFor(allow, {}, [app('m.exe'), app('a.exe')]);
    expect(rows.map((r) => r.exe)).toEqual(['z.exe', 'M.EXE', 'a.exe']);
    expect(rows.map((r) => r.running)).toEqual([false, true, true]);
  });

  it('does not add running apps that are not in allow', () => {
    const rows = gameRowsFor(['a.exe'], {}, [app('b.exe', { iconB64: 'B' })]);
    expect(rows).toEqual([{ exe: 'a.exe', name: 'a', iconB64: null, running: false }]);
  });

  it('uses displayNameFor for the name', () => {
    const games = { 'a.exe': { display_name: 'Alpha' } };
    const rows = gameRowsFor(['a.exe'], games, []);
    expect(rows[0].name).toBe(displayNameFor('a.exe', games));
  });
});

describe('addGame immutability and ordering', () => {
  it('never mutates its input, including nested override entries', () => {
    const slice: GamesSlice = {
      allow: ['b.exe'],
      block: ['c.exe'],
      games: { 'b.exe': { display_name: 'B', folder: 'D:\\x' } },
    };
    const before = structuredClone(slice);
    const r = addGame(slice, 'A.EXE', 'Alpha');
    expect(r.ok).toBe(true);
    expect(slice).toStrictEqual(before);
  });

  it('does not mutate the input when the add is refused', () => {
    const slice: GamesSlice = { allow: ['b.exe'], block: ['c.exe'], games: {} };
    const before = structuredClone(slice);
    addGame(slice, 'C.exe', 'C');
    addGame(slice, 'b.EXE', 'B');
    expect(slice).toStrictEqual(before);
  });

  it('returns allow in input order with the new exe last', () => {
    const slice: GamesSlice = { allow: ['b.exe', 'a.exe'], block: [], games: {} };
    const r = addGame(slice, 'C.EXE', 'Gamma');
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.value.allow).toEqual(['b.exe', 'a.exe', 'c.exe']);
  });

  it('returned allow is a new array, so pushing to it leaves the input alone', () => {
    const slice: GamesSlice = { allow: ['b.exe'], block: [], games: {} };
    const r = addGame(slice, 'a.exe', 'A');
    if (!r.ok) throw new Error('expected ok');
    r.value.allow.push('zzz.exe');
    expect(slice.allow).toEqual(['b.exe']);
  });
});
