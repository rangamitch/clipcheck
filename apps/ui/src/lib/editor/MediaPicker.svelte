<script lang="ts">
  import { tick, untrack } from 'svelte';
  import { buildListQuery, listRecordings, type RecordingDto } from '../api';
  import { formatBytes, formatDuration } from '../format';
  import { thumbUrl } from '../thumbs';

  interface Props {
    /** Whether the dialog is shown. */
    open: boolean;
    /** Allow picking several recordings (checkboxes). Single mode picks on click. */
    multiple?: boolean;
    /** Called with the picked recording ids, in the order they were checked. */
    onpick: (recordingIds: string[]) => void;
    /** Called when the dialog wants to close (Esc, Cancel, backdrop). */
    onclose: () => void;
  }

  let { open, multiple = true, onpick, onclose }: Props = $props();

  /** Rows fetched per request. */
  const PAGE_LIMIT = 200;
  /** Delay after typing before the search runs, ms. */
  const SEARCH_DEBOUNCE_MS = 200;

  let searchText = $state('');
  let gameFilter = $state<string | null>(null);
  /** Game names seen so far, keyed by game id, for the game filter. */
  let gameNames = $state<Record<string, string>>({});
  /** Rows of the current search. */
  let items = $state<RecordingDto[]>([]);
  let nextCursor = $state<string | null>(null);
  let loading = $state(false);
  let errorText = $state('');
  /** Thumbnails that failed to load (no poster written yet). */
  let brokenPoster = $state<Record<string, boolean>>({});
  /** Picked recording ids, for the checkbox state. */
  let chosen = $state<Record<string, boolean>>({});
  /**
   * Picked recording ids in selection order: each check appends, each uncheck removes. The clips
   * are added to the project in this order (F-UX-10), not in library order.
   */
  let pickOrder = $state<string[]>([]);

  let dialogEl = $state<HTMLDivElement | undefined>(undefined);
  let searchEl = $state<HTMLInputElement | undefined>(undefined);
  let requestSeq = 0;
  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  const pickedCount = $derived(pickOrder.length);

  /** Fetches one page. `append` adds to the current list instead of replacing it. */
  async function load(append: boolean): Promise<void> {
    const seq = ++requestSeq;
    loading = true;
    errorText = '';
    try {
      const page = await listRecordings(
        buildListQuery(
          { text: searchText, game: gameFilter, kind: null, favoritesOnly: false, sort: 'newest' },
          append ? nextCursor : null,
          PAGE_LIMIT,
        ),
      );
      if (seq !== requestSeq) return;
      const names = { ...gameNames };
      for (const r of page.items) {
        if (r.gameId && r.gameName) names[r.gameId] = r.gameName;
      }
      gameNames = names;
      items = append ? [...items, ...page.items] : page.items;
      nextCursor = page.nextCursor;
    } catch (e) {
      if (seq === requestSeq) errorText = `Could not load recordings: ${String(e)}`;
    } finally {
      if (seq === requestSeq) loading = false;
    }
  }

  /** Clears the list and loads the first page for the current filters. */
  function reload(): void {
    items = [];
    nextCursor = null;
    void load(false);
  }

  function onSearchInput(): void {
    if (searchTimer !== null) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      searchTimer = null;
      reload();
    }, SEARCH_DEBOUNCE_MS);
  }

  function onGameChange(value: string): void {
    gameFilter = value === '' ? null : value;
    reload();
  }

  function togglePick(row: RecordingDto): void {
    if (row.status !== 'ready') return;
    if (chosen[row.id] === true) {
      chosen = { ...chosen, [row.id]: false };
      pickOrder = pickOrder.filter((id) => id !== row.id);
    } else {
      chosen = { ...chosen, [row.id]: true };
      pickOrder = [...pickOrder, row.id];
    }
  }

  function pickSingle(row: RecordingDto): void {
    if (row.status !== 'ready') return;
    onpick([row.id]);
  }

  function confirmPick(): void {
    if (pickedCount === 0) return;
    onpick([...pickOrder]);
  }

  function markPosterBroken(id: string): void {
    brokenPoster = { ...brokenPoster, [id]: true };
  }

  function onWindowKey(e: KeyboardEvent): void {
    if (!open) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
      return;
    }
    if (e.key === 'Tab' && dialogEl) {
      // Keep focus inside the dialog: wrap from last to first and back.
      const focusable = Array.from(
        dialogEl.querySelectorAll<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), select:not([disabled])',
        ),
      );
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      const active = document.activeElement;
      if (e.shiftKey && (active === first || !dialogEl.contains(active))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (active === last || !dialogEl.contains(active))) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  function formatWhen(ms: number): string {
    return Number.isFinite(ms) && ms > 0 ? new Date(ms).toLocaleString() : '';
  }

  // Each time the dialog opens: reset the selection and load the first page. On close, focus
  // goes back to whatever had it before the dialog opened.
  $effect(() => {
    if (!open) return;
    const previous = document.activeElement;
    untrack(() => {
      searchText = '';
      gameFilter = null;
      brokenPoster = {};
      chosen = {};
      pickOrder = [];
      reload();
    });
    void tick().then(() => searchEl?.focus());
    return () => {
      if (searchTimer !== null) {
        clearTimeout(searchTimer);
        searchTimer = null;
      }
      if (previous instanceof HTMLElement) previous.focus();
    };
  });
</script>

<svelte:window onkeydown={onWindowKey} />

{#if open}
  <!-- Backdrop click closes the dialog; keyboard users close with Escape or the Close button. -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="backdrop" role="presentation" onclick={() => onclose()}>
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="media-picker-title"
      tabindex="-1"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
    >
      <header class="head">
        <h2 id="media-picker-title">Add clips from library</h2>
        <button type="button" class="ghost" aria-label="Close media picker" onclick={() => onclose()}>
          Close
        </button>
      </header>

      <div class="filters">
        <input
          type="search"
          class="search"
          placeholder="Search name, game or tag"
          aria-label="Search library recordings"
          bind:this={searchEl}
          bind:value={searchText}
          oninput={onSearchInput}
        />
        <select
          aria-label="Filter by game"
          value={gameFilter ?? ''}
          onchange={(e) => onGameChange(e.currentTarget.value)}
        >
          <option value="">All games</option>
          {#each Object.entries(gameNames).sort((a, b) => a[1].localeCompare(b[1])) as [gid, gname] (gid)}
            <option value={gid}>{gname}</option>
          {/each}
        </select>
      </div>

      {#if errorText}
        <p class="error" role="alert">{errorText}</p>
      {/if}

      <div class="list-wrap">
        {#if items.length === 0 && !loading && !errorText}
          <p class="empty">No recordings match this search.</p>
        {/if}

        <ul class="list" aria-label="Library recordings">
          {#each items as row (row.id)}
            <li class="row" class:disabled={row.status !== 'ready'} class:on={chosen[row.id] === true}>
              {#if multiple}
                <label class="row-label">
                  <input
                    type="checkbox"
                    checked={chosen[row.id] === true}
                    disabled={row.status !== 'ready'}
                    aria-label={`Select ${row.fileName}`}
                    onchange={() => togglePick(row)}
                  />
                  <span class="thumb" aria-hidden="true">
                    {#if !brokenPoster[row.id]}
                      <img
                        src={thumbUrl(row.id)}
                        alt=""
                        loading="lazy"
                        decoding="async"
                        onerror={() => markPosterBroken(row.id)}
                      />
                    {/if}
                  </span>
                  <span class="meta">
                    <span class="title">{row.fileName}</span>
                    <span class="sub">
                      {row.gameName ?? 'No game'} · {formatDuration(row.durationMs)} · {formatBytes(row.sizeBytes)} · {formatWhen(row.createdAtMs)}
                      {#if row.status !== 'ready'}· Processing{/if}
                    </span>
                  </span>
                </label>
              {:else}
                <button
                  type="button"
                  class="row-button"
                  disabled={row.status !== 'ready'}
                  aria-label={`Use ${row.fileName}`}
                  onclick={() => pickSingle(row)}
                >
                  <span class="thumb" aria-hidden="true">
                    {#if !brokenPoster[row.id]}
                      <img
                        src={thumbUrl(row.id)}
                        alt=""
                        loading="lazy"
                        decoding="async"
                        onerror={() => markPosterBroken(row.id)}
                      />
                    {/if}
                  </span>
                  <span class="meta">
                    <span class="title">{row.fileName}</span>
                    <span class="sub">
                      {row.gameName ?? 'No game'} · {formatDuration(row.durationMs)} · {formatBytes(row.sizeBytes)} · {formatWhen(row.createdAtMs)}
                      {#if row.status !== 'ready'}· Processing{/if}
                    </span>
                  </span>
                </button>
              {/if}
            </li>
          {/each}
        </ul>

        {#if loading}
          <p class="status" role="status">Loading...</p>
        {/if}

        {#if nextCursor && !loading}
          <div class="more">
            <button type="button" class="ghost" onclick={() => void load(true)}>Load more</button>
          </div>
        {/if}
      </div>

      {#if multiple}
        <footer class="foot">
          <span class="count" aria-live="polite">{pickedCount} selected</span>
          <div class="foot-actions">
            <button type="button" class="ghost" onclick={() => onclose()}>Cancel</button>
            <button type="button" class="primary" disabled={pickedCount === 0} onclick={confirmPick}>
              Add
            </button>
          </div>
        </footer>
      {/if}
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }

  .dialog {
    display: flex;
    flex-direction: column;
    width: min(760px, 100%);
    max-height: min(80vh, 760px);
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }

  .dialog:focus {
    outline: none;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }

  .filters {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
  }

  .search {
    flex: 1;
    min-width: 0;
    padding: 6px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  select {
    max-width: 200px;
    padding: 6px 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .list-wrap {
    flex: 1;
    min-height: 120px;
    overflow-y: auto;
    padding: 8px 16px;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .row {
    border-bottom: 1px solid var(--border);
  }

  .row.disabled {
    opacity: 0.5;
  }

  .row.on {
    background: var(--surface-2);
  }

  .row-label,
  .row-button {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 8px 4px;
    background: none;
    border: 0;
    text-align: left;
    cursor: pointer;
  }

  .row-label input[type='checkbox'] {
    margin: 0;
    width: 16px;
    height: 16px;
    flex: none;
  }

  .row-button:disabled,
  .row.disabled .row-label {
    cursor: not-allowed;
  }

  .row-button:focus-visible,
  .row-label:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .thumb {
    position: relative;
    flex: none;
    width: 96px;
    aspect-ratio: 16 / 9;
    border-radius: 4px;
    overflow: hidden;
    background: linear-gradient(135deg, #222838, #14171f);
  }

  .thumb img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 2px;
  }

  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sub {
    color: var(--text-dim);
    font-size: 12px;
  }

  .empty,
  .status {
    margin: 12px 4px;
    color: var(--text-dim);
  }

  .error {
    margin: 8px 16px 0;
    color: var(--danger);
  }

  .more {
    display: flex;
    justify-content: center;
    padding: 12px 0;
  }

  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
  }

  .count {
    color: var(--text-dim);
  }

  .foot-actions {
    display: flex;
    gap: 8px;
  }

  button {
    padding: 6px 14px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  button.ghost {
    background: none;
  }

  button:focus-visible,
  input:focus-visible,
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
