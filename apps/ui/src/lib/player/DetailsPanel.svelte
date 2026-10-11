<script lang="ts">
  // Details column beside the player (D60, section D). Name and favourite are editable; the
  // rest is read-only. Rename and favourite are optimistic-free for rename (the parent decides
  // when the row changes) and optimistic for favourite (flipped locally, reverted on rejection).
  import { untrack } from 'svelte';
  import type { RecordingDto } from '../api';
  import { detailRows, splitName, validateStem } from './details';
  import Icon from './Icon.svelte';

  let {
    row,
    path,
    gameIcon,
    onrename,
    onfavorite,
    onreveal,
  }: {
    row: RecordingDto;
    path: string | null;
    gameIcon?: string;
    onrename: (stem: string) => Promise<void>;
    onfavorite: (value: boolean) => Promise<void>;
    onreveal: () => void;
  } = $props();

  const NAME_SAVED_MS = 2000;

  /** File stem (no extension) and the extension, derived from the row's file name. */
  const names = $derived(splitName(row.fileName));
  const stem = $derived(names.stem);
  const ext = $derived(names.ext);

  let draft = $state('');
  let nameInput: HTMLInputElement | undefined = $state();
  let saveState = $state<'idle' | 'saving' | 'saved'>('idle');
  let nameError = $state('');
  /** Last stem this panel asked the parent to save; blocks a repeat call before the row updates. */
  let lastRequested: string | null = null;
  let savedTimer: ReturnType<typeof setTimeout> | null = null;

  const dirty = $derived(draft !== stem);
  /** Live validation message for the draft; only shown while the draft differs from the stem. */
  const validation = $derived(dirty ? validateStem(draft) : null);

  /** Favourite state: flipped locally at once, reverted if the parent rejects. */
  let favorite = $state(false);
  let favBusy = $state(false);
  let favError = $state('');

  const rows = $derived(detailRows(row, null));

  // Resync the draft from the row whenever the stem changes, unless the user is typing in it.
  $effect(() => {
    const next = stem;
    untrack(() => {
      if (document.activeElement !== nameInput) {
        draft = next;
      }
    });
  });

  // Follow the row's favourite flag (the parent updates the row after a successful save).
  $effect(() => {
    const next = row.isFavorite;
    untrack(() => {
      favorite = next;
    });
  });

  $effect(() => {
    return () => {
      if (savedTimer !== null) clearTimeout(savedTimer);
    };
  });

  function messageOf(e: unknown): string {
    if (e instanceof Error && e.message) return e.message;
    if (typeof e === 'string' && e) return e;
    return 'Something went wrong.';
  }

  async function commitName(): Promise<void> {
    if (!dirty || saveState === 'saving') return;
    const name = draft.trim();
    if (name === lastRequested) return;
    const problem = validateStem(draft);
    if (problem !== null) return;

    lastRequested = name;
    nameError = '';
    saveState = 'saving';
    if (savedTimer !== null) {
      clearTimeout(savedTimer);
      savedTimer = null;
    }
    try {
      await onrename(name);
      draft = name;
      saveState = 'saved';
      savedTimer = setTimeout(() => {
        saveState = 'idle';
        savedTimer = null;
      }, NAME_SAVED_MS);
    } catch (e) {
      lastRequested = null;
      saveState = 'idle';
      nameError = messageOf(e);
    }
  }

  function onNameKeydown(e: KeyboardEvent): void {
    // The player binds global shortcuts (space, arrows); keep every key inside the field.
    e.stopPropagation();
    if (e.key === 'Enter') {
      e.preventDefault();
      void commitName();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      draft = stem;
      nameError = '';
      (e.currentTarget as HTMLInputElement).blur();
    }
  }

  function onNameInput(): void {
    nameError = '';
    lastRequested = null;
  }

  async function toggleFavorite(): Promise<void> {
    if (favBusy) return;
    const next = !favorite;
    favorite = next;
    favError = '';
    favBusy = true;
    try {
      await onfavorite(next);
    } catch (e) {
      favorite = !next;
      favError = messageOf(e);
    } finally {
      favBusy = false;
    }
  }
</script>

<section class="details" aria-labelledby="details-heading">
  <h2 id="details-heading" class="heading">Details</h2>

  <div class="group">
    <label class="label" for="details-name">Name</label>
    <div class="name-box">
      <input
        id="details-name"
        class="name-input"
        type="text"
        aria-label="Clip name"
        maxlength="200"
        autocomplete="off"
        spellcheck="false"
        bind:this={nameInput}
        bind:value={draft}
        oninput={onNameInput}
        onkeydown={onNameKeydown}
        onblur={() => void commitName()}
        aria-invalid={validation || nameError ? 'true' : undefined}
        aria-describedby="details-name-msg"
      />
      {#if ext}
        <span class="ext">{ext}</span>
      {/if}
    </div>
    <div class="name-meta" id="details-name-msg">
      {#if validation}
        <p class="error" role="alert">{validation}</p>
      {:else if nameError}
        <p class="error" role="alert">{nameError}</p>
      {:else if saveState === 'saving'}
        <p class="dim" role="status">Saving...</p>
      {:else if saveState === 'saved'}
        <p class="dim" role="status">Saved</p>
      {/if}
    </div>
  </div>

  <div class="group">
    <button
      type="button"
      class="fav"
      class:on={favorite}
      aria-pressed={favorite}
      disabled={favBusy}
      onclick={() => void toggleFavorite()}
    >
      <Icon name={favorite ? 'star-filled' : 'star'} size={18} />
      <span>{favorite ? 'Favourited' : 'Favourite'}</span>
    </button>
    {#if favError}
      <p class="error" role="alert">{favError}</p>
    {/if}
  </div>

  <dl class="facts">
    {#each rows as r (r.label)}
      <dt>{r.label}</dt>
      <dd>
        {#if r.label === 'Game'}
          <span class="game">
            {#if gameIcon}
              <img class="game-icon" src={'data:image/png;base64,' + gameIcon} alt="" width="24" height="24" />
            {/if}
            <span>{r.value}</span>
          </span>
        {:else}
          {r.value}
        {/if}
      </dd>
    {/each}
  </dl>

  <div class="group">
    <h3 class="label">File location</h3>
    {#if path}
      <button
        type="button"
        class="path"
        title={path}
        aria-label={`Show in folder: ${path}`}
        onclick={() => onreveal()}
      >
        <Icon name="folder" size={16} />
        <span class="path-text">{path}</span>
      </button>
      <p class="dim hint">Click to show in Explorer</p>
    {:else}
      <p class="dim">Locating file...</p>
    {/if}
  </div>
</section>

<style>
  .details {
    width: 300px;
    flex: 0 0 300px;
    height: 100%;
    overflow-y: auto;
    box-sizing: border-box;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 20px;
    background: var(--surface);
    border-left: 1px solid var(--border);
    color: var(--text);
  }

  .heading {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }

  .group {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }

  .label {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .name-box {
    display: flex;
    align-items: center;
    min-width: 0;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .name-box:focus-within {
    border-color: var(--accent);
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .name-input {
    flex: 1 1 auto;
    min-width: 0;
    padding: 8px 10px;
    background: transparent;
    border: none;
    outline: none;
    color: var(--text);
    font: inherit;
  }

  .ext {
    flex: 0 0 auto;
    padding: 8px 10px 8px 0;
    color: var(--text-dim);
    white-space: nowrap;
  }

  .name-meta {
    min-height: 1.2em;
  }

  .name-meta p {
    margin: 0;
    font-size: 13px;
  }

  .error {
    color: var(--danger);
  }

  .dim {
    color: var(--text-dim);
  }

  .fav {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    align-self: flex-start;
    padding: 6px 12px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font: inherit;
    cursor: pointer;
  }

  .fav.on {
    color: var(--accent);
    border-color: var(--accent);
  }

  .fav:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .fav:focus-visible,
  .path:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .facts {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px 16px;
    margin: 0;
    font-size: 14px;
  }

  .facts dt {
    color: var(--text-dim);
  }

  .facts dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .game {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .game-icon {
    width: 24px;
    height: 24px;
    flex: 0 0 24px;
    border-radius: 4px;
    object-fit: cover;
  }

  .path {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    width: 100%;
    padding: 8px 10px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-align: left;
    cursor: pointer;
    font: inherit;
  }

  .path:hover {
    border-color: var(--accent);
  }

  .path :global(svg) {
    flex: 0 0 auto;
    margin-top: 2px;
    color: var(--text-dim);
  }

  .path-text {
    min-width: 0;
    font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
    font-size: 12px;
    word-break: break-all;
  }

  .hint {
    margin: 0;
    font-size: 12px;
  }

  p {
    margin: 0;
  }
</style>
