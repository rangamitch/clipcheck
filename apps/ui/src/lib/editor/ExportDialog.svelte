<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { tick, untrack } from 'svelte';
  import { formatBytes } from '../format';
  import { editorApi } from './api';
  import { playhead, selection, snapshot, toast } from './store';
  import { formatDuration, secToTick } from './ticks';
  import {
    defaultExportSettings,
    type AudioExport,
    type EditorSettings,
    type EstimateReply,
    type ExportDefaults,
    type ExportKind,
    type ExportSettings,
    type Preset,
    type QualityMode,
    type Rate,
    type VideoCodec,
  } from './types';

  interface Props {
    /** Whether the dialog is shown. */
    open: boolean;
    /** Called when the dialog closes (Esc, Cancel, the close button, the backdrop, or after an export starts). */
    onclose: () => void;
  }

  let { open, onclose }: Props = $props();

  /** Delay after the last change before the estimate runs, ms. */
  const ESTIMATE_DEBOUNCE_MS = 300;
  const NAME_TEMPLATE_DEFAULT = defaultExportSettings().name_template;
  /** Audio-only export is not available in the engine yet, so the tab and its presets stay hidden. */
  const KIND_TABS: { id: ExportKind['kind']; label: string }[] = [
    { id: 'video', label: 'Video' },
    { id: 'still', label: 'Still frame (PNG/JPG)' },
  ];
  const AUDIO_BITRATES = [96, 128, 192, 256, 320];
  const SAMPLE_RATES = [
    { value: 44100, label: '44.1 kHz' },
    { value: 48000, label: '48 kHz' },
  ];
  const FPS_CHOICES = [24, 30, 60];
  /** Resolution choices by long side; the short side follows the canvas orientation and aspect. */
  const RES_CHOICES = [
    { id: '2160', label: '4K (2160p)', long: 3840 },
    { id: '1440', label: '1440p', long: 2560 },
    { id: '1080', label: '1080p', long: 1920 },
    { id: '720', label: '720p', long: 1280 },
    { id: '480', label: '480p', long: 854 },
    { id: '360', label: '360p', long: 640 },
  ];
  const SIZE_MIN = 128;
  const SIZE_MAX = 8192;
  const MBPS_MIN = 0.1;
  const MBPS_MAX = 200;
  const TARGET_MB_MIN = 1;
  const TARGET_MB_MAX = 64000;
  const DEFAULT_QUALITY = 70;
  const DEFAULT_MBPS = 8;
  const DEFAULT_TARGET_MB = 50;
  const DEFAULT_STILL_QUALITY = 90;

  /** Smart-lossless eligibility as the dialog uses it (the backend reply is untyped JSON). */
  interface SmartInfo {
    eligible: boolean;
    /** Why the timeline is not eligible; null when the engine gave no reason. */
    reason: string | null;
    notes: string[];
  }

  let loading = $state(true);
  let loadError = $state('');
  let defaults = $state.raw<ExportDefaults | null>(null);
  let settings = $state.raw<ExportSettings | null>(null);
  /** User presets only; built-in presets come from `defaults`. */
  let userPresets = $state.raw<Preset[]>([]);
  /** False while the name follows the template; true once the user types a name. */
  let nameEdited = $state(false);
  /** Resolution dropdown value: 'canvas', a RES_CHOICES id, or 'custom'. */
  let resChoice = $state('canvas');
  let lockAspect = $state(true);
  /** Width / height ratio kept while the aspect lock is on. */
  let aspect = $state(16 / 9);
  let widthText = $state('');
  let heightText = $state('');
  let qualitySlider = $state(DEFAULT_QUALITY);
  let bitrateText = $state(String(DEFAULT_MBPS));
  let cbrText = $state(String(DEFAULT_MBPS));
  let targetText = $state(String(DEFAULT_TARGET_MB));
  let stillQualityText = $state(String(DEFAULT_STILL_QUALITY));
  let presetSaving = $state(false);
  let presetLabel = $state('');
  let presetSaveError = $state('');
  /** Id of an existing user preset that the next save would replace; null when none is pending. */
  let replaceId = $state<string | null>(null);
  let estimate = $state.raw<EstimateReply | null>(null);
  let estimateError = $state('');
  let estimating = $state(false);
  let smart = $state.raw<SmartInfo | null>(null);
  let busy = $state(false);
  /** Error text for the footer (export, still frame, folder picker, preset delete). */
  let errorText = $state('');
  /** Path of the last saved still frame. */
  let stillPath = $state('');
  let dialogEl = $state<HTMLDivElement | undefined>(undefined);
  let presetSelect = $state<HTMLSelectElement | undefined>(undefined);
  /** Element that had focus before the dialog opened, restored when it closes. */
  let returnFocus: HTMLElement | null = null;
  let loadSeq = 0;
  let estimateSeq = 0;

  const canvas = $derived($snapshot?.project.canvas ?? null);
  const selectedIds = $derived($selection);
  const hasWorkArea = $derived(Boolean($snapshot?.project.work_area));
  /** Presets that export video or stills; audio-only presets are hidden until the engine supports them. */
  function videoPresets(list: Preset[]): Preset[] {
    return list.filter((p) => p.settings.kind.kind !== 'audio_only');
  }
  const builtinPresets = $derived(videoPresets((defaults?.presets ?? []).filter((p) => p.builtin)));
  const shownUserPresets = $derived(videoPresets(userPresets));
  const allPresets = $derived([...builtinPresets, ...shownUserPresets]);
  /** The preset id the settings came from, or 'custom' once a field was edited by hand. */
  const presetId = $derived(
    settings && allPresets.some((p) => p.id === settings?.preset) ? settings.preset : 'custom',
  );
  const selectedPreset = $derived(allPresets.find((p) => p.id === presetId) ?? null);
  const kindId = $derived(settings?.kind.kind ?? 'video');
  const still = $derived(stillOf(settings));
  const rangeKind = $derived(settings?.range.kind ?? 'entire');
  const qMode = $derived(settings?.quality.mode ?? 'quality');
  const folder = $derived(settings?.dir ?? defaults?.dir ?? '');
  const stem = $derived(estimate ? fileStem(estimate.outPath) : '');
  const nameShown = $derived(nameEdited && settings ? settings.name_template : stem);
  const av1Available = $derived((defaults?.encoders ?? []).some((e) => /av1/i.test(e)));
  const audioTrackCount = $derived.by(() => {
    const project = $snapshot?.project;
    if (!project) return 0;
    const audioIds = new Set(project.tracks.filter((t) => t.kind === 'audio').map((t) => t.id));
    const used = new Set(
      project.clips.filter((c) => audioIds.has(c.track_id)).map((c) => c.track_id),
    );
    return used.size;
  });

  const nameError = $derived(
    estimate && nameShown.trim() === '' ? 'Enter a file name.' : null,
  );
  const rangeError = $derived.by((): string | null => {
    if (!settings) return null;
    const range = settings.range;
    if (range.kind === 'work_area' && !hasWorkArea) return 'There is no work area to export.';
    if (range.kind === 'selected_clips' && range.ids.length === 0) return 'Select clips to export.';
    return null;
  });
  const isVideo = $derived(kindId === 'video');
  const widthError = $derived(isVideo ? sizeError(widthText) : null);
  const heightError = $derived(isVideo ? sizeError(heightText) : null);
  const qualityError = $derived.by((): string | null => {
    if (!isVideo) return null;
    if (qMode === 'bitrate') return mbpsError(bitrateText);
    if (qMode === 'cbr') return mbpsError(cbrText);
    if (qMode === 'target_size') return targetError(targetText);
    return null;
  });
  const stillQualityError = $derived(
    still && still.format === 'jpg'
      ? intError(stillQualityText, 1, 100, 'Use a whole number from 1 to 100.')
      : null,
  );
  /** Backend problems plus the dialog's own checks. Any entry disables Export. */
  const blockers = $derived([
    ...(estimate?.problems ?? []),
    nameError,
    rangeError,
    widthError,
    heightError,
    qualityError,
    stillQualityError,
  ].filter(isText));
  const overFree = $derived(
    Boolean(estimate && defaults && estimate.estimate.bytes > defaults.freeBytes),
  );
  const warnings = $derived(
    [
      estimate?.warning ?? null,
      overFree ? 'Estimated size is larger than the free space on this drive.' : null,
    ].filter(isText),
  );
  const canRun = $derived(
    Boolean(settings) && !loading && !busy && !estimating && blockers.length === 0,
  );
  /** Text under the lossless button: why it is off, or "Checking..." while the engine answers. */
  const losslessNote = $derived.by((): string => {
    if (!smart) return estimateError ? 'Not available: the estimate failed.' : 'Checking...';
    if (smart.eligible) return '';
    return smart.reason ?? 'Not eligible for lossless export.';
  });

  const fpsOptions = $derived.by(() => {
    const opts: { value: string; label: string }[] = [
      { value: 'project', label: canvas ? `Project (${fpsLabel(canvas.fps)} fps)` : 'Project' },
    ];
    for (const n of FPS_CHOICES) opts.push({ value: `${n}/1`, label: String(n) });
    const current = settings?.fps ?? null;
    if (current && !opts.some((o) => o.value === fpsKey(current))) {
      opts.push({ value: fpsKey(current), label: fpsLabel(current) });
    }
    return opts;
  });
  const codecOptions = $derived.by(() => {
    const opts: { value: VideoCodec; label: string }[] = [
      { value: 'h264', label: 'H.264' },
      { value: 'hevc', label: 'HEVC (H.265)' },
    ];
    if (av1Available || settings?.codec === 'av1') opts.push({ value: 'av1', label: 'AV1' });
    return opts;
  });
  const encoderOptions = $derived.by(() => {
    const names = defaults?.encoders ?? [];
    const current = settings?.encoder ?? 'auto';
    const all = names.includes(current) ? names : [...names, current];
    return all.map((v) => ({ value: v, label: encoderLabel(v) }));
  });
  const loudnessOptions = $derived.by(() => {
    const opts = [
      { value: 'off', label: 'Off' },
      { value: '-14', label: '-14 LUFS' },
      { value: '-16', label: '-16 LUFS' },
    ];
    const l = settings?.audio.loudness;
    if (l && l.mode === 'lufs' && !opts.some((o) => o.value === String(l.target))) {
      opts.push({ value: String(l.target), label: `${l.target} LUFS` });
    }
    return opts;
  });

  function isText(value: string | null | undefined): value is string {
    return typeof value === 'string' && value !== '';
  }

  /** Reads the value of the input or select that fired the event. */
  function inputValue(e: Event): string {
    return (e.currentTarget as HTMLInputElement | HTMLSelectElement).value;
  }

  /** Reads the checked state of the checkbox that fired the event. */
  function inputChecked(e: Event): boolean {
    return (e.currentTarget as HTMLInputElement).checked;
  }

  function parseNum(text: string): number | null {
    const t = text.trim();
    if (t === '') return null;
    const n = Number(t);
    return Number.isFinite(n) ? n : null;
  }

  /** Largest even value at or above 2, nearest to `v`. */
  function evenRound(v: number): number {
    return Math.max(2, 2 * Math.round(v / 2));
  }

  function intError(text: string, lo: number, hi: number, message: string): string | null {
    const n = parseNum(text);
    return n !== null && Number.isInteger(n) && n >= lo && n <= hi ? null : message;
  }

  function sizeError(text: string): string | null {
    const n = parseNum(text);
    if (n === null || !Number.isInteger(n)) return 'Enter a whole number of pixels.';
    if (n % 2 !== 0) return 'Use an even number of pixels.';
    if (n < SIZE_MIN || n > SIZE_MAX) return `Use a value from ${SIZE_MIN} to ${SIZE_MAX}.`;
    return null;
  }

  function mbpsError(text: string): string | null {
    const n = parseNum(text);
    if (n === null) return 'Enter a bitrate in Mbps.';
    if (n < MBPS_MIN || n > MBPS_MAX) return `Use a value from ${MBPS_MIN} to ${MBPS_MAX} Mbps.`;
    return null;
  }

  function targetError(text: string): string | null {
    const n = parseNum(text);
    if (n === null) return 'Enter a target size in MB.';
    if (n < TARGET_MB_MIN || n > TARGET_MB_MAX) {
      return `Use a value from ${TARGET_MB_MIN} to ${TARGET_MB_MAX} MB.`;
    }
    return null;
  }

  function kbpsFromMbps(text: string): number {
    return Math.round(Number(text) * 1000);
  }

  function mbpsText(kbps: number): string {
    return String(Math.round(kbps / 100) / 10);
  }

  function fpsKey(r: Rate | null): string {
    return r === null ? 'project' : `${r.num}/${r.den}`;
  }

  function parseFpsKey(v: string): Rate | null {
    if (v === 'project') return null;
    const parts = v.split('/');
    return { num: Number(parts[0]), den: Number(parts[1] ?? 1) };
  }

  function fpsLabel(r: Rate): string {
    return (r.num / r.den).toFixed(3).replace(/\.?0+$/, '');
  }

  function encoderLabel(v: string): string {
    if (v === 'auto') return 'Auto (hardware first)';
    if (v === 'software') return 'Software (CPU)';
    return v;
  }

  function fileStem(path: string): string {
    const name = path.split(/[\\/]/).pop() ?? '';
    const dot = name.lastIndexOf('.');
    return dot > 0 ? name.slice(0, dot) : name;
  }

  function stillOf(s: ExportSettings | null): { format: string; quality: number } | null {
    if (!s || s.kind.kind !== 'still') return null;
    return { format: s.kind.format, quality: s.kind.quality };
  }

  /** Size of a resolution choice for the canvas, or null for 'custom'. */
  function sizeForChoice(id: string): { width: number; height: number } | null {
    const c = canvas;
    if (!c || c.width <= 0 || c.height <= 0) return null;
    if (id === 'canvas') return { width: c.width, height: c.height };
    const choice = RES_CHOICES.find((r) => r.id === id);
    if (!choice) return null;
    if (c.width >= c.height) {
      return { width: choice.long, height: evenRound((choice.long * c.height) / c.width) };
    }
    return { width: evenRound((choice.long * c.width) / c.height), height: choice.long };
  }

  function resChoiceFor(s: ExportSettings): string {
    for (const id of ['canvas', ...RES_CHOICES.map((r) => r.id)]) {
      const size = sizeForChoice(id);
      if (size && size.width === s.width && size.height === s.height) return id;
    }
    return 'custom';
  }

  /** Parses the backend's untyped smart-lossless reply. */
  function parseSmart(v: unknown): SmartInfo {
    if (typeof v !== 'object' || v === null) {
      return { eligible: false, reason: 'No answer from the export engine.', notes: [] };
    }
    const r = v as Record<string, unknown>;
    const reason = typeof r.reason === 'string' && r.reason.trim() !== '' ? r.reason : null;
    // The backend sends camelCase `snapNotes`; `notes` is accepted too.
    const rawNotes: unknown = r.snapNotes ?? r.notes;
    const list: unknown[] = Array.isArray(rawNotes) ? rawNotes : [];
    const notes = list.filter((n): n is string => typeof n === 'string');
    return { eligible: r.eligible === true, reason, notes };
  }

  /** Applies an edit made by hand: the settings no longer match a preset. */
  function editSettings(next: Partial<ExportSettings>): void {
    if (!settings) return;
    settings = { ...settings, ...next, preset: 'custom' };
  }

  function patchAudio(p: Partial<AudioExport>): void {
    if (!settings) return;
    editSettings({ audio: { ...settings.audio, ...p } });
  }

  function patchStill(p: { format?: string; quality?: number }): void {
    if (!settings || settings.kind.kind !== 'still') return;
    settings = { ...settings, kind: { ...settings.kind, ...p } };
  }

  /** Copies the fields the UI shows from settings into the typed text and slider state. */
  function syncFromSettings(s: ExportSettings): void {
    widthText = String(s.width);
    heightText = String(s.height);
    resChoice = resChoiceFor(s);
    const q = s.quality;
    if (q.mode === 'quality') qualitySlider = q.value;
    if (q.mode === 'bitrate') bitrateText = mbpsText(q.kbps);
    if (q.mode === 'cbr') cbrText = mbpsText(q.kbps);
    if (q.mode === 'target_size') targetText = String(q.mb);
    if (s.kind.kind === 'still') stillQualityText = String(s.kind.quality);
  }

  async function loadDialog(): Promise<void> {
    const seq = ++loadSeq;
    loading = true;
    loadError = '';
    errorText = '';
    estimate = null;
    estimateError = '';
    smart = null;
    stillPath = '';
    presetSaving = false;
    presetSaveError = '';
    try {
      const [d, prefs] = await Promise.all([
        editorApi.exportDefaults(),
        editorApi.getSettings().catch((): null => null),
      ]);
      if (seq !== loadSeq) return;
      const start: ExportSettings = { ...d.settings };
      // Audio-only export is not available yet; a stored audio-only start falls back to video.
      if (start.kind.kind === 'audio_only') start.kind = { kind: 'video' };
      // The last folder chosen in this dialog wins over the project default.
      if (start.dir === null && prefs && prefs.lastExportDir !== '') {
        start.dir = prefs.lastExportDir;
      }
      defaults = d;
      userPresets = d.presets.filter((p) => !p.builtin);
      nameEdited = false;
      bitrateText = String(DEFAULT_MBPS);
      cbrText = String(DEFAULT_MBPS);
      targetText = String(DEFAULT_TARGET_MB);
      stillQualityText = String(DEFAULT_STILL_QUALITY);
      const c = canvas;
      aspect = c && c.height > 0 ? c.width / c.height : 16 / 9;
      lockAspect = true;
      settings = start;
      syncFromSettings(start);
      loading = false;
      await tick();
      presetSelect?.focus();
    } catch (e) {
      if (seq === loadSeq) loadError = String(e);
    } finally {
      if (seq === loadSeq) loading = false;
    }
  }

  async function runEstimate(s: ExportSettings, seq: number): Promise<void> {
    try {
      const smartPromise: Promise<unknown> =
        s.kind.kind === 'video'
          ? editorApi
              .checkSmartExport(s)
              .catch((e: unknown) => ({ eligible: false, reason: String(e), notes: [] }))
          : Promise.resolve(null);
      const [reply, smartRaw] = await Promise.all([editorApi.estimateExport(s), smartPromise]);
      if (seq !== estimateSeq) return;
      estimate = reply;
      estimateError = '';
      smart = s.kind.kind === 'video' ? parseSmart(smartRaw) : null;
    } catch (e) {
      if (seq !== estimateSeq) return;
      estimate = null;
      smart = null;
      estimateError = String(e);
    } finally {
      if (seq === estimateSeq) estimating = false;
    }
  }

  // Live estimate: re-run 300 ms after the last settings change. Replaced objects trigger the effect.
  $effect(() => {
    const s = settings;
    if (!open || loading || !s) return;
    const seq = ++estimateSeq;
    estimating = true;
    const timer = setTimeout(() => void runEstimate(s, seq), ESTIMATE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  });

  // Open: load the defaults and remember what had focus. Close: give focus back.
  $effect(() => {
    if (open) {
      untrack(() => {
        returnFocus =
          document.activeElement instanceof HTMLElement ? document.activeElement : null;
        void loadDialog();
      });
    } else if (returnFocus) {
      const target = returnFocus;
      returnFocus = null;
      target.focus();
    }
  });

  function close(): void {
    if (!busy) onclose();
  }

  function onBackdropClick(e: MouseEvent): void {
    if (e.target === e.currentTarget) close();
  }

  /** Esc closes; Tab stays inside the dialog. */
  function onKeydown(event: KeyboardEvent): void {
    if (!open) return;
    if (event.key === 'Escape') {
      if (busy) return;
      event.preventDefault();
      onclose();
      return;
    }
    if (event.key === 'Tab' && dialogEl) trapFocus(event, dialogEl);
  }

  function trapFocus(event: KeyboardEvent, root: HTMLElement): void {
    const items = Array.from(
      root.querySelectorAll<HTMLElement>(
        'button:not(:disabled), input:not(:disabled), select:not(:disabled), details > summary',
      ),
    );
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) return;
    const active = document.activeElement;
    const inside = active instanceof Node && root.contains(active);
    if (event.shiftKey && (active === first || !inside)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (active === last || !inside)) {
      event.preventDefault();
      first.focus();
    }
  }

  function applyPreset(id: string): void {
    if (!settings) return;
    if (id === 'custom') {
      settings = { ...settings, preset: 'custom' };
      return;
    }
    const p = allPresets.find((x) => x.id === id);
    const c = canvas;
    if (!p || !c) return;
    // Name, location, range and encoder stay as the user chose them; the preset supplies the rest.
    const size = p.fixed_size
      ? fitBox(c.width, c.height, p.settings.width, p.settings.height)
      : { width: c.width, height: c.height };
    const next: ExportSettings = {
      ...p.settings,
      preset: p.id,
      name_template: settings.name_template,
      dir: settings.dir,
      range: settings.range,
      encoder: settings.encoder,
      kind: p.settings.kind,
      lossless: false,
      width: size.width,
      height: size.height,
    };
    settings = next;
    aspect = next.height > 0 ? next.width / next.height : aspect;
    syncFromSettings(next);
  }

  /**
   * Fits a canvas inside a bounding box, keeping its aspect. Mirrors the backend `fit_into_box`:
   * never scaled up, sides rounded to the nearest pixel then down to even, at least SIZE_MIN.
   */
  function fitBox(cw: number, ch: number, bw: number, bh: number): { width: number; height: number } {
    if (cw <= 0 || ch <= 0) return { width: bw, height: bh };
    const scale = Math.min(1, bw / cw, bh / ch);
    const even = (v: number): number => Math.max(SIZE_MIN, 2 * Math.floor(Math.round(v) / 2));
    return { width: even(cw * scale), height: even(ch * scale) };
  }

  function presetSlug(label: string): string {
    return label
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '');
  }

  async function savePresetNow(): Promise<void> {
    const current = settings;
    if (!current) return;
    const label = presetLabel.trim();
    if (label === '') {
      presetSaveError = 'Enter a name for the preset.';
      return;
    }
    // A name with no Latin letters or digits gets a timestamp id.
    let id = presetSlug(label);
    if (id === '') id = `user_${Date.now()}`;
    else if (id === 'custom' || builtinPresets.some((p) => p.id === id)) id = `user_${id}`;
    // Saving over a user preset needs an inline confirmation first (the second click repeats the save).
    if (userPresets.some((p) => !p.builtin && p.id === id) && replaceId !== id) {
      replaceId = id;
      presetSaveError = '';
      return;
    }
    replaceId = null;
    presetSaveError = '';
    try {
      const saved = await editorApi.savePreset({
        id,
        label,
        settings: { ...current, preset: id, dir: null, range: { kind: 'entire' } },
        fixed_size: true,
        max_mb: null,
        builtin: false,
      });
      userPresets = saved;
      if (settings) settings = { ...settings, preset: id };
      presetSaving = false;
      presetLabel = '';
      replaceId = null;
    } catch (e) {
      presetSaveError = String(e);
    }
  }

  async function deleteSelectedPreset(): Promise<void> {
    const p = selectedPreset;
    if (!p || p.builtin) return;
    errorText = '';
    try {
      userPresets = await editorApi.deletePreset(p.id);
      if (settings) settings = { ...settings, preset: 'custom' };
    } catch (e) {
      errorText = String(e);
    }
  }

  function onNameInput(text: string): void {
    if (!settings) return;
    nameEdited = true;
    settings = { ...settings, name_template: text };
  }

  /** Editing the template shows the expanded template again instead of a typed name. */
  function onTemplateInput(text: string): void {
    if (!settings) return;
    nameEdited = false;
    settings = { ...settings, name_template: text };
  }

  async function rememberFolder(dir: string): Promise<void> {
    try {
      const current: EditorSettings = await editorApi.getSettings();
      await editorApi.setSettings({ ...current, lastExportDir: dir });
    } catch {
      // Remembering the folder is a convenience; the export still uses the chosen folder.
    }
  }

  async function changeFolder(): Promise<void> {
    errorText = '';
    try {
      const picked = await invoke<string | null>('pick_folder', {
        initial: folder === '' ? null : folder,
      });
      if (picked === null || !settings) return;
      settings = { ...settings, dir: picked };
      await rememberFolder(picked);
    } catch (e) {
      errorText = `Could not open the folder picker: ${String(e)}`;
    }
  }

  function resetFolder(): void {
    if (!settings) return;
    settings = { ...settings, dir: null };
    void rememberFolder('');
  }

  function setRange(kind: 'entire' | 'work_area' | 'selected'): void {
    if (!settings) return;
    const range: ExportSettings['range'] =
      kind === 'entire'
        ? { kind: 'entire' }
        : kind === 'work_area'
          ? { kind: 'work_area' }
          : { kind: 'selected_clips', ids: [...selectedIds] };
    settings = { ...settings, range };
  }

  // While "Selected clips" is the range, follow the timeline selection as it changes.
  $effect(() => {
    const ids = selectedIds;
    const s = untrack(() => settings);
    if (!s || s.range.kind !== 'selected_clips') return;
    const same =
      s.range.ids.length === ids.length && s.range.ids.every((id, i) => id === ids[i]);
    if (same) return;
    untrack(() => {
      settings = { ...s, range: { kind: 'selected_clips', ids: [...ids] } };
    });
  });

  function onResChoice(id: string): void {
    resChoice = id;
    const size = id === 'custom' ? null : sizeForChoice(id);
    if (!size) return;
    widthText = String(size.width);
    heightText = String(size.height);
    editSettings({ width: size.width, height: size.height });
  }

  function onWidthInput(text: string): void {
    widthText = text;
    resChoice = 'custom';
    const w = parseNum(text);
    if (w === null || sizeError(text) !== null) return;
    if (lockAspect) {
      const h = evenRound(w / aspect);
      heightText = String(h);
      editSettings({ width: w, height: h });
    } else {
      editSettings({ width: w });
    }
  }

  function onHeightInput(text: string): void {
    heightText = text;
    resChoice = 'custom';
    const h = parseNum(text);
    if (h === null || sizeError(text) !== null) return;
    if (lockAspect) {
      const w = evenRound(h * aspect);
      widthText = String(w);
      editSettings({ width: w, height: h });
    } else {
      editSettings({ height: h });
    }
  }

  function onLockChange(checked: boolean): void {
    lockAspect = checked;
    if (checked && settings && settings.height > 0) {
      aspect = settings.width / settings.height;
    }
  }

  function kbpsOr(text: string): number {
    return mbpsError(text) === null ? kbpsFromMbps(text) : DEFAULT_MBPS * 1000;
  }

  function targetOr(): number {
    return targetError(targetText) === null ? Number(targetText) : DEFAULT_TARGET_MB;
  }

  function setQualityMode(mode: QualityMode['mode']): void {
    switch (mode) {
      case 'quality':
        editSettings({ quality: { mode: 'quality', value: qualitySlider } });
        break;
      case 'bitrate':
        editSettings({ quality: { mode: 'bitrate', kbps: kbpsOr(bitrateText) } });
        break;
      case 'cbr':
        editSettings({ quality: { mode: 'cbr', kbps: kbpsOr(cbrText) } });
        break;
      case 'target_size':
        editSettings({ quality: { mode: 'target_size', mb: targetOr() } });
        break;
    }
  }

  function onQualitySlider(text: string): void {
    const n = Math.round(Number(text));
    qualitySlider = n;
    editSettings({ quality: { mode: 'quality', value: n } });
  }

  function onBitrateInput(text: string): void {
    bitrateText = text;
    if (mbpsError(text) === null) {
      editSettings({ quality: { mode: 'bitrate', kbps: kbpsFromMbps(text) } });
    }
  }

  function onCbrInput(text: string): void {
    cbrText = text;
    if (mbpsError(text) === null) {
      editSettings({ quality: { mode: 'cbr', kbps: kbpsFromMbps(text) } });
    }
  }

  function onTargetInput(text: string): void {
    targetText = text;
    if (targetError(text) === null) {
      editSettings({ quality: { mode: 'target_size', mb: Number(text) } });
    }
  }

  function setKind(id: ExportKind['kind']): void {
    if (!settings) return;
    errorText = '';
    stillPath = '';
    if (id === 'video') {
      settings = { ...settings, kind: { kind: 'video' } };
    } else if (id === 'audio_only') {
      settings = { ...settings, kind: { kind: 'audio_only' } };
    } else {
      settings = {
        ...settings,
        kind: {
          kind: 'still',
          format: still?.format ?? 'png',
          quality: still?.quality ?? DEFAULT_STILL_QUALITY,
          at: $playhead,
        },
      };
    }
  }

  function onStillQuality(text: string): void {
    stillQualityText = text;
    if (intError(text, 1, 100, '') === null) patchStill({ quality: Math.round(Number(text)) });
  }

  /**
   * When the user always wants to choose the location, asks for a folder first and returns the
   * settings with that folder. Null when the picker was cancelled.
   */
  async function withLocation(s: ExportSettings): Promise<ExportSettings | null> {
    if (!defaults?.alwaysAskLocation) return s;
    const picked = await invoke<string | null>('pick_folder', {
      initial: folder === '' ? null : folder,
    });
    if (picked === null) return null;
    const next = { ...s, dir: picked };
    settings = next;
    await rememberFolder(picked);
    return next;
  }

  /** Starts the export as a queued job, with or without smart lossless. Closes on success. */
  async function startExportJob(lossless: boolean): Promise<void> {
    if (!settings || !canRun) return;
    errorText = '';
    let target: ExportSettings | null;
    try {
      target = await withLocation(settings);
    } catch (e) {
      errorText = `Could not open the folder picker: ${String(e)}`;
      return;
    }
    if (target === null) return;
    busy = true;
    try {
      await editorApi.exportStart({ ...target, lossless });
      toast.set('Export started');
      onclose();
    } catch (e) {
      errorText = String(e);
    } finally {
      busy = false;
    }
  }

  /** Saves the still frame under the playhead right away (not queued). */
  async function saveStill(): Promise<void> {
    const s = still;
    if (!s || busy || stillQualityError !== null || !settings) return;
    errorText = '';
    stillPath = '';
    let target: ExportSettings | null;
    try {
      target = await withLocation(settings);
    } catch (e) {
      errorText = `Could not open the folder picker: ${String(e)}`;
      return;
    }
    if (target === null) return;
    busy = true;
    try {
      const nameTemplate = target.name_template.trim() === '' ? null : target.name_template;
      stillPath = await editorApi.exportStill(
        $playhead,
        s.format,
        s.quality,
        target.dir ?? null,
        nameTemplate,
      );
    } catch (e) {
      errorText = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <div class="backdrop" role="presentation" onclick={onBackdropClick}>
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="export-dialog-title"
      bind:this={dialogEl}
    >
      <header class="head">
        <h2 id="export-dialog-title" class="title">Export</h2>
        <button
          type="button"
          class="icon"
          aria-label="Close export dialog"
          disabled={busy}
          onclick={close}
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <path d="M3 3l10 10M13 3L3 13" stroke="currentColor" stroke-width="1.8" fill="none" />
          </svg>
        </button>
      </header>

      {#if loading}
        <p class="status" role="status">Loading export settings...</p>
      {:else if loadError}
        <p class="field-error" role="alert">Could not load export settings. {loadError}</p>
      {:else if settings}
        <div class="body">
          <div class="kind-tabs" role="group" aria-label="What to export">
            {#each KIND_TABS as tab (tab.id)}
              <button
                type="button"
                class="tab"
                aria-pressed={kindId === tab.id}
                onclick={() => setKind(tab.id)}
              >
                {tab.label}
              </button>
            {/each}
          </div>

          <div class="row">
            <label class="lbl" for="exp-preset">Preset</label>
            <select
              id="exp-preset"
              class="grow"
              bind:this={presetSelect}
              value={presetId}
              onchange={(e) => applyPreset(inputValue(e))}
            >
              <option value="custom">Custom</option>
              <optgroup label="Built-in">
                {#each builtinPresets as p (p.id)}
                  <option value={p.id}>{p.label}</option>
                {/each}
              </optgroup>
              {#if shownUserPresets.length > 0}
                <optgroup label="Your presets">
                  {#each shownUserPresets as p (p.id)}
                    <option value={p.id}>{p.label}</option>
                  {/each}
                </optgroup>
              {/if}
            </select>
            <button
              type="button"
              class="secondary"
              aria-expanded={presetSaving}
              onclick={() => (presetSaving = !presetSaving)}
            >
              Save as preset...
            </button>
            {#if selectedPreset && !selectedPreset.builtin}
              <button type="button" class="secondary" onclick={() => void deleteSelectedPreset()}>
                Delete preset
              </button>
            {/if}
          </div>

          {#if presetSaving}
            <div class="row">
              <label class="lbl" for="exp-preset-name">Preset name</label>
              <input
                id="exp-preset-name"
                type="text"
                class="grow"
                maxlength="60"
                value={presetLabel}
                aria-describedby={presetSaveError ? 'exp-preset-err' : undefined}
                oninput={(e) => {
                  presetLabel = inputValue(e);
                  replaceId = null;
                }}
              />
              <button type="button" class="primary" onclick={() => void savePresetNow()}>
                Save
              </button>
            </div>
            {#if replaceId}
              <div class="row">
                <span class="field-error">
                  Replace preset {userPresets.find((p) => p.id === replaceId)?.label ?? replaceId}?
                </span>
                <button type="button" class="primary" onclick={() => void savePresetNow()}>
                  Replace
                </button>
                <button type="button" class="secondary" onclick={() => (replaceId = null)}>
                  Keep existing
                </button>
              </div>
            {/if}
            {#if presetSaveError}
              <p id="exp-preset-err" class="field-error">{presetSaveError}</p>
            {/if}
          {/if}

          <div class="row">
            <label class="lbl" for="exp-name">Name</label>
            <input
              id="exp-name"
              type="text"
              class="grow"
              value={nameShown}
              aria-invalid={nameError !== null}
              aria-describedby={nameError ? 'exp-name-err' : undefined}
              oninput={(e) => onNameInput(inputValue(e))}
            />
          </div>
          {#if nameError}
            <p id="exp-name-err" class="field-error">{nameError}</p>
          {/if}
          <details class="advanced">
            <summary>Advanced: file name template</summary>
            <div class="row">
              <label class="lbl" for="exp-template">Template</label>
              <input
                id="exp-template"
                type="text"
                class="grow mono"
                value={settings.name_template}
                oninput={(e) => onTemplateInput(inputValue(e))}
              />
            </div>
            <p class="hint">
              Tokens: {'{project}'} {'{date}'} {'{time}'} {'{yyyy-MM-dd HH-mm-ss}'} {'{res}'}
              {'{preset}'} {'{n}'}. Default: {NAME_TEMPLATE_DEFAULT}
            </p>
          </details>

          <div class="row">
            <span class="lbl">Location</span>
            <span class="folder" title={folder}>{folder}</span>
            <button type="button" class="secondary" onclick={() => void changeFolder()}>
              Change...
            </button>
            {#if settings.dir !== null}
              <button type="button" class="link" onclick={resetFolder}>Reset to default</button>
            {/if}
          </div>

          <fieldset class="row radios">
            <legend class="lbl">Range</legend>
            <label>
              <input
                type="radio"
                name="exp-range"
                checked={rangeKind === 'entire'}
                onchange={() => setRange('entire')}
              />
              Entire timeline
            </label>
            <label>
              <input
                type="radio"
                name="exp-range"
                checked={rangeKind === 'work_area'}
                disabled={!hasWorkArea}
                onchange={() => setRange('work_area')}
              />
              Work area
            </label>
            <label>
              <input
                type="radio"
                name="exp-range"
                checked={rangeKind === 'selected_clips'}
                disabled={selectedIds.length === 0}
                onchange={() => setRange('selected')}
              />
              Selected clips
            </label>
          </fieldset>
          {#if rangeError}
            <p class="field-error">{rangeError}</p>
          {/if}

          {#if isVideo}
            <h3 class="section">Video</h3>
            <div class="row">
              <label class="lbl" for="exp-res">Resolution</label>
              <select id="exp-res" value={resChoice} onchange={(e) => onResChoice(inputValue(e))}>
                <option value="canvas">Match canvas</option>
                {#each RES_CHOICES as r (r.id)}
                  <option value={r.id}>{r.label}</option>
                {/each}
                <option value="custom">Custom</option>
              </select>
              <label for="exp-w">W</label>
              <input
                id="exp-w"
                class="num"
                type="number"
                inputmode="numeric"
                min={SIZE_MIN}
                max={SIZE_MAX}
                step="2"
                value={widthText}
                aria-invalid={widthError !== null}
                aria-describedby={widthError ? 'exp-w-err' : undefined}
                oninput={(e) => onWidthInput(inputValue(e))}
              />
              <label for="exp-h">H</label>
              <input
                id="exp-h"
                class="num"
                type="number"
                inputmode="numeric"
                min={SIZE_MIN}
                max={SIZE_MAX}
                step="2"
                value={heightText}
                aria-invalid={heightError !== null}
                aria-describedby={heightError ? 'exp-h-err' : undefined}
                oninput={(e) => onHeightInput(inputValue(e))}
              />
              <label class="check">
                <input
                  type="checkbox"
                  checked={lockAspect}
                  onchange={(e) => onLockChange(inputChecked(e))}
                />
                Lock aspect
              </label>
              {#if canvas}
                <span class="muted">Canvas: {canvas.width} x {canvas.height} ({canvas.aspect_preset})</span>
              {/if}
            </div>
            {#if widthError}
              <p id="exp-w-err" class="field-error">Width: {widthError}</p>
            {/if}
            {#if heightError}
              <p id="exp-h-err" class="field-error">Height: {heightError}</p>
            {/if}

            <div class="row">
              <label class="lbl" for="exp-fps">Frame rate</label>
              <select
                id="exp-fps"
                value={fpsKey(settings.fps)}
                onchange={(e) => editSettings({ fps: parseFpsKey(inputValue(e)) })}
              >
                {#each fpsOptions as o (o.value)}
                  <option value={o.value}>{o.label}</option>
                {/each}
              </select>
              <label class="lbl-inline" for="exp-codec">Codec</label>
              <select
                id="exp-codec"
                value={settings.codec}
                onchange={(e) => {
                  const picked = codecOptions.find((o) => o.value === inputValue(e));
                  if (picked) editSettings({ codec: picked.value });
                }}
              >
                {#each codecOptions as o (o.value)}
                  <option value={o.value}>{o.label}</option>
                {/each}
              </select>
            </div>
            <div class="row">
              <label class="lbl" for="exp-encoder">Encoder</label>
              <select
                id="exp-encoder"
                class="grow"
                value={settings.encoder}
                onchange={(e) => editSettings({ encoder: inputValue(e) })}
              >
                {#each encoderOptions as o (o.value)}
                  <option value={o.value}>{o.label}</option>
                {/each}
              </select>
            </div>

            <fieldset class="quality">
              <legend class="lbl">Quality</legend>
              <div class="q-row">
                <label>
                  <input
                    type="radio"
                    name="exp-quality"
                    checked={qMode === 'quality'}
                    onchange={() => setQualityMode('quality')}
                  />
                  Quality
                </label>
                <input
                  type="range"
                  min="1"
                  max="100"
                  step="1"
                  value={qualitySlider}
                  disabled={qMode !== 'quality'}
                  aria-label="Quality slider, 1 to 100"
                  oninput={(e) => onQualitySlider(inputValue(e))}
                />
                <output class="q-value">{qualitySlider}</output>
              </div>
              <div class="q-row">
                <label>
                  <input
                    type="radio"
                    name="exp-quality"
                    checked={qMode === 'bitrate'}
                    onchange={() => setQualityMode('bitrate')}
                  />
                  Bitrate
                </label>
                <input
                  id="exp-bitrate"
                  class="num"
                  type="number"
                  inputmode="decimal"
                  min={MBPS_MIN}
                  max={MBPS_MAX}
                  step="0.1"
                  value={bitrateText}
                  disabled={qMode !== 'bitrate'}
                  aria-label="Average bitrate in Mbps"
                  aria-invalid={qMode === 'bitrate' && qualityError !== null}
                  oninput={(e) => onBitrateInput(inputValue(e))}
                />
                <span class="muted">Mbps</span>
              </div>
              <div class="q-row">
                <label>
                  <input
                    type="radio"
                    name="exp-quality"
                    checked={qMode === 'cbr'}
                    onchange={() => setQualityMode('cbr')}
                  />
                  Constant bitrate
                </label>
                <input
                  id="exp-cbr"
                  class="num"
                  type="number"
                  inputmode="decimal"
                  min={MBPS_MIN}
                  max={MBPS_MAX}
                  step="0.1"
                  value={cbrText}
                  disabled={qMode !== 'cbr'}
                  aria-label="Constant bitrate in Mbps"
                  aria-invalid={qMode === 'cbr' && qualityError !== null}
                  oninput={(e) => onCbrInput(inputValue(e))}
                />
                <span class="muted">Mbps</span>
              </div>
              <div class="q-row">
                <label>
                  <input
                    type="radio"
                    name="exp-quality"
                    checked={qMode === 'target_size'}
                    onchange={() => setQualityMode('target_size')}
                  />
                  Target size
                </label>
                <input
                  id="exp-target"
                  class="num"
                  type="number"
                  inputmode="decimal"
                  min={TARGET_MB_MIN}
                  max={TARGET_MB_MAX}
                  step="1"
                  value={targetText}
                  disabled={qMode !== 'target_size'}
                  aria-label="Target file size in MB"
                  aria-invalid={qMode === 'target_size' && qualityError !== null}
                  oninput={(e) => onTargetInput(inputValue(e))}
                />
                <span class="muted">MB</span>
              </div>
              {#if qualityError}
                <p class="field-error">{qualityError}</p>
              {/if}
            </fieldset>
          {/if}

          {#if kindId !== 'still'}
            <h3 class="section">Audio</h3>
            <fieldset class="row radios">
              <legend class="lbl">Tracks</legend>
              <label>
                <input
                  type="radio"
                  name="exp-layout"
                  checked={settings.audio.layout === 'mix'}
                  onchange={() => patchAudio({ layout: 'mix' })}
                />
                Mix to stereo
              </label>
              <label>
                <input
                  type="radio"
                  name="exp-layout"
                  checked={settings.audio.layout === 'separate'}
                  onchange={() => patchAudio({ layout: 'separate' })}
                />
                Keep {audioTrackCount} separate {audioTrackCount === 1 ? 'track' : 'tracks'}
              </label>
            </fieldset>
            <div class="row">
              <label class="lbl" for="exp-abr">Bitrate</label>
              <select
                id="exp-abr"
                value={String(settings.audio.bitrate_kbps)}
                onchange={(e) => patchAudio({ bitrate_kbps: Number(inputValue(e)) })}
              >
                {#each AUDIO_BITRATES as b (b)}
                  <option value={String(b)}>{b} kbps</option>
                {/each}
              </select>
              <label class="lbl-inline" for="exp-rate">Sample rate</label>
              <select
                id="exp-rate"
                value={String(settings.audio.sample_rate)}
                onchange={(e) => patchAudio({ sample_rate: Number(inputValue(e)) })}
              >
                {#each SAMPLE_RATES as r (r.value)}
                  <option value={String(r.value)}>{r.label}</option>
                {/each}
              </select>
            </div>
            <div class="row">
              <label class="lbl" for="exp-loudness">Loudness</label>
              <select
                id="exp-loudness"
                value={settings.audio.loudness.mode === 'off'
                  ? 'off'
                  : String(settings.audio.loudness.target)}
                onchange={(e) => {
                  const v = inputValue(e);
                  patchAudio({
                    loudness: v === 'off' ? { mode: 'off' } : { mode: 'lufs', target: Number(v) },
                  });
                }}
              >
                {#each loudnessOptions as o (o.value)}
                  <option value={o.value}>{o.label}</option>
                {/each}
              </select>
              <label class="check">
                <input
                  type="checkbox"
                  checked={settings.audio.limiter}
                  onchange={(e) => patchAudio({ limiter: inputChecked(e) })}
                />
                Peak limiter
              </label>
            </div>
          {/if}

          {#if still}
            <h3 class="section">Still frame</h3>
            <p class="hint">Saves the frame under the playhead at {formatDuration($playhead)}.</p>
            <div class="row">
              <label class="lbl" for="exp-still-format">Format</label>
              <select
                id="exp-still-format"
                value={still.format}
                onchange={(e) => patchStill({ format: inputValue(e) })}
              >
                <option value="png">PNG</option>
                <option value="jpg">JPG</option>
              </select>
              {#if still.format === 'jpg'}
                <label class="lbl-inline" for="exp-still-quality">JPG quality</label>
                <input
                  id="exp-still-quality"
                  class="num"
                  type="number"
                  inputmode="numeric"
                  min="1"
                  max="100"
                  step="1"
                  value={stillQualityText}
                  aria-invalid={stillQualityError !== null}
                  oninput={(e) => onStillQuality(inputValue(e))}
                />
              {/if}
            </div>
            {#if stillQualityError}
              <p class="field-error">{stillQualityError}</p>
            {/if}
          {/if}
        </div>

        <footer class="foot">
          <div class="summary" aria-live="polite">
            {#if estimate}
              <p class="stats">
                <span>Estimated size ~ {formatBytes(estimate.estimate.bytes)}</span>
                <span>Duration {formatDuration(secToTick(estimate.durationS))}</span>
                <span
                  >Free space {formatBytes(defaults?.freeBytes ?? 0)}{settings.dir !== null
                    ? ' (default folder)'
                    : ''}</span
                >
                {#if estimating}<span class="muted">Updating...</span>{/if}
              </p>
              <p class="path" title={estimate.outPath}>Saves to {estimate.outPath}</p>
            {:else if estimateError}
              <p class="field-error">Could not estimate the export: {estimateError}</p>
            {:else if estimating}
              <p class="muted">Estimating...</p>
            {/if}

            {#if estimate}
              {#each estimate.problems as problem, i (i)}
                <p class="field-error">{problem}</p>
              {/each}
            {/if}
            {#each warnings as w, i (i)}
              <p class="warn-chip" role="note">{w}</p>
            {/each}

            {#if smart?.eligible && smart.notes.length > 0}
              <ul class="notes">
                {#each smart.notes as n, i (i)}
                  <li>{n}</li>
                {/each}
              </ul>
            {/if}

            {#if errorText}
              <p class="field-error" role="alert">{errorText}</p>
            {/if}
            {#if stillPath}
              <p class="ok" role="status">Still frame saved to {stillPath}</p>
            {/if}
          </div>

          <div class="actions">
            <button type="button" class="secondary" disabled={busy} onclick={close}>Cancel</button>
            {#if isVideo}
              <div class="lossless">
                <button
                  type="button"
                  class="secondary"
                  disabled={!smart?.eligible || !canRun}
                  title={losslessNote}
                  onclick={() => void startExportJob(true)}
                >
                  Export without re-encoding
                </button>
                {#if losslessNote !== ''}
                  <p class="muted lossless-note">{losslessNote}</p>
                {/if}
              </div>
            {/if}
            {#if still}
              <button
                type="button"
                class="primary"
                disabled={busy || stillQualityError !== null}
                onclick={() => void saveStill()}
              >
                Save still frame
              </button>
            {:else}
              <button
                type="button"
                class="primary"
                disabled={!canRun}
                onclick={() => void startExportJob(false)}
              >
                Export
              </button>
            {/if}
          </div>
        </footer>
      {/if}
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.6);
  }

  .dialog {
    --warn: #e7b730;
    width: 100%;
    max-width: 680px;
    max-height: 90vh;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 20px 10px;
    border-bottom: 1px solid var(--border);
  }

  .title {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }

  .body {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 12px 20px;
    min-height: 0;
  }

  .status {
    margin: 0;
    padding: 24px 20px;
    color: var(--text-dim);
  }

  .kind-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 12px;
  }

  .tab {
    background: var(--surface-2);
    color: var(--text-dim);
    border: 1px solid var(--border);
  }

  .tab[aria-pressed='true'] {
    color: var(--text);
    border-color: var(--accent);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin: 0 0 10px;
    border: 0;
    padding: 0;
    min-width: 0;
  }

  .lbl {
    min-width: 84px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .lbl-inline {
    font-size: 13px;
    color: var(--text-dim);
  }

  .section {
    margin: 16px 0 8px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  .radios legend {
    float: left;
    width: 100%;
    padding: 0;
  }

  .radios label,
  .quality label,
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
  }

  .quality {
    margin: 0 0 10px;
    padding: 0;
    border: 0;
  }

  .quality legend {
    margin-bottom: 6px;
  }

  .q-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }

  .q-row > label {
    min-width: 150px;
  }

  .q-row input[type='range'] {
    flex: 1 1 200px;
  }

  .q-value {
    min-width: 2.5em;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  input[type='text'],
  input[type='number'],
  select {
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text);
  }

  input[type='number'].num {
    width: 92px;
  }

  input[type='text'].grow,
  select.grow {
    flex: 1 1 200px;
    min-width: 0;
  }

  input[type='text'].mono {
    font-family: ui-monospace, Consolas, monospace;
    font-size: 12px;
  }

  input:disabled,
  select:disabled {
    opacity: 0.5;
  }

  input[aria-invalid='true'] {
    border-color: var(--danger);
  }

  .advanced {
    margin: -4px 0 10px 92px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .advanced summary {
    cursor: pointer;
    margin-bottom: 6px;
  }

  .hint {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--text-dim);
    line-height: 1.5;
  }

  .folder {
    flex: 1 1 200px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    font-family: ui-monospace, Consolas, monospace;
  }

  .muted {
    font-size: 12px;
    color: var(--text-dim);
  }

  .field-error {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--danger);
  }

  .warn-chip {
    display: inline-block;
    margin: 0 6px 6px 0;
    padding: 3px 8px;
    border: 1px solid var(--warn);
    border-radius: 999px;
    font-size: 12px;
    color: var(--warn);
  }

  .ok {
    margin: 4px 0 0;
    font-size: 13px;
    color: var(--text);
    word-break: break-all;
  }

  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border);
  }

  .summary {
    flex: 1 1 260px;
    min-width: 0;
  }

  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    margin: 0 0 4px;
    font-size: 13px;
  }

  .path {
    margin: 0 0 8px;
    font-size: 12px;
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .notes {
    margin: 4px 0 8px;
    padding-left: 18px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    align-items: flex-end;
    gap: 8px;
  }

  .lossless {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }

  .lossless-note {
    margin: 0;
    max-width: 260px;
    text-align: right;
  }

  button {
    padding: 8px 14px;
    border-radius: var(--radius);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  button:focus-visible,
  input:focus-visible,
  select:focus-visible,
  summary:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .secondary {
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
  }

  .secondary:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .primary {
    background: var(--accent);
    color: #fff;
    border: 1px solid var(--accent);
  }

  .primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  .link {
    padding: 4px 2px;
    background: none;
    border: 0;
    color: var(--text-dim);
    text-decoration: underline;
  }

  .icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    padding: 0;
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-dim);
  }

  .icon:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border);
  }

  @media (max-width: 520px) {
    .dialog {
      max-height: 94vh;
    }

    .advanced {
      margin-left: 0;
    }

    .lbl {
      min-width: 100%;
    }
  }
</style>
