<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { SETTINGS_TABS, requestedTab } from './settings/tabs';
  import { hasBlockingErrors } from './settings/pure';
  import { videoDefaultWrites } from './settings/video';
  import {
    acceptExternal,
    flushAutosave,
    keepMine,
    loadSettings,
    onRecorderConnected,
    retryAutosave,
    revert,
    setPath,
    settings,
    startSettingsSync,
  } from './settings/store';
  import { liveStatus } from './status';

  let active = $state(SETTINGS_TABS[0].id);

  function messageOf(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  const connected = $derived($liveStatus.connected);
  const readOnly = $derived(!connected);
  const current = $derived(SETTINGS_TABS.find((t) => t.id === active) ?? SETTINGS_TABS[0]);
  const blocking = $derived(hasBlockingErrors($settings.diagnostics));

  /** Footer text. The order says what the user must act on first. */
  const statusText = $derived.by(() => {
    const s = $settings;
    if (s.saving) return 'Saving…';
    if (s.dirty && s.external) return 'Settings changed elsewhere. Choose Reload or Keep mine.';
    if (s.dirty && !connected) return 'Recorder offline. Changes are kept and save when it reconnects.';
    if (s.dirty && blocking) return 'Fix the highlighted fields to save';
    if (s.saveError) return "Couldn't save:";
    if (s.dirty && s.held) return 'Unsaved changes. Saved when you leave the field.';
    if (s.dirty) return 'Saving…';
    return 'All changes saved';
  });

  onMount(() => {
    startSettingsSync();
  });

  // Load (or check for outside changes) each time the recorder becomes reachable.
  $effect(() => {
    if (connected) untrack(() => onRecorderConnected());
  });

  function selectTab(id: string): void {
    void flushAutosave();
    active = id;
  }

  /** Leaving a field (or the panel) saves a change that waits for blur. */
  function onPanelFocusOut(): void {
    void flushAutosave();
  }

  function onPageHide(): void {
    void flushAutosave();
  }

  function onVisibility(): void {
    if (document.visibilityState === 'hidden') void flushAutosave();
  }

  // Other tabs can ask to jump somewhere (for example Audio -> Hotkeys for push-to-talk).
  $effect(() => {
    const wanted = $requestedTab;
    if (wanted) {
      void flushAutosave();
      active = wanted;
      requestedTab.set(null);
    }
  });

  /** Arrow keys move between tabs, Home and End jump to the ends. */
  function onTabKey(e: KeyboardEvent): void {
    const ids = SETTINGS_TABS.map((t) => t.id);
    const at = ids.indexOf(active);
    let next: number | null = null;
    if (e.key === 'ArrowDown') next = (at + 1) % ids.length;
    else if (e.key === 'ArrowUp') next = (at - 1 + ids.length) % ids.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = ids.length - 1;
    if (next === null) return;
    e.preventDefault();
    const id = ids[next];
    selectTab(id);
    queueMicrotask(() => document.getElementById(`settings-tab-${id}`)?.focus());
  }

  /** Ctrl+S saves what is waiting now instead of after the debounce. */
  function onWindowKey(e: KeyboardEvent): void {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
      e.preventDefault();
      void flushAutosave();
    }
  }
</script>

<svelte:window onkeydown={onWindowKey} onpagehide={onPageHide} />
<svelte:document onvisibilitychange={onVisibility} />

<div class="settings">
  {#if !connected}
    <div class="banner offline" role="status">
      The recorder is offline. Settings are read-only until it reconnects.
    </div>
  {/if}

  {#if $settings.external}
    <div class="banner external" role="alert">
      <span>Settings changed elsewhere.</span>
      <div class="banner-actions">
        <button type="button" onclick={acceptExternal}>Reload</button>
        <button type="button" onclick={keepMine}>Keep mine</button>
      </div>
    </div>
  {/if}

  <div class="split">
    <div
      class="tabs"
      role="tablist"
      aria-label="Settings sections"
      aria-orientation="vertical"
      tabindex="-1"
      onkeydown={onTabKey}
    >
      {#each SETTINGS_TABS as tab (tab.id)}
        <button
          type="button"
          role="tab"
          id="settings-tab-{tab.id}"
          class="tab"
          class:active={active === tab.id}
          aria-selected={active === tab.id}
          aria-controls="settings-panel"
          tabindex={active === tab.id ? 0 : -1}
          onclick={() => selectTab(tab.id)}
        >
          {tab.label}
        </button>
      {/each}
    </div>

    <div
      class="panel"
      role="tabpanel"
      id="settings-panel"
      aria-labelledby="settings-tab-{active}"
      tabindex="0"
      onfocusout={onPanelFocusOut}
    >
      {#if !$settings.saved}
        {#if $settings.error}
          <p class="message">{$settings.error}</p>
          <button type="button" class="retry" onclick={() => void loadSettings()}>Try again</button>
        {:else if $settings.loading}
          <p class="dim">Loading settings…</p>
        {:else if !connected}
          <p class="dim">Settings are not available while the recorder is offline.</p>
        {/if}
      {:else}
        {@const Tab = current.component}
        {#key active}
          <!-- A tab that throws shows its message here instead of blanking the whole panel. -->
          <svelte:boundary onerror={(e) => console.error(`Settings tab "${active}" failed:`, e)}>
            <Tab disabled={readOnly || $settings.saving} />
            {#snippet failed(error, reset)}
              <div class="tab-failed" role="alert">
                <p class="message">This tab failed to load: {messageOf(error)}</p>
                <div class="tab-failed-actions">
                  <button type="button" onclick={reset}>Try again</button>
                  {#if active === 'video'}
                    <button
                      type="button"
                      onclick={() => {
                        for (const [path, value] of videoDefaultWrites()) setPath(path, value);
                        reset();
                      }}>Reset video settings to defaults</button
                    >
                  {/if}
                </div>
              </div>
            {/snippet}
          </svelte:boundary>
        {/key}
      {/if}
    </div>
  </div>

  <footer class="footer">
    <div class="status" role="status" aria-live="polite">
      <span>{statusText}</span>
      {#if $settings.saveError && !$settings.saving}
        <span class="message">{$settings.saveError}</span>
        <button type="button" class="link" onclick={() => void retryAutosave()} disabled={!connected}>
          Retry
        </button>
      {/if}
      {#if $settings.checking}
        <span class="dim">Checking…</span>
      {/if}
      {#if $settings.notice}
        <span class="notice">{$settings.notice}</span>
      {/if}
      {#if $settings.error && $settings.saved && !$settings.saveError}
        <span class="message">{$settings.error}</span>
      {/if}
    </div>
    <div class="actions">
      {#if $settings.dirty && !$settings.saving}
        <button type="button" class="ghost" onclick={revert}>Revert</button>
      {/if}
    </div>
  </footer>
</div>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
  }

  .banner {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 16px;
    font-size: 13px;
    border-bottom: 1px solid var(--border);
  }

  .banner.offline {
    background: var(--surface-2);
    color: var(--text-dim);
  }

  .banner.external {
    background: var(--surface-2);
    color: var(--text);
    border-left: 3px solid var(--accent);
  }

  .banner-actions {
    display: flex;
    gap: 8px;
  }

  .split {
    display: flex;
    flex: 1 1 auto;
    min-height: 0;
  }

  .tabs {
    flex: 0 0 160px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px 8px;
    border-right: 1px solid var(--border);
    background: var(--surface);
    overflow-y: auto;
  }

  .tab {
    text-align: left;
    padding: 8px 10px;
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-dim);
    font: inherit;
    cursor: pointer;
  }

  .tab:hover {
    color: var(--text);
    border-color: var(--border);
  }

  .tab.active {
    color: var(--text);
    background: var(--surface-2);
    border-color: var(--accent);
  }

  .tab:focus-visible,
  .panel:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .panel {
    flex: 1 1 auto;
    min-width: 0;
    overflow-y: auto;
    padding: 16px 24px 24px;
  }

  .panel p {
    margin: 0;
  }

  /* Rules identical across every settings tab; tabs rely on these instead of repeating them. */
  .panel :global(button:hover:not(:disabled)) {
    border-color: var(--accent);
  }

  .panel :global(button:focus-visible),
  .panel :global(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .panel :global(button:disabled) {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .panel :global(h3) {
    font-size: 15px;
  }

  /* Heading style shared by every settings tab (the tab components no longer repeat it). */
  .panel :global(h2) {
    margin: 0 0 4px;
    font-size: 18px;
  }

  .dim {
    color: var(--text-dim);
  }

  .message {
    color: var(--danger);
    font-size: 13px;
  }

  .footer {
    flex: 0 0 auto;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 16px;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }

  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .notice {
    color: var(--text);
  }

  .actions {
    display: flex;
    gap: 8px;
  }

  button {
    padding: 7px 14px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.ghost {
    background: none;
  }

  button.link {
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    text-decoration: underline;
  }

  button.retry {
    margin-top: 10px;
  }

  .tab-failed {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-width: 560px;
  }

  .tab-failed-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
</style>
