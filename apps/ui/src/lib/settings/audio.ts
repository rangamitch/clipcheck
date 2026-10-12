/**
 * Pure rules for the Audio settings tab. No Svelte, no I/O.
 *
 * Every edit returns a new array. `setPath('audio.apps', list)` then sends the whole list,
 * because `diffPatch` replaces arrays whole. The track layout itself is derived by the daemon
 * (`Audio::track_plan` in crates/core/src/config/schema.rs); `trackPlanPreview` mirrors it so
 * the tab can show the result before saving.
 */
import type { AudioDevice } from '../discovery';
import { APP_NAME_MAX_CHARS } from './bounds';
import { normalizeExeName } from './pure';
import type { AudioAppConfig, AudioConfig } from './store';

/** Schema limit on tracks in one recording (decision D9). */
export const MAX_TRACKS = 8;
/** Schema limit on `[[audio.apps]]` entries. */
export const MAX_APPS = 16;
/** Longest allowed app display name, in characters. */
export const MAX_APP_NAME_CHARS = APP_NAME_MAX_CHARS;
/** Gain range per source, in dB (schema). 0 % on the slider is the floor, 100 % is 0 dB. */
export const GAIN_MIN_DB = -60;
export const GAIN_MAX_DB = 12;
/** Noise gate threshold range, in dB (schema). */
export const GATE_MIN_DB = -100;
export const GATE_MAX_DB = -20;
/** AAC bitrates offered per track. */
export const BITRATE_OPTIONS_KBPS: readonly number[] = [128, 160, 192, 256, 320];
/** The schema's value for "the Windows default capture device". */
export const DEFAULT_DEVICE = 'default';
/** ClipCheck's own executable. It is never offered as a recorded app. */
export const SELF_EXE = 'clipcheck.exe';

/** The part of `AudioConfig` that decides the track layout. */
export type TrackPlanInput = Pick<AudioConfig, 'separate_tracks' | 'game' | 'mic' | 'other_apps' | 'apps'>;

/** What the recorded file will contain, as shown before saving. */
export interface TrackPlanPreview {
  /** Track titles in order. The first is always "Mix". */
  titles: string[];
  /** Number of tracks (titles.length). */
  count: number;
  /** True when `count` exceeds MAX_TRACKS. Saving is refused by the daemon (CFG-A001). */
  over: boolean;
  /** Enabled sources, in track order. */
  enabledSources: number;
}

/**
 * Mirrors `Audio::track_plan`. Track 1 is "Mix" and holds every enabled source except the
 * push-to-talk track. With `separate_tracks`, one more track follows per enabled source in the
 * order game, microphone, push to talk (when `mic.ptt_separate_track`), enabled apps in list
 * order, other apps. With no enabled source the result is one silent Mix.
 */
export function trackPlanPreview(audio: TrackPlanInput): TrackPlanPreview {
  const sources: string[] = [];
  if (audio.game.enabled) sources.push('Game');
  if (audio.mic.enabled) sources.push('Microphone');
  if (audio.mic.enabled && audio.mic.ptt_separate_track) sources.push('Push to talk');
  for (const app of audio.apps) if (app.enabled) sources.push(app.name);
  if (audio.other_apps.enabled) sources.push('Other apps');

  const titles = ['Mix'];
  if (sources.length > 0 && audio.separate_tracks) titles.push(...sources);
  return {
    titles,
    count: titles.length,
    over: titles.length > MAX_TRACKS,
    enabledSources: sources.length,
  };
}

/** Number of enabled sources (game, mic, enabled apps, other apps). */
export function sourcesEnabledCount(audio: TrackPlanInput): number {
  return trackPlanPreview(audio).enabledSources;
}

/**
 * True when turning one more source on keeps the track count within MAX_TRACKS. With
 * separate tracks off a source never adds a track, so this is always true then.
 */
export function canAddSource(audio: TrackPlanInput): boolean {
  if (!audio.separate_tracks) return true;
  return trackPlanPreview(audio).count < MAX_TRACKS;
}

/** The exe as stored: lowercase, ends in ".exe". Returns null when the text is not a valid name. */
export function canonicalExe(raw: string): string | null {
  const n = normalizeExeName(raw);
  return n.ok ? n.value : null;
}

export function isSelfExe(exe: string): boolean {
  return canonicalExe(exe) === SELF_EXE;
}

/** Result of a list edit: the new list, or a message for the user. */
export type ListResult = { ok: true; apps: AudioAppConfig[] } | { ok: false; error: string };

/** A display name for an app: trimmed, at most MAX_APP_NAME_CHARS, else the exe without ".exe". */
export function appDisplayName(name: string, exe: string): string {
  const trimmed = name.trim();
  const base = trimmed === '' ? exe.replace(/\.exe$/i, '') : trimmed;
  return Array.from(base).slice(0, MAX_APP_NAME_CHARS).join('');
}

/** Index of the entry for an exe (case-insensitive), or -1. */
export function findAppIndex(apps: readonly AudioAppConfig[], exe: string): number {
  const key = exe.toLowerCase();
  return apps.findIndex((a) => a.exe.toLowerCase() === key);
}

/**
 * Appends an app that played audio, enabled, at 0 dB. Refuses ClipCheck itself, an exe that is
 * already listed (case-insensitive), an invalid exe name, and a full list (MAX_APPS).
 */
export function addApp(
  apps: readonly AudioAppConfig[],
  app: { exe: string; name?: string },
): ListResult {
  if (apps.length >= MAX_APPS) return { ok: false, error: `The list holds at most ${MAX_APPS} apps.` };
  const exe = canonicalExe(app.exe);
  if (exe === null) return { ok: false, error: 'That app has no valid .exe name.' };
  if (exe === SELF_EXE) return { ok: false, error: 'ClipCheck cannot record itself.' };
  if (findAppIndex(apps, exe) >= 0) return { ok: false, error: `${exe} is already in the list.` };
  const entry: AudioAppConfig = {
    exe,
    name: appDisplayName(app.name ?? '', exe),
    enabled: true,
    gain_db: 0,
  };
  return { ok: true, apps: [...apps.map((a) => ({ ...a })), entry] };
}

/** Removes the entry for an exe (case-insensitive). Unknown exes leave the list as it is. */
export function removeApp(apps: readonly AudioAppConfig[], exe: string): AudioAppConfig[] {
  const key = exe.toLowerCase();
  return apps.filter((a) => a.exe.toLowerCase() !== key).map((a) => ({ ...a }));
}

/** Patches one field of one entry (matched by exe). Other entries keep their values. */
export function setAppField(
  apps: readonly AudioAppConfig[],
  exe: string,
  patch: Partial<Pick<AudioAppConfig, 'enabled' | 'gain_db' | 'name'>>,
): AudioAppConfig[] {
  const key = exe.toLowerCase();
  return apps.map((a) => (a.exe.toLowerCase() === key ? { ...a, ...patch } : { ...a }));
}

/** The label of the mic option for the saved device, for the Select's accessible summary. */
export function selectedMicLabel(capture: readonly AudioDevice[], micDevice: string): string {
  if (micDevice === DEFAULT_DEVICE) {
    const def = capture.find((d) => d.isDefault);
    return def ? `Default — ${def.name}` : 'Default';
  }
  const found = capture.find((d) => d.id === micDevice);
  return found ? found.name : 'Unavailable device';
}

export interface MicOption {
  value: string;
  label: string;
  title?: string;
}

/**
 * Options for the microphone Select. The first option is the default device, then each capture
 * device by id. A saved id that is not in the list is kept as an "Unavailable device" option,
 * so the saved value is never lost by opening the tab.
 */
export function micDeviceOptions(capture: readonly AudioDevice[], micDevice: string): MicOption[] {
  const options: MicOption[] = [{ value: DEFAULT_DEVICE, label: selectedMicLabel(capture, DEFAULT_DEVICE) }];
  for (const d of capture) options.push({ value: d.id, label: d.name });
  if (micDevice !== DEFAULT_DEVICE && !capture.some((d) => d.id === micDevice)) {
    options.push({
      value: micDevice,
      label: 'Unavailable device',
      title: 'This microphone is not connected. Recording uses it when it returns.',
    });
  }
  return options;
}

/** True when the saved mic device is not in the list (after the list has loaded). */
export function isMicUnavailable(capture: readonly AudioDevice[], micDevice: string): boolean {
  return micDevice !== DEFAULT_DEVICE && capture.length > 0 && !capture.some((d) => d.id === micDevice);
}

/** Bitrate of each recorded track, in kbps, for the buffer-size estimate. Every track uses `bitrate_kbps`. */
export function enabledTrackKbps(audio: TrackPlanInput & Pick<AudioConfig, 'bitrate_kbps'>): number[] {
  return Array.from({ length: trackPlanPreview(audio).count }, () => audio.bitrate_kbps);
}

/** The bitrate options, with the saved value added when it is not one of the standard steps. */
export function bitrateOptions(current: number): number[] {
  return BITRATE_OPTIONS_KBPS.includes(current) ? [...BITRATE_OPTIONS_KBPS] : [...BITRATE_OPTIONS_KBPS, current].sort((a, b) => a - b);
}

/** Peaks at or below this level (dB) count as nothing picked up by the mic test. */
export const MIC_TEST_SILENT_DB = -60;

/** Verdict for a mic test recording. A missing or non-finite peak counts as silent. */
export function micTestVerdict(peakDb: number): 'silent' | 'ok' {
  return !Number.isFinite(peakDb) || peakDb <= MIC_TEST_SILENT_DB ? 'silent' : 'ok';
}

/** The two push-to-talk recording choices on the Audio tab. */
export type PttMode = 'ptt_only' | 'ptt_and_open';

/**
 * The choice a config is in. `mic.ptt_separate_track` false is "Push to talk only": no open mic
 * is recorded anywhere. True is "Push to talk and open microphone".
 */
export function pttModeOf(audio: Pick<AudioConfig, 'mic'>): PttMode {
  return audio.mic.ptt_separate_track ? 'ptt_and_open' : 'ptt_only';
}

/** The `mic.ptt_separate_track` value for a choice. */
export function pttSeparateOf(mode: PttMode): boolean {
  return mode === 'ptt_and_open';
}

/**
 * Read-only line naming the microphone tracks that will be recorded, for example
 * "Mix (push to talk), Microphone (push to talk)". The Microphone and Push to talk tracks only
 * exist with `separate_tracks` (see `trackPlanPreview`). `mic.ptt_mix_gated` only matters in
 * "Push to talk and open microphone".
 */
export function micTrackSummary(audio: Pick<AudioConfig, 'separate_tracks' | 'mic'>): string {
  if (!audio.mic.enabled) return 'Microphone is off, so no microphone is recorded.';
  const separate = audio.separate_tracks;
  if (pttModeOf(audio) === 'ptt_only') {
    const parts = ['Mix (push to talk)'];
    if (separate) parts.push('Microphone (push to talk)');
    return parts.join(', ');
  }
  const parts = [`Mix (${audio.mic.ptt_mix_gated ? 'push to talk' : 'open mic'})`];
  if (separate) parts.push('Microphone (open mic)', 'Push to talk (push to talk)');
  return parts.join(', ');
}
