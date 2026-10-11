#!/usr/bin/env node
// Writes the Tauri-updater manifest (latest.json) for one release.
// Format: { version, notes, pub_date, platforms: { "windows-x86_64": { signature, url } } }
// There are no extension fields (the old FFmpeg-pack fields were removed, D58/D62).

import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { basename, dirname, resolve } from 'node:path';
import { REPO_RE, isSemver, readText, runIfMain, sha256File } from './common.mjs';

export const OPTIONS = {
  version: { type: 'string', required: true, help: 'App version, semver without the v (e.g. 0.2.0).' },
  'notes-file': { type: 'string', required: true, help: 'Text/Markdown file with the release notes.' },
  installer: { type: 'string', required: true, help: 'Path to the NSIS installer (ClipCheck_X.Y.Z_x64-setup.exe).' },
  'sig-file': { type: 'string', required: true, help: 'Path to the installer .sig file (Tauri updater signature).' },
  repo: { type: 'string', required: true, help: 'Releases repository, OWNER/REPO.' },
  'pub-date': { type: 'string', help: 'RFC 3339 UTC timestamp (default: now, whole seconds).' },
  out: { type: 'string', required: true, help: 'Output path for latest.json.' },
};

export const SPEC = {
  name: 'make-latest-json.mjs',
  summary: 'Generate the Tauri updater manifest latest.json for a release.',
  options: OPTIONS,
};

/** Current time as RFC 3339 UTC with whole seconds, e.g. 2026-10-10T12:00:00Z. */
export function rfc3339Now(date = new Date()) {
  return date.toISOString().replace(/\.\d{3}Z$/, 'Z');
}

/** Strict RFC 3339 UTC check (fraction optional). */
export function isRfc3339Utc(s) {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(\.\d+)?Z$/.exec(s);
  if (!m) return false;
  const [year, month, day, hour, minute, second] = m.slice(1, 7).map(Number);
  if (hour > 23 || minute > 59 || second > 59) return false;
  // Round-trip the calendar date: rejects e.g. 2026-02-30. setUTCFullYear avoids Date.UTC's 0-99 mapping.
  const d = new Date(0);
  d.setUTCFullYear(year, month - 1, day);
  return d.getUTCFullYear() === year && d.getUTCMonth() === month - 1 && d.getUTCDate() === day;
}

/** Installer download URL on the releases repo: .../releases/download/vX/<file> (file URL-encoded). */
export function installerUrl(repo, version, fileName) {
  return `https://github.com/${repo}/releases/download/v${version}/${encodeURIComponent(fileName)}`;
}

/**
 * Builds the manifest object. Throws on invalid input. Key order is fixed for deterministic output.
 */
export function buildLatestJson({
  version,
  notes,
  pubDate,
  repo,
  installerFileName,
  signature,
}) {
  const v = version.startsWith('v') ? version.slice(1) : version;
  if (!isSemver(v)) throw new Error(`version "${version}" is not valid semver`);
  if (!REPO_RE.test(repo)) throw new Error(`repo "${repo}" must be OWNER/REPO`);
  if (!isRfc3339Utc(pubDate)) throw new Error(`pub_date "${pubDate}" is not RFC 3339 UTC`);
  if (!signature) throw new Error('signature is empty');
  if (!installerFileName.endsWith('.exe')) throw new Error(`installer "${installerFileName}" is not an .exe`);

  const manifest = {
    version: v,
    notes,
    pub_date: pubDate,
    platforms: {
      'windows-x86_64': {
        signature,
        url: installerUrl(repo, v, installerFileName),
      },
    },
  };
  return manifest;
}

export async function run(values) {
  const notes = readText(resolve(values['notes-file'])).replace(/\r\n/g, '\n').trim();
  const signature = readText(resolve(values['sig-file'])).trim();
  const installerPath = resolve(values.installer);
  if (!existsSync(installerPath)) throw new Error(`installer not found: ${installerPath}`);
  // Hash the installer too so a wrong file is noticed early; the hash is not part of latest.json.
  const installerSha = sha256File(installerPath);

  const manifest = buildLatestJson({
    version: values.version,
    notes,
    pubDate: values['pub-date'] ?? rfc3339Now(),
    repo: values.repo,
    installerFileName: basename(installerPath),
    signature,
  });

  const out = resolve(values.out);
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  process.stdout.write(`wrote ${out} (installer sha256 ${installerSha})\n`);
  return 0;
}

runIfMain(import.meta.url, SPEC, run);
