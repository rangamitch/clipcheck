<script lang="ts">
  import { onMount } from 'svelte';
  import { openEditor } from '../nav';
  import { editorApi } from './api';
  import { formatDuration, msToTick } from './ticks';
  import type { ProjectSummary } from './types';
  import MediaPicker from './MediaPicker.svelte';

  let projects = $state<ProjectSummary[]>([]);
  let loading = $state(true);
  /** Error from the last list load or action, shown inline above the list. */
  let errorText = $state('');
  /** Project id whose action is running, or null. */
  let busyId = $state<string | null>(null);

  /** Project being renamed inline, or null. */
  let renamingId = $state<string | null>(null);
  let renameText = $state('');
  let renameError = $state('');

  /** Project waiting for delete confirmation, or null. */
  let confirmDeleteId = $state<string | null>(null);

  /** Project whose Open action is waiting for an autosave choice, or null. */
  let choiceId = $state<string | null>(null);

  let pickerOpen = $state(false);

  /** Loads the project list. */
  async function reload(): Promise<void> {
    loading = true;
    try {
      projects = await editorApi.listProjects();
    } catch (e) {
      errorText = `Could not load projects: ${String(e)}`;
    } finally {
      loading = false;
    }
  }

  /**
   * Runs one action on a project, reports failures inline and reloads the list afterwards.
   * Returns true when the action succeeded.
   */
  async function act(id: string, work: () => Promise<void>): Promise<boolean> {
    busyId = id;
    errorText = '';
    let ok = false;
    try {
      await work();
      ok = true;
    } catch (e) {
      errorText = String(e);
    } finally {
      busyId = null;
    }
    await reload();
    return ok;
  }

  function open(p: ProjectSummary): void {
    openEditor({ projectId: p.id, recordingIds: [] });
  }

  /**
   * Open action. A project with a newer autosave asks first: opening it would otherwise leave the
   * autosave in place, and the next autosave would overwrite the recoverable work.
   */
  function openOrChoose(p: ProjectSummary): void {
    if (p.hasAutosave) {
      confirmDeleteId = null;
      choiceId = p.id;
      return;
    }
    open(p);
  }

  /**
   * Discards the autosave (deleting its file) and then opens the last saved version of the project.
   */
  async function openLastSaved(p: ProjectSummary): Promise<void> {
    busyId = p.id;
    errorText = '';
    try {
      await editorApi.closeProject(true);
    } catch (e) {
      errorText = `Could not discard the autosave of "${p.name}": ${String(e)}`;
      busyId = null;
      return;
    }
    busyId = null;
    choiceId = null;
    open(p);
  }

  /**
   * Recovers the autosave permanently: the recovered session is saved over the project file
   * before the editor opens by id, so the editor's reopen gets the recovered content.
   */
  async function recover(p: ProjectSummary): Promise<void> {
    busyId = p.id;
    choiceId = null;
    errorText = '';
    try {
      await editorApi.recoverAutosave(p.id);
      await editorApi.saveProject();
    } catch (e) {
      errorText = `Could not recover "${p.name}": ${String(e)}`;
      busyId = null;
      await reload();
      return;
    }
    busyId = null;
    openEditor({ projectId: p.id, recordingIds: [] });
  }

  function startRename(p: ProjectSummary): void {
    confirmDeleteId = null;
    renamingId = p.id;
    renameText = p.name;
    renameError = '';
  }

  function cancelRename(): void {
    renamingId = null;
    renameError = '';
  }

  async function commitRename(p: ProjectSummary): Promise<void> {
    const name = renameText.trim();
    if (name.length === 0) {
      renameError = 'Name cannot be empty.';
      return;
    }
    if (name === p.name) {
      cancelRename();
      return;
    }
    const ok = await act(p.id, () => editorApi.renameProject(p.id, name));
    if (ok) cancelRename();
  }

  function onRenameKey(e: KeyboardEvent, p: ProjectSummary): void {
    if (e.key === 'Enter') {
      e.preventDefault();
      void commitRename(p);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelRename();
    }
  }

  async function duplicate(p: ProjectSummary): Promise<void> {
    await act(p.id, async () => {
      await editorApi.duplicateProject(p.id);
    });
  }

  async function confirmDelete(p: ProjectSummary): Promise<void> {
    const ok = await act(p.id, () => editorApi.deleteProject(p.id));
    if (ok) confirmDeleteId = null;
  }

  function newEmpty(): void {
    openEditor({ projectId: null, recordingIds: [] });
  }

  function onPicked(recordingIds: string[]): void {
    pickerOpen = false;
    if (recordingIds.length > 0) openEditor({ projectId: null, recordingIds });
  }

  /** Focuses the node when it mounts (used for the rename field). */
  function focusOnMount(node: HTMLElement): void {
    node.focus();
  }

  function formatModified(ms: number): string {
    return Number.isFinite(ms) && ms > 0 ? new Date(ms).toLocaleString() : 'Unknown';
  }

  onMount(() => {
    void reload();
  });
</script>

<section class="home" aria-labelledby="editor-home-title">
  <header class="top">
    <div>
      <h1 id="editor-home-title">Editor</h1>
      <p class="intro">Edit your recordings in projects. Autosaves keep unsaved work safe.</p>
    </div>
    <div class="top-actions">
      <button type="button" onclick={newEmpty}>New empty project</button>
      <button type="button" class="primary" onclick={() => (pickerOpen = true)}>
        New from library clips
      </button>
    </div>
  </header>

  {#if errorText}
    <p class="error" role="alert">{errorText}</p>
  {/if}

  {#if loading && projects.length === 0}
    <p class="status" role="status">Loading projects...</p>
  {:else if projects.length === 0}
    <div class="empty">
      <p class="empty-title">No projects yet</p>
      <p>Start an empty project, or build one from clips in your library.</p>
    </div>
  {:else}
    <ul class="grid" aria-label="Projects">
      {#each projects as p (p.id)}
        <li class="card" class:busy={busyId === p.id}>
          <div class="card-head">
            {#if renamingId === p.id}
              <div class="rename">
                <input
                  type="text"
                  aria-label="New project name"
                  bind:value={renameText}
                  use:focusOnMount
                  onkeydown={(e) => onRenameKey(e, p)}
                  disabled={busyId === p.id}
                />
                <div class="rename-actions">
                  <button type="button" disabled={busyId === p.id} onclick={() => void commitRename(p)}>
                    Save name
                  </button>
                  <button type="button" class="ghost" onclick={cancelRename}>Cancel</button>
                </div>
                {#if renameError}
                  <p class="field-error" role="alert">{renameError}</p>
                {/if}
              </div>
            {:else}
              <h2 class="name" title={p.name}>{p.name}</h2>
            {/if}
          </div>

          <div class="badges">
            {#if p.hasAutosave}
              <span class="badge recover">Recovered autosave available</span>
            {/if}
            {#if p.broken}
              <span class="badge damaged">Damaged</span>
            {/if}
          </div>

          <dl class="facts">
            <div>
              <dt>Modified</dt>
              <dd>{formatModified(p.modifiedMs)}</dd>
            </div>
            <div>
              <dt>Duration</dt>
              <dd>{formatDuration(msToTick(p.durationMs))}</dd>
            </div>
            <div>
              <dt>Clips</dt>
              <dd>{p.clipCount}</dd>
            </div>
            <div>
              <dt>Canvas</dt>
              <dd>{p.width} x {p.height}</dd>
            </div>
          </dl>

          {#if confirmDeleteId === p.id}
            <div class="confirm" role="group" aria-label={`Confirm delete ${p.name}`}>
              <p>Delete "{p.name}"? It moves to the Recycle Bin.</p>
              <div class="actions">
                <button
                  type="button"
                  class="danger"
                  disabled={busyId === p.id}
                  onclick={() => void confirmDelete(p)}
                >
                  Delete
                </button>
                <button type="button" class="ghost" onclick={() => (confirmDeleteId = null)}>
                  Cancel
                </button>
              </div>
            </div>
          {:else if choiceId === p.id}
            <div class="confirm" role="group" aria-label={`Autosave choice for ${p.name}`}>
              <p>
                "{p.name}" has an autosave newer than the last save. Recover it, or open the last
                saved version and discard the autosave?
              </p>
              <div class="actions">
                <button
                  type="button"
                  class="primary"
                  aria-label={`Recover autosave of ${p.name}`}
                  disabled={busyId === p.id}
                  onclick={() => void recover(p)}
                >
                  Recover autosave
                </button>
                <button
                  type="button"
                  aria-label={`Open last saved version of ${p.name}`}
                  disabled={busyId === p.id}
                  onclick={() => void openLastSaved(p)}
                >
                  Open last saved
                </button>
                <button type="button" class="ghost" onclick={() => (choiceId = null)}>
                  Cancel
                </button>
              </div>
            </div>
          {:else}
            <div class="actions">
              {#if !p.broken}
                <button
                  type="button"
                  class="primary"
                  aria-label={`Open ${p.name}`}
                  disabled={busyId === p.id}
                  onclick={() => openOrChoose(p)}
                >
                  Open
                </button>
              {/if}
              {#if p.broken && p.hasAutosave}
                <button
                  type="button"
                  aria-label={`Recover autosave of ${p.name}`}
                  disabled={busyId === p.id}
                  onclick={() => void recover(p)}
                >
                  Recover
                </button>
              {/if}
              {#if !p.broken && renamingId !== p.id}
                <button
                  type="button"
                  aria-label={`Rename ${p.name}`}
                  disabled={busyId === p.id}
                  onclick={() => startRename(p)}
                >
                  Rename
                </button>
                <button
                  type="button"
                  aria-label={`Duplicate ${p.name}`}
                  disabled={busyId === p.id}
                  onclick={() => void duplicate(p)}
                >
                  Duplicate
                </button>
              {/if}
              <button
                type="button"
                class="ghost danger-text"
                aria-label={`Delete ${p.name}`}
                disabled={busyId === p.id}
                onclick={() => {
                  renamingId = null;
                  confirmDeleteId = p.id;
                }}
              >
                Delete
              </button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<MediaPicker open={pickerOpen} multiple={true} onpick={onPicked} onclose={() => (pickerOpen = false)} />

<style>
  .home {
    height: 100%;
    overflow-y: auto;
    padding: 24px;
    box-sizing: border-box;
    background: var(--bg);
    color: var(--text);
  }

  .top {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
  }

  h1 {
    margin: 0 0 4px;
    font-size: 22px;
    font-weight: 600;
  }

  .intro {
    margin: 0;
    color: var(--text-dim);
  }

  .top-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .error {
    margin: 0 0 16px;
    padding: 8px 12px;
    border: 1px solid var(--danger);
    border-radius: var(--radius);
    color: var(--danger);
  }

  .status,
  .empty {
    color: var(--text-dim);
  }

  .empty {
    padding: 32px 16px;
    text-align: center;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }

  .empty-title {
    margin: 0 0 6px;
    color: var(--text);
    font-weight: 600;
  }

  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 14px;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    min-width: 0;
  }

  .card.busy {
    opacity: 0.7;
  }

  .name {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rename {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .rename input {
    width: 100%;
    padding: 6px 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .rename-actions {
    display: flex;
    gap: 6px;
  }

  .field-error {
    margin: 0;
    color: var(--danger);
    font-size: 12px;
  }

  .badges {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .badge {
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 12px;
    border: 1px solid var(--border);
    color: var(--text-dim);
  }

  .badge.recover {
    color: var(--text);
    background: var(--surface-2);
  }

  .badge.damaged {
    color: var(--danger);
    border-color: var(--danger);
  }

  .facts {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px 12px;
    margin: 0;
  }

  .facts div {
    min-width: 0;
  }

  dt {
    color: var(--text-dim);
    font-size: 12px;
  }

  dd {
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .confirm p {
    margin: 0 0 8px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: auto;
  }

  button {
    padding: 6px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  button.danger {
    background: var(--danger);
    border-color: var(--danger);
    color: #1b0d0d;
  }

  button.ghost {
    background: none;
  }

  button.danger-text {
    color: var(--danger);
  }

  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
