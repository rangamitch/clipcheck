/**
 * Clip hotkey list rules for the Recordings tab. Pure: no Svelte, no I/O.
 *
 * The schema limits (crates/core/src/config/validate.rs): at most 8 `[[hotkeys.clips]]`
 * entries, names 1..=32 characters, seconds 5..=3600 and never more than replay.max_seconds.
 */
import { isReservedChord, canonicalizeChord } from './hotkeys';
import { CLIP_MAX_SECONDS, CLIP_MIN_SECONDS, CLIP_NAME_MAX_CHARS, MAX_CLIPS } from './bounds';
import type { ClipHotkeyConfig, ClipPress } from './store';

export { CLIP_MAX_SECONDS, CLIP_MIN_SECONDS, CLIP_NAME_MAX_CHARS, MAX_CLIPS };

/** Hold time that makes a clip hotkey a long press (the daemon uses the same 400 ms). */
export const LONG_PRESS_MS = 400;
export const LONG_PRESS_SECONDS = LONG_PRESS_MS / 1000;

/** Options for the Tap / Long press select on each clip row. */
export const CLIP_PRESS_OPTIONS: { value: ClipPress; label: string; title: string }[] = [
  { value: 'tap', label: 'Tap', title: 'Saves when you press the key.' },
  {
    value: 'long',
    label: 'Long press',
    title: 'Saves when you hold the key for ' + LONG_PRESS_SECONDS + ' s.',
  },
];

/** The press type of a clip. A missing value (older config) counts as tap. */
export function pressOf(clip: Pick<ClipHotkeyConfig, 'press'>): ClipPress {
  return clip.press === 'long' ? 'long' : 'tap';
}

/**
 * The press a new clip gets for a chord: long when a tap clip already uses that chord (the
 * pair is then Tap + Long press), tap otherwise.
 */
export function defaultPressFor(chord: string, clips: readonly ClipHotkeyConfig[]): ClipPress {
  const canon = canonicalizeChord(chord) ?? chord;
  const tapUsesChord = clips.some((c) => (canonicalizeChord(c.keys) ?? c.keys) === canon && pressOf(c) === 'tap');
  return tapUsesChord ? 'long' : 'tap';
}

/** Preset chips for a clip length. */
export const CLIP_PRESETS: ReadonlyArray<{ label: string; seconds: number }> = [
  { label: '15 s', seconds: 15 },
  { label: '30 s', seconds: 30 },
  { label: '1 min', seconds: 60 },
  { label: '2 min', seconds: 120 },
  { label: '5 min', seconds: 300 },
];

/**
 * Chords tried for a new clip, in order. The first one not already in use wins. The bare
 * function keys F13..F24 need no modifier, so they are the last resort.
 */
const NEW_CLIP_CHORDS = [
  'Alt+F9',
  'Alt+F10',
  'Alt+F6',
  'Alt+F5',
  'Alt+F3',
  'Alt+F2',
  'Alt+F1',
  ...Array.from({ length: 12 }, (_, i) => `F${13 + i}`),
];

export type ClipSecondsProblem = 'too_short' | 'too_long' | 'not_whole';

/** What is wrong with a clip length, or null. `maxSeconds` is the replay buffer length. */
export function clipSecondsProblem(seconds: number, maxSeconds: number): ClipSecondsProblem | null {
  if (!Number.isInteger(seconds)) return 'not_whole';
  if (seconds < CLIP_MIN_SECONDS) return 'too_short';
  if (seconds > maxSeconds || seconds > CLIP_MAX_SECONDS) return 'too_long';
  return null;
}

/** The inline message for a clip length. Null when the length is fine. */
export function clipSecondsMessage(seconds: number, maxSeconds: number): string | null {
  switch (clipSecondsProblem(seconds, maxSeconds)) {
    case 'not_whole':
      return 'Enter whole seconds.';
    case 'too_short':
      return `A clip must be at least ${CLIP_MIN_SECONDS} seconds.`;
    case 'too_long':
      return `A clip cannot be longer than the replay buffer (${maxSeconds} s). Shorten it or lengthen the buffer.`;
    default:
      return null;
  }
}

/**
 * The length a clip should use after a fix: clamped into 5 s .. the replay length (or the
 * schema maximum when that is lower). Used by the "Use N s" suggestion.
 */
export function suggestClipSeconds(seconds: number, maxSeconds: number): number {
  const top = Math.min(maxSeconds, CLIP_MAX_SECONDS);
  const whole = Math.round(Number.isFinite(seconds) ? seconds : CLIP_MIN_SECONDS);
  return Math.min(top, Math.max(CLIP_MIN_SECONDS, whole));
}

/** Indexes of clips whose length is over the replay buffer. */
export function clipsOverLimit(clips: readonly ClipHotkeyConfig[], maxSeconds: number): number[] {
  const out: number[] = [];
  clips.forEach((c, i) => {
    if (clipSecondsProblem(c.seconds, maxSeconds) !== null) out.push(i);
  });
  return out;
}

export function canAddClip(clips: readonly ClipHotkeyConfig[]): boolean {
  return clips.length < MAX_CLIPS;
}

/** "Clip N" with N one more than the count, skipping names already taken. */
export function nextClipName(clips: readonly ClipHotkeyConfig[]): string {
  const used = new Set(clips.map((c) => c.name.toLowerCase()));
  for (let n = clips.length + 1; ; n++) {
    const candidate = `Clip ${n}`;
    if (!used.has(candidate.toLowerCase())) return candidate;
  }
}

/**
 * The first suggested chord that no clip or session hotkey uses and that Windows does not
 * reserve. Candidates are checked in order, F13..F24 included, so a chord that is already in
 * use is never returned. Returns null when every candidate is taken.
 */
export function suggestNewChord(usedChords: readonly string[]): string | null {
  const used = new Set(usedChords.map((c) => canonicalizeChord(c) ?? c));
  for (const candidate of NEW_CLIP_CHORDS) {
    const canon = canonicalizeChord(candidate) ?? candidate;
    if (!used.has(canon) && isReservedChord(canon) === null) return canon;
  }
  return null;
}

/**
 * Appends a clip with a new name, a 30 s length and a free chord. Returns the same list
 * (copied) when the list is full, or when no free chord is left (so no duplicate is added).
 */
export function addClip(
  clips: readonly ClipHotkeyConfig[],
  usedChords: readonly string[] = clips.map((c) => c.keys),
): ClipHotkeyConfig[] {
  if (!canAddClip(clips)) return [...clips];
  const keys = suggestNewChord(usedChords);
  if (keys === null) return [...clips];
  const fresh: ClipHotkeyConfig = {
    name: nextClipName(clips),
    keys,
    seconds: 30,
    press: defaultPressFor(keys, clips),
  };
  return [...clips, fresh];
}

export function removeClip(clips: readonly ClipHotkeyConfig[], index: number): ClipHotkeyConfig[] {
  if (index < 0 || index >= clips.length) return [...clips];
  return clips.filter((_, i) => i !== index);
}

/** Shallow patch of one clip. */
export function updateClip(
  clips: readonly ClipHotkeyConfig[],
  index: number,
  patch: Partial<ClipHotkeyConfig>,
): ClipHotkeyConfig[] {
  return clips.map((c, i) => (i === index ? { ...c, ...patch } : c));
}

/** "3 of 8 clip hotkeys" for the list header. */
export function clipCountText(count: number): string {
  return `${count} of ${MAX_CLIPS} clip hotkeys`;
}
