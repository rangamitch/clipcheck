/**
 * In-app update state and wrappers over the updater commands (D28).
 *
 * The Rust side (`src-tauri/src/updater.rs`) does the network work. This module keeps a
 * Svelte store with what the title-bar pill, the update dialog and the Settings tab show.
 * Every Tauri call is guarded, so the bundle also runs in browser dev mode and under vitest.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { get, writable } from 'svelte/store';
import { isTauriRuntime } from './window';

/** Event: a background check found a version (payload: `UpdateInfo`, `skipped` set when skipped). */
export const UPDATE_AVAILABLE_EVENT = 'update-available';
/** Event: download, verify and install progress. */
export const UPDATE_PROGRESS_EVENT = 'update-progress';

/**
 * An available update, as the backend sends it (`UpdateInfoDto` in
 * `apps/ui/src-tauri/src/updater.rs`); the frontend only sees these fields.
 */
export interface UpdateInfo {
  version: string;
  current_version: string;
  notes: string;
  pub_date: string | null;
  /** True when the user chose to skip this version. */
  skipped: boolean;
}

/** `update-state.json` as returned by `get_update_state`. */
export interface UpdateState {
  last_checked_unix_ms: number | null;
  skipped_version: string | null;
  last_installed_from: string | null;
}

/** Raw `update-progress` payload. */
export interface UpdateProgressPayload {
  phase: 'downloading' | 'verifying' | 'installing';
  done: number;
  total: number | null;
}

/** Download progress shown in the modal. `total` is null while the size is unknown. */
export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

export type UpdateErrorKind = 'recording' | 'signature' | 'network' | 'other';

export interface UpdateError {
  kind: UpdateErrorKind;
  message: string;
}

export interface UpdateStoreState {
  available: UpdateInfo | null;
  progress: UpdateProgress | null;
  installing: boolean;
  error: UpdateError | null;
  state: UpdateState | null;
  modalOpen: boolean;
}

export const INITIAL_UPDATE_STATE: UpdateStoreState = {
  available: null,
  progress: null,
  installing: false,
  error: null,
  state: null,
  modalOpen: false,
};

/** Shared update store. Components read it with `$updateStore`. */
export const updateStore = writable<UpdateStoreState>({ ...INITIAL_UPDATE_STATE });

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/**
 * Maps a backend error message to a dialog state. Matching is by substring, lowercase:
 * "recording" -> recording; "signature" or "verify" -> signature;
 * "network", "timed out" or "connect" -> network; anything else -> other.
 */
export function classifyUpdateError(message: string): UpdateErrorKind {
  const m = message.toLowerCase();
  if (m.includes('recording')) return 'recording';
  if (m.includes('signature') || m.includes('verify')) return 'signature';
  if (m.includes('network') || m.includes('timed out') || m.includes('connect')) return 'network';
  return 'other';
}

const GENERIC_ERROR = 'Something went wrong while updating.';
const MAX_RAW_ERROR_CHARS = 200;

/** A stack frame: indented "at ..." or a "file.ext:line:col" location. */
function isStackFrameLine(line: string): boolean {
  return /^\s+at\s/.test(line) || /\S+\.\w+:\d+:\d+/.test(line);
}

/**
 * Reduces a raw backend message to one short line: the first non-empty line that is not a
 * stack frame, truncated with an ellipsis. Falls back to a generic message when nothing is left.
 */
function sanitizeRawError(raw: string): string {
  const line = raw
    .split(/\r?\n/)
    .find((l) => l.trim() !== '' && !isStackFrameLine(l));
  const text = (line ?? '').trim();
  if (text === '') return GENERIC_ERROR;
  if (text.length <= MAX_RAW_ERROR_CHARS) return text;
  return text.slice(0, MAX_RAW_ERROR_CHARS - 1) + '…';
}

/** The text shown for an error kind. `raw` is the backend message, used for `other`. */
export function updateErrorMessage(kind: UpdateErrorKind, raw: string): string {
  switch (kind) {
    case 'recording':
      return 'A recording is in progress. Finish it, then update.';
    case 'signature':
      return 'The downloaded update could not be verified. Your current version is unchanged.';
    case 'network':
      return 'Could not reach the update server. Check your connection and try again.';
    case 'other':
      return sanitizeRawError(raw);
  }
}

/** The title-bar pill shows only for an update that is not skipped. */
export function isPillVisible(info: UpdateInfo | null): boolean {
  return info !== null && !info.skipped;
}

/** Message text from a rejected invoke. Tauri rejects with the Rust `String` error. */
function messageOf(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

// ---------------------------------------------------------------------------
// Typed command wrappers (no guard: callers decide what happens outside Tauri)
// ---------------------------------------------------------------------------

export function fetchUpdateState(): Promise<UpdateState> {
  return invoke<UpdateState>('get_update_state');
}

export function checkForUpdate(manual: boolean): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>('check_for_update', { manual });
}

export function installUpdate(): Promise<void> {
  return invoke<void>('install_update');
}

export function skipUpdateVersion(version: string): Promise<void> {
  return invoke<void>('skip_update_version', { version });
}

// ---------------------------------------------------------------------------
// Store actions
// ---------------------------------------------------------------------------

/**
 * Loads the saved update state and subscribes to the update events. Returns a function
 * that unsubscribes. Outside Tauri it does nothing and returns a no-op.
 */
export async function initUpdates(): Promise<() => void> {
  if (!isTauriRuntime()) return () => {};

  try {
    const state = await fetchUpdateState();
    updateStore.update((s) => ({ ...s, state }));
  } catch {
    // The pill and tab work without the saved state; they just lack the last-checked time.
  }

  const unlisteners: UnlistenFn[] = [];
  try {
    unlisteners.push(
      await listen<UpdateInfo>(UPDATE_AVAILABLE_EVENT, (ev) => {
        updateStore.update((s) => ({ ...s, available: ev.payload }));
      }),
    );
    unlisteners.push(
      await listen<UpdateProgressPayload>(UPDATE_PROGRESS_EVENT, (ev) => {
        const p = ev.payload;
        updateStore.update((s) => ({
          ...s,
          progress: { downloaded: p.done, total: p.total ?? null },
        }));
      }),
    );
  } catch {
    // Without the listeners the pill appears only after a manual check.
  }

  return () => {
    for (const fn of unlisteners) fn();
  };
}

/**
 * Runs a user-started check (works even when `updates.check = never`). Updates the store
 * and returns the result: an update, or null when up to date. Throws on failure.
 * Outside Tauri it returns null.
 */
export async function checkNow(): Promise<UpdateInfo | null> {
  if (!isTauriRuntime()) return null;
  const info = await checkForUpdate(true);
  updateStore.update((s) => ({ ...s, available: info }));
  try {
    const state = await fetchUpdateState();
    updateStore.update((s) => ({ ...s, state }));
  } catch {
    // The check result is already in the store; the last-checked time may be stale.
  }
  return info;
}

/**
 * Downloads, verifies and installs the update. On success the app exits, so this normally
 * never resolves into a visible state. Errors are classified into the store's `error`.
 */
export async function installNow(): Promise<void> {
  if (!isTauriRuntime()) return;
  updateStore.update((s) => ({ ...s, installing: true, error: null, progress: null }));
  try {
    await installUpdate();
    updateStore.update((s) => ({ ...s, installing: false }));
  } catch (e) {
    const raw = messageOf(e);
    const kind = classifyUpdateError(raw);
    updateStore.update((s) => ({
      ...s,
      installing: false,
      progress: null,
      error: { kind, message: updateErrorMessage(kind, raw) },
    }));
  }
}

/**
 * Remembers the version the user skipped. The pill hides and the modal closes.
 * A failed save is reported in `error` (kind `other`).
 */
export async function skipVersion(): Promise<void> {
  if (!isTauriRuntime()) return;
  const info = get(updateStore).available;
  if (!info) return;
  try {
    await skipUpdateVersion(info.version);
    updateStore.update((s) => ({
      ...s,
      available: { ...info, skipped: true },
      modalOpen: false,
      error: null,
    }));
  } catch (e) {
    const raw = messageOf(e);
    updateStore.update((s) => ({
      ...s,
      error: { kind: 'other', message: updateErrorMessage('other', raw) },
    }));
  }
}

/** Opens the update dialog (from the pill). */
export function openUpdateModal(): void {
  updateStore.update((s) => ({ ...s, modalOpen: true }));
}

/** Closes the update dialog. Does not cancel an install in progress. */
export function closeUpdateModal(): void {
  updateStore.update((s) => ({ ...s, modalOpen: false }));
}
