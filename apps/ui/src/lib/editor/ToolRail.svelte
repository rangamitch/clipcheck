<script lang="ts">
  import { tool, type EditorTool } from './store';

  /** Tool buttons shown in the rail, top to bottom. */
  const TOOLS: { id: EditorTool; label: string; key: string }[] = [
    { id: 'select', label: 'Selection tool', key: 'V' },
    { id: 'razor', label: 'Cut tool', key: 'C' },
    { id: 'hand', label: 'Hand tool', key: 'H' },
    { id: 'zoom', label: 'Zoom tool', key: 'Z' },
  ];
</script>

<div class="rail" role="toolbar" aria-label="Tools" aria-orientation="vertical">
  {#each TOOLS as t (t.id)}
    <div class="slot">
      <button
        class="btn"
        class:active={$tool === t.id}
        type="button"
        aria-label={t.label}
        aria-pressed={$tool === t.id}
        title={`${t.label} (${t.key})`}
        onclick={() => tool.set(t.id)}
      >
        {#if t.id === 'select'}
          <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <path d="M6 4l12 7-5.2 1.3L15 19l-2.6 1.1-2.3-5.6L6 18z" />
          </svg>
        {:else if t.id === 'razor'}
          <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <circle cx="6" cy="18" r="2.5" stroke-width="1.8" />
            <circle cx="6" cy="6" r="2.5" stroke-width="1.8" />
            <path d="M8 7.5L20 18" stroke-width="1.8" />
            <path d="M8 16.5L20 6" stroke-width="1.8" />
          </svg>
        {:else if t.id === 'hand'}
          <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V11" />
            <path d="M11 11V4a1.5 1.5 0 0 1 3 0v7" />
            <path d="M14 11V5.5a1.5 1.5 0 0 1 3 0V13" />
            <path d="M17 9.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-1.5a6 6 0 0 1-4.6-2.2L5 15.5a1.5 1.5 0 0 1 2.2-2L8 14.5" />
          </svg>
        {:else}
          <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
            <circle cx="10.5" cy="10.5" r="6" />
            <path d="M15 15l5 5" />
            <path d="M10.5 8v5M8 10.5h5" />
          </svg>
        {/if}
      </button>
      <span class="tip" aria-hidden="true">
        {t.label}
        <kbd>{t.key}</kbd>
      </span>
    </div>
  {/each}
</div>

<style>
  .rail {
    position: relative;
    z-index: 2;
    flex: 0 0 32px;
    width: 32px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    padding-top: 6px;
    background: var(--surface);
    border-right: 1px solid var(--border);
  }

  .slot {
    position: relative;
  }

  .btn {
    width: 28px;
    height: 28px;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
  }

  .btn:hover {
    border-color: var(--text-dim);
  }

  .btn.active {
    border-color: var(--accent);
    color: var(--text);
    box-shadow: inset 0 -2px 0 var(--accent);
  }

  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .tip {
    position: absolute;
    left: 36px;
    top: 50%;
    transform: translateY(-50%);
    z-index: 10;
    display: none;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    background: #1b1d22;
    color: #e8e8ea;
    border: 1px solid #2f323a;
    border-radius: 5px;
    font-size: 11px;
    white-space: nowrap;
    pointer-events: none;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  }

  .slot:hover .tip,
  .slot:focus-within .tip {
    display: inline-flex;
  }

  kbd {
    color: #8a8f99;
    font-family: inherit;
    font-size: 10px;
    padding: 0 4px;
    border: 1px solid #3a3e47;
    border-radius: 3px;
  }
</style>
