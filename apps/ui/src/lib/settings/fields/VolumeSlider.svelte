<script lang="ts">
  import { dbToVolumePercent, volumePercentToDb, VOLUME_MAX_PCT } from '../../discovery';

  interface Props {
    /** Current gain in dB (-60 floor = muted, 0 = 100 %, +12 = 150 %). */
    valueDb: number;
    muted?: boolean;
    disabled?: boolean;
    id?: string;
    ariaLabel?: string;
    /** Called with the new gain in dB. */
    onChange: (db: number) => void;
  }

  let { valueDb, muted = false, disabled = false, id, ariaLabel = 'Volume', onChange }: Props = $props();

  const pct = $derived(Math.round(dbToVolumePercent(valueDb)));
  const isMuted = $derived(muted || pct === 0);
  const label = $derived(isMuted ? 'Muted' : `${pct}%`);
  /** Position of the 100 % tick along the track. */
  const tickLeft = (100 / VOLUME_MAX_PCT) * 100;

  function emit(percent: number): void {
    const clamped = Math.min(VOLUME_MAX_PCT, Math.max(0, Math.round(percent)));
    onChange(volumePercentToDb(clamped));
  }

  /** Arrows step 1 %, Shift+arrows and PageUp/PageDown step 10 %, Home and End go to the ends. */
  function onKeydown(e: KeyboardEvent): void {
    const step = e.shiftKey ? 10 : 1;
    let next: number;
    switch (e.key) {
      case 'ArrowRight':
      case 'ArrowUp':
        next = pct + step;
        break;
      case 'ArrowLeft':
      case 'ArrowDown':
        next = pct - step;
        break;
      case 'PageUp':
        next = pct + 10;
        break;
      case 'PageDown':
        next = pct - 10;
        break;
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = VOLUME_MAX_PCT;
        break;
      default:
        return;
    }
    e.preventDefault();
    emit(next);
  }
</script>

<div class="volume" class:muted={isMuted}>
  <div class="track">
    <input
      {id}
      type="range"
      min="0"
      max={VOLUME_MAX_PCT}
      step="1"
      value={pct}
      {disabled}
      aria-label={ariaLabel}
      aria-valuetext={label}
      oninput={(e) => emit(Number(e.currentTarget.value))}
      onkeydown={onKeydown}
    />
    <span class="tick" style:left="{tickLeft}%" aria-hidden="true"></span>
  </div>
  <span class="value" aria-hidden="true">{label}</span>
</div>

<style>
  .volume {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 260px;
    max-width: 100%;
  }

  .track {
    position: relative;
    flex: 1;
  }

  input[type='range'] {
    width: 100%;
    margin: 0;
    accent-color: var(--accent);
  }

  input[type='range']:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 4px;
    border-radius: 4px;
  }

  input[type='range']:disabled {
    opacity: 0.5;
  }

  .tick {
    position: absolute;
    top: 50%;
    width: 2px;
    height: 12px;
    margin-top: -6px;
    background: var(--text-dim);
    pointer-events: none;
    transform: translateX(-1px);
  }

  .value {
    min-width: 56px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }

  .volume.muted .value {
    color: var(--text-dim);
  }
</style>
