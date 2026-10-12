<script lang="ts">
  import { runAction, type ActionContext } from './actions';
  import { ACTIONS } from './shortcuts';
  import { dispatch, project, selection, snapshot, tool, undo, redo, zoomBy, type EditorTool } from './store';

  interface Props {
    /** Opens the export dialog. */
    onexport: () => void;
    /** Opens the shortcuts overlay. */
    onshortcuts: () => void;
    /** Leaves the editor for the projects list. */
    onhome: () => void;
    /** Width of the track rows in px for zoom to fit (the header column excluded). Defaults to the window width. */
    viewWidth?: number;
    /** True while a source media is open in the source monitor. */
    hasSource: boolean;
  }

  let { onexport, onshortcuts, onhome, viewWidth, hasSource }: Props = $props();

  /** Name being typed, or null when the name input is not focused. */
  let editingName = $state<string | null>(null);

  const TOOLS: { id: EditorTool; label: string; key: string; glyph: string }[] = [
    { id: 'select', label: 'Selection tool', key: 'V', glyph: 'V' },
    { id: 'razor', label: 'Razor tool', key: 'C', glyph: 'C' },
    { id: 'hand', label: 'Hand tool', key: 'H', glyph: 'H' },
    { id: 'zoom', label: 'Zoom tool', key: 'Z', glyph: 'Z' },
  ];

  /** Context for actions started from buttons. */
  function ctx(): ActionContext {
    return {
      openExport: onexport,
      showShortcuts: onshortcuts,
      viewWidth: viewWidth !== undefined && viewWidth > 0 ? viewWidth : window.innerWidth,
      hasSource,
    };
  }

  /** Name shown in the input: the draft while editing, otherwise the project name. */
  const displayName = $derived(editingName ?? $project?.name ?? '');
  const hasSelection = $derived($selection.length > 0);

  /** Starts editing the project name. */
  function startName(): void {
    editingName = $project?.name ?? '';
  }

  /** Commits the name draft with a rename command when it changed and is not empty. */
  function commitName(): void {
    if (editingName === null) return;
    const next = editingName.trim();
    const current = $project?.name ?? '';
    editingName = null;
    if (next.length > 0 && next !== current) {
      void dispatch({ type: 'rename_project', name: next });
    }
  }

  /** Enter commits and leaves the field; Escape discards the draft. */
  function onNameKey(e: KeyboardEvent): void {
    const el = e.currentTarget;
    if (!(el instanceof HTMLInputElement)) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      el.blur();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      editingName = null;
      el.blur();
    }
  }

  /** Sets one of the editing modes, keeping the other one. */
  function setMode(which: 'magnetic' | 'snap'): void {
    const modes = $project?.modes;
    if (!modes) return;
    void dispatch({
      type: 'set_modes',
      magnetic: which === 'magnetic' ? !modes.magnetic : modes.magnetic,
      snap: which === 'snap' ? !modes.snap : modes.snap,
    });
  }
</script>

<div class="toolbar" role="toolbar" aria-label="Editor toolbar">
  <button class="btn icon" type="button" aria-label="Back to projects" title="Back to projects" onclick={onhome}>
    <span aria-hidden="true">&#8962;</span>
  </button>

  <input
    class="name"
    type="text"
    aria-label="Project name"
    title="Project name"
    value={displayName}
    onfocus={startName}
    oninput={(e) => (editingName = e.currentTarget.value)}
    onblur={commitName}
    onkeydown={onNameKey}
    maxlength="120"
  />

  <div class="group" role="group" aria-label="History">
    <button
      class="btn icon"
      type="button"
      aria-label="Undo"
      title="Undo (Ctrl+Z)"
      disabled={!$snapshot?.can_undo}
      onclick={() => void undo()}
    >
      <span aria-hidden="true">&#8630;</span>
    </button>
    <button
      class="btn icon"
      type="button"
      aria-label="Redo"
      title="Redo (Ctrl+Y)"
      disabled={!$snapshot?.can_redo}
      onclick={() => void redo()}
    >
      <span aria-hidden="true">&#8631;</span>
    </button>
  </div>

  <div class="group" role="group" aria-label="Tools">
    {#each TOOLS as t (t.id)}
      <button
        class="btn tool"
        class:active={$tool === t.id}
        type="button"
        aria-label={t.label}
        aria-pressed={$tool === t.id}
        title={`${t.label} (${t.key})`}
        onclick={() => tool.set(t.id)}
      >
        {t.glyph}
      </button>
    {/each}
  </div>

  <div class="group" role="group" aria-label="Editing modes">
    <button
      class="btn text"
      type="button"
      aria-pressed={$project?.modes.magnetic ?? false}
      class:active={$project?.modes.magnetic ?? false}
      title="Magnetic: deleting and moving closes gaps"
      disabled={$project === null}
      onclick={() => setMode('magnetic')}
    >
      Magnetic
    </button>
    <button
      class="btn text"
      type="button"
      aria-pressed={$project?.modes.snap ?? false}
      class:active={$project?.modes.snap ?? false}
      title="Snap edits to clip edges, the playhead and markers (N)"
      disabled={$project === null}
      onclick={() => setMode('snap')}
    >
      Snap
    </button>
  </div>

  <div class="group" role="group" aria-label="Clip actions">
    <button class="btn text" type="button" aria-label="Split at playhead" title="Split at playhead (Ctrl+K). With no selection, splits the targeted tracks" onclick={() => void runAction(ACTIONS.split, ctx())}>
      Split
    </button>
    <button class="btn text" type="button" aria-label="Delete selection" title="Delete, leaves a gap (Delete)" disabled={!hasSelection} onclick={() => void runAction(ACTIONS.delete, ctx())}>
      Delete
    </button>
    <button class="btn text" type="button" aria-label="Group selection" title="Group (Ctrl+G)" disabled={!hasSelection} onclick={() => void runAction(ACTIONS.group, ctx())}>
      Group
    </button>
    <button class="btn text" type="button" aria-label="Ungroup selection" title="Ungroup (Ctrl+Shift+G)" disabled={!hasSelection} onclick={() => void runAction(ACTIONS.ungroup, ctx())}>
      Ungroup
    </button>
  </div>

  <div class="group" role="group" aria-label="Zoom">
    <button class="btn icon" type="button" aria-label="Zoom out" title="Zoom out (-)" onclick={() => zoomBy(0.8)}>
      <span aria-hidden="true">&minus;</span>
    </button>
    <button class="btn icon" type="button" aria-label="Zoom in" title="Zoom in (+)" onclick={() => zoomBy(1.25)}>
      <span aria-hidden="true">+</span>
    </button>
    <button class="btn text" type="button" aria-label="Zoom to fit" title="Zoom to fit (\)" onclick={() => void runAction(ACTIONS.zoom_fit, ctx())}>
      Fit
    </button>
  </div>

  <div class="spacer"></div>

  {#if $snapshot}
    <span class="status" class:dirty={$snapshot.dirty} role="status" aria-live="polite">
      {$snapshot.dirty ? 'Unsaved changes' : 'Saved'}
    </span>
  {/if}

  <button class="btn primary" type="button" aria-label="Export" title="Export (Ctrl+E)" onclick={onexport}>
    Export
  </button>
  <button class="btn icon" type="button" aria-label="Keyboard shortcuts" title="Keyboard shortcuts (?)" onclick={onshortcuts}>
    <span aria-hidden="true">?</span>
  </button>
</div>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: nowrap;
    height: 44px;
    padding: 0 10px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    min-width: 0;
    overflow-x: auto;
  }

  .group {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
    padding-right: 10px;
    border-right: 1px solid var(--border);
  }

  .group:last-of-type {
    border-right: none;
  }

  .name {
    flex: 0 1 220px;
    min-width: 120px;
    height: 28px;
    padding: 0 8px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    font: inherit;
  }

  .name:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn {
    height: 28px;
    min-width: 28px;
    padding: 0 8px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
  }

  .btn.icon {
    font-size: 16px;
    line-height: 1;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .btn.active {
    border-color: var(--accent);
    color: var(--text);
    box-shadow: inset 0 -2px 0 var(--accent);
  }

  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
    font-weight: 600;
    padding: 0 14px;
  }

  .btn:focus-visible,
  .name:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .spacer {
    flex: 1 1 auto;
    min-width: 8px;
  }

  .status {
    color: var(--text-dim);
    font-size: 12px;
    white-space: nowrap;
  }

  .status.dirty {
    color: var(--danger);
  }
</style>
