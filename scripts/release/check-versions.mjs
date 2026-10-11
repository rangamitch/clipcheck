#!/usr/bin/env node
// Checks that every version source of ClipCheck agrees (and optionally matches a release tag).
//
// Sources:
//   - root Cargo.toml                   [workspace.package] version  (reference)
//   - apps/ui/src-tauri/Cargo.toml      [package] version
//   - apps/ui/src-tauri/tauri.conf.json "version" (optional: Tauri then uses the crate version, see issue #139)
//   - apps/ui/package.json              "version"
//   - --tag vX.Y.Z (optional)
// Exit 0 when all agree and are valid semver; exit 1 and print a table otherwise.

import { join } from 'node:path';
import { REPO_ROOT, isSemver, readText, runIfMain } from './common.mjs';

export const OPTIONS = {
  tag: { type: 'string', help: 'Release tag to compare against, e.g. v1.2.3 (optional).' },
  root: { type: 'string', help: 'Repository root (default: this repository).', default: REPO_ROOT },
};

export const SPEC = {
  name: 'check-versions.mjs',
  summary: 'Verify that all version sources agree and match the optional release tag.',
  options: OPTIONS,
};

/** Reads `key = "value"` from the TOML table `section`. Returns undefined when absent. */
export function tomlValue(text, section, key) {
  let inSection = false;
  const keyRe = new RegExp(`^\\s*${key}\\s*=\\s*"([^"]*)"`);
  for (const line of text.split(/\r?\n/)) {
    const header = line.match(/^\s*\[([^\]]+)\]\s*(#.*)?$/);
    if (header) {
      inSection = header[1].trim() === section;
      continue;
    }
    if (inSection) {
      const m = line.match(keyRe);
      if (m) return m[1];
    }
  }
  return undefined;
}

/** Top-level "version" field of a JSON document. */
export function jsonVersion(text) {
  const obj = JSON.parse(text);
  return typeof obj.version === 'string' ? obj.version : undefined;
}

/**
 * Compares version rows. `rows` = [{ name, file, version }] in display order; the first row is the
 * reference. `tag` is optional ("v1.2.3" or "1.2.3"). Returns { rows, ok } where each row has
 * `status` ('ok' | 'MISMATCH' | 'MISSING' | 'INVALID').
 */
export function compareVersions(rows, tag) {
  const reference = rows[0]?.version;
  const out = rows.map((r) => {
    let status = 'ok';
    if (r.version === undefined) status = 'MISSING';
    else if (!isSemver(r.version)) status = 'INVALID';
    else if (r.version !== reference) status = 'MISMATCH';
    return { ...r, status };
  });
  if (tag !== undefined) {
    const bare = tag.startsWith('v') ? tag.slice(1) : tag;
    let status = 'ok';
    if (!isSemver(bare)) status = 'INVALID';
    else if (bare !== reference) status = 'MISMATCH';
    out.push({ name: 'release tag', file: tag, version: bare, status });
  }
  return { rows: out, ok: out.every((r) => r.status === 'ok') };
}

/** Renders rows as a fixed-width text table. */
export function formatTable(rows) {
  const headers = ['SOURCE', 'FILE', 'VERSION', 'STATUS'];
  const data = rows.map((r) => [r.name, r.file, r.version ?? '-', r.status]);
  const widths = headers.map((h, i) => Math.max(h.length, ...data.map((d) => String(d[i]).length)));
  const line = (cells) => cells.map((c, i) => String(c).padEnd(widths[i])).join('  ').trimEnd();
  return [line(headers), line(widths.map((w) => '-'.repeat(w))), ...data.map(line)].join('\n');
}

/** Reads all sources from disk under `root`. */
export function readSources(root) {
  const cargoRoot = readText(join(root, 'Cargo.toml'));
  const cargoTauri = readText(join(root, 'apps', 'ui', 'src-tauri', 'Cargo.toml'));
  const conf = readText(join(root, 'apps', 'ui', 'src-tauri', 'tauri.conf.json'));
  const pkg = readText(join(root, 'apps', 'ui', 'package.json'));
  return [
    { name: 'workspace (reference)', file: 'Cargo.toml', version: tomlValue(cargoRoot, 'workspace.package', 'version') },
    { name: 'Tauri crate', file: 'apps/ui/src-tauri/Cargo.toml', version: tomlValue(cargoTauri, 'package', 'version') },
    { name: 'Tauri config', file: 'apps/ui/src-tauri/tauri.conf.json', version: jsonVersion(conf) ?? tomlValue(cargoTauri, 'package', 'version') },
    { name: 'UI package', file: 'apps/ui/package.json', version: jsonVersion(pkg) },
  ];
}

export async function run(values) {
  const rows = readSources(values.root);
  const result = compareVersions(rows, values.tag);
  process.stdout.write(`${formatTable(result.rows)}\n`);
  if (!result.ok) {
    process.stderr.write('error: version sources disagree or are invalid (see table above)\n');
    return 1;
  }
  process.stdout.write('All version sources agree.\n');
  return 0;
}

runIfMain(import.meta.url, SPEC, run);
