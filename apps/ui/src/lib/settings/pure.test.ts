import { describe, expect, it } from 'vitest';
import type { DiagnosticDto } from '../daemon';
import {
  addToList,
  appliesAfterGameEnds,
  applyNotice,
  clamp,
  commitNumber,
  cloneJson,
  deepEqual,
  defaultNormalize,
  diffPatch,
  fieldDiagnostic,
  formatFilenameExample,
  getPath,
  hasBlockingErrors,
  normalizeExeName,
  parseNumber,
  setPath,
} from './pure';

const base = {
  general: { autostart: false, close_behavior: 'minimize_to_tray', start_minimized: true },
  storage: { root: 'C:\\Videos', quota_gb: 0, filename_template: '{game} {kind}' },
  detection: { mode: 'auto', allow: ['a.exe'], block: [] as string[], stop_grace_s: 10 },
  games: { 'cs2.exe': { display_name: 'CS2' } } as Record<string, unknown>,
};

describe('deepEqual', () => {
  it.each([
    [1, 1, true],
    [1, '1', false],
    [null, null, true],
    [[1, 2], [1, 2], true],
    [[1, 2], [2, 1], false],
    [[1], [1, 2], false],
    [{ a: 1, b: { c: [1] } }, { b: { c: [1] }, a: 1 }, true],
    [{ a: 1 }, { a: 1, b: 2 }, false],
    [{ a: null }, { a: undefined }, false],
    [{ a: 1 }, [1], false],
  ])('deepEqual(%j, %j) is %s', (a, b, expected) => {
    expect(deepEqual(a, b)).toBe(expected);
  });
});

describe('diffPatch', () => {
  // Table of [name, saved, draft, expected patch]
  const cases: [string, unknown, unknown, Record<string, unknown>][] = [
    ['identical configs give an empty patch', base, cloneJson(base), {}],
    [
      'one scalar in one section',
      base,
      { ...base, general: { ...base.general, autostart: true } },
      { general: { autostart: true } },
    ],
    [
      'two sections change, only leaves are sent',
      base,
      {
        ...base,
        general: { ...base.general, close_behavior: 'exit' },
        storage: { ...base.storage, quota_gb: 50 },
      },
      { general: { close_behavior: 'exit' }, storage: { quota_gb: 50 } },
    ],
    [
      'an array that changes is replaced whole, not merged',
      base,
      { ...base, detection: { ...base.detection, allow: ['a.exe', 'b.exe'] } },
      { detection: { allow: ['a.exe', 'b.exe'] } },
    ],
    [
      'an array cleared to empty is sent as []',
      base,
      { ...base, detection: { ...base.detection, allow: [] } },
      { detection: { allow: [] } },
    ],
    [
      'an array reordered is still a change',
      { list: ['a', 'b'] },
      { list: ['b', 'a'] },
      { list: ['b', 'a'] },
    ],
    [
      'a removed games entry becomes null',
      base,
      { ...base, games: {} },
      { games: { 'cs2.exe': null } },
    ],
    [
      'an added games entry is sent whole',
      base,
      { ...base, games: { ...base.games, 'x.exe': { display_name: 'X' } } },
      { games: { 'x.exe': { display_name: 'X' } } },
    ],
    [
      'a changed nested game field only sends that field',
      { games: { 'cs2.exe': { display_name: 'CS2', folder: null } } },
      { games: { 'cs2.exe': { display_name: 'Counter', folder: null } } },
      { games: { 'cs2.exe': { display_name: 'Counter' } } },
    ],
    [
      'null to value is a change',
      { games: { 'cs2.exe': { folder: null } } },
      { games: { 'cs2.exe': { folder: 'D:\\clips' } } },
      { games: { 'cs2.exe': { folder: 'D:\\clips' } } },
    ],
    [
      'value to null clears the field',
      { games: { 'cs2.exe': { folder: 'D:\\clips' } } },
      { games: { 'cs2.exe': { folder: null } } },
      { games: { 'cs2.exe': { folder: null } } },
    ],
    [
      'a key removed from the draft becomes null',
      { general: { a: 1, b: 2 } },
      { general: { a: 1 } },
      { general: { b: null } },
    ],
    [
      'a key added to the draft is sent',
      { general: { a: 1 } },
      { general: { a: 1, b: 2 } },
      { general: { b: 2 } },
    ],
    [
      'object replaced by a scalar',
      { x: { a: 1 } },
      { x: 5 },
      { x: 5 },
    ],
    [
      'scalar replaced by an object',
      { x: 5 },
      { x: { a: 1 } },
      { x: { a: 1 } },
    ],
    ['a null saved side with an object draft is sent', { x: null }, { x: { a: 1 } }, { x: { a: 1 } }],
    ['non-object inputs give an empty patch', 5, 6, {}],
    ['a false-to-true toggle is sent', { g: { t: false } }, { g: { t: true } }, { g: { t: true } }],
    ['a zero is not treated as unset', { g: { n: 5 } }, { g: { n: 0 } }, { g: { n: 0 } }],
    ['an empty string is a real value', { g: { s: 'x' } }, { g: { s: '' } }, { g: { s: '' } }],
  ];

  it.each(cases)('%s', (_name, saved, draft, expected) => {
    expect(diffPatch(saved, draft)).toEqual(expected);
  });

  it('does not mutate its inputs', () => {
    const saved = cloneJson(base);
    const draft = cloneJson(base);
    draft.general.autostart = true;
    diffPatch(saved, draft);
    expect(saved).toEqual(base);
    expect(draft.general.autostart).toBe(true);
  });

  it('applying the patch to saved gives the draft back', () => {
    const draft = {
      ...base,
      general: { ...base.general, close_behavior: 'exit' },
      detection: { ...base.detection, allow: [] as string[] },
      games: { 'x.exe': { display_name: 'X' } },
    };
    const patch = diffPatch(base, draft);
    let applied: Record<string, unknown> = cloneJson(base);
    for (const [key, value] of Object.entries(patch)) {
      applied = applyMerge(applied, key, value);
    }
    expect(applied).toEqual(draft);
  });
});

/** Test-side model of the daemon's merge: objects merge, arrays and scalars replace, null removes. */
function applyMerge(target: Record<string, unknown>, key: string, value: unknown): Record<string, unknown> {
  const out = { ...target };
  if (value === null) {
    delete out[key];
  } else if (
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    typeof out[key] === 'object' &&
    out[key] !== null &&
    !Array.isArray(out[key])
  ) {
    let merged: Record<string, unknown> = out[key] as Record<string, unknown>;
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      merged = applyMerge(merged, k, v);
    }
    out[key] = merged;
  } else {
    out[key] = value;
  }
  return out;
}

describe('getPath and setPath', () => {
  it('reads dotted paths and tolerates missing segments', () => {
    expect(getPath(base, 'general.close_behavior')).toBe('minimize_to_tray');
    expect(getPath(base, 'general.missing.deeper')).toBeUndefined();
    expect(getPath(null, 'a')).toBeUndefined();
  });

  it('sets a value without mutating the original', () => {
    const next = setPath(base, 'general.autostart', true);
    expect(next.general.autostart).toBe(true);
    expect(base.general.autostart).toBe(false);
    expect(next.storage).toBe(base.storage);
  });

  it('creates missing intermediate objects', () => {
    const next = setPath({} as Record<string, unknown>, 'a.b.c', 1);
    expect(next).toEqual({ a: { b: { c: 1 } } });
  });

  it('sets a top-level key', () => {
    expect(setPath({ a: 1 }, 'b', 2)).toEqual({ a: 1, b: 2 });
  });
});

describe('fieldDiagnostic', () => {
  const diags: DiagnosticDto[] = [
    { code: 'CFG-S003', severity: 'error', path: 'storage.filename_template', message: 'illegal char' },
    { code: 'CFG-S007', severity: 'warning', path: 'storage.filename_template', message: 'no kind' },
    { code: 'CFG-D001', severity: 'error', path: 'detection.allow[1]', message: 'bad exe' },
    { code: 'CFG-X001', severity: 'warning', path: 'detection.allowed', message: 'not a child' },
    { code: 'CFG-W001', severity: 'warning', path: 'detection.stop_grace_s', message: 'slow' },
  ];

  it('prefers an error over a warning on the same path', () => {
    expect(fieldDiagnostic('storage.filename_template', diags)).toEqual({
      code: 'CFG-S003',
      severity: 'error',
      message: 'illegal char',
    });
  });

  it('matches list items under the list path', () => {
    expect(fieldDiagnostic('detection.allow', diags)?.message).toBe('bad exe');
  });

  it('does not match a sibling whose name only starts the same way', () => {
    expect(fieldDiagnostic('detection.allow', [diags[3]])).toBeNull();
  });

  it('returns a warning when only a warning exists', () => {
    expect(fieldDiagnostic('detection.stop_grace_s', diags)?.severity).toBe('warning');
  });

  it('returns null when nothing matches', () => {
    expect(fieldDiagnostic('general.autostart', diags)).toBeNull();
  });

  it('hasBlockingErrors ignores warnings', () => {
    expect(hasBlockingErrors([diags[1], diags[3]])).toBe(false);
    expect(hasBlockingErrors(diags)).toBe(true);
    expect(hasBlockingErrors([])).toBe(false);
  });
});

describe('number input', () => {
  it.each([
    [5, 0, 10, 5],
    [-3, 0, 10, 0],
    [42, 0, 10, 10],
    [0, 0, 0, 0],
  ])('clamp(%d, %d, %d) is %d', (v, lo, hi, expected) => {
    expect(clamp(v, lo, hi)).toBe(expected);
  });

  it.each([
    ['12', 12],
    ['  7  ', 7],
    ['-1.5', -1.5],
    ['', null],
    ['   ', null],
    ['abc', null],
    ['Infinity', null],
    ['1e3', 1000],
  ])('parseNumber(%j) is %s', (text, expected) => {
    expect(parseNumber(text)).toBe(expected);
  });

  it('commitNumber clamps and rounds integers on commit', () => {
    expect(commitNumber('250', 0, 100000, true)).toBe(250);
    expect(commitNumber('999999', 0, 100000, true)).toBe(100000);
    expect(commitNumber('-4', 0, 300, true)).toBe(0);
    expect(commitNumber('2.6', 0, 300, true)).toBe(3);
    expect(commitNumber('2.6', 0, 300, false)).toBe(2.6);
  });

  it('commitNumber returns null for text that is not a number', () => {
    expect(commitNumber('', 0, 10, true)).toBeNull();
    expect(commitNumber('x', 0, 10, true)).toBeNull();
  });
});

describe('formatFilenameExample', () => {
  const when = new Date(2026, 9, 10, 14, 3, 9); // local time: 2026-10-10 14:03:09

  it('renders the default template with the sample parts', () => {
    expect(formatFilenameExample('{game} {yyyy-MM-dd HH-mm-ss} {kind}', when)).toBe(
      'Counter-Strike 2 2026-10-10 14-03-09 session',
    );
  });

  it('renders the id token', () => {
    expect(formatFilenameExample('{id}-{MM}', when)).toBe('a1b2c3d4-10');
  });

  it('pads single digit fields', () => {
    expect(formatFilenameExample('{dd}.{MM}.{yyyy} {HH}{mm}{ss}', new Date(2026, 0, 2, 3, 4, 5))).toBe(
      '02.01.2026 030405',
    );
  });

  it('leaves unknown tokens visible', () => {
    expect(formatFilenameExample('{game} {bogus}', when)).toBe('Counter-Strike 2 {bogus}');
  });

  it('leaves literal text alone', () => {
    expect(formatFilenameExample('clip', when)).toBe('clip');
  });

  it('uses the given sample parts when supplied', () => {
    expect(formatFilenameExample('{game}', when, { game: 'Apex', kind: 'clip', id: 'x' })).toBe('Apex');
  });
});

describe('list entries', () => {
  it.each([
    ['Discord.exe', 'discord.exe'],
    ['  game.EXE ', 'game.exe'],
  ])('normalizeExeName(%j) is %j', (raw, value) => {
    expect(normalizeExeName(raw)).toEqual({ ok: true, value });
  });

  it.each([
    ['', 'Enter an executable name'],
    ['.exe', 'must end in .exe'],
    ['game.dll', 'must end in .exe'],
    ['C:\\Games\\game.exe', 'file name only'],
    ['games/game.exe', 'file name only'],
  ])('normalizeExeName(%j) is refused', (raw, message) => {
    const r = normalizeExeName(raw);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain(message);
  });

  it('addToList appends a normalized entry', () => {
    expect(addToList(['a.exe'], ' B.exe ', normalizeExeName)).toEqual({ ok: true, list: ['a.exe', 'b.exe'] });
  });

  it('addToList refuses duplicates ignoring case', () => {
    const r = addToList(['game.exe'], 'GAME.exe', normalizeExeName);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toBe('game.exe is already in the list.');
  });

  it('addToList refuses blank input with the default normalizer', () => {
    expect(addToList([], '   ')).toEqual({ ok: false, error: 'Enter a value first.' });
  });

  it('addToList does not mutate the input list', () => {
    const list = ['a'];
    addToList(list, 'b');
    expect(list).toEqual(['a']);
  });

  it('defaultNormalize trims', () => {
    expect(defaultNormalize('  x  ')).toEqual({ ok: true, value: 'x' });
  });
});

describe('apply notices and deferred paths', () => {
  it('applyNotice reports Applied or the deferred wording', () => {
    expect(applyNotice(false)).toBe('Applied');
    expect(applyNotice(true)).toBe('Will apply when recording stops');
  });

  it('storage.quota_gb is applied immediately', () => {
    // Not in issues_b12.test.ts's apply_class table; the other deferred paths are covered there.
    expect(appliesAfterGameEnds('storage.quota_gb')).toBe(false);
  });
});
