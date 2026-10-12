// Pure helpers for the inspector form fields (no DOM access).

/** Clamps `v` into `lo..hi`. A non-finite value gives `lo`. */
export function clamp(v: number, lo: number, hi: number): number {
  if (!Number.isFinite(v)) return lo;
  return Math.min(hi, Math.max(lo, v));
}

/** Rounds to the nearest even integer. */
export function roundEven(v: number): number {
  return Math.round(v / 2) * 2;
}

/** Parses a canvas dimension typed by the user. Returns null when it is not a whole number. */
export function parseDimension(text: string): number | null {
  const s = text.trim();
  if (!/^\d+$/.test(s)) return null;
  const n = Number(s);
  return Number.isSafeInteger(n) ? n : null;
}

/**
 * Parses a decibel value typed by the user, such as `-6`, `-6.5 dB` or `+3dB`. Returns null when the
 * text is not a finite number.
 */
export function parseDb(text: string): number | null {
  const s = text.trim().replace(/dB$/i, '').trim();
  if (!/^[+-]?\d+(\.\d+)?$/.test(s)) return null;
  const n = Number(s);
  return Number.isFinite(n) ? n : null;
}

/** Formats a decibel value for display, such as `-6.0 dB` or `+3.0 dB`. */
export function formatDb(v: number): string {
  if (!Number.isFinite(v)) return '0.0 dB';
  const r = Math.round(v * 10) / 10;
  const sign = r > 0 ? '+' : '';
  return `${sign}${r.toFixed(1)} dB`;
}

/**
 * Parses a percentage typed by the user, such as `50`, `50%` or `12.5 %`. Returns the number
 * without the sign (0..100 is not enforced here), or null when the text is not a number.
 */
export function parsePercent(text: string): number | null {
  const s = text.trim().replace(/%$/, '').trim();
  if (!/^[+-]?\d+(\.\d+)?$/.test(s)) return null;
  const n = Number(s);
  return Number.isFinite(n) ? n : null;
}

/** A debounced function with `flush` (run now with the last arguments) and `cancel`. */
export interface Debounced<A extends unknown[]> {
  /** Schedules a call after `ms` of idle time, replacing any pending call. */
  call: (...args: A) => void;
  /** Runs the pending call now, if there is one. */
  flush: () => void;
  /** Drops the pending call without running it. */
  cancel: () => void;
}

/**
 * Wraps `fn` so that repeated `call`s run once, `ms` after the last one. `flush` runs the pending
 * call immediately (used on slider release).
 */
export function debounce<A extends unknown[]>(fn: (...args: A) => void, ms: number): Debounced<A> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: A | null = null;

  const cancel = (): void => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    pending = null;
  };

  const flush = (): void => {
    const args = pending;
    cancel();
    if (args !== null) fn(...args);
  };

  const call = (...args: A): void => {
    pending = args;
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(flush, ms);
  };

  return { call, flush, cancel };
}
