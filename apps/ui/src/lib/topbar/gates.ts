/**
 * Availability rules for the top-bar quick actions. Pure: no Svelte, no I/O, so the rules can be
 * read and tested in one place.
 */
import type { LiveStatus } from '../status';

/** The quick actions that can be gated. */
export type GateAction = 'clip' | 'start' | 'stop' | 'split';

/** Tooltip of every quick action while the recorder is turned off. */
export const RECORDER_OFF_REASON = 'Recorder is off';

/** How long a transient notice (action error, degraded recorder, toast) stays on screen. */
export const NOTICE_MS = 6000;

/**
 * Why a quick action cannot run right now, or null when it can. The reason is shown as the
 * chip's tooltip, so it should read as a sentence.
 */
export function gateReason(s: LiveStatus, action: GateAction): string | null {
  if (!s.connected) return 'The recorder is not running, so this is unavailable.';
  if (!s.recorderEnabled) return RECORDER_OFF_REASON;
  const attached = s.game !== null || s.session !== null;
  switch (action) {
    case 'clip':
    case 'start':
      return attached ? null : 'No game window is attached. Start a supported game first.';
    case 'stop':
    case 'split':
      return s.session !== null ? null : 'No session is recording.';
  }
}
