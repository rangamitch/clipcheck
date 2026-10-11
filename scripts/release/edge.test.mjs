// Edge-case tests for the release scripts (tester-owned; does not modify release.test.mjs).
// Run: node --test "scripts/release/*.test.mjs"
// Uses only temp files under os.tmpdir(); no network, no cargo, no game or recording access.

import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { execFile } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, truncateSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { compareVersions, tomlValue } from './check-versions.mjs';
import {
  buildLatestJson,
  installerUrl,
  isRfc3339Utc,
  run as runLatestJson,
} from './make-latest-json.mjs';
import { checkBudget } from './size-budget.mjs';
import { normalizeLicense, normalizeRepository, renderNotices, sortUnique } from './write-notices.mjs';

const execFileP = promisify(execFile);
const HERE = dirname(fileURLToPath(import.meta.url));
const SIZE_SCRIPT = resolve(HERE, 'size-budget.mjs');
const MIB = 1024 * 1024;
const TIMEOUT = 60_000;

const dir = mkdtempSync(join(tmpdir(), 'clipcheck-edge-'));
after(() => rmSync(dir, { recursive: true, force: true }));

let counter = 0;
function fixture(name, content) {
  const p = join(dir, `${++counter}-${name}`);
  writeFileSync(p, content);
  return p;
}

// ---------------------------------------------------------------- A1. check-versions: TOML parsing

test('tomlValue reads CRLF Cargo.toml and picks the right table', { timeout: TIMEOUT }, () => {
  const toml = '[package]\r\nname = "x"\r\nversion = "1.2.3"\r\n\r\n[workspace.package]\r\nversion = "4.5.6"\r\n';
  assert.equal(tomlValue(toml, 'package', 'version'), '1.2.3');
  assert.equal(tomlValue(toml, 'workspace.package', 'version'), '4.5.6');
  assert.equal(tomlValue(toml, 'dependencies', 'version'), undefined);
});

test('tomlValue ignores commented-out version lines', { timeout: TIMEOUT }, () => {
  const toml = '[package]\n# version = "9.9.9"\nversion = "1.0.0" # real\n';
  assert.equal(tomlValue(toml, 'package', 'version'), '1.0.0');
});

test('tomlValue recognises a table header followed by a comment (valid TOML)', { timeout: TIMEOUT }, () => {
  const toml = '[workspace.package] # shared metadata\nversion = "0.1.0"\n';
  assert.equal(tomlValue(toml, 'workspace.package', 'version'), '0.1.0');
});

// ---------------------------------------------------------------- A2. check-versions: version tags

function tagStatus(tag, refVersion = '1.2.3') {
  const rows = [{ name: 'ref', file: 'Cargo.toml', version: refVersion }];
  return compareVersions(rows, tag).rows.at(-1).status;
}

for (const tag of ['v1.2.3', '1.2.3', 'v1.2.3-rc.1', '1.2.3-rc.1']) {
  test(`tag "${tag}" matches the matching version`, { timeout: TIMEOUT }, () => {
    const ver = tag.replace(/^v/, '');
    assert.equal(tagStatus(tag, ver), 'ok');
  });
}

// Note: "v1.2.3--" is valid semver (prerelease identifier "-"), so it is not in this list.
for (const tag of ['v1.2', 'v01.2.3', 'V1.2.3', 'vv1.2.3', 'v', '', 'v1.2.3-', 'v1.2.3-01']) {
  test(`tag "${tag}" is rejected as INVALID`, { timeout: TIMEOUT }, () => {
    assert.equal(tagStatus(tag), 'INVALID');
  });
}

test('tag v1.2.4 against version 1.2.3 is MISMATCH, and v1.2.3 with a prerelease is MISMATCH', { timeout: TIMEOUT }, () => {
  assert.equal(tagStatus('v1.2.4'), 'MISMATCH');
  assert.equal(tagStatus('v1.2.3-rc.1'), 'MISMATCH');
});

// ---------------------------------------------------------------- A3. make-latest-json

const B64 = 'dW50cnVzdGVkIHNpZ25hdHVyZSBmb3IgdGVzdA==';
const NOTES_RAW = 'Fixed "quotes", a back\\slash,\nnew line 🚀 and émoji\r\n  indented\t\n';
const NOTES_EXPECTED = 'Fixed "quotes", a back\\slash,\nnew line 🚀 and émoji\n  indented';
const INSTALLER_NAME = 'ClipCheck 0.2.0+rc1_x64-setup.exe';

async function writeManifest(extra = {}) {
  // Own sub-folder so the installer keeps its exact file name (basename is part of the URL).
  const installerDir = join(dir, `inst-${++counter}`);
  mkdirSync(installerDir);
  const installer = join(installerDir, INSTALLER_NAME);
  writeFileSync(installer, 'MZ-fake-installer');
  const values = {
    version: '0.2.0',
    'notes-file': fixture('notes.md', NOTES_RAW),
    installer,
    'sig-file': fixture('setup.exe.sig', `  ${B64}\r\n\r\n  `),
    repo: 'owner/clipcheck',
    'pub-date': '2026-10-10T12:00:00Z',
    out: join(dir, `${++counter}`, 'latest.json'),
    ...extra,
  };
  await runLatestJson(values);
  return { values, manifest: JSON.parse(readFileSync(values.out, 'utf8')) };
}

test('make-latest-json writes valid JSON for notes with quotes, newlines, emoji and backslashes', { timeout: TIMEOUT }, async () => {
  const { manifest } = await writeManifest();
  assert.equal(manifest.notes, NOTES_EXPECTED);
  assert.equal(manifest.version, '0.2.0');
});

test('make-latest-json trims the signature to the exact base64 text (CRLF and spaces removed)', { timeout: TIMEOUT }, async () => {
  const { manifest } = await writeManifest();
  assert.equal(manifest.platforms['windows-x86_64'].signature, B64);
});

test('make-latest-json percent-encodes spaces and "+" in the installer URL', { timeout: TIMEOUT }, async () => {
  const { manifest } = await writeManifest();
  const url = manifest.platforms['windows-x86_64'].url;
  assert.equal(url, 'https://github.com/owner/clipcheck/releases/download/v0.2.0/ClipCheck%200.2.0%2Brc1_x64-setup.exe');
  assert.equal(decodeURIComponent(url.split('/').pop()), INSTALLER_NAME);
});

test('make-latest-json keeps pub_date verbatim and the key order is fixed', { timeout: TIMEOUT }, async () => {
  const { manifest, values } = await writeManifest({});
  assert.equal(manifest.pub_date, '2026-10-10T12:00:00Z');
  assert.deepEqual(Object.keys(manifest), ['version', 'notes', 'pub_date', 'platforms']);
  assert.ok(readFileSync(values.out, 'utf8').endsWith('}\n'));
});

test('make-latest-json default pub_date is RFC 3339 UTC with whole seconds and close to now', { timeout: TIMEOUT }, async () => {
  const before = Date.now();
  const { manifest } = await writeManifest({ 'pub-date': undefined });
  const after = Date.now();
  assert.match(manifest.pub_date, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
  const t = Date.parse(manifest.pub_date);
  assert.ok(t >= Math.floor(before / 1000) * 1000 && t <= after, `pub_date ${manifest.pub_date} not near now`);
});

test('make-latest-json rejects an empty or whitespace-only signature', { timeout: TIMEOUT }, async () => {
  for (const content of ['', ' \r\n\t ']) {
    const installer = join(dir, `${++counter}-ClipCheck_0.2.0_x64-setup.exe`);
    writeFileSync(installer, 'MZ');
    await assert.rejects(
      runLatestJson({
        version: '0.2.0',
        'notes-file': fixture('n.md', 'x'),
        installer,
        'sig-file': fixture('empty.sig', content),
        repo: 'owner/clipcheck',
        'pub-date': '2026-10-10T12:00:00Z',
        out: join(dir, `${++counter}`, 'latest.json'),
      }),
      /signature is empty/,
    );
  }
});

test('make-latest-json rejects a missing installer', { timeout: TIMEOUT }, async () => {
  await assert.rejects(
    runLatestJson({
      version: '0.2.0',
      'notes-file': fixture('n.md', 'x'),
      installer: join(dir, 'does-not-exist-setup.exe'),
      'sig-file': fixture('s.sig', B64),
      repo: 'owner/clipcheck',
      'pub-date': '2026-10-10T12:00:00Z',
      out: join(dir, 'x', 'latest.json'),
    }),
    /installer not found/,
  );
});

test('installerUrl encodes #, %, spaces and + in file names', { timeout: TIMEOUT }, () => {
  assert.equal(
    installerUrl('o/r', '1.0.0', 'a+b c#d%.exe'),
    'https://github.com/o/r/releases/download/v1.0.0/a%2Bb%20c%23d%25.exe',
  );
});

test('isRfc3339Utc accepts whole-second and fractional UTC stamps', { timeout: TIMEOUT }, () => {
  assert.equal(isRfc3339Utc('2026-10-10T12:00:00Z'), true);
  assert.equal(isRfc3339Utc('2026-10-10T12:00:00.123Z'), true);
});

test('isRfc3339Utc rejects offsets other than Z', { timeout: TIMEOUT }, () => {
  assert.equal(isRfc3339Utc('2026-10-10T12:00:00+00:00'), false);
});

test('isRfc3339Utc rejects impossible calendar dates', { timeout: TIMEOUT }, () => {
  assert.equal(isRfc3339Utc('2026-02-30T00:00:00Z'), false);
});

test('isRfc3339Utc rejects hour 25', { timeout: TIMEOUT }, () => {
  assert.equal(isRfc3339Utc('2026-10-10T25:00:00Z'), false);
});

test('isRfc3339Utc rejects hour 24 (RFC 3339 time-hour is 00-23)', { timeout: TIMEOUT }, () => {
  assert.equal(isRfc3339Utc('2026-10-10T24:00:00Z'), false);
});

// ---------------------------------------------------------------- A4. size-budget

test('checkBudget: exactly at the budget passes, 1 byte over fails', { timeout: TIMEOUT }, () => {
  assert.equal(checkBudget(40 * MIB, '40').ok, true);
  assert.equal(checkBudget(40 * MIB + 1, '40').ok, false);
});

test('checkBudget: zero bytes passes; zero, negative and non-numeric budgets throw', { timeout: TIMEOUT }, () => {
  assert.equal(checkBudget(0, '40').ok, true);
  assert.throws(() => checkBudget(1, '0'), /positive number/);
  assert.throws(() => checkBudget(1, '-1'), /positive number/);
  assert.throws(() => checkBudget(1, 'abc'), /positive number/);
});

test('checkBudget: fractional budget in MiB', { timeout: TIMEOUT }, () => {
  assert.equal(checkBudget(MIB + MIB / 2, '1.5').ok, true);
  assert.equal(checkBudget(MIB + MIB / 2 + 1, '1.5').ok, false);
});

async function sizeCli(file, extra = []) {
  try {
    const { stdout } = await execFileP(process.execPath, [SIZE_SCRIPT, '--file', file, ...extra]);
    return { code: 0, stdout };
  } catch (e) {
    return { code: e.code, stdout: e.stdout, stderr: e.stderr };
  }
}

test('size-budget CLI: file of exactly 40 MiB exits 0', { timeout: TIMEOUT }, async () => {
  const p = fixture('exact.exe', '');
  truncateSync(p, 40 * MIB);
  const r = await sizeCli(p, ['--max-mb', '40']);
  assert.equal(r.code, 0, r.stderr);
  assert.match(r.stdout, /^OK /);
});

test('size-budget CLI: file 1 byte over 40 MiB exits 1', { timeout: TIMEOUT }, async () => {
  const p = fixture('over.exe', '');
  truncateSync(p, 40 * MIB + 1);
  const r = await sizeCli(p, ['--max-mb', '40']);
  assert.equal(r.code, 1);
  assert.match(r.stderr, /FAIL .*over budget/);
});

test('size-budget CLI: empty file exits 0', { timeout: TIMEOUT }, async () => {
  const p = fixture('empty.exe', '');
  const r = await sizeCli(p, ['--max-mb', '40']);
  assert.equal(r.code, 0, r.stderr);
});

test('size-budget CLI: missing file exits 1 with an error', { timeout: TIMEOUT }, async () => {
  const r = await sizeCli(join(dir, 'missing.exe'), ['--max-mb', '40']);
  assert.equal(r.code, 1);
  assert.match(r.stderr, /error: .*ENOENT/);
});

// ---------------------------------------------------------------- A5. write-notices

test('normalizeLicense handles strings, blanks, arrays, objects and missing values', { timeout: TIMEOUT }, () => {
  assert.equal(normalizeLicense(' MIT '), 'MIT');
  assert.equal(normalizeLicense('   '), 'UNKNOWN');
  assert.equal(normalizeLicense(['MIT', 'Apache-2.0']), 'MIT OR Apache-2.0');
  assert.equal(normalizeLicense([]), 'UNKNOWN');
  assert.equal(normalizeLicense({ type: 'GPL-3.0' }), 'GPL-3.0');
  assert.equal(normalizeLicense(undefined), 'UNKNOWN');
});

test('normalizeRepository strips git+ and reads {url} objects', { timeout: TIMEOUT }, () => {
  assert.equal(normalizeRepository('git+https://github.com/a/b.git'), 'https://github.com/a/b.git');
  assert.equal(normalizeRepository({ url: 'git+ssh://x/y.git' }), 'ssh://x/y.git');
  assert.equal(normalizeRepository(undefined), '');
});

const CRATES = [
  { name: 'zeta', version: '1.0.0', license: 'MIT', repository: 'https://example.invalid/zeta' },
  { name: 'Alpha', version: '2.0.0', license: 'Apache-2.0', repository: '' },
  { name: 'alpha', version: '0.1.0', license: 'MIT', repository: '' },
  { name: 'zeta', version: '1.0.0', license: 'MIT', repository: 'https://example.invalid/zeta' },
];
const NPM = [
  { name: 'react', version: '18.3.1', license: 'MIT', repository: 'https://github.com/facebook/react' },
  { name: '@scope/pkg', version: '1.0.0', license: 'ISC', repository: '' },
];

test('write-notices render is byte-identical across runs', { timeout: TIMEOUT }, () => {
  const render = () => renderNotices({ crates: sortUnique(CRATES), npm: sortUnique(NPM) });
  assert.equal(render(), render());
});

test('write-notices output does not depend on input order', { timeout: TIMEOUT }, () => {
  const forward = renderNotices({ crates: sortUnique(CRATES), npm: sortUnique(NPM) });
  const reversed = renderNotices({
    crates: sortUnique([...CRATES].reverse()),
    npm: sortUnique([...NPM].reverse()),
  });
  assert.equal(forward, reversed);
});

test('write-notices sorts by code point and removes exact duplicates', { timeout: TIMEOUT }, () => {
  const names = sortUnique(CRATES).map((p) => p.name);
  assert.deepEqual(names, ['Alpha', 'alpha', 'zeta']);
  assert.equal(sortUnique(CRATES).length, 3);
});

test('write-notices: duplicate name+version with different license resolves the same in any order', { timeout: TIMEOUT }, () => {
  const a = { name: 'dup', version: '1.0.0', license: 'MIT', repository: '' };
  const b = { name: 'dup', version: '1.0.0', license: 'UNKNOWN', repository: '' };
  assert.deepEqual(sortUnique([a, b]), sortUnique([b, a]));
});

