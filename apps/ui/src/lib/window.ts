import { getCurrentWindow } from '@tauri-apps/api/window';

/**
 * Window-control wrappers used by the custom title bar.
 *
 * Every call is guarded by the Tauri runtime check so the same bundle runs in
 * plain browser dev mode and under vitest without throwing. Each wrapper maps
 * to one permission in `src-tauri/capabilities/default.json`; the static test
 * asserts that mapping stays complete.
 */

/** Window permissions required by the wrappers below (and the drag region). */
export const WINDOW_PERMISSIONS = {
  minimize: 'core:window:allow-minimize',
  toggleMaximize: 'core:window:allow-toggle-maximize',
  close: 'core:window:allow-close',
  isMaximized: 'core:window:allow-is-maximized',
} as const;

/**
 * True when running inside the Tauri webview (the IPC bridge is present). The single
 * Tauri-detection helper for the UI: `daemon.ts` and every window and updater caller use it.
 * In the browser `window` and `globalThis` are the same object; the `window` check exists so
 * tests that stub only `window` see the same answer.
 */
export function isTauriRuntime(): boolean {
  if (typeof globalThis !== 'undefined' && '__TAURI_INTERNALS__' in globalThis) return true;
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** Minimizes the main window. No-op outside Tauri. */
export async function minimize(): Promise<void> {
  if (!isTauriRuntime()) return;
  await getCurrentWindow().minimize();
}

/** Toggles between maximized and restored. No-op outside Tauri. */
export async function toggleMaximize(): Promise<void> {
  if (!isTauriRuntime()) return;
  await getCurrentWindow().toggleMaximize();
}

/** Closes the main window. No-op outside Tauri. */
export async function close(): Promise<void> {
  if (!isTauriRuntime()) return;
  await getCurrentWindow().close();
}

/** Reports whether the window is maximized. Always false outside Tauri. */
export async function isMaximized(): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  return getCurrentWindow().isMaximized();
}

/** Minimum gap between maximized-state reads while a resize is in progress. */
export const RESIZE_THROTTLE_MS = 100;

/**
 * Subscribes to resize events and reports the maximized state. A drag sends many resize
 * events, so reads are throttled: the first event reads at once, events inside the window
 * only mark a pending read, and one trailing read runs when the window ends, so the final
 * state is always reported. Resolves to an unlisten function. Outside Tauri it resolves to a
 * no-op.
 */
export async function onMaximizeChange(
  handler: (maximized: boolean) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) return () => {};
  const win = getCurrentWindow();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let pending = false;

  const report = async (): Promise<void> => {
    handler(await win.isMaximized());
  };
  const startWindow = (): void => {
    timer = setTimeout(() => {
      timer = undefined;
      if (!pending) return;
      pending = false;
      startWindow();
      report().catch(() => {});
    }, RESIZE_THROTTLE_MS);
  };

  let closed = false;
  const unlisten = await win.onResized(async () => {
    if (closed) return;
    if (timer !== undefined) {
      pending = true;
      return;
    }
    startWindow();
    await report();
  });
  return () => {
    closed = true;
    pending = false;
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    unlisten();
  };
}
