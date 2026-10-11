// Regression tests for the release-tooling fixes (tag regex, TOML headers, RFC 3339 dates, notices dedup).
// Run: node --test "scripts/release/*.test.mjs"

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { REPO_ROOT, isSemver } from './common.mjs';
import { tomlValue } from './check-versions.mjs';
import { isRfc3339Utc } from './make-latest-json.mjs';
import { sortUnique } from './write-notices.mjs';

const TIMEOUT = 60_000;

test('fixer: tomlValue accepts a table header with a trailing comment', { timeout: TIMEOUT }, () => {
  const toml = '[workspace.package]   # shared metadata\r\nversion = "2.0.0"\r\n';
  assert.equal(tomlValue(toml, 'workspace.package', 'version'), '2.0.0');
  assert.equal(tomlValue('[package]#no-space\nversion = "1.0.0"\n', 'package', 'version'), '1.0.0');
});

test('fixer: isRfc3339Utc rejects impossible dates and times, keeps valid leap days', { timeout: TIMEOUT }, () => {
  assert.equal(isRfc3339Utc('2024-02-29T00:00:00Z'), true);
  assert.equal(isRfc3339Utc('2026-02-29T00:00:00Z'), false);
  assert.equal(isRfc3339Utc('2026-04-31T00:00:00Z'), false);
  assert.equal(isRfc3339Utc('2026-10-10T23:59:59Z'), true);
  assert.equal(isRfc3339Utc('2026-10-10T12:60:00Z'), false);
  assert.equal(isRfc3339Utc('2026-10-10T12:00:60Z'), false);
});

test('fixer: sortUnique picks the same duplicate regardless of input order', { timeout: TIMEOUT }, () => {
  const known = { name: 'dup', version: '1.0.0', license: 'Apache-2.0', repository: '' };
  const unknown = { name: 'dup', version: '1.0.0', license: 'UNKNOWN', repository: '' };
  const expected = [known];
  assert.deepEqual(sortUnique([unknown, known]), expected);
  assert.deepEqual(sortUnique([known, unknown]), expected);
});

test('fixer: sortUnique breaks license ties by string order', { timeout: TIMEOUT }, () => {
  const mit = { name: 'dup', version: '1.0.0', license: 'MIT', repository: '' };
  const gpl = { name: 'dup', version: '1.0.0', license: 'GPL-3.0', repository: '' };
  assert.deepEqual(sortUnique([mit, gpl]), [gpl]);
  assert.deepEqual(sortUnique([gpl, mit]), [gpl]);
});

test('fixer: the workflow tag regex rejects numeric prerelease identifiers with leading zeros', { timeout: TIMEOUT }, () => {
  const text = readFileSync(join(REPO_ROOT, '.github', 'workflows', 'release.yml'), 'utf8');
  const line = text.split('\n').find((l) => /\[\[ "\$TAG" =~ /.test(l));
  assert.ok(line, 'tag regex line not found');
  const re = new RegExp(line.match(/=~ (\^\S+\$) \]\]/)[1]);
  assert.equal(re.test('v1.2.3-01'), false);
  assert.equal(re.test('v1.2.3-0'), true);
  assert.equal(re.test('v1.2.3-rc.1'), true);
  assert.equal(re.test('v1.2.3-1a.2'), true);
  assert.equal(isSemver('1.2.3-01'), false);
});
