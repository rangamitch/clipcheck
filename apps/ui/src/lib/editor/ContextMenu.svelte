<script module lang="ts">
  /** One row of a context menu. A separator row ignores every other field. */
  export interface MenuItem {
    label: string;
    action?: () => void;
    disabled?: boolean;
    separator?: boolean;
    /** Display text of the keyboard shortcut, such as "Ctrl+C". */
    shortcut?: string;
  }
</script>

<script lang="ts">
  import { onMount, tick } from 'svelte';

  interface Props {
    /** Viewport x of the pointer (or anchor) where the menu opens. */
    x: number;
    /** Viewport y of the pointer (or anchor) where the menu opens. */
    y: number;
    items: MenuItem[];
    /** Called when the menu should close: after an action, on outside click, Escape, scroll, or blur. */
    onclose: () => void;
  }

  let { x, y, items, onclose }: Props = $props();

  /** Keeps the menu this far from the viewport edges, in px. */
  const EDGE = 8;

  let menuEl: HTMLDivElement | undefined = $state();
  let left = $state(0);
  let top = $state(0);
  let ready = $state(false);

  /** Element that had focus before the menu opened; focus returns there on close. */
  let returnFocusEl: HTMLElement | null = null;

  /** Enabled menu item buttons in display order. */
  function enabledItems(): HTMLButtonElement[] {
    if (!menuEl) return [];
    return Array.from(
      menuEl.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([aria-disabled="true"])'),
    );
  }

  /**
   * Places the menu at (x, y). It flips to the left or upward when it would overflow the
   * viewport, then is clamped inside the viewport as a last resort.
   */
  function place(): void {
    if (!menuEl) return;
    const w = menuEl.offsetWidth;
    const h = menuEl.offsetHeight;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let nl = x;
    if (nl + w > vw - EDGE) nl = x - w;
    nl = Math.max(EDGE, Math.min(nl, vw - w - EDGE));
    let nt = y;
    if (nt + h > vh - EDGE) nt = y - h;
    nt = Math.max(EDGE, Math.min(nt, vh - h - EDGE));
    left = nl;
    top = nt;
    ready = true;
  }

  // Re-place when the anchor point changes while the menu is open.
  $effect(() => {
    void x;
    void y;
    void items;
    if (menuEl) void tick().then(place);
  });

  function runItem(item: MenuItem): void {
    if (item.separator || item.disabled) return;
    item.action?.();
    onclose();
  }

  function onMenuKeydown(e: KeyboardEvent): void {
    e.stopPropagation();
    const list = enabledItems();
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
        onclose();
        break;
      case 'Tab':
        e.preventDefault();
        onclose();
        break;
    }
  }

  onMount(() => {
    returnFocusEl = document.activeElement as HTMLElement | null;
    void tick().then(() => {
      place();
      enabledItems()[0]?.focus();
    });

    const onPointerDown = (e: PointerEvent): void => {
      if (menuEl?.contains(e.target as Node)) return;
      onclose();
    };
    const onDismiss = (): void => onclose();
    const onContextMenu = (e: MouseEvent): void => {
      // A right click elsewhere dismisses this menu; the new one opens in its place.
      if (menuEl?.contains(e.target as Node)) return;
      onclose();
    };
    const onResize = (): void => place();

    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('contextmenu', onContextMenu, true);
    window.addEventListener('scroll', onDismiss, true);
    window.addEventListener('blur', onDismiss);
    window.addEventListener('resize', onResize);

    return () => {
      window.removeEventListener('pointerdown', onPointerDown, true);
      window.removeEventListener('contextmenu', onContextMenu, true);
      window.removeEventListener('scroll', onDismiss, true);
      window.removeEventListener('blur', onDismiss);
      window.removeEventListener('resize', onResize);
      if (returnFocusEl && returnFocusEl.isConnected) returnFocusEl.focus();
    };
  });
</script>

<div
  bind:this={menuEl}
  class="menu"
  role="menu"
  tabindex="-1"
  aria-orientation="vertical"
  style:left="{left}px"
  style:top="{top}px"
  style:visibility={ready ? 'visible' : 'hidden'}
  onkeydown={onMenuKeydown}
  onpointerdown={(e) => e.stopPropagation()}
>
  {#each items as item, i (i)}
    {#if item.separator}
      <div class="separator" role="separator"></div>
    {:else}
      <button
        type="button"
        role="menuitem"
        class="item"
        aria-disabled={item.disabled ? 'true' : undefined}
        tabindex="-1"
        onclick={() => runItem(item)}
      >
        <span class="label">{item.label}</span>
        {#if item.shortcut}
          <kbd class="shortcut">{item.shortcut}</kbd>
        {/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 1000;
    min-width: 180px;
    max-width: calc(100vw - 16px);
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    outline: none;
  }

  .item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
    padding: 6px 10px;
    border: none;
    border-radius: 6px;
    background: none;
    color: inherit;
    font: inherit;
    font-size: 13px;
    text-align: left;
    cursor: pointer;
  }

  .item:hover:not([aria-disabled='true']),
  .item:focus-visible {
    background: var(--surface);
    outline: none;
    color: var(--text);
    box-shadow: inset 2px 0 0 var(--accent);
  }

  .item[aria-disabled='true'] {
    color: var(--text-dim);
    opacity: 0.45;
    cursor: default;
  }

  .shortcut {
    font-family: inherit;
    font-size: 12px;
    color: var(--text-dim);
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  .separator {
    height: 1px;
    margin: 4px 6px;
    background: var(--border);
  }
</style>
