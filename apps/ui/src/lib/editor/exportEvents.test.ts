import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

// Fake event bus: each `onExport*` helper records its subscription and hands back an unlisten
// function. `hold` delays the subscription until `release` runs; `fail` makes one subscribe reject.
const h = vi.hoisted(() => {
  const handlers = new Map<string, (payload: unknown) => void>();
  const subscribed: string[] = [];
  const unlistened: string[] = [];
  const fail = new Set<string>();
  const release: Array<() => void> = [];
  const state = { hold: false };

  const sub =
    (name: string) =>
    (cb: (payload: unknown) => void): Promise<() => void> => {
      if (fail.has(name)) return Promise.reject(new Error(`no bus for ${name}`));
      const attach = (): (() => void) => {
        subscribed.push(name);
        handlers.set(name, cb);
        return () => {
          unlistened.push(name);
          handlers.delete(name);
        };
      };
      if (state.hold) {
        return new Promise<() => void>((resolve) => {
          release.push(() => resolve(attach()));
        });
      }
      return Promise.resolve(attach());
    };

  return {
    handlers,
    subscribed,
    unlistened,
    fail,
    release,
    state,
    sub,
    queue: vi.fn(),
  };
});

vi.mock('./api', () => ({
  editorApi: { exportQueue: () => h.queue() },
  onExportProgress: h.sub('export-progress'),
  onExportDone: h.sub('export-done'),
  onExportFailed: h.sub('export-failed'),
  onExportQueue: h.sub('export-queue'),
}));

// Module-level state (the toast-once set, the active subscription, the stores) is reloaded per test
// with vi.resetModules(); the fake event bus above is shared and keeps its handlers.
type ExportEventsModule = typeof import('./exportEvents');
type StoreModule = typeof import('./store');
let startExportEvents: ExportEventsModule['startExportEvents'];
let closeEditor: StoreModule['closeEditor'];
let exportJobs: StoreModule['exportJobs'];
let toast: StoreModule['toast'];
import type { JobDto } from './types';

function job(id: string, over: Partial<JobDto> = {}): JobDto {
  return {
    id,
    projectName: 'Project',
    state: 'running',
    percent: 0,
    elapsedS: 0,
    etaS: null,
    speedX: 1,
    outPath: 'D:\\Exports\\out.mp4',
    bytes: 0,
    pausedReason: null,
    error: null,
    warnings: [],
    encoder: null,
    recordingId: null,
    ...over,
  };
}

/** Lets the queue load and any pending subscription settle (real timers). */
const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

/** Delivers a payload to the active handler of an event. Fails the test if none is subscribed. */
function emit(name: string, payload: unknown): void {
  const cb = h.handlers.get(name);
  if (cb === undefined) throw new Error(`no subscription for ${name}`);
  cb(payload);
}

const ALL_EVENTS = ['export-done', 'export-failed', 'export-progress', 'export-queue'];

/** Teardown of the subscription started by the current test, if any. */
let stop: (() => void) | undefined;

beforeEach(async () => {
  vi.resetAllMocks();
  vi.resetModules();
  ({ startExportEvents } = await import('./exportEvents'));
  ({ closeEditor, exportJobs, toast } = await import('./store'));
  h.handlers.clear();
  h.subscribed.length = 0;
  h.unlistened.length = 0;
  h.fail.clear();
  h.release.length = 0;
  h.state.hold = false;
  h.queue.mockResolvedValue([]);
  closeEditor();
  exportJobs.set([]);
  stop = undefined;
});

afterEach(() => {
  if (stop) stop();
  stop = undefined;
  h.state.hold = false;
  closeEditor();
});

/** Starts the subscriptions and waits until the queue load has finished. */
async function start(): Promise<() => void> {
  const off = startExportEvents();
  stop = off;
  await flush();
  return off;
}

// ---------------------------------------------------------------------------------------------
describe('subscriptions', () => {
  it('start subscribes to the four export events', async () => {
    await start();
    expect([...h.subscribed].sort()).toEqual(ALL_EVENTS);
  });

  it('start loads the current queue once', async () => {
    await start();
    expect(h.queue).toHaveBeenCalledTimes(1);
  });

  it('the loaded queue becomes the export job list', async () => {
    h.queue.mockResolvedValueOnce([job('q1'), job('q2')]);
    await start();
    expect(get(exportJobs).map((j) => j.id)).toEqual(['q1', 'q2']);
  });

  it('a failed queue load leaves the list empty and does not throw', async () => {
    h.queue.mockRejectedValueOnce(new Error('no runtime'));
    await expect(start()).resolves.toBeTypeOf('function');
    expect(get(exportJobs)).toEqual([]);
  });

  it('a second start while a subscription is active subscribes nothing new', async () => {
    const first = await start();
    const second = startExportEvents();
    await flush();
    expect(h.subscribed.length).toBe(4);
    expect(second).not.toBe(first);
  });

  it('the second start returns a function that does nothing when called', async () => {
    await start();
    const second = startExportEvents();
    second();
    expect(h.unlistened.length).toBe(0);
    expect(h.handlers.size).toBe(4);
  });

  it('a second start does not reload the queue', async () => {
    await start();
    startExportEvents();
    await flush();
    expect(h.queue).toHaveBeenCalledTimes(1);
  });

  it('after teardown a new start subscribes again', async () => {
    const off = await start();
    off();
    stop = undefined;
    await start();
    expect(h.subscribed.length).toBe(8);
  });

  it('a failing subscription does not stop the other events', async () => {
    h.fail.add('export-done');
    await start();
    expect(h.handlers.has('export-progress')).toBe(true);
    expect(h.handlers.has('export-failed')).toBe(true);
    expect(h.handlers.has('export-queue')).toBe(true);
    expect(h.handlers.has('export-done')).toBe(false);
  });

  it('a failing subscription still gives a usable teardown', async () => {
    h.fail.add('export-failed');
    const off = await start();
    expect(() => off()).not.toThrow();
  });
});

// ---------------------------------------------------------------------------------------------
describe('teardown', () => {
  it('teardown unlistens all four events', async () => {
    const off = await start();
    off();
    stop = undefined;
    expect([...h.unlistened].sort()).toEqual(ALL_EVENTS);
  });

  it('teardown twice does not unlisten twice', async () => {
    const off = await start();
    off();
    off();
    stop = undefined;
    expect(h.unlistened.length).toBe(4);
  });

  it('after teardown no handler is left subscribed', async () => {
    const off = await start();
    off();
    stop = undefined;
    expect(h.handlers.size).toBe(0);
  });

  it('teardown before the subscriptions resolve unlistens them as they arrive', async () => {
    h.state.hold = true;
    const off = startExportEvents();
    // Let the four subscribe calls run; each one parks until released.
    await flush();
    expect(h.release.length).toBe(4);
    off();
    for (const r of h.release.splice(0)) r();
    await flush();
    expect(h.unlistened.length).toBe(4);
    expect(h.handlers.size).toBe(0);
  });

  it('a teardown that comes before the queue load stops later list updates', async () => {
    let resolveQueue: (jobs: JobDto[]) => void = () => {};
    h.queue.mockReturnValueOnce(new Promise<JobDto[]>((r) => (resolveQueue = r)));
    const off = startExportEvents();
    off();
    resolveQueue([job('late')]);
    await flush();
    expect(get(exportJobs)).toEqual([]);
  });

  it('a stale subscription from an earlier start can be torn down without touching a newer one', async () => {
    const first = await start();
    first();
    stop = undefined;
    const second = await start();
    first();
    expect(h.handlers.size).toBe(4);
    second();
    stop = undefined;
    expect(h.handlers.size).toBe(0);
  });
});

// ---------------------------------------------------------------------------------------------
describe('progress events', () => {
  it('a progress event for a new job appends it', async () => {
    await start();
    emit('export-progress', job('a', { percent: 10 }));
    expect(get(exportJobs).map((j) => j.id)).toEqual(['a']);
  });

  it('a progress event for a known job replaces it', async () => {
    await start();
    emit('export-progress', job('a', { percent: 10 }));
    emit('export-progress', job('a', { percent: 50 }));
    expect(get(exportJobs)).toHaveLength(1);
    expect(get(exportJobs)[0]?.percent).toBe(50);
  });

  it('progress keeps the order of the other jobs', async () => {
    h.queue.mockResolvedValueOnce([job('a'), job('b'), job('c')]);
    await start();
    emit('export-progress', job('b', { percent: 80 }));
    expect(get(exportJobs).map((j) => j.id)).toEqual(['a', 'b', 'c']);
    expect(get(exportJobs)[1]?.percent).toBe(80);
  });

  it('a progress event does not raise a toast', async () => {
    await start();
    emit('export-progress', job('a', { percent: 99 }));
    expect(get(toast)).toBeNull();
  });

  it('a progress event carries the paused state through', async () => {
    await start();
    emit('export-progress', job('a', { state: 'paused', pausedReason: 'recording' }));
    expect(get(exportJobs)[0]).toMatchObject({ state: 'paused', pausedReason: 'recording' });
  });

  it('progress updates the list even after an earlier done event', async () => {
    await start();
    emit('export-done', job('a', { state: 'done', percent: 100 }));
    emit('export-progress', job('a', { state: 'running', percent: 5 }));
    expect(get(exportJobs)[0]?.state).toBe('running');
  });
});

// ---------------------------------------------------------------------------------------------
describe('done events', () => {
  it('a done event replaces the job and sets the finished toast', async () => {
    await start();
    emit('export-progress', job('a'));
    emit('export-done', job('a', { state: 'done', percent: 100, outPath: 'D:\\Exports\\clip.mp4' }));
    expect(get(exportJobs)[0]?.state).toBe('done');
    expect(get(toast)).toBe('Export finished: clip.mp4');
  });

  it('the toast uses only the file name of a Windows path', async () => {
    await start();
    emit('export-done', job('a', { state: 'done', outPath: 'C:\\Users\\me\\Videos\\my cut.mp4' }));
    expect(get(toast)).toBe('Export finished: my cut.mp4');
  });

  it('the toast uses only the file name of a POSIX path', async () => {
    await start();
    emit('export-done', job('a', { state: 'done', outPath: '/home/u/out.mov' }));
    expect(get(toast)).toBe('Export finished: out.mov');
  });

  it('a bare file name is shown as it is', async () => {
    await start();
    emit('export-done', job('a', { state: 'done', outPath: 'plain.mp4' }));
    expect(get(toast)).toBe('Export finished: plain.mp4');
  });

  it('a done event for an unknown job appends it', async () => {
    await start();
    emit('export-done', job('new', { state: 'done' }));
    expect(get(exportJobs).map((j) => j.id)).toEqual(['new']);
  });

  it('a job raises its finished toast only once', async () => {
    await start();
    emit('export-done', job('once', { state: 'done', outPath: 'one.mp4' }));
    toast.set(null);
    emit('export-done', job('once', { state: 'done', outPath: 'one.mp4' }));
    expect(get(toast)).toBeNull();
  });

  it('a second done event for the same job keeps one entry', async () => {
    await start();
    emit('export-done', job('a', { state: 'done' }));
    emit('export-done', job('a', { state: 'done' }));
    expect(get(exportJobs)).toHaveLength(1);
  });

  it('a done toast replaces an earlier toast', async () => {
    await start();
    toast.set('Something else');
    emit('export-done', job('a', { state: 'done', outPath: 'x.mp4' }));
    expect(get(toast)).toBe('Export finished: x.mp4');
  });

  it('a done event keeps the warnings of the job', async () => {
    await start();
    emit('export-done', job('a', { state: 'done', warnings: ['audio dropped'] }));
    expect(get(exportJobs)[0]?.warnings).toEqual(['audio dropped']);
  });
});

// ---------------------------------------------------------------------------------------------
describe('failed events', () => {
  it('a failed event replaces the job and sets the error toast', async () => {
    await start();
    emit('export-progress', job('a'));
    emit('export-failed', job('a', { state: 'failed', error: 'codec error' }));
    expect(get(exportJobs)[0]?.state).toBe('failed');
    expect(get(toast)).toBe('Export failed: codec error');
  });

  it('a failed event without error text says unknown error', async () => {
    await start();
    emit('export-failed', job('a', { state: 'failed', error: null }));
    expect(get(toast)).toBe('Export failed: unknown error');
  });

  it('a failed event for an unknown job appends it', async () => {
    await start();
    emit('export-failed', job('z', { state: 'failed', error: 'disk full' }));
    expect(get(exportJobs).map((j) => j.id)).toEqual(['z']);
  });

  it('a failed event does not change the other jobs', async () => {
    h.queue.mockResolvedValueOnce([job('a'), job('b')]);
    await start();
    emit('export-failed', job('a', { state: 'failed', error: 'x' }));
    expect(get(exportJobs)[1]?.state).toBe('running');
  });

  it('a failed event keeps the error text on the job', async () => {
    await start();
    emit('export-failed', job('a', { state: 'failed', error: 'out of memory' }));
    expect(get(exportJobs)[0]?.error).toBe('out of memory');
  });
});

// ---------------------------------------------------------------------------------------------
describe('queue events', () => {
  it('a queue event replaces the whole list', async () => {
    h.queue.mockResolvedValueOnce([job('a'), job('b')]);
    await start();
    emit('export-queue', [job('c')]);
    expect(get(exportJobs).map((j) => j.id)).toEqual(['c']);
  });

  it('an empty queue event clears the list', async () => {
    h.queue.mockResolvedValueOnce([job('a')]);
    await start();
    emit('export-queue', []);
    expect(get(exportJobs)).toEqual([]);
  });

  it('a queue event does not raise a toast', async () => {
    await start();
    emit('export-queue', [job('a', { state: 'done' })]);
    expect(get(toast)).toBeNull();
  });

  it('a queue event after progress uses the queue as the truth', async () => {
    await start();
    emit('export-progress', job('a', { percent: 40 }));
    emit('export-queue', [job('a', { percent: 60 })]);
    expect(get(exportJobs)[0]?.percent).toBe(60);
    expect(get(exportJobs)).toHaveLength(1);
  });

  it('a queue event keeps the job order it was given', async () => {
    await start();
    emit('export-queue', [job('b'), job('a'), job('c')]);
    expect(get(exportJobs).map((j) => j.id)).toEqual(['b', 'a', 'c']);
  });
});

// ---------------------------------------------------------------------------------------------
describe('closeEditor and the job list', () => {
  it('closeEditor clears the toast but keeps the export job list (exports outlive the editor)', async () => {
    h.queue.mockResolvedValueOnce([job('a')]);
    await start();
    emit('export-done', job('a', { state: 'done', outPath: 'a.mp4' }));
    closeEditor();
    expect(get(toast)).toBeNull();
    expect(get(exportJobs).map((j) => j.id)).toEqual(['a']);
  });
});

// ---------------------------------------------------------------------------------------------
describe('one toast per job id', () => {
  it('a failed event after a done event for the same job raises no second toast', async () => {
    await start();
    emit('export-done', job('j1', { state: 'done', outPath: 'D:/out/first.mp4' }));
    toast.set(null);
    emit('export-failed', job('j1', { state: 'failed', error: 'late error' }));
    expect(get(toast)).toBeNull();
  });

  it('a done event after a failed event for the same job raises no second toast', async () => {
    await start();
    emit('export-failed', job('j2', { state: 'failed', error: 'first error' }));
    expect(get(toast)).toBe('Export failed: first error');
    toast.set(null);
    emit('export-done', job('j2', { state: 'done', outPath: 'D:/out/x.mp4' }));
    expect(get(toast)).toBeNull();
  });

  it('two different jobs each raise their own toast', async () => {
    await start();
    emit('export-done', job('a', { state: 'done', outPath: 'D:/out/a.mp4' }));
    expect(get(toast)).toBe('Export finished: a.mp4');
    emit('export-done', job('b', { state: 'done', outPath: 'D:/out/b.mp4' }));
    expect(get(toast)).toBe('Export finished: b.mp4');
  });

  it('a repeated failed event for the same job raises the toast once', async () => {
    await start();
    emit('export-failed', job('r', { state: 'failed', error: 'boom' }));
    toast.set(null);
    emit('export-failed', job('r', { state: 'failed', error: 'boom' }));
    emit('export-failed', job('r', { state: 'failed', error: 'boom again' }));
    expect(get(toast)).toBeNull();
  });

  it('a repeated progress event for a finished job raises no toast', async () => {
    await start();
    emit('export-done', job('p', { state: 'done', outPath: 'D:/out/p.mp4' }));
    toast.set(null);
    emit('export-progress', job('p', { state: 'running', percent: 50 }));
    expect(get(toast)).toBeNull();
  });

  it('the job list still records the latest state after a suppressed toast', async () => {
    await start();
    emit('export-done', job('s', { state: 'done', outPath: 'D:/out/s.mp4' }));
    emit('export-failed', job('s', { state: 'failed', error: 'after' }));
    expect(get(exportJobs).find((j) => j.id === 's')?.state).toBe('failed');
  });
});
