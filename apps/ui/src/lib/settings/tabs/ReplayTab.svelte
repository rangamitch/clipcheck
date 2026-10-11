<script lang="ts">
  import FieldRow from '../fields/FieldRow.svelte';
  import KeyCapture from '../fields/KeyCapture.svelte';
  import NumberField from '../fields/NumberField.svelte';
  import PathField from '../fields/PathField.svelte';
  import Segmented from '../fields/Segmented.svelte';
  import Select from '../fields/Select.svelte';
  import TextField from '../fields/TextField.svelte';
  import Toggle from '../fields/Toggle.svelte';
  import { enabledTrackKbps } from '../audio';
  import {
    BUFFER_MAX_SECONDS,
    BUFFER_MIN_SECONDS,
    RAM_MAX_MB_MAX,
    RAM_MAX_MB_MIN,
  } from '../bounds';
  import {
    CLIP_MAX_SECONDS,
    CLIP_MIN_SECONDS,
    CLIP_NAME_MAX_CHARS,
    CLIP_PRESETS,
    CLIP_PRESS_OPTIONS,
    LONG_PRESS_SECONDS,
    MAX_CLIPS,
    addClip,
    clipCountText,
    clipSecondsMessage,
    clipsOverLimit,
    pressOf,
    removeClip,
    suggestClipSeconds,
    updateClip,
  } from '../clips';
  import { sessionChordsOf, tapLongPairFlags } from '../hotkeys';
  import { frameCoalesced } from '../frame';
  import { charCount, effectiveVideoKbps, estimateBufferMB, fieldDiagnostic, formatSpanSeconds, formatMB } from '../pure';
  import { settings, setPath, type ClipHotkeyConfig, type ClipPress, type ReplayStorage } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  const replay = $derived($settings.draft?.replay);
  const detection = $derived($settings.draft?.detection);
  const video = $derived($settings.draft?.video);
  const audio = $derived($settings.draft?.audio);
  const hotkeys = $derived($settings.draft?.hotkeys);
  const clips = $derived<ClipHotkeyConfig[]>(hotkeys?.clips ?? []);
  const diagnostics = $derived($settings.diagnostics);

  const STORAGE_OPTIONS: { value: ReplayStorage; label: string }[] = [
    { value: 'disk', label: 'Disk' },
    { value: 'ram', label: 'Memory (RAM)' },
  ];

  /** Session chords, so a new clip does not get one of them. */
  const sessionChords = $derived(sessionChordsOf(hotkeys?.session));

  const bufferSeconds = $derived(replay?.max_seconds ?? 0);
  const minutesValue = $derived(Number((bufferSeconds / 60).toFixed(2)));
  /** Estimated buffer size in MB, at the current video target and enabled audio tracks. */
  const estimateMB = $derived(
    video && audio
      ? estimateBufferMB(bufferSeconds, effectiveVideoKbps(video), enabledTrackKbps(audio))
      : 0,
  );
  const overLimit = $derived(clipsOverLimit(clips, bufferSeconds));
  /** Rows that are one half of a Tap + Long press pair on the same chord. */
  const pairFlags = $derived(
    tapLongPairFlags(clips.map((c) => ({ label: c.name, chord: c.keys, press: pressOf(c) }))),
  );

  function setBufferSeconds(seconds: number): void {
    setPath('replay.max_seconds', Math.round(seconds));
  }

  /** The buffer slider writes at most once per frame while it is dragged. */
  const dragBufferSeconds = frameCoalesced(setBufferSeconds);

  function setClips(next: ClipHotkeyConfig[]): void {
    setPath('hotkeys.clips', next);
  }

  function addClipRow(): void {
    if (clips.length >= MAX_CLIPS) return;
    setClips(addClip(clips, [...sessionChords, ...clips.map((c) => c.keys)]));
  }

  function daemonMessage(path: string): string | null {
    return fieldDiagnostic(path, diagnostics)?.message ?? null;
  }
</script>

{#if replay}
  <h2>Recordings</h2>

  {#if detection}
    <FieldRow
      label="Automatically record full sessions"
      path="detection.record_sessions"
      help="Off: only the replay buffer runs, so you get clips and no full recording of each game. You can still start a session by hand with the session hotkey."
    >
      {#snippet children(ctl)}
        <Toggle
          {...ctl}
          checked={detection.record_sessions}
          disabled={disabled || !detection.auto_record}
          onchange={(v) => setPath('detection.record_sessions', v)}
        />
      {/snippet}
    </FieldRow>

    {#if !detection.record_sessions && !replay.enabled}
      <p class="warn" role="alert">
        Sessions and the replay buffer are both off, so nothing will be saved unless you start a session by hand.
      </p>
    {/if}
  {/if}

  <FieldRow
    label="Replay buffer"
    path="replay.enabled"
    help="Keeps the last few minutes running, so you can save a clip after something happens."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={replay.enabled}
        {disabled}
        onchange={(v) => setPath('replay.enabled', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Buffer length"
    path="replay.max_seconds"
    help="How far back a clip can reach. Clips cannot be longer than this."
  >
    {#snippet children(ctl)}
      <div class="length">
        <NumberField
          {...ctl}
          value={minutesValue}
          min={BUFFER_MIN_SECONDS / 60}
          max={BUFFER_MAX_SECONDS / 60}
          step={0.5}
          integer={false}
          unit="minutes"
          {disabled}
          onchange={(m) => setBufferSeconds(m * 60)}
        />
        <span class="dim">= {replay.max_seconds} seconds ({formatSpanSeconds(replay.max_seconds)})</span>
        <input
          class="slider"
          type="range"
          min={BUFFER_MIN_SECONDS}
          max={BUFFER_MAX_SECONDS}
          step="1"
          value={replay.max_seconds}
          aria-label="Buffer length in seconds"
          aria-valuetext={`${replay.max_seconds} seconds`}
          {disabled}
          oninput={(e) => dragBufferSeconds(Number(e.currentTarget.value))}
        />
      </div>
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Keep the buffer in"
    path="replay.storage"
    help="Disk uses little memory. Memory is faster to save from, but it uses RAM up to the budget below."
  >
    {#snippet children(ctl)}
      <Segmented
        {...ctl}
        name="replay-storage"
        value={replay.storage}
        options={STORAGE_OPTIONS}
        {disabled}
        onchange={(v) => setPath('replay.storage', v)}
      />
    {/snippet}
  </FieldRow>

  {#if replay.storage === 'ram'}
    <FieldRow
      label="Memory budget"
      path="replay.ram_max_mb"
      help="Most RAM the buffer may use. The recorder checks this against the buffer length when you save."
    >
      {#snippet children(ctl)}
        <NumberField
          {...ctl}
          value={replay.ram_max_mb}
          min={RAM_MAX_MB_MIN}
          max={RAM_MAX_MB_MAX}
          step={64}
          unit="MB"
          {disabled}
          onchange={(v) => setPath('replay.ram_max_mb', v)}
        />
      {/snippet}
    </FieldRow>
  {/if}

  <FieldRow
    label="Buffer folder"
    path="replay.location"
    help="Where buffer segments are written while the buffer runs. Windows variables such as %LOCALAPPDATA% are expanded."
  >
    {#snippet children(ctl)}
      <PathField
        {...ctl}
        value={replay.location}
        {disabled}
        onchange={(v) => setPath('replay.location', v)}
      />
    {/snippet}
  </FieldRow>

  <p class="estimate" aria-live="polite">
    Disk space: {formatMB(estimateMB)} at the current bitrate for a {formatSpanSeconds(replay.max_seconds)} buffer.
  </p>

  <section class="clips" aria-labelledby="clips-heading">
    <div class="clips-head">
      <h3 id="clips-heading">Clip hotkeys</h3>
      <span class="meter" aria-live="polite">{clipCountText(clips.length)}</span>
    </div>
    <p class="help">
      Each hotkey saves the last N seconds of the buffer as its own clip. Lengths are between
      {CLIP_MIN_SECONDS} seconds and the buffer length.
    </p>
    <p class="hint">
      Use the same key twice with Tap and Long press to save two different lengths. Long press = hold for {LONG_PRESS_SECONDS} s.
    </p>
    {#if overLimit.length > 0}
      <p class="message" role="alert">
        {overLimit.length === 1 ? 'One clip is' : `${overLimit.length} clips are`} longer than the buffer.
        Shorten the clip or lengthen the buffer.
      </p>
    {/if}
    {#if daemonMessage('hotkeys.clips')}
      <p class="message" role="alert">{daemonMessage('hotkeys.clips')}</p>
    {/if}

    {#if clips.length === 0}
      <p class="empty">No clip hotkeys yet.</p>
    {/if}

    <ol class="list">
      {#each clips as clip, i (i)}
        {@const localProblem = clipSecondsMessage(clip.seconds, bufferSeconds)}
        {@const suggested = suggestClipSeconds(clip.seconds, bufferSeconds)}
        {@const nameMsg = daemonMessage(`hotkeys.clips[${i}].name`)}
        {@const keysMsg = daemonMessage(`hotkeys.clips[${i}].keys`)}
        {@const secondsMsg = localProblem ?? daemonMessage(`hotkeys.clips[${i}].seconds`)}
        <li class="clip" class:bad={!!secondsMsg || !!nameMsg}>
          <div class="row">
            <span class="index" aria-hidden="true">{i + 1}</span>
            <div class="name-wrap">
              <TextField
                ariaLabel={`Name of clip hotkey ${i + 1}`}
                value={clip.name}
                placeholder="Clip name"
                {disabled}
                describedBy={nameMsg ? `clip-${i}-name-msg` : undefined}
                commitOn="change"
                onchange={(v) => setClips(updateClip(clips, i, { name: v }))}
              />
              {#if nameMsg}
                <p class="message" id={`clip-${i}-name-msg`}>{nameMsg}</p>
              {:else if charCount(clip.name) > CLIP_NAME_MAX_CHARS}
                <p class="message">Names are at most {CLIP_NAME_MAX_CHARS} characters.</p>
              {/if}
            </div>
            <button
              type="button"
              class="remove"
              aria-label={`Remove clip hotkey ${clip.name}`}
              {disabled}
              onclick={() => setClips(removeClip(clips, i))}
            >
              Remove
            </button>
          </div>

          <div class="row seconds-row">
            <span class="label">Length</span>
            <NumberField
              ariaLabel={`Length of ${clip.name} in seconds`}
              value={clip.seconds}
              min={CLIP_MIN_SECONDS}
              max={CLIP_MAX_SECONDS}
              step={1}
              unit="seconds"
              {disabled}
              onchange={(v) => setClips(updateClip(clips, i, { seconds: v }))}
            />
            <div class="chips" role="group" aria-label={`Presets for ${clip.name}`}>
              {#each CLIP_PRESETS as preset (preset.seconds)}
                <button
                  type="button"
                  class="chip"
                  aria-pressed={clip.seconds === preset.seconds}
                  {disabled}
                  onclick={() => setClips(updateClip(clips, i, { seconds: preset.seconds }))}
                >
                  {preset.label}
                </button>
              {/each}
            </div>
          </div>
          {#if secondsMsg}
            <p class="message" role="alert">
              {secondsMsg}
              {#if localProblem && suggested !== clip.seconds}
                <button
                  type="button"
                  class="suggest"
                  {disabled}
                  onclick={() => setClips(updateClip(clips, i, { seconds: suggested }))}
                >
                  Use {formatSpanSeconds(suggested)}
                </button>
              {/if}
            </p>
          {/if}

          <div class="row">
            <span class="label" id={`clip-${i}-keys-label`}>Hotkey</span>
            <KeyCapture
              labelId={`clip-${i}-keys-label`}
              value={clip.keys}
              allowClear={false}
              {disabled}
              describedBy={keysMsg ? `clip-${i}-keys-msg` : undefined}
              onchange={(v) => setClips(updateClip(clips, i, { keys: v }))}
            />
            <Select
              ariaLabel={`Press type for ${clip.name}`}
              value={pressOf(clip)}
              options={CLIP_PRESS_OPTIONS}
              {disabled}
              onchange={(v) => setClips(updateClip(clips, i, { press: (v === 'long' ? 'long' : 'tap') as ClipPress }))}
            />
            {#if pairFlags[i]}
              <span class="pair" title="This key saves two lengths: one on tap and one on long press.">Tap + Long press</span>
            {/if}
          </div>
          {#if keysMsg}
            <p class="message" id={`clip-${i}-keys-msg`}>{keysMsg}</p>
          {/if}
        </li>
      {/each}
    </ol>

    <div class="list-actions">
      <button type="button" id="clip-add" disabled={disabled || clips.length >= MAX_CLIPS} onclick={addClipRow}>
        Add clip hotkey
      </button>
      {#if clips.length >= MAX_CLIPS}
        <span class="dim">The limit is {MAX_CLIPS} clip hotkeys.</span>
      {/if}
    </div>
  </section>
{/if}

<style>
  h3 {
    margin: 0;
  }

  .length {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    width: min(100%, 480px);
  }

  .slider {
    width: 100%;
    accent-color: var(--accent);
  }

  .estimate {
    margin: 16px 0 0;
    font-size: 13px;
    color: var(--text);
  }

  .warn {
    margin: 8px 0 0;
    font-size: 13px;
    color: var(--danger);
  }

  .hint {
    margin: 0;
    padding: 8px 10px;
    font-size: 13px;
    color: var(--text);
    border-left: 3px solid var(--accent);
    background: var(--surface);
    border-radius: var(--radius);
  }

  .pair {
    padding: 3px 8px;
    border-radius: 999px;
    font-size: 12px;
    border: 1px solid var(--accent);
    color: var(--text);
  }

  .dim,
  .help,
  .empty {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }

  .message {
    margin: 0;
    font-size: 13px;
    color: var(--danger);
  }

  .clips {
    margin-top: 26px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .clips-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }

  .meter {
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .clip {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .clip.bad {
    border-color: var(--danger);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }

  .index {
    min-width: 22px;
    text-align: center;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .label {
    min-width: 56px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .name-wrap {
    flex: 1 1 220px;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .chip,
  .suggest,
  .remove,
  .list-actions button,
  .clips button {
    padding: 5px 10px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  .chip[aria-pressed='true'] {
    border-color: var(--accent);
    background: var(--surface);
  }

  .remove {
    color: var(--text-dim);
  }

  .suggest {
    margin-left: 8px;
    border-color: var(--accent);
  }

  .list-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
</style>
