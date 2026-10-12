import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

// The Tauri-backed API is replaced by plain spies. The store itself is real.
const api = vi.hoisted(() => ({
  apply: vi.fn(),
  undo: vi.fn(),
  redo: vi.fn(),
  autosave: vi.fn(),
  setUiState: vi.fn(),
  saveProject: vi.fn(),
}));

vi.mock('./api', () => ({ editorApi: api }));

import {
  binDrag,
  busy,
  closeEditor,
  copySelection,
  dispatch,
  dispatchWith,
  exportJobs,
  loopInOut,
  shuttleRate,
  expandWithGroups,
  isBusy,
  openSnapshot,
  pasteAtPlayhead,
  playhead,
  playing,
  plan,
  project as projectStore,
  pruneSelection,
  pxPerSec,
  redo,
  scrollX,
  scrollY,
  seek,
  selectAllClips,
  selectClips,
  selectedClips,
  selection,
  snapshot,
  startAutosave,
  stopAutosave,
  timelineEndTick,
  toast,
  tool,
  undo,
  MIN_PX_PER_SEC,
  zoomBy,
  zoomToFit,
} from './store';
import type { ApplyReply, Clip, Command, EditorSnapshot, Project, Track } from './types';
import { defaultCanvas, defaultClipAudio, defaultExportSettings, defaultTransform } from './types';

const TPS = 254_016_000_000;
/** Whole seconds to ticks. */
const sec = (s: number): number => Math.round(s * TPS);

function track(id: string, kind: 'video' | 'audio'): Track {
  return {
    id,
    kind,
    name: id,
    muted: false,
    solo: false,
    locked: false,
    hidden: false,
    volume_db: 0,
    targeted: false,
    height: 56,
  };
}

function clip(id: string, trackId: string, start: number, dur: number, over: Partial<Clip> = {}): Clip {
  return {
    id,
    track_id: trackId,
    kind: 'video',
    media_id: 'm1',
    name: id,
    start,
    dur,
    src_in: 0,
    speed: 1,
    reverse: false,
    enabled: true,
    color: null,
    transform: defaultTransform(),
    opacity: 1,
    audio: defaultClipAudio(),
    link_id: null,
    group_id: null,
    transition_in: null,
    text: null,
    solid: null,
    ...over,
  };
}

function project(clips: Clip[], over: Partial<Project> = {}): Project {
  return {
    schema_version: 1,
    id: 'p1',
    name: 'Test',
    created_ms: 0,
    modified_ms: 0,
    canvas: defaultCanvas(),
    media: [],
    tracks: [track('v1', 'video'), track('a1', 'audio')],
    clips,
    groups: [],
    markers: [],
    work_area: null,
    modes: { magnetic: true, snap: true },
    export_defaults: defaultExportSettings(),
    ui_state: { zoom_px_per_sec: 80, scroll_x: 0, playhead: 0 },
    ...over,
  };
}

function snapshotOf(p: Project, over: Partial<EditorSnapshot> = {}): EditorSnapshot {
  return {
    project: p,
    can_undo: false,
    can_redo: false,
    dirty: false,
    plan: { canvas: p.canvas, start: 0, end: 0, segments: [] },
    clipboard_len: 0,
    ...over,
  };
}

function replyOf(p: Project): ApplyReply {
  return { applied: { created: [], removed: [] }, snapshot: snapshotOf(p) };
}

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void } {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Drains pending promise callbacks without relying on timers (safe with fake timers). */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i++) await Promise.resolve();
}

/** Four clips: group g1 = c1 + c2 on V1, c3 alone, c4 on A1 linked to c1. */
function selectionFixture(): Project {
  return project([
    clip('c1', 'v1', 0, sec(1), { group_id: 'g1', link_id: 'L1' }),
    clip('c2', 'v1', sec(1), sec(1), { group_id: 'g1' }),
    clip('c3', 'v1', sec(2), sec(1)),
    clip('c4', 'a1', 0, sec(2), { link_id: 'L1', kind: 'audio', media_id: 'm1' }),
  ]);
}

beforeEach(() => {
  vi.resetAllMocks();
  closeEditor();
  api.apply.mockImplementation(async () => replyOf(selectionFixture()));
});

afterEach(() => {
  closeEditor();
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------------------------
describe('openSnapshot', () => {
  it('installs the snapshot as the open project', () => {
    const s = snapshotOf(selectionFixture());
    openSnapshot(s);
    expect(get(snapshot)).toBe(s);
    expect(get(projectStore)).toBe(s.project);
  });

  it('clears the selection', () => {
    selection.set(['c1']);
    openSnapshot(snapshotOf(selectionFixture()));
    expect(get(selection)).toEqual([]);
  });

  it('restores the zoom from ui_state', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 120, scroll_x: 0, playhead: 0 } })));
    expect(get(pxPerSec)).toBe(120);
  });

  it('clamps a too small zoom from ui_state to the minimum zoom', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: MIN_PX_PER_SEC / 10, scroll_x: 0, playhead: 0 } })));
    expect(get(pxPerSec)).toBe(MIN_PX_PER_SEC);
  });

  it('clamps a too large zoom from ui_state to 4000 px per second', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 99_999, scroll_x: 0, playhead: 0 } })));
    expect(get(pxPerSec)).toBe(4000);
  });

  it('uses the default zoom when ui_state zoom is not finite', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: NaN, scroll_x: 0, playhead: 0 } })));
    expect(get(pxPerSec)).toBe(80);
  });

  it('restores horizontal scroll from ui_state', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 80, scroll_x: 240, playhead: 0 } })));
    expect(get(scrollX)).toBe(240);
  });

  it('clamps negative horizontal scroll to zero', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 80, scroll_x: -50, playhead: 0 } })));
    expect(get(scrollX)).toBe(0);
  });

  it('resets vertical scroll to zero', () => {
    scrollY.set(300);
    openSnapshot(snapshotOf(selectionFixture()));
    expect(get(scrollY)).toBe(0);
  });

  it('restores the playhead from ui_state', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 80, scroll_x: 0, playhead: sec(3) } })));
    expect(get(playhead)).toBe(sec(3));
  });

  it('clamps a playhead beyond the project limit', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 80, scroll_x: 0, playhead: sec(20 * 3600) } })));
    expect(get(playhead)).toBe(4 * 3600 * TPS);
  });

  it('clamps a negative playhead to zero', () => {
    openSnapshot(snapshotOf(project([], { ui_state: { zoom_px_per_sec: 80, scroll_x: 0, playhead: -sec(5) } })));
    expect(get(playhead)).toBe(0);
  });
});

// ---------------------------------------------------------------------------------------------
describe('closeEditor', () => {
  it('resets every store to its default', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c1']);
    playhead.set(sec(4));
    playing.set(true);
    tool.set('razor');
    pxPerSec.set(500);
    scrollX.set(90);
    scrollY.set(12);
    toast.set('hello');
    busy.set(true);
    exportJobs.set([{ id: 'j' } as never]);

    closeEditor();

    expect(get(snapshot)).toBeNull();
    expect(get(selection)).toEqual([]);
    expect(get(playhead)).toBe(0);
    expect(get(playing)).toBe(false);
    expect(get(tool)).toBe('select');
    expect(get(pxPerSec)).toBe(80);
    expect(get(scrollX)).toBe(0);
    expect(get(scrollY)).toBe(0);
    expect(get(toast)).toBeNull();
    expect(get(busy)).toBe(false);
  });

  it('keeps the export job list, because exports outlive the editor session', () => {
    const job = { id: 'j' } as never;
    exportJobs.set([job]);
    closeEditor();
    expect(get(exportJobs)).toEqual([job]);
  });

  it('stops autosave so no timer fires afterwards', () => {
    vi.useFakeTimers();
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(10);
    closeEditor();
    vi.advanceTimersByTime(60_000);
    expect(api.autosave).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------------------------
describe('dispatch', () => {
  it('returns null and makes no API call when no project is open', async () => {
    const r = await dispatch({ type: 'copy_clips', ids: ['c1'] });
    expect(r).toBeNull();
    expect(api.apply).not.toHaveBeenCalled();
    expect(get(toast)).toBeNull();
  });

  it('applies the command and returns the reply', async () => {
    const p = selectionFixture();
    openSnapshot(snapshotOf(p));
    const reply = replyOf(p);
    api.apply.mockResolvedValueOnce(reply);
    const r = await dispatch({ type: 'copy_clips', ids: ['c1'] });
    expect(r).toBe(reply);
  });

  it('sends the command object unchanged', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const cmd: Command = { type: 'split_at', time: sec(1), ids: ['c1'] };
    await dispatch(cmd);
    expect(api.apply).toHaveBeenCalledWith(cmd);
  });

  it('stores the reply snapshot as the current snapshot', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const next = snapshotOf(project([clip('x', 'v1', 0, sec(1))]));
    api.apply.mockResolvedValueOnce({ applied: { created: ['x'], removed: [] }, snapshot: next });
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(snapshot)).toBe(next);
  });

  it('updates the derived project store after an edit', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const p2 = project([clip('x', 'v1', 0, sec(1))]);
    api.apply.mockResolvedValueOnce(replyOf(p2));
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(projectStore)).toBe(p2);
  });

  it('updates the derived plan store after an edit', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const s2 = snapshotOf(selectionFixture(), { plan: { canvas: defaultCanvas(), start: 0, end: sec(9), segments: [] } });
    api.apply.mockResolvedValueOnce({ applied: { created: [], removed: [] }, snapshot: s2 });
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(plan)).toBe(s2.plan);
  });

  it('applies overlapping dispatches in call order', async () => {
    const p = selectionFixture();
    openSnapshot(snapshotOf(p));
    const d1 = deferred<ApplyReply>();
    const d2 = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(d1.promise).mockReturnValueOnce(d2.promise);

    const first = dispatch({ type: 'copy_clips', ids: ['c1'] });
    const second = dispatch({ type: 'copy_clips', ids: ['c2'] });
    await settle();
    expect(api.apply).toHaveBeenCalledTimes(1);
    expect(api.apply).toHaveBeenLastCalledWith({ type: 'copy_clips', ids: ['c1'] });

    d1.resolve(replyOf(p));
    await first;
    await settle();
    expect(api.apply).toHaveBeenCalledTimes(2);
    expect(api.apply).toHaveBeenLastCalledWith({ type: 'copy_clips', ids: ['c2'] });

    const s2 = replyOf(p);
    d2.resolve(s2);
    await second;
    expect(get(snapshot)).toBe(s2.snapshot);
  });

  it('keeps the second edit waiting even when its reply would resolve first', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const d1 = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(d1.promise);
    api.apply.mockResolvedValueOnce(replyOf(selectionFixture()));

    const first = dispatch({ type: 'copy_clips', ids: [] });
    const second = dispatch({ type: 'copy_clips', ids: [] });
    await settle();
    expect(api.apply).toHaveBeenCalledTimes(1);
    d1.resolve(replyOf(selectionFixture()));
    await Promise.all([first, second]);
    expect(api.apply).toHaveBeenCalledTimes(2);
  });

  it('prunes the selection to clips that still exist', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c1', 'c3']);
    api.apply.mockResolvedValueOnce(replyOf(project([clip('c1', 'v1', 0, sec(1))])));
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(selection)).toEqual(['c1']);
  });

  it('removes duplicate ids from the selection when pruning', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c1', 'c1', 'c2']);
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(selection)).toEqual(['c1', 'c2']);
  });

  it('empties the selection when every selected clip is gone', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c3']);
    api.apply.mockResolvedValueOnce(replyOf(project([])));
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(selection)).toEqual([]);
  });

  it('on rejection sets the toast to the error text and returns null', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.apply.mockRejectedValueOnce('clip overlaps another clip');
    const r = await dispatch({ type: 'copy_clips', ids: [] });
    expect(r).toBeNull();
    expect(get(toast)).toBe('clip overlaps another clip');
  });

  it('converts an Error rejection to its String form in the toast', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.apply.mockRejectedValueOnce(new Error('disk full'));
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(toast)).toBe('Error: disk full');
  });

  it('leaves the snapshot unchanged after a rejection', async () => {
    const s = snapshotOf(selectionFixture());
    openSnapshot(s);
    api.apply.mockRejectedValueOnce('nope');
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(snapshot)).toBe(s);
  });

  it('a rejected edit does not block the edits queued after it', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.apply.mockRejectedValueOnce('first failed');
    const good = replyOf(selectionFixture());
    api.apply.mockResolvedValueOnce(good);
    const a = dispatch({ type: 'copy_clips', ids: [] });
    const b = dispatch({ type: 'copy_clips', ids: [] });
    expect(await a).toBeNull();
    expect(await b).toBe(good);
    expect(get(busy)).toBe(false);
  });

  it('sets busy while an edit is in flight and clears it afterwards', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const d = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(d.promise);
    const pending = dispatch({ type: 'copy_clips', ids: [] });
    await settle();
    expect(get(busy)).toBe(true);
    d.resolve(replyOf(selectionFixture()));
    await pending;
    expect(get(busy)).toBe(false);
  });

  it('keeps busy true until the last of two overlapping edits finishes', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const d1 = deferred<ApplyReply>();
    const d2 = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(d1.promise).mockReturnValueOnce(d2.promise);
    const a = dispatch({ type: 'copy_clips', ids: [] });
    const b = dispatch({ type: 'copy_clips', ids: [] });
    await settle();
    d1.resolve(replyOf(selectionFixture()));
    await a;
    await settle();
    expect(get(busy)).toBe(true);
    d2.resolve(replyOf(selectionFixture()));
    await b;
    expect(get(busy)).toBe(false);
  });

  it('clears busy after a failed edit too', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.apply.mockRejectedValueOnce('x');
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(get(busy)).toBe(false);
  });
});

// ---------------------------------------------------------------------------------------------
describe('undo and redo', () => {
  it('undo does nothing when no project is open', async () => {
    await undo();
    expect(api.undo).not.toHaveBeenCalled();
  });

  it('redo does nothing when no project is open', async () => {
    await redo();
    expect(api.redo).not.toHaveBeenCalled();
  });

  it('undo replaces the snapshot with the returned one', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const s = snapshotOf(project([]));
    api.undo.mockResolvedValueOnce(s);
    await undo();
    expect(get(snapshot)).toBe(s);
  });

  it('redo replaces the snapshot with the returned one', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const s = snapshotOf(project([]));
    api.redo.mockResolvedValueOnce(s);
    await redo();
    expect(get(snapshot)).toBe(s);
  });

  it('undo prunes the selection against the restored project', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c1', 'c3']);
    api.undo.mockResolvedValueOnce(snapshotOf(project([clip('c3', 'v1', 0, sec(1))])));
    await undo();
    expect(get(selection)).toEqual(['c3']);
  });

  it('redo prunes the selection against the restored project', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c1', 'c4']);
    api.redo.mockResolvedValueOnce(snapshotOf(project([clip('c4', 'a1', 0, sec(1))])));
    await redo();
    expect(get(selection)).toEqual(['c4']);
  });

  it('undo failure sets the toast and keeps the snapshot', async () => {
    const s = snapshotOf(selectionFixture());
    openSnapshot(s);
    api.undo.mockRejectedValueOnce('nothing to undo');
    await undo();
    expect(get(toast)).toBe('nothing to undo');
    expect(get(snapshot)).toBe(s);
  });

  it('redo failure sets the toast', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.redo.mockRejectedValueOnce('nothing to redo');
    await redo();
    expect(get(toast)).toBe('nothing to redo');
  });

  it('undo waits for an earlier edit that is still in flight', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const d = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(d.promise);
    api.undo.mockResolvedValueOnce(snapshotOf(project([])));
    const edit = dispatch({ type: 'copy_clips', ids: [] });
    const u = undo();
    await settle();
    expect(api.undo).not.toHaveBeenCalled();
    d.resolve(replyOf(selectionFixture()));
    await edit;
    await u;
    expect(api.undo).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------------------------
describe('selection', () => {
  beforeEach(() => {
    openSnapshot(snapshotOf(selectionFixture()));
  });

  it('replace sets the selection and expands the group of each id', () => {
    selectClips(['c1'], 'replace');
    expect(get(selection)).toEqual(['c1', 'c2']);
  });

  it('replace does not add link partners', () => {
    selectClips(['c3'], 'replace');
    expect(get(selection)).toEqual(['c3']);
  });

  it('replace drops ids that are not in the project', () => {
    selectClips(['zzz'], 'replace');
    expect(get(selection)).toEqual([]);
  });

  it('replace discards the previous selection', () => {
    selection.set(['c3', 'c4']);
    selectClips(['c1'], 'replace');
    expect(get(selection)).toEqual(['c1', 'c2']);
  });

  it('add unions the new ids with the current selection', () => {
    selection.set(['c3']);
    selectClips(['c1'], 'add');
    expect(get(selection)).toEqual(['c3', 'c1', 'c2']);
  });

  it('add does not duplicate ids already selected', () => {
    selection.set(['c1', 'c2']);
    selectClips(['c1'], 'add');
    expect(get(selection)).toEqual(['c1', 'c2']);
  });

  it('toggle selects an unselected clip and its group', () => {
    selection.set(['c3']);
    selectClips(['c1'], 'toggle');
    expect(get(selection)).toEqual(['c3', 'c1', 'c2']);
  });

  it('toggle deselects a selected group', () => {
    selection.set(['c1', 'c2', 'c3']);
    selectClips(['c2'], 'toggle');
    expect(get(selection)).toEqual(['c3']);
  });

  it('toggle flips each member of a group independently', () => {
    selection.set(['c1']);
    selectClips(['c2'], 'toggle');
    expect(get(selection)).toEqual(['c2']);
  });

  it('toggle with an unknown id changes nothing', () => {
    selection.set(['c3']);
    selectClips(['nope'], 'toggle');
    expect(get(selection)).toEqual(['c3']);
  });

  it('selectAllClips selects every clip in project order', () => {
    selectAllClips();
    expect(get(selection)).toEqual(['c1', 'c2', 'c3', 'c4']);
  });

  it('selectAllClips skips clips on locked tracks', () => {
    const p = selectionFixture();
    openSnapshot(snapshotOf({ ...p, tracks: p.tracks.map((t) => (t.id === 'v1' ? { ...t, locked: true } : t)) }));
    selectAllClips();
    expect(get(selection)).toEqual(['c4']);
  });

  it('selectAllClips with no project selects nothing', () => {
    closeEditor();
    selectAllClips();
    expect(get(selection)).toEqual([]);
  });

  it('selectedClips returns clips in selection order, not project order', () => {
    selection.set(['c3', 'c1']);
    expect(get(selectedClips).map((c) => c.id)).toEqual(['c3', 'c1']);
  });

  it('selectedClips skips ids whose clip no longer exists', () => {
    selection.set(['c3', 'gone']);
    expect(get(selectedClips).map((c) => c.id)).toEqual(['c3']);
  });

  it('selectedClips is empty without a project', () => {
    closeEditor();
    selection.set(['c1']);
    expect(get(selectedClips)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('clipboard actions', () => {
  beforeEach(() => {
    openSnapshot(snapshotOf(selectionFixture()));
  });

  it('copySelection does nothing when nothing is selected', async () => {
    const r = await copySelection();
    expect(r).toBeNull();
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('copySelection dispatches copy_clips with the selected ids', async () => {
    selection.set(['c3']);
    await copySelection();
    expect(api.apply).toHaveBeenCalledWith({ type: 'copy_clips', ids: ['c3'] });
  });

  it('copySelection returns the reply of the copy', async () => {
    selection.set(['c3']);
    const r = await copySelection();
    expect(r).not.toBeNull();
    expect(r?.snapshot.project.id).toBe('p1');
  });

  it('pasteAtPlayhead sends a paste at the playhead in overwrite mode', async () => {
    playhead.set(sec(2));
    await pasteAtPlayhead();
    expect(api.apply).toHaveBeenCalledWith({
      type: 'paste_clips',
      clips: [],
      at: sec(2),
      track_id: null,
      mode: 'overwrite',
    });
  });

  it('pasteAtPlayhead uses the playhead value at call time', async () => {
    playhead.set(sec(1));
    playhead.set(sec(7));
    await pasteAtPlayhead();
    expect(api.apply.mock.calls[0]?.[0]).toMatchObject({ at: sec(7) });
  });

  it('pasteAtPlayhead with no project does nothing', async () => {
    closeEditor();
    const r = await pasteAtPlayhead();
    expect(r).toBeNull();
    expect(api.apply).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------------------------
describe('playhead', () => {
  it('seek moves the playhead', () => {
    seek(sec(2));
    expect(get(playhead)).toBe(sec(2));
  });

  it('seek clamps negative values to zero', () => {
    seek(-sec(1));
    expect(get(playhead)).toBe(0);
  });

  it('seek clamps values past the project limit', () => {
    seek(sec(10 * 3600));
    expect(get(playhead)).toBe(4 * 3600 * TPS);
  });

  it('seek rounds fractional ticks to whole ticks', () => {
    seek(10.4);
    expect(get(playhead)).toBe(10);
  });
});

// ---------------------------------------------------------------------------------------------
describe('zoom', () => {
  it('zoomBy doubles the zoom', () => {
    pxPerSec.set(80);
    zoomBy(2);
    expect(get(pxPerSec)).toBe(160);
  });

  it('zoomBy clamps to the maximum of 4000 px per second', () => {
    pxPerSec.set(3000);
    zoomBy(2);
    expect(get(pxPerSec)).toBe(4000);
  });

  it('zoomBy clamps to the minimum zoom', () => {
    pxPerSec.set(3);
    zoomBy(0.001);
    expect(get(pxPerSec)).toBe(MIN_PX_PER_SEC);
  });

  it('zoomBy ignores a zero factor', () => {
    pxPerSec.set(80);
    zoomBy(0);
    expect(get(pxPerSec)).toBe(80);
  });

  it('zoomBy ignores a negative factor', () => {
    pxPerSec.set(80);
    zoomBy(-2);
    expect(get(pxPerSec)).toBe(80);
  });

  it('zoomBy ignores a NaN factor', () => {
    pxPerSec.set(80);
    zoomBy(NaN);
    expect(get(pxPerSec)).toBe(80);
  });

  it('zoomBy ignores an infinite factor', () => {
    pxPerSec.set(80);
    zoomBy(Infinity);
    expect(get(pxPerSec)).toBe(80);
  });

  it('zoomBy keeps the playhead x position stable by default', () => {
    pxPerSec.set(80);
    scrollX.set(0);
    playhead.set(sec(10));
    const before = 10 * 80 - get(scrollX);
    zoomBy(2);
    const after = 10 * get(pxPerSec) - get(scrollX);
    expect(after).toBeCloseTo(before, 6);
  });

  it('zoomBy keeps an explicit anchor x position stable', () => {
    pxPerSec.set(80);
    scrollX.set(0);
    zoomBy(2, sec(10));
    expect(get(scrollX)).toBeCloseTo(800, 6);
    expect(10 * get(pxPerSec) - get(scrollX)).toBeCloseTo(800, 6);
  });

  it('zoomBy keeps the anchor stable when scrolled and zooming in', () => {
    pxPerSec.set(100);
    scrollX.set(50);
    const before = 4 * 100 - 50;
    zoomBy(2, sec(4));
    expect(4 * get(pxPerSec) - get(scrollX)).toBeCloseTo(before, 6);
  });

  it('zoomBy with the anchor at zero leaves the scroll unchanged', () => {
    pxPerSec.set(80);
    scrollX.set(30);
    zoomBy(2, 0);
    expect(get(scrollX)).toBe(30);
  });

  it('zoomBy zooming out never produces a negative scroll', () => {
    pxPerSec.set(100);
    scrollX.set(10);
    zoomBy(0.5, sec(1));
    expect(get(scrollX)).toBeGreaterThanOrEqual(0);
  });

  it('zoomToFit uses a 10 second minimum timeline on an empty project', () => {
    openSnapshot(snapshotOf(project([])));
    zoomToFit(1040);
    expect(get(pxPerSec)).toBe(100);
  });

  it('zoomToFit fits the timeline end when it is longer than 10 seconds', () => {
    openSnapshot(snapshotOf(project([clip('a', 'v1', 0, sec(60))])));
    zoomToFit(640);
    expect(get(pxPerSec)).toBe(10);
  });

  it('zoomToFit scrolls back to zero', () => {
    openSnapshot(snapshotOf(project([clip('a', 'v1', 0, sec(20))])));
    scrollX.set(500);
    zoomToFit(1000);
    expect(get(scrollX)).toBe(0);
  });

  it('zoomToFit clamps the result to the maximum', () => {
    openSnapshot(snapshotOf(project([])));
    zoomToFit(100_000);
    expect(get(pxPerSec)).toBe(4000);
  });

  it('zoomToFit clamps the result to the minimum', () => {
    openSnapshot(snapshotOf(project([])));
    zoomToFit(20);
    expect(get(pxPerSec)).toBe(MIN_PX_PER_SEC);
  });

  it('zoomToFit ignores a non-finite view width', () => {
    openSnapshot(snapshotOf(project([])));
    pxPerSec.set(77);
    zoomToFit(NaN);
    expect(get(pxPerSec)).toBe(77);
  });

  it('timelineEndTick is the end of the last clip', () => {
    expect(timelineEndTick(project([clip('a', 'v1', sec(1), sec(2)), clip('b', 'v1', 0, sec(5))]))).toBe(sec(5));
  });

  it('timelineEndTick is zero for an empty project', () => {
    expect(timelineEndTick(project([]))).toBe(0);
  });

  it('timelineEndTick is zero without a project', () => {
    expect(timelineEndTick(null)).toBe(0);
  });
});

// ---------------------------------------------------------------------------------------------
describe('autosave', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it('calls autosave after the interval', () => {
    startAutosave(30);
    vi.advanceTimersByTime(29_999);
    expect(api.autosave).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('clamps a too short interval up to 10 seconds', () => {
    startAutosave(1);
    vi.advanceTimersByTime(9_999);
    expect(api.autosave).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('clamps a too long interval down to 600 seconds', () => {
    startAutosave(10_000);
    vi.advanceTimersByTime(599_999);
    expect(api.autosave).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('uses 60 seconds when the interval is NaN', () => {
    startAutosave(NaN);
    vi.advanceTimersByTime(59_999);
    expect(api.autosave).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('uses 60 seconds when the interval is infinite', () => {
    startAutosave(Infinity);
    vi.advanceTimersByTime(60_000);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('clamps a negative interval up to 10 seconds', () => {
    startAutosave(-5);
    vi.advanceTimersByTime(9_999);
    expect(api.autosave).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('calling startAutosave again replaces the previous timer', () => {
    startAutosave(10);
    startAutosave(20);
    vi.advanceTimersByTime(10_000);
    expect(api.autosave).not.toHaveBeenCalled();
    vi.advanceTimersByTime(10_000);
    expect(api.autosave).toHaveBeenCalledTimes(1);
  });

  it('keeps firing at every interval', () => {
    startAutosave(10);
    vi.advanceTimersByTime(35_000);
    expect(api.autosave).toHaveBeenCalledTimes(3);
  });

  it('stopAutosave stops the timer', () => {
    startAutosave(10);
    stopAutosave();
    vi.advanceTimersByTime(60_000);
    expect(api.autosave).not.toHaveBeenCalled();
  });

  it('stopAutosave is safe when nothing is running', () => {
    expect(() => stopAutosave()).not.toThrow();
    expect(() => stopAutosave()).not.toThrow();
  });

  it('an autosave failure is swallowed and the next tick tries again', async () => {
    api.autosave.mockRejectedValueOnce(new Error('disk busy'));
    startAutosave(10);
    vi.advanceTimersByTime(10_000);
    await settle();
    vi.advanceTimersByTime(10_000);
    await settle();
    expect(api.autosave).toHaveBeenCalledTimes(2);
  });

  it('does not write ui_state when autosave starts', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    vi.advanceTimersByTime(2_000);
    expect(api.setUiState).not.toHaveBeenCalled();
  });

  it('saves ui_state 800 ms after a zoom change', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    pxPerSec.set(100);
    vi.advanceTimersByTime(799);
    expect(api.setUiState).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.setUiState).toHaveBeenCalledTimes(1);
    expect(api.setUiState).toHaveBeenCalledWith({ zoom_px_per_sec: 100, scroll_x: 0, playhead: 0 });
  });

  it('saves ui_state after a scroll change', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    scrollX.set(120);
    vi.advanceTimersByTime(800);
    expect(api.setUiState).toHaveBeenCalledWith({ zoom_px_per_sec: 80, scroll_x: 120, playhead: 0 });
  });

  it('saves ui_state after a playhead change', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    seek(sec(1));
    vi.advanceTimersByTime(800);
    expect(api.setUiState).toHaveBeenCalledWith({ zoom_px_per_sec: 80, scroll_x: 0, playhead: sec(1) });
  });

  it('merges a burst of changes into one ui_state save', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    seek(sec(1));
    vi.advanceTimersByTime(100);
    seek(sec(2));
    vi.advanceTimersByTime(100);
    seek(sec(3));
    vi.advanceTimersByTime(799);
    expect(api.setUiState).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.setUiState).toHaveBeenCalledTimes(1);
    expect(api.setUiState).toHaveBeenCalledWith({ zoom_px_per_sec: 80, scroll_x: 0, playhead: sec(3) });
  });

  it('stopAutosave cancels a pending ui_state save', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    seek(sec(1));
    stopAutosave();
    vi.advanceTimersByTime(5_000);
    expect(api.setUiState).not.toHaveBeenCalled();
  });

  it('no ui_state save happens when no project is open', () => {
    startAutosave(60);
    seek(sec(1));
    vi.advanceTimersByTime(2_000);
    expect(api.setUiState).not.toHaveBeenCalled();
  });

  it('a failed ui_state save is swallowed', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.setUiState.mockRejectedValueOnce(new Error('locked'));
    startAutosave(60);
    seek(sec(1));
    vi.advanceTimersByTime(800);
    await settle();
    expect(api.setUiState).toHaveBeenCalledTimes(1);
  });

  it('stopping autosave also stops ui_state watching', () => {
    openSnapshot(snapshotOf(selectionFixture()));
    startAutosave(60);
    stopAutosave();
    seek(sec(1));
    vi.advanceTimersByTime(2_000);
    expect(api.setUiState).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------------------------
describe('pure helpers', () => {
  const p = selectionFixture();

  it('pruneSelection returns [] for a null project', () => {
    expect(pruneSelection(null, ['c1'])).toEqual([]);
  });

  it('pruneSelection keeps only known ids in their order', () => {
    expect(pruneSelection(p, ['c3', 'nope', 'c1'])).toEqual(['c3', 'c1']);
  });

  it('pruneSelection removes duplicates', () => {
    expect(pruneSelection(p, ['c1', 'c1'])).toEqual(['c1']);
  });

  it('expandWithGroups with a null project only dedupes', () => {
    expect(expandWithGroups(null, ['a', 'b', 'a'])).toEqual(['a', 'b']);
  });

  it('expandWithGroups adds every member of a group', () => {
    expect(expandWithGroups(p, ['c2'])).toEqual(['c2', 'c1']);
  });

  it('expandWithGroups does not add link partners', () => {
    expect(expandWithGroups(p, ['c1'])).not.toContain('c4');
  });

  it('expandWithGroups lists each member once even when several are given', () => {
    expect(expandWithGroups(p, ['c1', 'c2', 'c1'])).toEqual(['c1', 'c2']);
  });

  it('expandWithGroups drops ids that are not in the project', () => {
    expect(expandWithGroups(p, ['zzz', 'c3'])).toEqual(['c3']);
  });

  it('expandWithGroups keeps ungrouped clips as they are', () => {
    expect(expandWithGroups(p, ['c4'])).toEqual(['c4']);
  });
});

describe('store defaults', () => {
  it('starts with no snapshot, no selection and the select tool', () => {
    expect(get(snapshot)).toBeNull();
    expect(get(selection)).toEqual([]);
    expect(get(tool)).toBe('select');
    expect(get(toast)).toBeNull();
    expect(get(busy)).toBe(false);
  });

  it('derived project and plan are null without a snapshot', () => {
    expect(get(projectStore)).toBeNull();
    expect(get(plan)).toBeNull();
  });

  it('derived plan follows the open snapshot', () => {
    const s = snapshotOf(selectionFixture());
    openSnapshot(s);
    expect(get(plan)).toBe(s.plan);
  });
});

// ---------------------------------------------------------------------------------------------
describe('dispatchWith', () => {
  it('resolves null and makes no API call when no project is open', async () => {
    const build = vi.fn(() => ({ type: 'copy_clips', ids: [] }) as Command);
    await expect(dispatchWith(build)).resolves.toBeNull();
    expect(build).not.toHaveBeenCalled();
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('builds the command from the project current when the task starts', async () => {
    const first = selectionFixture();
    openSnapshot(snapshotOf(first));
    const later = project([clip('late', 'v1', 0, sec(1))]);
    // A queued edit changes the snapshot before the second build runs.
    const gate = deferred<ApplyReply>();
    api.apply.mockImplementationOnce(() => gate.promise);
    const p1 = dispatch({ type: 'copy_clips', ids: ['c1'] });
    const seen: Project[] = [];
    const p2 = dispatchWith((p) => {
      seen.push(p);
      return { type: 'copy_clips', ids: p.clips.map((c) => c.id) };
    });
    snapshot.set(snapshotOf(later));
    gate.resolve(replyOf(first));
    await p1;
    await p2;
    expect(seen).toHaveLength(1);
    expect(seen[0]).toBe(first);
    expect(api.apply).toHaveBeenLastCalledWith({ type: 'copy_clips', ids: first.clips.map((c) => c.id) });
  });

  it('a build that returns null skips the backend call and resolves null', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    await expect(dispatchWith(() => null)).resolves.toBeNull();
    expect(api.apply).not.toHaveBeenCalled();
    expect(get(toast)).toBeNull();
  });

  it('a build that returns a command applies it and resolves with the reply', async () => {
    const p = selectionFixture();
    openSnapshot(snapshotOf(p));
    const reply = replyOf(p);
    api.apply.mockResolvedValueOnce(reply);
    await expect(dispatchWith(() => ({ type: 'copy_clips', ids: ['c2'] }))).resolves.toBe(reply);
    expect(api.apply).toHaveBeenCalledWith({ type: 'copy_clips', ids: ['c2'] });
  });

  it('a throwing build sends the error to the toast and resolves null', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    await expect(
      dispatchWith(() => {
        throw 'bad build';
      }),
    ).resolves.toBeNull();
    expect(get(toast)).toBe('bad build');
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('a backend error goes to the toast and the snapshot is not replaced', async () => {
    const p = selectionFixture();
    openSnapshot(snapshotOf(p));
    api.apply.mockRejectedValueOnce('clip is locked');
    await expect(dispatchWith(() => ({ type: 'copy_clips', ids: ['c1'] }))).resolves.toBeNull();
    expect(get(toast)).toBe('clip is locked');
    expect(get(snapshot)?.project).toBe(p);
  });

  it('prunes the selection to clips that still exist after the edit', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    selection.set(['c1', 'c3']);
    api.apply.mockResolvedValueOnce(replyOf(project([clip('c1', 'v1', 0, sec(1))])));
    await dispatchWith(() => ({ type: 'copy_clips', ids: [] }));
    expect(get(selection)).toEqual(['c1']);
  });

  it('dispatch is dispatchWith with a fixed command', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const cmd: Command = { type: 'copy_clips', ids: ['c1'] };
    await dispatch(cmd);
    expect(api.apply).toHaveBeenCalledWith(cmd);
  });

  it('edits queued through dispatchWith run in call order', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const order: string[] = [];
    api.apply.mockImplementation(async (cmd: Command) => {
      order.push(cmd.type);
      return replyOf(selectionFixture());
    });
    const a = dispatchWith(() => ({ type: 'copy_clips', ids: [] }));
    const b = dispatchWith(() => ({ type: 'split_at', time: sec(1), ids: ['c1'] }));
    await Promise.all([a, b]);
    expect(order).toEqual(['copy_clips', 'split_at']);
  });
});

// ---------------------------------------------------------------------------------------------
describe('isBusy and busy store', () => {
  it('is false when nothing is queued', () => {
    expect(isBusy()).toBe(false);
    expect(get(busy)).toBe(false);
  });

  it('is true while an edit is running and false after it settles', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const gate = deferred<ApplyReply>();
    api.apply.mockImplementationOnce(() => gate.promise);
    const p = dispatch({ type: 'copy_clips', ids: [] });
    await settle();
    expect(isBusy()).toBe(true);
    expect(get(busy)).toBe(true);
    gate.resolve(replyOf(selectionFixture()));
    await p;
    expect(isBusy()).toBe(false);
    expect(get(busy)).toBe(false);
  });

  it('stays true until the last of several queued edits finishes', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const g1 = deferred<ApplyReply>();
    const g2 = deferred<ApplyReply>();
    api.apply.mockImplementationOnce(() => g1.promise).mockImplementationOnce(() => g2.promise);
    const a = dispatch({ type: 'copy_clips', ids: [] });
    const b = dispatch({ type: 'copy_clips', ids: [] });
    await settle();
    expect(isBusy()).toBe(true);
    g1.resolve(replyOf(selectionFixture()));
    await a;
    await settle();
    expect(isBusy()).toBe(true);
    g2.resolve(replyOf(selectionFixture()));
    await b;
    expect(isBusy()).toBe(false);
  });

  it('is cleared even when the edit fails', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    api.apply.mockRejectedValueOnce('boom');
    await dispatch({ type: 'copy_clips', ids: [] });
    expect(isBusy()).toBe(false);
    expect(get(busy)).toBe(false);
  });
});

// ---------------------------------------------------------------------------------------------
describe('binDrag store', () => {
  it('starts as null', () => {
    expect(get(binDrag)).toBeNull();
  });

  it('holds the dragged media id and duration', () => {
    binDrag.set({ mediaId: 'm9', durationTicks: sec(4) });
    expect(get(binDrag)).toEqual({ mediaId: 'm9', durationTicks: sec(4) });
  });

  it('is cleared by closeEditor', () => {
    binDrag.set({ mediaId: 'm9', durationTicks: sec(4) });
    closeEditor();
    expect(get(binDrag)).toBeNull();
  });
});

// ---------------------------------------------------------------------------------------------
describe('late replies from a previous session', () => {
  /** A promise the test resolves by hand, so the reply can arrive after the session changed. */
  function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
    let resolve: (v: T) => void = () => undefined;
    const promise = new Promise<T>((r) => {
      resolve = r;
    });
    return { promise, resolve };
  }

  it('an edit reply that arrives after closeEditor does not reopen the snapshot', async () => {
    const p = selectionFixture();
    openSnapshot(snapshotOf(p));
    const late = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(late.promise);

    const pending = dispatch({ type: 'copy_clips', ids: ['c1'] });
    await vi.waitFor(() => expect(api.apply).toHaveBeenCalled());
    closeEditor();
    late.resolve(replyOf(p));
    await pending;

    expect(get(snapshot)).toBeNull();
  });

  it('an edit reply from project A does not overwrite project B opened meanwhile', async () => {
    const a = selectionFixture();
    openSnapshot(snapshotOf(a));
    const late = deferred<ApplyReply>();
    api.apply.mockReturnValueOnce(late.promise);

    const pending = dispatch({ type: 'copy_clips', ids: ['c1'] });
    await vi.waitFor(() => expect(api.apply).toHaveBeenCalled());
    const b = snapshotOf(project([clip('b1', 'v1', 0, sec(2))], { id: 'pB' }));
    openSnapshot(b);
    late.resolve(replyOf(a));
    await pending;

    expect(get(snapshot)).toBe(b);
  });

  it('an undo reply that arrives after closeEditor leaves the store closed', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const late = deferred<EditorSnapshot>();
    api.undo.mockReturnValueOnce(late.promise);

    const pending = undo();
    await vi.waitFor(() => expect(api.undo).toHaveBeenCalled());
    closeEditor();
    late.resolve(snapshotOf(project([])));
    await pending;

    expect(get(snapshot)).toBeNull();
  });

  it('a redo reply from a previous project does not overwrite the newly opened one', async () => {
    openSnapshot(snapshotOf(selectionFixture()));
    const late = deferred<EditorSnapshot>();
    api.redo.mockReturnValueOnce(late.promise);

    const pending = redo();
    await vi.waitFor(() => expect(api.redo).toHaveBeenCalled());
    const b = snapshotOf(project([], { id: 'pB' }));
    openSnapshot(b);
    late.resolve(snapshotOf(project([clip('old', 'v1', 0, sec(1))])));
    await pending;

    expect(get(snapshot)).toBe(b);
  });
});

describe('closeEditor resets loop and shuttle', () => {
  it('returns shuttle to 1x and turns loop off', () => {
    shuttleRate.set(-4);
    loopInOut.set(true);
    closeEditor();
    expect(get(shuttleRate)).toBe(1);
    expect(get(loopInOut)).toBe(false);
  });
});
