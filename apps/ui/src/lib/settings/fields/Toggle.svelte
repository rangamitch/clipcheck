<script lang="ts">
  interface Props {
    checked: boolean;
    disabled?: boolean;
    id?: string;
    labelId?: string;
    describedBy?: string;
    onchange: (value: boolean) => void;
  }

  let { checked, disabled = false, id, labelId, describedBy, onchange }: Props = $props();
</script>

<button
  type="button"
  role="switch"
  class="switch"
  class:on={checked}
  {id}
  aria-checked={checked}
  aria-labelledby={labelId}
  aria-describedby={describedBy}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob" aria-hidden="true"></span>
</button>

<style>
  .switch {
    position: relative;
    width: 44px;
    height: 24px;
    padding: 0;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    cursor: pointer;
    transition: background 120ms ease;
  }

  .switch.on {
    background: var(--accent);
    border-color: var(--accent);
  }

  .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--text);
    transition: transform 120ms ease;
  }

  .switch.on .knob {
    transform: translateX(20px);
  }

  .switch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .switch:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
