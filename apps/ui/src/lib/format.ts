/**
 * Pure display helpers for the Gallery, the live panel and the status chip. No DOM or Tauri
 * imports, so they are unit-testable. Duration helpers:
 * - `formatDuration(ms)`: media length, `m:ss` / `h:mm:ss`.
 * - `formatClock(ms)`: live clock, `MM:SS`.
 * - The seconds-based "2 min 30 s" variant is `formatSpanSeconds` in `settings/pure.ts`.
 */

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const;

/** Formats a byte count with binary units, e.g. `1536` -> `"1.5 KB"`, `734003200` -> `"700 MB"`. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return '0 B';
  }
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) {
    return `${Math.floor(value)} B`;
  }
  const digits = value >= 10 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/**
 * Splits a millisecond duration into whole hours (unbounded), minutes and seconds. Fractions of
 * a second are truncated. Negative or non-finite input counts as zero. Shared by the two
 * duration formatters below so the arithmetic lives in one place.
 */
function splitDuration(ms: number): { hours: number; minutes: number; seconds: number } {
  const safe = Number.isFinite(ms) && ms > 0 ? ms : 0;
  const totalSeconds = Math.floor(safe / 1000);
  return {
    hours: Math.floor(totalSeconds / 3600),
    minutes: Math.floor((totalSeconds % 3600) / 60),
    seconds: totalSeconds % 60,
  };
}

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/**
 * Formats a millisecond duration as `m:ss`, or `h:mm:ss` from one hour up. Used for media
 * lengths. Fractions of a second are truncated. `93000` -> `"1:33"`, `3723000` -> `"1:02:03"`.
 */
export function formatDuration(ms: number): string {
  const { hours, minutes, seconds } = splitDuration(ms);
  if (hours > 0) {
    return `${hours}:${pad2(minutes)}:${pad2(seconds)}`;
  }
  return `${minutes}:${pad2(seconds)}`;
}

/**
 * Formats a live clock with a zero-padded minute field: `MM:SS` under an hour, `H:MM:SS` under a
 * day, then `Nd H:MM:SS`. Negative or non-finite input counts as zero. `5000` -> `"00:05"`,
 * `86400000` -> `"1d 0:00:00"`. Used by the recorder status chip and live panel.
 */
export function formatClock(ms: number): string {
  const { hours: totalHours, minutes, seconds } = splitDuration(ms);
  const days = Math.floor(totalHours / 24);
  const hours = totalHours % 24;
  if (days > 0) return `${days}d ${hours}:${pad2(minutes)}:${pad2(seconds)}`;
  if (hours > 0) return `${hours}:${pad2(minutes)}:${pad2(seconds)}`;
  return `${pad2(minutes)}:${pad2(seconds)}`;
}

/**
 * Formats a UTC millisecond timestamp relative to `nowMs`:
 * "just now", "5 min ago", "3 h ago", "2 d ago", then a local `YYYY-MM-DD` date.
 * Timestamps in the future count as "just now".
 */
export function formatRelativeDate(timestampMs: number, nowMs: number = Date.now()): string {
  if (!Number.isFinite(timestampMs)) {
    return '';
  }
  const diff = nowMs - timestampMs;
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  if (diff < minute) {
    return 'just now';
  }
  if (diff < hour) {
    return `${Math.floor(diff / minute)} min ago`;
  }
  if (diff < day) {
    return `${Math.floor(diff / hour)} h ago`;
  }
  if (diff < 7 * day) {
    return `${Math.floor(diff / day)} d ago`;
  }
  const d = new Date(timestampMs);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const dd = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${dd}`;
}
