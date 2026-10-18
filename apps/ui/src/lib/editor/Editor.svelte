<script module lang="ts">
  /**
   * Backend session work queued by editor unmounts (final autosave, close). A new editor waits for
   * it before it opens or reuses a session, so the saves land in order.
   */
  let sessionTail: Promise<void> = Promise.resolve();
</script>

<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { editorApi } from './api';
  import { runAction, saveNow, sourceRequest, type ActionContext } from './actions';
  import ExportDialog from './ExportDialog.svelte';
  import Inspector from './Inspector.svelte';
  import MediaBin from './MediaBin.svelte';
  import ProgramMonitor from './ProgramMonitor.svelte';
  import ShortcutsOverlay from './ShortcutsOverlay.svelte';
  import SourceMonitor from './SourceMonitor.svelte';
  import Timeline from './Timeline.svelte';
  import Toolbar from './Toolbar.svelte';
  import ToolRail from './ToolRail.svelte';
  import { ACTIONS, resolveShortcut, type ActionId } from './shortcuts';
  import {
    closeEditor,
    dispatch,
    fitToContent,
    needsFitOnOpen,
    openSnapshot,
    playing,
    snapshot,
    startAutosave,
    stopAutosave,
  } from './store';
  import { msToTick } from './ticks';
  import { watchMicPref } from './micPref';
  import type { Command, EditorSnapshot } from './types';
  import { editorTarget, type EditorTarget } from '../nav';

  interface Props {
    /** Project to open, or the recordings to build a new project from. */
    target: EditorTarget;
  }

  let { target }: Props = $props();

  type Status = 'loading' | 'ready' | 'error';
  type DragKind = 'bin' | 'insp' | 'timeline';

  /** Panel sizes restored from localStorage. */
  interface LayoutSizes {
    /** Media bin width in px. */
    bin: number;
    /** Inspector width in px. */
    insp: number;
    /** Share of the height given to the timeline (0..1). */
    frac: number;
  }

  const LAYOUT_KEY = 'clipcheck.editor.layout';
  const DEFAULT_LAYOUT: LayoutSizes = { bin: 280, insp: 300, frac: 0.38 };
  const BIN_MIN = 180;
  const BIN_MAX = 480;
  const INSP_MIN = 220;
  const INSP_MAX = 480;
  const FRAC_MIN = 0.15;
  const FRAC_MAX = 0.75;
  /** Autosave interval used when the settings cannot be read. */
  const FALLBACK_AUTOSAVE_S = 60;
  /** Width of the track header column in the timeline, excluded from the zoom-to-fit width. */
  const HEADER_W = 140;
  /** Width of the tool rail left of the timeline, excluded from the zoom-to-fit width. */
  const RAIL_W = 32;
  /** What leaving the editor does to the backend session: keep it, close it, or discard it. */
  type LeaveAction = 'keep' | 'close' | 'discard';
  /** Shortcuts that may repeat while a key is held. */
  const REPEATABLE: ReadonlySet<ActionId> = new Set<ActionId>([
    ACTIONS.frame_back,
    ACTIONS.frame_forward,
    ACTIONS.second_back,
    ACTIONS.second_forward,
    ACTIONS.zoom_in,
    ACTIONS.zoom_out,
    ACTIONS.prev_edit,
    ACTIONS.next_edit,
  ]);

  let status = $state<Status>('loading');
  let errorText = $state('');

  /** Media shown in the source monitor, or null. */
  let sourceMediaId = $state<string | null>(null);
  let monitorTab = $state<'program' | 'source'>('program');

  let exportOpen = $state(false);
  let shortcutsOpen = $state(false);
  let homeOpen = $state(false);
  let homeBusy = $state(false);

  /** Set when the user leaves through Home (close) or Discard (discard). Rail navigation keeps the session. */
  let leaveAction: LeaveAction = 'keep';

  /** Height of the editor root, used to turn vertical drags into a fraction. */
  let rootHeight = $state(0);
  /** Width of the timeline cell, including the tool rail and the track headers. */
  let timelineWidth = $state(0);
  /** Width of the track rows only (the cell minus the tool rail and the header column), used by zoom to fit. */
  const trackViewWidth = $derived(Math.max(0, timelineWidth - RAIL_W - HEADER_W));
  /** Set when a project is opened: zoom to its content once the track width is known (never again). */
  let needsInitialFit = $state(false);

  $effect(() => {
    if (!needsInitialFit || $snapshot === null || trackViewWidth <= 0) return;
    needsInitialFit = false;
    fitToContent(trackViewWidth);
  });

  /** Clamps `v` into `lo..hi`. */
  function clamp(v: number, lo: number, hi: number): number {
    return Math.min(hi, Math.max(lo, v));
  }

  /** Returns `v` when it is a finite number (clamped), otherwise the fallback. */
  function numberOr(v: unknown, fallback: number, lo: number, hi: number): number {
    return typeof v === 'number' && Number.isFinite(v) ? clamp(v, lo, hi) : fallback;
  }

  /** Reads the saved panel sizes. Any problem gives the defaults. */
  function readLayout(): LayoutSizes {
    try {
      const raw = localStorage.getItem(LAYOUT_KEY);
      if (raw === null) return { ...DEFAULT_LAYOUT };
      const parsed: unknown = JSON.parse(raw);
      if (typeof parsed !== 'object' || parsed === null) return { ...DEFAULT_LAYOUT };
      const o = parsed as Record<string, unknown>;
      return {
        bin: numberOr(o.bin, DEFAULT_LAYOUT.bin, BIN_MIN, BIN_MAX),
        insp: numberOr(o.insp, DEFAULT_LAYOUT.insp, INSP_MIN, INSP_MAX),
        frac: numberOr(o.frac, DEFAULT_LAYOUT.frac, FRAC_MIN, FRAC_MAX),
      };
    } catch {
      return { ...DEFAULT_LAYOUT };
    }
  }

  /** Saves the panel sizes. Storage failures are ignored (the layout is cosmetic). */
  function writeLayout(): void {
    try {
      const sizes: LayoutSizes = { bin: binWidth, insp: inspWidth, frac: timelineFrac };
      localStorage.setItem(LAYOUT_KEY, JSON.stringify(sizes));
    } catch {
      // Private windows or blocked storage: keep the sizes for this session only.
    }
  }

  const initialLayout = readLayout();
  let binWidth = $state(initialLayout.bin);
  let inspWidth = $state(initialLayout.insp);
  let timelineFrac = $state(initialLayout.frac);

  /** Context passed to `runAction` from the keyboard and toolbar. */
  function actionContext(): ActionContext {
    return {
      openExport: () => {
        exportOpen = true;
      },
      showShortcuts: () => {
        shortcutsOpen = true;
      },
      viewWidth: trackViewWidth > 0 ? trackViewWidth : window.innerWidth,
      hasSource: sourceMediaId !== null,
    };
  }

  /** Starts a splitter drag. Sizes are saved when the pointer is released. */
  function startDrag(kind: DragKind, e: PointerEvent): void {
    if (e.button !== 0) return;
    e.preventDefault();
    const x0 = e.clientX;
    const y0 = e.clientY;
    const bin0 = binWidth;
    const insp0 = inspWidth;
    const frac0 = timelineFrac;

    const move = (ev: PointerEvent): void => {
      const dx = ev.clientX - x0;
      const dy = ev.clientY - y0;
      if (kind === 'bin') {
        binWidth = clamp(bin0 + dx, BIN_MIN, BIN_MAX);
      } else if (kind === 'insp') {
        inspWidth = clamp(insp0 - dx, INSP_MIN, INSP_MAX);
      } else if (rootHeight > 0) {
        timelineFrac = clamp(frac0 - dy / rootHeight, FRAC_MIN, FRAC_MAX);
      }
    };
    const up = (): void => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      writeLayout();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
  }

  /** Arrow keys move a focused splitter (Shift for larger steps). Stops the key reaching the editor. */
  function splitterKey(kind: DragKind, e: KeyboardEvent): void {
    const step = e.shiftKey ? 48 : 16;
    const horizontal = kind !== 'timeline';
    const forward = horizontal ? e.key === 'ArrowRight' : e.key === 'ArrowUp';
    const back = horizontal ? e.key === 'ArrowLeft' : e.key === 'ArrowDown';
    if (!forward && !back) return;
    e.preventDefault();
    e.stopPropagation();
    const sign = forward ? 1 : -1;
    if (kind === 'bin') {
      binWidth = clamp(binWidth + sign * step, BIN_MIN, BIN_MAX);
    } else if (kind === 'insp') {
      inspWidth = clamp(inspWidth - sign * step, INSP_MIN, INSP_MAX);
    } else if (rootHeight > 0) {
      timelineFrac = clamp(timelineFrac + (sign * step) / rootHeight, FRAC_MIN, FRAC_MAX);
    }
    writeLayout();
  }

  /**
   * Switches the monitor tab. Both monitors stay mounted; the inactive one is hidden. Switching to
   * the source monitor stops the program playback.
   */
  function showTab(tab: 'program' | 'source'): void {
    if (tab === 'source') playing.set(false);
    monitorTab = tab;
  }

  /** Called by the media bin when a clip is chosen for the source monitor. */
  function showSource(mediaId: string): void {
    sourceMediaId = mediaId;
    showTab('source');
  }

  /** Closes the source monitor and returns to the program monitor. */
  function closeSource(): void {
    sourceMediaId = null;
    sourceRequest.set(null);
    showTab('program');
  }

  /** Window-level keyboard handling for the editor. Ignored while an overlay is open. */
  function onWindowKey(e: KeyboardEvent): void {
    // Monitors and inputs handle their own keys and mark them as prevented.
    if (e.defaultPrevented) return;
    if (status !== 'ready') return;
    if (exportOpen || shortcutsOpen || homeOpen) return;
    const action = resolveShortcut({
      key: e.key,
      ctrlKey: e.ctrlKey,
      shiftKey: e.shiftKey,
      altKey: e.altKey,
      metaKey: e.metaKey,
      target: e.target instanceof HTMLElement ? e.target : null,
    });
    if (action === null) return;
    e.preventDefault();
    if (e.repeat && !REPEATABLE.has(action)) return;
    void runAction(action, actionContext());
  }

  /** Back to the projects list. Asks first when there are unsaved changes. */
  function requestHome(): void {
    if ($snapshot?.dirty) {
      homeOpen = true;
      return;
    }
    leaveEditor();
  }

  /**
   * Clears the editor target so the app shows the projects list. Unmounting then closes the backend
   * session (the final autosave runs first).
   */
  function leaveEditor(): void {
    leaveAction = 'close';
    editorTarget.set(null);
  }

  /** Saves, then leaves. A failed save keeps the editor open (the error is in the toast). */
  async function saveAndLeave(): Promise<void> {
    homeBusy = true;
    const ok = await saveNow();
    homeBusy = false;
    homeOpen = false;
    if (ok) leaveEditor();
  }

  /**
   * Leaves without saving. The saved project file is kept; the autosave of the unsaved changes is
   * deleted when the session closes.
   */
  function discardAndLeave(): void {
    homeOpen = false;
    leaveAction = 'discard';
    editorTarget.set(null);
  }

  /**
   * Applies the source range of a new project's single recording: the video clip and its linked
   * audio clips are trimmed to [startMs, endMs] and moved back to timeline 0. Failures are shown
   * in the toast by `dispatch`; the steps stop at the first failure.
   */
  async function applyRange(snap: EditorSnapshot, range: { startMs: number; endMs: number }): Promise<void> {
    const video = snap.project.clips.find((c) => c.kind === 'video');
    if (video === undefined || range.endMs <= range.startMs) return;
    const ids = snap.project.clips
      .filter((c) => c.id === video.id || (video.link_id !== null && c.link_id === video.link_id))
      .map((c) => c.id);
    const steps: Command[] = [];
    for (const id of ids) {
      steps.push({ type: 'trim_edge', clip_id: id, edge: 'start', new_time: msToTick(range.startMs), ripple: false });
    }
    steps.push({ type: 'move_clips', moves: ids.map((id) => ({ clip_id: id, new_start: 0, new_track: null })) });
    for (const id of ids) {
      steps.push({
        type: 'trim_edge',
        clip_id: id,
        edge: 'end',
        new_time: msToTick(range.endMs - range.startMs),
        ripple: false,
      });
    }
    for (const cmd of steps) {
      if ((await dispatch(cmd)) === null) return;
    }
  }

  onMount(() => {
    const t = target;
    let cancelled = false;

    // Source requests (`,` and `.`) need the source monitor visible, so switch to it.
    const unsubSource = sourceRequest.subscribe((req) => {
      if (req !== null && sourceMediaId !== null) showTab('source');
    });
    // Tell the backend which microphone the recorder's Mix carries (see micPref.ts).
    const unsubMicPref = watchMicPref();

    void (async () => {
      try {
        await sessionTail;
        if (cancelled) return;
        // Rail navigation unmounts the editor but keeps the session: reuse it when it is the same project.
        const current = await editorApi.currentSnapshot();
        let snap: EditorSnapshot;
        if (t.projectId !== null && current !== null && current.project.id === t.projectId) {
          snap = current;
        } else {
          if (current !== null) await editorApi.closeProject(false);
          snap =
            t.projectId !== null
              ? await editorApi.openProject(t.projectId)
              : await editorApi.newProject('New project', null, t.recordingIds);
        }
        if (cancelled) return;
        needsInitialFit = t.projectId === null || needsFitOnOpen(snap);
        openSnapshot(snap);
        status = 'ready';
        if (t.projectId === null) {
          // Later mounts (rail navigation) reopen this project instead of creating another one.
          editorTarget.set({ projectId: snap.project.id, recordingIds: [] });
          if (t.range !== undefined && t.recordingIds.length === 1) {
            await applyRange(snap, t.range);
          }
        }
      } catch (e) {
        if (cancelled) return;
        errorText = String(e);
        status = 'error';
        return;
      }
      let seconds = FALLBACK_AUTOSAVE_S;
      try {
        seconds = (await editorApi.getSettings()).autosaveS;
      } catch {
        // Keep the fallback interval when the settings cannot be read.
      }
      if (!cancelled) startAutosave(seconds);
    })();

    window.addEventListener('keydown', onWindowKey);
    return () => {
      cancelled = true;
      unsubSource();
      unsubMicPref();
      window.removeEventListener('keydown', onWindowKey);
    };
  });

  onDestroy(() => {
    stopAutosave();
    const mode = leaveAction;
    const autosave = status === 'ready' && mode !== 'discard';
    sessionTail = sessionTail.then(async () => {
      if (autosave) await editorApi.autosave().catch(() => undefined);
      if (mode === 'close') await editorApi.closeProject(false).catch(() => undefined);
      if (mode === 'discard') await editorApi.closeProject(true).catch(() => undefined);
    });
    closeEditor();
  });
</script>

<div class="editor-root" bind:clientHeight={rootHeight}>
  {#if status === 'loading'}
    <div class="state" role="status">Opening project...</div>
  {:else if status === 'error'}
    <div class="state">
      <p class="state-title">This project could not be opened</p>
      <p class="state-detail">{errorText}</p>
      <button class="btn" type="button" onclick={leaveEditor}>Back to projects</button>
    </div>
  {:else}
    <div
      class="grid"
      style:grid-template-columns={`${binWidth}px 6px minmax(0, 1fr) 6px ${inspWidth}px`}
      style:grid-template-rows={`auto minmax(0, ${(1 - timelineFrac) * 100}fr) 6px minmax(0, ${timelineFrac * 100}fr)`}
    >
      <div class="toolbar-cell">
        <Toolbar
          onexport={() => (exportOpen = true)}
          onshortcuts={() => (shortcutsOpen = true)}
          onhome={requestHome}
          viewWidth={trackViewWidth}
          hasSource={sourceMediaId !== null}
        />
      </div>

      <div class="bin">
        <MediaBin onsource={showSource} />
      </div>

      <div
        class="splitter-v bin-splitter"
        role="slider"
        aria-orientation="vertical"
        aria-label="Resize media bin"
        aria-valuemin={BIN_MIN}
        aria-valuemax={BIN_MAX}
        aria-valuenow={binWidth}
        tabindex="0"
        onpointerdown={(e) => startDrag('bin', e)}
        onkeydown={(e) => splitterKey('bin', e)}
      ></div>

      <div class="monitor">
        <div class="tabs" role="tablist" aria-label="Monitor">
          <button
            class="tab"
            class:active={monitorTab === 'program'}
            type="button"
            role="tab"
            aria-selected={monitorTab === 'program'}
            onclick={() => showTab('program')}
          >
            Program
          </button>
          <button
            class="tab"
            class:active={monitorTab === 'source'}
            type="button"
            role="tab"
            aria-selected={monitorTab === 'source'}
            onclick={() => showTab('source')}
          >
            Source
          </button>
        </div>
        <div class="monitor-body" role="tabpanel">
          <div class="pane" hidden={monitorTab !== 'program'}>
            <ProgramMonitor />
          </div>
          {#if sourceMediaId !== null}
            <div class="pane" hidden={monitorTab !== 'source'}>
              <SourceMonitor mediaId={sourceMediaId} onclose={closeSource} />
            </div>
          {:else if monitorTab === 'source'}
            <div class="empty">Choose a clip in the media bin to view it here.</div>
          {/if}
        </div>
      </div>

      <div
        class="splitter-v insp-splitter"
        role="slider"
        aria-orientation="vertical"
        aria-label="Resize inspector"
        aria-valuemin={INSP_MIN}
        aria-valuemax={INSP_MAX}
        aria-valuenow={inspWidth}
        tabindex="0"
        onpointerdown={(e) => startDrag('insp', e)}
        onkeydown={(e) => splitterKey('insp', e)}
      ></div>

      <div class="insp">
        <Inspector />
      </div>

      <div
        class="splitter-h"
        role="slider"
        aria-orientation="horizontal"
        aria-label="Resize timeline"
        aria-valuemin={Math.round(FRAC_MIN * 100)}
        aria-valuemax={Math.round(FRAC_MAX * 100)}
        aria-valuenow={Math.round(timelineFrac * 100)}
        tabindex="0"
        onpointerdown={(e) => startDrag('timeline', e)}
        onkeydown={(e) => splitterKey('timeline', e)}
      ></div>

      <div class="timeline" bind:clientWidth={timelineWidth}>
        <ToolRail />
        <div class="timeline-main">
          <Timeline />
        </div>
      </div>
    </div>
  {/if}

  {#if status === 'ready'}
    <ExportDialog open={exportOpen} onclose={() => (exportOpen = false)} />
    <ShortcutsOverlay open={shortcutsOpen} onclose={() => (shortcutsOpen = false)} />
  {/if}

  {#if homeOpen}
    <div class="scrim">
      <div
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="home-title"
        tabindex="-1"
        onkeydown={(e) => {
          if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            homeOpen = false;
          }
        }}
      >
        <h2 id="home-title" class="modal-title">Unsaved changes</h2>
        <p class="modal-body">
          Save the project before leaving, or discard the changes since your last save.
        </p>
        <div class="modal-actions">
          <button class="btn" type="button" disabled={homeBusy} onclick={() => (homeOpen = false)}>
            Cancel
          </button>
          <button class="btn danger" type="button" disabled={homeBusy} onclick={discardAndLeave}>
            Discard
          </button>
          <button class="btn primary" type="button" disabled={homeBusy} onclick={() => void saveAndLeave()}>
            Save
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .editor-root {
    position: relative;
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
  }

  .grid {
    flex: 1 1 auto;
    min-height: 0;
    display: grid;
    grid-template-areas:
      'toolbar toolbar toolbar toolbar toolbar'
      'bin bin-splitter monitor insp-splitter insp'
      'hsplit hsplit hsplit hsplit hsplit'
      'timeline timeline timeline timeline timeline';
  }

  .toolbar-cell {
    grid-area: toolbar;
    min-width: 0;
  }

  .bin {
    grid-area: bin;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--surface);
  }

  .monitor {
    grid-area: monitor;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }

  .tabs {
    display: flex;
    gap: 2px;
    padding: 4px 6px 0;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    flex: 0 0 auto;
  }

  .tab {
    padding: 4px 12px;
    background: transparent;
    color: var(--text-dim);
    border: none;
    border-bottom: 2px solid transparent;
    font: inherit;
    cursor: pointer;
  }

  .tab.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }

  .tab:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .monitor-body {
    flex: 1 1 auto;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
  }

  .pane:not([hidden]) {
    height: 100%;
    min-width: 0;
    min-height: 0;
  }

  .empty {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    color: var(--text-dim);
    text-align: center;
  }

  .insp {
    grid-area: insp;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--surface);
  }

  .timeline {
    grid-area: timeline;
    display: flex;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--bg);
    border-top: 1px solid var(--border);
  }

  .timeline-main {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
  }

  .bin-splitter {
    grid-area: bin-splitter;
  }

  .insp-splitter {
    grid-area: insp-splitter;
  }

  .splitter-v,
  .splitter-h {
    background: var(--border);
    touch-action: none;
  }

  .splitter-v {
    cursor: col-resize;
  }

  .splitter-h {
    grid-area: hsplit;
    cursor: row-resize;
  }

  .splitter-v:hover,
  .splitter-h:hover,
  .splitter-v:focus-visible,
  .splitter-h:focus-visible {
    background: var(--accent);
    outline: none;
  }

  .state {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 16px;
    color: var(--text-dim);
    text-align: center;
  }

  .state-title {
    margin: 0;
    color: var(--text);
    font-size: 16px;
  }

  .state-detail {
    margin: 0;
    max-width: 480px;
    overflow-wrap: anywhere;
  }

  .btn {
    height: 30px;
    padding: 0 12px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    font: inherit;
    cursor: pointer;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .btn.danger {
    color: var(--danger);
  }

  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .scrim {
    position: absolute;
    inset: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.55);
  }

  .modal {
    width: min(420px, calc(100% - 32px));
    padding: 20px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
  }

  .modal:focus {
    outline: none;
  }

  .modal-title {
    margin: 0 0 8px;
    font-size: 16px;
  }

  .modal-body {
    margin: 0 0 16px;
    color: var(--text-dim);
    line-height: 1.45;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
