// Pure helpers for the player details panel (D60). No DOM or Tauri imports except types.

import type { RecordingDto } from '../api';
import { formatBytes, formatDuration } from '../format';

/** Longest allowed file stem (without the extension), in characters. */
const MAX_STEM_LENGTH = 200;

/** Characters Windows does not allow in file names. */
const INVALID_CHARS = /[<>:"/\|?*]/;

/** Device names Windows reserves, matched against the part before the first dot. */
const RESERVED_NAMES = new Set([
  'CON', 'PRN', 'AUX', 'NUL',
  'COM1', 'COM2', 'COM3', 'COM4', 'COM5', 'COM6', 'COM7', 'COM8', 'COM9',
  'LPT1', 'LPT2', 'LPT3', 'LPT4', 'LPT5', 'LPT6', 'LPT7', 'LPT8', 'LPT9',
]);

/**
 * Checks a file stem typed by the user (no extension). Returns null when it is valid, otherwise
 * a short message for the inline error. Checks run in order: blank, length, characters, control
 * characters, trailing dot or space, reserved device names.
 */
export function validateStem(s: string): string | null {
  if (s.trim() === '') {
    return 'Name cannot be empty';
  }
  if (s.length > MAX_STEM_LENGTH) {
    return `Name must be ${MAX_STEM_LENGTH} characters or fewer`;
  }
  if (INVALID_CHARS.test(s)) {
    return 'Name cannot contain < > : " / \ | ? *';
  }
  for (let i = 0; i < s.length; i += 1) {
    if (s.charCodeAt(i) < 32) {
      return 'Name cannot contain control characters';
    }
  }
  if (s.endsWith('.') || s.endsWith(' ')) {
    return 'Name cannot end with a dot or a space';
  }
  const base = s.split('.')[0].toUpperCase();
  if (RESERVED_NAMES.has(base)) {
    return `"${base}" is a reserved name on Windows`;
  }
  return null;
}

/**
 * Splits a file name at its last dot. `ext` includes the dot (`.mp4`). A name without a dot,
 * a leading-dot-only name (`.mp4`) and a name ending in a dot have an empty `ext`.
 */
export function splitName(fileName: string): { stem: string; ext: string } {
  const dot = fileName.lastIndexOf('.');
  if (dot <= 0 || dot === fileName.length - 1) {
    return { stem: fileName, ext: '' };
  }
  return { stem: fileName.slice(0, dot), ext: fileName.slice(dot) };
}

/**
 * Formats a UTC millisecond timestamp in local time as `D Mon YYYY, HH:MM`, e.g.
 * `11 Oct 2026, 14:03`. Built from `formatToParts` so the separators are fixed. A NaN or
 * non-positive timestamp gives `-`.
 */
export function formatDateTime(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) {
    return '-';
  }
  const parts = new Intl.DateTimeFormat('en-GB', {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).formatToParts(new Date(ms));
  const pick = (type: Intl.DateTimeFormatPartTypes): string =>
    parts.find((p) => p.type === type)?.value ?? '';
  // Some engines print midnight as 24 even with hour12: false.
  const hour = pick('hour') === '24' ? '00' : pick('hour');
  return `${pick('day')} ${pick('month')} ${pick('year')}, ${hour}:${pick('minute')}`;
}

/** One label/value line of the details panel. */
export interface DetailRow {
  label: string;
  value: string;
}

/** Human label for a recording kind. Unknown kinds are shown as they are. */
function kindLabel(kind: string): string {
  if (kind === 'clip') return 'Clip';
  if (kind === 'session') return 'Session';
  return kind;
}

/**
 * Rows for the details panel, in display order. Location is included only when `path` is a
 * non-empty string, because the path arrives later than the row itself.
 */
export function detailRows(row: RecordingDto, path?: string | null): DetailRow[] {
  const rows: DetailRow[] = [
    { label: 'Game', value: row.gameName ?? 'Unknown' },
    { label: 'Size', value: formatBytes(row.sizeBytes) },
    { label: 'Length', value: formatDuration(row.durationMs) },
    { label: 'Recorded', value: formatDateTime(row.createdAtMs) },
    { label: 'Type', value: kindLabel(row.kind) },
  ];
  if (typeof path === 'string' && path !== '') {
    rows.push({ label: 'Location', value: path });
  }
  return rows;
}
