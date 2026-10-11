<script lang="ts">
  interface Props {
    value: string;
    placeholder?: string;
    disabled?: boolean;
    id?: string;
    labelId?: string;
    /** Accessible name when there is no visible label to point at. */
    ariaLabel?: string;
    describedBy?: string;
    /**
     * When the value is written to the draft. `input` (default) on every keystroke, which suits
     * live previews. `change` on blur or Enter, which keeps list rows from re-rendering per key.
     */
    commitOn?: 'input' | 'change';
    onchange: (value: string) => void;
  }

  let {
    value,
    placeholder = '',
    disabled = false,
    id,
    labelId,
    ariaLabel,
    describedBy,
    commitOn = 'input',
    onchange,
  }: Props = $props();
</script>

<input
  type="text"
  class="text"
  spellcheck="false"
  autocomplete="off"
  {id}
  {placeholder}
  {disabled}
  {value}
  aria-labelledby={labelId}
  aria-label={ariaLabel}
  aria-describedby={describedBy}
  oninput={commitOn === 'input' ? (e) => onchange(e.currentTarget.value) : undefined}
  onchange={commitOn === 'change' ? (e) => onchange(e.currentTarget.value) : undefined}
/>

<style>
  .text {
    width: min(100%, 480px);
    padding: 7px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
  }

  .text:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .text:disabled {
    opacity: 0.5;
  }
</style>
