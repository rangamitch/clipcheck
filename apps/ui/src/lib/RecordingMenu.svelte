<script module lang="ts">
  /** Source of unique element ids for the dialogs of each menu instance. */
  let sequence = 0;
  function nextUid(): string {
    sequence += 1;
    return `rec-menu-${sequence}`;
  }
</script>

<script lang="ts">
  import { tick } from 'svelte';
  import {
    deleteRecording,
    editInEditor,
    renameRecording,
    revealRecording,
    setFavorite,
  } from './recordingActions';

  type Change =
    | { kind: 'renamed'; id: string; name: string }
    | { kind: 'deleted'; id: string }
    | { kind: 'favorite'; id: string; value: boolean };
  type Dialog = 'rename' | 'delete' | null;
  type Anchor = { left: number; right: number; top: number; bottom: number };

  interface Props {
    id: string;
    fileName: string;
    ready: boolean;
    onchanged?: (change: Change) => void;
    showButton?: boolean;
    /** Current favourite flag; the menu shows Add to or Remove from favourites. */
    isFavorite?: boolean;
  }

  let { id, fileName, ready, onchanged, showButton = true, isFavorite = false }: Props = $props();

  const uid = nextUid();
  const EDGE = 8;
  const GAP = 4;

  let menuOpen = $state(false);
  let menuEl: HTMLDivElement | undefined = $state();
  let buttonEl: HTMLButtonElement | undefined = $state();
  let pos = $state({ left: 0, top: 0 });

  let dialog = $state<Dialog>(null);
  let dialogEl: HTMLDivElement | undefined = $state();
  let renameInput: HTMLInputElement | undefined = $state();
  let cancelButton: HTMLButtonElement | undefined = $state();
  let renameValue = $state('');
  let busy = $state(false);
  let errorText = $state('');

  /** Where focus returns when the menu or dialog closes. */
  let returnFocusEl: HTMLElement | null = null;
  /** Anchor rectangle and alignment for placing the open menu. */
  let placement: { anchor: Anchor; alignRight: boolean } | null = null;

  const names = $derived(splitName(fileName));

  function splitName(name: string): { stem: string; ext: string } {
    const dot = name.lastIndexOf('.');
    return dot > 0 ? { stem: name.slice(0, dot), ext: name.slice(dot) } : { stem: name, ext: '' };
  }

  function messageOf(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /** Moves the node to document.body so card overflow and transforms cannot clip it. */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      },
    };
  }

  function stop(e: Event) {
    e.stopPropagation();
  }

  function items(): HTMLButtonElement[] {
    if (!menuEl) return [];
    return Array.from(menuEl.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled])'));
  }

  function focusReturn() {
    if (returnFocusEl && returnFocusEl.isConnected) returnFocusEl.focus();
  }

  /** Positions the menu under its anchor, right-aligned or left-aligned, flipping above when needed. */
  function place() {
    if (!menuEl || !placement) return;
    const { anchor, alignRight } = placement;
    const w = menuEl.offsetWidth;
    const h = menuEl.offsetHeight;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let left = alignRight ? anchor.right - w : anchor.left;
    left = Math.max(EDGE, Math.min(left, vw - w - EDGE));
    let top = anchor.bottom + GAP;
    if (top + h > vh - EDGE) {
      const above = anchor.top - h - GAP;
      top = above >= EDGE ? above : Math.max(EDGE, vh - h - EDGE);
    }
    pos = { left, top };
  }

  function openMenu(anchor: Anchor, alignRight: boolean, returnTo: HTMLElement | null) {
    dialog = null;
    returnFocusEl = returnTo ?? (document.activeElement as HTMLElement | null);
    placement = { anchor, alignRight };
    menuOpen = true;
    void tick().then(() => {
      place();
      items()[0]?.focus();
    });
  }

  function closeMenu(restoreFocus: boolean) {
    if (!menuOpen) return;
    menuOpen = false;
    if (restoreFocus) focusReturn();
  }

  /** Opens the menu from the kebab button, or closes it when already open. */
  function onButtonClick(e: MouseEvent) {
    e.stopPropagation();
    if (menuOpen) {
      closeMenu(true);
      return;
    }
    if (!buttonEl) return;
    const r = buttonEl.getBoundingClientRect();
    openMenu({ left: r.left, right: r.right, top: r.top, bottom: r.bottom }, true, buttonEl);
  }

  function onButtonKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'ArrowDown' && !menuOpen && buttonEl) {
      e.preventDefault();
      const r = buttonEl.getBoundingClientRect();
      openMenu({ left: r.left, right: r.right, top: r.top, bottom: r.bottom }, true, buttonEl);
    }
  }

  /** Opens the same popup at a point, for right-click context menus. */
  export function showAt(x: number, y: number): void {
    if (dialog) return;
    if (menuOpen) closeMenu(false);
    openMenu({ left: x, right: x, top: y, bottom: y }, false, null);
  }

  function onMenuKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    const list = items();
    if (list.length === 0) return;
    const idx = list.indexOf(document.activeElement as HTMLButtonElement);
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        list[idx < 0 ? 0 : (idx + 1) % list.length].focus();
        break;
      case 'ArrowUp':
        e.preventDefault();
        list[idx < 0 ? list.length - 1 : (idx - 1 + list.length) % list.length].focus();
        break;
      case 'Home':
        e.preventDefault();
        list[0].focus();
        break;
      case 'End':
        e.preventDefault();
        list[list.length - 1].focus();
        break;
      case 'Escape':
        e.preventDefault();
        closeMenu(true);
        break;
      case 'Tab':
        closeMenu(false);
        break;
    }
  }

  function onMenuFocusOut() {
    // Deferred so a focus move inside the menu (or a click that focuses an item) is not
    // mistaken for leaving it.
    setTimeout(() => {
      if (menuOpen && !menuEl?.contains(document.activeElement)) closeMenu(false);
    }, 0);
  }

  // Close on outside pointer down, scroll, resize, and window blur while the menu is open.
  $effect(() => {
    if (!menuOpen) return;
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (menuEl?.contains(target) || buttonEl?.contains(target)) return;
      closeMenu(false);
    };
    const onDismiss = () => closeMenu(false);
    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('scroll', onDismiss, true);
    window.addEventListener('resize', onDismiss);
    window.addEventListener('blur', onDismiss);
    return () => {
      window.removeEventListener('pointerdown', onPointerDown, true);
      window.removeEventListener('scroll', onDismiss, true);
      window.removeEventListener('resize', onDismiss);
      window.removeEventListener('blur', onDismiss);
    };
  });

  async function openDialog(kind: Exclude<Dialog, null>) {
    dialog = kind;
    busy = false;
    errorText = '';
    if (kind === 'rename') renameValue = names.stem;
    await tick();
    if (kind === 'rename') {
      renameInput?.focus();
      renameInput?.select();
    } else {
      cancelButton?.focus();
    }
  }

  function closeDialog() {
    dialog = null;
    busy = false;
    errorText = '';
    focusReturn();
  }

  function onRenameClick() {
    if (!ready) return;
    closeMenu(false);
    void openDialog('rename');
  }

  function onDeleteClick() {
    if (!ready) return;
    closeMenu(false);
    void openDialog('delete');
  }

  function onFavoriteClick() {
    const value = !isFavorite;
    closeMenu(false);
    setFavorite(id, value).then(
      () => onchanged?.({ kind: 'favorite', id, value }),
      (e: unknown) => console.error('Could not change the favourite flag', messageOf(e)),
    );
  }

  function onEditClick() {
    if (!ready) return;
    closeMenu(false);
    editInEditor(id);
  }

  function onRevealClick() {
    closeMenu(false);
    revealRecording(id).catch((e: unknown) => {
      console.error('Could not open the file location', messageOf(e));
    });
  }

  async function submitRename() {
    if (busy) return;
    const name = renameValue.trim();
    if (name.length === 0) {
      errorText = 'Enter a name.';
      return;
    }
    if (name === names.stem) {
      closeDialog();
      return;
    }
    busy = true;
    errorText = '';
    try {
      await renameRecording(id, name);
      onchanged?.({ kind: 'renamed', id, name });
      closeDialog();
    } catch (e) {
      errorText = messageOf(e);
      busy = false;
    }
  }

  async function confirmDelete() {
    if (busy) return;
    busy = true;
    errorText = '';
    try {
      await deleteRecording(id);
      onchanged?.({ kind: 'deleted', id });
      closeDialog();
    } catch (e) {
      errorText = messageOf(e);
      busy = false;
    }
  }

  function onOverlayClick(e: MouseEvent) {
    e.stopPropagation();
    if (e.target === e.currentTarget && !busy) closeDialog();
  }

  function onDialogKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') {
      e.preventDefault();
      if (!busy) closeDialog();
      return;
    }
    if (e.key === 'Tab' && dialogEl) {
      const focusable = Array.from(
        dialogEl.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled])'),
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
    }
  }
</script>

{#if showButton}
  <button
    bind:this={buttonEl}
    type="button"
    class="rec-menu-button"
    aria-label="Recording options"
    aria-haspopup="menu"
    aria-expanded={menuOpen}
    onclick={onButtonClick}
    onkeydown={onButtonKeydown}
    ondblclick={stop}
  >
    <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
      <circle cx="8" cy="3" r="1.5" fill="currentColor" />
      <circle cx="8" cy="8" r="1.5" fill="currentColor" />
      <circle cx="8" cy="13" r="1.5" fill="currentColor" />
    </svg>
  </button>
{/if}

{#if menuOpen}
  <div
    bind:this={menuEl}
    use:portal
    class="rec-menu"
    role="menu"
    aria-label="Recording options"
    tabindex="-1"
    style="left: {pos.left}px; top: {pos.top}px;"
    onkeydown={onMenuKeydown}
    onfocusout={onMenuFocusOut}
    onclick={stop}
    ondblclick={stop}
  >
    <button type="button" role="menuitem" tabindex="-1" disabled={!ready} aria-disabled={!ready} onclick={onRenameClick}>
      Rename
    </button>
    <button type="button" role="menuitem" tabindex="-1" disabled={!ready} aria-disabled={!ready} onclick={onFavoriteClick}>
      {isFavorite ? 'Remove from favourites' : 'Add to favourites'}
    </button>
    <button type="button" role="menuitem" tabindex="-1" disabled={!ready} aria-disabled={!ready} onclick={onEditClick}>
      Edit in video editor
    </button>
    <button type="button" role="menuitem" tabindex="-1" onclick={onRevealClick}>
      Open file location
    </button>
    <div class="rec-menu-separator" role="separator"></div>
    <button
      type="button"
      role="menuitem"
      tabindex="-1"
      class="rec-menu-danger"
      disabled={!ready}
      aria-disabled={!ready}
      onclick={onDeleteClick}
    >
      Delete
    </button>
  </div>
{/if}

{#if dialog === 'rename'}
  <div class="rec-overlay" role="presentation" use:portal onclick={onOverlayClick} onkeydown={onDialogKeydown}>
    <div
      bind:this={dialogEl}
      class="rec-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="{uid}-title"
      aria-describedby={errorText ? `${uid}-error` : undefined}
    >
      <form
        onsubmit={(e) => {
          e.preventDefault();
          void submitRename();
        }}
      >
        <h2 id="{uid}-title">Rename recording</h2>
        <label for="{uid}-name">Name</label>
        <input
          id="{uid}-name"
          type="text"
          autocomplete="off"
          spellcheck="false"
          bind:this={renameInput}
          bind:value={renameValue}
          disabled={busy}
          aria-invalid={errorText ? 'true' : undefined}
        />
        {#if names.ext}
          <p class="rec-hint">The {names.ext} extension is kept.</p>
        {/if}
        {#if errorText}
          <p class="rec-error" id="{uid}-error" role="alert">{errorText}</p>
        {/if}
        <div class="rec-actions">
          <button type="button" class="rec-btn" onclick={closeDialog} disabled={busy}>Cancel</button>
          <button type="submit" class="rec-btn rec-primary" disabled={busy}>
            {busy ? 'Renaming...' : 'Rename'}
          </button>
        </div>
      </form>
    </div>
  </div>
{/if}

{#if dialog === 'delete'}
  <div class="rec-overlay" role="presentation" use:portal onclick={onOverlayClick} onkeydown={onDialogKeydown}>
    <div
      bind:this={dialogEl}
      class="rec-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="{uid}-title"
      aria-describedby="{uid}-message"
    >
      <h2 id="{uid}-title">Delete recording?</h2>
      <p id="{uid}-message" class="rec-message">Move {fileName} to the Recycle Bin?</p>
      {#if errorText}
        <p class="rec-error" role="alert">{errorText}</p>
      {/if}
      <div class="rec-actions">
        <button type="button" class="rec-btn" bind:this={cancelButton} onclick={closeDialog} disabled={busy}>
          Cancel
        </button>
        <button type="button" class="rec-btn rec-danger" onclick={confirmDelete} disabled={busy}>
          {busy ? 'Deleting...' : 'Delete'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .rec-menu-button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--radius);
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .rec-menu-button:hover,
  .rec-menu-button[aria-expanded='true'] {
    background: var(--surface-2);
    color: var(--text);
    border-color: var(--border);
  }
  .rec-menu-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .rec-menu {
    position: fixed;
    z-index: 1000;
    min-width: 180px;
    padding: 4px;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  }
  .rec-menu [role='menuitem'] {
    display: block;
    width: 100%;
    padding: 8px 12px;
    border: none;
    border-radius: calc(var(--radius) - 2px);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 13px;
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
  }
  .rec-menu [role='menuitem']:hover:not([disabled]),
  .rec-menu [role='menuitem']:focus-visible {
    background: var(--surface-2);
    outline: none;
  }
  .rec-menu [role='menuitem']:disabled {
    color: var(--text-dim);
    opacity: 0.5;
    cursor: default;
  }
  .rec-menu-separator {
    height: 1px;
    margin: 4px 0;
    background: var(--border);
  }
  .rec-menu .rec-menu-danger:not(:disabled) {
    color: var(--danger);
  }
  .rec-menu .rec-menu-danger:not(:disabled):hover,
  .rec-menu .rec-menu-danger:not(:disabled):focus-visible {
    background: color-mix(in srgb, var(--danger) 14%, transparent);
  }

  .rec-overlay {
    position: fixed;
    inset: 0;
    z-index: 1100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.55);
  }
  .rec-dialog {
    width: min(420px, 100%);
    padding: 20px;
    box-sizing: border-box;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
    box-shadow: 0 16px 40px rgba(0, 0, 0, 0.5);
  }
  .rec-dialog h2 {
    margin: 0 0 12px;
    font-size: 16px;
    font-weight: 600;
  }
  .rec-dialog label {
    display: block;
    margin-bottom: 6px;
    font-size: 12px;
    color: var(--text-dim);
  }
  .rec-dialog input {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
    font: inherit;
    font-size: 14px;
  }
  .rec-dialog input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 0;
  }
  .rec-dialog input[aria-invalid='true'] {
    border-color: var(--accent);
  }
  .rec-message,
  .rec-hint {
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--text-dim);
    overflow-wrap: anywhere;
  }
  .rec-message {
    color: var(--text);
  }
  .rec-error {
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--accent);
  }
  .rec-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .rec-btn {
    padding: 7px 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .rec-btn:hover:not([disabled]) {
    border-color: var(--text-dim);
  }
  .rec-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .rec-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .rec-primary,
  .rec-danger {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
</style>
