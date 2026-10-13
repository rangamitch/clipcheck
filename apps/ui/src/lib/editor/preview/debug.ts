/**
 * Temporary investigation aid for the preview frame rate (D77 follow-up): runtime switches that
 * bisect the playback pipeline without rebuilds, and the pure helpers for the diagnostics line.
 */

import type { PreviewStats } from '../store';

/** Runtime switches of the preview pipeline. All on is the normal behaviour. */
export interface DebugFlags {
  /** false: only the default stream plays (no hidden <audio> elements for extra streams). */
  multiAudio: boolean;
  /** false: no Web Audio routing; elements play natively (pan, gain and envelopes do not apply). */
  webAudio: boolean;
  /** false: direct-video mode is never entered. */
  direct: boolean;
}

export const DEBUG_STORAGE_KEY = 'cc-editor-debug';

export function defaultDebugFlags(): DebugFlags {
  return { multiAudio: true, webAudio: true, direct: true };
}

/** Reads the saved flags. Missing, malformed or unavailable storage yields the defaults. */
export function loadDebugFlags(storage?: Pick<Storage, 'getItem'> | null): DebugFlags {
  const flags = defaultDebugFlags();
  try {
    const store = storage === undefined ? globalThis.localStorage : storage;
    if (!store) return flags;
    const raw = store.getItem(DEBUG_STORAGE_KEY);
    if (!raw) return flags;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return flags;
    const rec = parsed as Record<string, unknown>;
    for (const key of Object.keys(flags) as (keyof DebugFlags)[]) {
      if (typeof rec[key] === 'boolean') flags[key] = rec[key] as boolean;
    }
  } catch {
    // Storage may throw (private mode, blocked): use the defaults.
  }
  return flags;
}

/** Saves the flags. Returns false when the storage is unavailable. */
export function saveDebugFlags(flags: DebugFlags, storage?: Pick<Storage, 'setItem'> | null): boolean {
  try {
    const store = storage === undefined ? globalThis.localStorage : storage;
    if (!store) return false;
    store.setItem(DEBUG_STORAGE_KEY, JSON.stringify(flags));
    return true;
  } catch {
    return false;
  }
}

/** Per-second rate from a cumulative counter delta over `spanMs`. Never negative, 0 for no span. */
export function ratePerSec(delta: number, spanMs: number): number {
  if (!(spanMs > 0) || !Number.isFinite(delta)) return 0;
  return Math.max(0, delta) / (spanMs / 1000);
}

/** Seconds buffered ahead of `currentTime` in the range that contains it, else 0 (1 decimal). */
export function bufferedAhead(ranges: { length: number; start(i: number): number; end(i: number): number }, currentTime: number): number {
  for (let i = 0; i < ranges.length; i++) {
    if (currentTime >= ranges.start(i) - 0.05 && currentTime <= ranges.end(i)) {
      return Math.round(Math.max(0, ranges.end(i) - currentTime) * 10) / 10;
    }
  }
  return 0;
}

function signed(n: number): string {
  const s = n.toFixed(2);
  return n >= 0 ? `+${s}` : s;
}

/** The compact second monitor line, built from the optional diagnostic fields of the stats. */
export function formatDiagLine(s: PreviewStats): string {
  const parts: string[] = [];
  const lat =
    s.baseLatencyMs !== undefined || s.outputLatencyMs !== undefined
      ? ` lat ${Math.round(s.baseLatencyMs ?? 0)}/${Math.round(s.outputLatencyMs ?? 0)}ms`
      : '';
  parts.push(`ctx ${s.ctxState ?? 'none'}${lat}`);
  if (s.rs !== undefined) {
    parts.push(
      `v rs${s.rs} ns${s.ns ?? 0} ahead ${(s.aheadS ?? 0).toFixed(1)}s dec ${s.decodedFps ?? 0}/s drop ${s.droppedFrames ?? 0} wait ${s.waiting ?? 0} stall ${s.stalled ?? 0} drift ${signed(s.drift ?? 0)}`,
    );
  } else {
    parts.push('v none');
  }
  parts.push(`a ${s.audioPlaying ?? 0}/${s.audioEls ?? 0}`);
  parts.push(`long ${s.longTasks ?? 0} max ${Math.round(s.longTaskMaxMs ?? 0)}ms`);
  parts.push(s.mode ?? 'canvas');
  return parts.join(' | ');
}
