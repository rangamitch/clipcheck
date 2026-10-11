<script lang="ts" module>
  /** One entry in the picker. The list comes from the daemon, never from typed text. */
  export interface PickerApp {
    exe: string;
    name: string;
    iconB64?: string | null;
    /** Small secondary line. Defaults to the exe name. */
    hint?: string;
    suggested?: boolean;
    /** Currently playing audio. */
    active?: boolean;
    /** Shown but cannot be picked. */
    disabled?: boolean;
    /** Short note shown on the row, such as "Already detected automatically". */
    badge?: string;
  }

  let pickerCount = 0;
</script>

<script lang="ts">
  import AppIcon from './AppIcon.svelte';
  import { tick } from 'svelte';

  interface Props {
    apps: PickerApp[];
    /** Exes (case-insensitive) that are already in the list and must not be offered again. */
    exclude?: string[];
    placeholder?: string;
    emptyText?: string;
    /** Show a "Suggested" group first. */
    groups?: boolean;
    busy?: boolean;
    onPick: (app: PickerApp) => void;
    onRefresh?: () => void;
  }

  let {
    apps,
    exclude = [],
    placeholder = 'Add app…',
    emptyText = 'Nothing found yet — start the app and it will appear here',
    groups = true,
    busy = false,
    onPick,
    onRefresh,
  }: Props = $props();

  const uid = `app-picker-${++pickerCount}`;
  const optionId = (i: number): string => `${uid}-opt-${i}`;

  let open = $state(false);
  let query = $state('');
  let active = $state(0);
  let root: HTMLDivElement | undefined = $state();
  let searchEl: HTMLInputElement | undefined = $state();
  let triggerEl: HTMLButtonElement | undefined = $state();

  const excluded = $derived(new Set(exclude.map((e) => e.toLowerCase())));
  const needle = $derived(query.trim().toLowerCase());

  interface Item {
    app: PickerApp;
    index: number;
  }
  interface Section {
    label: string | null;
    items: Item[];
  }

  const sections = $derived.by((): Section[] => {
    const matches = apps.filter((a) => {
      if (excluded.has(a.exe.toLowerCase())) return false;
      if (needle === '') return true;
      return a.name.toLowerCase().includes(needle) || a.exe.toLowerCase().includes(needle);
    });
    const suggested = groups ? matches.filter((a) => a.suggested) : [];
    const rest = groups ? matches.filter((a) => !a.suggested) : matches;
    const plan: { label: string | null; apps: PickerApp[] }[] =
      suggested.length > 0
        ? [
            { label: 'Suggested', apps: suggested },
            { label: 'Other apps', apps: rest },
          ]
        : [{ label: null, apps: rest }];
    let index = 0;
    return plan
      .filter((s) => s.apps.length > 0)
      .map((s) => ({ label: s.label, items: s.apps.map((app) => ({ app, index: index++ })) }));
  });

  const flat = $derived(sections.flatMap((s) => s.items.map((i) => i.app)));
  const activeIndex = $derived(Math.min(active, Math.max(0, flat.length - 1)));

  async function openPicker(): Promise<void> {
    open = true;
    query = '';
    active = 0;
    await tick();
    searchEl?.focus();
  }

  function close(returnFocus = false): void {
    open = false;
    query = '';
    if (returnFocus) triggerEl?.focus();
  }

  function toggle(): void {
    if (open) close();
    else void openPicker();
  }

  function pick(app: PickerApp): void {
    if (app.disabled) return;
    onPick(app);
    close(true);
  }

  function onKey(e: KeyboardEvent): void {
    const n = flat.length;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        if (n) active = (activeIndex + 1) % n;
        break;
      case 'ArrowUp':
        e.preventDefault();
        if (n) active = (activeIndex - 1 + n) % n;
        break;
      case 'Home':
        e.preventDefault();
        active = 0;
        break;
      case 'End':
        e.preventDefault();
        active = Math.max(0, n - 1);
        break;
      case 'Enter': {
        e.preventDefault();
        const app = flat[activeIndex];
        if (app) pick(app);
        break;
      }
      case 'Escape':
        e.preventDefault();
        close(true);
        break;
    }
  }

  function onWindowPointer(e: PointerEvent): void {
    if (open && root && !root.contains(e.target as Node)) close();
  }

  // Keep the highlighted option visible while arrowing through a long list.
  $effect(() => {
    if (!open || flat.length === 0) return;
    const el = document.getElementById(optionId(activeIndex));
    el?.scrollIntoView?.({ block: 'nearest' });
  });
</script>

<svelte:window onpointerdown={onWindowPointer} />

<div class="picker" bind:this={root}>
  <button
    type="button"
    class="trigger"
    bind:this={triggerEl}
    aria-haspopup="listbox"
    aria-expanded={open}
    aria-controls={open ? `${uid}-list` : undefined}
    onclick={toggle}
  >
    <span>{placeholder}</span>
    <span class="chev" aria-hidden="true">▾</span>
  </button>

  {#if onRefresh}
    <button
      type="button"
      class="icon-btn"
      class:spinning={busy}
      aria-label="Refresh app list"
      title="Refresh"
      disabled={busy}
      onclick={() => onRefresh?.()}
    >
      <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
        <path
          d="M20 12a8 8 0 1 1-2.34-5.66M20 4v5h-5"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
  {/if}

  {#if open}
    <div class="panel">
      <input
        bind:this={searchEl}
        type="text"
        role="combobox"
        class="search"
        aria-expanded="true"
        aria-controls="{uid}-list"
        aria-autocomplete="list"
        aria-activedescendant={flat.length ? optionId(activeIndex) : undefined}
        aria-label="Filter apps"
        placeholder="Type to filter…"
        autocomplete="off"
        spellcheck="false"
        value={query}
        oninput={(e) => {
          query = e.currentTarget.value;
          active = 0;
        }}
        onkeydown={onKey}
      />

      <div id="{uid}-list" class="list" role="listbox" aria-label="Apps">
        {#each sections as section (section.label ?? 'all')}
          {#if section.label}
            <div class="group" role="presentation">{section.label}</div>
          {/if}
          {#each section.items as item (item.app.exe)}
            <!-- Keyboard selection runs through the focused combobox input (aria-activedescendant). -->
            <!-- svelte-ignore a11y_interactive_supports_focus a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
            <div
              id={optionId(item.index)}
              class="opt"
              class:active={item.index === activeIndex}
              class:disabled={item.app.disabled}
              role="option"
              aria-selected={item.index === activeIndex}
              aria-disabled={item.app.disabled ? 'true' : undefined}
              onpointermove={() => (active = item.index)}
              onclick={() => pick(item.app)}
            >
              <AppIcon exe={item.app.exe} name={item.app.name} iconB64={item.app.iconB64 ?? null} size={24} />
              <span class="text">
                <span class="name">{item.app.name || item.app.exe}</span>
                <span class="hint">{item.app.hint ?? item.app.exe}</span>
                {#if item.app.badge}
                  <span class="badge">{item.app.badge}</span>
                {/if}
              </span>
              {#if item.app.active}
                <span class="live" aria-hidden="true" title="Playing audio"></span>
                <span class="sr">Playing audio</span>
              {/if}
            </div>
          {/each}
        {/each}

        {#if flat.length === 0}
          <div class="empty" role="presentation">
            {needle ? `No apps match “${query.trim()}”` : emptyText}
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .picker {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .trigger {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 7px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    cursor: pointer;
  }

  .trigger:hover {
    border-color: var(--text-dim);
  }

  .chev {
    color: var(--text-dim);
    font-size: 11px;
  }

  .icon-btn {
    display: inline-grid;
    place-items: center;
    width: 30px;
    height: 30px;
    padding: 0;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text-dim);
    cursor: pointer;
  }

  .icon-btn:hover:not(:disabled) {
    color: var(--text);
  }

  .icon-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .icon-btn.spinning svg {
    animation: spin 0.9s linear infinite;
  }

  .trigger:focus-visible,
  .icon-btn:focus-visible,
  .search:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .panel {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 20;
    width: min(360px, 90vw);
    padding: 8px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.45);
  }

  .search {
    width: 100%;
    padding: 7px 10px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--text);
  }

  .list {
    max-height: 300px;
    overflow-y: auto;
    margin-top: 6px;
  }

  .group {
    padding: 8px 8px 4px;
    font-size: 11px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  .opt {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px;
    border-radius: 6px;
    cursor: pointer;
  }

  .opt.active {
    background: var(--surface-2);
    box-shadow: inset 2px 0 0 var(--accent);
  }

  .text {
    flex: 1;
    min-width: 0;
  }

  .name,
  .hint {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hint {
    font-size: 11px;
    color: var(--text-dim);
    font-family: ui-monospace, Consolas, monospace;
  }

  .opt.disabled {
    opacity: 0.55;
    cursor: default;
  }

  .badge {
    display: block;
    font-size: 11px;
    color: var(--text-dim);
  }

  .live {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #3ddc84;
    box-shadow: 0 0 0 3px rgba(61, 220, 132, 0.2);
  }

  .empty {
    padding: 12px 8px;
    text-align: center;
    color: var(--text-dim);
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .icon-btn.spinning svg {
      animation: none;
    }
  }
</style>
