// Export job events: keeps the `exportJobs` store in sync with the Rust export manager and raises
// a toast when a job finishes or fails.
//
// Subscriptions are shared: the first `startExportEvents()` call subscribes, later calls return a
// no-op until the active subscription is torn down. Outside Tauri (tests, plain browser) every
// listener falls back to a no-op, so importing this module never throws.
import type { UnlistenFn } from '@tauri-apps/api/event';
import { editorApi, onExportDone, onExportFailed, onExportProgress, onExportQueue } from './api';
import { exportJobs, toast } from './store';
import type { JobDto } from './types';

/** Teardown of the active subscription, or null when no subscription is active. */
let activeTeardown: (() => void) | null = null;

/** Ids of jobs whose done or failed toast has already been raised (each job toasts once). */
const toastedJobIds = new Set<string>();

const noop = (): void => {};

/** Last path segment of a file path (accepts Windows and POSIX separators). */
function fileName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] ?? path;
}

/** Records that a job was toasted. Returns false when it already was (so the caller skips the toast). */
function markToasted(jobId: string): boolean {
  if (toastedJobIds.has(jobId)) return false;
  toastedJobIds.add(jobId);
  return true;
}

/** Replaces the job with the same id, or appends it when unknown. */
function upsert(job: JobDto): void {
  exportJobs.update((list) => {
    const index = list.findIndex((existing) => existing.id === job.id);
    if (index < 0) return [...list, job];
    const next = list.slice();
    next[index] = job;
    return next;
  });
}

/** Turns a listener factory into a safe unlisten handle (never throws, never rejects). */
function safeListen(subscribe: () => Promise<UnlistenFn>): Promise<() => void> {
  return Promise.resolve()
    .then(subscribe)
    .then((fn) => fn)
    .catch((): (() => void) => noop);
}

/**
 * Subscribes to the export events and loads the current queue. Returns the unsubscribe function.
 * A second call while a subscription is active returns a no-op.
 */
export function startExportEvents(): () => void {
  if (activeTeardown !== null) return noop;

  let disposed = false;
  const unlisteners: Array<() => void> = [];

  const track = (promise: Promise<() => void>): void => {
    void promise.then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisteners.push(fn);
      }
    });
  };

  track(
    safeListen(() =>
      onExportProgress((job) => {
        upsert(job);
      }),
    ),
  );

  track(
    safeListen(() =>
      onExportDone((job) => {
        upsert(job);
        if (!markToasted(job.id)) return;
        toast.set(`Export finished: ${fileName(job.outPath)}`);
      }),
    ),
  );

  track(
    safeListen(() =>
      onExportFailed((job) => {
        upsert(job);
        if (!markToasted(job.id)) return;
        toast.set(`Export failed: ${job.error ?? 'unknown error'}`);
      }),
    ),
  );

  track(
    safeListen(() =>
      onExportQueue((jobs) => {
        exportJobs.set(jobs);
      }),
    ),
  );

  void Promise.resolve()
    .then(() => editorApi.exportQueue())
    .then((jobs) => {
      if (!disposed) exportJobs.set(jobs);
    })
    .catch(() => {
      // No queue yet (or no Tauri runtime): the events fill the list as jobs start.
    });

  const teardown = (): void => {
    if (disposed) return;
    disposed = true;
    for (const fn of unlisteners.splice(0)) fn();
    if (activeTeardown === teardown) activeTeardown = null;
  };
  activeTeardown = teardown;

  return teardown;
}
