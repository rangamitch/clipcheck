// Volume state shared by every clip and every app session. YouTube-style: muting is volume 0, and
// `last` remembers the level to come back to. Pure helpers plus a localStorage round trip; the
// storage may be missing or throw (private windows, blocked site data), so every access is guarded.

import { clampVolume } from './model';

const STORAGE_KEY = 'clipcheck.player.volume.v1';

/** Level restored by unmuting when nothing audible was ever remembered. */
export const FALLBACK_VOLUME = 0.5;

/** `volume` is the slider (0 = muted); `last` is the audible level unmuting restores. */
export interface VolumeState {
  volume: number;
  last: number;
}

export const DEFAULT_VOLUME_STATE: VolumeState = { volume: 1, last: 1 };

/** True when the slider is at zero, which is what "muted" means. */
export function isMuted(s: VolumeState): boolean {
  return s.volume <= 0;
}

/** Sets the slider. Any audible level becomes the level unmuting returns to. */
export function setLevel(s: VolumeState, v: number): VolumeState {
  const volume = clampVolume(v);
  return { volume, last: volume > 0 ? volume : s.last };
}

/** Mute button: muted -> back to the remembered level; audible -> slider to 0, level remembered. */
export function toggleMute(s: VolumeState): VolumeState {
  if (isMuted(s)) {
    return { volume: s.last > 0 ? s.last : FALLBACK_VOLUME, last: s.last > 0 ? s.last : FALLBACK_VOLUME };
  }
  return { volume: 0, last: s.volume };
}

function defaultStorage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

/** Reads the saved state; anything missing or malformed gives the default (full volume). */
export function loadVolume(storage: Pick<Storage, 'getItem'> | null = defaultStorage()): VolumeState {
  try {
    const raw = storage?.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_VOLUME_STATE };
    const parsed = JSON.parse(raw) as Partial<VolumeState>;
    if (typeof parsed.volume !== 'number' || !Number.isFinite(parsed.volume)) {
      return { ...DEFAULT_VOLUME_STATE };
    }
    const volume = clampVolume(parsed.volume);
    const lastRaw = typeof parsed.last === 'number' && Number.isFinite(parsed.last) ? parsed.last : volume;
    const last = clampVolume(lastRaw);
    return { volume, last: last > 0 ? last : volume > 0 ? volume : 1 };
  } catch {
    return { ...DEFAULT_VOLUME_STATE };
  }
}

/** Saves the state. Never throws. */
export function saveVolume(
  s: VolumeState,
  storage: Pick<Storage, 'setItem'> | null = defaultStorage(),
): void {
  try {
    storage?.setItem(STORAGE_KEY, JSON.stringify({ volume: s.volume, last: s.last }));
  } catch {
    // Storage unavailable: the volume just is not remembered.
  }
}
