<script lang="ts">
  import { formatBytes } from '../format';
  import { openFolder, revealRecording } from '../recordingActions';
  import { editorApi } from './api';
  import { exportJobs } from './store';
  import { formatDuration, secToTick } from './ticks';
  import type { JobDto, JobState } from './types';

  const STATE_LABEL: Record<JobState, string> = {
    queued: 'Queued',
    running: 'Running',
    paused: 'Paused',
    done: 'Done',
    failed: 'Failed',
    cancelled: 'Cancelled',
  };

  let jobs = $derived($exportJobs);

  /** Number of queued jobs (the reorder index counts these only). */
  let queuedCount = $derived(jobs.filter((job) => job.state === 'queued').length);

  let actionError = $state<string | null>(null);
  let confirmCancelId = $state<string | null>(null);
  let expanded = $state<Record<string, boolean>>({});

  function fileName(path: string): string {
    const parts = path.split(/[\\/]/);
    return parts[parts.length - 1] || path;
  }

  /** Folder part of a file path (accepts Windows and POSIX separators). */
  function folderOf(path: string): string {
    const cut = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
    return cut > 0 ? path.slice(0, cut) : path;
  }

  function percentOf(job: JobDto): number {
    return Math.max(0, Math.min(100, Math.floor(job.percent)));
  }

  /** True while the job still needs the machine (queued, running or paused). */
  function isActive(state: JobState): boolean {
    return state === 'queued' || state === 'running' || state === 'paused';
  }

  function pausedLabel(job: JobDto): string {
    if (job.state !== 'paused') return '';
    if (job.pausedReason === 'recording') return 'Paused: a game is running';
    return 'Paused';
  }

  function errorText(err: unknown): string {
    if (err instanceof Error) return err.message;
    if (typeof err === 'string') return err;
    return 'Something went wrong.';
  }

  /** Runs one action and shows its failure in the list header. */
  async function run(action: () => Promise<unknown>): Promise<void> {
    actionError = null;
    try {
      await action();
    } catch (err) {
      actionError = errorText(err);
    }
  }

  function togglePause(job: JobDto): void {
    void run(() => editorApi.exportPause(job.id, job.state !== 'paused'));
  }

  function exportAnyway(job: JobDto): void {
    void run(() => editorApi.exportPauseOverride(job.id));
  }

  function cancel(job: JobDto): void {
    confirmCancelId = null;
    void run(() => editorApi.exportCancel(job.id));
  }

  function dismiss(job: JobDto): void {
    void run(() => editorApi.exportDismiss(job.id));
  }

  /** Position of a queued job among the queued jobs only, or -1 when it is not queued. */
  function queuedRank(job: JobDto): number {
    if (job.state !== 'queued') return -1;
    return jobs.filter((j) => j.state === 'queued').findIndex((j) => j.id === job.id);
  }

  function canMove(job: JobDto, direction: -1 | 1): boolean {
    const rank = queuedRank(job);
    if (rank < 0) return false;
    return direction < 0 ? rank > 0 : rank < queuedCount - 1;
  }

  /** Moves a queued job one place among the queued jobs. The backend index counts queued jobs only. */
  function move(job: JobDto, direction: -1 | 1): void {
    if (!canMove(job, direction)) return;
    const target = queuedRank(job) + direction;
    void run(() => editorApi.exportReorder(job.id, target));
  }

  /** Opens the finished export in the default player. */
  function play(job: JobDto): void {
    void run(() => editorApi.openExport(job.outPath));
  }

  /** Shows the output file selected in Explorer, or its folder when it is not in the library. */
  function showInFolder(job: JobDto): void {
    const id = job.recordingId;
    if (id !== null) {
      void run(() => revealRecording(id));
    } else {
      void run(() => openFolder(folderOf(job.outPath)));
    }
  }

  function toggleWarnings(job: JobDto): void {
    expanded[job.id] = !expanded[job.id];
  }
</script>

<section class="queue" aria-label="Export queue">
  {#if jobs.length === 0}
    <p class="empty">No exports yet</p>
  {:else}
    {#if actionError}
      <p class="error" role="alert">{actionError}</p>
    {/if}

    <ul class="list">
      {#each jobs as job (job.id)}
        {@const pct = percentOf(job)}
        <li class="row" data-state={job.state}>
          <div class="head">
            <div class="titles">
              <span class="project" title={job.projectName}>{job.projectName}</span>
              <span class="file" title={job.outPath}>{fileName(job.outPath)}</span>
            </div>
            <span class="badge {job.state}">{STATE_LABEL[job.state]}</span>
          </div>

          <div
            class="progress"
            role="progressbar"
            aria-label="Progress for {job.projectName}"
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow={pct}
          >
            <span class="fill" style:width="{pct}%"></span>
          </div>

          <div class="meta">
            <span>{pct}%</span>
            <span>Elapsed {formatDuration(secToTick(job.elapsedS))}</span>
            {#if isActive(job.state) && job.state !== 'queued' && job.etaS !== null}
              <span>ETA {formatDuration(secToTick(job.etaS))}</span>
            {/if}
            {#if job.state === 'running' && job.speedX > 0}
              <span>{job.speedX.toFixed(1)}x</span>
            {/if}
            {#if job.bytes > 0 && job.state !== 'done'}
              <span>{formatBytes(job.bytes)}</span>
            {/if}
            {#if job.encoder}
              <span>{job.encoder}</span>
            {/if}
          </div>

          {#if job.state === 'done' && job.bytes > 0}
            <p class="note">Final size: {formatBytes(job.bytes)}</p>
          {/if}

          {#if job.state === 'paused'}
            <p class="note">{pausedLabel(job)}</p>
          {/if}

          {#if job.error}
            <p class="error">{job.error}</p>
          {/if}

          {#if job.warnings.length > 0}
            {#if job.state !== 'done'}
              <button
                type="button"
                class="link"
                aria-expanded={expanded[job.id] === true}
                aria-controls="warnings-{job.id}"
                onclick={() => toggleWarnings(job)}
              >
                {job.warnings.length} {job.warnings.length === 1 ? 'warning' : 'warnings'}
              </button>
            {/if}
            {#if job.state === 'done' || expanded[job.id] === true}
              <ul class="warnings" id="warnings-{job.id}">
                {#each job.warnings as warning, w (w)}
                  <li>{warning}</li>
                {/each}
              </ul>
            {/if}
          {/if}

          <div class="actions">
            {#if job.state === 'running' || job.state === 'paused'}
              <button
                type="button"
                class="btn"
                aria-label="{job.state === 'paused' ? 'Resume' : 'Pause'} export of {job.projectName}"
                onclick={() => togglePause(job)}
              >
                {job.state === 'paused' ? 'Resume' : 'Pause'}
              </button>
            {/if}

            {#if job.state === 'paused' && job.pausedReason === 'recording'}
              <button
                type="button"
                class="btn"
                aria-label="Export anyway, ignoring the recording pause, {job.projectName}"
                onclick={() => exportAnyway(job)}
              >
                Export anyway
              </button>
            {/if}

            {#if job.state === 'queued'}
              <button
                type="button"
                class="btn"
                aria-label="Move {job.projectName} up in the queue"
                disabled={!canMove(job, -1)}
                onclick={() => move(job, -1)}
              >
                Move up
              </button>
              <button
                type="button"
                class="btn"
                aria-label="Move {job.projectName} down in the queue"
                disabled={!canMove(job, 1)}
                onclick={() => move(job, 1)}
              >
                Move down
              </button>
            {/if}

            {#if isActive(job.state)}
              {#if confirmCancelId === job.id}
                <span class="confirm" role="group" aria-label="Confirm cancel">
                  Cancel this export?
                  <button type="button" class="btn danger" onclick={() => cancel(job)}>
                    Yes, cancel
                  </button>
                  <button type="button" class="btn" onclick={() => (confirmCancelId = null)}>
                    Keep going
                  </button>
                </span>
              {:else}
                <button
                  type="button"
                  class="btn danger"
                  aria-label="Cancel export of {job.projectName}"
                  onclick={() => (confirmCancelId = job.id)}
                >
                  Cancel
                </button>
              {/if}
            {/if}

            {#if job.state === 'done'}
              <button
                type="button"
                class="btn"
                aria-label="Play {fileName(job.outPath)}"
                onclick={() => play(job)}
              >
                Play
              </button>

              <button
                type="button"
                class="btn"
                aria-label="Show {fileName(job.outPath)} in folder"
                onclick={() => showInFolder(job)}
              >
                Show in folder
              </button>
            {/if}

            {#if job.state === 'done' || job.state === 'failed' || job.state === 'cancelled'}
              <button
                type="button"
                class="btn"
                aria-label="Dismiss {job.projectName}"
                onclick={() => dismiss(job)}
              >
                Dismiss
              </button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .queue {
    color: var(--text);
    font-size: 12px;
  }

  .empty {
    margin: 8px 4px;
    color: var(--text-dim);
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .row {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
  }

  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
  }

  .titles {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .project {
    overflow: hidden;
    font-size: 13px;
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file {
    overflow: hidden;
    color: var(--text-dim);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .badge {
    flex: 0 0 auto;
    padding: 1px 8px;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--text-dim);
    white-space: nowrap;
  }

  .badge.running {
    border-color: var(--accent);
    color: var(--accent);
  }

  .badge.done {
    color: var(--text);
  }

  .badge.failed {
    border-color: var(--danger);
    color: var(--danger);
  }

  .progress {
    position: relative;
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    background: var(--accent);
    transition: width 0.3s linear;
  }

  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    color: var(--text-dim);
  }

  .note {
    margin: 0;
    color: var(--text-dim);
  }

  .error {
    margin: 0;
    color: var(--danger);
    overflow-wrap: anywhere;
  }

  .warnings {
    margin: 0;
    padding-left: 18px;
    color: var(--text-dim);
  }

  .link {
    align-self: flex-start;
    padding: 0;
    border: none;
    background: none;
    color: var(--text-dim);
    text-decoration: underline;
    cursor: pointer;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .confirm {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
  }

  .btn {
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .btn:hover:not(:disabled) {
    background: var(--surface-2);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn.danger {
    color: var(--danger);
  }

  .btn:focus-visible,
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
