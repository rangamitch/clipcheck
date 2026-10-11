<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import { close, isMaximized, minimize, onMaximizeChange, toggleMaximize } from './window';
  import UpdatePill from './UpdatePill.svelte';

  /** Optional left-side content after the brand (the recorder state and quick actions). */
  let { leading, status }: { leading?: Snippet; status?: Snippet } = $props();

  let maximized = $state(false);

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void isMaximized()
      .then((value) => {
        if (!disposed) maximized = value;
      })
      .catch(() => {
        // Keep the restored icon if the state cannot be read.
      });

    void onMaximizeChange((value) => {
      if (!disposed) maximized = value;
    })
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => {
        // Without the listener the icon may go stale; the buttons still work.
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  });
</script>

<!--
  Double-clicking the drag region toggles maximize: Tauri handles this natively
  for elements carrying data-tauri-drag-region (needs allow-internal-toggle-maximize).
  Only the header and the brand/spacer carry the attribute, so the status slot and
  the window buttons never start a drag.
-->
<header class="titlebar" data-tauri-drag-region>
  <div class="brand" data-tauri-drag-region>
    <img src="/logo.svg" alt="" width="18" height="18" draggable="false" />
    <span class="wordmark">ClipCheck</span>
  </div>

  {#if leading}
    <div class="leading">{@render leading()}</div>
  {/if}

  <div class="drag-fill" data-tauri-drag-region></div>

  <div class="status">
    {#if status}{@render status()}{/if}
    <UpdatePill />
  </div>

  <div class="controls">
    <button
      type="button"
      class="wc"
      aria-label="Minimize"
      title="Minimize"
      onclick={() => void minimize()}
    >
      <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
        <path d="M0 5 H10" stroke="currentColor" stroke-width="1" fill="none" />
      </svg>
    </button>

    <button
      type="button"
      class="wc"
      aria-label={maximized ? 'Restore' : 'Maximize'}
      title={maximized ? 'Restore' : 'Maximize'}
      onclick={() => void toggleMaximize()}
    >
      {#if maximized}
        <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
          <path
            d="M2.5 0.5 H9.5 V7.5"
            stroke="currentColor"
            stroke-width="1"
            fill="none"
          />
          <rect x="0.5" y="2.5" width="7" height="7" stroke="currentColor" stroke-width="1" fill="none" />
        </svg>
      {:else}
        <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
          <rect x="0.5" y="0.5" width="9" height="9" stroke="currentColor" stroke-width="1" fill="none" />
        </svg>
      {/if}
    </button>

    <button
      type="button"
      class="wc close"
      aria-label="Close"
      title="Close"
      onclick={() => void close()}
    >
      <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
        <path d="M0 0 L10 10 M10 0 L0 10" stroke="currentColor" stroke-width="1" fill="none" />
      </svg>
    </button>
  </div>
</header>

<style>
  .titlebar {
    position: relative;
    z-index: 10;
    display: flex;
    align-items: stretch;
    flex: 0 0 36px;
    height: 36px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    color: var(--text);
    user-select: none;
    -webkit-user-select: none;
    cursor: default;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.02em;
  }

  .brand img {
    display: block;
    width: 18px;
    height: 18px;
    pointer-events: none;
  }

  /* Not a drag region: the chips and notices inside take the pointer, the gap to the right drags. */
  .leading {
    display: flex;
    align-items: center;
    flex: 0 1 auto;
    min-width: 0;
    padding: 0 8px 0 0;
  }

  .drag-fill {
    flex: 1 1 auto;
    min-width: 0;
  }

  @media (max-width: 560px) {
    .wordmark {
      display: none;
    }
  }

  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 8px;
  }

  .controls {
    display: flex;
    align-items: stretch;
    flex: 0 0 auto;
  }

  .wc {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 46px;
    height: 36px;
    margin: 0;
    padding: 0;
    border: none;
    border-radius: 0;
    background: transparent;
    color: var(--text);
    cursor: pointer;
  }

  .wc:hover {
    background: var(--surface-2);
  }

  .wc.close:hover {
    background: #e81123;
    color: #fff;
  }

  .wc:active {
    filter: brightness(0.9);
  }

  .wc:focus {
    outline: none;
  }

  .wc:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
</style>
