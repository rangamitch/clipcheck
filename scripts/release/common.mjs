// Shared helpers for the release scripts (scripts/release/*.mjs).
// Node built-ins only. Pure helpers are exported so release.test.mjs can cover them.

import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** Repository root (two levels above this folder). */
export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

/** Official semver 2.0.0 regular expression (anchored). */
export const SEMVER_RE =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$/;

/** GitHub OWNER/REPO. */
export const REPO_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

export function isSemver(v) {
  return typeof v === 'string' && SEMVER_RE.test(v);
}

/** Thrown for bad command-line usage (prints usage, exit 1). */
export class UsageError extends Error {}

/**
 * Parses `--name value`, `--name=value` and boolean `--flag` arguments.
 * `options` maps option name -> { type: 'string' | 'boolean', required?, default?, help }.
 * Returns { help: boolean, values }.
 */
export function parseArgs(argv, options) {
  const values = {};
  for (const [k, o] of Object.entries(options)) {
    if (o.default !== undefined) values[k] = o.default;
  }
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--help' || arg === '-h') return { help: true, values };
    if (!arg.startsWith('--')) throw new UsageError(`unexpected argument: ${arg}`);
    let name = arg.slice(2);
    let inline;
    const eq = name.indexOf('=');
    if (eq >= 0) {
      inline = name.slice(eq + 1);
      name = name.slice(0, eq);
    }
    const opt = options[name];
    if (!opt) throw new UsageError(`unknown option --${name}`);
    if (opt.type === 'boolean') {
      values[name] = true;
      continue;
    }
    let value = inline;
    if (value === undefined) {
      value = argv[++i];
      if (value === undefined || value.startsWith('--')) {
        throw new UsageError(`--${name} needs a value`);
      }
    }
    values[name] = value;
  }
  for (const [k, o] of Object.entries(options)) {
    if (o.required && values[k] === undefined) throw new UsageError(`missing required option --${k}`);
  }
  return { help: false, values };
}

/** Builds the --help text for a script. */
export function formatUsage({ name, summary, options }) {
  const lines = [`Usage: node scripts/release/${name} [options]`, '', summary, '', 'Options:'];
  for (const [k, o] of Object.entries(options)) {
    const arg = o.type === 'boolean' ? '' : ' <value>';
    const req = o.required ? ' (required)' : '';
    const def = o.default !== undefined ? ` [default: ${o.default}]` : '';
    lines.push(`  --${k}${arg}`.padEnd(34) + o.help + req + def);
  }
  lines.push('  --help'.padEnd(34) + 'Show this help and exit.');
  return lines.join('\n');
}

/**
 * Runs a script's CLI: parses args, handles --help, maps errors to exit codes.
 * `run(values)` returns an exit code (default 0) or throws.
 */
export async function cli(spec, run, argv = process.argv.slice(2)) {
  let parsed;
  try {
    parsed = parseArgs(argv, spec.options);
  } catch (e) {
    if (e instanceof UsageError) {
      process.stderr.write(`error: ${e.message}\n\n${formatUsage(spec)}\n`);
      return 1;
    }
    throw e;
  }
  if (parsed.help) {
    process.stdout.write(`${formatUsage(spec)}\n`);
    return 0;
  }
  try {
    const code = await run(parsed.values);
    return code ?? 0;
  } catch (e) {
    process.stderr.write(`error: ${e.message}\n`);
    return 1;
  }
}

/** Runs `run` through cli() only when this module is the entry point. */
export function runIfMain(metaUrl, spec, run) {
  const entry = process.argv[1];
  if (!entry || pathToFileURL(resolve(entry)).href !== metaUrl) return;
  cli(spec, run).then((code) => {
    process.exitCode = code;
  });
}

export function sha256Buffer(buf) {
  return createHash('sha256').update(buf).digest('hex');
}

export function sha256File(path) {
  return sha256Buffer(readFileSync(path));
}

export function readText(path) {
  return readFileSync(path, 'utf8');
}

export function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

/** Deterministic code-point string comparison (no locale effects). */
export function compareStrings(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}
