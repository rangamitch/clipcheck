// Typed wrappers for the clip trimmer backend. Contract: docs/impl/features-f1.md D27.
// Backend commands are implemented in src-tauri later; this file only names the contract.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { TrimMode } from './player/trim';

export type { TrimMode };

export interface TrimRequest {
  id: string;
  startMs: number;
  endMs: number;
  mode: TrimMode;
  overwrite: false;
  /** The new clip keeps the source clip's date and favourite flag ("Save clip"). */
  keepTimestamp?: boolean;
}

export interface TrimStarted {
  jobId: string;
}

export interface TrimPreviewRequest {
  id: string;
  startMs: number;
  endMs: number;
  mode: TrimMode;
}

/** Planner result for the snap readout. */
export interface TrimPreview {
  actualStartMs: number;
  actualEndMs: number;
  snapDeltaMs: number;
}

export interface TrimProgress {
  jobId: string;
  percent: number;
  outTimeMs: number;
}

export interface TrimDone {
  jobId: string;
  ok: boolean;
  newId?: string;
  error?: string;
}

/** Starts a trim job. Resolves with the job id; progress and completion arrive as events. */
export function startTrim(req: TrimRequest): Promise<TrimStarted> {
  return invoke<TrimStarted>('trim_clip', { req });
}

/** Cancels a running trim job. The selection stays intact in the UI. */
export function cancelTrim(jobId: string): Promise<void> {
  return invoke<void>('cancel_trim', { jobId });
}

/** Gets the actual output range for a requested selection (lossless snap readout). */
export function getTrimPreview(req: TrimPreviewRequest): Promise<TrimPreview> {
  return invoke<TrimPreview>('get_trim_preview', {
    id: req.id,
    startMs: req.startMs,
    endMs: req.endMs,
    mode: req.mode,
  });
}

/** Subscribes to `trim-progress`. Resolves with the unlisten function. */
export function onTrimProgress(cb: (p: TrimProgress) => void): Promise<UnlistenFn> {
  return listen<TrimProgress>('trim-progress', (event) => cb(event.payload));
}

/** Subscribes to `trim-done`. Resolves with the unlisten function. */
export function onTrimDone(cb: (d: TrimDone) => void): Promise<UnlistenFn> {
  return listen<TrimDone>('trim-done', (event) => cb(event.payload));
}
