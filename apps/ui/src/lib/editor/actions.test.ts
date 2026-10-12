import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

// The Tauri API is replaced by spies; the store and the action table are real.
const api = vi.hoisted(() => ({
  apply: vi.fn(),
  undo: vi.fn(),
  redo: vi.fn(),
  saveProject: vi.fn(),
  exportStill: vi.fn(),
  setUiState: vi.fn(),
  autosave: vi.fn(),
}));

vi.mock('./api', () => ({ editorApi: api }));

import { cancelDragRequest, loopInOut, runAction, shuttleRate, sourceRequest, type ActionContext } from './actions';
import {
  closeEditor,
  dispatch,
  openSnapshot,
  playhead,
  playing,
  pxPerSec,
  scrollX,
  selection,
  snapshot,
  toast,
  tool,
} from './store';
import { ACTIONS, type ActionId } from './shortcuts';
import type { ApplyReply, Clip, EditorSnapshot, Project, Track, WorkArea } from './types';
import { defaultCanvas, defaultClipAudio, defaultExportSettings, defaultTransform } from './types';

const TPS = 254_016_000_000;
const sec = (s: number): number => Math.round(s * TPS);
/** Ticks per frame at 30 fps (exact). */
const FRAME = TPS / 30;

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

/** Video clips a (0-2 s) and b (5-7 s), audio clips ca (0-2 s, L1) and cb (5-7 s, L2). */
function baseProject(): Project {
  return project([
    clip('a', 'v1', 0, sec(2)),
    clip('b', 'v1', sec(5), sec(2)),
    clip('ca', 'a1', 0, sec(2), { kind: 'audio', link_id: 'L1' }),
    clip('cb', 'a1', sec(5), sec(2), { kind: 'audio', link_id: 'L2' }),
  ]);
}

function ctxOf(over: Partial<ActionContext> = {}): ActionContext & {
  openExport: ReturnType<typeof vi.fn>;
  showShortcuts: ReturnType<typeof vi.fn>;
} {
  return {
    openExport: vi.fn(),
    showShortcuts: vi.fn(),
    viewWidth: 1040,
    hasSource: true,
    ...over,
  } as ActionContext & { openExport: ReturnType<typeof vi.fn>; showShortcuts: ReturnType<typeof vi.fn> };
}

/** The command sent by the most recent `editor_apply` call. */
function lastCommand(): unknown {
  return api.apply.mock.calls.at(-1)?.[0];
}

/** Commands sent to the API, in order. */
function commands(): unknown[] {
  return api.apply.mock.calls.map((c) => c[0]);
}

function openBase(p: Project = baseProject()): void {
  openSnapshot(snapshotOf(p));
}

beforeEach(() => {
  vi.resetAllMocks();
  closeEditor();
  api.apply.mockImplementation(async () => replyOf(baseProject()));
  api.undo.mockImplementation(async () => snapshotOf(baseProject()));
  api.redo.mockImplementation(async () => snapshotOf(baseProject()));
  api.saveProject.mockResolvedValue(undefined);
  api.exportStill.mockResolvedValue('C:/out/still.png');
  openBase();
});

afterEach(() => {
  closeEditor();
  shuttleRate.set(1);
  sourceRequest.set(null);
});

// ---------------------------------------------------------------------------------------------
describe('playback', () => {
  it('play_pause starts playback', () => {
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playing)).toBe(true);
  });

  it('play_pause stops playback when already playing', () => {
    runAction(ACTIONS.play_pause, ctxOf());
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playing)).toBe(false);
  });

  it('play_pause resets the shuttle to normal speed when starting', () => {
    shuttleRate.set(-4);
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(shuttleRate)).toBe(1);
  });

  it('play_pause stopping keeps the shuttle as it is', () => {
    runAction(ACTIONS.play_pause, ctxOf());
    shuttleRate.set(2);
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(shuttleRate)).toBe(2);
  });

  it('shuttle_forward from paused starts forward play at normal speed', () => {
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(1);
    expect(get(playing)).toBe(true);
  });

  it('shuttle_forward while playing climbs the ladder 1 -> 2 -> 4', () => {
    runAction(ACTIONS.shuttle_forward, ctxOf());
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(2);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(4);
  });

  it('shuttle_forward stops at the top of the ladder', () => {
    playing.set(true);
    shuttleRate.set(4);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(4);
  });

  it('shuttle_forward from paused ignores the stored rate and plays at 1x', () => {
    shuttleRate.set(-4);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(1);
  });

  it('shuttle_back from normal speed goes to reverse 1x', () => {
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-1);
    expect(get(playing)).toBe(true);
  });

  it('shuttle_back walks down the ladder -1 -> -2 -> -4', () => {
    runAction(ACTIONS.shuttle_back, ctxOf());
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-2);
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-4);
  });

  it('shuttle_back stops at the bottom of the ladder', () => {
    shuttleRate.set(-4);
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-4);
  });

  it('shuttle_forward from reverse 2x goes back to normal then 2x', () => {
    playing.set(true);
    shuttleRate.set(-2);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(-1);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(1);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(2);
  });

  it('shuttle_forward walks the full ladder from -4 to 4', () => {
    playing.set(true);
    shuttleRate.set(-4);
    const seen: number[] = [];
    for (let i = 0; i < 5; i++) {
      runAction(ACTIONS.shuttle_forward, ctxOf());
      seen.push(get(shuttleRate));
    }
    expect(seen).toEqual([-2, -1, 1, 2, 4]);
  });

  it('an unknown shuttle rate counts as normal speed when stepping forward', () => {
    playing.set(true);
    shuttleRate.set(3);
    runAction(ACTIONS.shuttle_forward, ctxOf());
    expect(get(shuttleRate)).toBe(2);
  });

  it('an unknown shuttle rate counts as normal speed when stepping back', () => {
    shuttleRate.set(3);
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-1);
  });

  it('shuttle_stop pauses and resets the shuttle', () => {
    shuttleRate.set(-4);
    playing.set(true);
    runAction(ACTIONS.shuttle_stop, ctxOf());
    expect(get(playing)).toBe(false);
    expect(get(shuttleRate)).toBe(1);
  });

  it('frame_forward moves one frame from the start', () => {
    runAction(ACTIONS.frame_forward, ctxOf());
    expect(get(playhead)).toBe(FRAME);
  });

  it('frame_forward pauses playback', () => {
    playing.set(true);
    runAction(ACTIONS.frame_forward, ctxOf());
    expect(get(playing)).toBe(false);
  });

  it('frame_forward moves two frames after two presses', () => {
    runAction(ACTIONS.frame_forward, ctxOf());
    runAction(ACTIONS.frame_forward, ctxOf());
    expect(get(playhead)).toBe(2 * FRAME);
  });

  it('frame_back moves one frame back', () => {
    playhead.set(3 * FRAME);
    runAction(ACTIONS.frame_back, ctxOf());
    expect(get(playhead)).toBe(2 * FRAME);
  });

  it('frame_back at the start stays at zero', () => {
    runAction(ACTIONS.frame_back, ctxOf());
    expect(get(playhead)).toBe(0);
  });

  it('frame steps do nothing without a project', () => {
    closeEditor();
    playhead.set(5 * FRAME);
    runAction(ACTIONS.frame_forward, ctxOf());
    runAction(ACTIONS.frame_back, ctxOf());
    expect(get(playhead)).toBe(5 * FRAME);
  });

  it('second_forward moves one second', () => {
    runAction(ACTIONS.second_forward, ctxOf());
    expect(get(playhead)).toBe(sec(1));
  });

  it('second_back moves one second back', () => {
    playhead.set(sec(3));
    runAction(ACTIONS.second_back, ctxOf());
    expect(get(playhead)).toBe(sec(2));
  });

  it('second_back clamps at zero', () => {
    playhead.set(sec(0.5));
    runAction(ACTIONS.second_back, ctxOf());
    expect(get(playhead)).toBe(0);
  });

  it('go_start moves the playhead to zero', () => {
    playhead.set(sec(4));
    runAction(ACTIONS.go_start, ctxOf());
    expect(get(playhead)).toBe(0);
  });

  it('go_end moves to the end of the timeline', () => {
    runAction(ACTIONS.go_end, ctxOf());
    expect(get(playhead)).toBe(sec(7));
  });

  it('go_end does nothing without a project', () => {
    closeEditor();
    playhead.set(sec(1));
    runAction(ACTIONS.go_end, ctxOf());
    expect(get(playhead)).toBe(sec(1));
  });

  it('next_edit jumps to the next edit point', () => {
    playhead.set(sec(3));
    runAction(ACTIONS.next_edit, ctxOf());
    expect(get(playhead)).toBe(sec(5));
  });

  it('prev_edit jumps to the previous edit point', () => {
    playhead.set(sec(3));
    runAction(ACTIONS.prev_edit, ctxOf());
    expect(get(playhead)).toBe(sec(2));
  });

  it('next_edit at the last edit point leaves the playhead alone', () => {
    playhead.set(sec(7));
    runAction(ACTIONS.next_edit, ctxOf());
    expect(get(playhead)).toBe(sec(7));
  });

  it('prev_edit at zero leaves the playhead alone', () => {
    runAction(ACTIONS.prev_edit, ctxOf());
    expect(get(playhead)).toBe(0);
  });

  it('edit points ignore disabled clips', () => {
    openBase(
      project([
        clip('a', 'v1', 0, sec(2)),
        clip('b', 'v1', sec(5), sec(2), { enabled: false }),
      ]),
    );
    playhead.set(sec(2));
    runAction(ACTIONS.next_edit, ctxOf());
    expect(get(playhead)).toBe(sec(2));
  });

  it('next_edit from the start stops at the first clip end', () => {
    playhead.set(0);
    runAction(ACTIONS.next_edit, ctxOf());
    expect(get(playhead)).toBe(sec(2));
  });
});

// ---------------------------------------------------------------------------------------------
describe('tools and view', () => {
  it('tool_select sets the select tool', () => {
    tool.set('hand');
    runAction(ACTIONS.tool_select, ctxOf());
    expect(get(tool)).toBe('select');
  });

  it('tool_razor sets the razor tool', () => {
    runAction(ACTIONS.tool_razor, ctxOf());
    expect(get(tool)).toBe('razor');
  });

  it('tool_hand sets the hand tool', () => {
    runAction(ACTIONS.tool_hand, ctxOf());
    expect(get(tool)).toBe('hand');
  });

  it('tool_zoom sets the zoom tool', () => {
    runAction(ACTIONS.tool_zoom, ctxOf());
    expect(get(tool)).toBe('zoom');
  });

  it('zoom_in multiplies the zoom by 1.25', () => {
    pxPerSec.set(80);
    runAction(ACTIONS.zoom_in, ctxOf());
    expect(get(pxPerSec)).toBeCloseTo(100, 9);
  });

  it('zoom_out multiplies the zoom by 0.8', () => {
    pxPerSec.set(80);
    runAction(ACTIONS.zoom_out, ctxOf());
    expect(get(pxPerSec)).toBeCloseTo(64, 9);
  });

  it('zoom_fit fits the timeline into the view width from the context', () => {
    runAction(ACTIONS.zoom_fit, ctxOf({ viewWidth: 1040 }));
    // 7 s timeline is shorter than the 10 s minimum: (1040 - 40) / 10.
    expect(get(pxPerSec)).toBe(100);
  });

  it('zoom_fit uses a different width when the context gives one', () => {
    runAction(ACTIONS.zoom_fit, ctxOf({ viewWidth: 640 }));
    expect(get(pxPerSec)).toBe(60);
    expect(get(scrollX)).toBe(0);
  });

  it('select_all selects every clip', () => {
    runAction(ACTIONS.select_all, ctxOf());
    expect(get(selection)).toEqual(['a', 'b', 'ca', 'cb']);
  });

  it('escape clears the selection', () => {
    selection.set(['a', 'b']);
    runAction(ACTIONS.escape, ctxOf());
    expect(get(selection)).toEqual([]);
  });

  it('escape bumps the drag-cancel counter so an active drag is cancelled', () => {
    const before = get(cancelDragRequest);
    runAction(ACTIONS.escape, ctxOf());
    expect(get(cancelDragRequest)).toBe(before + 1);
  });

  it('show_shortcuts calls the context callback', () => {
    const ctx = ctxOf();
    runAction(ACTIONS.show_shortcuts, ctx);
    expect(ctx.showShortcuts).toHaveBeenCalledTimes(1);
  });

  it('export calls the context callback that opens the export dialog', () => {
    const ctx = ctxOf();
    runAction(ACTIONS.export, ctx);
    expect(ctx.openExport).toHaveBeenCalledTimes(1);
  });

  it('insert_from_source requests an insert from the source monitor', () => {
    runAction(ACTIONS.insert_from_source, ctxOf());
    expect(get(sourceRequest)).toEqual({ action: 'insert' });
  });

  it('insert_from_source without a source clip shows a toast and requests nothing', () => {
    runAction(ACTIONS.insert_from_source, ctxOf({ hasSource: false }));
    expect(get(sourceRequest)).toBeNull();
    expect(get(toast)).toBe('Open a clip in the source monitor first');
  });

  it('overwrite_from_source without a source clip shows a toast and requests nothing', () => {
    runAction(ACTIONS.overwrite_from_source, ctxOf({ hasSource: false }));
    expect(get(sourceRequest)).toBeNull();
    expect(get(toast)).toBe('Open a clip in the source monitor first');
  });

  it('overwrite_from_source requests an overwrite from the source monitor', () => {
    runAction(ACTIONS.overwrite_from_source, ctxOf());
    expect(get(sourceRequest)).toEqual({ action: 'overwrite' });
  });

  it('an unknown action id does nothing and does not throw', () => {
    expect(() => runAction('no_such_action' as ActionId, ctxOf())).not.toThrow();
    expect(api.apply).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------------------------
describe('work area and markers', () => {
  it('set_in without an out point uses the timeline end', async () => {
    playhead.set(sec(3));
    await runAction(ACTIONS.set_in, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_work_area', area: { start: sec(3), end: sec(7) } });
  });

  it('set_in keeps an existing out point that is after the playhead', async () => {
    openBase(project(baseProject().clips, { work_area: { start: sec(1), end: sec(6) } }));
    playhead.set(sec(3));
    await runAction(ACTIONS.set_in, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_work_area', area: { start: sec(3), end: sec(6) } });
  });

  it('set_in past the timeline end shows a toast and sends nothing', async () => {
    playhead.set(sec(8));
    await runAction(ACTIONS.set_in, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
    expect(get(toast)).toBe('The in point must come before the out point');
  });

  it('set_in does nothing without a project', async () => {
    closeEditor();
    await runAction(ACTIONS.set_in, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('set_out without an in point starts at zero', async () => {
    playhead.set(sec(4));
    await runAction(ACTIONS.set_out, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_work_area', area: { start: 0, end: sec(4) } });
  });

  it('set_out keeps an existing in point before the playhead', async () => {
    openBase(project(baseProject().clips, { work_area: { start: sec(1), end: sec(6) } }));
    playhead.set(sec(4));
    await runAction(ACTIONS.set_out, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_work_area', area: { start: sec(1), end: sec(4) } });
  });

  it('set_out with the playhead at zero shows a toast and sends nothing', async () => {
    await runAction(ACTIONS.set_out, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
    expect(get(toast)).toBe('The out point must come after the in point');
  });

  it('set_out before an existing in point replaces the in point with zero', async () => {
    openBase(project(baseProject().clips, { work_area: { start: sec(6), end: sec(7) } }));
    playhead.set(sec(4));
    await runAction(ACTIONS.set_out, ctxOf());
    const cmd = lastCommand() as { area: WorkArea };
    expect(cmd.area.start).toBeLessThan(cmd.area.end);
    expect(cmd.area).toEqual({ start: 0, end: sec(4) });
  });

  it('set_out does nothing without a project', async () => {
    closeEditor();
    await runAction(ACTIONS.set_out, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('clear_in_out sends a null work area', async () => {
    await runAction(ACTIONS.clear_in_out, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_work_area', area: null });
  });

  it('add_marker labels the marker with the next number', async () => {
    openBase(project(baseProject().clips, { markers: [{ id: 'm1', time: 0, label: 'Marker 1', color: null }] }));
    playhead.set(sec(2));
    await runAction(ACTIONS.add_marker, ctxOf());
    expect(lastCommand()).toEqual({ type: 'add_marker', time: sec(2), label: 'Marker 2', color: null });
  });

  it('add_marker on an empty project uses Marker 1', async () => {
    await runAction(ACTIONS.add_marker, ctxOf());
    expect(lastCommand()).toMatchObject({ type: 'add_marker', label: 'Marker 1' });
  });

  it('toggle_snap flips the snap mode and keeps magnetic', async () => {
    await runAction(ACTIONS.toggle_snap, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_modes', magnetic: true, snap: false });
  });

  it('toggle_snap turns snapping back on when it is off', async () => {
    openBase(project(baseProject().clips, { modes: { magnetic: false, snap: false } }));
    await runAction(ACTIONS.toggle_snap, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_modes', magnetic: false, snap: true });
  });

  it('toggle_snap does nothing without a project', async () => {
    closeEditor();
    await runAction(ACTIONS.toggle_snap, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------------------------
describe('editing commands', () => {
  it('delete ripples when magnetic mode is on (the default)', async () => {
    selection.set(['a']);
    await runAction(ACTIONS.delete, ctxOf());
    expect(lastCommand()).toEqual({ type: 'delete_clips', ids: ['a'], ripple: true });
  });

  it('delete leaves a gap when magnetic mode is off', async () => {
    openBase(project(baseProject().clips, { modes: { magnetic: false, snap: true } }));
    selection.set(['a']);
    await runAction(ACTIONS.delete, ctxOf());
    expect(lastCommand()).toEqual({ type: 'delete_clips', ids: ['a'], ripple: false });
  });

  it('delete with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.delete, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('ripple_delete sends delete_clips with ripple', async () => {
    selection.set(['a', 'b']);
    await runAction(ACTIONS.ripple_delete, ctxOf());
    expect(lastCommand()).toEqual({ type: 'delete_clips', ids: ['a', 'b'], ripple: true });
  });

  it('ripple_delete with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.ripple_delete, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('split sends split_at at the playhead for the selected ids', async () => {
    playhead.set(sec(1));
    selection.set(['a']);
    await runAction(ACTIONS.split, ctxOf());
    expect(lastCommand()).toEqual({ type: 'split_at', time: sec(1), ids: ['a'] });
  });

  it('split with an empty selection still sends split_at with no ids', async () => {
    playhead.set(sec(1));
    await runAction(ACTIONS.split, ctxOf());
    expect(lastCommand()).toEqual({ type: 'split_at', time: sec(1), ids: [] });
  });

  it('group sends group_clips for the selection', async () => {
    selection.set(['a', 'b']);
    await runAction(ACTIONS.group, ctxOf());
    expect(lastCommand()).toEqual({ type: 'group_clips', ids: ['a', 'b'] });
  });

  it('group with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.group, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('ungroup sends ungroup for the selection', async () => {
    selection.set(['a']);
    await runAction(ACTIONS.ungroup, ctxOf());
    expect(lastCommand()).toEqual({ type: 'ungroup', ids: ['a'] });
  });

  it('ungroup with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.ungroup, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('link_toggle links clips that are not linked', async () => {
    selection.set(['a', 'b']);
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(lastCommand()).toEqual({ type: 'link_clips', ids: ['a', 'b'] });
  });

  it('link_toggle unlinks when every selected clip shares one link id', async () => {
    selection.set(['ca']);
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(lastCommand()).toEqual({ type: 'unlink_clips', ids: ['ca'] });
  });

  it('link_toggle links when the selected clips have different link ids', async () => {
    selection.set(['ca', 'cb']);
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(lastCommand()).toEqual({ type: 'link_clips', ids: ['ca', 'cb'] });
  });

  it('link_toggle links when only some selected clips are linked', async () => {
    selection.set(['ca', 'a']);
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(lastCommand()).toEqual({ type: 'link_clips', ids: ['ca', 'a'] });
  });

  it('link_toggle unlinks a set of clips that all share the same link id', async () => {
    openBase(
      project([
        clip('x1', 'v1', 0, sec(1), { link_id: 'Z' }),
        clip('x2', 'a1', 0, sec(1), { link_id: 'Z', kind: 'audio' }),
      ]),
    );
    selection.set(['x1', 'x2']);
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(lastCommand()).toEqual({ type: 'unlink_clips', ids: ['x1', 'x2'] });
  });

  it('link_toggle with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('link_toggle ignores selected ids that no longer exist', async () => {
    selection.set(['ghost']);
    await runAction(ACTIONS.link_toggle, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('copy sends copy_clips for the selection', async () => {
    selection.set(['a']);
    await runAction(ACTIONS.copy, ctxOf());
    expect(lastCommand()).toEqual({ type: 'copy_clips', ids: ['a'] });
  });

  it('copy with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.copy, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('cut copies the selection and then deletes it', async () => {
    selection.set(['a', 'b']);
    await runAction(ACTIONS.cut, ctxOf());
    expect(commands()).toEqual([
      { type: 'copy_clips', ids: ['a', 'b'] },
      { type: 'delete_clips', ids: ['a', 'b'], ripple: false },
    ]);
  });

  it('cut does not delete when the copy fails', async () => {
    selection.set(['a']);
    api.apply.mockRejectedValueOnce('clipboard busy');
    await runAction(ACTIONS.cut, ctxOf());
    expect(api.apply).toHaveBeenCalledTimes(1);
    expect(get(toast)).toBe('clipboard busy');
  });

  it('cut with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.cut, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('paste sends paste_clips at the playhead', async () => {
    playhead.set(sec(6));
    await runAction(ACTIONS.paste, ctxOf());
    expect(lastCommand()).toEqual({
      type: 'paste_clips',
      clips: [],
      at: sec(6),
      track_id: null,
      mode: 'overwrite',
    });
  });

  it('duplicate offsets by the span of the selection', async () => {
    selection.set(['a', 'b']);
    await runAction(ACTIONS.duplicate, ctxOf());
    // Earliest start 0, latest end 7 s.
    expect(lastCommand()).toEqual({ type: 'duplicate_clips', ids: ['a', 'b'], offset: sec(7) });
  });

  it('duplicate of one clip offsets by its own length from its start', async () => {
    selection.set(['b']);
    await runAction(ACTIONS.duplicate, ctxOf());
    expect(lastCommand()).toEqual({ type: 'duplicate_clips', ids: ['b'], offset: sec(2) });
  });

  it('duplicate with an empty selection sends nothing', async () => {
    await runAction(ACTIONS.duplicate, ctxOf());
    expect(api.apply).not.toHaveBeenCalled();
  });

  it('undo calls the store undo', async () => {
    await runAction(ACTIONS.undo, ctxOf());
    expect(api.undo).toHaveBeenCalledTimes(1);
  });

  it('redo calls the store redo', async () => {
    await runAction(ACTIONS.redo, ctxOf());
    expect(api.redo).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------------------------
describe('save and still export', () => {
  it('save writes the project and shows Saved', async () => {
    openSnapshot(snapshotOf(baseProject(), { dirty: true }));
    await runAction(ACTIONS.save, ctxOf());
    expect(api.saveProject).toHaveBeenCalledTimes(1);
    expect(get(toast)).toBe('Saved');
    expect(get(snapshot)?.dirty).toBe(false);
  });

  it('save failure shows the error and keeps the dirty flag', async () => {
    openSnapshot(snapshotOf(baseProject(), { dirty: true }));
    api.saveProject.mockRejectedValueOnce(new Error('disk full'));
    await runAction(ACTIONS.save, ctxOf());
    expect(get(toast)).toBe('Error: disk full');
    expect(get(snapshot)?.dirty).toBe(true);
  });

  it('save does nothing without a project', async () => {
    closeEditor();
    await runAction(ACTIONS.save, ctxOf());
    expect(api.saveProject).not.toHaveBeenCalled();
  });

  it('export_still exports a PNG at the playhead and reports the path', async () => {
    playhead.set(sec(2));
    await runAction(ACTIONS.export_still, ctxOf());
    expect(api.exportStill).toHaveBeenCalledWith(sec(2), 'png', 90);
    expect(get(toast)).toBe('Saved still to C:/out/still.png');
  });

  it('export_still failure shows the error text', async () => {
    api.exportStill.mockRejectedValueOnce('no frame at this time');
    await runAction(ACTIONS.export_still, ctxOf());
    expect(get(toast)).toBe('no frame at this time');
  });
});

// ---------------------------------------------------------------------------------------------
describe('every action id', () => {
  const ids = Object.values(ACTIONS) as ActionId[];

  it('the action table has the expected number of ids', () => {
    expect(ids.length).toBe(44);
  });

  it.each(ids)('%s runs without throwing on an open project', async (id) => {
    selection.set(['a']);
    await expect(Promise.resolve(runAction(id, ctxOf()))).resolves.toBeUndefined();
  });

  it.each(ids)('%s runs without throwing with no project open', async (id) => {
    closeEditor();
    await expect(Promise.resolve(runAction(id, ctxOf()))).resolves.toBeUndefined();
  });
});

// Dispatch is re-exported through the store; checking it here guards the import path used by the
// actions above.
describe('store dispatch used by actions', () => {
  it('dispatch returns the reply of a successful edit', async () => {
    const r = await dispatch({ type: 'copy_clips', ids: [] });
    expect(r?.snapshot.project.id).toBe('p1');
  });
});

// ---------------------------------------------------------------------------------------------
/** Drains pending promise callbacks so a queued edit has started (no timers involved). */
async function settleQueue(): Promise<void> {
  for (let i = 0; i < 20; i++) await Promise.resolve();
}

function deferredReply(): { promise: Promise<ApplyReply>; resolve: (r: ApplyReply) => void } {
  let resolve!: (r: ApplyReply) => void;
  const promise = new Promise<ApplyReply>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe('play, shuttle and loop cueing', () => {
  beforeEach(() => {
    loopInOut.set(false);
  });

  it('play_pause resets a non-normal shuttle rate when playback starts', () => {
    shuttleRate.set(4);
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playing)).toBe(true);
    expect(get(shuttleRate)).toBe(1);
  });

  it('play_pause leaves the shuttle rate alone when it pauses', () => {
    playing.set(true);
    shuttleRate.set(2);
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playing)).toBe(false);
    expect(get(shuttleRate)).toBe(2);
  });

  it('play_pause with loop in/out jumps a playhead before the work area to its in point', () => {
    openBase(
      project(baseProject().clips, {
        work_area: { start: sec(2), end: sec(6) } as WorkArea,
      }),
    );
    loopInOut.set(true);
    playhead.set(0);
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playhead)).toBe(sec(2));
    expect(get(playing)).toBe(true);
  });

  it('play_pause with loop in/out keeps a playhead that is already inside the work area', () => {
    openBase(project(baseProject().clips, { work_area: { start: sec(2), end: sec(6) } as WorkArea }));
    loopInOut.set(true);
    playhead.set(sec(3));
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playhead)).toBe(sec(3));
  });

  it('play_pause with loop in/out jumps a playhead at or after the work area out point', () => {
    openBase(project(baseProject().clips, { work_area: { start: sec(2), end: sec(6) } as WorkArea }));
    loopInOut.set(true);
    playhead.set(sec(6));
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playhead)).toBe(sec(2));
  });

  it('play_pause without loop in/out does not move a playhead outside the work area', () => {
    openBase(project(baseProject().clips, { work_area: { start: sec(2), end: sec(6) } as WorkArea }));
    loopInOut.set(false);
    playhead.set(0);
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playhead)).toBe(0);
  });

  it('play_pause restarts from zero when the playhead is at or after the timeline end', () => {
    // baseProject ends at 7 s.
    playhead.set(sec(7));
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playhead)).toBe(0);
    expect(get(playing)).toBe(true);
  });

  it('play_pause does not seek when the playhead is inside the timeline', () => {
    playhead.set(sec(1));
    runAction(ACTIONS.play_pause, ctxOf());
    expect(get(playhead)).toBe(sec(1));
  });

  it('shuttle_back from paused plays in reverse one notch below normal', () => {
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-1);
    expect(get(playing)).toBe(true);
  });

  it('shuttle_back at the bottom of the ladder stays at -4', () => {
    shuttleRate.set(-4);
    runAction(ACTIONS.shuttle_back, ctxOf());
    expect(get(shuttleRate)).toBe(-4);
  });
});

describe('loop in/out store', () => {
  it('loopInOut can be switched on and off', () => {
    loopInOut.set(true);
    expect(get(loopInOut)).toBe(true);
    loopInOut.set(false);
    expect(get(loopInOut)).toBe(false);
  });
});

describe('cancel drag and escape', () => {
  it('escape bumps the cancel request counter once per press', () => {
    const before = get(cancelDragRequest);
    runAction(ACTIONS.escape, ctxOf());
    runAction(ACTIONS.escape, ctxOf());
    expect(get(cancelDragRequest)).toBe(before + 2);
  });

  it('escape clears the selection after bumping the counter', () => {
    selection.set(['a', 'b']);
    runAction(ACTIONS.escape, ctxOf());
    expect(get(selection)).toEqual([]);
  });

  it('escape without a project still bumps the counter and clears the selection', () => {
    closeEditor();
    selection.set(['a']);
    const before = get(cancelDragRequest);
    runAction(ACTIONS.escape, ctxOf());
    expect(get(cancelDragRequest)).toBe(before + 1);
    expect(get(selection)).toEqual([]);
  });
});

describe('insert and overwrite from source', () => {
  it('insert without a source shows a toast and sets no source request', () => {
    runAction(ACTIONS.insert_from_source, ctxOf({ hasSource: false }));
    expect(get(toast)).toBe('Open a clip in the source monitor first');
    expect(get(sourceRequest)).toBeNull();
  });

  it('overwrite without a source shows the same toast and sets no source request', () => {
    runAction(ACTIONS.overwrite_from_source, ctxOf({ hasSource: false }));
    expect(get(toast)).toBe('Open a clip in the source monitor first');
    expect(get(sourceRequest)).toBeNull();
  });

  it('insert with a source sets an insert request and no toast', () => {
    runAction(ACTIONS.insert_from_source, ctxOf({ hasSource: true }));
    expect(get(sourceRequest)).toEqual({ action: 'insert' });
    expect(get(toast)).toBeNull();
  });

  it('overwrite with a source sets an overwrite request', () => {
    runAction(ACTIONS.overwrite_from_source, ctxOf({ hasSource: true }));
    expect(get(sourceRequest)).toEqual({ action: 'overwrite' });
  });

  it('does not call the backend for insert or overwrite', async () => {
    runAction(ACTIONS.insert_from_source, ctxOf({ hasSource: true }));
    runAction(ACTIONS.overwrite_from_source, ctxOf({ hasSource: true }));
    await settleQueue();
    expect(api.apply).not.toHaveBeenCalled();
  });
});

describe('busy guards on split and duplicate', () => {
  it('split does nothing while an edit is still queued', async () => {
    const gate = deferredReply();
    api.apply.mockImplementationOnce(() => gate.promise);
    const pending = dispatch({ type: 'copy_clips', ids: [] });
    await settleQueue();
    selection.set(['a']);
    await runAction(ACTIONS.split, ctxOf());
    expect(api.apply).toHaveBeenCalledTimes(1);
    gate.resolve(replyOf(baseProject()));
    await pending;
  });

  it('split runs again once the queue is empty', async () => {
    selection.set(['a']);
    playhead.set(sec(1));
    await runAction(ACTIONS.split, ctxOf());
    expect(lastCommand()).toEqual({ type: 'split_at', time: sec(1), ids: ['a'] });
  });

  it('duplicate does nothing while an edit is still queued', async () => {
    const gate = deferredReply();
    api.apply.mockImplementationOnce(() => gate.promise);
    const pending = dispatch({ type: 'copy_clips', ids: [] });
    await settleQueue();
    selection.set(['a']);
    await runAction(ACTIONS.duplicate, ctxOf());
    expect(api.apply).toHaveBeenCalledTimes(1);
    gate.resolve(replyOf(baseProject()));
    await pending;
  });
});

describe('delete and magnetic mode', () => {
  it('ripple_delete ripples even when magnetic mode is off', async () => {
    openBase(project(baseProject().clips, { modes: { magnetic: false, snap: true } }));
    selection.set(['a']);
    await runAction(ACTIONS.ripple_delete, ctxOf());
    expect(lastCommand()).toEqual({ type: 'delete_clips', ids: ['a'], ripple: true });
  });

  it('toggle_snap keeps magnetic off when it was off', async () => {
    openBase(project(baseProject().clips, { modes: { magnetic: false, snap: true } }));
    await runAction(ACTIONS.toggle_snap, ctxOf());
    expect(lastCommand()).toEqual({ type: 'set_modes', magnetic: false, snap: false });
  });
});

describe('still export action', () => {
  it('export_still exports the frame at the playhead as a png at quality 90', async () => {
    playhead.set(sec(3));
    await runAction(ACTIONS.export_still, ctxOf());
    expect(api.exportStill).toHaveBeenCalledWith(sec(3), 'png', 90);
    expect(get(toast)).toBe('Saved still to C:/out/still.png');
  });

  it('export_still reports a backend failure in the toast', async () => {
    api.exportStill.mockRejectedValueOnce('no frame');
    await runAction(ACTIONS.export_still, ctxOf());
    expect(get(toast)).toBe('no frame');
  });
});

describe('view context', () => {
  it('show_shortcuts calls the context callback', () => {
    const ctx = ctxOf();
    runAction(ACTIONS.show_shortcuts, ctx);
    expect(ctx.showShortcuts).toHaveBeenCalledTimes(1);
  });
});
