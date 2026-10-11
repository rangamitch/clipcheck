// Pure player model. Contract: docs/impl/player-mvp.md. No DOM, no Svelte.

export interface TrackState {
  index: number;
  audible: boolean;
  solo: boolean;
  gainDb: number;
}

export const GAIN_MIN_DB = -20;
export const GAIN_MAX_DB = 6;
export const SPEEDS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2];
export const SEEK_STEP_S = 5;

export type PlayerAction =
  | { type: 'toggle-play' }
  | { type: 'seek'; deltaS: number }
  | { type: 'volume'; deltaPct: number }
  | { type: 'mute' }
  | { type: 'fullscreen' }
  | { type: 'speed'; dir: 1 | -1 }
  | { type: 'jump'; digit: number }
  | { type: 'track-toggle'; index: number }
  | { type: 'track-solo'; index: number }
  | { type: 'frame'; dir: 1 | -1 };

/** Clamps a number to [lo, hi]. */
function clamp(x: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, x));
}

/** Maps every track through `f`, returning a new array (input is never mutated). */
function mapTracks(s: TrackState[], i: number, f: (t: TrackState) => TrackState): TrackState[] {
  return s.map((t) => (t.index === i ? f(t) : { ...t }));
}

/** Track 0 is audible, the others are not; no solo; gain 0 dB. */
export function initTracks(count: number): TrackState[] {
  const n = Number.isFinite(count) ? Math.max(0, Math.floor(count)) : 0;
  const tracks: TrackState[] = [];
  for (let i = 0; i < n; i += 1) {
    tracks.push({ index: i, audible: i === 0, solo: false, gainDb: 0 });
  }
  return tracks;
}

/** Flips the audible flag of track `i`. Unknown index returns an unchanged copy. */
export function toggleTrack(s: TrackState[], i: number): TrackState[] {
  return mapTracks(s, i, (t) => ({ ...t, audible: !t.audible }));
}

/** Flips the solo flag of track `i`. Several tracks may be soloed at once. */
export function soloTrack(s: TrackState[], i: number): TrackState[] {
  return mapTracks(s, i, (t) => ({ ...t, solo: !t.solo }));
}

/** Sets the gain of track `i`, clamped to [GAIN_MIN_DB, GAIN_MAX_DB]. NaN becomes 0 dB. */
export function setGainDb(s: TrackState[], i: number, db: number): TrackState[] {
  const value = Number.isNaN(db) ? 0 : clamp(db, GAIN_MIN_DB, GAIN_MAX_DB);
  return mapTracks(s, i, (t) => ({ ...t, gainDb: value }));
}

/** Converts decibels to a linear amplitude factor. -Infinity and NaN give 0. */
export function dbToLinear(db: number): number {
  if (Number.isNaN(db) || db === -Infinity) {
    return 0;
  }
  return Math.pow(10, db / 20);
}

/**
 * Whether track `i` is audible. If any track has solo set, only soloed tracks
 * are audible; otherwise the audible flags decide. Unknown index is not audible.
 */
export function isTrackAudible(s: TrackState[], i: number): boolean {
  const track = s.find((t) => t.index === i);
  if (!track) {
    return false;
  }
  const anySolo = s.some((t) => t.solo);
  return anySolo ? track.solo : track.audible;
}

/** Effective linear gain of track `i`: 0 when muted or not audible, else gain times clamped volume. */
export function trackGain(s: TrackState[], i: number, volume: number, muted: boolean): number {
  if (muted || !isTrackAudible(s, i)) {
    return 0;
  }
  const track = s.find((t) => t.index === i);
  if (!track) {
    return 0;
  }
  return dbToLinear(track.gainDb) * clampVolume(volume);
}

/** Clamps a volume to [0, 1]. NaN becomes 1. */
export function clampVolume(v: number): number {
  if (Number.isNaN(v)) {
    return 1;
  }
  return clamp(v, 0, 1);
}

/**
 * Moves the volume by `deltaPct` percentage points (e.g. 5 = +0.05), clamped to
 * [0, 1] and rounded to 2 decimals. A NaN delta is treated as 0.
 */
export function stepVolume(v: number, deltaPct: number): number {
  const delta = Number.isNaN(deltaPct) ? 0 : deltaPct / 100;
  const next = clampVolume(clampVolume(v) + delta);
  return Math.round(next * 100) / 100;
}

/**
 * Next (dir 1) or previous (dir -1) entry of SPEEDS, clamped at the ends.
 * A speed not in the list moves to the nearest entry in the given direction.
 * NaN gives 1x.
 */
export function stepSpeed(cur: number, dir: 1 | -1): number {
  if (Number.isNaN(cur)) {
    return 1;
  }
  const idx = SPEEDS.indexOf(cur);
  const last = SPEEDS.length - 1;
  if (idx >= 0) {
    return SPEEDS[clamp(idx + dir, 0, last)];
  }
  if (dir > 0) {
    const above = SPEEDS.find((s) => s > cur);
    return above !== undefined ? above : SPEEDS[last];
  }
  const below = [...SPEEDS].reverse().find((s) => s < cur);
  return below !== undefined ? below : SPEEDS[0];
}

/**
 * Clamps a time to [0, duration]. NaN time gives 0. If the duration is unknown
 * (<= 0, NaN or Infinity), only the lower bound 0 applies.
 */
export function clampTime(t: number, durationS: number): number {
  if (Number.isNaN(t)) {
    return 0;
  }
  const known = durationS > 0 && durationS !== Infinity;
  if (!known) {
    return Math.max(0, t);
  }
  return clamp(t, 0, durationS);
}

/**
 * Formats seconds as `m:ss`, or `h:mm:ss` from one hour up. Fractions are floored.
 * NaN, Infinity and negative values give "0:00".
 */
export function formatTime(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) {
    return '0:00';
  }
  const total = Math.floor(sec);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const ss = String(seconds).padStart(2, '0');
  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, '0')}:${ss}`;
  }
  return `${minutes}:${ss}`;
}

/** Seek target for a digit key: digit (clamped to 0..9) times 10% of the duration. */
export function percentJump(digit: number, durationS: number): number {
  if (!(durationS > 0) || !Number.isFinite(durationS) || !Number.isFinite(digit)) {
    return 0;
  }
  const d = clamp(Math.trunc(digit), 0, 9);
  return (d * durationS) / 10;
}

/** True when the audio and video clocks differ by more than the threshold (strictly). NaN gives false. */
export function needsResync(videoT: number, audioT: number, thresholdS = 0.15): boolean {
  if (Number.isNaN(videoT) || Number.isNaN(audioT)) {
    return false;
  }
  return Math.abs(videoT - audioT) > thresholdS;
}

/** Trimmed title, or `Track N` (1-based) when the title is blank. */
export function trackLabel(t: { index: number; title: string }): string {
  const title = typeof t.title === 'string' ? t.title.trim() : '';
  return title !== '' ? title : `Track ${t.index + 1}`;
}

const DIGIT_CODE = /^(?:Digit|Numpad)([0-9])$/;

/** Maps a keyboard event to a player action, or null when the key is not a player key. */
export function keyToAction(e: {
  key: string;
  code?: string;
  shiftKey: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey?: boolean;
}): PlayerAction | null {
  if (e.ctrlKey || e.metaKey) {
    return null;
  }

  if (e.altKey) {
    // Alt changes `key` on some platforms, so prefer the physical key code.
    const fromCode = e.code !== undefined ? DIGIT_CODE.exec(e.code) : null;
    const digitText = fromCode ? fromCode[1] : /^[0-9]$/.test(e.key) ? e.key : null;
    if (digitText === null) {
      return null;
    }
    const digit = Number(digitText);
    if (digit === 0) {
      return null;
    }
    return e.shiftKey
      ? { type: 'track-solo', index: digit - 1 }
      : { type: 'track-toggle', index: digit - 1 };
  }

  const key = e.key;
  const lower = key.length === 1 ? key.toLowerCase() : key;

  if (key === '<' || (key === ',' && e.shiftKey)) {
    return { type: 'speed', dir: -1 };
  }
  if (key === '>' || (key === '.' && e.shiftKey)) {
    return { type: 'speed', dir: 1 };
  }

  if (key === ',') {
    return { type: 'frame', dir: -1 };
  }
  if (key === '.') {
    return { type: 'frame', dir: 1 };
  }

  if (/^[0-9]$/.test(key)) {
    return { type: 'jump', digit: Number(key) };
  }

  switch (lower) {
    case ' ':
    case 'Spacebar':
    case 'k':
      return { type: 'toggle-play' };
    case 'j':
      return { type: 'seek', deltaS: -10 };
    case 'l':
      return { type: 'seek', deltaS: 10 };
    case 'ArrowLeft':
      return { type: 'seek', deltaS: e.shiftKey ? -1 : -SEEK_STEP_S };
    case 'ArrowRight':
      return { type: 'seek', deltaS: e.shiftKey ? 1 : SEEK_STEP_S };
    case 'ArrowUp':
      return { type: 'volume', deltaPct: 5 };
    case 'ArrowDown':
      return { type: 'volume', deltaPct: -5 };
    case 'm':
      return { type: 'mute' };
    case 'f':
      return { type: 'fullscreen' };
    default:
      return null;
  }
}
