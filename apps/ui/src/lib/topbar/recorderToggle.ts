/**
 * Rules for the top-bar Recorder switch. Pure: no Svelte, no I/O, so the rules can be tested
 * on their own. The component owns the popover and the request.
 */
import type { LiveStatus } from '../status';

/** Confirm text shown before turning the recorder off while a session is recording. */
export const OFF_CONFIRM_TEXT = 'Turn off the recorder? The current session will be saved up to now.';

/** Accessible name of the switch. It names the action a press performs. */
export function switchLabel(recorderEnabled: boolean): string {
  return recorderEnabled ? 'Turn recorder off' : 'Turn recorder on';
}

/**
 * Why the switch cannot be used right now, or null when it can. `busy` is true while another
 * top-bar action (or this switch's own request) is in flight.
 */
export function switchDisabledReason(s: LiveStatus, busy: boolean): string | null {
  if (!s.connected) return 'The recorder is not running, so this is unavailable.';
  if (busy) return 'Another action is still running.';
  return null;
}

/**
 * True when turning the recorder off needs a confirm first: it is on and a session is recording.
 * With nothing recording the switch acts at once.
 */
export function needsOffConfirm(s: LiveStatus): boolean {
  return s.connected && s.recorderEnabled && s.session !== null;
}

/** The value a press sends: the opposite of the current state. */
export function nextRecorderEnabled(s: LiveStatus): boolean {
  return !s.recorderEnabled;
}
