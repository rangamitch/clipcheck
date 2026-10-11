/**
 * Autosave scheduler for the Settings screen. It knows nothing about the config: it only decides
 * WHEN one run of the injected `run` happens.
 *
 * - `schedule()` records a change and (re)starts the debounce. A run happens `debounceMs` after the
 *   last change, unless a change was made without a timer (`mark()`).
 * - `mark()` records a change without a timer. It waits for `flush()`, used for fields that commit
 *   on blur (for example a typed folder path).
 * - `flush()` runs now if a change is waiting. It clears the debounce and waits for a run in flight.
 * - Runs never overlap. A change that lands during a run is handled by a further run afterwards.
 * - `cancel()` drops the waiting change (revert, reload). A run in flight is not interrupted.
 *
 * Timers are injectable so the scheduler can be tested with a fake clock.
 */
export interface AutosaveTimers {
  set(run: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

export interface AutosaveOptions {
  debounceMs: number;
  /** One save attempt. It should not throw; a throw is reported through `onError`. */
  run: () => Promise<void>;
  onError?: (error: unknown) => void;
  timers?: AutosaveTimers;
}

export interface Autosave {
  /** A change was made: (re)start the debounce. */
  schedule(): void;
  /** A change was made that waits for `flush()`; no timer is started. */
  mark(): void;
  /** Run now if a change is waiting. Resolves when that run (and any run it waits for) is done. */
  flush(): Promise<void>;
  /** Drop the waiting change and the debounce. */
  cancel(): void;
  /** True while a change is waiting or a run is in flight. */
  readonly busy: boolean;
}

const defaultTimers: AutosaveTimers = {
  set: (run, ms) => setTimeout(run, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createAutosave(options: AutosaveOptions): Autosave {
  const timers = options.timers ?? defaultTimers;
  let timer: unknown = null;
  /** A change is waiting for a run. It is set again by any change made during a run. */
  let changed = false;
  let inFlight: Promise<void> | null = null;

  function clearTimer(): void {
    if (timer !== null) timers.clear(timer);
    timer = null;
  }

  /** Runs when a change is waiting and no debounce is pending. Waits for a run in flight first. */
  async function drain(): Promise<void> {
    for (;;) {
      if (inFlight) {
        await inFlight;
        continue;
      }
      if (!changed || timer !== null) return;
      changed = false;
      const attempt = (async () => {
        try {
          await options.run();
        } catch (e) {
          options.onError?.(e);
        }
      })();
      inFlight = attempt;
      try {
        await attempt;
      } finally {
        inFlight = null;
      }
    }
  }

  return {
    schedule() {
      changed = true;
      clearTimer();
      timer = timers.set(() => {
        timer = null;
        void drain();
      }, options.debounceMs);
    },
    mark() {
      changed = true;
    },
    flush() {
      clearTimer();
      return drain();
    },
    cancel() {
      clearTimer();
      changed = false;
    },
    get busy() {
      return changed || timer !== null || inFlight !== null;
    },
  };
}
