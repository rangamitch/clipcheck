/**
 * Device, app and encoder discovery for the settings pickers.
 *
 * This is the only module that maps the daemon's snake_case discovery replies
 * (wire types in `daemon.ts`) to the camelCase types the UI uses. If the daemon
 * contract changes, fix the mapping here.
 */
import { writable } from 'svelte/store';
import {
  request,
  onEvent,
  type Command,
  type Reply,
  type Event,
  type WireAudioApp,
  type WireAudioDevice,
  type WireGpu,
  type WireEncoder,
  type WireRunningApp,
} from './daemon';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** An application that has (or recently had) an audio session. */
export interface AudioApp {
  exe: string;
  name: string;
  iconB64: string | null;
  /** Currently playing audio. */
  active: boolean;
  /** Raw peak value from the daemon (unit not fixed by the contract). */
  peak: number;
  lastActiveUnixMs: number;
}

export interface AudioDevice {
  id: string;
  name: string;
  isDefault: boolean;
}

export interface AudioDevices {
  capture: AudioDevice[];
  render: AudioDevice[];
}

export interface Gpu {
  name: string;
  vendor: string;
  index: number;
  dedicatedMb: number;
}

export interface EncoderInfo {
  backend: string;
  codec: string;
  encoderName: string;
  works: boolean;
}

export interface EncoderState {
  encoderReady: boolean;
  gpus: Gpu[];
  encoders: EncoderInfo[];
}

/** A process with a visible window, offered as a game or app candidate. */
export interface RunningApp {
  exe: string;
  name: string;
  iconB64: string | null;
  pid: number;
  isGame: boolean;
  inAllowList: boolean;
  suggested: boolean;
}

export interface MicLevel {
  rmsDb: number;
  peakDb: number;
  /** True once a level event has arrived while the meter is enabled. */
  active: boolean;
}

/** Minimal shape `sortApps` needs. */
export interface SortableApp {
  exe: string;
  name: string;
  active?: boolean;
  lastActiveUnixMs?: number;
}

// ---------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------

export const audioApps = writable<AudioApp[]>([]);
export const audioDevices = writable<AudioDevices>({ capture: [], render: [] });
export const encoders = writable<EncoderState>({ encoderReady: false, gpus: [], encoders: [] });
export const runningApps = writable<RunningApp[]>([]);
export const micLevel = writable<MicLevel>({ rmsDb: -100, peakDb: -100, active: false });

// ---------------------------------------------------------------------------
// Wire adapter (the one place that knows the snake_case field names)
// ---------------------------------------------------------------------------

/** Throws on an `error` reply or an unexpected reply type. */
function unwrap<T extends Reply['type']>(reply: Reply, type: T): Extract<Reply, { type: T }> {
  if (reply.type === 'error') throw new Error(reply.data.message);
  if (reply.type !== type) throw new Error(`expected ${type} reply, got ${reply.type}`);
  return reply as Extract<Reply, { type: T }>;
}

/** Sends a command and swallows transport errors. Used by background loops. */
async function safeRequest(command: Command): Promise<Reply | null> {
  try {
    return await request(command);
  } catch {
    return null;
  }
}

function mapAudioApp(w: WireAudioApp): AudioApp {
  return {
    exe: w.exe,
    name: w.name,
    iconB64: w.icon_png_b64 ?? null,
    active: w.active,
    peak: w.peak,
    lastActiveUnixMs: w.last_active_unix_ms,
  };
}

function mapDevice(w: WireAudioDevice): AudioDevice {
  return { id: w.id, name: w.name, isDefault: w.is_default };
}

function mapGpu(w: WireGpu): Gpu {
  return { name: w.name, vendor: w.vendor, index: w.index, dedicatedMb: w.dedicated_mb };
}

function mapEncoder(w: WireEncoder): EncoderInfo {
  return { backend: w.backend, codec: w.codec, encoderName: w.encoder_name ?? w.ffmpeg_name ?? '', works: w.works };
}

function mapRunningApp(w: WireRunningApp): RunningApp {
  return {
    exe: w.exe,
    name: w.name,
    iconB64: w.icon_png_b64 ?? null,
    pid: w.pid,
    isGame: w.is_game,
    inAllowList: w.in_allow_list,
    suggested: w.suggested,
  };
}

// ---------------------------------------------------------------------------
// Audio app polling (refcounted, paused when hidden or offline)
// ---------------------------------------------------------------------------

export const AUDIO_APPS_POLL_MS = 2000;

let pollRefs = 0;
let pollTimer: ReturnType<typeof setInterval> | undefined;
let pollListening = false;
let appsInFlight = false;

function pollPaused(): boolean {
  if (typeof document !== 'undefined' && document.hidden === true) return true;
  if (typeof navigator !== 'undefined' && navigator.onLine === false) return true;
  return false;
}

/** Starts or stops the timer so it runs only while wanted and not paused. */
function syncPoll(): void {
  const want = pollRefs > 0 && !pollPaused();
  if (want && pollTimer === undefined) {
    void refreshAudioApps();
    pollTimer = setInterval(() => void refreshAudioApps(), AUDIO_APPS_POLL_MS);
  } else if (!want && pollTimer !== undefined) {
    clearInterval(pollTimer);
    pollTimer = undefined;
  }
}

function attachPollListeners(): void {
  if (pollListening) return;
  pollListening = true;
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', syncPoll);
  if (typeof window !== 'undefined') {
    window.addEventListener('online', syncPoll);
    window.addEventListener('offline', syncPoll);
  }
}

function detachPollListeners(): void {
  if (!pollListening) return;
  pollListening = false;
  if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', syncPoll);
  if (typeof window !== 'undefined') {
    window.removeEventListener('online', syncPoll);
    window.removeEventListener('offline', syncPoll);
  }
}

/**
 * Polls `audioApps` every 2 s. Calls share one timer (refcounted), and the
 * timer pauses while the document is hidden or the browser is offline. On
 * resume it refreshes immediately. Returns an idempotent stop function.
 */
export function startAudioAppsPolling(): () => void {
  pollRefs += 1;
  if (pollRefs === 1) attachPollListeners();
  syncPoll();
  let released = false;
  return () => {
    if (released) return;
    released = true;
    pollRefs -= 1;
    if (pollRefs === 0) detachPollListeners();
    syncPoll();
  };
}

/** Fetches the audio app list once. Keeps the previous list on failure. */
export async function refreshAudioApps(): Promise<void> {
  if (appsInFlight) return;
  appsInFlight = true;
  try {
    const r = unwrap(await request({ type: 'list_audio_apps' }), 'audio_apps');
    audioApps.set(sortApps(r.data.apps.map(mapAudioApp)));
  } catch {
    // Keep the last known list.
  } finally {
    appsInFlight = false;
  }
}

// ---------------------------------------------------------------------------
// One-shot loaders
// ---------------------------------------------------------------------------

/** Fetches capture and render devices once. Keeps the previous value on failure. */
export async function loadAudioDevices(): Promise<void> {
  try {
    const r = unwrap(await request({ type: 'list_audio_devices' }), 'audio_devices');
    audioDevices.set({ capture: r.data.capture.map(mapDevice), render: r.data.render.map(mapDevice) });
  } catch {
    // Keep the previous value.
  }
}

/** Retry interval and cap while the daemon reports `encoder_ready: false`. */
export const ENCODER_RETRY_MS = 1500;
export const ENCODER_MAX_RETRIES = 20;

let encoderRun = 0;
let encoderTimer: ReturnType<typeof setTimeout> | undefined;

/**
 * Loads the encoder list. While `encoder_ready` is false (or the request
 * fails), retries every 1.5 s, up to 20 retries. Resolves after the first
 * attempt. Later attempts run in the background. A new call cancels the
 * previous retry chain.
 */
export async function loadEncoders(): Promise<void> {
  const run = ++encoderRun;
  if (encoderTimer !== undefined) {
    clearTimeout(encoderTimer);
    encoderTimer = undefined;
  }
  await attemptEncoders(run, 0);
}

async function attemptEncoders(run: number, retry: number): Promise<void> {
  let ready = false;
  try {
    const r = unwrap(await request({ type: 'list_encoders' }), 'encoders');
    if (run !== encoderRun) return;
    ready = (r.data.encoder_ready ?? r.data.ffmpeg_ready) === true;
    encoders.set({ encoderReady: ready, gpus: r.data.gpus.map(mapGpu), encoders: r.data.encoders.map(mapEncoder) });
  } catch {
    if (run !== encoderRun) return;
  }
  if (ready || retry >= ENCODER_MAX_RETRIES) return;
  encoderTimer = setTimeout(() => {
    encoderTimer = undefined;
    void attemptEncoders(run, retry + 1);
  }, ENCODER_RETRY_MS);
}

/** Fetches the running-process candidate list once. */
export async function refreshRunningApps(): Promise<void> {
  try {
    const r = unwrap(await request({ type: 'list_running_apps' }), 'running_apps');
    runningApps.set(sortApps(r.data.apps.map(mapRunningApp)));
  } catch {
    // Keep the previous list.
  }
}

// ---------------------------------------------------------------------------
// Mic meter (refcounted lease; the daemon needs a renewal every 10 s)
// ---------------------------------------------------------------------------

export const MIC_METER_RENEW_MS = 10_000;

let meterRefs = 0;
let meterDevice: string | null = null;
let meterTimer: ReturnType<typeof setInterval> | undefined;
let meterUnlisten: (() => void) | undefined;

function sendMeter(enable: boolean): void {
  void safeRequest({ type: 'mic_meter', data: { enable, device: enable ? meterDevice : null } });
}

function handleMeterEvent(event: Event): void {
  if (event.type !== 'mic_level') return;
  micLevel.set({ rmsDb: event.data.rms_db, peakDb: event.data.peak_db, active: true });
}

/**
 * Enables the input meter and feeds `micLevel`. The first start subscribes to
 * the `meter` topic, enables the meter and renews it every 10 s. Later starts
 * share that lease. A later start that passes a device switches the meter to
 * it (last start wins). Returns an idempotent stop function. Stopping the last
 * consumer disables the meter and resets `micLevel`.
 */
export function startMicMeter(device?: string | null): () => void {
  meterRefs += 1;
  if (meterRefs === 1) {
    meterDevice = device ?? null;
    meterUnlisten = onEvent(handleMeterEvent);
    void safeRequest({ type: 'subscribe', data: { topics: ['meter'] } });
    sendMeter(true);
    meterTimer = setInterval(() => sendMeter(true), MIC_METER_RENEW_MS);
  } else if (device !== undefined && device !== meterDevice) {
    meterDevice = device;
    sendMeter(true);
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    releaseMeter();
  };
}

function releaseMeter(): void {
  meterRefs -= 1;
  if (meterRefs > 0) return;
  meterRefs = 0;
  if (meterTimer !== undefined) {
    clearInterval(meterTimer);
    meterTimer = undefined;
  }
  meterUnlisten?.();
  meterUnlisten = undefined;
  sendMeter(false);
  void safeRequest({ type: 'unsubscribe', data: { topics: ['meter'] } });
  meterDevice = null;
  micLevel.set({ rmsDb: -100, peakDb: -100, active: false });
}

/** Result of a 5-second microphone test recording. */
export interface MicTestResult {
  wav: Uint8Array;
  durationMs: number;
  peakDb: number;
}

/**
 * Asks the daemon to record 5 seconds from the microphone (`null` = the default device).
 * Errors propagate: an `error` reply rejects with the daemon's message.
 */
export async function recordMicTest(device?: string | null): Promise<MicTestResult> {
  const r = unwrap(await request({ type: 'mic_test', data: { device: device ?? null } }), 'mic_test');
  const binary = atob(r.data.wav_base64);
  const wav = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) wav[i] = binary.charCodeAt(i);
  return { wav, durationMs: r.data.duration_ms, peakDb: r.data.peak_db };
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/** `data:` URL for a base64 PNG, or undefined when there is no icon. */
export function iconSrc(b64?: string | null): string | undefined {
  return b64 ? `data:image/png;base64,${b64}` : undefined;
}

/**
 * Order for pickers: playing audio first, then most recently active, then
 * name (falling back to exe), then exe. Returns a new array.
 */
export function sortApps<T extends SortableApp>(apps: readonly T[]): T[] {
  return [...apps].sort((a, b) => {
    const playing = (b.active ? 1 : 0) - (a.active ? 1 : 0);
    if (playing !== 0) return playing;
    const recent = (b.lastActiveUnixMs ?? 0) - (a.lastActiveUnixMs ?? 0);
    if (recent !== 0) return recent;
    const byName = (a.name || a.exe).localeCompare(b.name || b.exe, undefined, { sensitivity: 'base' });
    if (byName !== 0) return byName;
    return a.exe.localeCompare(b.exe);
  });
}

/** Meter range floor and ceiling in dBFS. */
export const METER_FLOOR_DB = -100;
export const METER_CEIL_DB = 0;

/** Maps -100..0 dB linearly onto 0..1, clamped. NaN maps to 0. */
export function dbToMeterFraction(db: number): number {
  if (Number.isNaN(db)) return 0;
  const f = (db - METER_FLOOR_DB) / (METER_CEIL_DB - METER_FLOOR_DB);
  return Math.min(1, Math.max(0, f));
}

/** Volume slider range in percent. */
export const VOLUME_MAX_PCT = 150;
/** Volume floor in dB. Reaching it (0 %) means muted. */
export const VOLUME_FLOOR_DB = -60;
/** Gain at the top of the slider in dB. */
export const VOLUME_MAX_DB = 12;

/**
 * Maps the 0..150 % slider onto dB with a piecewise-linear curve.
 *   0 %   -> -60 dB (floor, treated as mute)
 *   100 % ->   0 dB
 *   150 % ->  +12 dB
 * 0..100 % spans -60..0 dB (60 dB across 100 steps). 100..150 % spans 0..+12 dB.
 * Input is clamped to 0..150. NaN is treated as mute.
 */
export function volumePercentToDb(pct: number): number {
  if (Number.isNaN(pct)) return VOLUME_FLOOR_DB;
  const p = Math.min(VOLUME_MAX_PCT, Math.max(0, pct));
  if (p <= 0) return VOLUME_FLOOR_DB;
  if (p <= 100) return VOLUME_FLOOR_DB + (p / 100) * -VOLUME_FLOOR_DB;
  return ((p - 100) / (VOLUME_MAX_PCT - 100)) * VOLUME_MAX_DB;
}

/** Inverse of `volumePercentToDb`. dB is clamped to -60..+12. NaN maps to 0 %. */
export function dbToVolumePercent(db: number): number {
  if (Number.isNaN(db)) return 0;
  const d = Math.min(VOLUME_MAX_DB, Math.max(VOLUME_FLOOR_DB, db));
  if (d <= VOLUME_FLOOR_DB) return 0;
  if (d <= 0) return ((d - VOLUME_FLOOR_DB) / -VOLUME_FLOOR_DB) * 100;
  return 100 + (d / VOLUME_MAX_DB) * (VOLUME_MAX_PCT - 100);
}

/** Stable pastel-ish colour for an exe, used as the icon fallback background. */
export function fallbackColor(exe: string): string {
  let h = 5381;
  for (const ch of exe.toLowerCase()) h = ((h << 5) + h + ch.charCodeAt(0)) | 0;
  return `hsl(${Math.abs(h) % 360} 55% 42%)`;
}

/** First letter (upper case) of the name, or of the exe when the name is blank. */
export function initialOf(name: string, exe: string): string {
  const s = (name ?? '').trim() || (exe ?? '').trim();
  return Array.from(s)[0]?.toUpperCase() ?? '?';
}
