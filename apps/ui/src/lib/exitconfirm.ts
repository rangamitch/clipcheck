import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { writable, get, type Readable } from 'svelte/store';
import { isTauriRuntime } from './window';

/**
 * State of the exit confirmation dialog shown when the window is closed during a
 * recording session (`general.confirm_exit_while_recording`).
 *
 * `open` shows the dialog. `busy` is set while the exit request is in flight; the
 * dialog cannot be cancelled then.
 */
export interface ExitConfirmState {
  open: boolean;
  busy: boolean;
}

/** Tauri event emitted by the Rust close handler when a confirm is needed. */
export const CONFIRM_EXIT_EVENT = 'confirm-exit';

/** Dialog state before any close request. */
export const INITIAL_EXIT_CONFIRM: ExitConfirmState = { open: false, busy: false };

/** Events that move the dialog between states. */
export type ExitConfirmAction = 'open' | 'cancel' | 'confirm' | 'failed';

/**
 * Pure transition function.
 *
 * - `open` shows the dialog (idempotent).
 * - `cancel` closes it, unless an exit is already in flight.
 * - `confirm` starts the exit, only when the dialog is open and not busy.
 * - `failed` ends the in-flight exit and keeps the dialog open so the user can retry.
 */
export function reduceExitConfirm(
  state: ExitConfirmState,
  action: ExitConfirmAction,
): ExitConfirmState {
  switch (action) {
    case 'open':
      return state.open ? state : { open: true, busy: false };
    case 'cancel':
      return state.busy ? state : INITIAL_EXIT_CONFIRM;
    case 'confirm':
      return state.open && !state.busy ? { open: true, busy: true } : state;
    case 'failed':
      return state.busy ? { open: true, busy: false } : state;
  }
}

/** Signature of a Tauri command call (command name plus optional arguments). */
export type InvokeFn = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

/** Store exposing the dialog state and its actions. */
export interface ExitConfirmStore extends Readable<ExitConfirmState> {
  /** Shows the dialog. */
  open(): void;
  /** Hides the dialog. Ignored while an exit is in flight. */
  cancel(): void;
  /**
   * Calls `exit_app` with `stopDaemon: true` once. Ignored unless the dialog is open
   * and not already busy. Resolves when the request settles.
   */
  confirm(): Promise<void>;
}

/** Creates a store that calls `invokeFn` for the exit. Used directly by tests. */
export function createExitConfirmStore(invokeFn: InvokeFn): ExitConfirmStore {
  const store = writable<ExitConfirmState>(INITIAL_EXIT_CONFIRM);
  const apply = (action: ExitConfirmAction) =>
    store.update((state) => reduceExitConfirm(state, action));
  return {
    subscribe: store.subscribe,
    open: () => apply('open'),
    cancel: () => apply('cancel'),
    async confirm() {
      const state = get(store);
      if (!state.open || state.busy) return;
      apply('confirm');
      try {
        await invokeFn('exit_app', { stopDaemon: true });
      } catch {
        apply('failed');
      }
    },
  };
}

/** The app-wide dialog store. */
export const exitConfirm = createExitConfirmStore((command, args) => invoke(command, args));

/**
 * Listens for `confirm-exit` and opens the dialog. Resolves to an unlisten function.
 * Outside Tauri it does nothing and resolves to a no-op.
 */
export async function listenForExitConfirm(
  store: Pick<ExitConfirmStore, 'open'> = exitConfirm,
): Promise<() => void> {
  if (!isTauriRuntime()) return () => {};
  const unlisten: UnlistenFn = await listen(CONFIRM_EXIT_EVENT, () => store.open());
  return unlisten;
}
