<script lang="ts">
  // Header cell of one timeline track: name, M/S/L/T toggles, eye for video tracks, a volume
  // fader for audio tracks and a level meter. Every edit goes through `dispatch(patch_track)`.
  import { dispatch } from './store';
  import { trackLevels } from './mediaCache';
  import type { Track, TrackPatch } from './types';

  let { track, height }: { track: Track; height: number } = $props();

  /** Quietest level shown by the meter, dB. */
  const METER_FLOOR_DB = -60;
  /** Fader range, dB. */
  const FADER_MIN_DB = -60;
  const FADER_MAX_DB = 12;

  let editing = $state(false);
  let draftName = $state('');

  const compact = $derived(height < 40);
  const isAudio = $derived(track.kind === 'audio');
  const isVideo = $derived(track.kind === 'video');
  /** Current level in dB for the meter, from the playback engine's store (absent until it reports). */
  const levelDb = $derived($trackLevels[track.id] ?? METER_FLOOR_DB);
  const meterPct = $derived(
    Math.round(Math.min(1, Math.max(0, (levelDb - METER_FLOOR_DB) / -METER_FLOOR_DB)) * 100),
  );

  /** Sends a `patch_track` edit for this track. */
  function patch(p: TrackPatch): void {
    void dispatch({ type: 'patch_track', track_id: track.id, patch: p });
  }

  /** Opens the inline name editor. */
  function startRename(): void {
    draftName = track.name;
    editing = true;
  }

  /** Closes the name editor and applies the new name when it is non-empty and changed. */
  function commitRename(): void {
    if (!editing) return;
    editing = false;
    const name = draftName.trim();
    if (name !== '' && name !== track.name) {
      patch({ name });
    }
  }

  /** Enter commits, Escape cancels (the blur that follows is then a no-op). */
  function onNameKey(e: KeyboardEvent): void {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      editing = false;
    }
  }

  /** Selects the whole name when the editor opens. */
  function focusSelect(node: HTMLInputElement): void {
    node.focus();
    node.select();
  }

  /** Reads the fader value and applies it. */
  function onFaderChange(e: Event): void {
    const value = Number((e.currentTarget as HTMLInputElement).value);
    if (Number.isFinite(value)) {
      patch({ volume_db: value });
    }
  }

  /** Volume label such as "0.0 dB" or "-6.5 dB". */
  function formatDb(db: number): string {
    return `${db > 0 ? '+' : ''}${db.toFixed(1)} dB`;
  }
</script>

<div
  class="track-header"
  class:compact
  class:locked={track.locked}
  class:dim={track.hidden || track.muted}
  style:height="{height}px"
  role="group"
  aria-label="Track {track.name}"
>
  <div class="row top">
    {#if editing}
      <input
        class="name-input"
        type="text"
        bind:value={draftName}
        use:focusSelect
        onkeydown={onNameKey}
        onblur={commitRename}
        aria-label="Track name"
        spellcheck="false"
      />
    {:else}
      <button
        type="button"
        class="name"
        ondblclick={startRename}
        title="{track.name} (double click to rename)"
        aria-label="Track name {track.name}, double click to rename"
      >
        {track.name}
      </button>
    {/if}

    <div class="toggles">
      <button
        type="button"
        class="tb"
        class:on={track.muted}
        aria-pressed={track.muted}
        aria-label="Mute track"
        title="Mute"
        onclick={() => patch({ muted: !track.muted })}>M</button
      >
      <button
        type="button"
        class="tb"
        class:on={track.solo}
        aria-pressed={track.solo}
        aria-label="Solo track"
        title="Solo"
        onclick={() => patch({ solo: !track.solo })}>S</button
      >
      <button
        type="button"
        class="tb"
        class:on={track.locked}
        aria-pressed={track.locked}
        aria-label="Lock track"
        title="Lock"
        onclick={() => patch({ locked: !track.locked })}
      >
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <rect x="3" y="7" width="10" height="7" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.6" />
          <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" fill="none" stroke="currentColor" stroke-width="1.6" />
        </svg>
      </button>
      {#if isVideo}
        <button
          type="button"
          class="tb"
          class:on={track.hidden}
          aria-pressed={track.hidden}
          aria-label={track.hidden ? 'Show track' : 'Hide track'}
          title={track.hidden ? 'Show track' : 'Hide track'}
          onclick={() => patch({ hidden: !track.hidden })}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path
              d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8s-2.4 4.5-6.5 4.5S1.5 8 1.5 8z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
            />
            <circle cx="8" cy="8" r="2" fill="currentColor" />
            {#if track.hidden}
              <path d="M2.5 13.5l11-11" stroke="currentColor" stroke-width="1.4" />
            {/if}
          </svg>
        </button>
      {/if}
      <button
        type="button"
        class="tb"
        class:on={track.targeted}
        aria-pressed={track.targeted}
        aria-label="Target track for pasting and new clips"
        title="Target"
        onclick={() => patch({ targeted: !track.targeted })}>T</button
      >
    </div>
  </div>

  {#if isAudio && !compact}
    <div class="row bottom">
      <input
        class="fader"
        type="range"
        min={FADER_MIN_DB}
        max={FADER_MAX_DB}
        step="0.5"
        value={track.volume_db}
        onchange={onFaderChange}
        ondblclick={() => patch({ volume_db: 0 })}
        aria-label="Track volume"
        aria-valuetext={formatDb(track.volume_db)}
        title="{formatDb(track.volume_db)} (double click resets to 0 dB)"
      />
      <div
        class="meter"
        role="meter"
        aria-label="Track level"
        aria-valuemin={METER_FLOOR_DB}
        aria-valuemax={0}
        aria-valuenow={Math.round(levelDb)}
      >
        <div class="meter-fill" style="width: {meterPct}%"></div>
      </div>
    </div>
  {/if}
</div>

<style>
  .track-header {
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 3px;
    padding: 3px 6px;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
    color: var(--text);
    overflow: hidden;
    min-width: 0;
  }

  .track-header.locked {
    background-image: repeating-linear-gradient(
      -45deg,
      transparent 0 6px,
      rgba(255, 255, 255, 0.04) 6px 7px
    );
  }

  .track-header.dim .name {
    color: var(--text-dim);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    text-align: left;
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    font-size: 12px;
    font-weight: 600;
    cursor: default;
  }

  .name-input {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    padding: 1px 4px;
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--accent);
    border-radius: 3px;
  }

  .toggles {
    display: flex;
    gap: 2px;
    flex-shrink: 0;
  }

  .tb {
    width: 20px;
    height: 18px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
    background: var(--surface);
    color: var(--text-dim);
    border: 1px solid var(--border);
    border-radius: 3px;
    cursor: pointer;
  }

  .tb:hover {
    color: var(--text);
  }

  .tb:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .tb.on {
    color: var(--text);
    background: var(--accent);
    border-color: var(--accent);
  }

  .fader {
    flex: 1;
    min-width: 0;
    height: 14px;
    margin: 0;
    accent-color: var(--accent);
  }

  .meter {
    width: 36px;
    height: 6px;
    flex-shrink: 0;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 2px;
    overflow: hidden;
  }

  .meter-fill {
    height: 100%;
    background: linear-gradient(90deg, #2a9d8f, #d4a017 80%, var(--accent));
  }
</style>
