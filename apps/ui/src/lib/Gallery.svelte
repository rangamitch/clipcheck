<script lang="ts">
  import { onMount } from 'svelte';
  import {
    chunkPages,
    createLatestLoader,
    refetchTarget,
    shouldRenderPage,
    wantsMorePages,
  } from './gallerywindow';
  import {
    DEFAULT_PAGE_SIZE,
    buildListQuery,
    getGameIcons,
    getLibraryStats,
    listRecordings,
    type GalleryFilters,
    type RecordingDto,
    type RecordingKind,
    type SortKey,
    type StatsDto,
  } from './api';
  import { openRecording } from './nav';
  import { warmRecording } from './player/warm';
  import { debounce } from './debounce';
  import { formatBytes, formatDuration, formatRelativeDate } from './format';
  import { thumbUrl } from './thumbs';
  import AppIcon from './settings/fields/AppIcon.svelte';
  import RecordingMenu from './RecordingMenu.svelte';

  /** Result of a rename or delete made from a card's menu. */
  type MenuChange =
    | { kind: 'renamed'; id: string; name: string }
    | { kind: 'deleted'; id: string }
    | { kind: 'favorite'; id: string; value: boolean };

  /** The part of a RecordingMenu instance the gallery calls. */
  type MenuHandle = { showAt(x: number, y: number): void };

  const SORT_OPTIONS: { value: SortKey; label: string }[] = [
    { value: 'newest', label: 'Newest' },
    { value: 'oldest', label: 'Oldest' },
    { value: 'name_asc', label: 'Name A-Z' },
    { value: 'name_desc', label: 'Name Z-A' },
    { value: 'largest', label: 'Largest' },
    { value: 'smallest', label: 'Smallest' },
    { value: 'longest', label: 'Longest' },
  ];

  const KIND_OPTIONS: { value: RecordingKind | null; label: string }[] = [
    { value: null, label: 'All' },
    { value: 'clip', label: 'Clips' },
    { value: 'session', label: 'Sessions' },
  ];

  const SEARCH_DEBOUNCE_MS = 250;

  let text = $state('');
  let game = $state<string | null>(null);
  let kind = $state<RecordingKind | null>(null);
  let favoritesOnly = $state(false);
  let sort = $state<SortKey>('newest');

  /** The scroll container; the observers use it as their root. */
  let shellEl: HTMLDivElement | undefined = $state();

  let items = $state<RecordingDto[]>([]);
  let nextCursor = $state<string | null>(null);
  let loading = $state(false);
  let loadedOnce = $state(false);
  let error = $state<string | null>(null);
  let stats = $state<StatsDto | null>(null);

  /** Games seen in any loaded result, keyed by id. Accumulates so filtering does not drop options. */
  let knownGames = $state<Map<string, string>>(new Map());

  /** Cached game icons (base64 PNG) keyed by game id. */
  let gameIcons = $state<Record<string, string>>({});
  /** Game ids already requested for icons (not reactive; avoids re-asking on every page). */
  const requestedIcons = new Set<string>();

  /** Recording ids whose poster request failed (404 before the poster is written). Reset per listing. */
  let brokenPosters = $state<Record<string, true>>({});

  /** Per-card menus by recording id, for right-click (not reactive). */
  const menuRefs: Record<string, MenuHandle | null | undefined> = {};

  const gameOptions = $derived(
    [...knownGames.entries()]
      .map(([id, name]) => ({ id, name }))
      .sort((a, b) => a.name.localeCompare(b.name)),
  );

  const filtersActive = $derived(
    text.trim().length > 0 || game !== null || kind !== null || favoritesOnly,
  );

  /** Items split into pages of DEFAULT_PAGE_SIZE; each page is one windowed section. */
  const pages = $derived(chunkPages(items, DEFAULT_PAGE_SIZE));

  /** Pages currently near the viewport (set by the IntersectionObserver). Replaced, never mutated. */
  let visible = $state(new Set<number>());
  /** Last measured height of each page, in px. Replaced, never mutated. */
  let heights = $state(new Map<number, number>());
  /** Without IntersectionObserver every page renders, as before windowing. */
  const canWindow = typeof IntersectionObserver !== 'undefined';
  /** Page section element -> its page index. Plain, not reactive. */
  const pageNodes = new Map<Element, number>();
  let pageObserver: IntersectionObserver | null = null;
  let sizeObserver: ResizeObserver | null = null;

  let requestSeq = 0;
  /** Latest-wins stats fetch. A failed call keeps the previous stats. */
  const refreshStats = createLatestLoader(
    () => getLibraryStats(),
    (s) => {
      stats = s;
    },
  );
  /** Search input restarts the listing once typing pauses. */
  const searchDebounce = debounce(() => reload(), SEARCH_DEBOUNCE_MS);

  function currentFilters(): GalleryFilters {
    return { text, game, kind, favoritesOnly, sort };
  }

  function absorbGames(list: RecordingDto[]): void {
    let changed = false;
    const next = new Map(knownGames);
    for (const r of list) {
      if (r.gameId && r.gameName && !next.has(r.gameId)) {
        next.set(r.gameId, r.gameName);
        changed = true;
      }
    }
    if (changed) {
      knownGames = next;
    }
    loadIcons(list);
  }

  /** Requests icons for game ids not yet fetched. Fire-and-forget; failures leave letter tiles. */
  function loadIcons(list: RecordingDto[]): void {
    const fresh: string[] = [];
    for (const r of list) {
      const id = r.gameId;
      if (id && !(id in gameIcons) && !requestedIcons.has(id)) {
        requestedIcons.add(id);
        fresh.push(id);
      }
    }
    if (fresh.length === 0) return;
    void getGameIcons(fresh).then((found) => {
      // Ids without an icon yet may get one later (the daemon writes it when the game runs).
      for (const id of fresh) if (!(id in found)) requestedIcons.delete(id);
      if (Object.keys(found).length > 0) {
        gameIcons = { ...gameIcons, ...found };
      }
    });
  }

  /** True while a load-more page is in flight (not reactive; only gates refresh). */
  let loadMoreInFlight = false;
  /** A refresh asked for while a load-more was in flight; runs once that page lands. */
  let refreshQueued = false;

  /**
   * Fetches one page. A null cursor starts over; a cursor appends to the current list.
   * A `quiet` fetch (a background refresh once the list has loaded) does not raise `loading`,
   * so the list, the empty message and the Load more button stay put while it runs.
   */
  async function fetchPage(cursor: string | null, quiet = false): Promise<void> {
    const seq = ++requestSeq;
    if (!quiet) loading = true;
    error = null;
    // A fresh first page may find posters written since the last listing, so retry them.
    if (cursor === null) brokenPosters = {};
    try {
      const page = await listRecordings(
        buildListQuery(currentFilters(), cursor, DEFAULT_PAGE_SIZE),
      );
      if (seq !== requestSeq) return;
      // Replaced in one assignment, only once the page is in hand, so nothing blanks out.
      // An append pushes onto the existing list rather than copying it.
      if (cursor === null) {
        items = page.items;
      } else {
        items.push(...page.items);
      }
      nextCursor = page.nextCursor;
      absorbGames(page.items);
    } catch (e) {
      if (seq !== requestSeq) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (seq === requestSeq) {
        loading = false;
        loadedOnce = true;
      }
    }
  }

  /** Restarts the listing from the first page with the current filters. */
  function reload(): void {
    visible = new Set();
    heights = new Map();
    items = [];
    nextCursor = null;
    void fetchPage(null);
  }

  /**
   * Re-lists the same number of items that are loaded now (at least one page), so pages the
   * user loaded with Load more stay on screen and the scroll position holds. Pages are fetched
   * in order until that many items are in hand or the listing ends, then swapped in at once.
   * Unlike `fetchPage(null)`, the current list is not cleared first.
   */
  async function refetchLoaded(): Promise<void> {
    const seq = ++requestSeq;
    const target = refetchTarget(items.length, DEFAULT_PAGE_SIZE);
    const quiet = loadedOnce;
    if (!quiet) loading = true;
    error = null;
    // A fresh listing may find posters written since the last one, so retry them.
    brokenPosters = {};
    try {
      let collected: RecordingDto[] = [];
      let cursor: string | null = null;
      do {
        const page = await listRecordings(
          buildListQuery(currentFilters(), cursor, DEFAULT_PAGE_SIZE),
        );
        if (seq !== requestSeq) return;
        collected = [...collected, ...page.items];
        cursor = page.nextCursor;
        absorbGames(page.items);
      } while (wantsMorePages(collected.length, target, cursor));
      items = collected;
      nextCursor = cursor;
    } catch (e) {
      if (seq !== requestSeq) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (seq === requestSeq) {
        loading = false;
        loadedOnce = true;
      }
    }
  }

  function loadMore(): void {
    if (nextCursor !== null && !loading) {
      loadMoreInFlight = true;
      void fetchPage(nextCursor).finally(() => {
        loadMoreInFlight = false;
        if (refreshQueued) {
          refreshQueued = false;
          void refetchLoaded();
        }
      });
    }
  }

  /**
   * Re-lists the current filters, keeping every page already loaded on screen until the
   * new items arrive (see `refetchLoaded`). Called by the app when a clip or session part is
   * written, and by its poll while the gallery is empty. If a load-more is in flight, the
   * refresh waits for that page so it is not thrown away.
   */
  export function refresh(): void {
    if (loadMoreInFlight) {
      refreshQueued = true;
    } else {
      void refetchLoaded();
    }
    void refreshStats();
  }

  /** True when no recording is listed. The app polls this while the library may still be filling. */
  export function isEmpty(): boolean {
    return items.length === 0;
  }

  function markPosterBroken(id: string): void {
    brokenPosters = { ...brokenPosters, [id]: true };
  }

  /** Right-click on a card opens that card's menu at the pointer. */
  function onContextMenu(e: MouseEvent, id: string): void {
    e.preventDefault();
    menuRefs[id]?.showAt(e.clientX, e.clientY);
  }

  /** Keeps the list in step with a rename or delete made from a card's menu. */
  function onMenuChanged(c: MenuChange): void {
    if (c.kind === 'deleted') {
      items = items.filter((r) => r.id !== c.id);
      void refreshStats();
    } else if (c.kind === 'favorite') {
      items = items.map((r) => (r.id === c.id ? { ...r, isFavorite: c.value } : r));
    } else {
      // `c.name` is the stem only; put the extension back so the card keeps it until a refresh.
      items = items.map((r) => {
        if (r.id !== c.id) return r;
        const fileName = r.ext ? `${c.name}.${r.ext}` : c.name;
        return { ...r, fileName };
      });
    }
  }

  function onSearchInput(value: string): void {
    text = value;
    searchDebounce.call();
  }

  function onSortChange(value: SortKey): void {
    sort = value;
    reload();
  }

  function onGameChange(value: string): void {
    game = value === '' ? null : value;
    reload();
  }

  function onKindChange(value: RecordingKind | null): void {
    kind = value;
    reload();
  }

  function onFavoritesToggle(): void {
    favoritesOnly = !favoritesOnly;
    reload();
  }

  function clearFilters(): void {
    text = '';
    game = null;
    kind = null;
    favoritesOnly = false;
    reload();
  }

  /** Records the height of each rendered page. Pages with no height (hidden, or 0 px) are skipped. */
  function recordHeights(targets: Element[]): void {
    let next: Map<number, number> | null = null;
    for (const el of targets) {
      const i = pageNodes.get(el);
      if (i === undefined) continue;
      const h = (el as HTMLElement).offsetHeight;
      if (h <= 0 || (next ?? heights).get(i) === h) continue;
      next ??= new Map(heights);
      next.set(i, h);
    }
    if (next) heights = next;
  }

  /** Tracks which pages are near the viewport, and measures the visible ones. */
  function onPagesIntersect(entries: IntersectionObserverEntry[]): void {
    let next: Set<number> | null = null;
    for (const e of entries) {
      const i = pageNodes.get(e.target);
      if (i === undefined) continue;
      const has = (next ?? visible).has(i);
      if (e.isIntersecting === has) continue;
      next ??= new Set(visible);
      if (e.isIntersecting) next.add(i);
      else next.delete(i);
    }
    recordHeights(entries.filter((e) => e.isIntersecting).map((e) => e.target));
    if (next) visible = next;
  }

  /** Observes a page section. Its index may change when the page list shifts. */
  function observePage(node: HTMLElement, index: number) {
    pageNodes.set(node, index);
    pageObserver?.observe(node);
    sizeObserver?.observe(node);
    return {
      update(next: number) {
        pageNodes.set(node, next);
      },
      destroy() {
        pageObserver?.unobserve(node);
        sizeObserver?.unobserve(node);
        pageNodes.delete(node);
      },
    };
  }

  onMount(() => {
    if (canWindow) {
      pageObserver = new IntersectionObserver(onPagesIntersect, {
        root: shellEl ?? null,
        rootMargin: '1200px 0px',
      });
      // A section changes size when its page is swapped for a spacer or grows (the last page on
      // append). Re-measure any rendered page then.
      if (typeof ResizeObserver !== 'undefined') {
        sizeObserver = new ResizeObserver((entries) =>
          recordHeights(entries.map((e) => e.target)),
        );
      }
      for (const node of pageNodes.keys()) {
        pageObserver.observe(node);
        sizeObserver?.observe(node);
      }
    }
    reload();
    // Stats are decorative; the listing reports real errors.
    void refreshStats();
    return () => {
      searchDebounce.cancel();
      pageObserver?.disconnect();
      sizeObserver?.disconnect();
      pageObserver = null;
      sizeObserver = null;
    };
  });
</script>

<div class="shell" bind:this={shellEl}>
  <header class="topbar">
    <input
      class="search"
      type="search"
      placeholder="Search name, game or tag"
      aria-label="Search recordings"
      value={text}
      oninput={(e) => onSearchInput(e.currentTarget.value)}
    />
    <select
      class="control"
      aria-label="Sort"
      value={sort}
      onchange={(e) => onSortChange(e.currentTarget.value as SortKey)}
    >
      {#each SORT_OPTIONS as opt (opt.value)}
        <option value={opt.value}>{opt.label}</option>
      {/each}
    </select>
    <select
      class="control"
      aria-label="Game"
      value={game ?? ''}
      onchange={(e) => onGameChange(e.currentTarget.value)}
    >
      <option value="">All games</option>
      {#each gameOptions as g (g.id)}
        <option value={g.id}>{g.name}</option>
      {/each}
    </select>
    <div class="segmented" role="group" aria-label="Kind">
      {#each KIND_OPTIONS as opt (opt.label)}
        <button
          type="button"
          aria-pressed={kind === opt.value}
          class:active={kind === opt.value}
          onclick={() => onKindChange(opt.value)}
        >
          {opt.label}
        </button>
      {/each}
    </div>
    <button
      type="button"
      class="control toggle"
      aria-pressed={favoritesOnly}
      class:active={favoritesOnly}
      onclick={onFavoritesToggle}
    >
      Favorites
    </button>
  </header>

  <div class="subbar">
    {#if stats}
      <span>
        {stats.recordings} recordings ({stats.clips} clips, {stats.sessions} sessions),
        {formatBytes(stats.totalSizeBytes)}
      </span>
    {:else}
      <span>Library</span>
    {/if}
    {#if filtersActive}
      <button type="button" class="link" onclick={clearFilters}>Clear filters</button>
    {/if}
  </div>

  <main>
    {#if error}
      <p class="notice error" role="alert">Could not load recordings: {error}</p>
    {/if}

    {#if items.length > 0}
      {#each pages as page, i (i)}
        <section class="page" data-page={i} use:observePage={i}>
          {#if !canWindow || shouldRenderPage(i, visible, heights)}
            <ul class="grid">
              {#each page as r (r.id)}
                {@const ready = r.status === 'ready'}
                <li class="card" oncontextmenu={(e) => onContextMenu(e, r.id)}>
                  <button
                    type="button"
                    class="open"
                    disabled={!ready}
                    onpointerenter={() => ready && warmRecording(r.id)}
                    onfocus={() => ready && warmRecording(r.id)}
                    onclick={() => openRecording(r)}
                  >
                    <span class="thumb" aria-hidden="true">
                      {#if ready && !brokenPosters[r.id]}
                        <img
                          class="poster"
                          src={thumbUrl(r.id)}
                          alt=""
                          loading="lazy"
                          decoding="async"
                          onerror={() => markPosterBroken(r.id)}
                        />
                      {/if}
                      <span class="badge kind">{r.kind === 'session' ? 'Session' : 'Clip'}</span>
                      {#if r.isFavorite}
                        <span class="star" title="Favorite">&#9733;</span>
                      {/if}
                      <span class="badge duration">{formatDuration(r.durationMs)}</span>
                    </span>
                    <span class="meta">
                      <span class="name" title={r.fileName}>{r.fileName}</span>
                      {#if r.gameId}
                        <span class="sub game-row">
                          <AppIcon
                            exe={r.gameId}
                            name={r.gameName ?? ''}
                            iconB64={gameIcons[r.gameId] ?? null}
                            size={16}
                          />
                          <span class="game-name">{r.gameName ?? ''}</span>
                        </span>
                      {:else}
                        <span class="sub">No game</span>
                      {/if}
                      <span class="sub">
                        {formatBytes(r.sizeBytes)} &middot; {formatRelativeDate(r.createdAtMs)}
                      </span>
                    </span>
                  </button>
                  <div class="menu-slot">
                    <RecordingMenu
                      id={r.id}
                      fileName={r.fileName}
                      {ready}
                      isFavorite={r.isFavorite}
                      onchanged={onMenuChanged}
                      bind:this={menuRefs[r.id]}
                    />
                  </div>
                </li>
              {/each}
            </ul>
          {:else}
            <div class="spacer" style="height: {heights.get(i)}px" aria-hidden="true"></div>
          {/if}
        </section>
      {/each}
    {:else if loadedOnce && !loading && !error}
      <div class="empty">
        {#if filtersActive}
          <p>No recordings match these filters.</p>
        {:else}
          <p>No recordings yet.</p>
          <p class="dim">Clips you save and sessions you record will appear here.</p>
        {/if}
      </div>
    {/if}

    {#if loading}
      <p class="dim center">Loading...</p>
    {/if}

    {#if nextCursor !== null && !loading}
      <div class="more">
        <button type="button" class="control" onclick={loadMore}>Load more</button>
      </div>
    {/if}
  </main>
</div>

<style>
  .shell {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }

  .topbar {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    padding: 12px 16px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }

  .search {
    flex: 1 1 220px;
    min-width: 0;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .control {
    padding: 7px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
  }

  .control:hover,
  .segmented button:hover,
  .link:hover {
    border-color: var(--accent);
  }

  .segmented {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }

  .segmented button {
    padding: 7px 12px;
    background: var(--surface-2);
    border: none;
    cursor: pointer;
  }

  .segmented button + button {
    border-left: 1px solid var(--border);
  }

  .active {
    background: var(--accent) !important;
    color: #fff !important;
    border-color: var(--accent) !important;
  }

  .toggle[aria-pressed='false'] {
    color: var(--text-dim);
  }

  .subbar {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 16px;
    color: var(--text-dim);
    font-size: 13px;
  }

  .link {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
  }

  main {
    padding: 0 16px 24px;
  }

  /* Padding (not margin) keeps the 14px row gap inside the measured height of each page. */
  .page {
    display: block;
    padding-bottom: 14px;
  }

  .spacer {
    width: 100%;
  }

  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 14px;
  }

  .card {
    position: relative;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
    min-width: 0;
  }

  .card:hover,
  .card:focus-within {
    border-color: var(--accent);
    cursor: pointer;
  }

  /* The whole card is one button so keyboard and screen reader users can open it. */
  .open {
    display: block;
    width: 100%;
    margin: 0;
    padding: 0;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .open:disabled {
    cursor: default;
  }

  .open:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .thumb {
    position: relative;
    display: block;
    aspect-ratio: 16 / 9;
    overflow: hidden;
    background: linear-gradient(135deg, #222838, #14171f);
  }

  .poster {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .badge {
    position: absolute;
    z-index: 1;
    bottom: 6px;
    right: 6px;
    padding: 2px 6px;
    font-size: 12px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.7);
  }

  .badge.kind {
    top: 6px;
    left: 6px;
    bottom: auto;
    right: auto;
    color: var(--text-dim);
  }

  .star {
    position: absolute;
    z-index: 1;
    bottom: 4px;
    left: 8px;
    color: #ffd166;
    font-size: 16px;
    text-shadow: 0 0 3px rgba(0, 0, 0, 0.8);
  }

  /* Menu button sits over the card's top-right corner; shown on hover, focus and touch. */
  .menu-slot {
    position: absolute;
    z-index: 2;
    top: 6px;
    right: 6px;
    opacity: 0;
    transition: opacity 0.12s;
  }

  .card:hover .menu-slot,
  .card:focus-within .menu-slot {
    opacity: 1;
  }

  @media (hover: none), (max-width: 600px) {
    .menu-slot {
      opacity: 1;
    }
  }

  .meta {
    display: block;
    padding: 8px 10px 10px;
    min-width: 0;
  }

  .name {
    display: block;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .sub {
    display: block;
    margin-top: 2px;
    color: var(--text-dim);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .game-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }

  .game-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .empty {
    padding: 48px 0;
    text-align: center;
  }

  .empty p {
    margin: 4px 0;
  }

  .notice {
    padding: 10px 12px;
    border-radius: var(--radius);
    background: var(--surface-2);
  }

  .error {
    color: var(--danger);
    border: 1px solid var(--danger);
  }

  .dim {
    color: var(--text-dim);
  }

  .center {
    text-align: center;
  }

  .more {
    display: flex;
    justify-content: center;
    padding: 16px 0;
  }

  @media (max-width: 600px) {
    .topbar {
      padding: 10px 12px;
    }
    main {
      padding: 0 12px 20px;
    }
  }
</style>
