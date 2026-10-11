<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { SPEEDS, formatTime } from './model';

  let {
    paused,
    now,
    duration,
    volume,
    muted,
    rate,
    fullscreen,
    ontoggle,
    onvolume,
    onmute,
    onrate,
    onfullscreen,
    leading,
  }: {
    paused: boolean;
    now: number;
    duration: number;
    volume: number;
    muted: boolean;
    rate: number;
    fullscreen: boolean;
    ontoggle: () => void;
    onvolume: (v: number) => void;
    onmute: () => void;
    onrate: (r: number) => void;
    onfullscreen: () => void;
    /** Rendered right before the mute button (the Audio menu lives here). */
    leading?: Snippet;
  } = $props();

  let menuOpen = $state(false);
  let speedWrap: HTMLDivElement | undefined = $state();
  let speedButton: HTMLButtonElement | undefined = $state();
  let menuEl: HTMLDivElement | undefined = $state();

  const volumeIcon = $derived<'mute' | 'volume-low' | 'volume'>(
    muted || volume === 0 ? 'mute' : volume < 0.5 ? 'volume-low' : 'volume',
  );
  const volumePercent = $derived(`${Math.round(volume * 100)}%`);

  // Close the speed menu on a pointer down outside the speed control.
  $effect(() => {
    if (!menuOpen) {
      return;
    }
    const onPointerDown = (e: PointerEvent) => {
      if (speedWrap && e.target instanceof Node && !speedWrap.contains(e.target)) {
        menuOpen = false;
      }
    };
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  });

  function closeMenu(refocus: boolean): void {
    menuOpen = false;
    if (refocus) {
      speedButton?.focus();
    }
  }

  function pick(value: number): void {
    onrate(value);
    closeMenu(true);
  }

  function rateLabel(value: number): string {
    return value === 1 ? 'Normal' : `${value}x`;
  }

  function onSpeedWrapKeydown(e: KeyboardEvent): void {
    if (!menuOpen) {
      return;
    }
    if (e.key === 'Escape') {
      // Stop here so the player does not also react to Escape.
      e.preventDefault();
      e.stopPropagation();
      closeMenu(true);
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      const items = Array.from(
        menuEl?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]') ?? [],
      );
      if (items.length === 0) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      const current = items.indexOf(document.activeElement as HTMLButtonElement);
      const step = e.key === 'ArrowDown' ? 1 : -1;
      const next = current < 0 ? 0 : (current + step + items.length) % items.length;
      items[next].focus();
    }
  }
</script>

<div class="controls" role="toolbar" aria-label="Playback controls">
  <button
    type="button"
    class="icon-btn play-btn"
    aria-label={paused ? 'Play' : 'Pause'}
    title={paused ? 'Play (Space)' : 'Pause (Space)'}
    onclick={ontoggle}
  >
    <Icon name={paused ? 'play' : 'pause'} size={18} />
  </button>

  <span class="time">{formatTime(now)} / {formatTime(duration)}</span>

  <div class="spacer"></div>

  {@render leading?.()}

  <button
    type="button"
    class="icon-btn"
    aria-label={muted ? 'Unmute' : 'Mute'}
    title={muted ? 'Unmute (M)' : 'Mute (M)'}
    onclick={onmute}
  >
    <Icon name={muted ? 'mute' : volumeIcon} size={18} />
  </button>
  <input
    class="volume"
    style="--fill: {Math.round(volume * 100)}%"
    type="range"
    min="0"
    max="1"
    step="0.01"
    value={volume}
    aria-label="Volume"
    aria-valuetext={volumePercent}
    title={`Volume ${volumePercent}`}
    oninput={(e) => onvolume(Number(e.currentTarget.value))}
  />

  <div class="speed-wrap" bind:this={speedWrap} onkeydown={onSpeedWrapKeydown} role="presentation">
    <button
      type="button"
      class="icon-btn speed-btn"
      bind:this={speedButton}
      aria-label={`Playback speed ${rate}x`}
      aria-haspopup="menu"
      aria-expanded={menuOpen}
      title="Playback speed"
      onclick={() => (menuOpen = !menuOpen)}
    >
      <Icon name="speed" size={18} />
      <span class="speed-label">{rate}x</span>
    </button>

    {#if menuOpen}
      <div class="speed-menu" role="menu" aria-label="Playback speed" bind:this={menuEl}>
        {#each SPEEDS as value (value)}
          <button
            type="button"
            role="menuitemradio"
            class="speed-item"
            aria-checked={value === rate}
            class:current={value === rate}
            onclick={() => pick(value)}
          >
            <span class="check">{#if value === rate}<Icon name="check" size={14} />{/if}</span>
            {rateLabel(value)}
          </button>
        {/each}
      </div>
    {/if}
  </div>

  <button
    type="button"
    class="icon-btn"
    aria-label={fullscreen ? 'Exit fullscreen' : 'Fullscreen'}
    title={fullscreen ? 'Exit fullscreen (F)' : 'Fullscreen (F)'}
    onclick={onfullscreen}
  >
    <Icon name={fullscreen ? 'fullscreen-exit' : 'fullscreen'} size={18} />
  </button>
</div>

<style>
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 10px;
    padding: 10px 16px;
    background: var(--surface);
    border-top: 1px solid var(--border);
    color: var(--text);
  }

  .spacer {
    flex: 1 1 auto;
  }

  .time {
    color: var(--text-dim);
    font-size: 13px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    min-width: 32px;
    min-height: 32px;
    padding: 6px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    font: inherit;
  }

  .icon-btn:hover {
    border-color: var(--accent);
  }

  .play-btn {
    width: 32px;
    padding: 0;
    border-radius: 50%;
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .play-btn:hover {
    filter: brightness(1.1);
    border-color: var(--accent);
  }

  .speed-label {
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }

  .icon-btn:focus-visible,
  .speed-item:focus-visible,
  .volume:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  /* High-contrast custom slider: bright filled part, clearly lighter empty part, white thumb. */
  .volume {
    -webkit-appearance: none;
    appearance: none;
    width: 96px;
    height: 18px;
    background: transparent;
    cursor: pointer;
  }

  .volume::-webkit-slider-runnable-track {
    height: 5px;
    border-radius: 3px;
    background: linear-gradient(
      to right,
      #ffffff 0,
      #ffffff var(--fill),
      rgba(255, 255, 255, 0.32) var(--fill),
      rgba(255, 255, 255, 0.32) 100%
    );
  }

  .volume::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 14px;
    height: 14px;
    margin-top: -4.5px;
    border-radius: 50%;
    background: #ffffff;
    border: 0;
    box-shadow: 0 0 0 2px rgba(0, 0, 0, 0.55);
  }

  .volume:hover::-webkit-slider-thumb {
    transform: scale(1.15);
  }

  .speed-wrap {
    position: relative;
  }

  .speed-menu {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    z-index: 10;
    display: flex;
    flex-direction: column;
    min-width: 128px;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  }

  .speed-item {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 32px;
    padding: 6px 10px;
    background: none;
    border: none;
    border-radius: calc(var(--radius) - 2px);
    color: var(--text);
    font: inherit;
    font-size: 13px;
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
  }

  .speed-item:hover {
    background: var(--border);
  }

  .speed-item.current {
    color: var(--accent);
    font-weight: 600;
  }

  .check {
    display: inline-flex;
    width: 14px;
    justify-content: center;
  }
</style>
