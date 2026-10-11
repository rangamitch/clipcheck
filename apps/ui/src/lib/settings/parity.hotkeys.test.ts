/**
 * Rust <-> TypeScript parity for hotkey constants (GitHub issue #188).
 *
 * The UI hand-copies a few Rust values. These tests read the Rust sources as text (node fs, because a
 * `?raw` import cannot leave the Vite root apps/ui) and fail when the two sides drift apart.
 *
 * Not covered: `DEFAULT_TRACKS` / `default_tracks()`. Neither symbol exists at HEAD (see
 * docs/triage/batch-19.md), so there is nothing to compare.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { RESERVED_CHORDS, SUPPORTED_KEYS } from './hotkeys';

/** From apps/ui/src/lib/settings up to the repo root. */
const REPO = '../../../../../';

function readRepo(rel: string): string {
  const text = readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');
  expect(text.length, `${rel} was not found`).toBeGreaterThan(0);
  return text;
}

interface Mods {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  win: boolean;
}

/** Canonical chord text in the order Ctrl, Alt, Shift, Win, key. */
function canonical(mods: Mods, key: string): string {
  const parts = [
    mods.ctrl ? 'Ctrl' : '',
    mods.alt ? 'Alt' : '',
    mods.shift ? 'Shift' : '',
    mods.win ? 'Win' : '',
  ].filter((p) => p !== '');
  return [...parts, key.toUpperCase()].join('+');
}

const ROW_RE =
  /\(\s*(true|false)\s*,\s*(true|false)\s*,\s*(true|false)\s*,\s*(true|false)\s*,\s*Key::(\w+)\s*,\s*"([^"]*)"\s*,?\s*\)/g;

describe('issue_188 hotkey parity', () => {
  it('issue_188 RESERVED_CHORDS matches the reserved() TABLE in crates/hotkeys/src/registry.rs', () => {
    const rust = readRepo(`${REPO}crates/hotkeys/src/registry.rs`);
    const start = rust.indexOf('const TABLE');
    expect(start, 'reserved() TABLE not found in registry.rs').toBeGreaterThan(-1);
    const end = rust.indexOf('];', start);
    const table = rust.slice(start, end);

    // Rust row order is (ctrl, shift, alt, win, key, label); the TS row uses the same order.
    const rustRows = [...table.matchAll(ROW_RE)].map((m) => {
      const [ctrl, shift, alt, win] = [m[1], m[2], m[3], m[4]].map((v) => v === 'true');
      const key = m[5];
      const label = m[6];
      return `${canonical({ ctrl, alt, shift, win }, key)}\t${label}`;
    });
    const tsRows = RESERVED_CHORDS.map(([ctrl, shift, alt, win, key, label]) => {
      return `${canonical({ ctrl, alt, shift, win }, key)}\t${label}`;
    });

    expect(rustRows.length, 'reserved() row count in registry.rs (parse failed?)').toBeGreaterThan(0);
    const rustSorted = [...rustRows].sort();
    const tsSorted = [...tsRows].sort();
    const onlyRust = rustSorted.filter((r) => !tsSorted.includes(r));
    const onlyTs = tsSorted.filter((r) => !rustSorted.includes(r));
    expect(onlyRust, 'reserved in Rust but missing or different in RESERVED_CHORDS (hotkeys.ts)').toEqual([]);
    expect(onlyTs, 'in RESERVED_CHORDS (hotkeys.ts) but not in Rust reserved()').toEqual([]);
    expect(tsRows.length, 'RESERVED_CHORDS count differs from the Rust TABLE').toBe(rustRows.length);
  });

  it('issue_188 session hotkey defaults match between schema.rs and the HotkeysTab help text', () => {
    const schema = readRepo(`${REPO}crates/core/src/config/schema.rs`);
    const actions = ['split', 'stop_start', 'toggle_buffer', 'mute_mic'];

    // Rust source of truth: the default_toml() literal, one `key = "chord"` line per action.
    const tomlDefaults: Record<string, string> = {};
    for (const action of actions) {
      const hits = [...schema.matchAll(new RegExp(`^\\s*${action}\\s*=\\s*"([^"]*)"`, 'gm'))];
      expect(hits.length, `${action} default line in schema.rs default_toml`).toBe(1);
      tomlDefaults[action] = hits[0][1];
    }

    // The Rust Default impl must agree with default_toml.
    for (const action of actions) {
      const m = new RegExp(`${action}:\\s*Some\\(KeyChord::alt\\(VK_(F\\d+)\\)\\)`).exec(schema);
      expect(m, `${action} Default impl in schema.rs (parse failed?)`).not.toBeNull();
      expect(tomlDefaults[action], `schema.rs Default impl vs default_toml for ${action}`).toBe(`Alt+${m![1]}`);
    }

    // The TS help text: `{ key: 'split', label: '...', help: '... Default Alt+F11.' }`.
    const tab = readFileSync(fileURLToPath(new URL('./tabs/HotkeysTab.svelte', import.meta.url)), 'utf8');
    const uiDefaults: Record<string, string> = {};
    for (const m of tab.matchAll(/\{\s*key:\s*'(\w+)',[^}]*?help:\s*'([^']*)'/g)) {
      const d = /Default ([A-Za-z0-9+]+)\./.exec(m[2]);
      if (d) uiDefaults[m[1]] = d[1];
    }

    for (const action of actions) {
      expect(uiDefaults[action], `HotkeysTab.svelte help text has no 'Default ...' for ${action}`).toBeDefined();
      expect(uiDefaults[action], `default chord for ${action}: HotkeysTab.svelte vs schema.rs`).toBe(
        tomlDefaults[action],
      );
    }
  });

  it('issue_188 SUPPORTED_KEYS equals the canonical key names in docs/design/hotkey-key-names.md', () => {
    const doc = readRepo(`${REPO}docs/design/hotkey-key-names.md`);
    // Table rows look like `| Pause | 0x13 |`. Aliases such as `Menu` appear only in prose and are not
    // listed here, so this compares canonical names only (the doc's alias is not in SUPPORTED_KEYS).
    const docKeys = [...doc.matchAll(/^\| ([A-Za-z0-9]+) \| 0x[0-9A-Fa-f]{2} \|$/gm)].map((m) => m[1]);
    expect(docKeys.length, 'key-name rows in hotkey-key-names.md (parse failed?)').toBeGreaterThan(100);

    const ui = new Set(SUPPORTED_KEYS);
    const docSet = new Set(docKeys);
    const missingInUi = docKeys.filter((k) => !ui.has(k));
    const missingInDoc = [...SUPPORTED_KEYS].filter((k) => !docSet.has(k));
    expect(missingInUi, 'named in hotkey-key-names.md but not in SUPPORTED_KEYS (hotkeys.ts)').toEqual([]);
    expect(missingInDoc, 'in SUPPORTED_KEYS (hotkeys.ts) but not in hotkey-key-names.md').toEqual([]);
  });
});
