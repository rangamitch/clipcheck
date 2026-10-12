<script module lang="ts">
  export interface SegmentOption<V extends string> {
    value: V;
    label: string;
    /** Disables this option only. The group's `disabled` still applies to all options. */
    disabled?: boolean;
  }
</script>

<script lang="ts" generics="T extends string">
  interface Props {
    value: T;
    options: SegmentOption<T>[];
    /** Radio group name. Must be unique on the page, for example the control id. */
    name: string;
    disabled?: boolean;
    labelId?: string;
    describedBy?: string;
    onchange: (value: T) => void;
  }

  let { value, options, name, disabled = false, labelId, describedBy, onchange }: Props = $props();
</script>

<div
  class="segmented"
  role="radiogroup"
  aria-labelledby={labelId}
  aria-describedby={describedBy}
>
  {#each options as opt (opt.value)}
    <label class="option" class:selected={opt.value === value} class:disabled={disabled || opt.disabled}>
      <input
        type="radio"
        {name}
        value={opt.value}
        checked={opt.value === value}
        disabled={disabled || opt.disabled}
        onchange={() => onchange(opt.value)}
      />
      <span>{opt.label}</span>
    </label>
  {/each}
</div>

<style>
  .segmented {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 3px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    align-self: flex-start;
    max-width: 100%;
  }

  .option {
    position: relative;
    padding: 6px 12px;
    border-radius: 6px;
    color: var(--text-dim);
    cursor: pointer;
    user-select: none;
  }

  .option.selected {
    color: var(--text);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  .option.disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .option input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
    margin: 0;
    pointer-events: none;
  }

  .option:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
