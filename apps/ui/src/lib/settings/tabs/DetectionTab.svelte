<script lang="ts">
  import { onMount } from 'svelte';
  import { refreshRunningApps, runningApps } from '../../discovery';
  import AppIcon from '../fields/AppIcon.svelte';
  import AppPicker, { type PickerApp } from '../fields/AppPicker.svelte';
  import FieldRow from '../fields/FieldRow.svelte';
  import NumberField from '../fields/NumberField.svelte';
  import Select from '../fields/Select.svelte';
  import Toggle from '../fields/Toggle.svelte';
  import {
    addBlocked,
    addGame,
    blockPickerAppsFor,
    gameRowsFor,
    pickerAppsFor,
    removeBlocked,
    removeGame,
    displayNameFor,
    stem,
    type GamesMap,
    type GamesSlice,
  } from '../games';
  import { STOP_GRACE_MAX_S, STOP_GRACE_MIN_S } from '../bounds';
  import { settings, setPath, updateDraft, type AfterManualStop, type DetectionMode } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  const detection = $derived($settings.draft?.detection);
  const games = $derived(($settings.draft?.games ?? {}) as GamesMap);

  const MODE_OPTIONS: { value: DetectionMode; label: string }[] = [
    { value: 'auto', label: 'Any detected game' },
    { value: 'allowlist', label: 'Only games on the allow list' },
  ];

  const AFTER_STOP_OPTIONS: { value: AfterManualStop; label: string }[] = [
    { value: 'until_next_launch', label: 'Stay off until the next game launch' },
    { value: 'until_reenabled', label: 'Stay off until auto-record is turned back on' },
  ];

  const running = $derived($runningApps);
  const gameRows = $derived(detection ? gameRowsFor(detection.allow, games, running) : []);
  const gamePicks = $derived(detection ? pickerAppsFor(running, detection.allow, detection.block) : []);
  const blockPicks = $derived(detection ? blockPickerAppsFor(running, detection.block) : []);

  let busy = $state(false);
  let addError = $state<string | null>(null);
  let blockError = $state<string | null>(null);
  let toast = $state<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  /** The allow, block and games values as the draft holds them now. */
  function currentSlice(): GamesSlice {
    return { allow: detection?.allow ?? [], block: detection?.block ?? [], games };
  }

  /** Writes the allow list, block list and games map in one draft change (one patch on save). */
  function commit(next: GamesSlice): void {
    updateDraft((d) => ({
      ...d,
      detection: { ...d.detection, allow: next.allow, block: next.block },
      games: next.games,
    }));
  }

  function flash(text: string): void {
    toast = text;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      toast = null;
    }, 2500);
  }

  function onPickGame(app: PickerApp): void {
    const r = addGame(currentSlice(), app.exe, app.name);
    if (!r.ok) {
      addError = r.error;
      return;
    }
    addError = null;
    commit(r.value);
    flash(`Added ${displayNameFor(app.exe, r.value.games)}`);
  }

  function onPickBlock(app: PickerApp): void {
    const r = addBlocked(currentSlice(), app.exe);
    if (!r.ok) {
      blockError = r.error;
      return;
    }
    blockError = null;
    commit(r.value);
  }

  /** Name for a block row: the running app's name when it is open, else the exe stem. */
  function blockName(exe: string): string {
    const live = running.find((a) => a.exe.toLowerCase() === exe.toLowerCase());
    return live?.name?.trim() || stem(exe);
  }

  function iconFor(exe: string): string | null {
    return running.find((a) => a.exe.toLowerCase() === exe.toLowerCase())?.iconB64 ?? null;
  }

  async function refresh(): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      await refreshRunningApps();
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void refresh();
    const onFocus = (): void => void refresh();
    window.addEventListener('focus', onFocus);
    return () => {
      window.removeEventListener('focus', onFocus);
      clearTimeout(toastTimer);
    };
  });
</script>

{#if detection}
  <h2>Games</h2>

  <FieldRow
    label="Detect games automatically"
    path="detection.auto_record"
    help="Recording starts when a game window is detected. Browsers, Discord and other apps are ignored."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={detection.auto_record}
        {disabled}
        onchange={(v) => setPath('detection.auto_record', v)}
      />
    {/snippet}
  </FieldRow>

  <section class="card" aria-labelledby="your-games-title">
    <h3 id="your-games-title">Your games</h3>

    {#if gameRows.length === 0}
      <p class="empty">
        No games added yet. Detected games appear in your library automatically; add a game here if it
        isn't detected.
      </p>
    {:else}
      <ul class="rows">
        {#each gameRows as row (row.exe)}
          <li class="row">
            <AppIcon exe={row.exe} name={row.name} iconB64={row.iconB64} size={28} />
            <span class="text">
              <span class="name">{row.name}</span>
              <span class="exe">{row.exe}</span>
            </span>
            {#if row.running}
              <span class="running"><span class="dot" aria-hidden="true"></span>Running</span>
            {/if}
            <button
              type="button"
              class="remove"
              {disabled}
              aria-label={`Remove ${row.name} from your games`}
              onclick={() => commit(removeGame(currentSlice(), row.exe))}>×</button
            >
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card" aria-labelledby="add-game-title">
    <h3 id="add-game-title">Add a game</h3>
    <p class="sub">Pick a game from the apps that are open now. Refresh after you start it.</p>

    <div class="picker-row" inert={disabled}>
      <AppPicker
        apps={gamePicks}
        placeholder="Select your game…"
        emptyText={busy ? 'Looking for open apps…' : 'Open your game, then press refresh'}
        {busy}
        onPick={onPickGame}
        onRefresh={refresh}
      />
    </div>

    {#if addError}
      <p class="error" role="alert">{addError}</p>
    {/if}
    {#if toast}
      <p class="toast" role="status">{toast}</p>
    {/if}
  </section>

  <section class="card" aria-labelledby="never-record-title">
    <h3 id="never-record-title">Never record these apps</h3>

    {#if detection.block.length === 0}
      <p class="empty">No apps blocked yet.</p>
    {:else}
      <ul class="rows">
        {#each detection.block as exe (exe)}
          <li class="row">
            <AppIcon exe={exe} name={blockName(exe)} iconB64={iconFor(exe)} size={28} />
            <span class="text">
              <span class="name">{blockName(exe)}</span>
              <span class="exe">{exe}</span>
            </span>
            <button
              type="button"
              class="remove"
              {disabled}
              aria-label={`Stop ignoring ${blockName(exe)}`}
              onclick={() => {
                blockError = null;
                commit(removeBlocked(currentSlice(), exe));
              }}>×</button
            >
          </li>
        {/each}
      </ul>
    {/if}

    <div class="picker-row" inert={disabled}>
      <AppPicker
        apps={blockPicks}
        placeholder="Select an app to ignore…"
        emptyText="Open the app, then press refresh"
        {busy}
        onPick={onPickBlock}
        onRefresh={refresh}
      />
    </div>

    {#if blockError}
      <p class="error" role="alert">{blockError}</p>
    {/if}

    <details class="note">
      <summary>Always ignored</summary>
      <p>Built-in: browsers, Discord, Steam, overlays and system apps are always ignored.</p>
    </details>
  </section>

  <details class="advanced">
    <summary>Advanced</summary>

    <FieldRow
      label="Which games to record"
      path="detection.mode"
      help="Allow list mode records only the games in your list above."
    >
      {#snippet children(ctl)}
        <Select
          {...ctl}
          value={detection.mode}
          options={MODE_OPTIONS}
          {disabled}
          onchange={(v) => setPath('detection.mode', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow
      label="After I stop a session by hand"
      path="detection.after_manual_stop"
      help="Controls whether detection starts a new session on its own."
    >
      {#snippet children(ctl)}
        <Select
          {...ctl}
          value={detection.after_manual_stop}
          options={AFTER_STOP_OPTIONS}
          {disabled}
          onchange={(v) => setPath('detection.after_manual_stop', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow
      label="Keep recording after the game closes"
      path="detection.stop_grace_s"
      help="Seconds to keep recording after the game window closes. 0 stops at once."
    >
      {#snippet children(ctl)}
        <NumberField
          {...ctl}
          value={detection.stop_grace_s}
          min={STOP_GRACE_MIN_S}
          max={STOP_GRACE_MAX_S}
          step={1}
          unit="seconds"
          {disabled}
          onchange={(v) => setPath('detection.stop_grace_s', v)}
        />
      {/snippet}
    </FieldRow>
  </details>
{/if}

<style>
  h3 {
    margin: 0 0 4px;
  }

  .card {
    margin-top: 18px;
    padding: 14px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .sub,
  .empty {
    margin: 4px 0 10px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .rows {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .name,
  .exe {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .exe {
    font-family: ui-monospace, Consolas, monospace;
    font-size: 11px;
    color: var(--text-dim);
  }

  .running {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #3ddc84;
  }

  .picker-row {
    display: flex;
  }

  .remove {
    padding: 2px 9px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text-dim);
    font: inherit;
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
  }

  .remove:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--accent);
  }

  .remove:focus-visible {
    border-radius: 4px;
  }

  summary:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: 4px;
  }

  .error {
    margin: 10px 0 0;
    font-size: 13px;
    color: var(--danger);
  }

  .toast {
    margin: 10px 0 0;
    font-size: 13px;
    color: #3ddc84;
  }

  .note {
    margin-top: 12px;
    font-size: 13px;
  }

  .note > summary,
  .advanced > summary {
    cursor: pointer;
    color: var(--text-dim);
    font-weight: 600;
    padding: 6px 0;
  }

  .note p {
    margin: 6px 0 0;
    color: var(--text-dim);
  }

  .advanced {
    margin-top: 18px;
  }
</style>
