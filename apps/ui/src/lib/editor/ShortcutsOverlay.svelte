<script lang="ts">
  import { tick } from 'svelte';
  import { SHORTCUT_HELP, type ShortcutGroup, type ShortcutHelp } from './shortcuts';

  interface Props {
    open: boolean;
    onclose: () => void;
  }

  let { open, onclose }: Props = $props();

  /** Group display order. Rows keep the order they have in SHORTCUT_HELP. */
  const GROUP_ORDER: readonly ShortcutGroup[] = ['Playback', 'Editing', 'Tools', 'Timeline', 'Project'];

  /** Rows grouped under their heading, in display order. Empty groups are left out. */
  const groups = GROUP_ORDER.map((name) => ({
    name,
    rows: SHORTCUT_HELP.filter((row: ShortcutHelp) => row.group === name),
  })).filter((g) => g.rows.length > 0);

  let dialogEl: HTMLDivElement | undefined = $state();
  let closeButton: HTMLButtonElement | undefined = $state();
  /** Element that had focus before the overlay opened. */
  let returnFocusEl: HTMLElement | null = null;

  // On open: remember focus and move it into the dialog. On close: give focus back.
  $effect(() => {
    if (!open) return;
    returnFocusEl = document.activeElement as HTMLElement | null;
    void tick().then(() => closeButton?.focus());
    return () => {
      if (returnFocusEl && returnFocusEl.isConnected) returnFocusEl.focus();
      returnFocusEl = null;
    };
  });

  /** Escape closes. Tab stays inside the dialog. Captured on window so editor keys do not fire. */
  $effect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onclose();
        return;
      }
      if (e.key === 'Tab' && dialogEl) {
        const focusable = Array.from(
          dialogEl.querySelectorAll<HTMLElement>('button:not([disabled])'),
        );
        if (focusable.length === 0) return;
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        const active = document.activeElement;
        const inside = active instanceof Node && dialogEl.contains(active);
        if (e.shiftKey && (active === first || !inside)) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && (active === last || !inside)) {
          e.preventDefault();
          first.focus();
        }
        return;
      }
      // Other keys belong to the dialog only; editor shortcuts must not run underneath it.
      e.stopPropagation();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });

  /** A click on the dimmed backdrop (not on the dialog) closes. */
  function onBackdropClick(e: MouseEvent): void {
    if (e.target === e.currentTarget) onclose();
  }
</script>

{#if open}
  <div class="backdrop" role="presentation" onclick={onBackdropClick}>
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="shortcuts-title"
      bind:this={dialogEl}
    >
      <header class="head">
        <h2 id="shortcuts-title">Keyboard shortcuts</h2>
        <button
          type="button"
          class="close"
          aria-label="Close keyboard shortcuts"
          bind:this={closeButton}
          onclick={onclose}>Close</button
        >
      </header>

      <div class="columns">
        {#each groups as group (group.name)}
          <section class="group" aria-labelledby="shortcuts-group-{group.name}">
            <h3 id="shortcuts-group-{group.name}">{group.name}</h3>
            <dl>
              {#each group.rows as row (row.action)}
                <div class="row">
                  <dt><kbd>{row.keys}</kbd></dt>
                  <dd>{row.label}</dd>
                </div>
              {/each}
            </dl>
          </section>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 900;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.55);
  }

  .dialog {
    width: min(760px, 100%);
    max-height: calc(100vh - 32px);
    overflow-y: auto;
    padding: 18px 20px 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 16px 40px rgba(0, 0, 0, 0.5);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
  }

  h2 {
    margin: 0;
    font-size: 16px;
  }

  h3 {
    margin: 14px 0 6px;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-dim);
  }

  .columns {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0 28px;
  }

  @media (max-width: 640px) {
    .columns {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  dl {
    margin: 0;
  }

  .row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    padding: 4px 0;
    border-bottom: 1px solid var(--border);
    font-size: 13px;
  }

  dt {
    flex: 0 0 auto;
  }

  dd {
    margin: 0;
    text-align: right;
    color: var(--text-dim);
  }

  kbd {
    display: inline-block;
    padding: 1px 6px;
    font-family: inherit;
    font-size: 12px;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 4px;
    white-space: nowrap;
  }

  .close {
    padding: 6px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }

  .close:hover {
    border-color: var(--accent);
  }

  .close:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
