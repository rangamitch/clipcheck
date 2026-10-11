<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { exitConfirm, listenForExitConfirm } from './exitconfirm';

  /** Shown when the window is closed while a recording session is active. */
  const MESSAGE = 'A recording session is in progress. Exit ClipCheck and stop recording?';
  const TITLE_ID = 'exit-confirm-title';
  const MESSAGE_ID = 'exit-confirm-message';

  let cancelButton: HTMLButtonElement | undefined = $state();
  let exitButton: HTMLButtonElement | undefined = $state();
  /** Element that had focus before the dialog opened, restored when it closes. */
  let returnFocus: HTMLElement | null = null;

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenForExitConfirm()
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => {
        // Without the listener the dialog never opens; the window still closes normally.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  });

  $effect(() => {
    if ($exitConfirm.open) {
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      void tick().then(() => cancelButton?.focus());
    } else if (returnFocus) {
      const target = returnFocus;
      returnFocus = null;
      target.focus();
    }
  });

  function enabledButtons(): HTMLButtonElement[] {
    return [cancelButton, exitButton].filter(
      (b): b is HTMLButtonElement => b !== undefined && !b.disabled,
    );
  }

  /** Esc cancels. Tab and Shift+Tab stay inside the dialog. */
  function onKeydown(event: KeyboardEvent) {
    if (!$exitConfirm.open) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      exitConfirm.cancel();
      return;
    }
    if (event.key !== 'Tab') return;
    const buttons = enabledButtons();
    if (buttons.length === 0) return;
    const first = buttons[0];
    const last = buttons[buttons.length - 1];
    const active = document.activeElement;
    if (!buttons.includes(active as HTMLButtonElement)) {
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
</script>

<svelte:window onkeydown={onKeydown} />

{#if $exitConfirm.open}
  <div class="backdrop">
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby={TITLE_ID}
      aria-describedby={MESSAGE_ID}
    >
      <h2 id={TITLE_ID} class="title">Exit ClipCheck</h2>
      <p id={MESSAGE_ID} class="message">{MESSAGE}</p>
      <div class="actions">
        <button
          type="button"
          class="secondary"
          bind:this={cancelButton}
          disabled={$exitConfirm.busy}
          onclick={() => exitConfirm.cancel()}
        >
          Cancel
        </button>
        <button
          type="button"
          class="danger"
          bind:this={exitButton}
          disabled={$exitConfirm.busy}
          onclick={() => void exitConfirm.confirm()}
        >
          Exit and stop recording
        </button>
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
    max-width: 420px;
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

  .message {
    margin: 0 0 20px;
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
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

  .danger {
    background: var(--accent);
    color: #fff;
    border: 1px solid var(--accent);
  }

  .danger:hover:not(:disabled) {
    filter: brightness(1.1);
  }
</style>
