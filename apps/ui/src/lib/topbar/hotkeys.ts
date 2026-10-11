/**
 * The hotkey chords the top bar shows next to its quick actions: each clip preset's chord and
 * the session split and start/stop chords. Read from `get_config`, refreshed when the config
 * changes and on every reconnect.
 */
import { writable } from 'svelte/store';
import { daemonConnected, onConnection, onEvent } from '../daemon';
import { fetchConfigShared } from '../status';
import { chordChipParts } from '../settings/hotkeys';
import { pressOf } from '../settings/clips';
import type { ClipPress } from '../settings/store';

/** Key caps for a hotkey chip, upper-cased: ["ALT", "F7"], or ["PAUSE"]. Empty when unbound. */
export function chipKeys(chord: string): string[] {
  return chordChipParts(chord);
}

export interface TopbarClip {
  name: string;
  seconds: number;
  /** Canonical chord such as `Ctrl+F7`. Empty when the clip has no hotkey. */
  keys: string;
  /** Tap, or long press (held for 400 ms). Missing in config means tap. */
  press: ClipPress;
}

export interface TopbarHotkeys {
  clips: TopbarClip[];
  /** Split chord. Empty when unbound. */
  split: string;
  /** Start/stop chord. Empty when unbound. */
  startStop: string;
}

export const EMPTY_HOTKEYS: TopbarHotkeys = { clips: [], split: '', startStop: '' };

/** Reads the chord fields from a config object. Tolerates missing or malformed sections. */
export function parseTopbarHotkeys(config: Record<string, unknown>): TopbarHotkeys {
  const hotkeys = config.hotkeys as { clips?: unknown; session?: unknown } | undefined;
  const clips: TopbarClip[] = [];
  if (Array.isArray(hotkeys?.clips)) {
    for (const entry of hotkeys.clips) {
      const e = entry as { name?: unknown; seconds?: unknown; keys?: unknown; press?: unknown };
      if (typeof e?.seconds === 'number' && e.seconds > 0) {
        clips.push({
          name: typeof e.name === 'string' && e.name.length > 0 ? e.name : `${e.seconds}s`,
          seconds: e.seconds,
          keys: typeof e.keys === 'string' ? e.keys : '',
          press: pressOf({ press: e.press as ClipPress }),
        });
      }
    }
  }
  const session = (hotkeys?.session ?? {}) as { split?: unknown; stop_start?: unknown };
  return {
    clips,
    split: typeof session.split === 'string' ? session.split : '',
    startStop: typeof session.stop_start === 'string' ? session.stop_start : '',
  };
}

export const topbarHotkeys = writable<TopbarHotkeys>(EMPTY_HOTKEYS);

/** Pulls the chords. Failures keep the previous values. */
export async function refreshTopbarHotkeys(): Promise<void> {
  try {
    const reply = await fetchConfigShared();
    if (reply.type === 'config') {
      topbarHotkeys.set(parseTopbarHotkeys(reply.data.config));
    }
  } catch {
    // Keep the previous values; the next config event or reconnect retries.
  }
}

/** Starts the listeners. Returns a stop function. */
export function startTopbarHotkeys(): () => void {
  const stopEvents = onEvent((ev) => {
    if (ev.type === 'config_changed' || ev.type === 'settings_reloaded') {
      void refreshTopbarHotkeys();
    }
  });
  const stopConnection = onConnection((connected) => {
    if (connected) void refreshTopbarHotkeys();
    else topbarHotkeys.set(EMPTY_HOTKEYS);
  });
  void daemonConnected().then((connected) => {
    if (connected) void refreshTopbarHotkeys();
  });
  return () => {
    stopEvents();
    stopConnection();
  };
}
