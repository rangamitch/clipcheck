<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { liveStatus } from '../../status';
  import {
    audioApps,
    audioDevices,
    loadAudioDevices,
    micLevel,
    recordMicTest,
    refreshAudioApps,
    startAudioAppsPolling,
    startMicMeter,
  } from '../../discovery';
  import AppIcon from '../fields/AppIcon.svelte';
  import AppPicker, { type PickerApp } from '../fields/AppPicker.svelte';
  import FieldRow from '../fields/FieldRow.svelte';
  import MeterBar from '../fields/MeterBar.svelte';
  import Select from '../fields/Select.svelte';
  import Toggle from '../fields/Toggle.svelte';
  import VolumeSlider from '../fields/VolumeSlider.svelte';
  import {
    DEFAULT_DEVICE,
    GATE_MAX_DB,
    GATE_MIN_DB,
    MAX_APPS,
    SELF_EXE,
    addApp,
    bitrateOptions,
    canAddSource,
    micDeviceOptions,
    micTestVerdict,
    removeApp,
    setAppField,
    trackPlanPreview,
  } from '../audio';
  import { diagnosticAt, fieldDiagnostic } from '../pure';
  import { frameCoalesced } from '../frame';
  import { selectTab } from '../tabs';
  import { settings, setPath, type AudioCodec } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  /** The gate slider writes at most once per frame while it is dragged. */
  const dragGateDb = frameCoalesced((db: number) => setPath('audio.mic.gate_db', db));
  /** The push-to-talk release slider writes at most once per frame while it is dragged. */
  const dragPttReleaseMs = frameCoalesced((ms: number) => setPath('audio.mic.ptt_release_ms', ms));

  const audio = $derived($settings.draft?.audio);
  const diagnostics = $derived($settings.diagnostics);
  const connected = $derived($liveStatus.connected);
  const muteMode = $derived($settings.draft?.hotkeys?.session?.mute_mic_mode ?? 'toggle');
  /** Push-to-talk keys set on the Hotkeys tab. */
  const pttKeyCount = $derived($settings.draft?.hotkeys?.session?.push_to_talk?.length ?? 0);
  /** PTT is active with keys set, or with the legacy mute-key hold mode (which still works). */
  const pttActive = $derived(pttKeyCount > 0 || muteMode === 'push_to_talk');

  const CODEC_OPTIONS: { value: AudioCodec; label: string; disabled?: boolean; title?: string }[] = [
    { value: 'aac', label: 'AAC-LC' },
    { value: 'opus', label: 'Opus (coming later)', disabled: true, title: 'Coming later' },
  ];

  let pickError = $state('');
  let refreshing = $state(false);

  // Apps that played audio, plus the exes already listed (so the picker can skip them).
  const pickerApps = $derived<PickerApp[]>(
    $audioApps.map((a) => ({ exe: a.exe, name: a.name, iconB64: a.iconB64, active: a.active, hint: a.exe })),
  );
  const listedExes = $derived((audio?.apps ?? []).map((a) => a.exe));
  const pickerExclude = $derived([...listedExes, SELF_EXE]);

  const preview = $derived(audio ? trackPlanPreview(audio) : null);
  /** Adding one more enabled source would exceed the track limit, or the app list is full. */
  const addBlocked = $derived(!audio || (audio.apps.length >= MAX_APPS || !canAddSource(audio)));
  const addMessage = $derived(
    !audio
      ? ''
      : audio.apps.length >= MAX_APPS
        ? `The list holds at most ${MAX_APPS} apps. Remove one to add another.`
        : !canAddSource(audio)
          ? 'The track limit is 8. Turn off a source or separate tracks to add another app.'
          : '',
  );
  const separateBlocked = $derived(
    !audio || (!audio.separate_tracks && trackPlanPreview({ ...audio, separate_tracks: true }).over),
  );

  const micDevice = $derived(audio?.mic.device ?? DEFAULT_DEVICE);
  const micOptions = $derived(micDeviceOptions($audioDevices.capture, micDevice));
  const bitrateOpts = $derived(
    bitrateOptions(audio?.bitrate_kbps ?? 192).map((v) => ({ value: String(v), label: `${v} kbps` })),
  );

  /** Gate state text while the meter runs. The gate opens when the level rises above the threshold. */
  const gateOpen = $derived(
    audio ? audio.mic.gate_enabled && $micLevel.active && $micLevel.rmsDb > audio.mic.gate_db : false,
  );

  onMount(() => {
    const stopPolling = startAudioAppsPolling();
    void loadAudioDevices();
    return stopPolling;
  });

  // The mic meter runs only while this tab is open, the recorder is reachable, and it is
  // pointed at the saved device. Changing the device restarts it. Cleanup stops it.
  $effect(() => {
    if (!connected) return;
    const device = micDevice === DEFAULT_DEVICE ? null : micDevice;
    return startMicMeter(device);
  });

  // Microphone test: a 5 s recording from the selected device, played back in place.
  let testing = $state(false);
  let clipUrl = $state<string | null>(null);
  let testMessage = $state('');
  let audioEl = $state<HTMLAudioElement | undefined>(undefined);

  async function runMicTest(): Promise<void> {
    testing = true;
    testMessage = 'Recording 5 seconds… speak now.';
    try {
      const r = await recordMicTest(micDevice === DEFAULT_DEVICE ? null : micDevice);
      if (clipUrl) URL.revokeObjectURL(clipUrl);
      clipUrl = URL.createObjectURL(new Blob([r.wav], { type: 'audio/wav' }));
      testMessage =
        micTestVerdict(r.peakDb) === 'silent'
          ? 'Nothing was picked up. Check the microphone is connected, not muted, and its input volume is up.'
          : 'Recorded 5 seconds. Press play to hear it.';
      await tick();
      // Autoplay can be blocked; the controls still work, so the rejection is ignored.
      try {
        audioEl?.play().catch(() => undefined);
      } catch {
        // play() can throw synchronously in some hosts; the controls still work.
      }
    } catch (e) {
      testMessage = `The microphone test failed: ${e instanceof Error ? e.message : String(e)}`;
    } finally {
      testing = false;
    }
  }

  onDestroy(() => {
    if (clipUrl) URL.revokeObjectURL(clipUrl);
  });

  async function refresh(): Promise<void> {
    refreshing = true;
    try {
      await refreshAudioApps();
    } finally {
      refreshing = false;
    }
  }

  function pickApp(app: PickerApp): void {
    if (!audio) return;
    const r = addApp(audio.apps, { exe: app.exe, name: app.name });
    if (r.ok) {
      pickError = '';
      setPath('audio.apps', r.apps);
    } else {
      pickError = r.error;
    }
  }

  function removeEntry(exe: string): void {
    if (!audio) return;
    pickError = '';
    setPath('audio.apps', removeApp(audio.apps, exe));
  }

  function setAppGain(exe: string, db: number): void {
    if (!audio) return;
    setPath('audio.apps', setAppField(audio.apps, exe, { gain_db: db }));
  }

  function setAppEnabled(exe: string, on: boolean): void {
    if (!audio) return;
    setPath('audio.apps', setAppField(audio.apps, exe, { enabled: on }));
  }

  /** The icon from the live app list, when the app is currently known to the daemon. */
  function iconFor(exe: string): string | null {
    const found = $audioApps.find((a) => a.exe.toLowerCase() === exe.toLowerCase());
    return found?.iconB64 ?? null;
  }

  function appDiagnostic(index: number) {
    return fieldDiagnostic(`audio.apps[${index}].gain_db`, diagnostics);
  }

  const listDiag = $derived(diagnosticAt('audio.apps', diagnostics));
  const separateDiag = $derived(fieldDiagnostic('audio.separate_tracks', diagnostics));
</script>

{#if audio}
  <h2>Audio</h2>
  <p class="intro">
    Choose what goes into your clips and how loud each source is. The game's video is always
    recorded. Audio sources are added here, and only apps that have played sound while ClipCheck
    is running can be picked.
  </p>

  <section class="card" aria-labelledby="audio-recording-heading">
    <h3 id="audio-recording-heading">Recording Audio</h3>

    <ul class="sources">
      <li class="source" class:off={!audio.game.enabled}>
        <input
          type="checkbox"
          class="check"
          aria-label="Record game audio"
          checked={audio.game.enabled}
          disabled={disabled || (!audio.game.enabled && !canAddSource(audio))}
          onchange={(e) => setPath('audio.game.enabled', e.currentTarget.checked)}
        />
        <span class="icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="20" height="20">
            <path
              d="M6 11h4M8 9v4M15 12h.01M18 10h.01M17.32 5H6.68a4 4 0 0 0-3.98 3.59C2.6 9.42 2 14.46 2 16a3 3 0 0 0 3 3c1 0 1.5-.5 2-1l1.41-1.41A2 2 0 0 1 9.83 16h4.34a2 2 0 0 1 1.41.59L17 18c.5.5 1 1 2 1a3 3 0 0 0 3-3c0-1.55-.6-6.58-.69-7.26A4 4 0 0 0 17.32 5z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </span>
        <span class="name">Game audio</span>
        <VolumeSlider
          valueDb={audio.game.gain_db}
          {disabled}
          ariaLabel="Game audio volume"
          onChange={(db) => setPath('audio.game.gain_db', db)}
        />
      </li>

      <li class="source" class:off={!audio.mic.enabled}>
        <input
          type="checkbox"
          class="check"
          aria-label="Record microphone"
          checked={audio.mic.enabled}
          disabled={disabled || (!audio.mic.enabled && !canAddSource(audio))}
          onchange={(e) => setPath('audio.mic.enabled', e.currentTarget.checked)}
        />
        <span class="icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="20" height="20">
            <rect x="9" y="2" width="6" height="12" rx="3" fill="none" stroke="currentColor" stroke-width="1.8" />
            <path
              d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
            />
          </svg>
        </span>
        <span class="name">Microphone</span>
        <VolumeSlider
          valueDb={audio.mic.gain_db}
          {disabled}
          ariaLabel="Microphone volume"
          onChange={(db) => setPath('audio.mic.gain_db', db)}
        />
      </li>

      <li class="source" class:off={!audio.other_apps.enabled}>
        <input
          type="checkbox"
          class="check"
          aria-label="Record everything else"
          checked={audio.other_apps.enabled}
          disabled={disabled || (!audio.other_apps.enabled && !canAddSource(audio))}
          onchange={(e) => setPath('audio.other_apps.enabled', e.currentTarget.checked)}
        />
        <span class="icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="20" height="20">
            <path
              d="M11 5 6 9H2v6h4l5 4V5zM15.5 8.5a5 5 0 0 1 0 7M19 5a10 10 0 0 1 0 14"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linejoin="round"
              stroke-linecap="round"
            />
          </svg>
        </span>
        <span class="name">
          Everything else
          <span class="help">All other apps that are not listed here</span>
        </span>
        <VolumeSlider
          valueDb={audio.other_apps.gain_db}
          {disabled}
          ariaLabel="Everything else volume"
          onChange={(db) => setPath('audio.other_apps.gain_db', db)}
        />
      </li>

      {#each audio.apps as app, i (app.exe)}
        {@const diag = appDiagnostic(i)}
        <li class="source" class:off={!app.enabled}>
          <input
            type="checkbox"
            class="check"
            aria-label={`Record ${app.name}`}
            checked={app.enabled}
            disabled={disabled || (!app.enabled && !canAddSource(audio))}
            onchange={(e) => setAppEnabled(app.exe, e.currentTarget.checked)}
          />
          <AppIcon exe={app.exe} name={app.name} iconB64={iconFor(app.exe)} size={24} />
          <span class="name">
            {app.name}
            <span class="help">{app.exe}</span>
          </span>
          <VolumeSlider
            valueDb={app.gain_db}
            {disabled}
            ariaLabel={`${app.name} volume`}
            onChange={(db) => setAppGain(app.exe, db)}
          />
          <button
            type="button"
            class="remove"
            aria-label={`Remove ${app.name}`}
            title="Remove"
            disabled={disabled}
            onclick={() => removeEntry(app.exe)}>×</button
          >
          {#if diag}
            <p class="message" class:warn={diag.severity === 'warning'}>{diag.message}</p>
          {/if}
        </li>
      {/each}
    </ul>

    {#if audio.apps.length === 0}
      <p class="empty">No apps added yet.</p>
    {/if}
    {#if listDiag}
      <p class="message" role="alert">{listDiag.message}</p>
    {/if}

    <div class="add" inert={disabled || addBlocked}>
      <AppPicker
        apps={pickerApps}
        exclude={pickerExclude}
        placeholder="Add app…"
        emptyText="Nothing found yet. Start an app that plays sound and it will appear here."
        groups={false}
        busy={refreshing}
        onPick={pickApp}
        onRefresh={refresh}
      />
    </div>
    <p class="help">Apps appear here when they play audio while ClipCheck is running.</p>
    {#if addMessage}
      <p class="message">{addMessage}</p>
    {/if}
    {#if pickError}
      <p class="message" role="alert">{pickError}</p>
    {/if}

    <div class="toggle-row">
      <div class="toggle-text">
        <span class="toggle-label" id="audio-separate-label">Separate audio tracks</span>
        <p class="help" id="audio-separate-help">
          Save each source on its own audio track so you can mute them separately when editing.
        </p>
      </div>
      <Toggle
        checked={audio.separate_tracks}
        labelId="audio-separate-label"
        describedBy="audio-separate-help"
        disabled={disabled || separateBlocked}
        onchange={(v) => setPath('audio.separate_tracks', v)}
      />
    </div>
    {#if separateDiag}
      <p class="message">{separateDiag.message}</p>
    {/if}

    {#if preview}
      <div class="preview" aria-live="polite">
        <span class="preview-label">Tracks in your clips ({preview.count} of 8)</span>
        <ul class="chips" aria-label="Track names">
          {#each preview.titles as title, t (t)}
            <li class="chip">{title}</li>
          {/each}
        </ul>
        {#if preview.over}
          <p class="message" role="alert">
            This would make {preview.count} tracks. The limit is 8. Turn off a source or separate
            tracks to save.
          </p>
        {/if}
      </div>
    {/if}
  </section>

  <section class="card" aria-labelledby="audio-mic-heading">
    <h3 id="audio-mic-heading">Microphone Settings</h3>

    <FieldRow label="Microphone device" path="audio.mic.device" help="The microphone your voice is recorded from.">
      {#snippet children(ctl)}
        <Select
          id={ctl.id}
          labelId={ctl.labelId}
          value={micDevice}
          options={micOptions}
          disabled={disabled}
          onchange={(v) => setPath('audio.mic.device', v)}
        />
      {/snippet}
    </FieldRow>

    <div class="meter-block">
      {#if connected}
        <MeterBar
          db={$micLevel.active ? $micLevel.rmsDb : -100}
          thresholdDb={audio.mic.gate_enabled ? audio.mic.gate_db : null}
          label="Microphone level"
        />
        <p class="help">
          {#if $micLevel.active}
            Speak to test your microphone.
            {#if audio.mic.gate_enabled}
              <span class:open={gateOpen} class="gate-state">{gateOpen ? 'Gate open' : 'Gate closed'}</span>
            {/if}
          {:else}
            Starting the level meter…
          {/if}
        </p>
      {:else}
        <MeterBar db={-100} label="Microphone level (recorder offline)" />
        <p class="help muted">The level meter is off while the recorder is offline.</p>
      {/if}
    </div>

    <div class="mic-test">
      <button
        type="button"
        class="mic-test-button"
        disabled={!connected || disabled || testing}
        onclick={() => void runMicTest()}>Test microphone</button
      >
      {#if !connected}
        <p class="help muted">The microphone test needs the recorder to be running.</p>
      {/if}
      <p class="help" aria-live="polite">{testMessage}</p>
      {#if clipUrl}
        <audio bind:this={audioEl} controls src={clipUrl} aria-label="Microphone test recording"></audio>
      {/if}
    </div>

    <div class="toggle-row">
      <div class="toggle-text">
        <span class="toggle-label" id="audio-gate-label">Noise gate</span>
        <p class="help" id="audio-gate-help">Silences the mic below this level to cut background noise.</p>
      </div>
      <Toggle
        checked={audio.mic.gate_enabled}
        labelId="audio-gate-label"
        describedBy="audio-gate-help"
        {disabled}
        onchange={(v) => setPath('audio.mic.gate_enabled', v)}
      />
    </div>

    <FieldRow
      label="Noise gate threshold"
      path="audio.mic.gate_db"
      help="Silences the mic below this level to cut background noise. The white marker on the meter shows it."
    >
      {#snippet children(ctl)}
        <div class="gate-slider">
          <input
            id={ctl.id}
            type="range"
            min={GATE_MIN_DB}
            max={GATE_MAX_DB}
            step="1"
            value={audio.mic.gate_db}
            disabled={disabled || !audio.mic.gate_enabled}
            aria-labelledby={ctl.labelId}
            aria-describedby={ctl.describedBy}
            aria-valuetext={`${audio.mic.gate_db} dB`}
            oninput={(e) => dragGateDb(Number(e.currentTarget.value))}
          />
          <span class="value">{audio.mic.gate_db} dB</span>
        </div>
      {/snippet}
    </FieldRow>

    <div class="ptt">
      <div class="toggle-text">
        <span class="toggle-label">Push to talk</span>
        <p class="help">
          {#if pttKeyCount > 0}
            {pttKeyCount} push-to-talk {pttKeyCount === 1 ? 'key is' : 'keys are'} set. The mic records only while one is held.
          {:else if muteMode === 'push_to_talk'}
            The mute key is set to hold-to-talk (older setting).
          {:else}
            Off. No push-to-talk keys are set.
          {/if}
        </p>
      </div>
      <button type="button" class="link" onclick={() => selectTab('hotkeys')}>Configure in Hotkeys</button>
    </div>

    {#if pttActive}
      <div class="toggle-row">
        <div class="toggle-text">
          <span class="toggle-label" id="audio-ptt-track-label">Record push to talk on its own track</span>
          <p class="help" id="audio-ptt-track-help">
            {#if audio.separate_tracks}
              Adds a Push to talk track that only has your voice while the key is held. The Microphone track keeps
              recording normally. The Mix track gets the push-to-talk version.
            {:else}
              Needs "Separate audio tracks" on. Turn that on above first.
            {/if}
          </p>
        </div>
        <Toggle
          checked={audio.mic.ptt_separate_track}
          labelId="audio-ptt-track-label"
          describedBy="audio-ptt-track-help"
          disabled={disabled || !audio.separate_tracks}
          onchange={(v) => setPath('audio.mic.ptt_separate_track', v)}
        />
      </div>

      <FieldRow
        label="Keep mic open after release"
        path="audio.mic.ptt_release_ms"
        help="Keeps the mic recording for this long after you let go of the key, so the end of a phrase is not cut off."
      >
        {#snippet children(ctl)}
          <div class="gate-slider">
            <input
              id={ctl.id}
              type="range"
              min="0"
              max="2000"
              step="50"
              value={audio.mic.ptt_release_ms}
              disabled={disabled}
              aria-labelledby={ctl.labelId}
              aria-describedby={ctl.describedBy}
              aria-valuetext={`${audio.mic.ptt_release_ms} ms`}
              oninput={(e) => dragPttReleaseMs(Number(e.currentTarget.value))}
            />
            <span class="value">{audio.mic.ptt_release_ms} ms</span>
          </div>
        {/snippet}
      </FieldRow>
    {/if}
  </section>

  <section class="card" aria-labelledby="audio-encoding-heading">
    <h3 id="audio-encoding-heading">Encoding</h3>

    <FieldRow label="Audio codec" path="audio.codec" help="AAC-LC is used in this version. Opus is planned for a later release.">
      {#snippet children(ctl)}
        <Select
          id={ctl.id}
          labelId={ctl.labelId}
          value={audio.codec}
          options={CODEC_OPTIONS}
          disabled={disabled}
          onchange={(v) => setPath('audio.codec', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow label="AAC bitrate per track" path="audio.bitrate_kbps" help="Used by every audio track. Higher is better quality and bigger files.">
      {#snippet children(ctl)}
        <Select
          id={ctl.id}
          labelId={ctl.labelId}
          value={String(audio.bitrate_kbps)}
          options={bitrateOpts}
          disabled={disabled}
          onchange={(v) => setPath('audio.bitrate_kbps', Number(v))}
        />
      {/snippet}
    </FieldRow>
  </section>
{/if}

<style>
  h3 {
    margin: 0 0 10px;
  }

  p {
    margin: 0;
  }

  .intro,
  .help,
  .empty {
    font-size: 13px;
    color: var(--text-dim);
  }

  .intro {
    margin-bottom: 14px;
  }

  .help {
    margin-top: 2px;
  }

  .help.muted {
    margin-top: 6px;
  }

  .message {
    margin-top: 4px;
    font-size: 13px;
    color: var(--danger);
  }

  .message.warn {
    color: var(--text-dim);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 16px;
    padding: 14px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .sources {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .source {
    display: grid;
    grid-template-columns: auto auto minmax(120px, 1fr) minmax(200px, 300px) auto;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: var(--radius);
    background: var(--surface-2);
  }

  .source.off .name,
  .source.off .icon {
    opacity: 0.6;
  }

  .source :global(.volume) {
    min-width: 0;
  }

  .source .message {
    grid-column: 1 / -1;
  }

  .check {
    width: 16px;
    height: 16px;
    margin: 0;
    accent-color: var(--accent);
  }

  .icon {
    display: inline-grid;
    place-items: center;
    width: 24px;
    height: 24px;
    color: var(--text-dim);
  }

  .name {
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name .help {
    font-family: ui-monospace, Consolas, monospace;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .remove {
    width: 26px;
    height: 26px;
    padding: 0;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface);
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

  .add {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .add[inert] {
    opacity: 0.5;
  }

  .toggle-row,
  .ptt {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }

  .toggle-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1 1 260px;
  }

  .toggle-label {
    font-weight: 600;
    color: var(--text);
  }

  .preview {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }

  .preview-label {
    font-size: 13px;
    color: var(--text-dim);
  }

  .chips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .chip {
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    font-size: 12px;
    color: var(--text);
  }

  .meter-block {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 8px;
  }

  .mic-test {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 8px;
    align-items: flex-start;
  }

  .mic-test-button {
    padding: 6px 12px;
    border: 1px solid var(--accent);
    border-radius: 6px;
    background: none;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }

  .mic-test-button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .mic-test audio {
    max-width: 100%;
  }

  .gate-state {
    margin-left: 8px;
    color: var(--text-dim);
  }

  .gate-state.open {
    color: #3ddc84;
  }

  .gate-slider {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 260px;
  }

  .gate-slider input[type='range'] {
    flex: 1;
    accent-color: var(--accent);
  }

  .gate-slider input:disabled {
    opacity: 0.5;
  }

  .value {
    min-width: 64px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  button.link {
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }

  @media (max-width: 720px) {
    .source {
      grid-template-columns: auto auto 1fr auto;
    }

    .source :global(.volume) {
      grid-column: 1 / -1;
    }
  }

  @media (max-width: 420px) {
    .source {
      grid-template-columns: auto 1fr auto;
    }
  }
</style>
