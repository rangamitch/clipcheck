// Audio output device selection for the player. Pure helpers: no Svelte, no Web Audio graph.
// The sink id 'default' is the system default playback device (sinkId ''), never a
// communications device.

export const OUTPUT_STORAGE_KEY = 'clipcheck.player.output';
export const DEFAULT_OUTPUT_ID = 'default';
export const SYSTEM_DEFAULT_LABEL = 'System default';

export interface OutputDevice {
  id: string;
  label: string;
}

/** Anything with an optional `setSinkId`: an AudioContext or an HTMLMediaElement. */
export interface SinkTarget {
  setSinkId?: (id: string) => Promise<void>;
}

/**
 * Lists playback devices. The first entry is always the system default (id 'default').
 * Chromium's own 'default' and 'communications' pseudo-devices are skipped. Devices with an
 * empty label (no permission yet) get the fallback 'Output N', where N counts real devices.
 * Never throws: an unavailable or failing enumerateDevices gives just the default entry.
 */
export async function listOutputDevices(): Promise<OutputDevice[]> {
  const result: OutputDevice[] = [{ id: DEFAULT_OUTPUT_ID, label: SYSTEM_DEFAULT_LABEL }];
  const md = typeof navigator !== 'undefined' ? navigator.mediaDevices : undefined;
  if (!md || typeof md.enumerateDevices !== 'function') return result;

  let devices: MediaDeviceInfo[];
  try {
    devices = await md.enumerateDevices();
  } catch {
    return result;
  }

  let n = 0;
  for (const d of devices) {
    if (d.kind !== 'audiooutput') continue;
    if (d.deviceId === '' || d.deviceId === DEFAULT_OUTPUT_ID || d.deviceId === 'communications') {
      continue;
    }
    n += 1;
    const label = typeof d.label === 'string' && d.label.trim() !== '' ? d.label : `Output ${n}`;
    result.push({ id: d.deviceId, label });
  }
  return result;
}

/** The saved output id, or 'default' when nothing is saved or storage is unavailable. */
export function getSavedOutput(): string {
  try {
    const value = globalThis.localStorage?.getItem(OUTPUT_STORAGE_KEY);
    return typeof value === 'string' && value !== '' ? value : DEFAULT_OUTPUT_ID;
  } catch {
    return DEFAULT_OUTPUT_ID;
  }
}

/** Saves the output id. Storage errors are ignored. An empty id saves 'default'. */
export function saveOutput(id: string): void {
  try {
    globalThis.localStorage?.setItem(OUTPUT_STORAGE_KEY, id === '' ? DEFAULT_OUTPUT_ID : id);
  } catch {
    // Storage blocked or full: the choice simply is not remembered.
  }
}

/**
 * Routes `target` to the device `id`. 'default' (and '') maps to sinkId '' so the system
 * default device is used. Resolves true when setSinkId succeeded. Resolves false, and never
 * throws, when the target is null, has no setSinkId (unsupported engine), or the call fails
 * (device gone, context closed).
 */
export async function applyOutput(target: SinkTarget | null, id: string): Promise<boolean> {
  if (!target || typeof target.setSinkId !== 'function') return false;
  const sinkId = id === DEFAULT_OUTPUT_ID || id === '' ? '' : id;
  try {
    await target.setSinkId(sinkId);
    return true;
  } catch {
    return false;
  }
}
