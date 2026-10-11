<script lang="ts">
  import { commitNumber } from '../pure';

  interface Props {
    value: number;
    min: number;
    max: number;
    step?: number;
    /** Round to whole numbers on commit. */
    integer?: boolean;
    unit?: string;
    disabled?: boolean;
    id?: string;
    labelId?: string;
    /** Accessible name when there is no visible label to point at. */
    ariaLabel?: string;
    describedBy?: string;
    onchange: (value: number) => void;
  }

  let {
    value,
    min,
    max,
    step = 1,
    integer = true,
    unit,
    disabled = false,
    id,
    labelId,
    ariaLabel,
    describedBy,
    onchange,
  }: Props = $props();

  /**
   * What the user has typed, or null when the field is not being edited. While null the shown
   * text is derived from the prop, so it is correct on the first render and follows outside
   * changes without an effect. While typed text exists it is kept, even if the prop changes.
   */
  let editing = $state<string | null>(null);
  const shown = $derived(editing ?? String(value));

  /**
   * Clamps and commits the typed text. Text that is not a number reverts to the current value.
   * The comparison is against the committed prop, so a clamped value that differs from the
   * prop is sent. Enter keeps focus and shows the committed number; blur ends the edit.
   */
  function commit(keepEditing: boolean): void {
    const n = commitNumber(editing ?? String(value), min, max, integer);
    if (n === null) {
      editing = null;
      return;
    }
    if (n !== value) onchange(n);
    editing = keepEditing ? String(n) : null;
  }
</script>

<div class="number">
  <input
    type="number"
    inputmode={integer ? 'numeric' : 'decimal'}
    {id}
    {min}
    {max}
    {step}
    {disabled}
    value={shown}
    aria-labelledby={labelId}
    aria-label={ariaLabel}
    aria-describedby={describedBy}
    oninput={(e) => (editing = e.currentTarget.value)}
    onblur={() => commit(false)}
    onkeydown={(e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        commit(true);
      }
    }}
  />
  {#if unit}
    <span class="unit">{unit}</span>
  {/if}
</div>

<style>
  .number {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }

  input {
    width: 120px;
    padding: 7px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    font-variant-numeric: tabular-nums;
  }

  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  input:disabled {
    opacity: 0.5;
  }

  .unit {
    color: var(--text-dim);
  }
</style>
