<script lang="ts" module>
  import { toast } from './store';

  /**
   * Shows a message. The store skips equal values, so the message is first cleared and then set
   * again in a microtask: the display re-arms its timer even when the same text repeats.
   */
  export function showToast(msg: string): void {
    toast.set(null);
    queueMicrotask(() => toast.set(msg));
  }
</script>

<script lang="ts">
  /** How long a normal message stays, in milliseconds. */
  const INFO_MS = 4000;
  /** How long a message that reports a problem stays, in milliseconds. */
  const ERROR_MS = 6000;

  /**
   * Messages that report a problem (case-insensitive word match). They stay longer so people can
   * read them. Plain status text such as "Saved" uses the short time.
   */
  const ERROR_PATTERN = /failed|error|locked|cannot|not /i;

  function isErrorMessage(message: string): boolean {
    return ERROR_PATTERN.test(message);
  }

  /** Display time for a message, in milliseconds. */
  function durationFor(message: string): number {
    return isErrorMessage(message) ? ERROR_MS : INFO_MS;
  }

  const message = $derived($toast);

  // Each new message starts its own timer; the cleanup cancels the previous one, so a
  // replacement restarts the countdown. When the timer fires the store is cleared.
  $effect(() => {
    const current = $toast;
    if (current === null) return;
    const id = setTimeout(() => {
      toast.set(null);
    }, durationFor(current));
    return () => clearTimeout(id);
  });

  function dismiss(): void {
    toast.set(null);
  }
</script>

<div class="host" role="status" aria-live="polite">
  {#if message !== null}
    <div class="toast" class:error={isErrorMessage(message)}>
      <span class="text">{message}</span>
      <button type="button" class="close" aria-label="Dismiss message" onclick={dismiss}>
        Close
      </button>
    </div>
  {/if}
</div>

<style>
  .host {
    position: fixed;
    left: 50%;
    bottom: 24px;
    transform: translateX(-50%);
    z-index: 1100;
    width: max-content;
    max-width: min(560px, calc(100vw - 32px));
    pointer-events: none;
  }

  .toast {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 10px 12px 10px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text);
    font-size: 13px;
    line-height: 1.4;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    pointer-events: auto;
  }

  .toast.error {
    border-left: 3px solid var(--danger);
  }

  .text {
    flex: 1 1 auto;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .close {
    flex: 0 0 auto;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: none;
    color: var(--text-dim);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .close:hover {
    color: var(--text);
    border-color: var(--accent);
  }

  .close:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
