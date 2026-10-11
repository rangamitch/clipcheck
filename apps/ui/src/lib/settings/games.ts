/**
 * Pure helpers for the Games settings tab: the allow list (games), the block list, the
 * per-game display names (`games` map), and the picker list built from running apps.
 * No Svelte and no I/O. Runtime imports are limited to `pure.ts`, so tests stay light.
 *
 * Daemon rules this follows (crates/core/src/config/schema.rs):
 * - `detection.allow` and `detection.block` are arrays of lowercase `*.exe` names. Arrays are
 *   replaced whole by a patch.
 * - `games` is keyed by GameId (`cs2.exe` or `cs2.exe@steam:730`). Each entry is a
 *   `GameOverride` with `deny_unknown_fields`. `display_name` must be 1..=64 characters.
 * - A removed `games` key is sent as `null` by `diffPatch`.
 */
import type { PickerApp } from './fields/AppPicker.svelte';
import { normalizeExeName } from './pure';

/** One `games` entry as the daemon sends it. Only `display_name` is written by this tab. */
export type GameEntry = Record<string, unknown>;
export type GamesMap = Record<string, GameEntry>;

/** The slice of the config this tab edits. */
export interface GamesSlice {
  allow: string[];
  block: string[];
  games: GamesMap;
}

/** A running app as the picker and the rows need it. Structurally a `RunningApp`. */
export interface GameCandidate {
  exe: string;
  name: string;
  iconB64: string | null;
  isGame: boolean;
  suggested: boolean;
}

export const DISPLAY_NAME_MAX = 64;
export const ALREADY_DETECTED = 'Already detected automatically';

/** File name without folder or `.exe`, such as `cs2` for `cs2.exe`. Falls back to the input. */
export function stem(exe: string): string {
  const base = exe.replace(/^.*[\\/]/, '');
  const s = base.replace(/\.exe$/i, '');
  return s === '' ? exe : s;
}

/**
 * A display name the daemon accepts: trimmed, at most 64 characters, and never blank. A blank
 * name falls back to the exe stem.
 */
export function safeDisplayName(name: string, exe: string): string {
  const trimmed = Array.from((name ?? '').trim()).slice(0, DISPLAY_NAME_MAX).join('').trim();
  return trimmed === '' ? stem(exe) : trimmed;
}

/** The `games` key for an exe, matched case-insensitively. Undefined when there is none. */
function gameKeyFor(exe: string, games: GamesMap): string | undefined {
  if (Object.prototype.hasOwnProperty.call(games, exe)) return exe;
  const wanted = exe.toLowerCase();
  return Object.keys(games).find((k) => k.toLowerCase() === wanted);
}

/** The override entry for an exe, or undefined. */
export function overrideFor(exe: string, games: GamesMap): GameEntry | undefined {
  const key = gameKeyFor(exe, games);
  return key === undefined ? undefined : games[key];
}

/** The display name to show: the override's `display_name` when set, else the exe stem. */
export function displayNameFor(exe: string, games: GamesMap): string {
  const dn = overrideFor(exe, games)?.display_name;
  return typeof dn === 'string' && dn.trim() !== '' ? dn.trim() : stem(exe);
}

/** True when an override holds nothing but `display_name` (null and missing count as absent). */
export function isDisplayNameOnly(entry: GameEntry): boolean {
  return Object.keys(entry).every((k) => k === 'display_name' || entry[k] === null || entry[k] === undefined);
}

function sameExe(a: string, b: string): boolean {
  return a.toLowerCase() === b.toLowerCase();
}

export type GamesResult = { ok: true; value: GamesSlice } | { ok: false; error: string };

/**
 * Adds a game to the allow list and records its display name. An existing `display_name` is
 * kept. Other override keys (folder, video, ...) are kept as they are.
 */
export function addGame(slice: GamesSlice, rawExe: string, appName: string): GamesResult {
  const n = normalizeExeName(rawExe);
  if (!n.ok) return { ok: false, error: n.error };
  const exe = n.value;
  if (slice.allow.some((e) => sameExe(e, exe))) {
    return { ok: false, error: `${exe} is already in your games.` };
  }
  if (slice.block.some((e) => sameExe(e, exe))) {
    return { ok: false, error: `${exe} is in Never record. Remove it there first.` };
  }
  const key = gameKeyFor(exe, slice.games) ?? exe;
  const existing = slice.games[key] ?? {};
  const kept = typeof existing.display_name === 'string' && existing.display_name.trim() !== '';
  const display_name = kept ? existing.display_name : safeDisplayName(appName, exe);
  return {
    ok: true,
    value: {
      allow: [...slice.allow, exe],
      block: slice.block,
      games: { ...slice.games, [key]: { ...existing, display_name } },
    },
  };
}

/**
 * Removes a game from the allow list. A `games` entry that holds only `display_name` is
 * removed too. A richer override stays, so folder and video settings are not lost.
 */
export function removeGame(slice: GamesSlice, exe: string): GamesSlice {
  const allow = slice.allow.filter((e) => !sameExe(e, exe));
  const key = gameKeyFor(exe, slice.games);
  if (key === undefined) return { ...slice, allow };
  const entry = slice.games[key];
  if (!isDisplayNameOnly(entry)) return { ...slice, allow };
  const games = { ...slice.games };
  delete games[key];
  return { ...slice, allow, games };
}

/** Adds an exe to the block list. Duplicates (any case) and invalid names are refused. */
export function addBlocked(slice: GamesSlice, rawExe: string): GamesResult {
  const n = normalizeExeName(rawExe);
  if (!n.ok) return { ok: false, error: n.error };
  if (slice.block.some((e) => sameExe(e, n.value))) {
    return { ok: false, error: `${n.value} is already in Never record.` };
  }
  return { ok: true, value: { ...slice, block: [...slice.block, n.value] } };
}

/** Removes an exe from the block list (any case). */
export function removeBlocked(slice: GamesSlice, exe: string): GamesSlice {
  return { ...slice, block: slice.block.filter((e) => !sameExe(e, exe)) };
}

/**
 * Entries for the "Add a game" picker. Apps already in the allow list or the block list are
 * left out. An app the detector already accepts (`isGame`) stays listed, disabled and badged.
 * Suggested apps come first, then the rest. Within each group, enabled apps come before
 * disabled ones. Otherwise the input order is kept.
 */
export function pickerAppsFor(
  runningApps: readonly GameCandidate[],
  allow: readonly string[],
  block: readonly string[],
): PickerApp[] {
  const taken = new Set([...allow, ...block].map((e) => e.toLowerCase()));
  const rows = runningApps
    .filter((a) => !taken.has(a.exe.toLowerCase()))
    .map((a) => {
      const disabled = a.isGame;
      const app: PickerApp = {
        exe: a.exe,
        name: a.name,
        iconB64: a.iconB64,
        hint: a.exe,
        suggested: a.suggested,
        disabled,
        badge: disabled ? ALREADY_DETECTED : undefined,
      };
      const rank = (a.suggested ? 0 : 2) + (disabled ? 1 : 0);
      return { app, rank };
    });
  return rows.map((r, i) => ({ r, i })).sort((x, y) => x.r.rank - y.r.rank || x.i - y.i).map((x) => x.r.app);
}

/** Entries for the "Never record" picker: every running app not already blocked. */
export function blockPickerAppsFor(runningApps: readonly GameCandidate[], block: readonly string[]): PickerApp[] {
  const taken = new Set(block.map((e) => e.toLowerCase()));
  return runningApps
    .filter((a) => !taken.has(a.exe.toLowerCase()))
    .map((a) => ({ exe: a.exe, name: a.name, iconB64: a.iconB64, hint: a.exe, suggested: a.suggested }));
}

/** True when the exe is in the running list (case-insensitive). */
export function isRunning(exe: string, runningApps: readonly GameCandidate[]): boolean {
  return runningApps.some((a) => sameExe(a.exe, exe));
}

/** Rows for the "Your games" card, in allow-list order. */
export interface GameRow {
  exe: string;
  name: string;
  iconB64: string | null;
  running: boolean;
}

export function gameRowsFor(
  allow: readonly string[],
  games: GamesMap,
  runningApps: readonly GameCandidate[],
): GameRow[] {
  return allow.map((exe) => {
    const live = runningApps.find((a) => sameExe(a.exe, exe));
    return {
      exe,
      name: displayNameFor(exe, games),
      iconB64: live?.iconB64 ?? null,
      running: live !== undefined,
    };
  });
}
