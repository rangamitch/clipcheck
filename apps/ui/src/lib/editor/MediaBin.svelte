<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { isTauriRuntime } from '../window';
  import { editorApi } from './api';
  import MediaPicker from './MediaPicker.svelte';
  import { binDrag, dispatch, playhead, project, toast } from './store';
  import { formatDuration } from './ticks';
  import type { MediaRef, PlaceMode } from './types';

  interface Props {
    /** Opens the media in the source monitor (double-click, Enter, or "Preview in source"). */
    onsource: (mediaId: string) => void;
  }

  let { onsource }: Props = $props();

  type SortKey = 'name' | 'duration' | 'added';
  type ViewMode = 'list' | 'grid';
  type PickKind = 'media' | 'any';

  /** Pointer travel in pixels before a press on a row becomes a drag to the timeline. */
  const DRAG_THRESHOLD_PX = 4;
  /** Thumbnail height in pixels. */
  const THUMB_HEIGHT = 48;

  let view = $state<ViewMode>('list');
  let sortKey = $state<SortKey>('added');
  let query = $state('');
  let selectedId = $state<string | null>(null);
  let pickerOpen = $state(false);
  let dragOver = $state(false);
  /** Missing flags from the last `editor_media_status` call, keyed by media id. */
  let statusMap = $state<Record<string, boolean>>({});
  /** First-frame thumbnails as data URLs, keyed by media id. */
  let thumbs = $state<Record<string, string>>({});

  /** Media ids whose thumbnail was already requested (plain set, not rendered). */
  const thumbRequested = new Set<string>();
  let destroyed = false;
  let unlistenDrop: (() => void) | null = null;

  /** A press on a media row that may turn into a drag to the timeline. */
  interface RowDrag {
    media: MediaRef;
    pointerId: number;
    el: HTMLElement | null;
    startX: number;
    startY: number;
    active: boolean;
  }

  let rowDrag: RowDrag | null = null;
  /** Floating label that follows the pointer during a drag; null when no drag is active. */
  let ghost = $state<{ x: number; y: number; name: string } | null>(null);
  /** Set when a drag just ended, so the click that follows its pointerup does not reselect. */
  let suppressClick = false;

  const media = $derived<MediaRef[]>($project?.media ?? []);
  const usedIds = $derived(
    new Set(($project?.clips ?? []).flatMap((c) => (c.media_id ? [c.media_id] : []))),
  );
  const projectId = $derived($project?.id ?? null);

  /** Media after the search filter and the sort order. "Date added" keeps the project list order. */
  const visible = $derived.by((): MediaRef[] => {
    const q = query.trim().toLowerCase();
    const list = media.filter((m) => q === '' || m.name.toLowerCase().includes(q));
    if (sortKey === 'name') return [...list].sort((a, b) => a.name.localeCompare(b.name));
    if (sortKey === 'duration') return [...list].sort((a, b) => a.duration - b.duration);
    return list;
  });

  const selected = $derived(media.find((m) => m.id === selectedId) ?? null);
  /** The one row in the tab order (roving tabindex): the selection, else the first row. */
  const tabStopId = $derived(selectedId ?? visible[0]?.id ?? null);

  /** Missing flag: the status from the last check wins; the project flag is the fallback. */
  function isMissing(m: MediaRef): boolean {
    return m.id in statusMap ? statusMap[m.id] : m.missing;
  }

  /** Refreshes the missing flags from the backend. Errors leave the project flags in use. */
  async function refreshStatus(): Promise<void> {
    try {
      const list = await editorApi.mediaStatus();
      const next: Record<string, boolean> = {};
      for (const s of list) next[s.mediaId] = s.missing;
      statusMap = next;
    } catch {
      // Keep the flags already known.
    }
  }

  /** Requests the first-frame thumbnail once. Audio and missing media get a placeholder instead. */
  async function loadThumb(m: MediaRef): Promise<void> {
    if (thumbRequested.has(m.id) || m.kind === 'audio' || isMissing(m)) return;
    thumbRequested.add(m.id);
    try {
      const [url] = await editorApi.thumbnails(m.id, [0], THUMB_HEIGHT);
      if (url) thumbs[m.id] = url;
    } catch {
      // The placeholder stays.
    }
  }

  /**
   * Svelte action: loads the thumbnail once the row scrolls into view. Re-checks on every update,
   * so a row that was missing gets its thumbnail after a relink.
   */
  function lazyThumb(
    node: HTMLElement,
    param: { media: MediaRef; missing: boolean },
  ): { update: (next: { media: MediaRef; missing: boolean }) => void; destroy: () => void } {
    let current = param;
    let inView = false;
    const check = (): void => {
      if (inView) void loadThumb(current.media);
    };
    if (typeof IntersectionObserver === 'undefined') {
      inView = true;
      check();
      return { update: (next) => { current = next; check(); }, destroy: () => {} };
    }
    const observer = new IntersectionObserver((entries) => {
      inView = entries.some((e) => e.isIntersecting);
      check();
    });
    observer.observe(node);
    return {
      update: (next) => {
        current = next;
        check();
      },
      destroy: () => observer.disconnect(),
    };
  }

  /** Opens the native file picker. Resolves to chosen absolute paths (empty when cancelled). */
  async function pickFiles(kind: PickKind): Promise<string[]> {
    if (!isTauriRuntime()) return [];
    return invoke<string[]>('editor_pick_files', { initial: null, kind });
  }

  function baseName(path: string): string {
    const parts = path.split(/[\\/]/);
    return parts[parts.length - 1] || path;
  }

  interface MediaJob {
    label: string;
    load: () => Promise<MediaRef>;
  }

  /**
   * Loads each media in order and adds it to the project. Failures are collected into one toast
   * at the end; the toast replaces the per-command error that `dispatch` shows.
   */
  async function addMedia(jobs: MediaJob[]): Promise<void> {
    if (jobs.length === 0) return;
    const failed: string[] = [];
    for (const job of jobs) {
      try {
        const media = await job.load();
        const reply = await dispatch({ type: 'add_media', media });
        if (reply === null) failed.push(job.label);
      } catch (e) {
        failed.push(`${job.label} (${String(e)})`);
      }
    }
    if (failed.length > 0) {
      toast.set(`Could not add ${failed.length} of ${jobs.length} item(s): ${failed.join('; ')}`);
    }
    void refreshStatus();
  }

  /** Probes and adds files given by absolute path. */
  async function importPaths(paths: string[]): Promise<void> {
    await addMedia(
      paths.map((p) => ({ label: baseName(p), load: () => editorApi.probeMedia(p) })),
    );
  }

  /** "Import files" button: native picker, then probe and add each file. */
  async function importFiles(): Promise<void> {
    try {
      await importPaths(await pickFiles('media'));
    } catch (e) {
      toast.set(`Import failed: ${String(e)}`);
    }
  }

  /** Library picker result: resolves each recording and adds it to the project. */
  async function addFromLibrary(recordingIds: string[]): Promise<void> {
    await addMedia(
      recordingIds.map((id) => ({ label: id, load: () => editorApi.mediaFromRecording(id) })),
    );
  }

  /** "Relink": picks a replacement file and points the missing media entry at it. */
  async function relink(m: MediaRef): Promise<void> {
    try {
      const [path] = await pickFiles('media');
      if (!path) return;
      const replacement = await editorApi.probeMedia(path);
      await dispatch({ type: 'relink_media', media_id: m.id, media: replacement });
      void refreshStatus();
    } catch (e) {
      toast.set(`Relink failed: ${String(e)}`);
    }
  }

  /** Places the media at the playhead. `overwrite` and `insert` use the playhead; `append` goes to the end. */
  function place(m: MediaRef, mode: PlaceMode): void {
    void dispatch({
      type: 'place_media',
      media_id: m.id,
      at: $playhead,
      src_in: 0,
      src_len: 0,
      video_track: null,
      mode,
    });
  }

  /** Removes an unused media entry. The in-use error is shown by `dispatch` as a toast. */
  async function remove(m: MediaRef): Promise<void> {
    const reply = await dispatch({ type: 'remove_media', media_id: m.id });
    if (reply !== null && selectedId === m.id) selectedId = null;
  }

  /**
   * Pointer drag from the bin to the timeline. HTML5 drag and drop is not used: Tauri's native
   * drag-drop handler swallows it inside the webview on Windows.
   */
  function onRowPointerDown(e: PointerEvent, m: MediaRef): void {
    if (e.button !== 0 || !e.isPrimary) return;
    if (e.target instanceof Element && e.target.closest('button')) return;
    rowDrag = {
      media: m,
      pointerId: e.pointerId,
      el: e.currentTarget instanceof HTMLElement ? e.currentTarget : null,
      startX: e.clientX,
      startY: e.clientY,
      active: false,
    };
  }

  function onRowPointerMove(e: PointerEvent): void {
    const d = rowDrag;
    if (!d || e.pointerId !== d.pointerId) return;
    if (!d.active) {
      if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < DRAG_THRESHOLD_PX) return;
      d.active = true;
      try {
        d.el?.setPointerCapture(d.pointerId);
      } catch {
        // Capture is best effort; the drag still ends on pointerup, cancel or Escape.
      }
      binDrag.set({ mediaId: d.media.id, durationTicks: d.media.duration });
      document.body.classList.add('bin-dragging');
      window.addEventListener('keydown', onDragKey, true);
    }
    ghost = { x: e.clientX + 12, y: e.clientY + 12, name: d.media.name };
  }

  function onRowPointerUp(e: PointerEvent): void {
    const d = rowDrag;
    if (!d || e.pointerId !== d.pointerId) return;
    if (d.active) suppressClick = true;
    endDrag();
  }

  /**
   * Ends the current pointer drag, if any. `binDrag` is cleared on the next animation frame so
   * the timeline's own pointerup handler can still read the dragged media during this event.
   */
  function endDrag(): void {
    const d = rowDrag;
    rowDrag = null;
    if (!d || !d.active) return;
    ghost = null;
    document.body.classList.remove('bin-dragging');
    window.removeEventListener('keydown', onDragKey, true);
    try {
      d.el?.releasePointerCapture(d.pointerId);
    } catch {
      // The pointer was already released.
    }
    requestAnimationFrame(() => {
      suppressClick = false;
      if (rowDrag === null) binDrag.set(null);
    });
  }

  /** Escape cancels an active drag. */
  function onDragKey(e: KeyboardEvent): void {
    if (e.key !== 'Escape') return;
    e.stopPropagation();
    endDrag();
  }

  /**
   * Keys on a row (roving tabindex): Arrow keys move the selection and focus, Enter opens the
   * selected media in the source monitor. Handled keys do not reach the editor-wide shortcuts,
   * and Delete/Backspace never reach the timeline's delete of the selected clips.
   */
  async function onListKey(e: KeyboardEvent, m: MediaRef): Promise<void> {
    if (e.key === 'Delete' || e.key === 'Backspace') {
      e.stopPropagation();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      e.stopPropagation();
      const idx = visible.findIndex((x) => x.id === m.id);
      const step = e.key === 'ArrowDown' ? 1 : -1;
      const next = Math.min(visible.length - 1, Math.max(0, idx + step));
      const target = visible[next];
      if (!target) return;
      selectedId = target.id;
      await tick();
      document.getElementById(`media-${target.id}`)?.focus();
    } else if (e.key === 'Enter' && !isMissing(m)) {
      e.preventDefault();
      e.stopPropagation();
      onsource(m.id);
    }
  }

  function onWindowFocus(): void {
    void refreshStatus();
  }

  // Refresh missing flags on mount and whenever the open project changes.
  $effect(() => {
    void projectId;
    void refreshStatus();
  });

  onMount(() => {
    window.addEventListener('focus', onWindowFocus);
    if (isTauriRuntime()) {
      void getCurrentWebview()
        .onDragDropEvent((event) => {
          const p = event.payload;
          if (p.type === 'enter' || p.type === 'over') {
            dragOver = true;
          } else if (p.type === 'leave') {
            dragOver = false;
          } else {
            dragOver = false;
            void importPaths(p.paths);
          }
        })
        .then((un) => {
          if (destroyed) un();
          else unlistenDrop = un;
        })
        .catch(() => {
          // Drop import is unavailable; the picker still works.
        });
    }
  });

  onDestroy(() => {
    destroyed = true;
    endDrag();
    window.removeEventListener('focus', onWindowFocus);
    unlistenDrop?.();
    unlistenDrop = null;
  });
</script>

<section class="bin" aria-label="Media bin">
  <header class="bin-head">
    <h2>Media</h2>
    <div class="head-actions">
      <button type="button" onclick={() => (pickerOpen = true)} aria-label="Add media from the ClipCheck library">
        Add from library
      </button>
      <button type="button" onclick={importFiles} aria-label="Import media files from disk">
        Import files
      </button>
    </div>
  </header>

  <div class="bin-tools">
    <input
      type="search"
      class="search"
      placeholder="Search media"
      aria-label="Search media"
      bind:value={query}
    />
    <select class="sort" aria-label="Sort media" bind:value={sortKey}>
      <option value="added">Date added</option>
      <option value="name">Name</option>
      <option value="duration">Duration</option>
    </select>
    <div class="view-toggle" role="group" aria-label="Media view">
      <button type="button" aria-pressed={view === 'list'} onclick={() => (view = 'list')}>List</button>
      <button type="button" aria-pressed={view === 'grid'} onclick={() => (view = 'grid')}>Grid</button>
    </div>
  </div>

  {#if media.length === 0}
    <p class="empty">No media yet. Add from the library, import files, or drop files here.</p>
  {:else if visible.length === 0}
    <p class="empty">No media matches the search.</p>
  {:else}
    <div
      class="items"
      class:grid={view === 'grid'}
      role="listbox"
      aria-label="Project media"
    >
      {#each visible as m (m.id)}
        {@const missing = isMissing(m)}
        {@const isSelected = m.id === selectedId}
        <div
          id={`media-${m.id}`}
          class="item"
          class:selected={isSelected}
          class:missing
          role="option"
          tabindex={m.id === tabStopId ? 0 : -1}
          aria-selected={isSelected}
          aria-label={`${m.name}, ${formatDuration(m.duration)}${missing ? ', missing' : ''}`}
          onpointerdown={(e) => onRowPointerDown(e, m)}
          onpointermove={onRowPointerMove}
          onpointerup={onRowPointerUp}
          onpointercancel={endDrag}
          onlostpointercapture={endDrag}
          onclick={() => {
            if (suppressClick) {
              suppressClick = false;
              return;
            }
            selectedId = m.id;
          }}
          onkeydown={(e) => void onListKey(e, m)}
          ondblclick={() => {
            if (!missing) onsource(m.id);
          }}
          use:lazyThumb={{ media: m, missing }}
        >
          <div class="thumb">
            {#if thumbs[m.id]}
              <img src={thumbs[m.id]} alt="" draggable="false" />
            {:else}
              <svg viewBox="0 0 24 24" aria-hidden="true" class="placeholder">
                {#if m.kind === 'audio'}
                  <path d="M9 18V5l12-2v13" fill="none" stroke="currentColor" stroke-width="1.6" />
                  <circle cx="6" cy="18" r="3" fill="none" stroke="currentColor" stroke-width="1.6" />
                  <circle cx="18" cy="16" r="3" fill="none" stroke="currentColor" stroke-width="1.6" />
                {:else}
                  <rect x="3" y="5" width="18" height="14" rx="2" fill="none" stroke="currentColor" stroke-width="1.6" />
                  <path d="M10 9.5v5l4-2.5z" fill="currentColor" />
                {/if}
              </svg>
            {/if}
          </div>

          <div class="meta">
            <div class="name-row">
              <span class="name" title={m.name}>{m.name}</span>
              {#if usedIds.has(m.id)}
                <span class="used" title="Used on the timeline" aria-label="Used on the timeline">&#10003;</span>
              {/if}
              {#if missing}
                <span class="badge" role="status">Missing</span>
              {/if}
            </div>
            <div class="info-row">
              <span class="duration">{formatDuration(m.duration)}</span>
              {#if m.width > 0 && m.height > 0}
                <span class="chip">{m.width}x{m.height}</span>
              {/if}
              {#if m.fps > 0}
                <span class="chip">{Math.round(m.fps * 100) / 100} fps</span>
              {/if}
              {#if m.video_codec}
                <span class="chip">{m.video_codec}</span>
              {/if}
              {#if m.audio.length > 0}
                <span class="chip">{m.audio.length} audio</span>
              {/if}
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if selected}
    {@const selMissing = isMissing(selected)}
    <div class="actions" role="toolbar" aria-label="Selected media actions">
      <button type="button" disabled={selMissing} onclick={() => place(selected, 'overwrite')}>
        Add to timeline
      </button>
      <button type="button" disabled={selMissing} onclick={() => place(selected, 'append')}>
        Append
      </button>
      <button type="button" disabled={selMissing} onclick={() => onsource(selected.id)}>
        Preview in source
      </button>
      {#if selMissing}
        <button type="button" onclick={() => relink(selected)}>Relink</button>
      {/if}
      <button type="button" class="danger" onclick={() => remove(selected)}>Remove</button>
    </div>
  {/if}

  {#if dragOver}
    <div class="drop-overlay" aria-hidden="true">Drop files to import</div>
  {/if}

  {#if ghost}
    <div
      class="drag-ghost"
      aria-hidden="true"
      style:left="{ghost.x}px"
      style:top="{ghost.y}px"
    >
      {ghost.name}
    </div>
  {/if}
</section>

<MediaPicker
  open={pickerOpen}
  multiple={true}
  onpick={(ids) => {
    pickerOpen = false;
    void addFromLibrary(ids);
  }}
  onclose={() => (pickerOpen = false)}
/>

<style>
  .bin {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 8px;
    height: 100%;
    min-height: 0;
    padding: 10px;
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .bin-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }

  h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  .head-actions,
  .view-toggle,
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .bin-tools {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
  }

  .search {
    flex: 1 1 140px;
    min-width: 0;
  }

  button {
    padding: 4px 10px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
  }

  button:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  button:focus-visible,
  input:focus-visible,
  select:focus-visible,
  .items:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  button[aria-pressed='true'] {
    border-color: var(--accent);
  }

  button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  button.danger {
    color: var(--danger);
  }

  input,
  select {
    padding: 4px 8px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }

  .items {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-radius: 6px;
  }

  .items.grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    align-content: start;
  }

  .item {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 6px;
    border: 1px solid transparent;
    border-radius: 6px;
    background: var(--surface-2);
    cursor: grab;
    min-width: 0;
  }

  .items.grid .item {
    flex-direction: column;
    align-items: stretch;
  }

  .item.selected {
    border-color: var(--accent);
  }

  .item.missing .thumb {
    border-color: var(--danger);
  }

  .thumb {
    flex: 0 0 auto;
    width: 64px;
    height: 48px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
    color: var(--text-dim);
  }

  .items.grid .thumb {
    width: 100%;
  }

  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .placeholder {
    width: 24px;
    height: 24px;
  }

  .meta {
    min-width: 0;
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .name-row,
  .info-row {
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
    flex-wrap: wrap;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    flex: 1 1 auto;
  }

  .used {
    color: var(--accent);
    font-weight: 700;
  }

  .badge {
    padding: 0 6px;
    font-size: 11px;
    color: #fff;
    background: var(--danger);
    border-radius: 4px;
  }

  .duration,
  .chip {
    font-size: 12px;
    color: var(--text-dim);
  }

  .chip {
    padding: 0 5px;
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  .empty {
    margin: 12px 0;
    color: var(--text-dim);
    text-align: center;
  }

  .drag-ghost {
    position: fixed;
    z-index: 1000;
    pointer-events: none;
    max-width: 220px;
    padding: 4px 8px;
    font-size: 12px;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--accent);
    border-radius: 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  :global(body.bin-dragging),
  :global(body.bin-dragging *) {
    cursor: grabbing !important;
    user-select: none;
  }

  .drop-overlay {
    position: absolute;
    inset: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 2px dashed var(--accent);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--bg) 75%, transparent);
    color: var(--text);
    pointer-events: none;
  }
</style>
