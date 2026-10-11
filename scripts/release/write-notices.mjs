#!/usr/bin/env node
// Writes licenses/THIRD-PARTY-NOTICES.txt: license and repository metadata for every third-party
// Rust crate (both Cargo workspaces) and every production npm dependency of apps/ui, followed by the
// license texts found in each package directory (deduplicated, so MIT/BSD/ISC/Apache notices travel
// with binary copies as those licenses require). Output is deterministic (sorted, no timestamps).
// Requires `cargo` on PATH. Files are only read from package directories; nothing is downloaded.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { REPO_ROOT, compareStrings, readJson, runIfMain } from './common.mjs';

export const OPTIONS = {
  out: {
    type: 'string',
    default: join(REPO_ROOT, 'licenses', 'THIRD-PARTY-NOTICES.txt'),
    help: 'Output file.',
  },
  strict: { type: 'boolean', help: 'Exit 1 when any package has no license declared.' },
};

export const SPEC = {
  name: 'write-notices.mjs',
  summary: 'Collect crate and npm license metadata into THIRD-PARTY-NOTICES.txt.',
  options: OPTIONS,
};

/** Normalizes a license field (string, {type}, array or missing) to one string. */
export function normalizeLicense(value) {
  if (typeof value === 'string' && value.trim() !== '') return value.trim();
  if (Array.isArray(value) && value.length > 0) return value.map(normalizeLicense).join(' OR ');
  if (value && typeof value === 'object' && typeof value.type === 'string') return value.type;
  return 'UNKNOWN';
}

/** Normalizes a repository field (string or {url}) to a plain URL string, or ''. */
export function normalizeRepository(value) {
  const url = typeof value === 'string' ? value : value && typeof value.url === 'string' ? value.url : '';
  return url.replace(/^git\+/, '');
}

const LICENSE_FILE_RE = /^(licen[cs]e|copying|notice|copyright|unlicense)([-._].*)?$/i;

/**
 * Absolute paths of the license-like files directly inside `dir` (LICENSE, LICENSE-MIT, COPYING,
 * NOTICE.txt, license.md, ...), sorted. Directories are skipped and nothing is searched recursively.
 * A missing or unreadable directory yields [].
 */
export function licenseFilesIn(dir) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return [];
  }
  return entries
    .filter((d) => d.isFile() && LICENSE_FILE_RE.test(d.name))
    .map((d) => join(dir, d.name))
    .sort(compareStrings);
}

/**
 * Third-party packages from one `cargo metadata --format-version 1` document.
 * Workspace members and path dependencies have source === null and are skipped.
 * When the package has a manifest_path, `licenseFiles` lists its license-like files in that directory
 * plus the file named by `license_file` (if any).
 */
export function cargoPackages(metadata) {
  // With `--filter-platform`, only packages in the resolved graph are built for the target; the
  // `packages` list can still name others. Keep only resolved ones when a resolve graph is present.
  const nodes = metadata.resolve?.nodes;
  const resolved = Array.isArray(nodes) ? new Set(nodes.map((n) => n.id)) : null;
  return (metadata.packages ?? [])
    .filter((p) => p.source !== null && p.source !== undefined)
    .filter((p) => resolved === null || resolved.has(p.id))
    .map((p) => {
      const pkg = {
        name: p.name,
        version: p.version,
        license: normalizeLicense(p.license),
        repository: normalizeRepository(p.repository),
      };
      if (typeof p.manifest_path === 'string') {
        const dir = dirname(p.manifest_path);
        const files = new Set(licenseFilesIn(dir));
        if (typeof p.license_file === 'string' && p.license_file.trim() !== '') {
          files.add(join(dir, p.license_file));
        }
        pkg.licenseFiles = [...files].sort(compareStrings);
      }
      return pkg;
    });
}

/** One npm entry; `licenseFiles` is set only when `dirOf` maps the lockfile key to a directory. */
function npmEntry(key, name, entry, installed, dirOf) {
  const pkg = {
    name,
    version: entry.version ?? installed.version ?? '',
    license: normalizeLicense(entry.license ?? installed.license),
    repository: normalizeRepository(installed.repository),
  };
  if (dirOf) pkg.licenseFiles = licenseFilesIn(dirOf(key));
  return pkg;
}

/**
 * Production packages from an npm lockfile (v2/v3 `packages` map). Dev-only entries are skipped.
 * `readPkg(key)` returns the parsed package.json of an installed package (or undefined); it is used
 * for license and repository because lockfiles often omit them. `dirOf(key)` (optional) maps a lockfile
 * key to the package directory whose license files are listed in `licenseFiles`.
 */
export function npmProductionPackages(lock, readPkg, dirOf) {
  const marker = 'node_modules/';
  const out = [];
  for (const [key, entry] of Object.entries(lock.packages ?? {})) {
    if (key === '' || !key.includes(marker) || entry.dev === true) continue;
    const name = key.slice(key.lastIndexOf(marker) + marker.length);
    const installed = readPkg(key) ?? {};
    out.push(npmEntry(key, name, entry, installed, dirOf));
  }
  return out;
}

/**
 * npm packages that are devDependencies but are compiled into the shipped UI bundle (their runtime
 * code ends up in apps/ui/dist). Listed so their notices are not lost.
 */
export const BUNDLED_DEV_PACKAGES = ['svelte'];

/**
 * Entries for `names` from an npm lockfile, whether or not they are dev-only. Missing names are skipped.
 * `readPkg(key)` and `dirOf(key)` are the same helpers as in `npmProductionPackages`.
 */
export function npmNamedPackages(lock, readPkg, names, dirOf) {
  const out = [];
  for (const name of names) {
    const key = `node_modules/${name}`;
    const entry = (lock.packages ?? {})[key];
    if (!entry) continue;
    const installed = readPkg(key) ?? {};
    out.push(npmEntry(key, name, entry, installed, dirOf));
  }
  return out;
}

/**
 * Sorts by name, then version, and removes duplicates of the same name+version. Among duplicates the
 * winner is chosen by a total order (known license before 'UNKNOWN', then license, then repository),
 * so the output never depends on input order. The first entry in that order is kept.
 */
export function sortUnique(pkgs) {
  const unknownRank = (p) => (p.license === 'UNKNOWN' ? 1 : 0);
  const sorted = [...pkgs].sort(
    (a, b) =>
      compareStrings(a.name, b.name) ||
      compareStrings(a.version, b.version) ||
      unknownRank(a) - unknownRank(b) ||
      compareStrings(a.license, b.license) ||
      compareStrings(a.repository ?? '', b.repository ?? ''),
  );
  return sorted.filter(
    (p, i) => i === 0 || p.name !== sorted[i - 1].name || p.version !== sorted[i - 1].version,
  );
}

/**
 * Normalizes a license file for comparison and output: CRLF/CR become LF, trailing whitespace is
 * removed from each line, and leading and trailing blank lines are trimmed.
 */
export function normalizeLicenseText(raw) {
  const body = raw
    .replace(/\r\n?/g, '\n')
    .split('\n')
    .map((line) => line.trimEnd())
    .join('\n');
  return body.replace(/^\n+/, '').replace(/\n+$/, '');
}

/**
 * Groups the license files of `pkgs` by normalized text. `readText(path)` returns the file text or
 * undefined (missing file). Returns [{ text, packages }] where `packages` are "name version" labels
 * (sorted, unique); groups are sorted by their first package, then by text. Empty texts are skipped.
 */
export function collectTexts(pkgs, readText) {
  const byText = new Map();
  for (const p of pkgs) {
    const label = `${p.name} ${p.version}`;
    for (const file of p.licenseFiles ?? []) {
      const raw = readText(file);
      if (typeof raw !== 'string') continue;
      const text = normalizeLicenseText(raw);
      if (text === '') continue;
      if (!byText.has(text)) byText.set(text, new Set());
      byText.get(text).add(label);
    }
  }
  return [...byText.entries()]
    .map(([text, labels]) => ({ text, packages: [...labels].sort(compareStrings) }))
    .sort((a, b) => compareStrings(a.packages[0], b.packages[0]) || compareStrings(a.text, b.text));
}

function section(title, pkgs) {
  const lines = [`== ${title} (${pkgs.length}) ==`];
  for (const p of pkgs) {
    lines.push(`- ${p.name} ${p.version} | ${p.license}${p.repository ? ` | ${p.repository}` : ''}`);
  }
  return lines.join('\n');
}

/**
 * Renders the notices file text. Pure: no clock, no environment. `texts` (from collectTexts) is
 * optional; when it is missing or empty the output has no license-text section.
 */
export function renderNotices({ crates, npm, texts }) {
  const hasTexts = Array.isArray(texts) && texts.length > 0;
  const parts = [
    'THIRD-PARTY NOTICES',
    '',
    'ClipCheck is licensed under Apache-2.0. The components below are used to build or',
    hasTexts
      ? 'ship ClipCheck. Each keeps its own license; license texts are listed at the end.'
      : 'ship ClipCheck. Each keeps its own license; full texts are in the upstream packages.',
    'Format per line: name version | license | repository.',
    '',
    section('Rust crates (cargo metadata)', crates),
    '',
    section('npm packages shipped in the UI bundle (apps/ui)', npm),
    '',
  ];
  if (hasTexts) {
    parts.push(`== License texts (${texts.length}) ==`);
    for (const group of texts) {
      parts.push(`---- ${group.packages.join(', ')} ----`, group.text, '');
    }
    parts.push('');
  }
  return parts.join('\n');
}

/** Runs `cargo metadata` for one manifest and returns the parsed JSON. */
export function cargoMetadata(manifestPath) {
  const stdout = execFileSync(
    'cargo',
    // Only the shipped target: crates for other platforms are never in the installer.
    ['metadata', '--format-version', '1', '--filter-platform', 'x86_64-pc-windows-msvc', '--manifest-path', manifestPath],
    { cwd: REPO_ROOT, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 },
  );
  return JSON.parse(stdout);
}

/** File text, or undefined when the file cannot be read. */
function readTextOrUndefined(path) {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return undefined;
  }
}

export async function run(values) {
  const uiDir = join(REPO_ROOT, 'apps', 'ui');
  const crates = sortUnique([
    ...cargoPackages(cargoMetadata(join(REPO_ROOT, 'Cargo.toml'))),
    ...cargoPackages(cargoMetadata(join(uiDir, 'src-tauri', 'Cargo.toml'))),
  ]);
  const lock = readJson(join(uiDir, 'package-lock.json'));
  const readPkg = (key) => {
    const file = join(uiDir, key, 'package.json');
    return existsSync(file) ? readJson(file) : undefined;
  };
  const dirOf = (key) => join(uiDir, key);
  const npm = sortUnique([
    ...npmProductionPackages(lock, readPkg, dirOf),
    ...npmNamedPackages(lock, readPkg, BUNDLED_DEV_PACKAGES, dirOf),
  ]);

  const unknown = [...crates, ...npm].filter((p) => p.license === 'UNKNOWN');
  const texts = collectTexts([...crates, ...npm], readTextOrUndefined);
  const text = renderNotices({ crates, npm, texts });

  const out = resolve(values.out);
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, text, 'utf8');
  process.stdout.write(
    `wrote ${out}: ${crates.length} crates, ${npm.length} npm packages, ${texts.length} license texts\n`,
  );
  if (unknown.length > 0) {
    const list = unknown.map((p) => `${p.name} ${p.version}`).join(', ');
    process.stderr.write(`warning: no license declared for: ${list}\n`);
    if (values.strict) return 1;
  }
  return 0;
}

runIfMain(import.meta.url, SPEC, run);
