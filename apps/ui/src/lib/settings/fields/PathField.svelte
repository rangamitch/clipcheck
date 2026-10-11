<script lang="ts">
  import { openFolder, pickFolder } from '../../recordingActions';

  interface Props {
    value: string;
    placeholder?: string;
    disabled?: boolean;
    id?: string;
    labelId?: string;
    describedBy?: string;
    /** Shows an "Open folder" button that opens the path in Explorer. */
    showOpen?: boolean;
    onchange: (value: string) => void;
  }

  let {
    value,
    placeholder = '',
    disabled = false,
    id,
    labelId,
    describedBy,
    showOpen = false,
    onchange,
  }: Props = $props();

  let error = $state<string | null>(null);

  function messageOf(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /** Opens the folder dialog, starting at the current path. A cancelled dialog changes nothing. */
  async function browse(): Promise<void> {
    error = null;
    try {
      const picked = await pickFolder(value.trim() || undefined);
      if (picked) onchange(picked);
    } catch (e) {
      error = `Could not open the folder picker: ${messageOf(e)}`;
    }
  }

  async function open(): Promise<void> {
    error = null;
    try {
      await openFolder(value);
    } catch (e) {
      error = `Could not open the folder: ${messageOf(e)}`;
    }
  }
</script>

<div class="path">
  <div class="row">
    <input
      type="text"
      spellcheck="false"
      autocomplete="off"
      {id}
      {placeholder}
      {disabled}
      {value}
      aria-labelledby={labelId}
      aria-describedby={describedBy}
      oninput={(e) => onchange(e.currentTarget.value)}
    />
    <button type="button" class="action" {disabled} onclick={() => void browse()}>Browse…</button>
    {#if showOpen}
      <button type="button" class="action" onclick={() => void open()}>Open folder</button>
    {/if}
  </div>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
</div>

<style>
  .path {
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: min(100%, 640px);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  input {
    flex: 1 1 280px;
    min-width: 0;
    padding: 7px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
  }

  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  input:disabled {
    opacity: 0.5;
  }

  .action {
    padding: 7px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }

  .action:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .action:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .error {
    margin: 0;
    font-size: 13px;
    color: var(--danger);
  }
</style>
