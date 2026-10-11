<script lang="ts">
  export interface Option<T extends string> {
    value: T;
    label: string;
    /** Shown but not selectable, such as a codec that is not in v1. */
    disabled?: boolean;
    /** Tooltip, for example why an option is disabled. */
    title?: string;
  }

  interface Props {
    value: string;
    options: Option<string>[];
    disabled?: boolean;
    id?: string;
    labelId?: string;
    /** Accessible name when there is no visible label to point at. */
    ariaLabel?: string;
    describedBy?: string;
    onchange: (value: string) => void;
  }

  let { value, options, disabled = false, id, labelId, ariaLabel, describedBy, onchange }: Props = $props();
</script>

<select
  {id}
  {disabled}
  aria-labelledby={labelId}
  aria-label={ariaLabel}
  aria-describedby={describedBy}
  onchange={(e) => onchange(e.currentTarget.value)}
>
  {#each options as opt (opt.value)}
    <option
      value={opt.value}
      selected={opt.value === value}
      disabled={opt.disabled}
      title={opt.title}>{opt.label}</option
    >
  {/each}
</select>

<style>
  select {
    align-self: flex-start;
    min-width: 240px;
    max-width: 100%;
    padding: 7px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
  }

  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  select:disabled {
    opacity: 0.5;
  }
</style>
