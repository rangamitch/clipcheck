<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { debounce } from './debounce';
  import {
    COARSE_MS,
    DEFAULT_MIN_MS,
    FRAME_MS,
    clampSelection,
    estimateOutputBytes,
    formatHmsMs,
    nudge,
    parseTimecode,
    setIn,
    setOut,
    snapReadout,
    trimKeyToAction,
    type HandleKind,
    type TrimMode,
    type TrimSelection,
  } from './player/trim';
  import {
    cancelTrim,
    getTrimPreview,
    onTrimDone,
    onTrimProgress,
    startTrim,
    type TrimDone,
    type TrimPreview,
    type TrimProgress,
  } from './trimapi';

  interface Props {
    id: string;
    durationMs: number;
    positionMs: number;
    /** Optional source size; when given, an approximate output size is shown. */
    sizeBytes?: number;
    onclose: () => void;
    seek: (ms: number) => void;
  }

  let { id, durationMs, positionMs, sizeBytes, onclose, seek }: Props = $props();

  type Status = 'idle' | 'running' | 'done';

  const PREVIEW_DEBOUNCE_MS = 150;

  // The full clip is the starting selection; the trimmer is remounted per clip.
  let sel = $state<TrimSelection>(
    untrack(() => clampSelection({ startMs: 0, endMs: durationMs }, durationMs)),
  );
  let mode = $state<TrimMode>('lossless');
  let focused = $state<HandleKind>('start');
  let dragging = $state<HandleKind | null>(null);
  let status = $state<Status>('idle');
  let jobId = $state<string | null>(null);
  let percent = $state(0);
  let outTimeMs = $state(0);
  let preview = $state<TrimPreview | null>(null);
  let previewKey = $state('');
  let errorMessage = $state('');
  let trackEl = $state<HTMLDivElement | undefined>(undefined);
  let startDraft = $state('');
  let endDraft = $state('');

  const running = $derived(status === 'running');
  const dur = $derived(Number.isFinite(durationMs) && durationMs > 0 ? durationMs : 0);
  const valid = $derived(
    dur > 0 &&
      sel.startMs >= 0 &&
      sel.endMs <= dur &&
      sel.endMs - sel.startMs >= DEFAULT_MIN_MS,
  );
  const spanMs = $derived(Math.max(0, sel.endMs - sel.startMs));

  const currentKey = $derived(`${sel.startMs}:${sel.endMs}:${mode}`);
  /** Preview is only shown when it belongs to the current selection and mode. */
  const currentPreview = $derived(preview !== null && previewKey === currentKey ? preview : null);

  const readout = $derived.by(() => {
    if (mode === 'precise') {
      return `Requested ${formatHmsMs(sel.startMs)} – ${formatHmsMs(sel.endMs)}, exact cut (re-encode)`;
    }
    if (currentPreview) {
      return snapReadout(sel, {
        startMs: currentPreview.actualStartMs,
        endMs: currentPreview.actualEndMs,
      });
    }
    return `Requested ${formatHmsMs(sel.startMs)} – ${formatHmsMs(sel.endMs)}, checking keyframes…`;
  });

  const sizeText = $derived.by(() => {
    if (sizeBytes === undefined || !valid) {
      return '';
    }
    const bytes = estimateOutputBytes(sizeBytes, dur, sel);
    return `about ${(bytes / 1_000_000).toFixed(1)} MB`;
  });

  function toPct(ms: number): number {
    return dur > 0 ? (ms / dur) * 100 : 0;
  }

  function errorText(err: unknown): string {
    if (err instanceof Error) {
      return err.message;
    }
    if (typeof err === 'string') {
      return err;
    }
    return 'Something went wrong.';
  }

  // Keep the time inputs in step with the handles unless the user is typing in them.
  $effect(() => {
    startDraft = formatHmsMs(sel.startMs);
  });
  $effect(() => {
    endDraft = formatHmsMs(sel.endMs);
  });

  // Live snap readout: re-query the planner after the selection or mode changes.
  let previewSeq = 0;

  async function refreshPreview(): Promise<void> {
    const seq = ++previewSeq;
    const key = currentKey;
    if (mode !== 'lossless' || !valid) {
      return;
    }
    const req = { id, startMs: sel.startMs, endMs: sel.endMs, mode };
    try {
      const result = await getTrimPreview(req);
      if (seq !== previewSeq) {
        return;
      }
      preview = result;
      previewKey = key;
    } catch (err) {
      if (seq !== previewSeq) {
        return;
      }
      preview = null;
      previewKey = '';
      errorMessage = errorText(err);
    }
  }

  const previewDebounce = debounce(() => {
    void refreshPreview();
  }, PREVIEW_DEBOUNCE_MS);

  $effect(() => {
    // Reading these registers the dependencies; any change restarts the debounce.
    void id;
    void sel.startMs;
    void sel.endMs;
    void mode;
    previewDebounce.call();
    return () => previewDebounce.cancel();
  });

  // Progress and completion events. Only the running job's events are accepted.
  function acceptsJob(incoming: string): boolean {
    if (status !== 'running') {
      return false;
    }
    if (jobId === null) {
      // Events can arrive before startTrim resolves; adopt the first id (one job at a time).
      jobId = incoming;
      return true;
    }
    return jobId === incoming;
  }

  function onProgress(p: TrimProgress): void {
    if (!acceptsJob(p.jobId)) {
      return;
    }
    percent = Math.min(100, Math.max(0, Number.isFinite(p.percent) ? p.percent : 0));
    outTimeMs = Number.isFinite(p.outTimeMs) ? p.outTimeMs : 0;
  }

  function onDone(d: TrimDone): void {
    if (!acceptsJob(d.jobId)) {
      return;
    }
    jobId = null;
    if (d.ok) {
      status = 'done';
      percent = 100;
    } else {
      status = 'idle';
      percent = 0;
      errorMessage = d.error ?? 'The trim did not finish.';
    }
  }

  onMount(() => {
    const subscriptions = Promise.all([onTrimProgress(onProgress), onTrimDone(onDone)]);
    subscriptions.catch((err: unknown) => {
      errorMessage = errorText(err);
    });
    return () => {
      void subscriptions.then((fns) => fns.forEach((unlisten) => unlisten())).catch(() => undefined);
    };
  });

  async function save(): Promise<void> {
    if (running || !valid) {
      return;
    }
    errorMessage = '';
    status = 'running';
    percent = 0;
    outTimeMs = 0;
    jobId = null;
    try {
      const started = await startTrim({
        id,
        startMs: sel.startMs,
        endMs: sel.endMs,
        mode,
        overwrite: false,
      });
      if (status === 'running' && jobId === null) {
        jobId = started.jobId;
      }
    } catch (err) {
      if (status === 'running') {
        status = 'idle';
        jobId = null;
        errorMessage = errorText(err);
      }
    }
  }

  async function cancel(): Promise<void> {
    if (!running) {
      onclose();
      return;
    }
    if (jobId === null) {
      return;
    }
    try {
      await cancelTrim(jobId);
    } catch (err) {
      errorMessage = errorText(err);
    }
  }

  function setHandle(which: HandleKind, ms: number): void {
    sel = which === 'start' ? setIn(sel, ms, dur) : setOut(sel, ms, dur);
  }

  function commitTime(which: HandleKind, text: string): void {
    const ms = parseTimecode(text);
    if (ms === null) {
      errorMessage = `"${text}" is not a valid time. Use hh:mm:ss.mmm, mm:ss or ss.`;
      if (which === 'start') {
        startDraft = formatHmsMs(sel.startMs);
      } else {
        endDraft = formatHmsMs(sel.endMs);
      }
      return;
    }
    errorMessage = '';
    setHandle(which, ms);
  }

  function onKey(e: KeyboardEvent): void {
    if (e.target instanceof HTMLInputElement) {
      return;
    }
    const action = trimKeyToAction(e);
    if (!action) {
      return;
    }
    e.preventDefault();
    if (action.type === 'save') {
      void save();
      return;
    }
    if (running) {
      return;
    }
    if (action.type === 'set-in') {
      sel = setIn(sel, positionMs, dur);
    } else if (action.type === 'set-out') {
      sel = setOut(sel, positionMs, dur);
    } else {
      const step = action.coarse ? COARSE_MS : FRAME_MS;
      sel = nudge(sel, focused, action.dir * step, dur);
    }
  }

  function onHandleKey(which: HandleKind, e: KeyboardEvent): void {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') {
      return;
    }
    e.preventDefault();
    if (running) {
      return;
    }
    const dir = e.key === 'ArrowLeft' ? -1 : 1;
    sel = nudge(sel, which, dir * (e.shiftKey ? COARSE_MS : FRAME_MS), dur);
  }

  function msFromClientX(clientX: number): number {
    const rect = trackEl?.getBoundingClientRect();
    if (!rect || rect.width <= 0) {
      return 0;
    }
    const frac = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
    return frac * dur;
  }

  function onHandleDown(which: HandleKind, e: PointerEvent): void {
    if (running) {
      return;
    }
    e.preventDefault();
    focused = which;
    dragging = which;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onHandleMove(e: PointerEvent): void {
    if (dragging === null) {
      return;
    }
    setHandle(dragging, msFromClientX(e.clientX));
  }

  function onHandleUp(): void {
    dragging = null;
  }
</script>

<section
  class="trimmer"
  aria-label="Clip trimmer"
  onkeydown={onKey}
  tabindex="-1"
>
  <div class="timeline" bind:this={trackEl}>
    <div class="range" style="left: {toPct(sel.startMs)}%; width: {Math.max(0, toPct(sel.endMs) - toPct(sel.startMs))}%"></div>
    <div
      class="handle in"
      class:focused={focused === 'start'}
      style="left: {toPct(sel.startMs)}%"
      role="slider"
      tabindex={running ? -1 : 0}
      aria-label="Start"
      aria-valuemin={0}
      aria-valuemax={Math.round(dur)}
      aria-valuenow={Math.round(sel.startMs)}
      aria-valuetext={formatHmsMs(sel.startMs)}
      aria-disabled={running}
      onpointerdown={(e) => onHandleDown('start', e)}
      onpointermove={onHandleMove}
      onpointerup={onHandleUp}
      onpointercancel={onHandleUp}
      onkeydown={(e) => onHandleKey('start', e)}
      onfocus={() => (focused = 'start')}
    >
      <span class="label">{formatHmsMs(sel.startMs)}</span>
    </div>
    <div
      class="handle out"
      class:focused={focused === 'end'}
      style="left: {toPct(sel.endMs)}%"
      role="slider"
      tabindex={running ? -1 : 0}
      aria-label="End"
      aria-valuemin={0}
      aria-valuemax={Math.round(dur)}
      aria-valuenow={Math.round(sel.endMs)}
      aria-valuetext={formatHmsMs(sel.endMs)}
      aria-disabled={running}
      onpointerdown={(e) => onHandleDown('end', e)}
      onpointermove={onHandleMove}
      onpointerup={onHandleUp}
      onpointercancel={onHandleUp}
      onkeydown={(e) => onHandleKey('end', e)}
      onfocus={() => (focused = 'end')}
    >
      <span class="label">{formatHmsMs(sel.endMs)}</span>
    </div>
  </div>

  <div class="times">
    <label>
      Start
      <input
        type="text"
        aria-label="Start time"
        bind:value={startDraft}
        disabled={running}
        onchange={() => commitTime('start', startDraft)}
        onkeydown={(e) => e.stopPropagation()}
      />
    </label>
    <label>
      End
      <input
        type="text"
        aria-label="End time"
        bind:value={endDraft}
        disabled={running}
        onchange={() => commitTime('end', endDraft)}
        onkeydown={(e) => e.stopPropagation()}
      />
    </label>
    <span class="span">Length {formatHmsMs(spanMs)}</span>
    <button type="button" class="control" onclick={() => seek(sel.startMs)}>Go to in</button>
    <button type="button" class="control" onclick={() => seek(sel.endMs)}>Go to out</button>
  </div>

  <div class="modes" role="group" aria-label="Trim mode">
    <button
      type="button"
      class="control"
      aria-pressed={mode === 'lossless'}
      disabled={running}
      onclick={() => (mode = 'lossless')}>Lossless (stream copy)</button
    >
    <button
      type="button"
      class="control"
      aria-pressed={mode === 'precise'}
      disabled={running}
      onclick={() => (mode = 'precise')}>Precise (re-encode)</button
    >
  </div>

  <p class="readout" aria-live="polite">
    {readout}{#if sizeText}<span class="size"> · {sizeText}</span>{/if}
  </p>

  {#if running}
    <div class="progress" aria-label="Export progress">
      <div class="row">
        <span>Exporting clip-trim ({mode})</span>
        <button type="button" class="control" onclick={cancel} disabled={jobId === null}>Cancel</button>
      </div>
      <div
        class="bar"
        role="progressbar"
        aria-label="Export progress"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(percent)}
      >
        <div class="fill" style="width: {percent}%"></div>
      </div>
      <div class="meta">{Math.round(percent)} % · output at {formatHmsMs(outTimeMs)}</div>
    </div>
  {:else}
    <div class="actions">
      <button type="button" class="control" onclick={cancel}>Close</button>
      <button
        type="button"
        class="control primary"
        disabled={running || !valid}
        title="Save as new clip (Ctrl+Enter)"
        onclick={save}>Save as new clip</button
      >
    </div>
  {/if}

  {#if status === 'done'}
    <p class="ok" role="status">Saved as a new clip. It appears in the library.</p>
  {/if}

  {#if errorMessage}
    <p class="error" role="alert">{errorMessage}</p>
  {/if}
</section>

<style>
  .trimmer {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    background: var(--surface);
    border-top: 1px solid var(--border);
    color: var(--text);
    outline: none;
  }

  .timeline {
    position: relative;
    height: 44px;
    margin-top: 22px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    user-select: none;
    touch-action: none;
  }

  .range {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgba(237, 28, 36, 0.18);
    pointer-events: none;
  }

  .handle {
    position: absolute;
    top: -6px;
    bottom: -6px;
    width: 10px;
    margin-left: -5px;
    border-radius: 3px;
    cursor: ew-resize;
  }

  .handle.in {
    background: #3fb950;
  }

  .handle.out {
    background: var(--accent);
  }

  .handle:focus-visible,
  .handle.focused {
    outline: 2px solid var(--text);
    outline-offset: 2px;
  }

  .handle[aria-disabled='true'] {
    cursor: default;
    opacity: 0.6;
  }

  .label {
    position: absolute;
    bottom: 100%;
    left: 50%;
    transform: translateX(-50%);
    margin-bottom: 4px;
    white-space: nowrap;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
  }

  .times {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .times label {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .times input {
    width: 130px;
    padding: 5px 8px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-family: inherit;
    font-variant-numeric: tabular-nums;
  }

  .span {
    font-variant-numeric: tabular-nums;
  }

  .modes,
  .actions,
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .actions {
    justify-content: flex-end;
  }

  .control {
    padding: 7px 10px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    white-space: nowrap;
    font-family: inherit;
    font-size: 13px;
  }

  .control:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .control[aria-pressed='true'] {
    border-color: var(--accent);
    background: rgba(237, 28, 36, 0.12);
  }

  .control.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .control:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .control:focus-visible,
  .times input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .readout {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .size {
    color: var(--text-dim);
  }

  .progress {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 13px;
  }

  .bar {
    height: 8px;
    background: var(--bg);
    border-radius: 4px;
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s linear;
  }

  .meta {
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .ok {
    margin: 0;
    color: #3fb950;
    font-size: 13px;
  }

  .error {
    margin: 0;
    padding: 8px 10px;
    color: var(--danger);
    background: var(--surface-2);
    border: 1px solid var(--danger);
    border-radius: var(--radius);
    font-size: 13px;
  }

  @media (max-width: 480px) {
    .trimmer {
      padding: 12px;
    }

    .times input {
      width: 110px;
    }
  }
</style>
