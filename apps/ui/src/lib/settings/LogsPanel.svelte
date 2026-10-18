<script lang="ts">
  import { tick } from 'svelte';
  import {
    collectDiagnostics,
    copyText,
    exportDiagnostics,
    friendlyError,
    getLogDir,
    openFolder,
    parentDir,
  } from './logs';

  /** Two-way bound. Settings sets it to true from the "Logs & diagnostics" button. */
  let { open = $bindable(false) }: { open?: boolean } = $props();

  type Job = 'logs' | 'copy' | 'save' | 'reveal';
  const TITLE_ID = 'logs-panel-title';
  const TEXT_ID = 'logs-panel-text';
  const INTRO =
    'Collects the recent ClipCheck log and system info for bug reports. Your Windows user name is hidden.';

  let busy = $state<Job | null>(null);
  let error = $state<string | null>(null);
  let copied = $state(false);
  let savedPath = $state<string | null>(null);
  let dialog: HTMLDivElement | undefined = $state();
  let firstButton: HTMLButtonElement | undefined = $state();
  /** Element that had focus before the dialog opened, restored when it closes. */
  let returnFocus: HTMLElement | null = null;

  $effect(() => {
    if (open) {
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      void tick().then(() => firstButton?.focus());
    } else if (returnFocus) {
      const target = returnFocus;
      returnFocus = null;
      target.focus();
    }
  });

  function close(): void {
    open = false;
    error = null;
    copied = false;
  }

  /** Runs one action at a time. Errors show inline; every button is disabled while busy. */
  async function run(job: Job, action: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = job;
    error = null;
    try {
      await action();
    } catch (e) {
      error = friendlyError(e);
    } finally {
      busy = null;
    }
  }

  function openLogs(): Promise<void> {
    return run('logs', async () => {
      await openFolder(await getLogDir());
    });
  }

  function copyDiagnostics(): Promise<void> {
    return run('copy', async () => {
      copied = false;
      await copyText(await collectDiagnostics());
      copied = true;
    });
  }

  function saveDiagnostics(): Promise<void> {
    return run('save', async () => {
      copied = false;
      savedPath = await exportDiagnostics();
    });
  }

  function showSavedFolder(): Promise<void> {
    return run('reveal', async () => {
      const path = savedPath;
      const dir = path ? parentDir(path) : '';
      if (!dir) throw new Error('The folder for that file could not be found.');
      await openFolder(dir);
    });
  }

  /** Esc closes. Tab and Shift+Tab stay inside the dialog. */
  function onKeydown(event: KeyboardEvent): void {
    if (!open) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      close();
      return;
    }
    if (event.key !== 'Tab' || !dialog) return;
    const buttons = Array.from(dialog.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'));
    if (buttons.length === 0) return;
    const first = buttons[0];
    const last = buttons[buttons.length - 1];
    const active = document.activeElement;
    if (!dialog.contains(active)) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  function onBackdropClick(event: MouseEvent): void {
    if (event.target === event.currentTarget) close();
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <div class="backdrop" role="presentation" onclick={onBackdropClick}>
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby={TITLE_ID}
      aria-describedby={TEXT_ID}
      bind:this={dialog}
    >
      <h2 id={TITLE_ID} class="title">Logs &amp; diagnostics</h2>
      <p id={TEXT_ID} class="intro">{INTRO}</p>

      <div class="actions">
        <button
          type="button"
          class="action"
          bind:this={firstButton}
          disabled={busy !== null}
          onclick={() => void openLogs()}
        >
          {busy === 'logs' ? 'Opening…' : 'Open logs folder'}
        </button>
        <button
          type="button"
          class="action"
          disabled={busy !== null}
          onclick={() => void copyDiagnostics()}
        >
          {busy === 'copy' ? 'Copying…' : 'Copy diagnostics'}
        </button>
        <button
          type="button"
          class="action"
          disabled={busy !== null}
          onclick={() => void saveDiagnostics()}
        >
          {busy === 'save' ? 'Saving…' : 'Save to file'}
        </button>
      </div>

      <div class="feedback" aria-live="polite">
        {#if copied}
          <p class="ok">Copied</p>
        {/if}
        {#if savedPath}
          <p class="saved">
            Saved to <span class="path">{savedPath}</span>
          </p>
          <button type="button" class="link" disabled={busy !== null} onclick={() => void showSavedFolder()}>
            {busy === 'reveal' ? 'Opening…' : 'Show in folder'}
          </button>
        {/if}
      </div>

      {#if error}
        <p class="error" role="alert">{error}</p>
      {/if}

      <div class="footer">
        <button type="button" class="secondary" onclick={close}>Close</button>
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
    max-width: 440px;
    box-sizing: border-box;
    padding: 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5);
  }

  .title {
    margin: 0 0 8px;
    font-size: 16px;
    font-weight: 600;
  }

  .intro {
    margin: 0 0 16px;
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .action,
  .secondary,
  .link {
    font: inherit;
    font-size: 13px;
    border-radius: var(--radius);
    cursor: pointer;
  }

  .action,
  .secondary {
    padding: 6px 12px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
  }

  .link {
    margin-top: 6px;
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    text-decoration: underline;
  }

  button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .feedback {
    margin-top: 12px;
    font-size: 13px;
  }

  .feedback p {
    margin: 0;
  }

  .ok {
    color: var(--text);
  }

  .saved {
    color: var(--text-dim);
    word-break: break-all;
  }

  .path {
    font-family: ui-monospace, Consolas, monospace;
    color: var(--text);
  }

  .error {
    margin: 12px 0 0;
    font-size: 13px;
    color: var(--accent);
  }

  .footer {
    display: flex;
    justify-content: flex-end;
    margin-top: 16px;
  }
</style>
