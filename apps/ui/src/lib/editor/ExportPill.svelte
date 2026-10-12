<script lang="ts">
  import { onMount, tick } from 'svelte';
  import ExportQueue from './ExportQueue.svelte';
  import { startExportEvents } from './exportEvents';
  import { exportJobs } from './store';
  import type { JobDto } from './types';

  const POPOVER_ID = 'export-queue-popover';

  let open = $state(false);
  let wrapEl = $state<HTMLDivElement | null>(null);
  let pillEl = $state<HTMLButtonElement | null>(null);
  let popoverEl = $state<HTMLDivElement | null>(null);

  /**
   * Opens or closes the popover. Opening moves focus into it; closing with `returnFocus` puts
   * focus back on the pill so keyboard users keep their place.
   */
  async function setOpen(value: boolean, returnFocus = false): Promise<void> {
    open = value;
    if (value) {
      await tick();
      popoverEl?.focus();
    } else if (returnFocus) {
      pillEl?.focus();
    }
  }

  /** Jobs that still need the machine: queued, running or paused. */
  const active = $derived(
    $exportJobs.filter(
      (job) => job.state === 'queued' || job.state === 'running' || job.state === 'paused',
    ),
  );

  /** The job the pill summarises: a running job first, then a paused one, then the first queued. */
  const primary = $derived<JobDto | undefined>(
    active.find((job) => job.state === 'running') ??
      active.find((job) => job.state === 'paused') ??
      active[0],
  );

  const percent = $derived(
    primary ? Math.max(0, Math.min(100, Math.floor(primary.percent))) : 0,
  );

  const label = $derived.by((): string => {
    if (!primary) return '';
    if (primary.state === 'running') return `Exporting ${percent}%`;
    if (primary.state === 'paused') return 'Export paused';
    return 'Export queued';
  });

  const stateText = $derived.by((): string => {
    if (!primary) return '';
    if (primary.state === 'paused') {
      return primary.pausedReason === 'recording' ? 'Paused: a game is running' : 'Paused';
    }
    if (primary.state === 'queued') return 'Waiting';
    return primary.etaS !== null ? `ETA ${formatEta(primary.etaS)}` : '';
  });

  const extra = $derived(active.length > 1 ? active.length - 1 : 0);

  /** Short m:ss or h:mm:ss text for the pill; the detail view has the full numbers. */
  function formatEta(seconds: number): string {
    const total = Math.max(0, Math.round(seconds));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    const ss = String(s).padStart(2, '0');
    if (h > 0) return `${h}:${String(m).padStart(2, '0')}:${ss}`;
    return `${m}:${ss}`;
  }

  // Closing the popover when the last job goes away keeps a stale list from reopening later.
  $effect(() => {
    if (active.length === 0) open = false;
  });

  onMount(() => startExportEvents());

  function onWindowPointerDown(event: PointerEvent): void {
    if (!open || !wrapEl) return;
    const target = event.target;
    if (target instanceof Node && wrapEl.contains(target)) return;
    void setOpen(false);
  }

  function onWindowKeyDown(event: KeyboardEvent): void {
    if (open && event.key === 'Escape') {
      event.preventDefault();
      void setOpen(false, true);
    }
  }
</script>

<svelte:window onpointerdown={onWindowPointerDown} onkeydown={onWindowKeyDown} />

{#if active.length > 0}
  <div class="wrap" bind:this={wrapEl}>
    <button
      type="button"
      class="pill"
      class:paused={primary?.state === 'paused'}
      bind:this={pillEl}
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-controls={POPOVER_ID}
      aria-label={`${label}${stateText ? `, ${stateText}` : ''}${extra > 0 ? `, ${extra} more in queue` : ''}. Open export queue`}
      title={`${label}${stateText ? ` (${stateText})` : ''}`}
      onclick={() => void setOpen(!open, open)}
    >
      <span class="label">{label}</span>
      <span class="bar" aria-hidden="true">
        <span class="fill" style:width="{percent}%"></span>
      </span>
      {#if stateText}
        <span class="state">{stateText}</span>
      {/if}
      {#if extra > 0}
        <span class="more">+{extra}</span>
      {/if}
    </button>

    {#if open}
      <div
        id={POPOVER_ID}
        class="popover"
        role="dialog"
        tabindex="-1"
        aria-label="Export queue"
        bind:this={popoverEl}
      >
        <ExportQueue />
      </div>
    {/if}
  </div>
{/if}

<style>
  .wrap {
    position: relative;
    display: inline-flex;
    flex: 0 0 auto;
  }

  .pill {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 24px;
    max-width: 320px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    font-size: 12px;
    white-space: nowrap;
    cursor: pointer;
  }

  .pill:hover {
    background: var(--surface);
  }

  .pill.paused {
    border-color: var(--text-dim);
    color: var(--text-dim);
  }

  .pill:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .label {
    font-weight: 600;
  }

  .bar {
    position: relative;
    display: inline-block;
    width: 56px;
    height: 4px;
    flex: 0 0 auto;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    background: var(--accent);
    transition: width 0.3s linear;
  }

  .paused .fill {
    background: var(--text-dim);
  }

  .state {
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .more {
    color: var(--text-dim);
  }

  .popover {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 50;
    width: min(440px, calc(100vw - 32px));
    max-height: 60vh;
    overflow: auto;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    cursor: default;
  }
</style>
