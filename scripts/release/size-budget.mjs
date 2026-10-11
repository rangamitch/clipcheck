#!/usr/bin/env node
// Fails when a release artifact is larger than its budget. MB here means MiB (1024 * 1024 bytes).

import { statSync } from 'node:fs';
import { resolve } from 'node:path';
import { runIfMain } from './common.mjs';

const MIB = 1024 * 1024;

export const OPTIONS = {
  file: { type: 'string', required: true, help: 'File to measure (e.g. the NSIS installer).' },
  'max-mb': { type: 'string', default: '40', help: 'Budget in MiB.' },
};

export const SPEC = {
  name: 'size-budget.mjs',
  summary: 'Fail when a file exceeds the size budget (MiB). Used for the installer (doc 11 §1, §5).',
  options: OPTIONS,
};

/** Returns { bytes, mib, budgetMib, ok }. `maxMb` may be a number or numeric string. */
export function checkBudget(bytes, maxMb) {
  const budgetMib = Number(maxMb);
  if (!Number.isFinite(budgetMib) || budgetMib <= 0) throw new Error(`--max-mb must be a positive number, got "${maxMb}"`);
  return { bytes, mib: bytes / MIB, budgetMib, ok: bytes <= budgetMib * MIB };
}

export async function run(values) {
  const path = resolve(values.file);
  const { size } = statSync(path);
  const r = checkBudget(size, values['max-mb']);
  const line = `${path}: ${r.mib.toFixed(2)} MiB (${size} bytes), budget ${r.budgetMib} MiB`;
  if (!r.ok) {
    process.stderr.write(`FAIL ${line}: over budget\n`);
    return 1;
  }
  process.stdout.write(`OK ${line}\n`);
  return 0;
}

runIfMain(import.meta.url, SPEC, run);
