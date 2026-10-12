// Editor state stores (Svelte stores, no DOM access at import time).
//
// The snapshot from the Rust session is the source of truth for the project; everything else here is
// UI state (selection, playhead, zoom, scroll) plus the plumbing for edits, undo/redo, autosave and
// the cosmetic ui_state that is saved with the project.

import { derived, get, writable } from 'svelte/store';
import type { ApplyReply, Clip, Command, EditorSnapshot, JobDto, Project, Tick } from './types';
import { editorApi } from './api';
import { MAX_PROJECT_TICKS, clampTick, clipEnd, tickToSec } from './ticks';

/** Editor tools. */
export type EditorTool = 'select' | 'razor' | 'hand' | 'zoom';

/** How `selectClips` combines the given ids with the current selection. */
export type SelectMode = 'replace' | 'add' | 'toggle';

/** Zoom limits in pixels per second. */
const MIN_PX_PER_SEC = 0.05;
const MAX_PX_PER_SEC = 4000;
/** Default zoom (pixels per second). */
const DEFAULT_PX_PER_SEC = 80;
/** Horizontal padding kept free when zooming to fit. */
const FIT_PADDING_PX = 40;
/** Minimum timeline length used by zoomToFit and fitToContent, in seconds. */
const FIT_MIN_SECONDS = 10;
/** Timeline length that fitToContent shows for an empty project, in seconds. */
export const DEFAULT_EMPTY_FIT_SECONDS = 15 * 60;
/** Autosave interval limits, in seconds. */
const MIN_AUTOSAVE_S = 10;
const MAX_AUTOSAVE_S = 600;
const DEFAULT_AUTOSAVE_S = 60;
/** Debounce for persisting ui_state, in milliseconds. */
const UI_STATE_DEBOUNCE_MS = 800;

/** Argument type of `editorApi.setUiState`. */
type UiStatePayload = Parameters<typeof editorApi.setUiState>[0];

// ---------------------------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------------------------

/** The current project snapshot, or null when no project is open. */
export const snapshot = writable<EditorSnapshot | null>(null);
/** Selected clip ids. */
export const selection = writable<string[]>([]);
/** Playhead position in ticks. */
export const playhead = writable<Tick>(0);
/** True while playback runs. */
export const playing = writable<boolean>(false);
/** Active tool. */
export const tool = writable<EditorTool>('select');
/** Zoom in pixels per second. */
export const pxPerSec = writable<number>(DEFAULT_PX_PER_SEC);
/** Horizontal scroll in pixels. */
export const scrollX = writable<number>(0);
/** Vertical scroll in pixels. */
export const scrollY = writable<number>(0);
/** Message for the toast area, or null. */
export const toast = writable<string | null>(null);
/** True while at least one edit is in flight. */
export const busy = writable<boolean>(false);
/** Export jobs shown in the queue. */
export const exportJobs = writable<JobDto[]>([]);
/** Media bin pointer-drag state: the media being dragged and its duration, or null. */
export const binDrag = writable<{ mediaId: string; durationTicks: number } | null>(null);

/** The project of the current snapshot, or null. */
export const project = derived(snapshot, ($s): Project | null => $s?.project ?? null);
/** The render plan of the current snapshot, or null. */
export const plan = derived(snapshot, ($s) => $s?.plan ?? null);
/** Clips for the selected ids that still exist in the project, in selection order. */
export const selectedClips = derived(
	[snapshot, selection],
	([$s, $sel]): Clip[] => {
		if ($s === null) return [];
		const byId = new Map<string, Clip>();
		for (const c of $s.project.clips) byId.set(c.id, c);
		const out: Clip[] = [];
		for (const id of $sel) {
			const c = byId.get(id);
			if (c !== undefined) out.push(c);
		}
		return out;
	},
);

// ---------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------

/** Removes duplicate ids, keeping the first occurrence. */
function dedupe(ids: readonly string[]): string[] {
	return Array.from(new Set(ids));
}

/**
 * Keeps only the ids of clips that exist in `project`, deduplicated. A null project gives [].
 */
export function pruneSelection(project: Project | null, ids: readonly string[]): string[] {
	if (project === null) return [];
	const known = new Set(project.clips.map((c) => c.id));
	return dedupe(ids).filter((id) => known.has(id));
}

/**
 * Expands each id to every clip sharing its `group_id`. Link partners (`link_id`) are not added.
 * Ids that do not exist in the project are dropped. With a null project the ids are only deduped.
 */
export function expandWithGroups(project: Project | null, ids: readonly string[]): string[] {
	if (project === null) return dedupe(ids);
	const byId = new Map<string, Clip>();
	const membersByGroup = new Map<string, string[]>();
	for (const c of project.clips) {
		byId.set(c.id, c);
		if (c.group_id) {
			const list = membersByGroup.get(c.group_id);
			if (list) list.push(c.id);
			else membersByGroup.set(c.group_id, [c.id]);
		}
	}
	const out: string[] = [];
	for (const id of ids) {
		const c = byId.get(id);
		if (c === undefined) continue;
		out.push(id);
		if (c.group_id) {
			const members = membersByGroup.get(c.group_id) ?? [];
			out.push(...members);
		}
	}
	return dedupe(out);
}

/** End of the last clip in the project, in ticks (0 for an empty or null project). */
export function timelineEndTick(project: Project | null): Tick {
	if (project === null) return 0;
	let end = 0;
	for (const c of project.clips) {
		const e = clipEnd(c);
		if (e > end) end = e;
	}
	return end;
}

/** Clamps a zoom value to the supported range. Non-finite values give the default. */
function clampZoom(px: number): number {
	if (!Number.isFinite(px)) return DEFAULT_PX_PER_SEC;
	return Math.min(MAX_PX_PER_SEC, Math.max(MIN_PX_PER_SEC, px));
}

/**
 * True when a snapshot should be zoomed to its content on open: its saved zoom is missing or still
 * the stock default, so the user never customised it.
 */
export function needsFitOnOpen(s: EditorSnapshot): boolean {
	const z = s.project.ui_state.zoom_px_per_sec;
	return !Number.isFinite(z) || z === DEFAULT_PX_PER_SEC;
}

/** Clamps a scroll offset to be non-negative and finite. */
function clampScroll(px: number): number {
	if (!Number.isFinite(px)) return 0;
	return Math.max(0, px);
}

/**
 * Largest valid horizontal scroll at a given zoom (px per second), supplied by the timeline view
 * while it is mounted. Null when no view is mounted, which leaves scroll unbounded above.
 */
let scrollLimitAt: ((pxPerSec: number) => number) | null = null;

/** Registers (or, with null, clears) the function giving the maximum scroll for a zoom level. */
export function setScrollLimit(fn: ((pxPerSec: number) => number) | null): void {
	scrollLimitAt = fn;
}

/** Clamps a scroll offset to be non-negative, finite, and at most the limit for the zoom `px`. */
function clampScrollAt(px: number, zoom: number): number {
	let v = clampScroll(px);
	if (scrollLimitAt !== null) {
		const limit = scrollLimitAt(zoom);
		if (Number.isFinite(limit)) v = Math.min(v, Math.max(0, limit));
	}
	return v;
}

// ---------------------------------------------------------------------------------------------
// Snapshot lifecycle
// ---------------------------------------------------------------------------------------------

/**
 * Installs a snapshot as the open project. Selection is cleared; playhead, zoom and horizontal
 * scroll are restored from `s.project.ui_state`.
 */
export function openSnapshot(s: EditorSnapshot): void {
	snapshot.set(s);
	selection.set([]);
	const ui = s.project.ui_state;
	pxPerSec.set(clampZoom(ui.zoom_px_per_sec));
	scrollX.set(clampScroll(ui.scroll_x));
	scrollY.set(0);
	playhead.set(clampTick(ui.playhead, 0, MAX_PROJECT_TICKS));
}

/** Stops autosave and state persistence, then resets every store to its default. */
export function closeEditor(): void {
	stopAutosave();
	snapshot.set(null);
	selection.set([]);
	playhead.set(0);
	playing.set(false);
	tool.set('select');
	pxPerSec.set(DEFAULT_PX_PER_SEC);
	scrollX.set(0);
	scrollY.set(0);
	toast.set(null);
	binDrag.set(null);
	// exportJobs is deliberately kept: exports outlive the editor session.
}

// ---------------------------------------------------------------------------------------------
// Edit plumbing: every edit goes through one promise chain, so edits apply in call order.
// ---------------------------------------------------------------------------------------------

/** Tail of the edit chain. Never rejects. */
let editChain: Promise<unknown> = Promise.resolve();
/** Edits in flight, so `busy` stays true until the last one finishes. */
let pendingEdits = 0;

/** True while at least one edit is queued or running (the busy counter is above zero). */
export function isBusy(): boolean {
	return pendingEdits > 0;
}

/** Queues `run` after every earlier edit. Sets `busy` while it runs. */
function enqueue<T>(run: () => Promise<T>): Promise<T> {
	const task = async (): Promise<T> => {
		pendingEdits += 1;
		busy.set(true);
		try {
			return await run();
		} finally {
			pendingEdits -= 1;
			busy.set(pendingEdits > 0);
		}
	};
	const result = editChain.then(task);
	editChain = result.catch(() => undefined);
	return result;
}

/** Shows an error text in the toast. */
function reportError(e: unknown): void {
	toast.set(String(e));
}

/**
 * Applies one command. Returns the reply on success, or null when no project is open or the
 * command failed (the error text goes to `toast`). Calls are applied in the order they were made.
 */
export function dispatch(cmd: Command): Promise<ApplyReply | null> {
	return dispatchWith(() => cmd);
}

/**
 * Like `dispatch`, but the command is built from the latest snapshot project when the queued task
 * actually starts, so it never uses stale state. `build` returning null skips the edit (resolves
 * null). Errors from `build` or from the backend go to `toast`.
 */
export function dispatchWith(build: (p: Project) => Command | null): Promise<ApplyReply | null> {
	return enqueue(async (): Promise<ApplyReply | null> => {
		const current = get(snapshot);
		if (current === null) return null;
		try {
			const cmd = build(current.project);
			if (cmd === null) return null;
			const reply = await editorApi.apply(cmd);
			snapshot.set(reply.snapshot);
			selection.update((ids) => pruneSelection(reply.snapshot.project, ids));
			return reply;
		} catch (e) {
			reportError(e);
			return null;
		}
	});
}

/** Undoes the last edit. Errors go to `toast`. */
export function undo(): Promise<void> {
	return enqueue(async (): Promise<void> => {
		if (get(snapshot) === null) return;
		try {
			const s = await editorApi.undo();
			snapshot.set(s);
			selection.update((ids) => pruneSelection(s.project, ids));
		} catch (e) {
			reportError(e);
		}
	});
}

/** Redoes the last undone edit. Errors go to `toast`. */
export function redo(): Promise<void> {
	return enqueue(async (): Promise<void> => {
		if (get(snapshot) === null) return;
		try {
			const s = await editorApi.redo();
			snapshot.set(s);
			selection.update((ids) => pruneSelection(s.project, ids));
		} catch (e) {
			reportError(e);
		}
	});
}

// ---------------------------------------------------------------------------------------------
// Selection and clipboard
// ---------------------------------------------------------------------------------------------

/**
 * Changes the selection. `replace` sets it, `add` unions, `toggle` flips membership of each id.
 * Each id also selects the clips of its group (link partners are not selected).
 */
export function selectClips(ids: string[], mode: SelectMode): void {
	const expanded = expandWithGroups(get(snapshot)?.project ?? null, ids);
	if (mode === 'replace') {
		selection.set(expanded);
		return;
	}
	if (mode === 'add') {
		selection.update((cur) => dedupe([...cur, ...expanded]));
		return;
	}
	selection.update((cur) => {
		const next = new Set(cur);
		for (const id of expanded) {
			if (next.has(id)) next.delete(id);
			else next.add(id);
		}
		return Array.from(next);
	});
}

/** Selects every clip in the project. */
export function selectAllClips(): void {
	const p = get(snapshot)?.project ?? null;
	if (p === null) {
		selection.set([]);
		return;
	}
	const locked = new Set(p.tracks.filter((t) => t.locked).map((t) => t.id));
	selection.set(p.clips.filter((c) => !locked.has(c.track_id)).map((c) => c.id));
}

/** Copies the selected clips to the session clipboard. Does nothing when nothing is selected. */
export function copySelection(): Promise<ApplyReply | null> {
	const ids = get(selection);
	if (ids.length === 0) return Promise.resolve(null);
	return dispatch({ type: 'copy_clips', ids });
}

/** Pastes the clipboard at the playhead in overwrite mode. */
export function pasteAtPlayhead(): Promise<ApplyReply | null> {
	return dispatch({
		type: 'paste_clips',
		clips: [],
		at: get(playhead),
		track_id: null,
		mode: 'overwrite',
	});
}

// ---------------------------------------------------------------------------------------------
// Playhead and zoom
// ---------------------------------------------------------------------------------------------

/** Moves the playhead, clamped to 0..MAX_PROJECT_TICKS. */
export function seek(t: Tick): void {
	playhead.set(clampTick(t, 0, MAX_PROJECT_TICKS));
}

/**
 * Multiplies the zoom by `factor` (clamped to 0.05..4000 px per second). Scroll is adjusted so the
 * anchor tick (default: the playhead) keeps its screen x position.
 */
export function zoomBy(factor: number, anchorTick?: Tick): void {
	if (!Number.isFinite(factor) || factor <= 0) return;
	const oldPx = get(pxPerSec);
	const newPx = clampZoom(oldPx * factor);
	const anchor = anchorTick ?? get(playhead);
	const anchorSec = Number.isFinite(anchor) ? tickToSec(anchor) : 0;
	const oldScroll = get(scrollX);
	const newScroll = clampScrollAt(oldScroll + anchorSec * (newPx - oldPx), newPx);
	pxPerSec.set(newPx);
	scrollX.set(newScroll);
}

/**
 * Sets the zoom so the whole timeline (at least 10 s) fits in `viewWidthPx`, and scrolls to 0.
 */
export function zoomToFit(viewWidthPx: number): void {
	if (!Number.isFinite(viewWidthPx)) return;
	const p = get(snapshot)?.project ?? null;
	const seconds = Math.max(FIT_MIN_SECONDS, tickToSec(timelineEndTick(p)));
	pxPerSec.set(clampZoom((viewWidthPx - FIT_PADDING_PX) / seconds));
	scrollX.set(0);
}

/**
 * Sets the zoom so the content fits in `viewWidthPx`, and scrolls to 0. The content is the whole
 * timeline (at least 10 s) for a project with clips, or 15 minutes for an empty project.
 */
export function fitToContent(viewWidthPx: number): void {
	if (!Number.isFinite(viewWidthPx)) return;
	const p = get(snapshot)?.project ?? null;
	const seconds =
		p === null || p.clips.length === 0
			? DEFAULT_EMPTY_FIT_SECONDS
			: Math.max(FIT_MIN_SECONDS, tickToSec(timelineEndTick(p)));
	pxPerSec.set(clampZoom((viewWidthPx - FIT_PADDING_PX) / seconds));
	scrollX.set(0);
	// Persist the fitted zoom; the ui_state watchers skip the initial value of each subscribe.
	scheduleUiStateSave();
}

// ---------------------------------------------------------------------------------------------
// Autosave and ui_state persistence
// ---------------------------------------------------------------------------------------------

/** Autosave interval timer, or null. */
let autosaveTimer: ReturnType<typeof setInterval> | null = null;
/** Debounce timer for ui_state saves, or null. */
let uiSaveTimer: ReturnType<typeof setTimeout> | null = null;
/** Unsubscribers for the ui_state watchers. */
let uiUnsubs: Array<() => void> = [];

/** Saves the current ui_state now. Errors are swallowed. */
async function saveUiStateNow(): Promise<void> {
	if (get(snapshot) === null) return;
	const payload: UiStatePayload = {
		zoom_px_per_sec: get(pxPerSec),
		scroll_x: get(scrollX),
		playhead: get(playhead),
	};
	try {
		await editorApi.setUiState(payload);
	} catch {
		// Cosmetic state: a failed save is not worth interrupting the user for.
	}
}

/** Schedules a debounced ui_state save. */
function scheduleUiStateSave(): void {
	if (uiSaveTimer !== null) clearTimeout(uiSaveTimer);
	uiSaveTimer = setTimeout(() => {
		uiSaveTimer = null;
		void saveUiStateNow();
	}, UI_STATE_DEBOUNCE_MS);
}

/**
 * Starts autosave every `seconds` (clamped to 10..600; non-finite uses 60). Calling it again
 * replaces the previous timer. Also starts saving ui_state (zoom, scroll, playhead) after changes.
 */
export function startAutosave(seconds: number): void {
	stopAutosave();
	const s = Number.isFinite(seconds)
		? Math.min(MAX_AUTOSAVE_S, Math.max(MIN_AUTOSAVE_S, seconds))
		: DEFAULT_AUTOSAVE_S;
	autosaveTimer = setInterval(() => {
		void (async () => {
			try {
				await editorApi.autosave();
			} catch {
				// Autosave failures are silent; the next tick tries again.
			}
		})();
	}, s * 1000);

	// Skip the initial callback each subscribe makes, so opening a project does not write ui_state.
	const watch = <T>(store: { subscribe: (fn: (v: T) => void) => () => void }): (() => void) => {
		let first = true;
		return store.subscribe(() => {
			if (first) {
				first = false;
				return;
			}
			scheduleUiStateSave();
		});
	};
	uiUnsubs = [watch(pxPerSec), watch(scrollX), watch(playhead)];
}

/** Stops autosave and cancels pending ui_state saves. Safe to call when nothing is running. */
export function stopAutosave(): void {
	if (autosaveTimer !== null) {
		clearInterval(autosaveTimer);
		autosaveTimer = null;
	}
	if (uiSaveTimer !== null) {
		clearTimeout(uiSaveTimer);
		uiSaveTimer = null;
	}
	for (const unsub of uiUnsubs) unsub();
	uiUnsubs = [];
}
