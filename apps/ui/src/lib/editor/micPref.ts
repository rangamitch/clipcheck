/**
 * Keeps the editor's microphone preference in step with `audio.mic.ptt_mix_gated`. The recorder's
 * Mix carries either the push-to-talk microphone (true, the default) or the open microphone
 * (false). The editor uses it to place only the microphone lane that the Mix already carries.
 */
import { invoke } from '@tauri-apps/api/core';
import { settings } from '../settings/store';

/** The `ptt_mix_gated` value of a config, true when it is missing or not a boolean. */
export function pttMixGatedOf(config: unknown): boolean {
  const value = (config as { audio?: { mic?: { ptt_mix_gated?: unknown } } } | null | undefined)
    ?.audio?.mic?.ptt_mix_gated;
  return typeof value === 'boolean' ? value : true;
}

/** Sends the preference to the backend. Errors are swallowed: the editor works without it. */
export async function sendMicPref(gated: boolean): Promise<void> {
  try {
    await invoke<void>('editor_set_mic_pref', { gated });
  } catch {
    // Keep the backend default (true) when the command is unavailable.
  }
}

/**
 * Sends the current preference now and again whenever it changes in the settings (the draft the
 * user sees, else the saved config). Returns the function that stops the watch.
 */
export function watchMicPref(): () => void {
  let last: boolean | null = null;
  const send = (gated: boolean): void => {
    if (gated === last) return;
    last = gated;
    void sendMicPref(gated);
  };
  return settings.subscribe((s) => {
    send(pttMixGatedOf(s.draft ?? s.saved));
  });
}
