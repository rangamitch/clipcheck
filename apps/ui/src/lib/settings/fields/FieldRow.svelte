<script module lang="ts">
  let uidSeq = 0;
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';
  import { fieldState } from '../store';

  /** Ids the control must use so the label, help and error are announced with it. */
  export interface ControlIds {
    id: string;
    labelId: string;
    describedBy: string | undefined;
  }

  interface Props {
    label: string;
    /** Dotted config path, for example `storage.root`. Errors are matched by it. */
    path: string;
    help?: string;
    children: Snippet<[ControlIds]>;
  }

  let { label, path, help, children }: Props = $props();

  const uid = `settings-field-${++uidSeq}`;
  const labelId = `${uid}-label`;
  const helpId = `${uid}-help`;
  const errorId = `${uid}-error`;

  /** Only this row's state is subscribed to, so other settings writes do not re-render it. */
  const row = $derived(fieldState(path));
  const diag = $derived($row.diag);
  const changed = $derived($row.changed);
  const deferred = $derived($row.deferred);
  const describedBy = $derived(
    [help ? helpId : '', diag ? errorId : ''].filter(Boolean).join(' ') || undefined,
  );
</script>

<div class="field" class:invalid={diag?.severity === 'error'} class:changed>
  <div class="head">
    <span class="label" id={labelId}>{label}</span>
    {#if changed}
      <span class="sr-only">(changed)</span>
    {/if}
    {#if deferred}
      <span class="chip">Applies when recording stops</span>
    {/if}
  </div>
  {#if help}
    <p class="help" id={helpId}>{help}</p>
  {/if}
  <div class="control">
    {@render children({ id: `${uid}-control`, labelId, describedBy })}
  </div>
  {#if diag}
    <p class="message" class:warn={diag.severity === 'warning'} id={errorId}>{diag.message}</p>
  {/if}
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .label {
    font-weight: 600;
    color: var(--text);
  }

  .help {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }

  .chip {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 999px;
    border: 1px solid var(--accent);
    color: var(--text);
    background: var(--surface-2);
  }

  .message {
    margin: 0;
    font-size: 13px;
    color: var(--danger);
  }

  .message.warn {
    color: var(--text-dim);
  }

  .field.invalid .label {
    color: var(--danger);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
