// Warm-up for a recording the user is likely to open (D60). Fire-and-forget: it never throws and
// never leaves an unhandled rejection. Used by the Gallery card `pointerenter` handler.

import { getMediaBase, mediaUrl, prepareAudio } from '../api';
import { isTauriRuntime } from '../window';

/** How many ids are remembered before the set is cleared (keeps memory bounded). */
const MAX_REMEMBERED = 200;

/** Ids already warmed in this session. */
const warmed = new Set<string>();

/** First 256 KiB of the video: enough for the moov atom and the first frames to reach the OS cache. */
const WARM_RANGE = 'bytes=0-262143';

/**
 * Warms the video bytes and the audio sidecars of one recording. Each id runs once per session
 * (until the set is cleared). Does nothing outside Tauri. Errors are swallowed on purpose.
 */
export function warmRecording(id: string): void {
  if (!isTauriRuntime() || typeof id !== 'string' || id === '') {
    return;
  }
  if (warmed.has(id)) {
    return;
  }
  if (warmed.size >= MAX_REMEMBERED) {
    warmed.clear();
  }
  warmed.add(id);

  try {
    void getMediaBase()
      .then((base) => fetch(mediaUrl(base, 'rec', id), { headers: { Range: WARM_RANGE } }))
      .then((r) => r.arrayBuffer())
      .catch(() => undefined);
  } catch {
    // fetch can throw synchronously in odd environments; warming is best effort.
  }

  try {
    prepareAudio(id).catch(() => undefined);
  } catch {
    // Same as above: a failed warm-up must not reach the caller.
  }
}

/** Clears the remembered ids. Tests only. */
export function _resetWarmForTests(): void {
  warmed.clear();
}
