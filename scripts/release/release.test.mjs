// Unit tests for the release scripts' pure functions. Run: node --test "scripts/release/*.test.mjs"

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isSemver, parseArgs, UsageError } from './common.mjs';
import { compareVersions, formatTable, tomlValue, jsonVersion } from './check-versions.mjs';
import { buildLatestJson, installerUrl, isRfc3339Utc, rfc3339Now } from './make-latest-json.mjs';
import { checkBudget } from './size-budget.mjs';
import {
  cargoPackages,
  normalizeLicense,
  npmProductionPackages,
  renderNotices,
  sortUnique,
} from './write-notices.mjs';

test('isSemver accepts releases and prereleases, rejects junk', () => {
  assert.equal(isSemver('0.1.0'), true);
  assert.equal(isSemver('8.1.3-1'), true);
  assert.equal(isSemver('1.2.3+build.5'), true);
  assert.equal(isSemver('01.2.3'), false);
  assert.equal(isSemver('1.2'), false);
  assert.equal(isSemver('v1.2.3'), false);
  assert.equal(isSemver(undefined), false);
});

test('parseArgs handles value, inline value, flags, defaults and errors', () => {
  const opts = {
    version: { type: 'string', required: true, help: '' },
    strict: { type: 'boolean', help: '' },
    'max-mb': { type: 'string', default: '40', help: '' },
  };
  const a = parseArgs(['--version', '1.0.0', '--strict'], opts);
  assert.equal(a.values.version, '1.0.0');
  assert.equal(a.values.strict, true);
  assert.equal(a.values['max-mb'], '40');
  assert.equal(parseArgs(['--version=2.0.0'], opts).values.version, '2.0.0');
  assert.equal(parseArgs(['--help'], opts).help, true);
  assert.throws(() => parseArgs([], opts), UsageError);
  assert.throws(() => parseArgs(['--version'], opts), UsageError);
  assert.throws(() => parseArgs(['--nope', 'x'], opts), UsageError);
});

test('tomlValue reads only the requested table', () => {
  const toml = '[package]\nname = "x"\nversion = "9.9.9"\n\n[workspace.package]\nversion = "0.1.0"\n';
  assert.equal(tomlValue(toml, 'workspace.package', 'version'), '0.1.0');
  assert.equal(tomlValue(toml, 'package', 'version'), '9.9.9');
  assert.equal(tomlValue(toml, 'missing', 'version'), undefined);
});

test('jsonVersion reads the top-level version', () => {
  assert.equal(jsonVersion('{"version":"0.1.0","x":{"version":"2"}}'), '0.1.0');
  assert.equal(jsonVersion('{}'), undefined);
});

test('compareVersions flags mismatches, missing values and tag differences', () => {
  const rows = [
    { name: 'ref', file: 'a', version: '0.2.0' },
    { name: 'b', file: 'b', version: '0.2.0' },
  ];
  assert.equal(compareVersions(rows, 'v0.2.0').ok, true);
  assert.equal(compareVersions(rows, '0.2.0').ok, true);
  assert.equal(compareVersions(rows, 'v0.2.1').ok, false);
  const bad = compareVersions([{ name: 'ref', file: 'a', version: '0.2.0' }, { name: 'c', file: 'c', version: undefined }]);
  assert.equal(bad.rows[1].status, 'MISSING');
  assert.equal(bad.ok, false);
  const table = formatTable(bad.rows);
  assert.match(table, /MISSING/);
  assert.match(table, /SOURCE/);
});

test('rfc3339Now produces whole-second UTC timestamps', () => {
  const s = rfc3339Now(new Date('2026-10-10T12:34:56.789Z'));
  assert.equal(s, '2026-10-10T12:34:56Z');
  assert.equal(isRfc3339Utc(s), true);
  assert.equal(isRfc3339Utc('2026-10-10 12:34:56'), false);
});

test('installerUrl encodes the file name', () => {
  assert.equal(
    installerUrl('owner/repo', '0.2.0', 'ClipCheck_0.2.0_x64-setup.exe'),
    'https://github.com/owner/repo/releases/download/v0.2.0/ClipCheck_0.2.0_x64-setup.exe',
  );
  assert.equal(
    installerUrl('owner/repo', '0.2.0', 'Clip Check#1.exe'),
    'https://github.com/owner/repo/releases/download/v0.2.0/Clip%20Check%231.exe',
  );
});

test('buildLatestJson emits the Tauri updater shape plus extensions', () => {
  const m = buildLatestJson({
    version: '0.2.0',
    notes: 'Notes',
    pubDate: '2026-10-10T12:00:00Z',
    repo: 'owner/repo',
    installerFileName: 'ClipCheck_0.2.0_x64-setup.exe',
    signature: 'SIG',
  });
  assert.deepEqual(Object.keys(m), ['version', 'notes', 'pub_date', 'platforms']);
  assert.deepEqual(m.platforms['windows-x86_64'], {
    signature: 'SIG',
    url: 'https://github.com/owner/repo/releases/download/v0.2.0/ClipCheck_0.2.0_x64-setup.exe',
  });
});

test('buildLatestJson rejects bad input', () => {
  const base = {
    version: '0.2.0',
    notes: '',
    pubDate: '2026-10-10T12:00:00Z',
    repo: 'owner/repo',
    installerFileName: 'x.exe',
    signature: 'SIG',
  };
  assert.throws(() => buildLatestJson({ ...base, version: '0.2' }), /semver/);
  assert.throws(() => buildLatestJson({ ...base, repo: 'noslash' }), /OWNER\/REPO/);
  assert.throws(() => buildLatestJson({ ...base, signature: '' }), /signature/);
  assert.throws(() => buildLatestJson({ ...base, pubDate: 'yesterday' }), /RFC 3339/);
});

test('checkBudget compares MiB against the budget', () => {
  assert.equal(checkBudget(40 * 1024 * 1024, 40).ok, true);
  assert.equal(checkBudget(40 * 1024 * 1024 + 1, '40').ok, false);
  assert.throws(() => checkBudget(1, 'abc'), /positive number/);
});

test('normalizeLicense handles strings, objects, arrays and missing values', () => {
  assert.equal(normalizeLicense('MIT'), 'MIT');
  assert.equal(normalizeLicense({ type: 'ISC' }), 'ISC');
  assert.equal(normalizeLicense(['MIT', 'Apache-2.0']), 'MIT OR Apache-2.0');
  assert.equal(normalizeLicense(undefined), 'UNKNOWN');
  assert.equal(normalizeLicense('  '), 'UNKNOWN');
});

test('cargoPackages skips workspace and path packages', () => {
  const meta = {
    packages: [
      { name: 'ours', version: '0.1.0', license: 'Apache-2.0', repository: null, source: null },
      { name: 'serde', version: '1.0.0', license: 'MIT OR Apache-2.0', repository: 'git+https://x/serde', source: 'registry+x' },
    ],
  };
  assert.deepEqual(cargoPackages(meta), [
    { name: 'serde', version: '1.0.0', license: 'MIT OR Apache-2.0', repository: 'https://x/serde' },
  ]);
});

test('npmProductionPackages drops dev-only entries and reads installed metadata', () => {
  const lock = {
    packages: {
      '': {},
      'node_modules/svelte': { version: '5.0.0', dev: false },
      'node_modules/vite': { version: '6.0.0', dev: true },
      'node_modules/@scope/pkg': { version: '1.2.3' },
    },
  };
  const installed = {
    'node_modules/svelte': { license: 'MIT', repository: { url: 'git+https://github.com/sveltejs/svelte' } },
    'node_modules/@scope/pkg': { license: 'Apache-2.0' },
  };
  const pkgs = sortUnique(npmProductionPackages(lock, (k) => installed[k]));
  assert.deepEqual(pkgs.map((p) => p.name), ['@scope/pkg', 'svelte']);
  assert.equal(pkgs[1].repository, 'https://github.com/sveltejs/svelte');
});

test('renderNotices is deterministic and lists every section', () => {
  const input = {
    crates: [{ name: 'serde', version: '1.0.0', license: 'MIT', repository: 'https://r' }],
    npm: [{ name: 'svelte', version: '5.0.0', license: 'MIT', repository: '' }],
  };
  const a = renderNotices(input);
  assert.equal(a, renderNotices(input));
  assert.match(a, /== Rust crates \(cargo metadata\) \(1\) ==/);
  assert.match(a, /- serde 1\.0\.0 \| MIT \| https:\/\/r/);
  assert.match(a, /- svelte 5\.0\.0 \| MIT\n/);
  assert.doesNotMatch(a, /FFmpeg/);
});

test('sortUnique orders by name then version and removes duplicates', () => {
  const out = sortUnique([
    { name: 'b', version: '1' },
    { name: 'a', version: '2' },
    { name: 'a', version: '1' },
    { name: 'a', version: '1' },
  ]);
  assert.deepEqual(out.map((p) => `${p.name}@${p.version}`), ['a@1', 'a@2', 'b@1']);
});
