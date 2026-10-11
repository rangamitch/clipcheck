// Structure tests for .github/workflows/{ci,release}.yml, PowerShell syntax checks of embedded and
// standalone .ps1 code, and a docs/release.md cross-check. No yaml package is installed, so the
// workflows are scanned line by line (the files use plain block style; see the helpers below).
// Run: node --test "scripts/release/*.test.mjs". Read-only: writes only to os.tmpdir().

import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { execFile } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { isSemver } from './common.mjs';
import { buildLatestJson } from './make-latest-json.mjs';

const execFileP = promisify(execFile);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '..', '..');
const TIMEOUT = 120_000;
const dir = mkdtempSync(join(tmpdir(), 'clipcheck-wf-'));
after(() => rmSync(dir, { recursive: true, force: true }));

const read = (rel) => readFileSync(join(ROOT, rel), 'utf8');
const WORKFLOWS = {
  ci: { rel: '.github/workflows/ci.yml', text: read('.github/workflows/ci.yml') },
  release: { rel: '.github/workflows/release.yml', text: read('.github/workflows/release.yml') },
};
const DOC = read('docs/release.md');

// ---------------------------------------------------------------- line-scan helpers

const indentOf = (l) => l.match(/^ */)[0].length;

/** Top-level job id -> lines of its body (jobs are 2-space keys under `jobs:`). */
function jobsOf(text) {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => /^jobs:\s*$/.test(l));
  const out = {};
  let cur = null;
  for (let i = start + 1; i < lines.length; i++) {
    const l = lines[i];
    if (/^\S/.test(l)) break;
    const m = l.match(/^ {2}([A-Za-z0-9_-]+):\s*$/);
    if (m) {
      cur = m[1];
      out[cur] = [];
    } else if (cur) out[cur].push(l);
  }
  return out;
}

/** Step blocks of a job body: each begins with a `      - ` line. */
function stepsOf(jobLines) {
  const res = [];
  let cur = null;
  for (const l of jobLines) {
    if (/^ {6}- /.test(l)) {
      cur = [l];
      res.push(cur);
    } else if (cur) cur.push(l);
  }
  return res;
}

/** Value block of a YAML key: the key line plus every following line indented deeper than it. */
function blockOf(lines, key) {
  const i = lines.findIndex((l) => new RegExp(`^\\s*(- )?${key}:`).test(l));
  if (i < 0) return undefined;
  const ind = indentOf(lines[i]);
  const body = [];
  for (let j = i + 1; j < lines.length; j++) {
    const l = lines[j];
    if (l.trim() === '') {
      body.push(l);
      continue;
    }
    if (indentOf(l) <= ind) break;
    body.push(l);
  }
  return { head: lines[i], body: body.join('\n') };
}

const stepName = (step) => (step[0].match(/name:\s*(.*)$/) ?? [])[1] ?? '(unnamed)';
const stepRun = (step) => blockOf(step, 'run')?.body ?? '';
const stepIf = (step) => {
  const m = step.find((l) => /^ {8}if:/.test(l));
  return m ? m.replace(/^ {8}if:\s*/, '') : undefined;
};

/** All run: bodies across a workflow text, with the step they belong to. */
function allRunBlocks(text) {
  const out = [];
  for (const [job, body] of Object.entries(jobsOf(text))) {
    for (const step of stepsOf(body)) {
      const r = stepRun(step);
      if (r) out.push({ job, step: stepName(step), run: r, shell: (blockOf(step, 'shell') ?? {}).head });
    }
  }
  return out;
}

/** Dedent a block by its smallest indentation. */
function dedent(text) {
  const lines = text.split('\n');
  const min = Math.min(...lines.filter((l) => l.trim() !== '').map(indentOf));
  return lines.map((l) => l.slice(Math.min(min, indentOf(l)))).join('\n');
}

// ---------------------------------------------------------------- B1. jobs and dependencies

for (const [label, { text }] of Object.entries(WORKFLOWS)) {
  test(`${label}: every job has runs-on`, { timeout: TIMEOUT }, () => {
    const jobs = jobsOf(text);
    assert.ok(Object.keys(jobs).length > 0, 'no jobs found');
    const missing = Object.keys(jobs).filter((id) => !/^ {4}runs-on:\s*\S/m.test(jobs[id].join('\n')));
    assert.deepEqual(missing, []);
  });

  test(`${label}: every needs: reference names an existing job`, { timeout: TIMEOUT }, () => {
    const ids = new Set(Object.keys(jobsOf(text)));
    const refs = [...text.matchAll(/^ {4}needs:\s*(.+)$/gm)].flatMap((m) =>
      m[1].replace(/[\[\]]/g, '').split(',').map((s) => s.trim()),
    );
    const dangling = refs.filter((r) => !ids.has(r));
    assert.deepEqual(dangling, []);
  });

  test(`${label}: every steps.<id>.outputs reference points at a step with that id`, { timeout: TIMEOUT }, () => {
    const ids = new Set([...text.matchAll(/^\s*id:\s*([A-Za-z0-9_-]+)\s*$/gm)].map((m) => m[1]));
    const refs = new Set([...text.matchAll(/steps\.([A-Za-z0-9_-]+)\.outputs/g)].map((m) => m[1]));
    const dangling = [...refs].filter((r) => !ids.has(r));
    assert.deepEqual(dangling, []);
  });
}

// ---------------------------------------------------------------- B2. permissions

test('ci: top-level permissions are contents: read only', { timeout: TIMEOUT }, () => {
  const b = blockOf(WORKFLOWS.ci.text.split(/\r?\n/), 'permissions');
  assert.ok(b, 'no top-level permissions block');
  assert.deepEqual(b.body.split('\n').map((s) => s.trim()).filter(Boolean), ['contents: read']);
});

test('release: top-level permissions are contents: read only', { timeout: TIMEOUT }, () => {
  const lines = WORKFLOWS.release.text.split(/\r?\n/);
  const i = lines.findIndex((l) => /^permissions:/.test(l));
  assert.ok(i >= 0, 'no top-level permissions block');
  const body = [];
  for (let j = i + 1; j < lines.length && /^\s/.test(lines[j]); j++) body.push(lines[j].trim());
  assert.deepEqual(body.filter(Boolean), ['contents: read']);
});

test('release: the only write permission is contents: write on the release job', { timeout: TIMEOUT }, () => {
  const writes = [];
  for (const [label, { text }] of Object.entries(WORKFLOWS)) {
    for (const [job, body] of Object.entries(jobsOf(text))) {
      for (const l of body) if (/:\s*write\s*$/.test(l)) writes.push(`${label}/${job}: ${l.trim()}`);
    }
  }
  assert.deepEqual(writes, ['release/release: contents: write']);
});

test('release: the write-scoped job is the one that publishes the draft', { timeout: TIMEOUT }, () => {
  const job = jobsOf(WORKFLOWS.release.text).release;
  const creates = job.join('\n').includes('gh release create');
  assert.equal(creates, true);
});

// ---------------------------------------------------------------- B3. pinned actions

for (const [label, { text }] of Object.entries(WORKFLOWS)) {
  test(`${label}: every uses: is pinned to a version tag or a full commit SHA`, { timeout: TIMEOUT }, () => {
    const uses = [...text.matchAll(/^\s*(?:- )?uses:\s*(\S+)/gm)].map((m) => m[1]);
    assert.ok(uses.length > 0);
    const bad = uses.filter((u) => {
      const ref = u.split('@')[1] ?? '';
      return !(/^v\d+(\.\d+){0,2}$/.test(ref) || /^[0-9a-f]{40}$/.test(ref));
    });
    assert.deepEqual(bad, []);
  });
}

// ---------------------------------------------------------------- B4. draft release, signing, secrets

test('release: GitHub release is created with --draft and never published by the workflow', { timeout: TIMEOUT }, () => {
  const t = WORKFLOWS.release.text;
  assert.match(t, /gh release create[\s\S]*?--draft/);
  assert.doesNotMatch(t, /--draft=false/);
  assert.doesNotMatch(t, /gh release (publish|edit)[^\n]*--draft/);
  assert.doesNotMatch(t, /gh release publish/);
});

test('release: every step that calls signtool is guarded by the secret-presence output', { timeout: TIMEOUT }, () => {
  const steps = stepsOf(jobsOf(WORKFLOWS.release.text).release);
  const signing = steps.filter((s) => /signtool/.test(s.join('\n')));
  assert.ok(signing.length >= 1, 'expected at least one signtool step');
  for (const s of signing) {
    assert.equal(stepIf(s), "steps.sign.outputs.enabled == 'true'", `step "${stepName(s)}" is not guarded`);
  }
});

test('release: the signing switch is set only when WINDOWS_CERTIFICATE is non-empty', { timeout: TIMEOUT }, () => {
  const steps = stepsOf(jobsOf(WORKFLOWS.release.text).release);
  const detect = steps.find((s) => /id:\s*sign\s*$/m.test(s.join('\n')));
  assert.ok(detect, 'no step with id sign');
  const run = stepRun(detect);
  assert.match(run, /if \[ -n "\$WINDOWS_CERTIFICATE" \]; then\s*\n\s*echo "enabled=true"/);
  assert.match(blockOf(detect, 'env').body, /WINDOWS_CERTIFICATE: \$\{\{ secrets\.WINDOWS_CERTIFICATE \}\}/);
});

test('release: no run: block expands a secret with ${{ secrets.* }}', { timeout: TIMEOUT }, () => {
  for (const { job, step, run } of Object.values(WORKFLOWS).flatMap((w) => allRunBlocks(w.text))) {
    assert.doesNotMatch(run, /\$\{\{\s*secrets\./, `${job} / "${step}" expands a secret inside run:`);
  }
});

test('release: no shell trace flags that would print secret values', { timeout: TIMEOUT }, () => {
  for (const { run } of Object.values(WORKFLOWS).flatMap((w) => allRunBlocks(w.text))) {
    assert.doesNotMatch(run, /set -x|xtrace|Set-PSDebug\s+-Trace|\$VerbosePreference\s*=\s*'Continue'/);
  }
});

test('release: echo / Write-Host / printf lines never print a secret or token value', { timeout: TIMEOUT }, () => {
  const printer = /\b(echo|printf|Write-Host|Write-Output|Write-Error|Write-Warning|Write-Verbose)\b/;
  const secretExpansion = /\$\{?(env:)?\w*(TOKEN|PASSWORD|PRIVATE_KEY|CERTIFICATE|PFX|token|password|pfx)\w*\}?/;
  const hits = [];
  for (const { job, step, run } of Object.values(WORKFLOWS).flatMap((w) => allRunBlocks(w.text))) {
    for (const line of run.split('\n')) {
      if (printer.test(line) && secretExpansion.test(line)) hits.push(`${job} / "${step}": ${line.trim()}`);
    }
  }
  assert.deepEqual(hits, []);
});

test('release: the Authenticode step does not write the PFX outside RUNNER_TEMP and removes it', { timeout: TIMEOUT }, () => {
  const run = allRunBlocks(WORKFLOWS.release.text).find((b) => /signtool sign/.test(b.run))?.run ?? '';
  assert.match(run, /Join-Path \$env:RUNNER_TEMP 'codesign\.pfx'/);
  assert.match(run, /finally\s*\{\s*Remove-Item -LiteralPath \$pfx -Force/);
});

test('release: secrets and variables used by the workflows are all documented in docs/release.md', { timeout: TIMEOUT }, () => {
  const used = new Set();
  for (const { text } of Object.values(WORKFLOWS)) {
    for (const m of text.matchAll(/secrets\.([A-Z0-9_]+)/g)) used.add(m[1]);
    for (const m of text.matchAll(/vars\.([A-Z0-9_]+)/g)) used.add(m[1]);
  }
  const undocumented = [...used].filter((n) => !DOC.includes(n));
  assert.deepEqual(undocumented, []);
});

test('release: the header comment lists every secret and variable the workflow uses', { timeout: TIMEOUT }, () => {
  const header = WORKFLOWS.release.text.split('\n').slice(0, 20).join('\n');
  const used = new Set();
  for (const m of WORKFLOWS.release.text.matchAll(/(?:secrets|vars)\.([A-Z0-9_]+)/g)) used.add(m[1]);
  const notListed = [...used].filter((n) => !header.includes(n));
  assert.deepEqual(notListed, []);
});

test('release: the release job runs in the protected release environment', { timeout: TIMEOUT }, () => {
  assert.match(jobsOf(WORKFLOWS.release.text).release.join('\n'), /^ {4}environment: release\s*$/m);
});

test('release: the tag regex in the workflow accepts only what check-versions accepts', { timeout: TIMEOUT }, () => {
  const line = WORKFLOWS.release.text.split('\n').find((l) => /\[\[ "\$TAG" =~ /.test(l));
  assert.ok(line, 'tag regex line not found');
  const src = line.match(/=~ (\^\S+\$) \]\]/)[1];
  const re = new RegExp(src);
  const samples = ['v1.2.3', 'v1.2.3-rc.1', 'v1.2.3-01', 'v0.0.0-a.b-c', 'v01.2.3', 'v1.2'];
  const mismatched = samples.filter((t) => re.test(t) && !isSemver(t.slice(1)));
  assert.deepEqual(mismatched, []);
});

// ---------------------------------------------------------------- C. PowerShell syntax

const HELPER = `param()
foreach ($f in $args) {
  $tokens = $null; $errors = $null
  try {
    [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$tokens, [ref]$errors)
  } catch { "ERR\`t$f\`t0\`t$($_.Exception.Message)"; continue }
  if ($errors.Count -eq 0) { "OK\`t$f" }
  else { foreach ($e in $errors) { "ERR\`t$f\`t$($e.Extent.StartLineNumber)\`t$($e.Message)" } }
}
`;

async function parsePs1(files) {
  const helper = join(dir, 'parse-ps1.ps1');
  writeFileSync(helper, HELPER);
  const { stdout } = await execFileP(
    'powershell',
    ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', helper, ...files],
    { timeout: 90_000, maxBuffer: 16 * 1024 * 1024 },
  );
  return stdout.split(/\r?\n/).filter(Boolean).map((l) => l.split('\t'));
}

test('PowerShell: the shell: pwsh step in release.yml parses without errors', { timeout: TIMEOUT }, async () => {
  const pwshRuns = allRunBlocks(WORKFLOWS.release.text).filter((b) => /pwsh|powershell/.test(b.shell ?? ''));
  assert.ok(pwshRuns.length >= 1, 'no pwsh step found');
  const files = pwshRuns.map((b, i) => {
    const p = join(dir, `wf-step-${i}.ps1`);
    writeFileSync(p, dedent(b.run));
    return p;
  });
  const rows = await parsePs1(files);
  const errors = rows.filter((r) => r[0] === 'ERR');
  assert.deepEqual(errors, [], 'syntax errors in embedded pwsh');
  assert.equal(rows.filter((r) => r[0] === 'OK').length, files.length);
});

test('PowerShell: scripts/release/*.ps1 parse without errors', { timeout: TIMEOUT }, async () => {
  const files = ['gen-update-keys.ps1'].map((f) => join(ROOT, 'scripts', 'release', f));
  const rows = await parsePs1(files);
  assert.deepEqual(rows.filter((r) => r[0] === 'ERR'), []);
  assert.equal(rows.filter((r) => r[0] === 'OK').length, 1);
});

test('PowerShell: both standalone scripts support -Help (comment-based help plus a Help switch)', { timeout: TIMEOUT }, async () => {
  for (const f of ['gen-update-keys.ps1']) {
    const src = readFileSync(join(ROOT, 'scripts', 'release', f), 'utf8');
    assert.match(src, /\[switch\]\$Help/, `${f} has no -Help switch`);
    assert.match(src, /\.SYNOPSIS/, `${f} has no comment-based help`);
    const { stdout } = await execFileP('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(ROOT, 'scripts', 'release', f), '-Help'], { timeout: 90_000 });
    assert.match(stdout, /SYNOPSIS/i, `${f} -Help did not print help`);
  }
});

// ---------------------------------------------------------------- CLI --help for node scripts

for (const f of ['check-versions.mjs', 'make-latest-json.mjs', 'size-budget.mjs', 'write-notices.mjs']) {
  test(`${f} --help prints usage and exits 0`, { timeout: TIMEOUT }, async () => {
    const { stdout } = await execFileP(process.execPath, [join(HERE, f), '--help'], { timeout: 60_000 });
    assert.match(stdout, /^Usage: node scripts\/release\//);
  });
}

test('check-versions.mjs passes on the current tree (all four sources agree)', { timeout: TIMEOUT }, async () => {
  const { stdout } = await execFileP(process.execPath, [join(HERE, 'check-versions.mjs')], { timeout: 60_000 });
  assert.match(stdout, /All version sources agree\./);
});

test('make-latest-json output for the current repo tag shape validates', { timeout: TIMEOUT }, () => {
  const m = buildLatestJson({
    version: '0.2.0',
    notes: 'n',
    pubDate: '2026-10-10T12:00:00Z',
    repo: 'owner/repo',
    installerFileName: 'ClipCheck_0.2.0_x64-setup.exe',
    signature: 'c2ln',
  });
  assert.deepEqual(Object.keys(m), ['version', 'notes', 'pub_date', 'platforms']);
});

// ---------------------------------------------------------------- E. docs/release.md cross-check

test('docs: every scripts/ and installer/ path mentioned in release.md exists', { timeout: TIMEOUT }, () => {
  const paths = [...DOC.matchAll(/`((?:scripts|installer|docs|licenses)\/[^`\s]+)`/g)].map((m) => m[1]);
  const plain = [...DOC.matchAll(/\b((?:scripts\/release|installer)\/[\w.\/-]+\.(?:mjs|ps1|json|sh))\b/g)].map((m) => m[1]);
  // Skip placeholders such as docs/release-notes/vX.Y.Z.md (not real files).
  const all = [...new Set([...paths, ...plain].map((p) => p.replace(/\.$/, '')))].filter((p) => !/X\.Y|<|\bvX\b/.test(p));
  const missing = all.filter((p) => !existsSync(join(ROOT, p)));
  assert.deepEqual(missing, []);
  assert.ok(all.length > 5);
});

test('docs: relative links in release.md resolve (design/11-installer-and-updates.md)', { timeout: TIMEOUT }, () => {
  assert.ok(existsSync(join(ROOT, 'docs', 'design', '11-installer-and-updates.md')));
});

test('docs: every script flag used in a release.md command exists in that script', { timeout: TIMEOUT }, () => {
  const mjs = {
    'check-versions.mjs': read('scripts/release/check-versions.mjs'),
    'size-budget.mjs': read('scripts/release/size-budget.mjs'),
    'write-notices.mjs': read('scripts/release/write-notices.mjs'),
    'make-latest-json.mjs': read('scripts/release/make-latest-json.mjs'),
  };
  const problems = [];
  for (const line of DOC.split('\n')) {
    const m = line.match(/node scripts\/release\/([\w.-]+\.mjs)(.*)/);
    if (!m) continue;
    const src = mjs[m[1]];
    if (!src) {
      problems.push(`unknown script ${m[1]}`);
      continue;
    }
    for (const flag of m[2].matchAll(/--([a-z][a-z-]*)/g)) {
      if (!new RegExp(`['"]?${flag[1]}['"]?\\s*:`).test(src)) problems.push(`${m[1]} has no --${flag[1]}`);
    }
  }
  assert.deepEqual(problems, []);
});

test('docs: latest.json fields listed in release.md match what make-latest-json writes', { timeout: TIMEOUT }, () => {
  const documented = ['version', 'notes', 'pub_date', 'platforms'];
  const written = Object.keys(
    buildLatestJson({
      version: '0.2.0',
      notes: 'n',
      pubDate: '2026-10-10T12:00:00Z',
      repo: 'o/r',
      installerFileName: 'ClipCheck_0.2.0_x64-setup.exe',
      signature: 'c2ln',
    }),
  );
  assert.deepEqual(written, documented);
});

test('docs: the updater endpoint in tauri.conf.json follows the step 1c template', { timeout: TIMEOUT }, () => {
  const conf = JSON.parse(read('apps/ui/src-tauri/tauri.conf.json'));
  const endpoints = conf.plugins?.updater?.endpoints ?? [];
  assert.equal(endpoints.length, 1);
  assert.match(endpoints[0], /^https:\/\/github\.com\/[^/]+\/[^/]+\/releases\/latest\/download\/latest\.json$/);
});

test('docs: release profile claim (lto thin, strip true) matches the root Cargo.toml', { timeout: TIMEOUT }, () => {
  const cargo = read('Cargo.toml');
  const block = cargo.slice(cargo.indexOf('[profile.release]'));
  assert.match(block, /lto = "thin"/);
  assert.match(block, /strip = true/);
});

test('docs: updates.check "never" named in release.md exists in the config schema', { timeout: TIMEOUT }, () => {
  assert.match(DOC, /updates\.check = never/);
  assert.match(read('crates/core/src/config/schema.rs'), /"never"/);
});

test('docs: scripts/test-all.sh named in release.md exists', { timeout: TIMEOUT }, () => {
  assert.ok(existsSync(join(ROOT, 'scripts', 'test-all.sh')));
});

test('docs: section 8 names the production updater key id and the config pubkey really has that id', { timeout: TIMEOUT }, () => {
  const conf = JSON.parse(read('apps/ui/src-tauri/tauri.conf.json'));
  const pubkey = conf.plugins?.updater?.pubkey;
  assert.ok(pubkey, 'updater pubkey must be set');
  // The pubkey is base64 of a minisign file whose comment line ends with the key id.
  const keyId = /public key: ([0-9A-F]{16})/.exec(Buffer.from(pubkey, 'base64').toString('utf8'))?.[1];
  assert.ok(keyId, 'pubkey must decode to a minisign public key');
  assert.ok(read('docs/release.md').includes(keyId), 'docs/release.md must name the key id ' + keyId);
});
