<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { editorApi, type EncoderInfo } from './api';
  import type { CacheInfo, EditorSettings, Preset } from './types';
  import { encoderOptionsFor } from './exportEncoders';
  import { formatBytes } from '../format';
  import { pickFolder } from '../recordingActions';

  /** Wait this long after the last change before saving, in milliseconds. */
  const SAVE_DEBOUNCE_MS = 400;
  /** How long the Clear cache button waits for a second click, in milliseconds. */
  const CLEAR_CONFIRM_MS = 4000;

  /** One dropdown entry for an export preset. */
  interface PresetOption {
    value: string;
    label: string;
  }

  /** Used only when the backend preset list cannot be loaded (ids from crates/timeline). */
  const FALLBACK_PRESETS: readonly PresetOption[] = [
    { value: 'original_quality', label: 'Original quality' },
    { value: 'high_1080p60', label: 'High 1080p60' },
    { value: 'balanced_1080p30', label: 'Balanced 1080p30' },
    { value: 'small_720p30', label: 'Small 720p30' },
  ];

  const CANVAS_OPTIONS: readonly { value: string; label: string }[] = [
    { value: 'match_first_clip', label: 'Match first clip' },
    { value: '16:9', label: '16:9 landscape' },
    { value: '9:16', label: '9:16 vertical' },
    { value: '1:1', label: '1:1 square' },
    { value: '4:5', label: '4:5 portrait' },
  ];

  const QUALITY_OPTIONS: readonly { value: string; label: string }[] = [
    { value: 'auto', label: 'Automatic' },
    { value: 'full', label: 'Full' },
    { value: 'half', label: 'Half' },
    { value: 'quarter', label: 'Quarter' },
  ];

  /** Settings as last loaded or saved. Null until the first load succeeds. */
  let draft = $state<EditorSettings | null>(null);
  let loadError = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  let browseError = $state<string | null>(null);
  /** True after a save succeeds and until the next change. */
  let saved = $state(false);
  /** True while a save request is in flight. */
  let saving = $state(false);
  /** True while a change waits for the debounce timer or for the running save to finish. */
  let pending = $state(false);

  /** Preset list from `editor_list_presets`, built-in first. Null until loaded or when loading failed. */
  let presetList = $state<PresetOption[] | null>(null);
  /** Shown when the preset list could not be loaded and the short fallback list is used. */
  let presetsNote = $state<string | null>(null);
  /** Encoders from `editor_list_encoders`. Null until loaded or when loading failed. */
  let encoderList = $state<EncoderInfo[] | null>(null);
  /** Cache size by kind from the backend. Null until the first read succeeds. */
  let cacheInfo = $state<CacheInfo | null>(null);
  let cacheError = $state<string | null>(null);
  /** True while a clear-cache request is in flight. */
  let cacheBusy = $state(false);
  /** True after the first Clear cache click, for the length of CLEAR_CONFIRM_MS. */
  let confirmingClear = $state(false);
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;

  let timer: ReturnType<typeof setTimeout> | null = null;

  /** Text boxes whose saved value may be normalised by the backend. */
  const TEXT_KEYS = ['cacheDir', 'filenameTemplate'] as const;
  type TextKey = (typeof TEXT_KEYS)[number];
  /** The text box the user is typing in, or null. */
  let editingKey = $state<TextKey | null>(null);
  /** Normalised text from a save that arrived while its box was being edited; applied on blur. */
  const heldText: Partial<Record<TextKey, string>> = {};

  function messageOf(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /**
   * Presets offered in the export preset list: the backend list (built-in, then the user's own).
   * Without it, the fallback list plus the user's presets from settings. A stored id that is in
   * neither is kept as an extra entry so the select still shows the saved value.
   */
  const presetOptions = $derived.by((): PresetOption[] => {
    const base: PresetOption[] = presetList ?? [
      ...FALLBACK_PRESETS,
      ...(draft?.customPresets ?? []).map((p) => ({ value: p.id, label: p.label })),
    ];
    const stored = draft?.defaultPreset;
    if (stored === undefined || base.some((o) => o.value === stored)) return base;
    return [...base, { value: stored, label: `${stored} (not found)` }];
  });

  /** Encoder dropdown entries: Auto, the real encoders by GPU name, and Software (CPU). */
  const encoderOptions = $derived(encoderOptionsFor(encoderList, draft?.encoder));

  /** One line describing cache use, e.g. "Using 1.5 GB of 10 GB (thumbnails ..., waveforms ..., audio ...)". */
  const cacheUsageText = $derived.by((): string => {
    if (cacheInfo === null) return cacheError === null ? 'Checking cache size…' : '';
    const info = cacheInfo;
    const limit = draft?.cacheMaxGb ?? 0;
    return `Using ${formatBytes(info.totalBytes)} of ${limit} GB (thumbnails ${formatBytes(info.thumbsBytes)}, waveforms ${formatBytes(info.peaksBytes)}, audio ${formatBytes(info.audioBytes)})`;
  });

  /** Clear cache button label: idle, first click (confirm), or running. */
  const clearLabel = $derived(
    cacheBusy ? 'Clearing…' : confirmingClear ? 'Click again to clear' : 'Clear cache',
  );

  /** Status line text. Errors win, then pending or saving work, then the saved note. */
  const statusText = $derived.by(() => {
    if (saveError !== null) return `Not saved: ${saveError}`;
    if (saving || pending) return 'Saving…';
    if (saved) return 'Saved';
    return '';
  });

  async function load(): Promise<void> {
    try {
      draft = await editorApi.getSettings();
      loadError = null;
    } catch (e) {
      loadError = messageOf(e);
    }
  }

  /** Sends the current draft. A change made while a save runs is sent by a follow-up save. */
  async function save(): Promise<void> {
    if (draft === null || saving) return;
    saving = true;
    pending = false;
    saveError = null;
    saved = false;
    try {
      const reply = await editorApi.setSettings($state.snapshot(draft) as EditorSettings);
      // Adopt the normalised values only when nothing newer is waiting, so typing is not overwritten.
      if (!pending && timer === null) adoptReply(reply);
      saved = true;
    } catch (e) {
      saveError = messageOf(e);
    } finally {
      saving = false;
      if (pending && timer === null) void save();
    }
  }

  /**
   * Takes the backend's normalised settings. A text box the user is editing keeps what they typed;
   * its normalised value is held until the box loses focus.
   */
  function adoptReply(reply: EditorSettings): void {
    if (draft === null) {
      draft = reply;
      return;
    }
    const next: EditorSettings = { ...reply };
    for (const key of TEXT_KEYS) {
      if (editingKey === key) {
        heldText[key] = reply[key];
        next[key] = draft[key];
      } else {
        delete heldText[key];
      }
    }
    draft = next;
  }

  /** Ends editing of a text box and applies any normalised value held for it. */
  function onTextBlur(key: TextKey): void {
    editingKey = null;
    const held = heldText[key];
    delete heldText[key];
    if (draft !== null && held !== undefined) draft[key] = held;
  }

  /** Marks a text box as being edited. */
  function onTextFocus(key: TextKey): void {
    editingKey = key;
  }

  /** Marks the draft as changed and restarts the debounce timer. */
  function scheduleSave(): void {
    pending = true;
    saved = false;
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void save();
    }, SAVE_DEBOUNCE_MS);
  }

  /** Writes one field of the draft and schedules a save. */
  function setField<K extends keyof EditorSettings>(key: K, value: EditorSettings[K]): void {
    if (draft === null) return;
    draft[key] = value;
    scheduleSave();
  }

  /** Reads a text box value. */
  function textOf(e: Event): string {
    return (e.currentTarget as HTMLInputElement | HTMLSelectElement).value;
  }

  /** Reads a number box value, or null when it is empty or not a number. */
  function numberOf(e: Event): number | null {
    const raw = (e.currentTarget as HTMLInputElement).value.trim();
    if (raw === '') return null;
    const value = Number(raw);
    return Number.isFinite(value) ? value : null;
  }

  /** Keeps a number within the range the backend accepts. */
  function clamp(value: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, value));
  }

  /** Number boxes apply on commit (Enter or leaving the box), so partial typing is not clamped. */
  function onNumber(key: 'autosaveS' | 'cacheMaxGb', min: number, max: number) {
    return (e: Event) => {
      const value = numberOf(e);
      if (value === null) {
        if (draft !== null) (e.currentTarget as HTMLInputElement).value = String(draft[key]);
        return;
      }
      setField(key, clamp(Math.round(value), min, max));
    };
  }

  /** Opens the folder picker for the cache folder, starting at the current value. */
  async function browseCache(): Promise<void> {
    if (draft === null) return;
    browseError = null;
    try {
      const picked = await pickFolder(draft.cacheDir.trim() || undefined);
      if (picked) setField('cacheDir', picked);
    } catch (e) {
      browseError = `Could not open the folder picker: ${messageOf(e)}`;
    }
  }

  /** Reads the export preset list. A failure keeps the fallback list and shows a note. */
  async function loadPresets(): Promise<void> {
    try {
      const all = await editorApi.listPresets();
      const toOption = (p: Preset): PresetOption => ({ value: p.id, label: p.label });
      presetList = [
        ...all.filter((p) => p.builtin).map(toOption),
        ...all.filter((p) => !p.builtin).map(toOption),
      ];
      presetsNote = null;
    } catch (e) {
      presetList = null;
      presetsNote = `Could not load the full preset list: ${messageOf(e)}`;
    }
  }

  /** Reads the encoder list. A failure leaves only Auto and Software offered. */
  async function loadEncoders(): Promise<void> {
    try {
      encoderList = await editorApi.listEncoders('h264');
    } catch {
      encoderList = null;
    }
  }

  /** Reads the cache size. */
  async function loadCache(): Promise<void> {
    try {
      cacheInfo = await editorApi.cacheInfo();
      cacheError = null;
    } catch (e) {
      cacheError = `Could not read the cache size: ${messageOf(e)}`;
    }
  }

  /** Stops the inline confirmation of Clear cache. */
  function cancelClearConfirm(): void {
    confirmingClear = false;
    if (confirmTimer !== null) {
      clearTimeout(confirmTimer);
      confirmTimer = null;
    }
  }

  /** First click asks for a second click within CLEAR_CONFIRM_MS; the second click clears. */
  function onClearClick(): void {
    if (cacheBusy) return;
    if (!confirmingClear) {
      confirmingClear = true;
      confirmTimer = setTimeout(cancelClearConfirm, CLEAR_CONFIRM_MS);
      return;
    }
    void clearCache();
  }

  /** Deletes the cache files and shows what is left. */
  async function clearCache(): Promise<void> {
    cancelClearConfirm();
    cacheBusy = true;
    cacheError = null;
    try {
      cacheInfo = await editorApi.clearCache();
    } catch (e) {
      cacheError = `Could not clear the cache: ${messageOf(e)}`;
    } finally {
      cacheBusy = false;
    }
  }

  onMount(() => {
    void load();
    void loadPresets();
    void loadEncoders();
    void loadCache();
  });

  onDestroy(() => {
    cancelClearConfirm();
    // Leaving the page saves a change that is still waiting for the debounce.
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
      void save();
    }
  });
</script>

<h2>Video editor</h2>
<p class="intro dim">
  Projects: new-project defaults. Cache: thumbnails, waveforms and audio files. Export: defaults for
  the Export dialog (resolution, fps and quality are chosen per export).
</p>

{#if loadError !== null}
  <p class="message" role="alert">Could not load video editor settings: {loadError}</p>
  <button type="button" class="retry" onclick={() => void load()}>Try again</button>
{:else if draft === null}
  <p class="dim">Loading video editor settings…</p>
{:else}
  {@const d = draft}

  <section class="group" aria-labelledby="ve-group-project">
    <h3 id="ve-group-project">Projects</h3>

    <div class="field">
      <label class="label" for="ve-autosave">Autosave interval</label>
      <p class="help" id="ve-autosave-help">How often an open project is saved, in seconds (10 to 600).</p>
      <div class="control">
        <input
          id="ve-autosave"
          type="number"
          min="10"
          max="600"
          step="10"
          aria-describedby="ve-autosave-help"
          value={d.autosaveS}
          onchange={onNumber('autosaveS', 10, 600)}
        />
        <span class="unit">seconds</span>
      </div>
    </div>

    <div class="field">
      <label class="label" for="ve-canvas">Default canvas</label>
      <p class="help">Canvas size used by a new project.</p>
      <div class="control">
        <select
          id="ve-canvas"
          value={d.defaultCanvas}
          onchange={(e) => setField('defaultCanvas', textOf(e))}
        >
          {#each CANVAS_OPTIONS as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
      </div>
    </div>

    <div class="field">
      <span class="label">Timeline defaults</span>
      <div class="checks">
        <label class="check">
          <input
            type="checkbox"
            checked={d.magneticDefault}
            onchange={(e) => setField('magneticDefault', e.currentTarget.checked)}
          />
          <span>Magnetic timeline on by default</span>
        </label>
        <label class="check">
          <input
            type="checkbox"
            checked={d.snapDefault}
            onchange={(e) => setField('snapDefault', e.currentTarget.checked)}
          />
          <span>Snapping on by default</span>
        </label>
      </div>
    </div>

    <div class="field">
      <label class="label" for="ve-preview">Preview quality</label>
      <p class="help">
        Starting quality of the preview when a project opens. Lower quality plays more smoothly on
        slow computers. The monitor's own Quality menu still changes it.
      </p>
      <div class="control">
        <select
          id="ve-preview"
          value={d.previewQuality}
          onchange={(e) => setField('previewQuality', textOf(e))}
        >
          {#each QUALITY_OPTIONS as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
      </div>
    </div>
  </section>

  <section class="group" aria-labelledby="ve-group-cache">
    <h3 id="ve-group-cache">Cache</h3>

    <div class="field">
      <label class="label" for="ve-cache-dir">Cache folder</label>
      <p class="help" id="ve-cache-dir-help">
        Holds waveforms, thumbnails, and proxies. Leave empty for the default folder.
      </p>
      <div class="control path">
        <input
          id="ve-cache-dir"
          type="text"
          spellcheck="false"
          autocomplete="off"
          placeholder="Default cache folder"
          aria-describedby="ve-cache-dir-help"
          value={d.cacheDir}
          onfocus={() => onTextFocus('cacheDir')}
          onblur={() => onTextBlur('cacheDir')}
          oninput={(e) => setField('cacheDir', textOf(e))}
        />
        <button type="button" class="action" onclick={() => void browseCache()}>Browse…</button>
      </div>
      {#if browseError !== null}
        <p class="message" role="alert">{browseError}</p>
      {/if}
    </div>

    <div class="field">
      <label class="label" for="ve-cache-max">Cache size limit</label>
      <p class="help" id="ve-cache-max-help">
        Oldest cached thumbnails, waveforms and audio are removed when the cache goes over this limit.
      </p>
      <div class="control">
        <input
          id="ve-cache-max"
          type="number"
          min="1"
          max="100"
          step="1"
          aria-describedby="ve-cache-max-help"
          value={d.cacheMaxGb}
          onchange={onNumber('cacheMaxGb', 1, 100)}
        />
        <span class="unit">GB (1 to 100)</span>
      </div>
    </div>

    <div class="field">
      <span class="label">Cache usage</span>
      <p class="help" role="status" aria-live="polite">{cacheUsageText}</p>
      <div class="control">
        <button
          type="button"
          class="action"
          disabled={cacheBusy}
          aria-describedby="ve-cache-usage-note"
          onclick={onClearClick}
        >
          {clearLabel}
        </button>
      </div>
      <p class="help" id="ve-cache-usage-note">Clearing deletes the cached thumbnails, waveforms and audio files.</p>
      {#if cacheError !== null}
        <p class="message" role="alert">{cacheError}</p>
      {/if}
    </div>
  </section>

  <section class="group" aria-labelledby="ve-group-export">
    <h3 id="ve-group-export">Export</h3>

    <div class="field">
      <label class="label" for="ve-preset">Default export preset</label>
      <p class="help">Preset selected when the export dialog opens.</p>
      <div class="control">
        <select
          id="ve-preset"
          value={d.defaultPreset}
          onchange={(e) => setField('defaultPreset', textOf(e))}
        >
          {#each presetOptions as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
      </div>
      {#if presetsNote !== null}
        <p class="note dim">{presetsNote}</p>
      {/if}
    </div>

    <div class="field">
      <label class="label" for="ve-encoder">Default encoder</label>
      <p class="help" id="ve-encoder-help">Auto uses the GPU with the most video memory.</p>
      <div class="control">
        <select
          id="ve-encoder"
          aria-describedby="ve-encoder-help"
          value={d.encoder}
          onchange={(e) => setField('encoder', textOf(e))}
        >
          {#each encoderOptions as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
      </div>
    </div>

    <div class="field">
      <label class="label" for="ve-filename">File name template</label>
      <p class="help" id="ve-filename-help">
        Tokens: {'{project}'}, {'{date}'}, {'{time}'}, {'{yyyy-MM-dd HH-mm-ss}'}, {'{res}'},
        {'{preset}'}, {'{n}'}. The .mp4 extension is added for you.
      </p>
      <div class="control">
        <input
          id="ve-filename"
          type="text"
          spellcheck="false"
          autocomplete="off"
          aria-describedby="ve-filename-help"
          value={d.filenameTemplate}
          onfocus={() => onTextFocus('filenameTemplate')}
          onblur={() => onTextBlur('filenameTemplate')}
          oninput={(e) => setField('filenameTemplate', textOf(e))}
        />
      </div>
    </div>

    <div class="field">
      <span class="label">Export behaviour</span>
      <div class="checks">
        <label class="check">
          <input
            type="checkbox"
            checked={d.pauseWhileRecording}
            onchange={(e) => setField('pauseWhileRecording', e.currentTarget.checked)}
          />
          <span>Pause exports while recording</span>
        </label>
        <label class="check">
          <input
            type="checkbox"
            checked={d.openFolderWhenDone}
            onchange={(e) => setField('openFolderWhenDone', e.currentTarget.checked)}
          />
          <span>Open the folder when an export is done</span>
        </label>
        <label class="check">
          <input
            type="checkbox"
            checked={d.lowPriority}
            onchange={(e) => setField('lowPriority', e.currentTarget.checked)}
          />
          <span>Run exports at low priority</span>
        </label>
        <label class="check">
          <input
            type="checkbox"
            checked={d.alwaysAskLocation}
            onchange={(e) => setField('alwaysAskLocation', e.currentTarget.checked)}
          />
          <span>Always ask where to save exports</span>
        </label>
        <!--
          "Protect exports from automatic deletion" (protectExportsFromQuota) is hidden: the daemon's
          cache cleanup does not read this setting yet, so the toggle would do nothing. Restore it
          with the cleanup change.
        -->
      </div>
    </div>
  </section>

  <p class="status" role="status" aria-live="polite">{statusText}</p>
  {#if saveError !== null}
    <p class="message">Your change is still shown here. Change a field again to retry.</p>
  {/if}
{/if}

<style>
  .note {
    margin: 4px 0 0;
    font-size: 12px;
  }

  .intro {
    margin: 0 0 8px;
    font-size: 13px;
  }

  .dim {
    color: var(--text-dim);
  }

  .message {
    color: var(--danger);
    font-size: 13px;
  }

  .group {
    margin-top: 18px;
  }

  h3 {
    margin: 0 0 4px;
    font-size: 13px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-dim);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
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

  .control {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    width: min(100%, 640px);
  }

  .control.path input {
    flex: 1 1 280px;
  }

  input[type='text'],
  input[type='number'],
  select {
    padding: 7px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
  }

  input[type='text'] {
    flex: 1 1 280px;
    min-width: 0;
  }

  input[type='number'] {
    width: 110px;
  }

  select {
    min-width: 220px;
  }

  input:focus-visible,
  select:focus-visible,
  button:focus-visible,
  input[type='checkbox']:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .unit {
    font-size: 13px;
    color: var(--text-dim);
  }

  .checks {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    cursor: pointer;
  }

  .action,
  .retry {
    padding: 7px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }

  .action:hover,
  .retry:hover {
    border-color: var(--accent);
  }

  .action:disabled {
    opacity: 0.6;
    cursor: default;
    border-color: var(--border);
  }

  .retry {
    margin-top: 10px;
  }

  .status {
    margin: 14px 0 0;
    font-size: 13px;
    color: var(--text-dim);
  }
</style>
