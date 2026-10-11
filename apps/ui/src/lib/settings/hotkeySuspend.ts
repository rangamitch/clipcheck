/**
 * Pauses the daemon's global hotkeys while a hotkey capture box is listening.
 *
 * The daemon resumes hotkeys 60 s after the last `suspended: true` and when the client
 * disconnects, so while any capture is open this module re-sends `true` every 20 s. The first
 * capture sends `true`; the last release sends `false`. Releases are idempotent.
 */
import { setHotkeysSuspended } from '../daemon';
import { isTauriRuntime } from '../window';

/** Keepalive period. Must stay well under the daemon's 60 s auto-resume. */
export const HOTKEY_SUSPEND_KEEPALIVE_MS = 20_000;

/** Sends one `set_hotkeys_suspended` request. May return a promise; rejections are swallowed. */
export type HotkeySuspendSender = (suspended: boolean) => unknown;

export interface HotkeySuspendTimers {
  setInterval: (fn: () => void, ms: number) => unknown;
  clearInterval: (handle: unknown) => void;
}

export interface HotkeySuspender {
  /** Opens a capture. Returns an idempotent release function. */
  begin: () => () => void;
  /** Number of captures currently open. Test and debug aid. */
  activeCount: () => number;
}

/** Creates a suspender that talks through `send` and schedules its keepalive with `timers`. */
export function createHotkeySuspender(
  send: HotkeySuspendSender,
  timers: HotkeySuspendTimers = {
    setInterval: (fn, ms) => globalThis.setInterval(fn, ms),
    clearInterval: (handle) => globalThis.clearInterval(handle as ReturnType<typeof setInterval>),
  },
): HotkeySuspender {
  let count = 0;
  let keepalive: unknown = null;

  function dispatch(suspended: boolean): void {
    try {
      const result = send(suspended);
      if (result instanceof Promise) {
        result.catch((e: unknown) => warn(suspended, e));
      }
    } catch (e) {
      warn(suspended, e);
    }
  }

  function warn(suspended: boolean, e: unknown): void {
    console.warn(`Could not ${suspended ? 'pause' : 'resume'} the global hotkeys`, e);
  }

  function begin(): () => void {
    count += 1;
    if (count === 1) {
      dispatch(true);
      keepalive = timers.setInterval(() => dispatch(true), HOTKEY_SUSPEND_KEEPALIVE_MS);
    }
    let released = false;
    return () => {
      if (released) return;
      released = true;
      count -= 1;
      if (count === 0) {
        if (keepalive !== null) timers.clearInterval(keepalive);
        keepalive = null;
        dispatch(false);
      }
    };
  }

  return { begin, activeCount: () => count };
}

/** Sends through the daemon. Outside the Tauri app there is no daemon, so nothing is sent. */
function sendToDaemon(suspended: boolean): Promise<unknown> | undefined {
  if (!isTauriRuntime()) return undefined;
  return setHotkeysSuspended(suspended);
}

const defaultSuspender = createHotkeySuspender(sendToDaemon);

/**
 * Pauses the global hotkeys until the returned function is called. Call the release on every
 * path that ends the capture, and on component destroy. Safe to call more than once.
 */
export function beginHotkeyCapture(): () => void {
  return defaultSuspender.begin();
}
