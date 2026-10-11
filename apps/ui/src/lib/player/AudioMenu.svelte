<script lang="ts">
  import type { AudioTrackDto } from '../api';
  import Icon from './Icon.svelte';
  import OutputPicker from './OutputPicker.svelte';
  import type { TrackMixer } from './mixer';
  import {
    GAIN_MAX_DB,
    GAIN_MIN_DB,
    setGainDb,
    soloTrack,
    toggleTrack,
    trackLabel,
    type TrackState,
  } from './model';

  interface Props {
    infos: AudioTrackDto[];
    tracks: TrackState[];
    mixer: TrackMixer;
    ontracks: (next: TrackState[]) => void;
  }

  let { infos, tracks, mixer, ontracks }: Props = $props();

  let open = $state(false);
  let advanced = $state(false);
  let wrap: HTMLDivElement | undefined = $state();

  /** Mixer state for one track. Falls back to a neutral state when the index is missing. */
  function stateOf(info: AudioTrackDto): TrackState {
    return (
      tracks.find((t) => t.index === info.index) ?? {
        index: info.index,
        audible: false,
        solo: false,
        gainDb: 0,
      }
    );
  }

  /** Gain as shown in the UI, e.g. "+3 dB" or "-6 dB". */
  function gainText(db: number): string {
    return `${db > 0 ? '+' : ''}${db} dB`;
  }

  function onGainInput(index: number, e: Event) {
    ontracks(setGainDb(tracks, index, Number((e.currentTarget as HTMLInputElement).value)));
  }

  // Close on Escape (captured, so the player's own key handling does not also see it).
  // Close on a pointerdown outside the menu. Both listeners exist only while the menu is open.
  $effect(() => {
    if (!open) return;

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        open = false;
      }
    };
    const onPointer = (e: PointerEvent) => {
      const target = e.target as Node | null;
      if (wrap && target && !wrap.contains(target)) open = false;
    };

    document.addEventListener('keydown', onKey, true);
    document.addEventListener('pointerdown', onPointer);
    return () => {
      document.removeEventListener('keydown', onKey, true);
      document.removeEventListener('pointerdown', onPointer);
    };
  });
</script>

{#if infos.length >= 2}
  <div class="audio-menu" bind:this={wrap}>
    <button
      type="button"
      class="menu-button"
      aria-haspopup="menu"
      aria-expanded={open}
      onclick={() => (open = !open)}
    >
      <Icon name="audio" size={18} />
      <span>Audio</span>
    </button>

    {#if open}
      <div class="menu" role="menu" aria-label="Audio tracks">
        <div class="rows">
          {#each infos as info (info.index)}
            {@const st = stateOf(info)}
            <button
              type="button"
              class="row"
              role="menuitemcheckbox"
              aria-checked={st.audible}
              onclick={() => ontracks(toggleTrack(tracks, info.index))}
            >
              <span class="toggle" class:on={st.audible} aria-hidden="true">
                <Icon name={st.audible ? 'volume' : 'mute'} size={16} />
              </span>
              <span class="label">
                <span class="name">{trackLabel(info)}</span>
                <small class="dim">{info.codec} &middot; {info.channels} ch</small>
              </span>
            </button>
          {/each}
        </div>

        <button
          type="button"
          class="disclosure"
          aria-expanded={advanced}
          onclick={() => (advanced = !advanced)}
        >
          <span>Advanced</span>
          <span class="chevron" class:up={advanced} aria-hidden="true">&#9662;</span>
        </button>

        {#if advanced}
          <div class="advanced">
            {#each infos as info (info.index)}
              {@const st = stateOf(info)}
              <div class="adv-track">
                <div class="adv-head">
                  <span class="name">{trackLabel(info)}</span>
                  <button
                    type="button"
                    class="solo"
                    aria-label={`Solo ${trackLabel(info)}`}
                    aria-pressed={st.solo}
                    onclick={() => ontracks(soloTrack(tracks, info.index))}>S</button
                  >
                </div>
                <div class="gain">
                  <input
                    type="range"
                    min={GAIN_MIN_DB}
                    max={GAIN_MAX_DB}
                    step="1"
                    value={st.gainDb}
                    aria-label={`Gain ${trackLabel(info)}`}
                    aria-valuetext={gainText(st.gainDb)}
                    oninput={(e) => onGainInput(info.index, e)}
                  />
                  <span class="db">{gainText(st.gainDb)}</span>
                </div>
              </div>
            {/each}
            <p class="dim note">Preview only. The file is not changed.</p>
          </div>
        {/if}

        <div class="output">
          <OutputPicker {mixer} />
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .audio-menu {
    position: relative;
    display: inline-block;
    font-size: 13px;
  }

  button {
    font: inherit;
    color: inherit;
    cursor: pointer;
  }

  button:focus-visible,
  input[type='range']:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .menu-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text);
  }

  .menu-button:hover {
    border-color: var(--text-dim);
  }

  .menu {
    position: absolute;
    left: 0;
    bottom: calc(100% + 6px);
    z-index: 20;
    width: 280px;
    max-height: 60vh;
    overflow-y: auto;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 6px 8px;
    border: none;
    border-radius: 6px;
    background: transparent;
    text-align: left;
  }

  .row:hover {
    background: var(--surface-2);
  }

  .toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 26px;
    height: 26px;
    border-radius: 6px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-dim);
  }

  .toggle.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .label {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dim {
    color: var(--text-dim);
    font-size: 12px;
  }

  .disclosure {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    margin-top: 6px;
    padding: 6px 8px;
    border: none;
    border-top: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
  }

  .chevron {
    display: inline-block;
    transition: transform 0.15s ease;
  }

  .chevron.up {
    transform: rotate(180deg);
  }

  .advanced {
    padding: 4px 8px 8px;
  }

  .adv-track {
    padding: 6px 0;
  }

  .adv-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .solo {
    min-width: 26px;
    height: 22px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface-2);
    font-weight: 600;
    font-size: 12px;
  }

  .solo[aria-pressed='true'] {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .gain {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }

  .gain input {
    flex: 1 1 auto;
    min-width: 0;
  }

  .db {
    width: 52px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
  }

  .note {
    margin: 4px 0 0;
  }

  .output {
    margin-top: 6px;
    padding: 6px 8px 2px;
    border-top: 1px solid var(--border);
  }
</style>
