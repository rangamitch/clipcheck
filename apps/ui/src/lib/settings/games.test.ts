import { describe, expect, it } from 'vitest';
import { diffPatch } from './pure';
import {
  ALREADY_DETECTED,
  addBlocked,
  addGame,
  blockPickerAppsFor,
  displayNameFor,
  gameRowsFor,
  isDisplayNameOnly,
  pickerAppsFor,
  removeBlocked,
  removeGame,
  safeDisplayName,
  stem,
  type GameCandidate,
  type GamesSlice,
} from './games';

const app = (exe: string, extra: Partial<GameCandidate> = {}): GameCandidate => ({
  exe,
  name: exe.replace(/\.exe$/, ''),
  iconB64: null,
  isGame: false,
  suggested: false,
  ...extra,
});

const empty: GamesSlice = { allow: [], block: [], games: {} };

describe('stem', () => {
  it.each([
    ['cs2.exe', 'cs2'],
    ['CS2.EXE', 'CS2'],
    ['C:\\Games\\Foo\\bar.exe', 'bar'],
    ['/usr/bin/tool.exe', 'tool'],
    ['noext', 'noext'],
    ['.exe', '.exe'],
  ])('stem(%s) is %s', (input, want) => {
    expect(stem(input)).toBe(want);
  });
});

describe('safeDisplayName', () => {
  it('trims the name', () => {
    expect(safeDisplayName('  Counter-Strike 2 ', 'cs2.exe')).toBe('Counter-Strike 2');
  });

  it('falls back to the stem when blank', () => {
    expect(safeDisplayName('   ', 'cs2.exe')).toBe('cs2');
    expect(safeDisplayName('', 'cs2.exe')).toBe('cs2');
  });

  it('caps the name at 64 characters', () => {
    const long = 'a'.repeat(80);
    expect(safeDisplayName(long, 'x.exe')).toHaveLength(64);
  });

  it('counts characters, not UTF-16 units', () => {
    const emoji = '🎮'.repeat(70);
    expect(Array.from(safeDisplayName(emoji, 'x.exe'))).toHaveLength(64);
  });
});

describe('displayNameFor', () => {
  it.each([
    ['uses display_name when set', 'cs2.exe', { 'cs2.exe': { display_name: 'Counter-Strike 2' } }, 'Counter-Strike 2'],
    ['falls back to stem with no entry', 'cs2.exe', {}, 'cs2'],
    ['falls back to stem for a blank display_name', 'cs2.exe', { 'cs2.exe': { display_name: '  ' } }, 'cs2'],
    ['falls back to stem for a null display_name', 'cs2.exe', { 'cs2.exe': { display_name: null } }, 'cs2'],
    ['matches the key case-insensitively', 'CS2.exe', { 'cs2.exe': { display_name: 'CS2' } }, 'CS2'],
  ])('%s', (_label, exe, games, want) => {
    expect(displayNameFor(exe, games)).toBe(want);
  });
});

describe('isDisplayNameOnly', () => {
  it.each([
    ['display_name only', { display_name: 'X' }, true],
    ['empty object', {}, true],
    ['display_name plus a null folder', { display_name: 'X', folder: null }, true],
    ['display_name plus folder', { display_name: 'X', folder: 'D:\\clips' }, false],
    ['video override', { video: { preset: 'high' } }, false],
    ['no display_name but a folder', { folder: 'D:\\clips' }, false],
  ])('%s -> %s', (_label, entry, want) => {
    expect(isDisplayNameOnly(entry)).toBe(want);
  });
});

describe('addGame', () => {
  it('appends a normalized exe and records the display name', () => {
    const r = addGame(empty, 'CS2.EXE', 'Counter-Strike 2');
    expect(r).toEqual({
      ok: true,
      value: {
        allow: ['cs2.exe'],
        block: [],
        games: { 'cs2.exe': { display_name: 'Counter-Strike 2' } },
      },
    });
  });

  it('refuses an exe already in the allow list, ignoring case', () => {
    const slice: GamesSlice = { allow: ['cs2.exe'], block: [], games: {} };
    const r = addGame(slice, 'CS2.exe', 'CS2');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toMatch(/already in your games/);
  });

  it('refuses an exe in the block list', () => {
    const slice: GamesSlice = { allow: [], block: ['launcher.exe'], games: {} };
    const r = addGame(slice, 'launcher.exe', 'Launcher');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toMatch(/Never record/);
  });

  it.each([
    ['', 'Enter an executable name'],
    ['game', 'must end in .exe'],
    ['C:\\x\\game.exe', 'file name only'],
  ])('refuses the invalid name %j', (raw, msg) => {
    const r = addGame(empty, raw, 'X');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error.toLowerCase()).toContain(msg.toLowerCase());
  });

  it('keeps an existing display_name and other override keys', () => {
    const slice: GamesSlice = {
      allow: [],
      block: [],
      games: { 'cs2.exe': { display_name: 'My CS', folder: 'D:\\clips' } },
    };
    const r = addGame(slice, 'cs2.exe', 'Counter-Strike 2');
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.value.games['cs2.exe']).toEqual({ display_name: 'My CS', folder: 'D:\\clips' });
    }
  });

  it('adds an override where the entry was blank', () => {
    const slice: GamesSlice = { allow: [], block: [], games: { 'cs2.exe': { display_name: '  ' } } };
    const r = addGame(slice, 'cs2.exe', 'Counter-Strike 2');
    if (r.ok) expect(r.value.games['cs2.exe']).toEqual({ display_name: 'Counter-Strike 2' });
    else throw new Error('expected ok');
  });

  it('does not change the input slice', () => {
    const slice: GamesSlice = { allow: ['a.exe'], block: [], games: {} };
    addGame(slice, 'b.exe', 'B');
    expect(slice).toEqual({ allow: ['a.exe'], block: [], games: {} });
  });
});

describe('removeGame', () => {
  it('removes the exe and a display_name-only override', () => {
    const slice: GamesSlice = {
      allow: ['cs2.exe', 'elden.exe'],
      block: [],
      games: { 'cs2.exe': { display_name: 'CS2' }, 'elden.exe': { display_name: 'ER' } },
    };
    expect(removeGame(slice, 'cs2.exe')).toEqual({
      allow: ['elden.exe'],
      block: [],
      games: { 'elden.exe': { display_name: 'ER' } },
    });
  });

  it('keeps a richer override so folder and video settings survive', () => {
    const slice: GamesSlice = {
      allow: ['cs2.exe'],
      block: [],
      games: { 'cs2.exe': { display_name: 'CS2', folder: 'D:\\clips' } },
    };
    expect(removeGame(slice, 'cs2.exe')).toEqual({
      allow: [],
      block: [],
      games: { 'cs2.exe': { display_name: 'CS2', folder: 'D:\\clips' } },
    });
  });

  it('matches the allow entry case-insensitively', () => {
    const slice: GamesSlice = { allow: ['cs2.exe'], block: [], games: {} };
    expect(removeGame(slice, 'CS2.EXE').allow).toEqual([]);
  });

  it('is a no-op for an exe not in the list', () => {
    const slice: GamesSlice = { allow: ['a.exe'], block: [], games: {} };
    expect(removeGame(slice, 'zzz.exe')).toEqual({ allow: ['a.exe'], block: [], games: {} });
  });
});

describe('addBlocked / removeBlocked', () => {
  it('appends a normalized exe to the block list', () => {
    expect(addBlocked(empty, 'Launcher.EXE')).toEqual({
      ok: true,
      value: { allow: [], block: ['launcher.exe'], games: {} },
    });
  });

  it('refuses a duplicate, ignoring case', () => {
    const slice: GamesSlice = { allow: [], block: ['a.exe'], games: {} };
    const r = addBlocked(slice, 'A.exe');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toMatch(/already in Never record/);
  });

  it('refuses an invalid name', () => {
    expect(addBlocked(empty, 'nope').ok).toBe(false);
  });

  it('removes an exe case-insensitively and leaves games alone', () => {
    const slice: GamesSlice = { allow: ['x.exe'], block: ['a.exe', 'b.exe'], games: { 'x.exe': { display_name: 'X' } } };
    expect(removeBlocked(slice, 'A.EXE')).toEqual({
      allow: ['x.exe'],
      block: ['b.exe'],
      games: { 'x.exe': { display_name: 'X' } },
    });
  });
});

describe('pickerAppsFor', () => {
  it('excludes apps already in the allow or block list', () => {
    const running = [app('a.exe'), app('b.exe'), app('c.exe')];
    const got = pickerAppsFor(running, ['A.exe'], ['c.exe']).map((p) => p.exe);
    expect(got).toEqual(['b.exe']);
  });

  it('keeps detector-accepted games listed, disabled and badged', () => {
    const running = [app('cs2.exe', { isGame: true }), app('notes.exe')];
    const got = pickerAppsFor(running, [], []);
    expect(got.find((p) => p.exe === 'cs2.exe')).toMatchObject({ disabled: true, badge: ALREADY_DETECTED });
    expect(got.find((p) => p.exe === 'notes.exe')).toMatchObject({ disabled: false, badge: undefined });
    // Enabled apps come before disabled ones within the same group.
    expect(got.map((p) => p.exe)).toEqual(['notes.exe', 'cs2.exe']);
  });

  it('lists suggested apps first, then the rest, keeping input order within each', () => {
    const running = [
      app('one.exe'),
      app('two.exe', { suggested: true }),
      app('three.exe'),
      app('four.exe', { suggested: true }),
    ];
    expect(pickerAppsFor(running, [], []).map((p) => p.exe)).toEqual([
      'two.exe',
      'four.exe',
      'one.exe',
      'three.exe',
    ]);
  });

  it('puts disabled apps after enabled ones within each group', () => {
    const running = [
      app('game.exe', { isGame: true }),
      app('plain.exe'),
      app('sugg.exe', { suggested: true, isGame: true }),
      app('sugg2.exe', { suggested: true }),
    ];
    expect(pickerAppsFor(running, [], []).map((p) => p.exe)).toEqual([
      'sugg2.exe',
      'sugg.exe',
      'plain.exe',
      'game.exe',
    ]);
  });

  it('uses the exe as the hint and keeps name and icon', () => {
    const got = pickerAppsFor([app('x.exe', { name: 'Xapp', iconB64: 'AAA' })], [], []);
    expect(got).toEqual([
      {
        exe: 'x.exe',
        name: 'Xapp',
        iconB64: 'AAA',
        hint: 'x.exe',
        suggested: false,
        disabled: false,
        badge: undefined,
      },
    ]);
  });

  it('returns an empty list when nothing is running', () => {
    expect(pickerAppsFor([], ['a.exe'], [])).toEqual([]);
  });
});

describe('blockPickerAppsFor', () => {
  it('excludes blocked apps and does not disable detected games', () => {
    const running = [app('a.exe'), app('cs2.exe', { isGame: true }), app('b.exe')];
    const got = blockPickerAppsFor(running, ['b.exe']);
    expect(got.map((p) => p.exe)).toEqual(['a.exe', 'cs2.exe']);
    expect(got.every((p) => !p.disabled)).toBe(true);
  });
});

describe('gameRowsFor', () => {
  it('builds rows in allow order with name, icon and running flag', () => {
    const running = [app('cs2.exe', { iconB64: 'ICON' })];
    const rows = gameRowsFor(['elden.exe', 'cs2.exe'], { 'cs2.exe': { display_name: 'CS2' } }, running);
    expect(rows).toEqual([
      { exe: 'elden.exe', name: 'elden', iconB64: null, running: false },
      { exe: 'cs2.exe', name: 'CS2', iconB64: 'ICON', running: true },
    ]);
  });

  it('matches running apps case-insensitively', () => {
    const rows = gameRowsFor(['CS2.exe'], {}, [app('cs2.exe')]);
    expect(rows[0].running).toBe(true);
  });
});

/** A config with only the keys this tab touches, as the store would hold them. */
function config(detection: { allow: string[]; block: string[] }, games: Record<string, unknown>) {
  return {
    detection: { mode: 'auto', auto_record: true, record_sessions: true, after_manual_stop: 'until_next_launch', stop_grace_s: 10, ...detection },
    games,
  };
}

function applyToConfig(saved: ReturnType<typeof config>, next: GamesSlice) {
  return {
    ...saved,
    detection: { ...saved.detection, allow: next.allow, block: next.block },
    games: next.games,
  };
}

describe('diff patch shapes (what the daemon receives)', () => {
  it('add to an empty list sends the allow array and the new games entry', () => {
    const saved = config({ allow: [], block: [] }, {});
    const r = addGame(empty, 'cs2.exe', 'Counter-Strike 2');
    if (!r.ok) throw new Error('expected ok');
    expect(diffPatch(saved, applyToConfig(saved, r.value))).toEqual({
      detection: { allow: ['cs2.exe'] },
      games: { 'cs2.exe': { display_name: 'Counter-Strike 2' } },
    });
  });

  it('removing a display_name-only game sends the array and a null for the games key', () => {
    const saved = config({ allow: ['cs2.exe'], block: [] }, { 'cs2.exe': { display_name: 'CS2' } });
    const slice: GamesSlice = { allow: ['cs2.exe'], block: [], games: { 'cs2.exe': { display_name: 'CS2' } } };
    expect(diffPatch(saved, applyToConfig(saved, removeGame(slice, 'cs2.exe')))).toEqual({
      detection: { allow: [] },
      games: { 'cs2.exe': null },
    });
  });

  it('removing a game with a richer override sends only the allow array', () => {
    const games = { 'cs2.exe': { display_name: 'CS2', folder: 'D:\\clips' } };
    const saved = config({ allow: ['cs2.exe'], block: [] }, games);
    const slice: GamesSlice = { allow: ['cs2.exe'], block: [], games };
    expect(diffPatch(saved, applyToConfig(saved, removeGame(slice, 'cs2.exe')))).toEqual({
      detection: { allow: [] },
    });
  });

  it('adding a block sends only the block array', () => {
    const saved = config({ allow: [], block: [] }, {});
    const r = addBlocked(empty, 'discord.exe');
    if (!r.ok) throw new Error('expected ok');
    expect(diffPatch(saved, applyToConfig(saved, r.value))).toEqual({
      detection: { block: ['discord.exe'] },
    });
  });

  it('an unrelated games entry is not touched by an add', () => {
    const games = { 'x.exe': { display_name: 'X' } };
    const saved = config({ allow: ['x.exe'], block: [] }, games);
    const slice: GamesSlice = { allow: ['x.exe'], block: [], games };
    const r = addGame(slice, 'y.exe', 'Y');
    if (!r.ok) throw new Error('expected ok');
    expect(diffPatch(saved, applyToConfig(saved, r.value))).toEqual({
      detection: { allow: ['x.exe', 'y.exe'] },
      games: { 'y.exe': { display_name: 'Y' } },
    });
  });
});
