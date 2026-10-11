/**
 * Pure helpers for the Settings screen: patch diffing, path access, diagnostics mapping,
 * number and list input rules, and the filename example. No Svelte, no I/O.
 */
import type { DiagnosticDto } from '../daemon';

export type PlainObject = Record<string, unknown>;

export function isPlainObject(value: unknown): value is PlainObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

/** Structural equality for JSON-like values. */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    return a.every((item, i) => deepEqual(item, b[i]));
  }
  if (isPlainObject(a)) {
    if (!isPlainObject(b)) return false;
    const keysA = Object.keys(a);
    if (keysA.length !== Object.keys(b).length) return false;
    return keysA.every((k) => hasOwn(b, k) && deepEqual(a[k], b[k]));
  }
  return false;
}

/** Deep copy of JSON data (config values are JSON by construction). */
export function cloneJson<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** Reads a dotted path such as `general.close_behavior`. Missing segments give `undefined`. */
export function getPath(obj: unknown, path: string): unknown {
  let cur: unknown = obj;
  for (const seg of path.split('.')) {
    if (!isPlainObject(cur)) return undefined;
    cur = cur[seg];
  }
  return cur;
}

/** Immutable deep set of a dotted path. Returns a new object; the input is never mutated. */
export function setPath<T extends PlainObject>(obj: T, path: string, value: unknown): T {
  const dot = path.indexOf('.');
  if (dot < 0) return { ...obj, [path]: value };
  const head = path.slice(0, dot);
  const rest = path.slice(dot + 1);
  const child = isPlainObject(obj[head]) ? (obj[head] as PlainObject) : {};
  return { ...obj, [head]: setPath(child, rest, value) } as T;
}

/**
 * Minimal patch that turns `saved` into `draft`, containing only the changed leaves.
 * Objects recurse. Arrays and scalars are replaced whole. A key that disappeared from
 * the draft becomes `null`, which the daemon reads as "clear" (or "remove a games entry").
 */
export function diffPatch(saved: unknown, draft: unknown): PlainObject {
  if (!isPlainObject(saved) || !isPlainObject(draft)) return {};
  const patch: PlainObject = {};
  const keys = new Set([...Object.keys(saved), ...Object.keys(draft)]);
  for (const key of keys) {
    const inSaved = hasOwn(saved, key);
    const inDraft = hasOwn(draft, key);
    if (!inDraft) {
      patch[key] = null;
      continue;
    }
    if (!inSaved) {
      patch[key] = draft[key];
      continue;
    }
    const s = saved[key];
    const d = draft[key];
    if (deepEqual(s, d)) continue;
    if (isPlainObject(s) && isPlainObject(d)) {
      const sub = diffPatch(s, d);
      if (Object.keys(sub).length > 0) patch[key] = sub;
    } else {
      patch[key] = d;
    }
  }
  return patch;
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

export interface FieldDiagnostic {
  code: string;
  severity: 'error' | 'warning';
  message: string;
}

/**
 * Shared rule for both lookups below: the first error wins, and otherwise the first warning.
 * `matches` decides which finding paths belong to the field.
 */
function pickDiagnostic(diagnostics: DiagnosticDto[], matches: (path: string) => boolean): FieldDiagnostic | null {
  let found: FieldDiagnostic | null = null;
  for (const d of diagnostics) {
    if (!matches(d.path)) continue;
    if (d.severity === 'error') return { code: d.code, severity: 'error', message: d.message };
    found ??= { code: d.code, severity: 'warning', message: d.message };
  }
  return found;
}

/**
 * The message for a field. Matches the field's own path and paths under it, such as
 * `detection.allow[1]` for `detection.allow`. An error beats a warning.
 */
export function fieldDiagnostic(path: string, diagnostics: DiagnosticDto[]): FieldDiagnostic | null {
  return pickDiagnostic(
    diagnostics,
    (p) => p === path || p.startsWith(`${path}.`) || p.startsWith(`${path}[`),
  );
}

/** True when any finding is an error (warnings never block a save). */
export function hasBlockingErrors(diagnostics: DiagnosticDto[]): boolean {
  return diagnostics.some((d) => d.severity === 'error');
}

/**
 * Paths whose change is NextPart in the daemon's `apply_class` (validate.rs): it waits until the
 * current game ends. Keep this in step with that function. `video.container` is included there,
 * though the UI does not expose it. Per-game folders are the only NextPart game key.
 */
const NEXT_PART_PATHS = new Set([
  'storage.root',
  'storage.per_game_folders',
  'storage.filename_template',
  'video.container',
]);
const NEXT_PART_GAME_FOLDER = /^games\..+\.folder$/;

/**
 * True when a change to this path waits for the current game to end (apply class NextPart).
 * Restart-class keys (`advanced.*`) and Live or PipelineReinit keys do not wait for a game.
 */
export function appliesAfterGameEnds(path: string): boolean {
  return NEXT_PART_PATHS.has(path) || NEXT_PART_GAME_FOLDER.test(path);
}

/**
 * Length of a name in Unicode characters (code points), the way the daemon counts it with
 * `chars().count()`. `.length` counts UTF-16 units, so astral characters such as emoji count 2.
 */
export function charCount(text: string): number {
  return Array.from(text).length;
}

export const APPLIED_NOTICE = 'Applied';
export const DEFERRED_NOTICE = 'Will apply when recording stops';

/** The notice shown after a successful save. */
export function applyNotice(restartRequired: boolean): string {
  return restartRequired ? DEFERRED_NOTICE : APPLIED_NOTICE;
}

// ---------------------------------------------------------------------------
// Number input
// ---------------------------------------------------------------------------

export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** Parses typed text. Blank or non-numeric text gives `null`. */
export function parseNumber(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === '') return null;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : null;
}

/**
 * The value a number field commits on blur: parsed, rounded when `integer`, then clamped.
 * `null` means the text was not a number, so the field should revert.
 */
export function commitNumber(text: string, min: number, max: number, integer: boolean): number | null {
  const n = parseNumber(text);
  if (n === null) return null;
  return clamp(integer ? Math.round(n) : n, min, max);
}

// ---------------------------------------------------------------------------
// Filename example
// ---------------------------------------------------------------------------

/** Sample values for the example. The recorder fills the real ones at save time. */
export const EXAMPLE_PARTS = { game: 'Counter-Strike 2', kind: 'session', id: 'a1b2c3d4' } as const;

const pad = (n: number, width = 2): string => String(n).padStart(width, '0');

/** Replaces the date tokens yyyy MM dd HH mm ss in one pass, using local time. */
export function formatDateTokens(text: string, d: Date): string {
  const map: Record<string, string> = {
    yyyy: pad(d.getFullYear(), 4),
    MM: pad(d.getMonth() + 1),
    dd: pad(d.getDate()),
    HH: pad(d.getHours()),
    mm: pad(d.getMinutes()),
    ss: pad(d.getSeconds()),
  };
  return text.replace(/yyyy|MM|dd|HH|mm|ss/g, (m) => map[m] ?? m);
}

/**
 * Renders a filename template with sample values, for the live example. Tokens that are
 * not recognised are left as written so the user sees them. The daemon validates for real.
 */
export function formatFilenameExample(
  template: string,
  now: Date,
  parts: { game: string; kind: string; id: string } = EXAMPLE_PARTS,
): string {
  return template.replace(/\{([^{}]*)\}/g, (whole, tok: string) => {
    if (tok === 'game') return parts.game;
    if (tok === 'kind') return parts.kind;
    if (tok === 'id') return parts.id;
    if (/^[yMdHms\-_.: ]+$/.test(tok) && /[yMdHms]/.test(tok)) return formatDateTokens(tok, now);
    return whole;
  });
}

// ---------------------------------------------------------------------------
// List entries
// ---------------------------------------------------------------------------

export type Normalized = { ok: true; value: string } | { ok: false; error: string };
export type Normalizer = (raw: string) => Normalized;

/** Trims and rejects blank input. */
export function defaultNormalize(raw: string): Normalized {
  const value = raw.trim();
  return value === '' ? { ok: false, error: 'Enter a value first.' } : { ok: true, value };
}

/** Mirrors the daemon's `ExeName` rule: a lowercase basename ending in `.exe`. */
export function normalizeExeName(raw: string): Normalized {
  const lower = raw.trim().toLowerCase();
  if (lower === '') return { ok: false, error: 'Enter an executable name, such as game.exe.' };
  if (/[\\/:]/.test(lower)) return { ok: false, error: 'Use the file name only, without a folder path.' };
  if (lower.length <= '.exe'.length || !lower.endsWith('.exe')) {
    return { ok: false, error: 'The name must end in .exe.' };
  }
  return { ok: true, value: lower };
}

export type AddResult = { ok: true; list: string[] } | { ok: false; error: string };

/** Appends a normalized entry. Duplicates are refused, ignoring case. */
export function addToList(list: string[], raw: string, normalize: Normalizer = defaultNormalize): AddResult {
  const n = normalize(raw);
  if (!n.ok) return n;
  const wanted = n.value.toLowerCase();
  if (list.some((item) => item.toLowerCase() === wanted)) {
    return { ok: false, error: `${n.value} is already in the list.` };
  }
  return { ok: true, list: [...list, n.value] };
}

// ---------------------------------------------------------------------------
// Exact-path diagnostics (rows and lists)
// ---------------------------------------------------------------------------

/**
 * The message for exactly this path, with no match on paths below it. Used where a list
 * shows its own count error and each row shows its own field errors.
 */
export function diagnosticAt(path: string, diagnostics: DiagnosticDto[]): FieldDiagnostic | null {
  return pickDiagnostic(diagnostics, (p) => p === path);
}

// ---------------------------------------------------------------------------
// Video settings (schema.rs `Preset::values`, docs/design/09-settings-and-config.md section 13.2)
// ---------------------------------------------------------------------------

export type PresetName = 'low' | 'medium' | 'high' | 'ultra';
export type CodecName = 'h264' | 'hevc' | 'av1';
export type VideoResolution = 'native' | '360p' | '480p' | '720p' | '1080p' | '1440p' | '2160p';

/** One row of the fixed preset table. `name` is the display name ("Standard" is the `medium` value). */
export interface PresetRow {
  name: string;
  resolution: VideoResolution;
  fps: number;
  kbps: number;
}

/** The fixed preset table, mirrored from `Preset::values` in crates/core/src/config/schema.rs. */
export const PRESET_ROWS: Record<PresetName, PresetRow> = {
  low: { name: 'Low', resolution: '480p', fps: 30, kbps: 4000 },
  medium: { name: 'Standard', resolution: '720p', fps: 60, kbps: 8000 },
  high: { name: 'High', resolution: '1080p', fps: 60, kbps: 14000 },
  ultra: { name: 'Ultra', resolution: '1440p', fps: 60, kbps: 30000 },
};

/** Short description under each preset name on the cards. */
export const PRESET_BLURBS: Record<PresetName | 'custom', string> = {
  low: 'Lower-end PCs & small files',
  medium: 'Balanced quality & size',
  high: 'Sharp footage',
  ultra: 'Max detail',
  custom: 'Dial in your own settings',
};

/** One quality card. `detail` is the "resolution fps" line, or null for Custom. */
export interface PresetCard {
  value: PresetName | 'custom';
  name: string;
  blurb: string;
  detail: string | null;
}

const PRESET_ORDER: readonly PresetName[] = ['low', 'medium', 'high', 'ultra'];

/** The five cards in display order: the four named presets, then Custom. */
export function presetCards(): PresetCard[] {
  const named: PresetCard[] = PRESET_ORDER.map((value) => {
    const row = PRESET_ROWS[value];
    return { value, name: row.name, blurb: PRESET_BLURBS[value], detail: `${row.resolution} ${row.fps} FPS` };
  });
  return [...named, { value: 'custom', name: 'Custom', blurb: PRESET_BLURBS.custom, detail: null }];
}

function isPresetName(preset: string): preset is PresetName {
  return hasOwn(PRESET_ROWS, preset);
}

/** Target bitrate in kbps for a named preset. Null for Custom or an unknown name. */
export function presetBitrate(preset: string): number | null {
  return isPresetName(preset) ? PRESET_ROWS[preset].kbps : null;
}

/** The video bitrate the recorder aims for: the preset table, or the custom target. */
export function effectiveVideoKbps(video: { preset: string; bitrate_kbps: number }): number {
  return presetBitrate(video.preset) ?? video.bitrate_kbps;
}

/** The draft fields one preset click writes. */
export interface PresetPatch {
  preset: string;
  resolution?: VideoResolution;
  fps?: number;
  bitrate_kbps?: number;
}

/**
 * What clicking a preset card writes. A named preset sets its resolution, fps and bitrate too,
 * so switching to Custom later starts from those numbers. Custom writes only the name.
 */
export function presetPatch(preset: string): PresetPatch {
  if (!isPresetName(preset)) return { preset };
  const row = PRESET_ROWS[preset];
  return { preset, resolution: row.resolution, fps: row.fps, bitrate_kbps: row.kbps };
}

/**
 * Resolution choices for the Custom panel, highest first. `value` is the config value; `label`
 * is display text (the output height, or "Native" for the capture size).
 */
export const RESOLUTION_CHOICES: { value: VideoResolution; label: string }[] = [
  { value: 'native', label: 'Native (match the game)' },
  { value: '2160p', label: '4K (2160p)' },
  { value: '1440p', label: '1440p' },
  { value: '1080p', label: '1080p' },
  { value: '720p', label: '720p' },
  { value: '480p', label: '480p' },
  { value: '360p', label: '360p' },
];

/** Frame rates offered in the Custom panel. */
export const FPS_CHOICES: readonly number[] = [24, 30, 60, 90, 120, 144];

/** Codec choices. The reason an option is disabled comes from `codecOptions`. */
export const CODEC_CHOICES: { value: CodecName; label: string }[] = [
  { value: 'h264', label: 'H.264 (most compatible)' },
  { value: 'hevc', label: 'H.265 / HEVC (smaller files)' },
  { value: 'av1', label: 'AV1 (smallest, needs a hardware encoder)' },
];

/** Bitrate stops for the Custom panel, in kbps (1 to 80 Mbps). */
export const BITRATE_STOPS_KBPS: readonly number[] = [
  1000, 2000, 4000, 6000, 8000, 10000, 14000, 20000, 30000, 40000, 60000, 80000,
];

/** Range the daemon accepts for a bitrate (CFG-V005). Defined in bounds.ts. */
export { BITRATE_MAX_KBPS, BITRATE_MIN_KBPS } from './bounds';

/** True when the kbps value is exactly one of the stops. */
export function isBitrateStop(kbps: number): boolean {
  return BITRATE_STOPS_KBPS.includes(kbps);
}

/**
 * The bitrate Select value. A stop gives its kbps as text. Any other value, or the user having
 * opened Custom, gives 'custom', which reveals the kbps field.
 */
export function bitrateChoice(kbps: number, customOpen = false): string {
  return customOpen || !isBitrateStop(kbps) ? 'custom' : String(kbps);
}

/** Options for the bitrate Select: the stops in Mbps, then "Custom…". */
export function bitrateOptions(): { value: string; label: string }[] {
  const stops = BITRATE_STOPS_KBPS.map((kbps) => ({ value: String(kbps), label: `${formatMbps(kbps)} Mbps` }));
  return [...stops, { value: 'custom', label: 'Custom…' }];
}

/** Short codec name for the summary and the hardware list. */
export function codecLabel(codec: string): string {
  return codec === 'hevc' ? 'H.265' : codec === 'av1' ? 'AV1' : 'H.264';
}

/** kbps for a value in Mbps with at most one decimal, such as "12" or "3.9". */
export function formatMbps(kbps: number): string {
  return String(Number((kbps / 1000).toFixed(1)));
}

/**
 * Approximate size of one minute of recording, in MB (1 MB = 1000 kB, the unit a file browser
 * shows): (video kbps + audio tracks x audio kbps) x 60 s / 8 bits / 1000, rounded.
 */
export function estimateMBPerMinute(videoKbps: number, audioTracks: number, audioKbps: number): number {
  return Math.round(((videoKbps + audioTracks * audioKbps) * 60) / 8 / 1000);
}

/**
 * The one-line summary under the cards, for example
 * "Records 1080p · 60 FPS · H.264 · 14 Mbps (≈105 MB per minute)". Video only.
 */
export function videoSummary(video: {
  preset: string;
  codec: string;
  resolution: string;
  fps: number;
  bitrate_kbps: number;
}): string {
  const kbps = effectiveVideoKbps(video);
  const mb = estimateMBPerMinute(kbps, 0, 0);
  return `Records ${video.resolution} · ${video.fps} FPS · ${codecLabel(video.codec)} · ${formatMbps(kbps)} Mbps (≈${mb} MB per minute)`;
}

/** Highest quality_cq for a codec (validate.rs CFG-V004): AV1 0..63, H.264 and HEVC 0..51. */
export function cqMax(codec: string): number {
  return codec === 'av1' ? 63 : 51;
}

/**
 * Slider position for a quality_cq value. The slider runs from "Smaller file" on the left
 * (position 0, cq at its maximum) to "Better quality" on the right (position max, cq 0).
 */
export function cqToSlider(cq: number, codec: string): number {
  const max = cqMax(codec);
  return max - clamp(Math.round(cq), 0, max);
}

/** Inverse of `cqToSlider`: a slider position back to quality_cq. */
export function sliderToCq(position: number, codec: string): number {
  const max = cqMax(codec);
  return max - clamp(Math.round(position), 0, max);
}

/** Hardware backends the probe reports, as the daemon writes them. */
const HARDWARE_BACKENDS: readonly string[] = ['nvenc', 'amf', 'qsv'];

/** Probe codec names, which differ from the config names (`hevc` is `h265`). */
const PROBE_CODEC: Record<CodecName, string> = { h264: 'h264', hevc: 'h265', av1: 'av1' };

/** Human names for a backend. */
const BACKEND_NAMES: Record<string, string> = {
  nvenc: 'NVIDIA NVENC',
  amf: 'AMD AMF',
  qsv: 'Intel Quick Sync',
  software: 'Software (CPU)',
};

/** One probed encoder row, as `encoders.encoders` holds it. `codec` is h264, h265 or av1. */
export interface ProbeRow {
  backend: string;
  codec: string;
  works: boolean;
}

export interface CodecAvailability {
  available: boolean;
  /** Why it is unavailable, for the option's tooltip. Null when available. */
  reason: string | null;
}

function backendMatches(backend: string, codec: CodecName, encoder: string): boolean {
  if (encoder === 'auto') {
    // Automatic uses any hardware encoder. It falls back to software only for H.264 and H.265.
    return HARDWARE_BACKENDS.includes(backend) || (codec !== 'av1' && backend === 'software');
  }
  return backend === encoder;
}

/**
 * Whether a codec can be recorded with the chosen encoder ("auto", "nvenc", "amf", "qsv" or
 * "software"). CPU never does AV1. Before the probe is ready (`ready` false) nothing is disabled,
 * except the CPU AV1 rule, which is a fact about the setting and not about the probe.
 */
export function codecAvailability(
  codec: CodecName,
  encoder: string,
  rows: readonly ProbeRow[],
  ready: boolean,
): CodecAvailability {
  if (codec === 'av1' && encoder === 'software') {
    return { available: false, reason: 'AV1 needs a hardware encoder.' };
  }
  if (!ready) return { available: true, reason: null };
  const wire = PROBE_CODEC[codec];
  const usable = rows.some((r) => r.works && r.codec === wire && backendMatches(r.backend, codec, encoder));
  if (usable) return { available: true, reason: null };
  const label = codecLabel(codec);
  if (encoder === 'auto') return { available: false, reason: `No working encoder for ${label} on this PC.` };
  return { available: false, reason: `${BACKEND_NAMES[encoder] ?? encoder} can't encode ${label} on this PC.` };
}

/** One option for the codec Select. */
export interface CodecOption {
  value: CodecName;
  label: string;
  disabled: boolean;
  title: string | undefined;
}

/** The three codec options with their availability for the chosen encoder. */
export function codecOptions(encoder: string, rows: readonly ProbeRow[], ready: boolean): CodecOption[] {
  return CODEC_CHOICES.map((c) => {
    const a = codecAvailability(c.value, encoder, rows, ready);
    return { value: c.value, label: c.label, disabled: !a.available, title: a.reason ?? undefined };
  });
}

/** A GPU as the Settings screen needs it. Matches `encoders.gpus`. */
export interface GpuInfo {
  name: string;
  vendor: string;
  dedicatedMb: number;
}

/** The Selected GPU value that means "let the recorder pick". */
export const AUTO_GPU = 'auto';

/** Display name for a vendor label from the daemon ("nvidia" becomes "NVIDIA"). */
export function vendorLabel(vendor: string): string {
  const known: Record<string, string> = { nvidia: 'NVIDIA', amd: 'AMD', intel: 'Intel' };
  return known[vendor.toLowerCase()] ?? vendor;
}

/** VRAM as "16 GB" or "512 MB". Empty when unknown. */
export function vramLabel(mb: number): string {
  if (mb <= 0) return '';
  return mb >= 1024 ? `${Math.round(mb / 1024)} GB` : `${mb} MB`;
}

/**
 * Options for the Selected GPU Select. "Automatic" comes first. Each detected GPU shows its
 * vendor and VRAM. A saved GPU that is not detected stays in the list as "Not found", so the
 * value is not lost. Before the probe is ready, the saved name is shown as it is.
 */
export function gpuOptions(
  gpus: readonly GpuInfo[],
  saved: string,
  ready: boolean,
): { value: string; label: string }[] {
  const options = [{ value: AUTO_GPU, label: 'Automatic (best available)' }];
  for (const g of gpus) {
    const vram = vramLabel(g.dedicatedMb);
    options.push({ value: g.name, label: `${g.name} · ${vendorLabel(g.vendor)}${vram ? ` · ${vram}` : ''}` });
  }
  const found = gpus.some((g) => g.name === saved);
  if (saved !== AUTO_GPU && !found) {
    options.push({ value: saved, label: ready ? `${saved} (Not found on this PC)` : saved });
  }
  return options;
}

const VENDOR_BACKEND: Record<string, string> = { nvidia: 'nvenc', amd: 'amf', intel: 'qsv' };
const SHORT_BACKEND: Record<string, string> = { nvenc: 'NVENC', amf: 'AMF', qsv: 'Quick Sync', software: 'Software' };

/**
 * The "Your hardware" lines: one per GPU with its hardware encoder and the codecs that work on
 * it, then one for the CPU encoder when the probe reports software rows. Example:
 * "NVIDIA GeForce RTX 5080 — NVENC H.264 ✓ H.265 ✓ AV1 ✓".
 */
export function hardwareLines(gpus: readonly GpuInfo[], rows: readonly ProbeRow[]): string[] {
  const works = (backend: string, wire: string): boolean =>
    rows.some((r) => r.backend === backend && r.codec === wire && r.works);
  const cells = (backend: string): string =>
    [
      ['h264', 'H.264'],
      ['h265', 'H.265'],
      ['av1', 'AV1'],
    ]
      .map(([wire, label]) => `${label} ${works(backend, wire) ? '✓' : '✗'}`)
      .join(' ');
  const lines: string[] = [];
  for (const g of gpus) {
    const backend = VENDOR_BACKEND[g.vendor.toLowerCase()];
    lines.push(backend ? `${g.name} — ${SHORT_BACKEND[backend]} ${cells(backend)}` : `${g.name} — no supported hardware encoder`);
  }
  if (rows.some((r) => r.backend === 'software')) lines.push(`Software (CPU) — ${cells('software')}`);
  return lines;
}

/**
 * Rough size of the replay buffer in MiB: (video kbps + every enabled track's kbps) times the
 * length. kbps * s / 8 gives kB, and dividing by 1024 gives MB as the UI shows it.
 */
export function estimateBufferMB(seconds: number, videoKbps: number, audioKbps: number[]): number {
  const total = videoKbps + audioKbps.reduce((sum, k) => sum + k, 0);
  return Math.round((total * seconds) / 8 / 1024);
}

/** "≈ 450 MB" style text for the disk estimate line, with GB past 1024 MB. */
export function formatMB(mb: number): string {
  if (mb >= 1024) return `≈ ${(mb / 1024).toFixed(1)} GB`;
  return `≈ ${mb} MB`;
}

/**
 * Formats a span given in SECONDS as "2 min 30 s", "5 min", or "45 s". Not the media-length
 * `m:ss` formatter: that one is `formatDuration(ms)` in `../format.ts`.
 */
export function formatSpanSeconds(totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds));
  const m = Math.floor(s / 60);
  const rest = s % 60;
  if (m === 0) return `${rest} s`;
  if (rest === 0) return `${m} min`;
  return `${m} min ${rest} s`;
}
