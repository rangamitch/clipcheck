<script lang="ts">
  // Right-hand inspector panel: Clip, Track or Project settings.
  // The tab follows the selection (clips selected: Clip, otherwise Project). A tab the user picks by
  // hand stays until the selection changes.
  import ClipInspector from './ClipInspector.svelte';
  import ProjectInspector from './ProjectInspector.svelte';
  import TrackInspector from './TrackInspector.svelte';
  import { project, selectedClips, selection } from './store';

  type InspectorTab = 'clip' | 'track' | 'project';

  const TABS: { id: InspectorTab; label: string }[] = [
    { id: 'clip', label: 'Clip' },
    { id: 'track', label: 'Track' },
    { id: 'project', label: 'Project' },
  ];

  /** Buttons of the tab list, for arrow-key focus moves. */
  const tabEls: (HTMLButtonElement | undefined)[] = [];

  /** Identifies the current selection. A manual tab choice applies only to this key. */
  const selectionKey = $derived($selection.join(','));

  /** Manual tab choice and the selection it was made for, or null. */
  let manual = $state<{ tab: InspectorTab; key: string } | null>(null);

  /** Tab chosen by the selection alone. */
  const autoTab: InspectorTab = $derived($selectedClips.length > 0 ? 'clip' : 'project');

  /** Tab shown now. */
  const activeTab = $derived(
    manual !== null && manual.key === selectionKey ? manual.tab : autoTab,
  );

  /** Track picked from the dropdown, or null to use the default. */
  let trackPick = $state<string | null>(null);

  const tracks = $derived($project?.tracks ?? []);

  /** Track of the first selected clip, used when no track is picked. */
  const selectedTrackId = $derived($selectedClips[0]?.track_id ?? null);

  /** Track shown in the Track tab: the pick, else the selected clip's track, else the first track. */
  const trackId = $derived.by((): string | null => {
    const pick = trackPick;
    if (pick !== null && tracks.some((t) => t.id === pick)) return pick;
    const fromSelection = selectedTrackId;
    if (fromSelection !== null && tracks.some((t) => t.id === fromSelection)) return fromSelection;
    return tracks[0]?.id ?? null;
  });

  /** Heading text: what is being inspected. */
  const heading = $derived.by((): string => {
    if (activeTab === 'clip') {
      const n = $selectedClips.length;
      if (n === 0) return 'Clip';
      if (n === 1) return $selectedClips[0]?.name ?? 'Clip';
      return `${n} clips`;
    }
    if (activeTab === 'track') {
      const t = tracks.find((x) => x.id === trackId);
      return t ? t.name : 'Track';
    }
    return $project?.name ?? 'Project';
  });

  /** Makes `tab` the active tab until the selection changes. */
  function chooseTab(tab: InspectorTab): void {
    manual = { tab, key: selectionKey };
  }

  /** Arrow, Home and End keys move between tabs. */
  function onTabKeydown(e: KeyboardEvent, index: number): void {
    let next: number;
    if (e.key === 'ArrowRight') next = (index + 1) % TABS.length;
    else if (e.key === 'ArrowLeft') next = (index - 1 + TABS.length) % TABS.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = TABS.length - 1;
    else return;
    e.preventDefault();
    const target = TABS[next];
    if (target === undefined) return;
    chooseTab(target.id);
    tabEls[next]?.focus();
  }
</script>

<aside class="inspector" aria-label="Inspector">
  <header class="head">
    <span class="title" title={heading}>{heading}</span>
  </header>

  <div class="tabs" role="tablist" aria-label="Inspector sections">
    {#each TABS as tab, i (tab.id)}
      <button
        bind:this={tabEls[i]}
        type="button"
        role="tab"
        id={`insp-tab-${tab.id}`}
        aria-selected={activeTab === tab.id}
        aria-controls="insp-panel"
        tabindex={activeTab === tab.id ? 0 : -1}
        class:active={activeTab === tab.id}
        onclick={() => chooseTab(tab.id)}
        onkeydown={(e) => onTabKeydown(e, i)}
      >
        {tab.label}
      </button>
    {/each}
  </div>

  <div class="body" id="insp-panel" role="tabpanel" aria-labelledby={`insp-tab-${activeTab}`}>
    {#if activeTab === 'clip'}
      {#if $selectedClips.length > 0}
        <ClipInspector clips={$selectedClips} />
      {:else}
        <p class="hint">Select one or more clips on the timeline to edit them.</p>
      {/if}
    {:else if activeTab === 'track'}
      {#if tracks.length === 0}
        <p class="hint">This project has no tracks.</p>
      {:else}
        <div class="track-pick">
          <label for="insp-track">Track</label>
          <select
            id="insp-track"
            value={trackId ?? ''}
            onchange={(e) => (trackPick = e.currentTarget.value)}
          >
            {#each tracks as t (t.id)}
              <option value={t.id}>{t.name} ({t.kind})</option>
            {/each}
          </select>
        </div>
        {#if trackId !== null}
          {#key trackId}
            <TrackInspector trackId={trackId} />
          {/key}
        {/if}
      {/if}
    {:else}
      <ProjectInspector />
    {/if}
  </div>
</aside>

<style>
  .inspector {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--surface);
    border-left: 1px solid var(--border);
    color: var(--text);
  }
  .head {
    display: flex;
    align-items: center;
    min-height: 40px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    overflow: hidden;
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tabs {
    display: flex;
    gap: 4px;
    padding: 8px 12px 0;
    border-bottom: 1px solid var(--border);
  }
  .tabs button {
    padding: 6px 10px;
    background: transparent;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    color: var(--text-dim);
    cursor: pointer;
  }
  .tabs button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .tabs button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
  }
  .track-pick {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 12px 0;
  }
  .track-pick label {
    flex: 0 0 auto;
    color: var(--text-dim);
  }
  .track-pick select {
    flex: 1 1 auto;
    min-width: 0;
    padding: 6px 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .track-pick select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .hint {
    margin: 0;
    padding: 12px;
    color: var(--text-dim);
    font-size: 12px;
  }
</style>
