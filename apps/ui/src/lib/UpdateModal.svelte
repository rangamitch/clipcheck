<script lang="ts">
  import { tick } from 'svelte';
  import { formatBytes } from './format';
  import {
    closeUpdateModal,
    installNow,
    skipVersion,
    updateStore,
  } from './updates';

  const TITLE_ID = 'update-modal-title';
  const NOTES_ID = 'update-modal-notes';

  /** Shown when there is an update and the user opened it from the pill. */
  const visible = $derived($updateStore.modalOpen && $updateStore.available !== null);
  const info = $derived($updateStore.available);
  const error = $derived($updateStore.error);
  const installing = $derived($updateStore.installing);
  const progress = $derived($updateStore.progress);
  /** A recording blocks a normal install; the user can end it and update, or close. */
  const blocked = $derived(error?.kind === 'recording');

  let primaryButton: HTMLButtonElement | undefined = $state();
  /** Element that had focus before the dialog opened, restored when it closes. */
  let returnFocus: HTMLElement | null = null;

  $effect(() => {
    if (visible) {
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      void tick().then(() => primaryButton?.focus());
    } else if (returnFocus) {
      const target = returnFocus;
      returnFocus = null;
      target.focus();
    }
  });

  /** Escape closes, except while an install is running (the app exits when it succeeds). */
  function onKeydown(event: KeyboardEvent) {
    if (!visible) return;
    if (event.key === 'Escape' && !installing) {
      event.preventDefault();
      closeUpdateModal();
    }
  }

  function percent(p: { downloaded: number; total: number | null }): number | null {
    if (p.total === null || p.total <= 0) return null;
    return Math.min(100, Math.round((p.downloaded / p.total) * 100));
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if visible && info}
  <div class="backdrop">
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby={TITLE_ID}
      aria-describedby={NOTES_ID}
    >
      <h2 id={TITLE_ID} class="title">Update to {info.version}</h2>
      <p class="current">You have version {info.current_version}.</p>

      <pre id={NOTES_ID} class="notes">{info.notes.trim() || 'No release notes.'}</pre>

      {#if error}
        <p class="error" role="alert">{error.message}</p>
      {/if}

      {#if installing || progress}
        <div class="progress-wrap">
          {#if progress}
            {@const pct = percent(progress)}
            <progress
              class="progress"
              max={pct === null ? undefined : 100}
              value={pct ?? undefined}
              aria-label="Update download progress"
            ></progress>
            <p class="progress-text">
              {#if progress.total !== null}
                {formatBytes(progress.downloaded)} of {formatBytes(progress.total)}
              {:else}
                {formatBytes(progress.downloaded)} downloaded
              {/if}
            </p>
          {:else}
            <progress class="progress" aria-label="Preparing update"></progress>
            <p class="progress-text">Preparing update...</p>
          {/if}
        </div>
      {/if}

      <div class="actions">
        {#if blocked}
          <button type="button" class="secondary" disabled={installing} onclick={closeUpdateModal}>
            Close
          </button>
          <button
            type="button"
            class="secondary"
            disabled={installing}
            onclick={() => void installNow()}
          >
            Try again
          </button>
          <button
            type="button"
            class="primary"
            bind:this={primaryButton}
            disabled={installing}
            onclick={() => void installNow(true)}
          >
            Stop recording and update
          </button>
        {:else}
          <button
            type="button"
            class="secondary"
            disabled={installing}
            onclick={skipVersion}
          >
            Skip this version
          </button>
          <button
            type="button"
            class="secondary"
            disabled={installing}
            onclick={closeUpdateModal}
          >
            Later
          </button>
          <button
            type="button"
            class="primary"
            bind:this={primaryButton}
            disabled={installing}
            onclick={() => void installNow()}
          >
            {error ? 'Try again' : 'Update now'}
          </button>
        {/if}
      </div>
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
    width: 100%;
    max-width: 480px;
    max-height: 90vh;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    padding: 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5);
  }

  .title {
    margin: 0 0 4px;
    font-size: 16px;
    font-weight: 600;
  }

  .current {
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .notes {
    margin: 0 0 16px;
    padding: 10px;
    max-height: 240px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    font: inherit;
    font-size: 13px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .error {
    margin: 0 0 16px;
    font-size: 13px;
    color: var(--danger);
  }

  .progress-wrap {
    margin: 0 0 16px;
  }

  .progress {
    display: block;
    width: 100%;
    height: 8px;
  }

  .progress-text {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--text-dim);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 8px;
  }

  button {
    padding: 8px 14px;
    border-radius: var(--radius);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .secondary {
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
  }

  .secondary:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .primary {
    background: var(--accent);
    color: #fff;
    border: 1px solid var(--accent);
  }

  .primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }
</style>
