// Editor actions: maps every `ActionId` from shortcuts.ts to a store call or an edit command.
//
// Keyboard shortcuts, toolbar buttons and menus all come through `runAction`, so each action has
// one implementation. Edits go through `dispatch` (errors already reach the toast there).

import { get, writable } from 'svelte/store';
import { editorApi } from './api';
import { ACTIONS, type ActionId } from './shortcuts';
import {
	copySelection,
	dispatch,
	dispatchWith,
	isBusy,
	pasteAtPlayhead,
	playhead,
	playing,
	redo,
	seek,
	selectAllClips,
	selectedClips,
	selection,
	snapshot,
	timelineEndTick,
	toast,
	tool,
	undo,
	zoomBy,
	zoomToFit,
} from './store';
import { clipEnd, frameDurationTicks, secToTick, snapToFrame } from './ticks';
import type { Clip, Project, Tick, WorkArea } from './types';

/** What `runAction` needs from the view that the store does not own. */
export interface ActionContext {
	/** Opens the export dialog. */
	openExport: () => void;
	/** Opens the keyboard shortcuts overlay. */
	showShortcuts: () => void;
	/** Width of the timeline in px, used by zoom to fit. */
	viewWidth: number;
	/** True when the source monitor has a clip loaded (needed for insert and overwrite). */
	hasSource: boolean;
}

/** Shuttle speed of J/L (negative = reverse). Read by the program monitor. */
export const shuttleRate = writable<number>(1);

/** When on, play starts at the work area in point and stops are kept inside the work area. */
export const loopInOut = writable<boolean>(false);

/**
 * Cancels the drag in progress, if any, and returns true when one was cancelled. The timeline
 * registers it while mounted (see `setDragCanceller`).
 */
export type DragCanceller = () => boolean;

/** The registered drag canceller, or null when no timeline is mounted. */
let dragCanceller: DragCanceller | null = null;

/** Registers (or, with null, clears) the canceller that Escape calls first. */
export function setDragCanceller(fn: DragCanceller | null): void {
	dragCanceller = fn;
}

/** A request to insert or overwrite the source monitor's range (handled by SourceMonitor). */
export interface SourceRequest {
	action: 'insert' | 'overwrite';
}

/** Set by the `,` and `.` actions. SourceMonitor reads it and performs the edit. */
export const sourceRequest = writable<SourceRequest | null>(null);

/** Shuttle ladder: each J or L tap moves one step along it. Rate 1 is normal forward play. */
const SHUTTLE_LADDER: readonly number[] = [-4, -2, -1, 1, 2, 4];
/** Index of rate 1 in `SHUTTLE_LADDER`. */
const SHUTTLE_NORMAL_INDEX = 3;

/** Steps the shuttle one notch in the given direction. Unknown rates count as normal speed. */
function stepShuttle(current: number, dir: -1 | 1): number {
	const found = SHUTTLE_LADDER.indexOf(current);
	const idx = found === -1 ? SHUTTLE_NORMAL_INDEX : found;
	const next = Math.min(SHUTTLE_LADDER.length - 1, Math.max(0, idx + dir));
	return SHUTTLE_LADDER[next] ?? 1;
}

/** Every edit point of the project: 0, each enabled clip's start and end. Sorted, unique. */
function editPoints(project: Project): Tick[] {
	const set = new Set<Tick>([0]);
	for (const c of project.clips) {
		if (!c.enabled) continue;
		set.add(c.start);
		set.add(clipEnd(c));
	}
	return Array.from(set).sort((a, b) => a - b);
}

/**
 * Prepares the playhead before playback starts. With loop in/out on and a work area, a playhead
 * outside the work area jumps to its in point. At or after the timeline end, playback restarts
 * from 0.
 */
function cuePlayStart(p: Project | null): void {
	if (p === null) return;
	const now = get(playhead);
	const area = p.work_area;
	if (get(loopInOut) && area !== null && (now < area.start || now >= area.end)) {
		seek(area.start);
		return;
	}
	const end = timelineEndTick(p);
	if (end > 0 && now >= end) seek(0);
}

/** Pauses playback and resets the shuttle to normal speed. */
function stopPlayback(): void {
	playing.set(false);
	shuttleRate.set(1);
}

/** Steps the playhead by one frame. Pauses playback. */
function stepFrame(dir: -1 | 1): void {
	const p = get(snapshot)?.project ?? null;
	if (p === null) return;
	const rate = p.canvas.fps;
	const d = frameDurationTicks(rate);
	if (d <= 0) return;
	const cur = snapToFrame(get(playhead), rate);
	const now = get(playhead);
	let target: Tick;
	if (dir > 0) target = cur > now ? cur : cur + d;
	else target = cur < now ? cur : cur - d;
	stopPlayback();
	seek(target);
}

/** Jumps the playhead to the previous (dir -1) or next (dir 1) edit point. */
function jumpEdit(dir: -1 | 1): void {
	const p = get(snapshot)?.project ?? null;
	if (p === null) return;
	const now = get(playhead);
	const points = editPoints(p);
	const target =
		dir > 0 ? points.find((t) => t > now) : [...points].reverse().find((t) => t < now);
	if (target !== undefined) seek(target);
}

/** The work area after setting the in point at the playhead (keeps in < out). */
function workAreaWithIn(p: Project): WorkArea | null | 'invalid' {
	const now = get(playhead);
	const area = p.work_area;
	let end = area?.end ?? timelineEndTick(p);
	if (end <= now) end = timelineEndTick(p);
	if (end <= now) return 'invalid';
	return { start: now, end };
}

/** The work area after setting the out point at the playhead (keeps in < out). */
function workAreaWithOut(p: Project): WorkArea | 'invalid' {
	const now = get(playhead);
	let start = p.work_area?.start ?? 0;
	if (start >= now) start = 0;
	if (start >= now) return 'invalid';
	return { start, end: now };
}

/** Sets the work area. Used by set in / set out. */
async function setWorkArea(area: WorkArea | null): Promise<void> {
	await dispatch({ type: 'set_work_area', area });
}

/** Saves the project, then marks the snapshot clean. Returns true on success (errors go to toast). */
export async function saveNow(): Promise<boolean> {
	if (get(snapshot) === null) return false;
	try {
		await editorApi.saveProject();
		snapshot.update((s) => (s === null ? s : { ...s, dirty: false }));
		return true;
	} catch (e) {
		toast.set(String(e));
		return false;
	}
}

/** Sets the duplicate offset: the span from the earliest selected start to the latest end. */
function duplicateOffset(clips: readonly Clip[]): Tick {
	let minStart = Number.POSITIVE_INFINITY;
	let maxEnd = Number.NEGATIVE_INFINITY;
	for (const c of clips) {
		if (c.start < minStart) minStart = c.start;
		const e = clipEnd(c);
		if (e > maxEnd) maxEnd = e;
	}
	if (!Number.isFinite(minStart) || !Number.isFinite(maxEnd)) return 0;
	return Math.max(0, maxEnd - minStart);
}

/** Runs one editor action. Synchronous actions return void; asynchronous ones return a promise. */
export function runAction(id: ActionId, ctx: ActionContext): void | Promise<void> {
	const project = get(snapshot)?.project ?? null;
	const ids = get(selection);

	switch (id) {
		case ACTIONS.play_pause: {
			const next = !get(playing);
			if (next) {
				shuttleRate.set(1);
				cuePlayStart(project);
			}
			playing.set(next);
			return;
		}
		case ACTIONS.shuttle_back: {
			const r = stepShuttle(get(shuttleRate), -1);
			shuttleRate.set(r);
			playing.set(true);
			return;
		}
		case ACTIONS.shuttle_forward: {
			// From a paused state, L starts forward play at normal speed (not one notch up).
			const r = get(playing) ? stepShuttle(get(shuttleRate), 1) : 1;
			shuttleRate.set(r);
			cuePlayStart(project);
			playing.set(true);
			return;
		}
		case ACTIONS.shuttle_stop:
			stopPlayback();
			return;
		case ACTIONS.frame_back:
			stepFrame(-1);
			return;
		case ACTIONS.frame_forward:
			stepFrame(1);
			return;
		case ACTIONS.second_back:
			seek(get(playhead) - secToTick(1));
			return;
		case ACTIONS.second_forward:
			seek(get(playhead) + secToTick(1));
			return;
		case ACTIONS.prev_edit:
			jumpEdit(-1);
			return;
		case ACTIONS.next_edit:
			jumpEdit(1);
			return;
		case ACTIONS.go_start:
			seek(0);
			return;
		case ACTIONS.go_end:
			if (project !== null) seek(timelineEndTick(project));
			return;

		case ACTIONS.tool_select:
			tool.set('select');
			return;
		case ACTIONS.tool_razor:
			tool.set('razor');
			return;
		case ACTIONS.tool_hand:
			tool.set('hand');
			return;
		case ACTIONS.tool_zoom:
			tool.set('zoom');
			return;

		case ACTIONS.set_in: {
			if (project === null) return;
			const area = workAreaWithIn(project);
			if (area === 'invalid') {
				toast.set('The in point must come before the out point');
				return;
			}
			return setWorkArea(area);
		}
		case ACTIONS.set_out: {
			if (project === null) return;
			const area = workAreaWithOut(project);
			if (area === 'invalid') {
				toast.set('The out point must come after the in point');
				return;
			}
			return setWorkArea(area);
		}
		case ACTIONS.clear_in_out:
			return setWorkArea(null);

		case ACTIONS.add_marker: {
			const count = project?.markers.length ?? 0;
			return dispatch({
				type: 'add_marker',
				time: get(playhead),
				label: `Marker ${count + 1}`,
				color: null,
			}).then(() => undefined);
		}

		case ACTIONS.toggle_snap: {
			if (project === null) return;
			return dispatchWith((p) => ({
				type: 'set_modes',
				magnetic: p.modes.magnetic,
				snap: !p.modes.snap,
			})).then(() => undefined);
		}

		case ACTIONS.delete:
			if (ids.length === 0) return;
			// Delete ripples only when magnetic mode is on.
			return dispatchWith((p) => ({
				type: 'delete_clips',
				ids,
				ripple: p.modes.magnetic,
			})).then(() => undefined);
		case ACTIONS.ripple_delete:
			if (ids.length === 0) return;
			return dispatch({ type: 'delete_clips', ids, ripple: true }).then(() => undefined);

		case ACTIONS.split:
			// Stale-state guard: a pending edit may still change what the playhead cuts.
			if (isBusy()) return;
			// An empty selection splits every targeted track (the Rust side decides).
			return dispatch({ type: 'split_at', time: get(playhead), ids }).then(() => undefined);

		case ACTIONS.group:
			if (ids.length === 0) return;
			return dispatch({ type: 'group_clips', ids }).then(() => undefined);
		case ACTIONS.ungroup:
			if (ids.length === 0) return;
			return dispatch({ type: 'ungroup', ids }).then(() => undefined);
		case ACTIONS.link_toggle: {
			const clips = get(selectedClips);
			if (clips.length === 0) return;
			const first = clips[0]?.link_id ?? null;
			const allLinked =
				first !== null && clips.every((c) => c.link_id !== null && c.link_id === first);
			const cmd = allLinked
				? { type: 'unlink_clips' as const, ids }
				: { type: 'link_clips' as const, ids };
			return dispatch(cmd).then(() => undefined);
		}

		case ACTIONS.copy:
			return copySelection().then(() => undefined);
		case ACTIONS.cut: {
			if (ids.length === 0) return;
			return (async () => {
				const copied = await copySelection();
				if (copied === null) return;
				await dispatch({ type: 'delete_clips', ids, ripple: false });
			})();
		}
		case ACTIONS.paste:
			return pasteAtPlayhead().then(() => undefined);
		case ACTIONS.duplicate: {
			// Stale-state guard, then the offset comes from the latest snapshot when the edit runs.
			if (isBusy()) return;
			if (ids.length === 0) return;
			return dispatchWith((p) => {
				const idSet = new Set(ids);
				const clips = p.clips.filter((c) => idSet.has(c.id));
				if (clips.length === 0) return null;
				const offset = duplicateOffset(clips);
				if (offset <= 0) return null;
				return { type: 'duplicate_clips', ids, offset };
			}).then(() => undefined);
		}
		case ACTIONS.select_all:
			selectAllClips();
			return;

		case ACTIONS.zoom_in:
			zoomBy(1.25);
			return;
		case ACTIONS.zoom_out:
			zoomBy(0.8);
			return;
		case ACTIONS.zoom_fit:
			zoomToFit(ctx.viewWidth);
			return;

		case ACTIONS.undo:
			return undo();
		case ACTIONS.redo:
			return redo();

		case ACTIONS.save:
			return saveNow().then((ok) => {
				if (ok) toast.set('Saved');
			});
		case ACTIONS.export:
			ctx.openExport();
			return;
		case ACTIONS.export_still: {
			const at = get(playhead);
			return (async () => {
				try {
					const path = await editorApi.exportStill(at, 'png', 90);
					toast.set(`Saved still to ${path}`);
				} catch (e) {
					toast.set(String(e));
				}
			})();
		}
		case ACTIONS.insert_from_source:
			if (!ctx.hasSource) {
				toast.set('Open a clip in the source monitor first');
				return;
			}
			sourceRequest.set({ action: 'insert' });
			return;
		case ACTIONS.overwrite_from_source:
			if (!ctx.hasSource) {
				toast.set('Open a clip in the source monitor first');
				return;
			}
			sourceRequest.set({ action: 'overwrite' });
			return;

		case ACTIONS.escape: {
			// Escape first cancels a drag. The selection is cleared only when no drag was cancelled.
			const cancelled = dragCanceller?.() ?? false;
			if (!cancelled) selection.set([]);
			return;
		}
		case ACTIONS.show_shortcuts:
			ctx.showShortcuts();
			return;
		default:
			return;
	}
}
