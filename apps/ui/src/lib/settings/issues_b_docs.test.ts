/**
 * Regression tests for issues #174 (default quality preset) and #176 (list editor keys).
 * Reads repo files as text; no component rendering.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { VIDEO_DEFAULTS } from './video';

function repoFile(rel: string): string {
  return readFileSync(fileURLToPath(new URL(`../../../../../${rel}`, import.meta.url)), 'utf8');
}

describe('issues_b docs and defaults (#174 #176)', () => {
  it('issue_174 the preset the UI treats as the default equals default_toml', () => {
    const schema = repoFile('crates/core/src/config/schema.rs');
    // default_toml() returns DEFAULT_TOML, a r##"..."## raw string.
    const start = schema.indexOf('const DEFAULT_TOML: &str = r##"');
    expect(start).toBeGreaterThanOrEqual(0);
    const bodyStart = schema.indexOf('"##;', start);
    expect(bodyStart).toBeGreaterThan(start);
    const body = schema.slice(start, bodyStart);
    // Uncommented top-level `preset = "..."` line only (the commented example is ignored).
    const match = /^preset = "(\w+)"/m.exec(body);
    expect(match).not.toBeNull();
    const rustDefault = match![1];
    expect(rustDefault).toBe('high');
    expect(VIDEO_DEFAULTS.preset).toBe(rustDefault);
  });

  it('issue_174 docs/design/03-encoding.md says High is the default preset', () => {
    const doc = repoFile('docs/design/03-encoding.md');
    expect(doc).toContain('High (default)');
    expect(doc).not.toContain('Medium (default)');
  });

  it('issue_174 schema.rs #[default] on the Preset enum is High', () => {
    const schema = repoFile('crates/core/src/config/schema.rs');
    const enumMatch = /pub enum Preset \{[\s\S]*?\n\}/.exec(schema);
    expect(enumMatch).not.toBeNull();
    expect(/#\[default\]\s*\n\s*High,/.test(enumMatch![0])).toBe(true);
  });

  it('issue_176 ListEditor.svelte keys its {#each} by index, not by the item string', () => {
    // A duplicated entry in a hand-edited config.toml would otherwise throw each_key_duplicate.
    const editor = repoFile('apps/ui/src/lib/settings/fields/ListEditor.svelte');
    expect(editor).toMatch(/\{#each items as item, i \(i\)\}/);
    expect(editor).not.toMatch(/\(item\)\}/);
  });
});

describe('issue_187 shared tab styles', () => {
  const read = (p: string): string =>
    readFileSync(fileURLToPath(new URL(p, import.meta.url)), 'utf8');

  it('issue_187 Settings.svelte holds the shared button states and Audio/Replay tabs do not repeat them', () => {
    const shell = read('../Settings.svelte');
    expect(shell).toContain(':global(button:disabled)');
    expect(shell).toContain(':global(button:focus-visible)');
    for (const tab of ['AudioTab', 'ReplayTab']) {
      const text = read(`./tabs/${tab}.svelte`);
      expect(text, `${tab} repeats the shared focus rule`).not.toMatch(/^\s*button:focus-visible/m);
      expect(text, `${tab} repeats the shared disabled rule`).not.toMatch(/^\s*button:disabled/m);
    }
  });
});
