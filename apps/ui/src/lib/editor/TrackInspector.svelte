<script lang="ts">
  // Settings of one track: name, flags, fader, height, edit target, add, remove and reorder.
  import { dispatch, project } from './store';
  import { clamp, debounce, formatDb, parseDb } from './fields';
  import type { TrackKind, TrackPatch } from './types';

  interface Props {
    /** Id of the track to show. */
    trackId: string;
  }

  let { trackId }: Props = $props();

  /** Track flags that can be toggled with a button. */
  type ToggleKey = 'muted' | 'solo' | 'locked' | 'hidden';

  const TOGGLES: { key: ToggleKey; label: string }[] = [
    { key: 'muted', label: 'Mute' },
    { key: 'solo', label: 'Solo' },
    { key: 'locked', label: 'Lock' },
    { key: 'hidden', label: 'Hide' },
  ];
  const HEIGHT_OPTIONS: { label: string; value: number }[] = [
    { label: 'Small (32 px)', value: 32 },
    { label: 'Normal (56 px)', value: 56 },
    { label: 'Large (96 px)', value: 96 },
  ];
  const MAX_TRACK_NAME = 40;
  const MAX_VIDEO_TRACKS = 6;
  const MAX_AUDIO_TRACKS = 8;
  const MIN_DB = -60;
  const MAX_DB = 12;
  /** Fader idle time before a drag sends its command, in milliseconds. */
  const DRAG_IDLE_MS = 250;

  const track = $derived($project?.tracks.find((t) => t.id === trackId) ?? null);
  const sameKind = $derived(
    track && $project ? $project.tracks.filter((t) => t.kind === track.kind) : [],
  );
  const indexInKind = $derived(track ? sameKind.findIndex((t) => t.id === track.id) : -1);
  /**
   * Video tracks are listed highest layer first on screen, so the list order is reversed: a later
   * index is a higher layer and is drawn higher. Audio tracks are listed top to bottom.
   */
  const isVideo = $derived(track?.kind === 'video');
  /** Move delta that moves the track visually up: +1 for video (later = higher), -1 for audio. */
  const upDelta = $derived(isVideo ? 1 : -1);
  const lastIndex = $derived(sameKind.length - 1);
  const canMoveUp = $derived(
    indexInKind >= 0 && (isVideo ? indexInKind < lastIndex : indexInKind > 0),
  );
  const canMoveDown = $derived(
    indexInKind >= 0 && (isVideo ? indexInKind > 0 : indexInKind < lastIndex),
  );
  /** Audio tracks have no visibility flag, so the Hide control is offered for video only. */
  const visibleToggles = $derived(
    isVideo ? TOGGLES : TOGGLES.filter((tg) => tg.key !== 'hidden'),
  );
  const videoCount = $derived($project ? $project.tracks.filter((t) => t.kind === 'video').length : 0);
  const audioCount = $derived($project ? $project.tracks.filter((t) => t.kind === 'audio').length : 0);
  const canAddVideo = $derived(videoCount < MAX_VIDEO_TRACKS);
  const canAddAudio = $derived(audioCount < MAX_AUDIO_TRACKS);
  /** The last track of its kind cannot be removed; a locked track must be unlocked first. */
  const removeBlockedByCount = $derived(sameKind.length <= 1);
  const removeBlockedByLock = $derived(track?.locked === true);
  const canRemove = $derived(track !== null && !removeBlockedByCount && !removeBlockedByLock);
  const heightKnown = $derived(HEIGHT_OPTIONS.some((o) => o.value === track?.height));

  /** Name while typed, or null when showing the track value. */
  let nameDraft = $state<string | null>(null);
  /** Fader value while dragging or just after a change, or null when showing the track value. */
  let dbDraft = $state<number | null>(null);
  /** Text of the dB number field while typed, or null. */
  let dbText = $state<string | null>(null);
  /** True after "Remove track" is pressed, until confirmed or cancelled. */
  let confirmingRemove = $state(false);

  const shownDb = $derived(dbDraft ?? track?.volume_db ?? 0);

  /** Sends a patch for the shown track. */
  function patch(p: TrackPatch): void {
    const t = track;
    if (!t) return;
    void dispatch({ type: 'patch_track', track_id: t.id, patch: p });
  }

  /** Flips one flag. */
  function toggle(key: ToggleKey): void {
    const t = track;
    if (!t) return;
    switch (key) {
      case 'muted':
        patch({ muted: !t.muted });
        break;
      case 'solo':
        patch({ solo: !t.solo });
        break;
      case 'locked':
        patch({ locked: !t.locked });
        break;
      case 'hidden':
        patch({ hidden: !t.hidden });
        break;
    }
  }

  /** Commits the typed name (on blur or Enter). Empty or too long names are ignored. */
  function commitName(): void {
    const draft = nameDraft;
    nameDraft = null;
    const t = track;
    if (draft === null || !t) return;
    const next = draft.trim();
    if (next.length === 0 || next.length > MAX_TRACK_NAME || next === t.name) return;
    patch({ name: next });
  }

  /** Sends one fader command. The draft stays on screen until the change lands. */
  function commitDb(v: number): void {
    const next = Math.round(clamp(v, MIN_DB, MAX_DB) * 10) / 10;
    const t = track;
    if (!t || next === t.volume_db) {
      dbDraft = null;
      return;
    }
    dbDraft = next;
    void dispatch({ type: 'patch_track', track_id: t.id, patch: { volume_db: next } }).then(() => {
      if (dbDraft === next) dbDraft = null;
    });
  }

  /** Sends the fader value after the drag has been idle for a while. */
  const dbIdle = debounce((v: number) => commitDb(v), DRAG_IDLE_MS);

  /** Fader moved: show the value and send it once the drag pauses. */
  function onFaderInput(v: number): void {
    dbDraft = v;
    dbIdle.call(v);
  }

  /** Fader released (or changed by keyboard): send at once. */
  function onFaderChange(v: number): void {
    dbIdle.cancel();
    commitDb(v);
  }

  /** Number field committed: parse the text and send it. Bad text reverts. */
  function commitDbText(text: string): void {
    const v = parseDb(text);
    dbText = null;
    dbIdle.cancel();
    if (v === null) return;
    commitDb(v);
  }

  /** Adds an empty track of the given kind at the end of its group. */
  function addTrack(kind: TrackKind): void {
    void dispatch({ type: 'add_track', kind });
  }

  /** Moves the track by one place within its kind (delta -1 or +1, see the list order note). */
  function moveBy(delta: number): void {
    const t = track;
    if (!t) return;
    void dispatch({ type: 'move_track', track_id: t.id, delta });
  }

  /** Removes the track and its clips after the inline confirmation. */
  function confirmRemove(): void {
    const t = track;
    confirmingRemove = false;
    if (!t) return;
    void dispatch({ type: 'remove_track', track_id: t.id });
  }
</script>

{#if track}
  <div class="ti">
    <section class="block" aria-labelledby="ti-main-h">
      <h3 id="ti-main-h">Track</h3>
      <div class="row">
        <label for="ti-name">Name</label>
        <input
          id="ti-name"
          type="text"
          maxlength={MAX_TRACK_NAME}
          value={nameDraft ?? track.name}
          oninput={(e) => (nameDraft = e.currentTarget.value)}
          onblur={commitName}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              e.currentTarget.blur();
            } else if (e.key === 'Escape') {
              nameDraft = null;
              e.currentTarget.blur();
            }
          }}
        />
      </div>
      <p class="kind">{track.kind === 'video' ? 'Video track' : 'Audio track'}</p>
      <div class="toggles" role="group" aria-label="Track flags">
        {#each visibleToggles as tg (tg.key)}
          <button
            type="button"
            class="toggle"
            aria-pressed={track[tg.key]}
            onclick={() => toggle(tg.key)}
          >
            {tg.label}
          </button>
        {/each}
      </div>
    </section>

    <section class="block" aria-labelledby="ti-fader-h">
      <h3 id="ti-fader-h">Fader</h3>
      <div class="row">
        <label for="ti-db-range">Level</label>
        <input
          id="ti-db-range"
          type="range"
          min={MIN_DB}
          max={MAX_DB}
          step={0.1}
          value={shownDb}
          oninput={(e) => onFaderInput(Number(e.currentTarget.value))}
          onchange={(e) => onFaderChange(Number(e.currentTarget.value))}
        />
        <input
          id="ti-db"
          type="text"
          inputmode="decimal"
          class="db"
          aria-label="Level in decibels"
          value={dbText ?? formatDb(shownDb)}
          oninput={(e) => (dbText = e.currentTarget.value)}
          onchange={(e) => commitDbText(e.currentTarget.value)}
          onblur={() => (dbText = null)}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              e.currentTarget.blur();
            }
          }}
        />
      </div>
    </section>

    <section class="block" aria-labelledby="ti-layout-h">
      <h3 id="ti-layout-h">Layout and edits</h3>
      <div class="row">
        <label for="ti-height">Height</label>
        <select
          id="ti-height"
          value={String(track.height)}
          onchange={(e) => patch({ height: Number(e.currentTarget.value) })}
        >
          {#each HEIGHT_OPTIONS as opt (opt.value)}
            <option value={String(opt.value)}>{opt.label}</option>
          {/each}
          {#if !heightKnown}
            <option value={String(track.height)}>{track.height} px</option>
          {/if}
        </select>
      </div>
      <div class="row">
        <input
          id="ti-target"
          type="checkbox"
          checked={track.targeted}
          onchange={(e) => patch({ targeted: e.currentTarget.checked })}
        />
        <label for="ti-target">Target for edits</label>
      </div>
      <div class="row buttons">
        <button type="button" disabled={!canMoveUp} onclick={() => moveBy(upDelta)}>Move up</button>
        <button type="button" disabled={!canMoveDown} onclick={() => moveBy(-upDelta)}>
          Move down
        </button>
      </div>
    </section>

    <section class="block" aria-labelledby="ti-add-h">
      <h3 id="ti-add-h">Add or remove</h3>
      <div class="row buttons">
        <button
          type="button"
          disabled={!canAddVideo}
          onclick={() => addTrack('video')}
        >
          Add video track ({videoCount}/{MAX_VIDEO_TRACKS})
        </button>
        <button
          type="button"
          disabled={!canAddAudio}
          onclick={() => addTrack('audio')}
        >
          Add audio track ({audioCount}/{MAX_AUDIO_TRACKS})
        </button>
      </div>
      {#if !confirmingRemove}
        <div class="row buttons">
          <button
            type="button"
            class="danger"
            disabled={!canRemove}
            onclick={() => (confirmingRemove = true)}
          >
            Remove track
          </button>
        </div>
        {#if removeBlockedByLock}
          <p class="hint">Unlock the track to remove it.</p>
        {:else if removeBlockedByCount}
          <p class="hint">Keep at least one {track.kind} track.</p>
        {/if}
      {:else}
        <p class="warn" role="alert">
          Remove "{track.name}" and every clip on it? You can undo this.
        </p>
        <div class="row buttons">
          <button type="button" class="danger" onclick={confirmRemove}>Remove track</button>
          <button type="button" onclick={() => (confirmingRemove = false)}>Cancel</button>
        </div>
      {/if}
    </section>
  </div>
{:else}
  <p class="hint">This track no longer exists.</p>
{/if}

<style>
  .ti {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 12px;
    color: var(--text);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  h3 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .row > label {
    flex: 0 0 auto;
    min-width: 70px;
    color: var(--text-dim);
  }
  .row.buttons {
    flex-wrap: wrap;
  }
  input[type='text'],
  select {
    flex: 1 1 auto;
    min-width: 0;
    padding: 6px 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  input[type='range'] {
    flex: 1 1 auto;
    min-width: 80px;
    accent-color: var(--accent);
  }
  .db {
    flex: 0 0 96px !important;
    text-align: right;
  }
  button {
    padding: 6px 10px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  button.toggle[aria-pressed='true'] {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  button.danger {
    color: var(--danger);
  }
  input:focus-visible,
  select:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .toggles {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .kind {
    margin: 0;
    color: var(--text-dim);
    font-size: 12px;
  }
  .hint {
    margin: 0;
    color: var(--text-dim);
    font-size: 12px;
  }
  .warn {
    margin: 0;
    color: var(--danger);
    font-size: 13px;
  }
</style>
