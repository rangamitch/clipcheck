// Tests for the license-text collection in write-notices.mjs. Uses a temp directory with fake
// license files only (no cargo, no network). Run: node --test scripts/release/notices-texts.test.mjs

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { collectTexts, licenseFilesIn, renderNotices } from './write-notices.mjs';

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), 'notices-texts-'));
  try {
    fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('licenseFilesIn matches license-like names and skips directories, READMEs and nested files', () => {
  withTempDir((dir) => {
    for (const name of ['LICENSE', 'LICENSE-MIT', 'COPYING', 'NOTICE.txt', 'license.md', 'README.md']) {
      writeFileSync(join(dir, name), 'text');
    }
    mkdirSync(join(dir, 'LICENSE_dir'));
    mkdirSync(join(dir, 'src'));
    writeFileSync(join(dir, 'src', 'LICENSE'), 'nested');

    const names = licenseFilesIn(dir).map((p) => p.slice(dir.length + 1));
    assert.deepEqual(names, ['COPYING', 'LICENSE', 'LICENSE-MIT', 'NOTICE.txt', 'license.md']);
    assert.deepEqual(licenseFilesIn(join(dir, 'does-not-exist')), []);
  });
});

test('collectTexts dedups texts that differ only by CRLF, trailing spaces and blank lines', () => {
  const files = {
    '/x/a/LICENSE': 'MIT text\r\nline  \r\n\r\n',
    '/x/b/LICENSE': '\nMIT text\nline\n',
    '/x/c/LICENSE': '   \n\n',
    '/x/d/LICENSE': 'Apache text\n',
  };
  const readText = (path) => files[path];
  const pkgs = [
    { name: 'b', version: '2', licenseFiles: ['/x/b/LICENSE'] },
    { name: 'a', version: '1', licenseFiles: ['/x/a/LICENSE'] },
    { name: 'c', version: '3', licenseFiles: ['/x/c/LICENSE', '/x/missing/LICENSE'] },
    { name: 'd', version: '4', licenseFiles: ['/x/d/LICENSE'] },
  ];
  const texts = collectTexts(pkgs, readText);
  assert.deepEqual(texts, [
    { text: 'MIT text\nline', packages: ['a 1', 'b 2'] },
    { text: 'Apache text', packages: ['d 4'] },
  ]);
});

test('collectTexts is deterministic and independent of input order', () => {
  const files = { '/p/1': 'Same\n', '/p/2': 'Same\r\n', '/p/3': 'Other\n' };
  const readText = (path) => files[path];
  const pkgs = [
    { name: 'z', version: '1', licenseFiles: ['/p/1', '/p/3'] },
    { name: 'm', version: '1', licenseFiles: ['/p/2'] },
  ];
  const forward = collectTexts(pkgs, readText);
  assert.deepEqual(collectTexts(pkgs, readText), forward);
  assert.deepEqual(collectTexts([...pkgs].reverse(), readText), forward);
  assert.deepEqual(forward, [
    { text: 'Same', packages: ['m 1', 'z 1'] },
    { text: 'Other', packages: ['z 1'] },
  ]);
});

test('renderNotices without texts is identical to renderNotices with an empty texts array', () => {
  const input = {
    crates: [{ name: 'serde', version: '1.0.0', license: 'MIT', repository: 'https://r' }],
    npm: [{ name: 'svelte', version: '5.0.0', license: 'MIT', repository: '' }],
  };
  const without = renderNotices(input);
  assert.equal(renderNotices({ ...input, texts: [] }), without);
  assert.equal(renderNotices({ ...input, texts: undefined }), without);
  assert.doesNotMatch(without, /License texts/);
});

test('renderNotices with texts appends the license-text section', () => {
  const texts = [
    { text: 'MIT text\nline', packages: ['a 1', 'b 2'] },
    { text: 'Apache text', packages: ['d 4'] },
  ];
  const out = renderNotices({ crates: [], npm: [], texts });
  assert.match(out, /== License texts \(2\) ==/);
  assert.match(out, /---- a 1, b 2 ----\nMIT text\nline\n\n---- d 4 ----\nApache text\n\n$/);
  assert.equal(out, renderNotices({ crates: [], npm: [], texts }));
});
