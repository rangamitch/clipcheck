<script lang="ts">
  import { addToList, defaultNormalize, type Normalizer } from '../pure';

  interface Props {
    items: string[];
    /** Validates and normalises one entry. Defaults to trim plus non-empty. */
    normalize?: Normalizer;
    placeholder?: string;
    /** Noun for the remove buttons, for example "executable". */
    itemNoun?: string;
    disabled?: boolean;
    id?: string;
    labelId?: string;
    describedBy?: string;
    onchange: (items: string[]) => void;
  }

  let {
    items,
    normalize = defaultNormalize,
    placeholder = '',
    itemNoun = 'entry',
    disabled = false,
    id,
    labelId,
    describedBy,
    onchange,
  }: Props = $props();

  let entry = $state('');
  let error = $state<string | null>(null);
  const errorId = $derived(`${id ?? 'list'}-error`);

  /** Adds the typed entry. Refused entries keep the text and show why. */
  function add(): void {
    const result = addToList(items, entry, normalize);
    if (!result.ok) {
      error = result.error;
      return;
    }
    onchange(result.list);
    entry = '';
    error = null;
  }

  function remove(index: number): void {
    onchange(items.filter((_, i) => i !== index));
    error = null;
  }
</script>

<div class="list">
  <div class="entry-row">
    <input
      type="text"
      spellcheck="false"
      autocomplete="off"
      {id}
      {placeholder}
      {disabled}
      value={entry}
      aria-labelledby={labelId}
      aria-describedby={error ? errorId : describedBy}
      aria-invalid={error !== null}
      oninput={(e) => {
        entry = e.currentTarget.value;
        error = null;
      }}
      onkeydown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          add();
        }
      }}
    />
    <button type="button" class="add" {disabled} onclick={add}>Add</button>
  </div>

  {#if error}
    <p class="message" id={errorId} role="alert">{error}</p>
  {/if}

  {#if items.length === 0}
    <p class="empty">None yet.</p>
  {:else}
    <ul>
      <!-- Keyed by position: a hand-edited config may hold the same entry twice, and a text key would throw. -->
      {#each items as item, i (i)}
        <li>
          <span class="name">{item}</span>
          <button
            type="button"
            class="remove"
            {disabled}
            aria-label={`Remove ${itemNoun} ${item}`}
            onclick={() => remove(i)}
          >
            Remove
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(100%, 480px);
  }

  .entry-row {
    display: flex;
    gap: 8px;
  }

  input {
    flex: 1 1 auto;
    min-width: 0;
    padding: 7px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
  }

  input[aria-invalid='true'] {
    border-color: var(--danger);
  }

  button {
    padding: 6px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }

  button:hover:not(:disabled) {
    border-color: var(--accent);
  }

  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  button:disabled,
  input:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .message {
    margin: 0;
    font-size: 13px;
    color: var(--danger);
  }

  .empty {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 10px;
    border-radius: var(--radius);
    background: var(--surface);
    border: 1px solid var(--border);
  }

  .name {
    font-family: ui-monospace, Consolas, monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .remove {
    padding: 3px 8px;
    font-size: 12px;
    color: var(--text-dim);
  }
</style>
