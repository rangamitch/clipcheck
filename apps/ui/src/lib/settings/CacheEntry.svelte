<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { formatBytes } from '../format';
  import { cacheInfo, clearCache, type AppCacheInfo } from './cache';

  /** How long the Clear cache button waits for a second click, in milliseconds. */
  const CONFIRM_MS = 4000;

  const TIP =
    'Deletes regenerable cache (audio extracts, thumbnails, waveforms) and old recording-buffer folders left by crashed sessions. Recordings and the live buffer are never touched.';

  /** Last known cache size. Null while loading and after a failed read. */
  let info = $state<AppCacheInfo | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  /** True after the first Clear cache click, for the length of CONFIRM_MS. */
  let confirming = $state(false);
  let clearError = $state<string | null>(null);
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;

  function messageOf(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /** Clear is possible only when the size is known, non-zero, and nothing is running. */
  const canClear = $derived(!loading && !busy && info !== null && info.totalBytes > 0);

  /** Status text next to the button. */
  const statusText = $derived.by((): string => {
    if (loading) return 'Cache: checking…';
    if (info === null) return 'Cache: size unavailable';
    return `Cache: ${formatBytes(info.totalBytes)}`;
  });

  /** Button label: idle, first click (confirm), or running. */
  const buttonLabel = $derived.by((): string => {
    if (busy) return 'Clearing…';
    if (confirming && info !== null) return `Click again to delete ${formatBytes(info.totalBytes)}`;
    return 'Clear cache';
  });

  /** Reads the cache size. Called on mount only; there is no polling. */
  async function refresh(): Promise<void> {
    loading = true;
    try {
      info = await cacheInfo();
    } catch {
      info = null;
    } finally {
      loading = false;
    }
  }

  /** Stops the inline confirmation of Clear cache. */
  function cancelConfirm(): void {
    confirming = false;
    if (confirmTimer !== null) {
      clearTimeout(confirmTimer);
      confirmTimer = null;
    }
  }

  function onClearClick(): void {
    if (!canClear) return;
    if (!confirming) {
      confirming = true;
      confirmTimer = setTimeout(cancelConfirm, CONFIRM_MS);
      return;
    }
    void doClear();
  }

  /** Deletes the cache and shows what is left. */
  async function doClear(): Promise<void> {
    cancelConfirm();
    busy = true;
    clearError = null;
    try {
      info = await clearCache();
    } catch (e) {
      clearError = `Could not clear the cache: ${messageOf(e)}`;
    } finally {
      busy = false;
    }
  }

  // A confirmation must not survive the button becoming unusable (for example while a clear runs).
  $effect(() => {
    if (!canClear && confirming) cancelConfirm();
  });

  onMount(() => {
    void refresh();
  });

  onDestroy(() => {
    cancelConfirm();
  });
</script>

<div class="cache-entry">
  <p class="dim status" aria-live="polite">{statusText}</p>
  <button
    type="button"
    class="clear"
    title={TIP}
    aria-label={buttonLabel}
    aria-describedby="cache-entry-tip"
    disabled={!canClear}
    onclick={onClearClick}
    onblur={cancelConfirm}
  >
    {buttonLabel}
  </button>
  <span id="cache-entry-tip" class="sr-only">{TIP}</span>
  {#if clearError !== null}
    <p class="message" role="alert">{clearError}</p>
  {/if}
</div>

<style>
  .cache-entry {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .dim {
    margin: 0;
    padding: 0 10px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .message {
    margin: 0;
    padding: 0 10px;
    font-size: 12px;
    color: var(--danger);
  }

  /* Same look as the neighbouring settings tab buttons (see Settings.svelte .tab). */
  .clear {
    width: 100%;
    box-sizing: border-box;
    text-align: left;
    padding: 8px 10px;
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-dim);
    font: inherit;
    cursor: pointer;
  }

  .clear:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border);
  }

  .clear:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .clear:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
