/**
 * Trailing-edge debounce. Each `call` restarts the wait; `fn` runs once, `waitMs` after the
 * last call. `cancel` drops a pending run (used on unmount).
 */
export interface Debounced {
  call: () => void;
  cancel: () => void;
  /** True while a run is scheduled. */
  readonly pending: boolean;
}

export function debounce(fn: () => void, waitMs: number): Debounced {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return {
    call() {
      if (timer !== undefined) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = undefined;
        fn();
      }, waitMs);
    },
    cancel() {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    },
    get pending() {
      return timer !== undefined;
    },
  };
}
