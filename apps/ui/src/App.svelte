<script lang="ts">
  import { onMount } from 'svelte';
  import Gallery from './lib/Gallery.svelte';
  import RecorderBar from './lib/topbar/RecorderBar.svelte';
  import Settings from './lib/Settings.svelte';
  import TitleBar from './lib/TitleBar.svelte';
  import { debounce } from './lib/debounce';
  import { daemonConnected, onConnection, onEvent } from './lib/daemon';
  import { refreshesGallery } from './lib/gallerysync';
  import { startLiveStatus } from './lib/status';
  import { VIEWS, activeView, setView, playerTarget, playerFileName, closePlayer, type View } from './lib/nav';
  import Player from './lib/Player.svelte';
  import ExitConfirm from './lib/ExitConfirm.svelte';
  import UpdateModal from './lib/UpdateModal.svelte';

  const LABELS: Record<View, string> = {
    gallery: 'Gallery',
    settings: 'Settings',
  };

  /** Debounce for gallery reloads after a clip or session part is written or a library scan changes rows. */
  const LIBRARY_REFRESH_MS = 500;

  /**
   * One-shot retries after mount. The gallery's first load can run before the daemon has
   * created or migrated the library database, or before its startup scan has registered any
   * rows. Tauri events emitted before this window has listeners are not replayed, so these
   * timers are the fallback. A retry runs only while the gallery shows nothing, or when the
   * daemon was not connected at mount.
   */
  const RETRY_DELAYS_MS = [1500, 5000];

  /**
   * Light poll for a library scan that finishes after the one-shot retries. It refreshes the
   * gallery every POLL_MS while the gallery is empty and the window is visible, for at most
   * POLL_WINDOW_MS after mount, and stops as soon as the gallery has items.
   */
  const POLL_MS = 4000;
  const POLL_WINDOW_MS = 120_000;

  /** `isEmpty` is optional: without it the retry always runs, which is harmless. */
  let gallery: { refresh: () => void; isEmpty?: () => boolean } | undefined = $state();

  onMount(() => {
    const stopStatus = startLiveStatus();
    const refreshSoon = debounce(() => gallery?.refresh(), LIBRARY_REFRESH_MS);
    let active = true;
    /** Null until the mount-time connection check answers. */
    let connectedAtMount: boolean | null = null;

    const stopEvents = onEvent((ev) => {
      if (refreshesGallery(ev)) {
        refreshSoon.call();
      }
    });
    // `daemon-connection` is emitted once per connect. A connect that happened before this
    // listener existed is picked up by the daemonConnected() check below.
    const stopConnection = onConnection((connected) => {
      if (connected) refreshSoon.call();
    });
    void daemonConnected().then((connected) => {
      if (!active) return;
      connectedAtMount = connected;
      if (connected) refreshSoon.call();
    });

    const galleryEmpty = (): boolean => gallery?.isEmpty?.() ?? true;
    const retryTimers = RETRY_DELAYS_MS.map((ms) =>
      setTimeout(() => {
        if (!active) return;
        if (connectedAtMount !== true || galleryEmpty()) {
          gallery?.refresh();
        }
      }, ms),
    );

    const pollStartedAt = Date.now();
    const pollTimer = setInterval(() => {
      if (!active) return;
      if (Date.now() - pollStartedAt >= POLL_WINDOW_MS) {
        clearInterval(pollTimer);
        return;
      }
      // Gallery not shown (Settings view): wait. Items present: done.
      const empty = gallery?.isEmpty?.();
      if (empty === false) {
        clearInterval(pollTimer);
        return;
      }
      if (empty === true && document.visibilityState === 'visible') {
        gallery?.refresh();
      }
    }, POLL_MS);

    return () => {
      active = false;
      for (const t of retryTimers) clearTimeout(t);
      clearInterval(pollTimer);
      stopConnection();
      stopEvents();
      stopStatus();
      refreshSoon.cancel();
    };
  });
</script>

<div class="window">
  <TitleBar>
    {#snippet leading()}
      <RecorderBar />
    {/snippet}
  </TitleBar>

  <div class="body">
    <nav class="rail" aria-label="Main">
      {#each VIEWS as view (view)}
        <button
          type="button"
          class="nav-item"
          aria-current={$activeView === view ? 'page' : undefined}
          class:active={$activeView === view}
          onclick={() => setView(view)}
        >
          {LABELS[view]}
        </button>
      {/each}
    </nav>

    <section class="content">
      {#if $playerTarget !== null}
        {#key $playerTarget}
          <Player id={$playerTarget} fileName={$playerFileName ?? undefined} onclose={closePlayer} />
        {/key}
      {:else if $activeView === 'gallery'}
        <div class="gallery-view">
          <Gallery bind:this={gallery} />
        </div>
      {:else}
        <Settings />
      {/if}
    </section>
  </div>
</div>

<ExitConfirm />
<UpdateModal />

<style>
  .window {
    display: flex;
    flex-direction: column;
    height: 100%;
    box-sizing: border-box;
    border: 1px solid var(--border);
    background: var(--bg);
    overflow: hidden;
  }

  .body {
    display: flex;
    flex: 1 1 auto;
    min-height: 0;
  }

  .rail {
    flex: 0 0 120px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px;
    background: var(--surface);
    border-right: 1px solid var(--border);
    user-select: none;
  }

  .nav-item {
    padding: 8px 10px;
    text-align: left;
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-dim);
    cursor: pointer;
  }

  .nav-item:hover {
    color: var(--text);
    border-color: var(--border);
  }

  .nav-item.active {
    color: var(--text);
    background: var(--surface-2);
    border-color: var(--accent);
  }

  .nav-item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .content {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .content > :global(*) {
    flex: 1 1 auto;
    min-height: 0;
  }

  /* The Gallery's own scroll container fills the view; the top bar sits above the whole window. */
  .gallery-view {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .gallery-view > :global(.shell) {
    flex: 1 1 auto;
    min-height: 0;
  }
</style>
